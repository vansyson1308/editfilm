//! VieNeu-TTS: offline Vietnamese voices (<https://github.com/pnnbao97/VieNeu-TTS>, Apache-2.0;
//! the model weights it downloads, VieNeu-TTS-v3-Turbo and MOSS-Audio-Tokenizer-Nano, are
//! Apache-2.0 too).
//!
//! An optional **external engine**, used like ffmpeg: Filmagic never links, bundles or ships it.
//! It runs a Python interpreter that has the `vieneu` package installed, as a subprocess, with the
//! small bridge in `crates/tts/python/vieneu_bridge.py` (written to `<models>/vieneu/bridge/` and
//! run as a script; it only calls the
//! package's public API). Requests go in as JSON on stdin; each speech segment comes back as raw
//! float32 samples in a file. Everything else happens here: pause markers, how long a take may
//! plausibly be, pace and pitch, headroom.
//!
//! - **Interpreter**: `FILMAGIC_VIENEU_PYTHON`, else the configured path, else the environment
//!   [`install`] creates under `<models>/vieneu/venv`, else `py -3` / `python` (Windows) or
//!   `python3` / `python`. [`detect`] takes the first that can import `vieneu`.
//! - **Models**: `vieneu` downloads its model from Hugging Face on first use, into
//!   `<models>/vieneu/cache` (`HF_HOME` and the bridge's download hook point there).
//! - **Voices**: the preset voices, cached in `<models>/vieneu/voices.json` by [`prepare`], so
//!   listing them never starts Python. Ids are `vieneu:<name>` ([`VOICE_PREFIX`]).
//! - **Cloning**: [`Request::reference`] clones the voice of a reference clip instead.
//! - **Timing**: VieNeu returns audio only, no word timestamps.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::{Audio, MAX_SECONDS, Params, TtsError, check_text, script};

/// The bridge the interpreter runs.
pub const HELPER: &str = include_str!("../python/vieneu_bridge.py");
/// Directory under the models directory: `venv/`, `cache/`, `bridge/`, `voices.json`, `tmp/`.
pub const DIR_NAME: &str = "vieneu";
/// Voice ids are `vieneu:<preset name>`.
pub const VOICE_PREFIX: &str = "vieneu:";
/// The voice id that clones a reference clip.
pub const CLONE_VOICE: &str = "vieneu-clone";
/// BCP 47 tag of every VieNeu voice.
pub const LANGUAGE: &str = "vi-VN";
/// The pip package.
pub const PACKAGE: &str = "vieneu";
pub const LICENSE: &str = "Apache-2.0 (VieNeu-TTS by pnnbao97; models VieNeu-TTS-v3-Turbo and MOSS-Audio-Tokenizer-Nano, Apache-2.0)";
pub const SOURCE: &str = "https://github.com/pnnbao97/VieNeu-TTS";
/// Environment variable naming the interpreter (wins over the setting).
pub const PYTHON_ENV: &str = "FILMAGIC_VIENEU_PYTHON";
/// The sentence "Hear this voice" speaks with a VieNeu voice.
pub const SAMPLE_SENTENCE: &str = "Xin chào! Đây là giọng của tôi khi đọc kịch bản của bạn.";
/// The model `vieneu` loads (`VIENEU_MODE=v3nano` picks the smaller one for weak CPUs).
pub const DEFAULT_MODE: &str = "v3turbo";

/// Most bytes of stdout read from the bridge (one JSON line).
const MAX_STDOUT: usize = 4 << 20;
/// Bytes of stderr kept for error messages (the end, where Python puts the exception).
const STDERR_TAIL: usize = 4096;
/// Most voices read from the cache.
const MAX_VOICES: usize = 500;
/// Largest reference clip for cloning.
pub const MAX_REFERENCE_BYTES: u64 = 64 << 20;
/// Takes per segment when a take's length is implausible.
const TAKES: u32 = 3;
/// Most speech segments in one request (a script is at most 64 KB).
const MAX_SEGMENTS: usize = 4096;

/// A Python interpreter: a program and the arguments that come before ours (`py -3`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Python {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Where it came from: `env`, `setting`, `venv` or `path`.
    pub source: &'static str,
}

impl Python {
    fn new(program: impl Into<PathBuf>, args: &[&str], source: &'static str) -> Self {
        Python { program: program.into(), args: args.iter().map(|a| a.to_string()).collect(), source }
    }

    /// As typed in a shell.
    pub fn display(&self) -> String {
        let p = self.program.to_string_lossy();
        let p = if p.contains(' ') { format!("\"{p}\"") } else { p.into_owned() };
        std::iter::once(p).chain(self.args.iter().cloned()).collect::<Vec<_>>().join(" ")
    }
}

/// `<models>/vieneu`.
pub fn dir(models_dir: &Path) -> PathBuf {
    models_dir.join(DIR_NAME)
}

/// The environment [`install`] creates.
pub fn venv_dir(models_dir: &Path) -> PathBuf {
    dir(models_dir).join("venv")
}

/// Its interpreter.
pub fn venv_python(models_dir: &Path) -> PathBuf {
    if cfg!(windows) { venv_dir(models_dir).join("Scripts").join("python.exe") } else { venv_dir(models_dir).join("bin").join("python") }
}

/// Where `vieneu` downloads its model.
pub fn cache_dir(models_dir: &Path) -> PathBuf {
    dir(models_dir).join("cache")
}

fn voices_file(models_dir: &Path) -> PathBuf {
    dir(models_dir).join("voices.json")
}

/// Interpreters to try, in order (see the module docs). `configured` is the setting.
pub fn candidates(configured: Option<&str>, models_dir: Option<&Path>) -> Vec<Python> {
    let mut out = Vec::new();
    if let Some(p) = std::env::var_os(PYTHON_ENV).filter(|v| !v.is_empty()) {
        out.push(Python::new(p, &[], "env"));
    }
    if let Some(p) = configured.map(str::trim).filter(|p| !p.is_empty()) {
        out.push(Python::new(p, &[], "setting"));
    }
    if let Some(m) = models_dir {
        let v = venv_python(m);
        if v.is_file() {
            out.push(Python::new(v, &[], "venv"));
        }
    }
    if cfg!(windows) {
        out.push(Python::new("py", &["-3"], "path"));
        out.push(Python::new("python", &[], "path"));
    } else {
        out.push(Python::new("python3", &[], "path"));
        out.push(Python::new("python", &[], "path"));
    }
    out
}

/// What a subprocess left behind.
struct Output {
    success: bool,
    code: Option<i32>,
    stdout: Vec<u8>,
    stderr: String,
}

fn tail(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);
    let s = s.trim();
    match s.char_indices().rev().nth(STDERR_TAIL) {
        Some((i, _)) => format!("…{}", s.get(i..).unwrap_or_default()),
        None => s.to_string(),
    }
}

/// Run `program args` with `stdin`, reading at most `cap` bytes of stdout, killed after `timeout`
/// or when `cancel` is set.
fn run_process(mut cmd: Command, stdin: &[u8], cap: usize, timeout: Duration, cancel: Option<&AtomicBool>) -> Result<Output, String> {
    if cfg!(target_arch = "wasm32") {
        return Err("external engines are not available in the web build".into());
    }
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: no console window flashes up behind the app
        cmd.creation_flags(0x0800_0000);
    }
    let what = format!("{:?}", cmd.get_program());
    let mut child = cmd.spawn().map_err(|e| format!("{what}: {e}"))?;
    let input = stdin.to_vec();
    let mut sin = child.stdin.take();
    let writer = std::thread::spawn(move || {
        if let Some(s) = sin.as_mut() {
            let _ = s.write_all(&input);
        }
        drop(sin);
    });
    let mut sout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(s) = sout.as_mut() {
            let _ = s.take(cap as u64 + 1).read_to_end(&mut buf);
            // drain the rest so the child never blocks on a full pipe
            let _ = std::io::copy(s, &mut std::io::sink());
        }
        buf
    });
    let mut serr = child.stderr.take();
    let err_reader = std::thread::spawn(move || {
        let mut keep: Vec<u8> = Vec::new();
        if let Some(s) = serr.as_mut() {
            let mut chunk = [0u8; 8192];
            while let Ok(n) = s.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                keep.extend_from_slice(chunk.get(..n).unwrap_or_default());
                if keep.len() > 4 * STDERR_TAIL {
                    let cut = keep.len() - 2 * STDERR_TAIL;
                    keep.drain(..cut);
                }
            }
        }
        keep
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Ok(st),
            Ok(None) => {}
            Err(e) => break Err(format!("{what}: {e}")),
        }
        let stop = if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            Some("stopped".to_string())
        } else if start.elapsed() > timeout {
            Some(format!("{what} did not finish within {} s and was stopped", timeout.as_secs()))
        } else {
            None
        };
        if let Some(why) = stop {
            let _ = child.kill();
            let _ = child.wait();
            // a process the interpreter started may still hold the pipes open: the reader threads
            // end when it does, and nobody waits for them
            break Err(why);
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let status = status?;
    let _ = writer.join();
    let stdout = reader.join().unwrap_or_default();
    let stderr = tail(&err_reader.join().unwrap_or_default());
    if stdout.len() > cap {
        return Err(format!("{what} printed more than {cap} bytes"));
    }
    Ok(Output { success: status.success(), code: status.code(), stdout, stderr })
}

/// Environment for the bridge: downloads into the models directory, UTF-8 text.
fn bridge_env(models_dir: &Path) -> Vec<(&'static str, OsString)> {
    let cache = cache_dir(models_dir);
    vec![
        ("HF_HOME", cache.join("hf").into_os_string()),
        ("VIENEU_CACHE", cache.into_os_string()),
        ("HF_HUB_DISABLE_SYMLINKS_WARNING", "1".into()),
        ("HF_HUB_DISABLE_TELEMETRY", "1".into()),
        ("PYTHONIOENCODING", "utf-8".into()),
        ("PYTHONUTF8", "1".into()),
        ("PYTHONDONTWRITEBYTECODE", "1".into()),
    ]
}

/// Write the bridge to `<models>/vieneu/bridge/vieneu_bridge.py` (when it differs) and return its
/// path. A script file rather than `-c`: batch-file shims (pyenv-win's `python.bat`) cannot take a
/// multi-line argument, and Python puts only the script's own folder on `sys.path`.
fn bridge_script(models_dir: &Path) -> Result<PathBuf, String> {
    let folder = dir(models_dir).join("bridge");
    let path = folder.join("vieneu_bridge.py");
    if std::fs::read(&path).ok().as_deref() == Some(HELPER.as_bytes()) {
        return Ok(path);
    }
    std::fs::create_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let n = RUN.fetch_add(1, Ordering::Relaxed);
    let tmp = folder.join(format!("vieneu_bridge.{}-{n}.part", std::process::id()));
    std::fs::write(&tmp, HELPER).map_err(|e| format!("{}: {e}", tmp.display()))?;
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        // another process may have just written the same file
        if std::fs::read(&path).ok().as_deref() != Some(HELPER.as_bytes()) {
            return Err(format!("{}: {e}", path.display()));
        }
    }
    Ok(path)
}

/// `path` as text for a JSON request (a path that is not valid Unicode is refused, never altered).
fn path_text(path: &Path) -> Result<&str, TtsError> {
    path.to_str().ok_or_else(|| TtsError::Invalid(format!("the path {} is not valid Unicode; choose another models location", path.display())))
}

fn bridge_command(py: &Python, op: &str, models_dir: &Path) -> Result<Command, String> {
    let work = dir(models_dir);
    std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    let script = bridge_script(models_dir)?;
    let mut cmd = Command::new(&py.program);
    cmd.args(&py.args).arg(script).arg(op).current_dir(&work);
    for (k, v) in bridge_env(models_dir) {
        cmd.env(k, v);
    }
    Ok(cmd)
}

/// The bridge's JSON reply.
fn reply(out: &Output) -> Result<serde_json::Value, String> {
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().rev().find(|l| l.trim_start().starts_with('{')).unwrap_or_default();
    match serde_json::from_str::<serde_json::Value>(line) {
        Ok(v) if v.is_object() => Ok(v),
        _ => {
            let code = out.code.map_or_else(|| "a signal".to_string(), |c| c.to_string());
            Err(if out.stderr.is_empty() { format!("no reply (exit status {code})") } else { format!("exit status {code}: {}", out.stderr) })
        }
    }
}

/// A working interpreter and whether it has `vieneu`.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Probe {
    pub python: Python,
    pub python_version: String,
    /// The installed `vieneu` version.
    pub vieneu: Option<String>,
}

/// Ask `py` for its version and whether `vieneu` is importable (does not import it).
pub fn probe(py: &Python, models_dir: &Path) -> Result<Probe, String> {
    let out = run_process(bridge_command(py, "probe", models_dir)?, b"", 64 * 1024, Duration::from_secs(30), None)?;
    let v = reply(&out)?;
    let version = v.get("python").and_then(|x| x.as_str()).unwrap_or_default().to_string();
    if !version.split('.').next().and_then(|m| m.parse::<u32>().ok()).is_some_and(|m| m >= 3) {
        return Err(format!("not a Python 3 interpreter (it says {version:?})"));
    }
    let vieneu = v.get("vieneu").and_then(|x| x.as_str()).map(|s| s.chars().take(64).collect());
    Ok(Probe { python: py.clone(), python_version: version.chars().take(32).collect(), vieneu })
}

/// What [`detect`] found.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// The first interpreter with `vieneu`.
    pub ready: Option<Probe>,
    /// The first working Python 3 (used to create the environment when `vieneu` is missing).
    pub python: Option<Probe>,
    /// Each interpreter tried and why it was not used.
    pub tried: Vec<(String, String)>,
}

impl Status {
    pub fn available(&self) -> bool {
        self.ready.is_some()
    }
}

/// Find an interpreter that has `vieneu` ([`candidates`] in order).
pub fn detect(configured: Option<&str>, models_dir: &Path) -> Status {
    let mut st = Status::default();
    for py in candidates(configured, Some(models_dir)) {
        match probe(&py, models_dir) {
            Ok(p) if p.vieneu.is_some() => {
                if st.python.is_none() {
                    st.python = Some(p.clone());
                }
                st.ready = Some(p);
                break;
            }
            Ok(p) => {
                st.tried.push((py.display(), format!("Python {} without the `vieneu` package", p.python_version)));
                if st.python.is_none() {
                    st.python = Some(p);
                }
            }
            Err(e) => st.tried.push((py.display(), e)),
        }
    }
    st
}

/// Exact steps to install VieNeu-TTS for this machine.
pub fn install_steps(st: &Status, models_dir: &Path) -> Vec<String> {
    let venv = venv_dir(models_dir);
    let vpy = venv_python(models_dir);
    let q = |p: &Path| {
        let s = p.to_string_lossy();
        if s.contains(' ') { format!("\"{s}\"") } else { s.into_owned() }
    };
    let mut out = Vec::new();
    let py = match &st.python {
        Some(p) => p.python.display(),
        None => {
            out.push(if cfg!(windows) {
                "Install Python 3.10 or newer from python.org (tick \"Add python.exe to PATH\"), then open a new terminal".to_string()
            } else {
                "Install Python 3.10 or newer (python3 and its venv module)".to_string()
            });
            if cfg!(windows) { "py -3".to_string() } else { "python3".to_string() }
        }
    };
    out.push(format!("{py} -m venv {}", q(&venv)));
    out.push(format!("{} -m pip install {PACKAGE}", q(&vpy)));
    out.push("Then run tts.setupVieneu (it downloads the voice model, about 0.6 GB, into the models directory and lists the voices)".to_string());
    out
}

/// Create `<models>/vieneu/venv` with `python` and `pip install vieneu` into it. `log` gets
/// progress lines; `cancel` stops it.
pub fn install(python: &Python, models_dir: &Path, cancel: Option<&AtomicBool>, log: &mut dyn FnMut(&str)) -> Result<Python, String> {
    let venv = venv_dir(models_dir);
    let vpy = venv_python(models_dir);
    let env_for = |cmd: &mut Command| {
        for (k, v) in bridge_env(models_dir) {
            cmd.env(k, v);
        }
        cmd.current_dir(models_dir);
    };
    std::fs::create_dir_all(models_dir).map_err(|e| format!("{}: {e}", models_dir.display()))?;
    if !vpy.is_file() {
        log(&format!("Creating a Python environment in {}", venv.display()));
        let mut cmd = Command::new(&python.program);
        cmd.args(&python.args).arg("-m").arg("venv").arg(&venv);
        env_for(&mut cmd);
        let out = run_process(cmd, b"", MAX_STDOUT, Duration::from_secs(600), cancel)?;
        if !out.success || !vpy.is_file() {
            return Err(format!("{} -m venv failed: {}", python.display(), out.stderr));
        }
    }
    log("Installing vieneu with pip (this can take several minutes)");
    let mut cmd = Command::new(&vpy);
    cmd.args(["-m", "pip", "install", "--disable-pip-version-check", PACKAGE]);
    env_for(&mut cmd);
    let out = run_process(cmd, b"", MAX_STDOUT, Duration::from_secs(3600), cancel)?;
    if !out.success {
        return Err(format!("pip install {PACKAGE} failed: {}", out.stderr));
    }
    Ok(Python::new(vpy, &[], "venv"))
}

/// A VieNeu preset voice.
#[derive(Clone, Debug, PartialEq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VieneuVoice {
    /// The preset's name (`Hải Đăng`); the voice id is `vieneu:<name>`.
    pub name: String,
    #[serde(default)]
    pub gender: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
}

impl VieneuVoice {
    pub fn id(&self) -> String {
        format!("{VOICE_PREFIX}{}", self.name)
    }

    /// The voice as `tts.voices` lists it (the same keys as [`crate::VoiceInfo`]).
    pub fn info_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id(),
            "name": self.name,
            "language": LANGUAGE,
            "gender": self.gender.as_deref().filter(|g| matches!(*g, "female" | "male")),
            "engine": "vieneu",
            "description": self.description.clone().unwrap_or_else(|| "VieNeu-TTS voice".into()),
            "region": self.region,
            "license": LICENSE,
            "installed": true,
        })
    }
}

fn clean(s: Option<String>, max: usize) -> Option<String> {
    s.map(|s| s.chars().filter(|c| !c.is_control()).take(max).collect::<String>()).filter(|s| !s.trim().is_empty())
}

fn sanitize_voices(list: Vec<VieneuVoice>) -> Vec<VieneuVoice> {
    let mut out: Vec<VieneuVoice> = Vec::new();
    for v in list.into_iter().take(MAX_VOICES) {
        let Some(name) = clean(Some(v.name), 120) else { continue };
        if out.iter().any(|o| o.name == name) {
            continue;
        }
        out.push(VieneuVoice { name, gender: clean(v.gender, 16), description: clean(v.description, 300), region: clean(v.region, 40) });
    }
    out
}

/// The voices [`prepare`] cached, if any.
pub fn cached_voices(models_dir: &Path) -> Option<Vec<VieneuVoice>> {
    let f = voices_file(models_dir);
    if std::fs::metadata(&f).ok()?.len() > 4 << 20 {
        return None;
    }
    let list: Vec<VieneuVoice> = serde_json::from_slice(&std::fs::read(f).ok()?).ok()?;
    Some(sanitize_voices(list)).filter(|l| !l.is_empty())
}

fn mode() -> String {
    std::env::var("VIENEU_MODE").ok().filter(|m| matches!(m.as_str(), "v3turbo" | "v3nano")).unwrap_or_else(|| DEFAULT_MODE.into())
}

fn engine_err(e: String) -> TtsError {
    TtsError::Engine(e)
}

/// Load the model with `python` (downloading it the first time) and cache the voice list.
pub fn prepare(python: &Python, models_dir: &Path, cancel: Option<&AtomicBool>) -> Result<Vec<VieneuVoice>, TtsError> {
    let cache = cache_dir(models_dir);
    let req = serde_json::json!({"mode": mode(), "cache": path_text(&cache)?});
    let cmd = bridge_command(python, "voices", models_dir).map_err(engine_err)?;
    let out = run_process(cmd, req.to_string().as_bytes(), MAX_STDOUT, Duration::from_secs(3 * 3600), cancel).map_err(engine_err)?;
    let v = reply(&out).map_err(engine_err)?;
    if v.get("ok").and_then(|x| x.as_bool()) != Some(true) {
        return Err(bridge_error(&v));
    }
    let list: Vec<VieneuVoice> = serde_json::from_value(v.get("voices").cloned().unwrap_or_default()).map_err(|e| engine_err(format!("voice list: {e}")))?;
    let list = sanitize_voices(list);
    if list.is_empty() {
        return Err(engine_err("VieNeu-TTS reported no voices".into()));
    }
    let f = voices_file(models_dir);
    let json = serde_json::to_vec_pretty(&list).map_err(|e| engine_err(e.to_string()))?;
    let tmp = f.with_extension("json.part");
    std::fs::write(&tmp, json).and_then(|()| std::fs::rename(&tmp, &f)).map_err(|e| engine_err(format!("{}: {e}", f.display())))?;
    Ok(list)
}

fn bridge_error(v: &serde_json::Value) -> TtsError {
    let msg = v.get("error").and_then(|x| x.as_str()).unwrap_or("VieNeu-TTS failed").chars().take(2000).collect::<String>();
    match v.get("kind").and_then(|x| x.as_str()) {
        Some("missing") => TtsError::EngineMissing(msg),
        Some("unknownVoice") => {
            let names: Vec<String> = v
                .get("voices")
                .and_then(|x| x.as_array())
                .map(|a| a.iter().filter_map(|n| n.as_str()).take(MAX_VOICES).map(|n| format!("{VOICE_PREFIX}{n}")).collect())
                .unwrap_or_default();
            TtsError::UnknownVoice(if names.is_empty() { msg } else { format!("{msg}; VieNeu voices: {}", names.join(", ")) })
        }
        _ => TtsError::Engine(msg),
    }
}

/// What to say and in which voice.
#[derive(Clone, Debug)]
pub struct Request<'a> {
    pub text: &'a str,
    /// A preset name (`Hải Đăng`), without the `vieneu:` prefix.
    pub voice: Option<&'a str>,
    /// A clip whose voice is cloned (instead of `voice`).
    pub reference: Option<&'a Path>,
    pub params: Params,
}

/// Seconds a narrator needs for `text`: Vietnamese is written one syllable per word; digits are
/// read one by one; about 4.5 syllables a second.
pub fn expected_seconds(text: &str, pace: f64) -> f64 {
    let mut syllables = 0usize;
    for w in text.split_whitespace() {
        let digits = w.chars().filter(char::is_ascii_digit).count();
        if digits > 0 {
            syllables += digits;
        } else if w.chars().any(char::is_alphanumeric) {
            // long Latin words (names, English) take more than one syllable
            syllables += 1 + w.chars().filter(|c| c.is_alphabetic()).count() / 7;
        }
    }
    let pace = if pace.is_finite() && pace > 0.0 { pace } else { 1.0 };
    (syllables as f64 / 4.5).max(0.4)
        / pace.max(crate::MIN_PACE)
        // VieNeu adds short pauses at punctuation itself
        + 0.15 * text.matches(['.', ',', '!', '?', ';', ':']).count() as f64
}

static RUN: AtomicU64 = AtomicU64::new(0);

/// A temporary directory for one request, removed when dropped.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn read_samples(path: &Path, samples: u64, max: u64) -> Result<Vec<f32>, TtsError> {
    if samples > max {
        return Err(TtsError::AudioTooLong);
    }
    let len = std::fs::metadata(path).map_err(|e| engine_err(format!("{}: {e}", path.display())))?.len();
    if Some(len) != samples.checked_mul(4) {
        return Err(engine_err(format!("{}: {len} bytes for {samples} samples", path.display())));
    }
    let bytes = std::fs::read(path).map_err(|e| engine_err(format!("{}: {e}", path.display())))?;
    let out: Vec<f32> = bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
    if out.iter().any(|x| !x.is_finite()) {
        return Err(TtsError::Model("VieNeu-TTS produced invalid samples".into()));
    }
    Ok(out)
}

/// Check a clip for cloning: an existing audio file of a sane size.
pub fn check_reference(path: &Path) -> Result<(), TtsError> {
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
    if !matches!(ext.as_str(), "wav" | "mp3" | "flac" | "ogg" | "m4a") {
        return Err(TtsError::Invalid("the reference clip must be a .wav, .mp3, .flac, .ogg or .m4a file".into()));
    }
    let meta = std::fs::metadata(path).map_err(|e| TtsError::Invalid(format!("reference clip {}: {e}", path.display())))?;
    if !meta.is_file() || meta.len() == 0 {
        return Err(TtsError::Invalid(format!("reference clip {} is not an audio file", path.display())));
    }
    if meta.len() > MAX_REFERENCE_BYTES {
        return Err(TtsError::Invalid(format!(
            "reference clip {} is larger than {} MB; use 3 to 30 seconds of speech",
            path.display(),
            MAX_REFERENCE_BYTES >> 20
        )));
    }
    Ok(())
}

/// Speak `req` with VieNeu-TTS through `python`. Pause markers become exact silence; pace and pitch
/// are applied afterwards (VieNeu has neither). `cancel` stops the subprocess.
pub fn synthesize(python: &Python, models_dir: &Path, req: &Request, cancel: Option<&AtomicBool>) -> Result<Audio, TtsError> {
    check_text(req.text)?;
    req.params.check()?;
    if let Some(r) = req.reference {
        check_reference(r)?;
    } else if req.voice.is_none_or(|v| v.trim().is_empty()) {
        return Err(TtsError::UnknownVoice(format!("{VOICE_PREFIX} needs a voice name (tts.voices lists them)")));
    }
    let pieces = script::parse(req.text);
    let speech: Vec<&str> = pieces
        .iter()
        .filter_map(|p| match p {
            script::Segment::Speech(t) if t.chars().any(char::is_alphanumeric) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    if speech.len() > MAX_SEGMENTS {
        return Err(TtsError::Invalid(format!("the script has more than {MAX_SEGMENTS} pause markers")));
    }
    let segments: Vec<serde_json::Value> = speech
        .iter()
        .map(|t| {
            let want = expected_seconds(t, 1.0);
            serde_json::json!({"text": t.trim(), "expectedSeconds": want, "minSeconds": want * 0.45, "maxSeconds": want * 2.2 + 1.0})
        })
        .collect();
    let n = RUN.fetch_add(1, Ordering::Relaxed);
    let tmp = TempDir(dir(models_dir).join("tmp").join(format!("{}-{n}", std::process::id())));
    let _ = std::fs::remove_dir_all(&tmp.0);
    std::fs::create_dir_all(&tmp.0).map_err(|e| engine_err(format!("{}: {e}", tmp.0.display())))?;
    let request = serde_json::json!({
        "mode": mode(),
        "cache": path_text(&cache_dir(models_dir))?,
        "voice": req.voice.map(str::trim),
        "reference": req.reference.map(path_text).transpose()?,
        "segments": segments,
        "takes": TAKES,
        "out": path_text(&tmp.0)?,
    });
    let chars = req.text.chars().count() as u64;
    let timeout = Duration::from_secs((900 + chars / 2).min(6 * 3600));
    let cmd = bridge_command(python, "synth", models_dir).map_err(engine_err)?;
    let out = run_process(cmd, request.to_string().as_bytes(), MAX_STDOUT, timeout, cancel).map_err(engine_err)?;
    let v = reply(&out).map_err(engine_err)?;
    if v.get("ok").and_then(|x| x.as_bool()) != Some(true) {
        return Err(bridge_error(&v));
    }
    let rate = v
        .get("sampleRate")
        .and_then(|x| x.as_u64())
        .filter(|r| (8_000..=192_000).contains(r))
        .ok_or_else(|| engine_err("VieNeu-TTS reported no valid sample rate".into()))? as u32;
    let reported = v.get("segments").and_then(|x| x.as_array()).cloned().unwrap_or_default();
    if reported.len() != speech.len() {
        return Err(engine_err(format!("VieNeu-TTS returned {} segments for {}", reported.len(), speech.len())));
    }
    let max = (MAX_SECONDS * f64::from(rate)) as u64;
    let mut audio: Vec<f32> = Vec::new();
    let mut next = 0usize;
    for p in &pieces {
        match p {
            script::Segment::Pause { millis } => {
                let n = (u64::from(*millis) * u64::from(rate) / 1000) as usize;
                audio.resize(audio.len().saturating_add(n), 0.0);
            }
            script::Segment::Speech(t) if t.chars().any(char::is_alphanumeric) => {
                let samples = reported.get(next).and_then(|s| s.get("samples")).and_then(|x| x.as_u64()).unwrap_or(0);
                let s = read_samples(&tmp.0.join(format!("seg-{next}.f32")), samples, max)?;
                audio.extend_from_slice(&s);
                next += 1;
            }
            script::Segment::Speech(_) => {}
        }
        if audio.len() as u64 > max {
            return Err(TtsError::AudioTooLong);
        }
    }
    if audio.is_empty() {
        return Err(TtsError::Empty);
    }
    if (req.params.pace - 1.0).abs() > 1e-6 {
        audio = crate::dsp::stretch(&audio, req.params.pace, rate);
    }
    if req.params.semitones.abs() > 1e-6 {
        audio = crate::dsp::pitch_shift(audio, req.params.semitones, rate);
    }
    crate::dsp::headroom(&mut audio);
    Ok(Audio { samples: audio, sample_rate: rate })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expected_lengths_are_sane() {
        let a = expected_seconds("Xin chào các bạn, hôm nay chúng ta nói về video.", 1.0);
        assert!((2.0..5.0).contains(&a), "{a}");
        assert!(expected_seconds("Năm 2024", 1.0) > expected_seconds("Năm nay", 1.0));
        assert!(expected_seconds("a", 2.0) > 0.0);
        for p in [f64::NAN, f64::INFINITY, -1.0, 0.0] {
            assert!(expected_seconds("một hai ba", p).is_finite());
        }
        let long = "chữ ".repeat(20_000);
        assert!(expected_seconds(&long, 1.0).is_finite());
    }

    #[test]
    fn voice_cache_is_sanitized() {
        let list = vec![
            VieneuVoice { name: "Hải Đăng".into(), gender: Some("male".into()), description: None, region: None },
            VieneuVoice { name: "Hải Đăng".into(), gender: None, description: None, region: None },
            VieneuVoice { name: "  ".into(), gender: None, description: None, region: None },
            VieneuVoice { name: "a\u{0}b\nc".into(), gender: Some("x".repeat(100)), description: Some("d".repeat(1000)), region: None },
        ];
        let out = sanitize_voices(list);
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].name, "abc");
        assert!(out[1].description.as_ref().unwrap().len() <= 300);
        assert_eq!(out[0].info_json()["gender"], "male");
        assert_eq!(out[1].info_json()["gender"], serde_json::Value::Null);
        assert_eq!(out[0].id(), "vieneu:Hải Đăng");
    }

    #[test]
    fn candidates_end_with_the_path_interpreters() {
        let c = candidates(Some("  "), None);
        assert!(c.iter().all(|p| p.source != "setting"), "blank setting ignored");
        let c = candidates(Some("/opt/py/bin/python3"), None);
        assert!(c.iter().any(|p| p.source == "setting"));
        assert_eq!(c.last().map(|p| p.source), Some("path"));
        assert_eq!(Python::new("C:/Program Files/Python/python.exe", &[], "setting").display(), "\"C:/Program Files/Python/python.exe\"");
    }

    #[test]
    fn a_missing_interpreter_is_reported_not_fatal() {
        let dir = std::env::temp_dir().join(format!("filmagic-vieneu-missing-{}", std::process::id()));
        let py = Python::new(dir.join("no-such-python"), &[], "setting");
        assert!(probe(&py, &dir).is_err());
        let req = Request { text: "Xin chào.", voice: Some("Hải Đăng"), reference: None, params: Params::default() };
        assert!(matches!(synthesize(&py, &dir, &req, None), Err(TtsError::Engine(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A stand-in for a Python with `vieneu`: a shell script that speaks the bridge's protocol
    /// (`sh fake.sh <bridge.py> <op>`, request on stdin). `synth` writes `samples` zero samples
    /// per segment at 48 kHz; `body` replaces the synth reply for hostile cases. It keeps the
    /// last request in `last-request.json` (in its working directory, `<models>/vieneu`).
    #[cfg(unix)]
    fn fake(root: &Path, samples: usize, body: Option<&str>) -> Python {
        let script = root.join("fake-python.sh");
        let synth = body.map(str::to_string).unwrap_or_else(|| {
            format!(
                r#"out=$(printf '%s' "$req" | sed -n 's/.*"out":"\([^"]*\)".*/\1/p')
n=$(printf '%s' "$req" | grep -o '"expectedSeconds"' | wc -l)
i=0; segs=""
while [ "$i" -lt "$n" ]; do
  head -c {bytes} /dev/zero > "$out/seg-$i.f32"
  segs="$segs${{segs:+,}}{{\"samples\":{samples},\"takes\":1}}"
  i=$((i+1))
done
echo "{{\"ok\":true,\"sampleRate\":48000,\"segments\":[$segs]}}""#,
                bytes = samples * 4
            )
        });
        let text = format!(
            r#"#!/bin/sh
for a in "$@"; do op="$a"; done
if [ "$op" = probe ]; then echo '{{"ok":true,"python":"3.12.1","vieneu":"3.8.4"}}'; exit 0; fi
req=$(cat)
printf '%s' "$req" > last-request.json
if [ "$op" = voices ]; then echo '{{"ok":true,"sampleRate":48000,"voices":[{{"name":"Hải Đăng","label":"x","gender":"male","description":"Nam"}},{{"name":"Mai Anh","gender":"female"}}]}}'; exit 0; fi
{synth}
"#
        );
        std::fs::write(&script, text).unwrap();
        Python { program: "sh".into(), args: vec![script.to_string_lossy().into_owned()], source: "setting" }
    }

    #[cfg(unix)]
    fn last_request(root: &Path) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(dir(root).join("last-request.json")).unwrap()).unwrap()
    }

    #[cfg(unix)]
    fn say(py: &Python, root: &Path, text: &str, voice: Option<&str>, pace: f64) -> Result<Audio, TtsError> {
        synthesize(py, root, &Request { text, voice, reference: None, params: Params { pace, semitones: 0.0 } }, None)
    }

    #[cfg(unix)]
    #[test]
    fn speaks_through_the_bridge_with_exact_pauses() {
        let root = tempfile::tempdir().unwrap();
        let py = fake(root.path(), 24_000, None);
        let p = probe(&py, root.path()).unwrap();
        assert_eq!((p.python_version.as_str(), p.vieneu.as_deref()), ("3.12.1", Some("3.8.4")));
        let a = say(&py, root.path(), "Xin chào các bạn. [pause 500ms] Hôm nay trời đẹp.", Some("Hải Đăng"), 1.0).unwrap();
        assert_eq!(a.sample_rate, 48_000);
        assert_eq!(a.samples.len(), 24_000 + 24_000 + 24_000, "two segments and exactly 0.5 s of pause");
        let req = last_request(root.path());
        assert_eq!(req["voice"], "Hải Đăng");
        assert_eq!(req["segments"][0]["text"], "Xin chào các bạn.");
        assert_eq!(req["segments"][1]["text"], "Hôm nay trời đẹp.");
        assert!(req["segments"][0]["minSeconds"].as_f64().unwrap() < req["segments"][0]["maxSeconds"].as_f64().unwrap());
        assert!(req["cache"].as_str().unwrap().ends_with("cache"), "downloads stay in the models directory");
        // pace is applied afterwards: twice as fast, half as long
        let fast = say(&py, root.path(), "Xin chào các bạn.", Some("Hải Đăng"), 2.0).unwrap();
        assert_eq!(fast.samples.len(), 12_000);
        assert!(!dir(root.path()).join("tmp").read_dir().unwrap().any(|_| true), "temporary files are removed");
    }

    #[cfg(unix)]
    #[test]
    fn hostile_text_reaches_the_engine_unchanged() {
        let root = tempfile::tempdir().unwrap();
        let py = fake(root.path(), 4_800, None);
        let nasty = "Chào \"bạn\" \\ 'x' $(rm -rf /) `id` %s {}\"out\":\"/etc\" \u{202e}ngược 🎬 \t tab";
        say(&py, root.path(), nasty, Some("Hải Đăng"), 1.0).unwrap();
        assert_eq!(last_request(root.path())["segments"][0]["text"], nasty.trim());
        assert!(!Path::new("/etc/seg-0.f32").exists());
        // a voice name is data too
        say(&py, root.path(), "Một hai ba.", Some("\"; rm -rf / #"), 1.0).unwrap();
        assert_eq!(last_request(root.path())["voice"], "\"; rm -rf / #");
        // a long script: many pause markers, the 64 KB limit, never a spawn past it
        let long = "Một hai ba bốn năm. [pause 10ms] ".repeat(1_500);
        assert!(long.len() < crate::MAX_TEXT_BYTES, "{}", long.len());
        let a = say(&py, root.path(), &long, Some("Hải Đăng"), 1.0).unwrap();
        assert_eq!(last_request(root.path())["segments"].as_array().unwrap().len(), 1_500);
        assert!(a.samples.len() > 1_500 * 4_800);
        let _ = std::fs::remove_file(dir(root.path()).join("last-request.json"));
        assert_eq!(say(&py, root.path(), &"a ".repeat(40_000), Some("Hải Đăng"), 1.0).err(), Some(TtsError::TextTooLong));
        assert_eq!(say(&py, root.path(), " [pause 1s] … ", Some("Hải Đăng"), 1.0).err(), Some(TtsError::Empty));
        assert!(!dir(root.path()).join("last-request.json").exists(), "refused before Python ran");
        assert!(matches!(say(&py, root.path(), "Chào.", Some("  "), 1.0), Err(TtsError::UnknownVoice(_))));
        assert!(matches!(say(&py, root.path(), "Chào.", None, 1.0), Err(TtsError::UnknownVoice(_))));
        for pace in [f64::NAN, 0.0, -1.0, 9.0] {
            assert!(matches!(say(&py, root.path(), "Chào.", Some("Hải Đăng"), pace), Err(TtsError::Invalid(_))), "{pace}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn bridge_failures_become_errors() {
        let cases: &[(&str, fn(&TtsError) -> bool)] = &[
            (
                r#"echo '{"ok":false,"kind":"unknownVoice","error":"unknown VieNeu voice","voices":["Hải Đăng","Mai Anh"]}'; exit 1"#,
                |e| matches!(e, TtsError::UnknownVoice(m) if m.contains("vieneu:Mai Anh")),
            ),
            (r#"echo '{"ok":false,"kind":"missing","error":"no module named vieneu"}'; exit 1"#, |e| matches!(e, TtsError::EngineMissing(_))),
            ("echo 'Traceback: boom' >&2; exit 3", |e| matches!(e, TtsError::Engine(m) if m.contains("boom") && m.contains('3'))),
            ("echo 'not json at all'", |e| matches!(e, TtsError::Engine(_))),
            (r#"echo '{"ok":true,"sampleRate":48000,"segments":[]}'"#, |e| matches!(e, TtsError::Engine(m) if m.contains("segments"))),
            (r#"echo '{"ok":true,"sampleRate":0,"segments":[{"samples":1}]}'"#, |e| matches!(e, TtsError::Engine(m) if m.contains("sample rate"))),
            (r#"echo '{"ok":true,"sampleRate":"48000","segments":[{"samples":1}]}'"#, |e| matches!(e, TtsError::Engine(_))),
            // the file is shorter than reported
            (
                r#"out=$(printf '%s' "$req" | sed -n 's/.*"out":"\([^"]*\)".*/\1/p'); head -c 10 /dev/zero > "$out/seg-0.f32"; echo '{"ok":true,"sampleRate":48000,"segments":[{"samples":100}]}'"#,
                |e| matches!(e, TtsError::Engine(m) if m.contains("bytes")),
            ),
            // NaN samples
            (
                r#"out=$(printf '%s' "$req" | sed -n 's/.*"out":"\([^"]*\)".*/\1/p'); printf '\000\000\300\177' > "$out/seg-0.f32"; echo '{"ok":true,"sampleRate":48000,"segments":[{"samples":1}]}'"#,
                |e| matches!(e, TtsError::Model(_)),
            ),
            // an absurd length is refused before anything is read
            (r#"echo '{"ok":true,"sampleRate":48000,"segments":[{"samples":18446744073709551615}]}'"#, |e| *e == TtsError::AudioTooLong),
            // a missing file
            (r#"echo '{"ok":true,"sampleRate":48000,"segments":[{"samples":5}]}'"#, |e| matches!(e, TtsError::Engine(_))),
        ];
        for (body, ok) in cases {
            let root = tempfile::tempdir().unwrap();
            let py = fake(root.path(), 0, Some(body));
            let e = say(&py, root.path(), "Xin chào.", Some("Hải Đăng"), 1.0).unwrap_err();
            assert!(ok(&e), "{body}: {e:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn prepare_caches_the_voices_and_detect_finds_the_interpreter() {
        let root = tempfile::tempdir().unwrap();
        let py = fake(root.path(), 0, None);
        assert!(cached_voices(root.path()).is_none());
        let v = prepare(&py, root.path(), None).unwrap();
        assert_eq!(v.iter().map(|v| v.name.as_str()).collect::<Vec<_>>(), ["Hải Đăng", "Mai Anh"]);
        assert_eq!(cached_voices(root.path()), Some(v));
        // a corrupt cache is ignored, not trusted
        std::fs::write(voices_file(root.path()), b"{not json").unwrap();
        assert!(cached_voices(root.path()).is_none());
        std::fs::write(voices_file(root.path()), b"[]").unwrap();
        assert!(cached_voices(root.path()).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn a_stuck_engine_can_be_stopped() {
        let root = tempfile::tempdir().unwrap();
        let py = fake(root.path(), 0, Some("sleep 30"));
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let c = cancel.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            c.store(true, Ordering::Relaxed);
        });
        let t0 = Instant::now();
        let req = Request { text: "Xin chào.", voice: Some("Hải Đăng"), reference: None, params: Params::default() };
        let e = synthesize(&py, root.path(), &req, Some(&cancel)).unwrap_err();
        assert!(matches!(&e, TtsError::Engine(m) if m == "stopped"), "{e:?}");
        assert!(t0.elapsed() < Duration::from_secs(10));
        t.join().unwrap();
    }

    /// `json!` of a `Path` unwraps its conversion to text: a non-Unicode path must be an error.
    #[cfg(unix)]
    #[test]
    fn a_non_unicode_models_path_is_an_error_not_a_panic() {
        use std::os::unix::ffi::OsStrExt;
        let root = tempfile::tempdir().unwrap();
        let py = fake(root.path(), 480, None);
        let odd = root.path().join(std::ffi::OsStr::from_bytes(b"models-\xff"));
        let r = std::panic::catch_unwind(|| say(&py, &odd, "Xin chào.", Some("Hải Đăng"), 1.0));
        assert!(matches!(r, Ok(Err(TtsError::Invalid(_)))), "{r:?}");
        let r = std::panic::catch_unwind(|| prepare(&py, &odd, None));
        assert!(matches!(r, Ok(Err(TtsError::Invalid(_)))), "{r:?}");
    }

    #[cfg(unix)]
    #[test]
    fn the_bridge_file_is_written_once_and_kept_current() {
        let root = tempfile::tempdir().unwrap();
        let p = bridge_script(root.path()).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), HELPER);
        std::fs::write(&p, "tampered").unwrap();
        assert_eq!(std::fs::read_to_string(bridge_script(root.path()).unwrap()).unwrap(), HELPER, "rewritten when it differs");
        assert_eq!(std::fs::read_dir(p.parent().unwrap()).unwrap().count(), 1, "no part files left");
    }

    #[cfg(unix)]
    #[test]
    fn a_cloned_voice_sends_the_reference_clip() {
        let root = tempfile::tempdir().unwrap();
        let py = fake(root.path(), 480, None);
        let clip = root.path().join("speaker.wav");
        std::fs::write(&clip, b"RIFF").unwrap();
        let req = Request { text: "Xin chào.", voice: None, reference: Some(&clip), params: Params::default() };
        synthesize(&py, root.path(), &req, None).unwrap();
        let r = last_request(root.path());
        assert_eq!(r["reference"], clip.to_string_lossy().as_ref());
        assert_eq!(r["voice"], serde_json::Value::Null);
    }

    #[cfg(unix)]
    #[test]
    fn install_steps_name_the_venv() {
        let root = tempfile::tempdir().unwrap();
        let st = Status::default();
        let steps = install_steps(&st, root.path());
        assert!(steps.iter().any(|s| s.contains("-m venv") && s.contains("venv")), "{steps:?}");
        assert!(steps.iter().any(|s| s.contains("pip install vieneu")), "{steps:?}");
        assert!(steps.first().is_some_and(|s| s.contains("Python 3.10")), "no Python found: install it first");
    }

    #[test]
    fn references_are_checked() {
        let dir = std::env::temp_dir().join(format!("filmagic-vieneu-ref-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(check_reference(&dir.join("x.txt")).is_err());
        assert!(check_reference(&dir.join("missing.wav")).is_err());
        let empty = dir.join("empty.wav");
        std::fs::write(&empty, b"").unwrap();
        assert!(check_reference(&empty).is_err());
        std::fs::write(&empty, b"RIFF....").unwrap();
        assert!(check_reference(&empty).is_ok());
        assert!(check_reference(&dir).is_err(), "a folder named like a clip");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
