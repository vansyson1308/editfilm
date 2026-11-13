//! VieNeu-TTS: offline Vietnamese voices (<https://github.com/pnnbao97/VieNeu-TTS>, Apache-2.0;
//! the model weights it downloads, VieNeu-TTS-v3-Turbo and MOSS-Audio-Tokenizer-Nano, are
//! Apache-2.0 too).
//!
//! An optional **external engine**: Filmagic never links, bundles or ships it, and installs it only
//! when asked (`tts.setupVieneu {"install": true}`). VieNeu exists only as a Python package, so
//! Filmagic runs a Python interpreter that has `vieneu` installed as a subprocess, with the small
//! bridge in `crates/tts/python/vieneu_bridge.py`. The owner asked for VieNeu to be integrated this
//! way (issue #20, "the way srt-whiteboard-animation does"), so this Python bridge is
//! owner-approved for this optional engine. The bridge calls `vieneu`'s documented methods only
//! (`Vieneu()`, `list_preset_voices()`, `get_preset_voice()`, `infer()`). Requests go in as JSON
//! on stdin. Each speech segment comes back as raw float32 samples in a file. Everything else
//! happens here: pause markers, how long a take may plausibly be, pace and pitch, headroom.
//!
//! Two directories ([`Dirs`]):
//! - **engine** (`<data dir>/vieneu`, `%LOCALAPPDATA%\Filmagic\vieneu` on Windows): what is
//!   *executed*, meaning the Python environment [`install`] creates (`venv/`) and the bridge
//!   (`bridge/`), plus scratch files. It is not under the models folder, which an agent can move,
//!   so no command can point Filmagic at a program to run.
//! - **models** (`<models>/vieneu`): data only, meaning the model `vieneu` downloads (`cache/`,
//!   through `HF_HOME`) and the voice list (`voices.json`).
//!
//! - **Interpreter**: `FILMAGIC_VIENEU_PYTHON` (set by a person when starting Filmagic), else the
//!   environment in the engine directory, else `py -3` / `python` (Windows) or `python3` /
//!   `python`. No setting or command parameter can name one. [`detect`] takes the first that can
//!   import `vieneu`, and from then on runs the interpreter's own `sys.executable` (so a
//!   `python.bat` shim is not what gets stopped on cancel).
//! - The child gets a scrubbed environment (no `FILMAGIC_*` secrets, no `PYTHON*` variables) and
//!   runs with `-E -s -X utf8` (the bridge) or `-I` (venv and pip) in an empty working directory.
//! - **Voices**: cached by [`prepare`], so listing them never starts Python. Ids are
//!   `vieneu:<name>` ([`VOICE_PREFIX`]). Names and text are NFC-normalised.
//! - **Cloning**: [`Request::reference`] clones the voice of a reference clip instead.
//! - **Output**: the model's own rate: 48 kHz for `v3turbo` (the default), 24 kHz for `v3nano`.
//! - **Timing**: VieNeu returns audio only, no word timestamps.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;
use unicode_normalization::UnicodeNormalization;

use crate::{Audio, MAX_SECONDS, Params, TtsError, check_text, script};

/// The bridge the interpreter runs.
pub const HELPER: &str = include_str!("../python/vieneu_bridge.py");
/// Directory name, both under the models directory (data) and as the engine directory.
pub const DIR_NAME: &str = "vieneu";
/// Voice ids are `vieneu:<preset name>`.
pub const VOICE_PREFIX: &str = "vieneu:";
/// The voice id that clones a reference clip.
pub const CLONE_VOICE: &str = "vieneu-clone";
/// BCP 47 tag of every VieNeu voice.
pub const LANGUAGE: &str = "vi-VN";
/// The pip package.
pub const PACKAGE: &str = "vieneu";
/// The exact version [`install`] installs (tested with Filmagic's bridge).
pub const PACKAGE_SPEC: &str = "vieneu==3.8.4";
pub const LICENSE: &str = "Apache-2.0 (VieNeu-TTS by pnnbao97; models VieNeu-TTS-v3-Turbo and MOSS-Audio-Tokenizer-Nano, Apache-2.0)";
pub const SOURCE: &str = "https://github.com/pnnbao97/VieNeu-TTS";
/// Environment variable naming the interpreter. Only a person starting Filmagic can set it.
pub const PYTHON_ENV: &str = "FILMAGIC_VIENEU_PYTHON";
/// The sentence "Hear this voice" speaks with a VieNeu voice.
pub const SAMPLE_SENTENCE: &str = "Xin chào! Đây là giọng của tôi khi đọc kịch bản của bạn.";
/// The model `vieneu` loads (`VIENEU_MODE=v3nano` picks the smaller one for weak CPUs).
pub const DEFAULT_MODE: &str = "v3turbo";
/// What the bridge puts before its one-line JSON reply (library output can't be taken for it).
const REPLY_PREFIX: &str = "FILMAGIC-REPLY ";

/// Most bytes of stdout read from the bridge.
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
    /// Where it came from: `env`, `host`, `venv` or `path`.
    pub source: &'static str,
}

impl Python {
    pub fn new(program: impl Into<PathBuf>, args: &[&str], source: &'static str) -> Self {
        Python { program: program.into(), args: args.iter().map(|a| a.to_string()).collect(), source }
    }

    /// As typed in a shell.
    pub fn display(&self) -> String {
        let p = self.program.to_string_lossy();
        let p = if p.contains(' ') { format!("\"{p}\"") } else { p.into_owned() };
        std::iter::once(p).chain(self.args.iter().cloned()).collect::<Vec<_>>().join(" ")
    }
}

/// Where VieNeu's files go (see the module docs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dirs {
    /// What is executed: `venv/`, `bridge/`, `work/` (an empty working directory), `tmp/`.
    pub engine: PathBuf,
    /// The models directory; VieNeu's data is in its `vieneu/` folder.
    pub models: PathBuf,
}

impl Dirs {
    /// The environment [`install`] creates.
    pub fn venv(&self) -> PathBuf {
        self.engine.join("venv")
    }

    /// Its interpreter.
    pub fn venv_python(&self) -> PathBuf {
        if cfg!(windows) { self.venv().join("Scripts").join("python.exe") } else { self.venv().join("bin").join("python") }
    }

    fn bridge(&self) -> PathBuf {
        self.engine.join("bridge").join("vieneu_bridge.py")
    }

    fn work(&self) -> PathBuf {
        self.engine.join("work")
    }

    fn tmp(&self) -> PathBuf {
        self.engine.join("tmp")
    }

    /// VieNeu's data under the models directory.
    pub fn data(&self) -> PathBuf {
        self.models.join(DIR_NAME)
    }

    /// Where `vieneu` downloads its model.
    pub fn cache(&self) -> PathBuf {
        self.data().join("cache")
    }

    fn voices_file(&self) -> PathBuf {
        self.data().join("voices.json")
    }

    /// Written once a setup has loaded the model: later runs need no network.
    fn ready_marker(&self) -> PathBuf {
        self.data().join("ready")
    }
}

/// Interpreters to try, in order (see the module docs). `host` is one a host program chose
/// (tests use it); nothing a command or a setting says ends up here.
pub fn candidates(dirs: &Dirs, host: Option<&Python>) -> Vec<Python> {
    let mut out: Vec<Python> = host.cloned().into_iter().collect();
    if let Some(p) = std::env::var_os(PYTHON_ENV).filter(|v| !v.is_empty()) {
        out.push(Python::new(p, &[], "env"));
    }
    let v = dirs.venv_python();
    if v.is_file() {
        out.push(Python::new(v, &[], "venv"));
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

/// NFC form of `s` (a name typed or pasted in decomposed form matches the preset).
pub fn nfc(s: &str) -> String {
    s.nfc().collect()
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

/// Start `program` with a scrubbed environment: none of Filmagic's own variables (the control
/// token among them) and none of Python's (`PYTHONPATH`, `PYTHONHOME`, …), then downloads into the
/// models directory, no telemetry, and offline once a setup has succeeded.
fn command(program: &Path, dirs: &Dirs, offline: bool) -> Result<Command, String> {
    let work = dirs.work();
    std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    let mut cmd = Command::new(program);
    for (k, _) in std::env::vars_os() {
        let key = k.to_string_lossy().to_ascii_uppercase();
        if key.starts_with("FILMAGIC_") || key.starts_with("PYTHON") || key.starts_with("PIP_") || key == "VIRTUAL_ENV" {
            cmd.env_remove(&k);
        }
    }
    let cache = dirs.cache();
    let vars: [(&str, OsString); 4] = [
        ("HF_HOME", cache.join("hf").into_os_string()),
        ("HF_HUB_DISABLE_SYMLINKS_WARNING", "1".into()),
        ("HF_HUB_DISABLE_TELEMETRY", "1".into()),
        ("PYTHONDONTWRITEBYTECODE", "1".into()),
    ];
    for (k, v) in vars {
        cmd.env(k, v);
    }
    if offline {
        cmd.env("HF_HUB_OFFLINE", "1");
    }
    cmd.current_dir(&work);
    Ok(cmd)
}

/// Write the bridge to the engine directory (when it differs) and return its path. A script file
/// rather than `-c`: batch-file shims (pyenv-win's `python.bat`) cannot take a multi-line argument.
fn bridge_script(dirs: &Dirs) -> Result<PathBuf, String> {
    let path = dirs.bridge();
    if std::fs::read(&path).ok().as_deref() == Some(HELPER.as_bytes()) {
        return Ok(path);
    }
    let folder = path.parent().map(Path::to_path_buf).unwrap_or_else(|| dirs.engine.clone());
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

fn bridge_command(py: &Python, op: &str, dirs: &Dirs, offline: bool) -> Result<Command, String> {
    let script = bridge_script(dirs)?;
    let mut cmd = command(&py.program, dirs, offline)?;
    // -E: ignore PYTHON* variables; -s: no user site-packages; -X utf8: UTF-8 everywhere
    cmd.args(&py.args).args(["-E", "-s", "-X", "utf8"]).arg(script).arg(op);
    Ok(cmd)
}

/// The bridge's JSON reply: the last line that starts with [`REPLY_PREFIX`].
fn reply(out: &Output) -> Result<serde_json::Value, String> {
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().rev().find_map(|l| l.strip_prefix(REPLY_PREFIX)).unwrap_or_default();
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
    /// What runs from now on: the interpreter's own `sys.executable` when it reported one.
    pub python: Python,
    /// What was found (`py -3`, a venv, …).
    pub found: Python,
    pub python_version: String,
    /// The installed `vieneu` version.
    pub vieneu: Option<String>,
}

/// Ask `py` for its version, its executable and whether `vieneu` is importable (does not import it).
pub fn probe(py: &Python, dirs: &Dirs) -> Result<Probe, String> {
    let out = run_process(bridge_command(py, "probe", dirs, true)?, b"", 64 * 1024, Duration::from_secs(30), None)?;
    let v = reply(&out)?;
    let version = v.get("python").and_then(|x| x.as_str()).unwrap_or_default().to_string();
    if !version.split('.').next().and_then(|m| m.parse::<u32>().ok()).is_some_and(|m| m >= 3) {
        return Err(format!("not a Python 3 interpreter (it says {version:?})"));
    }
    let vieneu = v.get("vieneu").and_then(|x| x.as_str()).map(|s| s.chars().take(64).collect());
    let exe = v.get("executable").and_then(|x| x.as_str()).map(PathBuf::from).filter(|p| p.is_absolute() && p.is_file());
    let python = match exe {
        Some(e) => Python { program: e, args: Vec::new(), source: py.source },
        None => py.clone(),
    };
    Ok(Probe { python, found: py.clone(), python_version: version.chars().take(32).collect(), vieneu })
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
pub fn detect(dirs: &Dirs, host: Option<&Python>) -> Status {
    let mut st = Status::default();
    for py in candidates(dirs, host) {
        match probe(&py, dirs) {
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
pub fn install_steps(st: &Status, dirs: &Dirs) -> Vec<String> {
    let q = |p: &Path| {
        let s = p.to_string_lossy();
        if s.contains(' ') { format!("\"{s}\"") } else { s.into_owned() }
    };
    let mut out = Vec::new();
    let py = match &st.python {
        Some(p) => p.found.display(),
        None => {
            out.push(if cfg!(windows) {
                "Install Python 3.10 or newer from python.org (tick \"Add python.exe to PATH\"), then open a new terminal".to_string()
            } else {
                "Install Python 3.10 or newer (python3 and its venv module)".to_string()
            });
            if cfg!(windows) { "py -3".to_string() } else { "python3".to_string() }
        }
    };
    out.push(format!("{py} -I -m venv {}", q(&dirs.venv())));
    out.push(format!("{} -I -m pip install {PACKAGE_SPEC}", q(&dirs.venv_python())));
    out.push("Then run tts.setupVieneu (it downloads the voice model, about 0.6 GB, into the models directory and lists the voices)".to_string());
    out
}

/// Create the environment in the engine directory with `python` (when it is not there yet) and
/// `pip install vieneu==<pinned>` into it, every time: a cancelled install is finished, and a
/// complete one is a quick no-op. `log` gets progress lines; `cancel` stops it.
pub fn install(python: &Python, dirs: &Dirs, cancel: Option<&AtomicBool>, log: &mut dyn FnMut(&str)) -> Result<Python, String> {
    let vpy = dirs.venv_python();
    std::fs::create_dir_all(&dirs.engine).map_err(|e| format!("{}: {e}", dirs.engine.display()))?;
    if !vpy.is_file() {
        log(&format!("Creating a Python environment in {}", dirs.venv().display()));
        let mut cmd = command(&python.program, dirs, false)?;
        cmd.args(&python.args).args(["-I", "-m", "venv"]).arg(dirs.venv());
        let out = run_process(cmd, b"", MAX_STDOUT, Duration::from_secs(600), cancel)?;
        if !out.success || !vpy.is_file() {
            return Err(format!("{} -m venv failed: {}", python.display(), out.stderr));
        }
    }
    log(&format!("Installing {PACKAGE_SPEC} with pip (this can take several minutes)"));
    let mut cmd = command(&vpy, dirs, false)?;
    cmd.args(["-I", "-m", "pip", "install", "--disable-pip-version-check", PACKAGE_SPEC]);
    let out = run_process(cmd, b"", MAX_STDOUT, Duration::from_secs(3600), cancel)?;
    if !out.success {
        return Err(format!("pip install {PACKAGE_SPEC} failed: {}", out.stderr));
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
    s.map(|s| nfc(&s).chars().filter(|c| !c.is_control()).take(max).collect::<String>()).filter(|s| !s.trim().is_empty())
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
    let f = models_dir.join(DIR_NAME).join("voices.json");
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

/// Load the model with `python` (downloading it the first time), cache the voice list and mark
/// the setup done (later runs are offline).
pub fn prepare(python: &Python, dirs: &Dirs, cancel: Option<&AtomicBool>) -> Result<Vec<VieneuVoice>, TtsError> {
    let req = serde_json::json!({"mode": mode()});
    std::fs::create_dir_all(dirs.data()).map_err(|e| engine_err(format!("{}: {e}", dirs.data().display())))?;
    path_text(&dirs.cache())?;
    let cmd = bridge_command(python, "voices", dirs, false).map_err(engine_err)?;
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
    let f = dirs.voices_file();
    let json = serde_json::to_vec_pretty(&list).map_err(|e| engine_err(e.to_string()))?;
    let tmp = f.with_extension("json.part");
    std::fs::write(&tmp, json).and_then(|()| std::fs::rename(&tmp, &f)).map_err(|e| engine_err(format!("{}: {e}", f.display())))?;
    std::fs::write(dirs.ready_marker(), PACKAGE_SPEC).map_err(|e| engine_err(format!("{}: {e}", dirs.ready_marker().display())))?;
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
    let out: Vec<f32> = bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
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
pub fn synthesize(python: &Python, dirs: &Dirs, req: &Request, cancel: Option<&AtomicBool>) -> Result<Audio, TtsError> {
    check_text(req.text)?;
    req.params.check()?;
    let voice = req.voice.map(|v| nfc(v.trim()));
    if let Some(r) = req.reference {
        check_reference(r)?;
    } else if voice.as_deref().is_none_or(str::is_empty) {
        return Err(TtsError::UnknownVoice(format!("{VOICE_PREFIX} needs a voice name (tts.voices lists them)")));
    }
    let text = nfc(req.text);
    let pieces = script::parse(&text);
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
    let tmp = TempDir(dirs.tmp().join(format!("{}-{n}", std::process::id())));
    let _ = std::fs::remove_dir_all(&tmp.0);
    std::fs::create_dir_all(&tmp.0).map_err(|e| engine_err(format!("{}: {e}", tmp.0.display())))?;
    path_text(&dirs.cache())?;
    let request = serde_json::json!({
        "mode": mode(),
        "voice": voice,
        "reference": req.reference.map(path_text).transpose()?,
        "segments": segments,
        "takes": TAKES,
        "out": path_text(&tmp.0)?,
    });
    let chars = req.text.chars().count() as u64;
    let timeout = Duration::from_secs((900 + chars / 2).min(6 * 3600));
    let offline = dirs.ready_marker().is_file();
    let cmd = bridge_command(python, "synth", dirs, offline).map_err(engine_err)?;
    let out = run_process(cmd, request.to_string().as_bytes(), MAX_STDOUT, timeout, cancel).map_err(engine_err)?;
    let v = reply(&out).map_err(|e| engine_err(if offline { format!("{e} (it ran offline: tts.setupVieneu downloads what is missing)") } else { e }))?;
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
        // a slower pace makes it longer
        if audio.len() as u64 > max {
            return Err(TtsError::AudioTooLong);
        }
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

    fn dirs(root: &Path) -> Dirs {
        Dirs { engine: root.join("engine"), models: root.join("models") }
    }

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
    fn voice_cache_is_sanitized_and_normalised() {
        let list = vec![
            VieneuVoice { name: "Hải Đăng".into(), gender: Some("male".into()), description: None, region: None },
            // the same name, decomposed (NFD)
            VieneuVoice { name: "Ha\u{309}i \u{110}a\u{306}ng".into(), gender: None, description: None, region: None },
            VieneuVoice { name: "  ".into(), gender: None, description: None, region: None },
            VieneuVoice { name: "a\u{0}b\nc".into(), gender: Some("x".repeat(100)), description: Some("d".repeat(1000)), region: None },
        ];
        let out = sanitize_voices(list);
        assert_eq!(out.len(), 2, "{out:?}");
        assert_eq!(out[1].name, "abc");
        assert!(out[1].description.as_ref().unwrap().len() <= 300);
        assert_eq!(out[0].info_json()["gender"], "male");
        assert_eq!(out[1].info_json()["gender"], serde_json::Value::Null);
        assert_eq!(out[0].id(), "vieneu:Hải Đăng");
        assert_eq!(nfc("Ha\u{309}i"), "Hải");
    }

    #[test]
    fn only_a_host_or_a_person_names_the_interpreter() {
        let root = tempfile::tempdir().unwrap();
        let d = dirs(root.path());
        let c = candidates(&d, None);
        assert!(c.iter().all(|p| matches!(p.source, "env" | "venv" | "path")), "{c:?}");
        assert_eq!(c.last().map(|p| p.source), Some("path"));
        let host = Python::new("/opt/py/bin/python3", &[], "host");
        assert_eq!(candidates(&d, Some(&host)).first(), Some(&host));
        // the environment lives in the engine directory, never in the models directory
        assert!(d.venv().starts_with(&d.engine) && d.bridge().starts_with(&d.engine));
        assert!(d.cache().starts_with(&d.models) && !d.cache().starts_with(&d.engine));
        assert_eq!(Python::new("C:/Program Files/Python/python.exe", &[], "host").display(), "\"C:/Program Files/Python/python.exe\"");
    }

    #[test]
    fn a_missing_interpreter_is_reported_not_fatal() {
        let root = tempfile::tempdir().unwrap();
        let d = dirs(root.path());
        let py = Python::new(root.path().join("no-such-python"), &[], "host");
        assert!(probe(&py, &d).is_err());
        let req = Request { text: "Xin chào.", voice: Some("Hải Đăng"), reference: None, params: Params::default() };
        assert!(matches!(synthesize(&py, &d, &req, None), Err(TtsError::Engine(_))));
    }

    /// A stand-in for a Python with `vieneu`: a shell script that speaks the bridge's protocol
    /// (`sh fake.sh -E -s -X utf8 <bridge.py> <op>`, request on stdin, a prefixed reply line).
    /// `synth` writes `samples` zero samples per segment at 48 kHz; `body` replaces the synth reply
    /// for hostile cases (`R '<json>'` prints a reply). It keeps the last request, its arguments
    /// and its environment in its working directory.
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
echo '{{"ok":true,"note":"a library printing JSON is not the reply"}}'
R "{{\"ok\":true,\"sampleRate\":48000,\"segments\":[$segs]}}""#,
                bytes = samples * 4
            )
        });
        let text = format!(
            r#"#!/bin/sh
R() {{ printf 'FILMAGIC-REPLY %s\n' "$1"; }}
for a in "$@"; do op="$a"; done
echo "$@" > args.txt
env > env.txt
if [ "$op" = probe ]; then R '{{"ok":true,"python":"3.12.1","vieneu":"3.8.4"}}'; exit 0; fi
req=$(cat)
printf '%s' "$req" > last-request.json
if [ "$op" = voices ]; then R '{{"ok":true,"sampleRate":48000,"voices":[{{"name":"Hải Đăng","label":"x","gender":"male","description":"Nam"}},{{"name":"Mai Anh","gender":"female"}}]}}'; exit 0; fi
{synth}
"#
        );
        std::fs::write(&script, text).unwrap();
        Python { program: "sh".into(), args: vec![script.to_string_lossy().into_owned()], source: "host" }
    }

    #[cfg(unix)]
    fn last(root: &Path, file: &str) -> String {
        std::fs::read_to_string(dirs(root).work().join(file)).unwrap()
    }

    #[cfg(unix)]
    fn last_request(root: &Path) -> serde_json::Value {
        serde_json::from_str(&last(root, "last-request.json")).unwrap()
    }

    #[cfg(unix)]
    fn say(py: &Python, root: &Path, text: &str, voice: Option<&str>, pace: f64) -> Result<Audio, TtsError> {
        synthesize(py, &dirs(root), &Request { text, voice, reference: None, params: Params { pace, semitones: 0.0 } }, None)
    }

    #[cfg(unix)]
    #[test]
    fn speaks_through_the_bridge_with_exact_pauses() {
        let root = tempfile::tempdir().unwrap();
        let py = fake(root.path(), 24_000, None);
        let p = probe(&py, &dirs(root.path())).unwrap();
        assert_eq!((p.python_version.as_str(), p.vieneu.as_deref()), ("3.12.1", Some("3.8.4")));
        let a = say(&py, root.path(), "Xin chào các bạn. [pause 500ms] Hôm nay trời đẹp.", Some("Hải Đăng"), 1.0).unwrap();
        assert_eq!(a.sample_rate, 48_000);
        assert_eq!(a.samples.len(), 24_000 + 24_000 + 24_000, "two segments and exactly 0.5 s of pause");
        let req = last_request(root.path());
        assert_eq!(req["voice"], "Hải Đăng");
        assert_eq!(req["segments"][0]["text"], "Xin chào các bạn.");
        assert_eq!(req["segments"][1]["text"], "Hôm nay trời đẹp.");
        assert!(req["segments"][0]["minSeconds"].as_f64().unwrap() < req["segments"][0]["maxSeconds"].as_f64().unwrap());
        // pace is applied afterwards: twice as fast, half as long
        let fast = say(&py, root.path(), "Xin chào các bạn.", Some("Hải Đăng"), 2.0).unwrap();
        assert_eq!(fast.samples.len(), 12_000);
        assert!(!dirs(root.path()).tmp().read_dir().unwrap().any(|_| true), "temporary files are removed");
    }

    #[cfg(unix)]
    #[test]
    fn the_engine_runs_isolated_and_offline_once_set_up() {
        let root = tempfile::tempdir().unwrap();
        let d = dirs(root.path());
        let py = fake(root.path(), 480, None);
        say(&py, root.path(), "Xin chào.", Some("Hải Đăng"), 1.0).unwrap();
        assert!(last(root.path(), "args.txt").starts_with("-E -s -X utf8 "), "{}", last(root.path(), "args.txt"));
        let env = last(root.path(), "env.txt");
        for line in env.lines() {
            assert!(!line.starts_with("FILMAGIC_"), "Filmagic's variables (the control token) stay out: {line}");
            assert!(!line.starts_with("PYTHON") || line.starts_with("PYTHONDONTWRITEBYTECODE="), "{line}");
        }
        assert!(env.contains(&format!("HF_HOME={}", d.cache().join("hf").display())), "downloads go to the models directory");
        assert!(!env.contains("HF_HUB_OFFLINE"), "online until a setup has succeeded");
        prepare(&py, &d, None).unwrap();
        say(&py, root.path(), "Xin chào.", Some("Hải Đăng"), 1.0).unwrap();
        assert!(last(root.path(), "env.txt").contains("HF_HUB_OFFLINE=1"));
        assert!(last(root.path(), "args.txt").ends_with(" synth\n"));
        // the working directory is empty but for what the stand-in wrote itself
        let names: Vec<String> = std::fs::read_dir(d.work()).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert!(names.iter().all(|n| ["args.txt", "env.txt", "last-request.json"].contains(&n.as_str())), "{names:?}");
    }

    #[cfg(unix)]
    #[test]
    fn the_interpreter_s_own_executable_runs_afterwards() {
        let root = tempfile::tempdir().unwrap();
        let exe = root.path().join("python3.12");
        std::fs::write(&exe, b"").unwrap();
        let body = r#"R '{"ok":true}'"#.to_string();
        let py = fake(root.path(), 0, Some(&body));
        // the stand-in reports an executable: a real path is taken, anything else is ignored
        let script =
            std::fs::read_to_string(&py.args[0]).unwrap().replace(r#""vieneu":"3.8.4"}'"#, &format!(r#""vieneu":"3.8.4","executable":"{}"}}'"#, exe.display()));
        std::fs::write(&py.args[0], script).unwrap();
        let p = probe(&py, &dirs(root.path())).unwrap();
        assert_eq!(p.python.program, exe);
        assert!(p.python.args.is_empty());
        assert_eq!(p.found, py);
        std::fs::remove_file(&exe).unwrap();
        assert_eq!(probe(&py, &dirs(root.path())).unwrap().python, py, "a path that is not there is not used");
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
        // a voice name is data too, sent in NFC
        say(&py, root.path(), "Một hai ba.", Some("\"; rm -rf / #"), 1.0).unwrap();
        assert_eq!(last_request(root.path())["voice"], "\"; rm -rf / #");
        say(&py, root.path(), "Ha\u{309}i.", Some("Ha\u{309}i \u{110}a\u{306}ng"), 1.0).unwrap();
        assert_eq!(last_request(root.path())["voice"], "Hải Đăng");
        assert_eq!(last_request(root.path())["segments"][0]["text"], "Hải.");
        // a long script: many pause markers, the 64 KB limit, never a spawn past it
        let long = "Một hai ba bốn năm. [pause 10ms] ".repeat(1_500);
        assert!(long.len() < crate::MAX_TEXT_BYTES, "{}", long.len());
        let a = say(&py, root.path(), &long, Some("Hải Đăng"), 1.0).unwrap();
        assert_eq!(last_request(root.path())["segments"].as_array().unwrap().len(), 1_500);
        assert!(a.samples.len() > 1_500 * 4_800);
        let _ = std::fs::remove_file(dirs(root.path()).work().join("last-request.json"));
        assert_eq!(say(&py, root.path(), &"a ".repeat(40_000), Some("Hải Đăng"), 1.0).err(), Some(TtsError::TextTooLong));
        assert_eq!(say(&py, root.path(), " [pause 1s] … ", Some("Hải Đăng"), 1.0).err(), Some(TtsError::Empty));
        assert!(!dirs(root.path()).work().join("last-request.json").exists(), "refused before Python ran");
        assert!(matches!(say(&py, root.path(), "Chào.", Some("  "), 1.0), Err(TtsError::UnknownVoice(_))));
        assert!(matches!(say(&py, root.path(), "Chào.", None, 1.0), Err(TtsError::UnknownVoice(_))));
        for pace in [f64::NAN, 0.0, -1.0, 9.0] {
            assert!(matches!(say(&py, root.path(), "Chào.", Some("Hải Đăng"), pace), Err(TtsError::Invalid(_))), "{pace}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_slower_pace_cannot_pass_the_length_limit() {
        let root = tempfile::tempdir().unwrap();
        // 20 minutes at 48 kHz: under the 30-minute limit, over it at half speed
        let py = fake(root.path(), 48_000 * 60 * 20, None);
        assert_eq!(say(&py, root.path(), "Xin chào.", Some("Hải Đăng"), 0.5).err(), Some(TtsError::AudioTooLong));
    }

    #[cfg(unix)]
    #[test]
    fn bridge_failures_become_errors() {
        let cases: &[(&str, fn(&TtsError) -> bool)] = &[
            (
                r#"R '{"ok":false,"kind":"unknownVoice","error":"unknown VieNeu voice","voices":["Hải Đăng","Mai Anh"]}'; exit 1"#,
                |e| matches!(e, TtsError::UnknownVoice(m) if m.contains("vieneu:Mai Anh")),
            ),
            (r#"R '{"ok":false,"kind":"missing","error":"no module named vieneu"}'; exit 1"#, |e| matches!(e, TtsError::EngineMissing(_))),
            ("echo 'Traceback: boom' >&2; exit 3", |e| matches!(e, TtsError::Engine(m) if m.contains("boom") && m.contains('3'))),
            ("R 'not json at all'", |e| matches!(e, TtsError::Engine(_))),
            // JSON a library printed, without the prefix, is never the reply
            (r#"echo '{"ok":true,"sampleRate":48000,"segments":[{"samples":0}]}'"#, |e| matches!(e, TtsError::Engine(m) if m.contains("no reply"))),
            (r#"R '{"ok":true,"sampleRate":48000,"segments":[]}'"#, |e| matches!(e, TtsError::Engine(m) if m.contains("segments"))),
            (r#"R '{"ok":true,"sampleRate":0,"segments":[{"samples":1}]}'"#, |e| matches!(e, TtsError::Engine(m) if m.contains("sample rate"))),
            (r#"R '{"ok":true,"sampleRate":"48000","segments":[{"samples":1}]}'"#, |e| matches!(e, TtsError::Engine(_))),
            // the file is shorter than reported
            (
                r#"out=$(printf '%s' "$req" | sed -n 's/.*"out":"\([^"]*\)".*/\1/p'); head -c 10 /dev/zero > "$out/seg-0.f32"; R '{"ok":true,"sampleRate":48000,"segments":[{"samples":100}]}'"#,
                |e| matches!(e, TtsError::Engine(m) if m.contains("bytes")),
            ),
            // NaN samples
            (
                r#"out=$(printf '%s' "$req" | sed -n 's/.*"out":"\([^"]*\)".*/\1/p'); printf '\000\000\300\177' > "$out/seg-0.f32"; R '{"ok":true,"sampleRate":48000,"segments":[{"samples":1}]}'"#,
                |e| matches!(e, TtsError::Model(_)),
            ),
            // an absurd length is refused before anything is read
            (r#"R '{"ok":true,"sampleRate":48000,"segments":[{"samples":18446744073709551615}]}'"#, |e| *e == TtsError::AudioTooLong),
            // a missing file
            (r#"R '{"ok":true,"sampleRate":48000,"segments":[{"samples":5}]}'"#, |e| matches!(e, TtsError::Engine(_))),
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
        let d = dirs(root.path());
        let py = fake(root.path(), 0, None);
        assert!(cached_voices(&d.models).is_none());
        let st = detect(&d, Some(&py));
        assert_eq!(st.ready.as_ref().map(|p| &p.found), Some(&py));
        let v = prepare(&py, &d, None).unwrap();
        assert_eq!(v.iter().map(|v| v.name.as_str()).collect::<Vec<_>>(), ["Hải Đăng", "Mai Anh"]);
        assert_eq!(cached_voices(&d.models), Some(v));
        // a corrupt cache is ignored, not trusted
        std::fs::write(d.voices_file(), b"{not json").unwrap();
        assert!(cached_voices(&d.models).is_none());
        std::fs::write(d.voices_file(), b"[]").unwrap();
        assert!(cached_voices(&d.models).is_none());
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
        let e = synthesize(&py, &dirs(root.path()), &req, Some(&cancel)).unwrap_err();
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
        let odd = Dirs { engine: root.path().join("engine"), models: root.path().join(std::ffi::OsStr::from_bytes(b"models-\xff")) };
        let req = Request { text: "Xin chào.", voice: Some("Hải Đăng"), reference: None, params: Params::default() };
        let r = std::panic::catch_unwind(|| synthesize(&py, &odd, &req, None));
        assert!(matches!(r, Ok(Err(TtsError::Invalid(_)))), "{r:?}");
        let r = std::panic::catch_unwind(|| prepare(&py, &odd, None));
        assert!(matches!(r, Ok(Err(TtsError::Invalid(_)))), "{r:?}");
    }

    #[cfg(unix)]
    #[test]
    fn the_bridge_file_is_written_once_and_kept_current() {
        let root = tempfile::tempdir().unwrap();
        let d = dirs(root.path());
        let p = bridge_script(&d).unwrap();
        assert!(p.starts_with(&d.engine));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), HELPER);
        std::fs::write(&p, "tampered").unwrap();
        assert_eq!(std::fs::read_to_string(bridge_script(&d).unwrap()).unwrap(), HELPER, "rewritten when it differs");
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
        synthesize(&py, &dirs(root.path()), &req, None).unwrap();
        let r = last_request(root.path());
        assert_eq!(r["reference"], clip.to_string_lossy().as_ref());
        assert_eq!(r["voice"], serde_json::Value::Null);
    }

    #[test]
    fn install_steps_pin_the_package_and_name_the_venv() {
        let root = tempfile::tempdir().unwrap();
        let d = dirs(root.path());
        let steps = install_steps(&Status::default(), &d);
        assert!(steps.iter().any(|s| s.contains("-I -m venv") && s.contains(&*d.venv().to_string_lossy())), "{steps:?}");
        assert!(steps.iter().any(|s| s.contains("-I -m pip install vieneu==3.8.4")), "{steps:?}");
        assert!(steps.first().is_some_and(|s| s.contains("Python 3.10")), "no Python found: install it first");
    }

    #[test]
    fn references_are_checked() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        assert!(check_reference(&dir.join("x.txt")).is_err());
        assert!(check_reference(&dir.join("missing.wav")).is_err());
        let empty = dir.join("empty.wav");
        std::fs::write(&empty, b"").unwrap();
        assert!(check_reference(&empty).is_err());
        std::fs::write(&empty, b"RIFF....").unwrap();
        assert!(check_reference(&empty).is_ok());
        let folder = dir.join("folder.wav");
        std::fs::create_dir_all(&folder).unwrap();
        assert!(check_reference(&folder).is_err(), "a folder named like a clip");
    }
}
