//! Where downloaded models live: the Whisper speech models, the natural (Kokoro) voices and
//! VieNeu-TTS's model cache and voice list. (VieNeu's Python environment and bridge, which are
//! executed, live in its engine directory instead: [`crate::narration::vieneu_engine_dir`].)
//!
//! One directory holds them all. It is, in order:
//! 1. `FILMAGIC_MODELS_DIR` (an environment variable; it wins so a launcher can point a session
//!    somewhere else without touching the settings),
//! 2. Settings ▸ Media Analysis & Transcription ▸ Models location (`mediaAnalysis.modelsLocation`),
//! 3. `<data dir>/models` (the default).
//!
//! | command | does |
//! |---|---|
//! | `models.info` | the directory, why it is the one used, what it holds and how big it is, and models left in another location |
//! | `models.setLocation` | choose another directory and move the models the current one holds there (or leave them, with a warning) |
//!
//! Only folders Filmagic itself puts there are ever moved or deleted: a Whisper model (`whisper-*`),
//! the natural voices (`kokoro-82m`) and `vieneu`. A models folder that holds anything else is not
//! moved. `models.setLocation` needs a session that saves its settings (the desktop app);
//! `filmagic-cli` and headless MCP sessions set `FILMAGIC_MODELS_DIR` instead.
//!
//! Changing `mediaAnalysis.modelsLocation` with `prefs.set` or the Settings dialog does not move
//! anything; `models.info`, `transcript.models` and `tts.voices` then warn about models left at
//! the default location, so nothing installed disappears without a word.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad};
use crate::{EngineError, Result, Session};

/// Environment variable that overrides the models location.
pub const ENV: &str = "FILMAGIC_MODELS_DIR";

/// Deepest directory level walked when sizing or copying a models directory.
const MAX_DEPTH: usize = 48;
/// Most entries walked when sizing or copying (a models directory holds a few thousand files).
const MAX_ENTRIES: usize = 200_000;

/// Why a models directory is the one in use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Env,
    Setting,
    Default,
}

impl Source {
    pub fn id(self) -> &'static str {
        match self {
            Source::Env => "env",
            Source::Setting => "setting",
            Source::Default => "default",
        }
    }
}

/// `<data dir>/models`.
pub fn default_dir() -> Option<PathBuf> {
    crate::autosave::default_data_dir().map(|d| d.join("models"))
}

fn absolute(p: &str) -> Option<PathBuf> {
    let p = p.trim();
    if p.is_empty() {
        return None;
    }
    std::path::absolute(p).ok()
}

/// The models directory for the `mediaAnalysis.modelsLocation` setting `setting`.
pub fn location(setting: &str) -> Option<(PathBuf, Source)> {
    resolve(std::env::var(ENV).ok().as_deref(), setting, default_dir())
}

/// [`location`] from its three inputs: the environment variable, the setting, the default.
fn resolve(env: Option<&str>, setting: &str, default: Option<PathBuf>) -> Option<(PathBuf, Source)> {
    if let Some(d) = env.and_then(absolute) {
        return Some((d, Source::Env));
    }
    if let Some(d) = absolute(setting) {
        return Some((d, Source::Setting));
    }
    default.map(|d| (d, Source::Default))
}

fn env_set() -> bool {
    std::env::var(ENV).ok().is_some_and(|v| !v.trim().is_empty())
}

/// Check a `mediaAnalysis.modelsLocation` value: empty (the default) or an absolute folder.
pub fn validate_setting(v: &Value) -> std::result::Result<(), String> {
    let s = v.as_str().ok_or("`mediaAnalysis.modelsLocation` must be a string (a folder path, or \"\" for the default)")?;
    if !s.trim().is_empty() && !Path::new(s.trim()).is_absolute() {
        return Err(format!(
            "`mediaAnalysis.modelsLocation` must be an absolute folder path (got `{}`), or \"\" for the default",
            s.chars().take(200).collect::<String>()
        ));
    }
    Ok(())
}

impl Session {
    /// Where downloaded models live ([module docs](crate::models_store)).
    pub fn models_dir(&self) -> Option<PathBuf> {
        location(&self.prefs.media_analysis.models_location).map(|(d, _)| d)
    }
}

/// What a models directory entry is, when Filmagic put it there (only those are ever moved).
fn known(name: &str) -> Option<&'static str> {
    if filmagic_speech::models::find(name).is_some() {
        Some("speech model (Whisper)")
    } else if name == filmagic_tts::catalog::PACKAGE_ID {
        Some("natural voices (Kokoro-82M)")
    } else if name == filmagic_tts::vieneu::DIR_NAME {
        Some("VieNeu-TTS (model cache and voice list)")
    } else {
        None
    }
}

/// Bytes under `p` (files only; links are not followed), bounded in depth and entry count.
fn size_of(p: &Path) -> u64 {
    fn walk(p: &Path, depth: usize, seen: &mut usize) -> u64 {
        let Ok(meta) = std::fs::symlink_metadata(p) else { return 0 };
        if meta.is_file() {
            return meta.len();
        }
        if !meta.is_dir() || depth >= MAX_DEPTH {
            return 0;
        }
        let Ok(rd) = std::fs::read_dir(p) else { return 0 };
        let mut total = 0u64;
        for e in rd.flatten() {
            *seen += 1;
            if *seen > MAX_ENTRIES {
                break;
            }
            total = total.saturating_add(walk(&e.path(), depth + 1, seen));
        }
        total
    }
    walk(p, 0, &mut 0)
}

/// The entries of a directory (sorted by name), skipping hidden ones.
fn entries(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| rd.flatten().filter_map(|e| e.file_name().to_str().map(str::to_string)).filter(|n| !n.starts_with('.')).collect())
        .unwrap_or_default();
    out.sort();
    out
}

/// The entries Filmagic put there.
fn model_entries(dir: &Path) -> Vec<String> {
    entries(dir).into_iter().filter(|n| known(n).is_some()).collect()
}

fn entries_json(dir: &Path) -> (Vec<Value>, u64) {
    let mut total = 0u64;
    let list = entries(dir)
        .into_iter()
        .map(|name| {
            let bytes = size_of(&dir.join(&name));
            total = total.saturating_add(bytes);
            json!({"name": name, "what": known(&name).unwrap_or("not Filmagic's (never moved)"), "bytes": bytes})
        })
        .collect();
    (list, total)
}

/// Warnings about models that sit in the default location (or `previous`) while another directory
/// is in use: they are not used until they are moved.
pub fn elsewhere_warnings(s: &Session) -> Vec<String> {
    let Some(current) = s.models_dir() else { return Vec::new() };
    let mut out = Vec::new();
    let mut others: Vec<PathBuf> = default_dir().into_iter().collect();
    if let Some(p) = &s.previous_models_dir
        && !others.contains(p)
    {
        others.push(p.clone());
    }
    for d in others.into_iter().filter(|d| *d != current) {
        let names = model_entries(&d);
        if !names.is_empty() {
            let back = if Some(&d) == default_dir().as_ref() { String::new() } else { d.to_string_lossy().into_owned() };
            out.push(format!(
                "{} holds models that are not used, because the models location is now {} ({}). To use them, move them: prefs.set {{\"key\": \"mediaAnalysis.modelsLocation\", \"value\": {:?}}}, then models.setLocation {{\"dir\": {:?}}}. Or delete them to free the space.",
                d.display(),
                current.display(),
                names.join(", "),
                back,
                current.to_string_lossy(),
            ));
        }
    }
    out
}

fn info(s: &mut Session, _: &Value) -> Result<Value> {
    let (dir, source) = location(&s.prefs.media_analysis.models_location).ok_or_else(|| EngineError::Other("there is no data directory for models".into()))?;
    let (list, total) = entries_json(&dir);
    Ok(json!({
        "dir": dir.to_string_lossy(),
        "source": source.id(),
        "env": ENV,
        "envValue": std::env::var(ENV).ok().filter(|v| !v.trim().is_empty()),
        "setting": s.prefs.media_analysis.models_location,
        "default": default_dir().map(|d| d.to_string_lossy().into_owned()),
        "exists": dir.is_dir(),
        "entries": list,
        "bytes": total,
        "savesSettings": s.prefs_path.is_some(),
        "warnings": elsewhere_warnings(s),
    }))
}

/// How one entry got to its new place (so a failure can put everything back).
enum Moved {
    Renamed(PathBuf, PathBuf),
    Copied(PathBuf, PathBuf),
}

/// Copy `from` to `to` (a file, a directory tree or a symbolic link), bounded.
fn copy_tree(from: &Path, to: &Path, depth: usize, seen: &mut usize) -> std::io::Result<()> {
    *seen += 1;
    if depth >= MAX_DEPTH || *seen > MAX_ENTRIES {
        return Err(std::io::Error::other(format!("{} is nested too deeply or holds too many files to copy", from.display())));
    }
    let meta = std::fs::symlink_metadata(from)?;
    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(from)?;
        #[cfg(unix)]
        {
            return std::os::unix::fs::symlink(target, to);
        }
        #[cfg(windows)]
        {
            // a link to a folder needs a folder link; one to a file (or to nothing) a file link
            let to_dir = from.parent().map(|p| p.join(&target)).is_some_and(|t| t.is_dir());
            return if to_dir { std::os::windows::fs::symlink_dir(target, to) } else { std::os::windows::fs::symlink_file(target, to) };
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = target;
            return Err(std::io::Error::other(format!("{} is a symbolic link; move it by hand", from.display())));
        }
    }
    if meta.is_dir() {
        std::fs::create_dir_all(to)?;
        for e in std::fs::read_dir(from)? {
            let e = e?;
            copy_tree(&e.path(), &to.join(e.file_name()), depth + 1, seen)?;
        }
        return Ok(());
    }
    let n = std::fs::copy(from, to)?;
    if n != meta.len() {
        return Err(std::io::Error::other(format!("{}: copied {n} of {} bytes", from.display(), meta.len())));
    }
    Ok(())
}

fn remove_any(p: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(p),
        Ok(_) => std::fs::remove_file(p),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// What [`move_contents`] did.
#[derive(Debug, Default)]
pub struct MoveReport {
    pub moved: Vec<String>,
    /// Originals left behind after a complete copy (they could not be deleted).
    pub warnings: Vec<String>,
}

/// Move the models Filmagic put in `from` ([`known`] entries only) into `to`. Nothing is
/// overwritten: an entry that exists in both stops the move before anything changes. When a step
/// fails, what was moved goes back, and anything that could not be put back is named. Once every
/// copy is complete the move counts as done: originals that cannot be deleted become warnings.
pub fn move_contents(from: &Path, to: &Path) -> std::result::Result<MoveReport, String> {
    let names = model_entries(from);
    let clashes: Vec<&String> = names.iter().filter(|n| std::fs::symlink_metadata(to.join(n)).is_ok()).collect();
    if !clashes.is_empty() {
        return Err(format!(
            "{} already has {}: move or delete those first (nothing was moved)",
            to.display(),
            clashes.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
        ));
    }
    if names.is_empty() {
        return Ok(MoveReport::default());
    }
    std::fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    let mut done: Vec<Moved> = Vec::new();
    let mut failure = None;
    for n in &names {
        let (src, dst) = (from.join(n), to.join(n));
        if std::fs::rename(&src, &dst).is_ok() {
            done.push(Moved::Renamed(src, dst));
            continue;
        }
        // another volume: copy, and delete the originals only once everything is copied
        match copy_tree(&src, &dst, 0, &mut 0) {
            Ok(()) => done.push(Moved::Copied(src, dst)),
            Err(e) => {
                let _ = remove_any(&dst);
                failure = Some(format!("{}: {e}", src.display()));
                break;
            }
        }
    }
    if let Some(e) = failure {
        let mut stuck = Vec::new();
        for m in done.into_iter().rev() {
            match m {
                Moved::Renamed(src, dst) => {
                    if let Err(err) = std::fs::rename(&dst, &src) {
                        stuck.push(format!("{} (could not be put back: {err})", dst.display()));
                    }
                }
                Moved::Copied(_, dst) => {
                    if let Err(err) = remove_any(&dst) {
                        stuck.push(format!("the partial copy {} (could not be deleted: {err})", dst.display()));
                    }
                }
            }
        }
        return Err(if stuck.is_empty() {
            format!("{e} (nothing was moved; the models stay in {})", from.display())
        } else {
            format!("{e}. The move was undone except for: {}. The models location is unchanged ({})", stuck.join("; "), from.display())
        });
    }
    let mut report = MoveReport { moved: names, warnings: Vec::new() };
    for m in &done {
        if let Moved::Copied(src, _) = m
            && let Err(e) = remove_any(src)
        {
            report.warnings.push(format!("the models were copied, but the original {} could not be deleted ({e}): delete it to free the space", src.display()));
        }
    }
    Ok(report)
}

fn set_location(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "models.setLocation";
    if env_set() {
        return Err(bad(cmd, format!("the models location is set by the environment variable {ENV}; change or unset it instead")));
    }
    if s.prefs_path.is_none() {
        return Err(bad(
            cmd,
            format!(
                "this session does not save settings (filmagic-cli and headless MCP don't), so the new location would be lost: start it with {ENV} set to the folder instead, or change the location in the desktop app"
            ),
        ));
    }
    let raw = match p.get("dir") {
        Some(Value::String(d)) => d.trim().to_string(),
        Some(_) => return Err(bad(cmd, "`dir` must be a string: an absolute folder path, or \"\" for the default")),
        None => return Err(bad(cmd, "`dir` is required: an absolute folder path, or \"\" for the default location")),
    };
    validate_setting(&json!(raw)).map_err(|e| bad(cmd, e))?;
    let move_models = match p.get("move") {
        None => true,
        Some(v) => v.as_bool().ok_or_else(|| bad(cmd, "`move` must be true or false"))?,
    };
    let current = s.models_dir().ok_or_else(|| EngineError::Other("there is no data directory for models".into()))?;
    let target = location(&raw).map(|(d, _)| d).ok_or_else(|| EngineError::Other("there is no data directory for models".into()))?;
    if target == current {
        return Ok(json!({"dir": target.to_string_lossy(), "previous": current.to_string_lossy(), "moved": [], "warnings": []}));
    }
    if target.starts_with(&current) || current.starts_with(&target) {
        return Err(bad(
            cmd,
            format!("{} and {} are inside one another; choose a folder outside the current models directory", target.display(), current.display()),
        ));
    }
    if target.exists() && !target.is_dir() {
        return Err(bad(cmd, format!("{} is a file, not a folder", target.display())));
    }
    if s.jobs.iter().any(|j| !j.progress.finished.load(std::sync::atomic::Ordering::Relaxed)) {
        return Err(bad(cmd, "a background job is running (a download or a synthesis may be using the models): wait for it (jobs.list) and try again"));
    }
    let others: Vec<String> = entries(&current).into_iter().filter(|n| known(n).is_none()).collect();
    if move_models && !others.is_empty() {
        return Err(bad(
            cmd,
            format!(
                "{} holds things Filmagic did not put there ({}), so it is not a models folder Filmagic can move: move them out first, or pass \"move\": false to switch without moving (nothing was moved)",
                current.display(),
                others.iter().take(10).cloned().collect::<Vec<_>>().join(", ")
            ),
        ));
    }
    let mut warnings = Vec::new();
    let moved = if move_models {
        let r = move_contents(&current, &target).map_err(|e| bad(cmd, e))?;
        warnings.extend(r.warnings);
        r.moved
    } else {
        std::fs::create_dir_all(&target).map_err(|e| bad(cmd, format!("{}: {e}", target.display())))?;
        let left = model_entries(&current);
        if !left.is_empty() {
            warnings.push(format!("{} still holds {}; they are not used from the new location", current.display(), left.join(", ")));
        }
        Vec::new()
    };
    let bytes = size_of(&target);
    let mut next = s.prefs.clone();
    next.media_analysis.models_location = raw;
    s.set_prefs(next).map_err(|e| EngineError::Other(format!("the models are in {} but saving the setting failed: {e}", target.display())))?;
    s.previous_models_dir = Some(current.clone());
    Ok(json!({"dir": target.to_string_lossy(), "previous": current.to_string_lossy(), "moved": moved, "bytes": bytes, "warnings": warnings}))
}

fn spec(id: &'static str, label: &'static str, params: &'static str, run: fn(&mut Session, &Value) -> Result<Value>, journal: bool) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: always, run, journal, named: crate::commands::NamedKeys::NONE }
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec("models.info", "Models Location", "{}", info, false),
        spec("models.setLocation", "Move Models", r#"{"dir":"absolute folder"|"" (default),"move":bool? (default true)}"#, set_location, false),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill(dir: &Path) {
        let w = dir.join("whisper-tiny");
        std::fs::create_dir_all(&w).unwrap();
        std::fs::write(w.join("model.safetensors"), vec![7u8; 1000]).unwrap();
        let v = dir.join("vieneu").join("cache").join("deep").join("er");
        std::fs::create_dir_all(&v).unwrap();
        std::fs::write(v.join("weights.onnx"), vec![1u8; 500]).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("../cache", dir.join("vieneu").join("link")).unwrap();
    }

    /// A session that saves its settings (as the desktop app does) in `root`.
    fn saving_session(root: &Path) -> Session {
        Session { prefs_path: Some(root.join("preferences.json")), ..Default::default() }
    }

    #[test]
    fn the_environment_wins_then_the_setting_then_the_default() {
        let def = Some(PathBuf::from("/data/models"));
        let abs = std::path::absolute("/elsewhere/models").unwrap();
        assert_eq!(resolve(Some("/env/models"), "/elsewhere/models", def.clone()).map(|x| x.1), Some(Source::Env));
        assert_eq!(resolve(Some("  "), "/elsewhere/models", def.clone()), Some((abs, Source::Setting)));
        assert_eq!(resolve(None, "", def.clone()), Some((PathBuf::from("/data/models"), Source::Default)));
        assert_eq!(resolve(None, "   ", None), None);
        // a relative FILMAGIC_MODELS_DIR is taken from the working directory
        assert!(resolve(Some("rel/models"), "", def).is_some_and(|(d, s)| d.is_absolute() && s == Source::Env));
    }

    #[test]
    fn the_setting_must_be_an_absolute_folder() {
        assert!(validate_setting(&json!("")).is_ok());
        assert!(validate_setting(&json!(std::env::temp_dir().to_string_lossy())).is_ok());
        for bad in [json!("models"), json!("./m"), json!(3), json!(null), json!(["/x"])] {
            assert!(validate_setting(&bad).is_err(), "{bad}");
        }
        let mut s = Session::default();
        assert!(s.execute("prefs.set", json!({"key": "mediaAnalysis.modelsLocation", "value": "relative/dir"})).is_err());
        assert!(s.execute("prefs.set", json!({"key": "mediaAnalysis.modelsLocation", "value": 42})).is_err());
        // there is no setting that names a program to run
        assert!(s.execute("prefs.set", json!({"key": "mediaAnalysis.vieneuPython", "value": "/bin/sh"})).is_err());
        let root = tempfile::tempdir().unwrap();
        s.execute("prefs.set", json!({"key": "mediaAnalysis.modelsLocation", "value": root.path().to_string_lossy()})).unwrap();
        if !env_set() {
            assert_eq!(s.models_dir().as_deref(), Some(root.path()));
            let r = s.execute("models.info", json!({})).unwrap();
            assert_eq!(r["source"], "setting");
        }
    }

    #[test]
    fn moving_keeps_everything_and_never_overwrites() {
        let root = tempfile::tempdir().unwrap();
        let (a, b) = (root.path().join("a"), root.path().join("b"));
        fill(&a);
        let r = move_contents(&a, &b).unwrap();
        assert_eq!(r.moved, ["vieneu", "whisper-tiny"]);
        assert!(r.warnings.is_empty());
        assert_eq!(std::fs::read(b.join("whisper-tiny/model.safetensors")).unwrap().len(), 1000);
        assert_eq!(size_of(&b), 1500);
        assert!(entries(&a).is_empty());
        #[cfg(unix)]
        assert!(std::fs::symlink_metadata(b.join("vieneu/link")).unwrap().file_type().is_symlink());
        // a clash stops the move before anything changes
        fill(&a);
        let e = move_contents(&a, &b).unwrap_err();
        assert!(e.contains("already has") && e.contains("nothing was moved"), "{e}");
        assert_eq!(entries(&a), ["vieneu", "whisper-tiny"]);
        assert_eq!(size_of(&b), 1500);
        // copying (another volume) keeps the tree and its link
        let c = root.path().join("c");
        copy_tree(&a, &c, 0, &mut 0).unwrap();
        assert_eq!(size_of(&c), 1500);
        assert!(move_contents(&root.path().join("empty-nothing"), &root.path().join("d")).unwrap().moved.is_empty());
    }

    #[test]
    fn only_filmagic_s_own_folders_are_ever_moved() {
        let root = tempfile::tempdir().unwrap();
        let (home, to) = (root.path().join("home"), root.path().join("to"));
        // a folder that is not a models folder: someone's documents, and one Whisper model
        std::fs::create_dir_all(home.join("Documents")).unwrap();
        std::fs::write(home.join("Documents").join("thesis.txt"), b"years of work").unwrap();
        std::fs::write(home.join("notes.txt"), b"x").unwrap();
        std::fs::create_dir_all(home.join("whisper-base")).unwrap();
        let r = move_contents(&home, &to).unwrap();
        assert_eq!(r.moved, ["whisper-base"]);
        assert_eq!(std::fs::read(home.join("Documents").join("thesis.txt")).unwrap(), b"years of work");
        assert!(home.join("notes.txt").exists() && !to.join("Documents").exists());
        if env_set() {
            return;
        }
        // and models.setLocation refuses to move from such a folder at all
        let mut s = saving_session(root.path());
        s.prefs.media_analysis.models_location = home.to_string_lossy().into_owned();
        std::fs::create_dir_all(home.join("whisper-tiny")).unwrap();
        let e = s.execute("models.setLocation", json!({"dir": root.path().join("x").to_string_lossy()})).unwrap_err().to_string();
        assert!(e.contains("did not put there") && e.contains("Documents") && e.contains("nothing was moved"), "{e}");
        assert!(home.join("whisper-tiny").exists() && home.join("Documents").join("thesis.txt").exists());
        assert_eq!(s.models_dir().as_deref(), Some(home.as_path()), "unchanged");
        // switching without moving is allowed (and warns about the models left behind)
        let r = s.execute("models.setLocation", json!({"dir": root.path().join("x").to_string_lossy(), "move": false})).unwrap();
        assert!(r["warnings"][0].as_str().unwrap().contains("whisper-tiny"), "{r}");
        assert!(home.join("Documents").join("thesis.txt").exists());
    }

    #[test]
    fn set_location_moves_the_models_and_reports_leftovers() {
        if env_set() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let (a, b, c) = (root.path().join("a"), root.path().join("b"), root.path().join("c"));
        let mut s = saving_session(root.path());
        s.prefs.media_analysis.models_location = a.to_string_lossy().into_owned();
        fill(&a);
        let r = s.execute("models.info", json!({})).unwrap();
        assert_eq!(r["bytes"], 1500);
        assert_eq!(r["entries"][1]["what"], "speech model (Whisper)");

        let r = s.execute("models.setLocation", json!({"dir": b.to_string_lossy()})).unwrap();
        assert_eq!(r["moved"], json!(["vieneu", "whisper-tiny"]));
        assert_eq!(s.models_dir().as_deref(), Some(b.as_path()));
        assert_eq!(s.prefs.media_analysis.models_location, b.to_string_lossy());
        assert!(std::fs::read_to_string(root.path().join("preferences.json")).unwrap().contains(&*b.to_string_lossy()), "saved");
        assert_eq!(size_of(&b), 1500);
        assert!(elsewhere_warnings(&s).is_empty(), "nothing left behind");

        // leaving them behind is allowed, never silent
        let r = s.execute("models.setLocation", json!({"dir": c.to_string_lossy(), "move": false})).unwrap();
        assert!(r["warnings"][0].as_str().unwrap().contains("still holds"), "{r}");
        let w = elsewhere_warnings(&s);
        assert!(w.iter().any(|w| w.contains(&*b.to_string_lossy()) && w.contains("models.setLocation")), "{w:?}");
        for cmd in ["tts.voices", "transcript.models", "models.info"] {
            let r = s.execute(cmd, json!({})).unwrap();
            assert!(!r["warnings"].as_array().unwrap().is_empty(), "{cmd}: {r}");
        }
        // the same place again is a no-op
        assert_eq!(s.execute("models.setLocation", json!({"dir": c.to_string_lossy()})).unwrap()["moved"], json!([]));
    }

    #[test]
    fn a_session_that_does_not_save_settings_cannot_move_models() {
        if env_set() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let a = root.path().join("a");
        fill(&a);
        let mut s = Session::default();
        assert!(s.prefs_path.is_none(), "like filmagic-cli and headless MCP");
        s.prefs.media_analysis.models_location = a.to_string_lossy().into_owned();
        let e = s.execute("models.setLocation", json!({"dir": root.path().join("b").to_string_lossy()})).unwrap_err().to_string();
        assert!(e.contains("does not save settings") && e.contains(ENV), "{e}");
        assert_eq!(entries(&a), ["vieneu", "whisper-tiny"], "nothing moved");
        assert_eq!(s.execute("models.info", json!({})).unwrap()["savesSettings"], false);
    }

    #[test]
    fn set_location_refuses_hostile_parameters() {
        if env_set() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let a = root.path().join("a");
        let mut s = saving_session(root.path());
        s.prefs.media_analysis.models_location = a.to_string_lossy().into_owned();
        fill(&a);
        let file = root.path().join("a-file");
        std::fs::write(&file, b"x").unwrap();
        for p in [
            json!({}),
            json!({"dir": 5}),
            json!({"dir": "relative"}),
            json!({"dir": a.join("inside").to_string_lossy()}),
            json!({"dir": root.path().to_string_lossy()}),
            json!({"dir": file.to_string_lossy()}),
            json!({"dir": root.path().join("b").to_string_lossy(), "move": "yes"}),
            json!({"dir": format!("/{}", "x".repeat(100_000))}),
        ] {
            assert!(s.execute("models.setLocation", p.clone()).is_err(), "{p}");
        }
        assert_eq!(s.models_dir().as_deref(), Some(a.as_path()), "unchanged");
        assert_eq!(entries(&a), ["vieneu", "whisper-tiny"]);
    }
}
