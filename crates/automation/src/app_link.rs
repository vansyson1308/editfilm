//! Which Filmagic an MCP server drives: the open desktop app, or a headless session (#17).
//!
//! The desktop app started with `--control` writes its port and credential to the control file in
//! its data directory (`filmagic_engine::control_file`). `filmagic-cli mcp` without a mode flag
//! reads it: when the app named there answers with that credential (`control.ping`), the server
//! drives that app; otherwise it works headless. The decision is made once, at start: a server
//! never moves an agent's edits to another document behind its back. Mid-session, the `app_open`
//! tool moves a headless session into the app on purpose (save, launch, attach), and `app_status`
//! says which one the agent is driving.

use std::path::{Path, PathBuf};
use std::time::Duration;

use filmagic_engine::control_file::{self, ControlEndpoint, ReadError};

use crate::AutomationError;
use crate::bridge::ping;

/// How long a control-file app gets to answer `control.ping` before it counts as not running.
pub const PING_TIMEOUT: Duration = Duration::from_secs(2);
/// How long `app_open` waits for a launched app's control server.
pub const LAUNCH_TIMEOUT: Duration = Duration::from_secs(90);
/// The environment variable naming the desktop app's executable, when it is not next to
/// `filmagic-cli`.
pub const APP_ENV: &str = "FILMAGIC_APP";

/// What the control file says about a running app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// The app in the file answered with its credential.
    Live(ControlEndpoint),
    /// The file names an app that does not answer (it crashed, or its port now belongs to another
    /// program).
    Stale { endpoint: ControlEndpoint, why: String },
    /// There is no control file: no app with `--control` is running.
    Missing,
    /// The file exists but is not usable.
    Invalid(String),
    /// There is no data directory to look in.
    NoDataDir,
}

impl Found {
    /// The live app, if any.
    pub fn live(&self) -> Option<&ControlEndpoint> {
        match self {
            Self::Live(e) => Some(e),
            _ => None,
        }
    }

    /// Why there is no app to attach to, as one sentence, or `None` for a live one.
    pub fn why_not(&self) -> Option<String> {
        match self {
            Self::Live(_) => None,
            Self::Stale { endpoint, why } => {
                Some(format!("the control file names an app on {} that does not answer ({why}); it was probably closed or crashed", endpoint.addr()))
            }
            Self::Missing => Some("no Filmagic app with a control server is running (no control file)".into()),
            Self::Invalid(why) => Some(format!("the control file cannot be used: {why}")),
            Self::NoDataDir => Some("there is no data directory to find the app's control file in (set FILMAGIC_DATA_DIR or --data-dir)".into()),
        }
    }
}

/// Look for a running app through the control file in `data_dir`.
pub async fn find_app(data_dir: Option<&Path>) -> Found {
    let Some(dir) = data_dir else { return Found::NoDataDir };
    let endpoint = match control_file::read(dir) {
        Ok(e) => e,
        Err(ReadError::Missing) => return Found::Missing,
        Err(ReadError::Invalid(why)) => return Found::Invalid(why),
    };
    match ping(&endpoint.addr(), Some(&endpoint.token), PING_TIMEOUT).await {
        Ok(_) => Found::Live(endpoint),
        Err(e) => Found::Stale { endpoint, why: e.to_string() },
    }
}

/// How the MCP server starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Start {
    /// Drive the app at `addr` with `token`. `from_file`: found through the control file.
    Bridge { addr: String, token: Option<String>, from_file: bool },
    /// Work headless; `why` says why no app was attached (`None`: asked for).
    Headless { why: Option<String> },
}

/// The start-up decision of `filmagic-cli mcp`:
/// - `--bridge ADDR` drives that address (the credential: `FILMAGIC_CONTROL_TOKEN`, else the
///   control file's when it names the same port);
/// - `--headless`, `--demo` or `--project` work headless;
/// - otherwise attach to the live app in the control file, or work headless when there is none.
pub fn choose(bridge: Option<&str>, headless: bool, found: &Found, env_token: Option<String>) -> Start {
    let env_token = env_token.filter(|t| !t.is_empty());
    if let Some(addr) = bridge {
        return Start::Bridge { addr: addr.to_string(), token: env_token.or_else(|| file_token_for(addr, found)), from_file: false };
    }
    if headless {
        return Start::Headless { why: None };
    }
    match found {
        Found::Live(e) => Start::Bridge { addr: e.addr(), token: Some(e.token.clone()), from_file: true },
        other => Start::Headless { why: other.why_not() },
    }
}

/// The credential for `--bridge addr` outside MCP (`filmagic-cli exec --bridge …`): `env`
/// (`FILMAGIC_CONTROL_TOKEN`) when set, else the control file's in `data_dir` when it names that
/// port. The file is not pinged here: a dead app is reported when the command connects.
pub fn token_for(addr: &str, data_dir: Option<&Path>, env: Option<String>) -> Option<String> {
    if let Some(t) = env.filter(|t| !t.is_empty()) {
        return Some(t);
    }
    let endpoint = control_file::read(data_dir?).ok()?;
    file_token_for(addr, &Found::Stale { endpoint, why: String::new() })
}

/// The control file's credential for `addr`, when the file names that port (live or not: the caller
/// reports a failure to connect with its own message).
fn file_token_for(addr: &str, found: &Found) -> Option<String> {
    let port: u16 = addr.rsplit_once(':')?.1.parse().ok()?;
    let e = match found {
        Found::Live(e) | Found::Stale { endpoint: e, .. } => e,
        _ => return None,
    };
    (e.port == port).then(|| e.token.clone())
}

/// The desktop app's executable: `FILMAGIC_APP`, else `filmagic` next to this program (the release
/// archives and `target/<profile>/` put them side by side). Never a path an agent chose.
pub fn app_executable() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os(APP_ENV).filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(p));
    }
    let exe = std::env::current_exe().map_err(|e| format!("cannot find this program's folder: {e}"))?;
    let dir = exe.parent().ok_or("cannot find this program's folder")?;
    let app = dir.join(format!("filmagic{}", std::env::consts::EXE_SUFFIX));
    if app.is_file() {
        Ok(app)
    } else {
        Err(format!("the Filmagic app is not at {}; set {APP_ENV} to the path of the filmagic executable in the MCP server's environment", app.display()))
    }
}

/// The arguments `app_open` starts the app with: a free control port, the same data directory (so
/// its control file is where this server looks), and the project.
pub fn launch_args(data_dir: &Path, project: Option<&str>) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = vec!["--control".into(), "0".into(), "--data-dir".into(), data_dir.as_os_str().to_owned()];
    if let Some(p) = project {
        // after `--`, so a file name that starts with `-` is still a file
        args.push("--".into());
        args.push(p.into());
    }
    args
}

/// Start the app `exe` with [`launch_args`] and wait until its control server answers. `previous` is
/// the control file's endpoint before the launch (a stale one), so it is not mistaken for the new
/// app. The app outlives this server: it gets no standard streams of ours (they carry MCP) and its
/// own process group.
pub async fn launch(exe: &Path, data_dir: &Path, project: Option<&str>, previous: Option<&ControlEndpoint>) -> Result<ControlEndpoint, AutomationError> {
    let mut cmd = std::process::Command::new(exe);
    cmd.args(launch_args(data_dir, project)).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    detach(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| AutomationError::Other(format!("could not start the Filmagic app {}: {e}", exe.display())))?;
    let log = data_dir.join("Logs").join("filmagic.log");
    let deadline = tokio::time::Instant::now() + LAUNCH_TIMEOUT;
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return Err(AutomationError::Other(format!("the Filmagic app exited ({status}) before its control server answered; its log is {}", log.display())));
        }
        if let Ok(e) = control_file::read(data_dir)
            && Some(&e) != previous
            && ping(&e.addr(), Some(&e.token), PING_TIMEOUT).await.is_ok()
        {
            return Ok(e);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(AutomationError::Other(format!(
                "the Filmagic app started but its control server did not answer within {} s; it is still running. Its log is {}",
                LAUNCH_TIMEOUT.as_secs(),
                log.display()
            )));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Keep the app running when the MCP client stops this server (it may signal the whole group).
fn detach(cmd: &mut std::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        /// No console of ours (the app is a windowed program) and its own Ctrl+C group.
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    #[cfg(not(any(unix, windows)))]
    let _ = cmd;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    fn endpoint(port: u16) -> ControlEndpoint {
        ControlEndpoint { port, token: "a".repeat(64), pid: 1, version: String::new() }
    }

    /// A stand-in for the app's control server: answers `control.ping` for `token`, refuses others.
    async fn fake_app(token: String) -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((s, _)) = listener.accept().await {
                let token = token.clone();
                tokio::spawn(async move {
                    let (r, mut w) = s.into_split();
                    let mut lines = BufReader::new(r).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        let req: Value = serde_json::from_str(&line).unwrap_or_default();
                        let reply = if req["token"] == json!(token) {
                            json!({"id": req["id"], "ok": true, "result": {"app": "filmagic", "pid": 1}})
                        } else {
                            json!({"ok": false, "error": "control authentication failed; closing the connection"})
                        };
                        let _ = w.write_all(format!("{reply}\n").as_bytes()).await;
                    }
                });
            }
        });
        port
    }

    /// A port nothing listens on.
    fn dead_port() -> u16 {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    }

    #[tokio::test]
    async fn a_live_app_in_the_control_file_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let port = fake_app("a".repeat(64)).await;
        control_file::write(dir.path(), &endpoint(port)).unwrap();
        assert_eq!(find_app(Some(dir.path())).await, Found::Live(endpoint(port)));
    }

    /// A file left by a crashed app (nothing listens), or naming a port another program answers
    /// without our credential, is stale: the server works headless instead of failing (#17).
    #[tokio::test]
    async fn a_stale_control_file_is_not_attached_to() {
        let dir = tempfile::tempdir().unwrap();
        let port = dead_port();
        control_file::write(dir.path(), &endpoint(port)).unwrap();
        let found = find_app(Some(dir.path())).await;
        assert!(matches!(&found, Found::Stale { .. }), "{found:?}");
        assert!(matches!(choose(None, false, &found, None), Start::Headless { why: Some(w) } if w.contains("does not answer")));
        // the port is now someone else's server, with another credential
        let other = fake_app("b".repeat(64)).await;
        control_file::write(dir.path(), &endpoint(other)).unwrap();
        assert!(matches!(find_app(Some(dir.path())).await, Found::Stale { .. }));
    }

    #[tokio::test]
    async fn no_file_or_no_data_dir_means_headless() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(find_app(Some(dir.path())).await, Found::Missing);
        assert_eq!(find_app(None).await, Found::NoDataDir);
        std::fs::write(control_file::path(dir.path()), "junk").unwrap();
        let found = find_app(Some(dir.path())).await;
        assert!(matches!(found, Found::Invalid(_)));
        for f in [Found::Missing, Found::NoDataDir, found] {
            assert!(matches!(choose(None, false, &f, None), Start::Headless { why: Some(_) }), "{f:?}");
        }
    }

    /// The auto-attach decision (#17): explicit flags win, and the live app is attached otherwise.
    #[test]
    fn the_start_decision() {
        let live = Found::Live(endpoint(4000));
        let token = "a".repeat(64);
        assert_eq!(choose(None, false, &live, None), Start::Bridge { addr: "127.0.0.1:4000".into(), token: Some(token.clone()), from_file: true });
        // --headless / --demo / --project: headless although the app runs
        assert_eq!(choose(None, true, &live, None), Start::Headless { why: None });
        // --bridge: that address; the variable's credential first, else the file's for the same port
        let env = "e".repeat(64);
        assert_eq!(
            choose(Some("127.0.0.1:4000"), false, &live, Some(env.clone())),
            Start::Bridge { addr: "127.0.0.1:4000".into(), token: Some(env), from_file: false }
        );
        assert_eq!(
            choose(Some("127.0.0.1:4000"), false, &live, Some(String::new())),
            Start::Bridge { addr: "127.0.0.1:4000".into(), token: Some(token), from_file: false }
        );
        // a different port gets no credential from the file
        assert_eq!(choose(Some("127.0.0.1:4001"), false, &live, None), Start::Bridge { addr: "127.0.0.1:4001".into(), token: None, from_file: false });
        // hostile addresses get no file credential either, and never panic
        for addr in ["", ":", "127.0.0.1:", "127.0.0.1:-1", "127.0.0.1:99999", "nonsense"] {
            assert!(matches!(choose(Some(addr), false, &live, None), Start::Bridge { token: None, .. }), "{addr}");
        }
    }

    #[test]
    fn a_cli_bridge_takes_the_variable_or_the_file_credential_for_its_port() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(token_for("127.0.0.1:4000", Some(dir.path()), None), None);
        control_file::write(dir.path(), &endpoint(4000)).unwrap();
        assert_eq!(token_for("127.0.0.1:4000", Some(dir.path()), None), Some("a".repeat(64)));
        assert_eq!(token_for("127.0.0.1:4000", Some(dir.path()), Some("e".repeat(64))), Some("e".repeat(64)));
        assert_eq!(token_for("127.0.0.1:4001", Some(dir.path()), None), None);
        assert_eq!(token_for("127.0.0.1:4000", None, None), None);
    }

    #[test]
    fn the_app_is_launched_with_a_free_port_the_data_dir_and_the_project() {
        let a = launch_args(Path::new("/data"), Some("-odd name.fcproj"));
        let a: Vec<_> = a.iter().map(|s| s.to_string_lossy().into_owned()).collect();
        assert_eq!(a, ["--control", "0", "--data-dir", "/data", "--", "-odd name.fcproj"]);
        assert_eq!(launch_args(Path::new("/data"), None).len(), 4);
    }

    /// A launched program that exits before answering is reported with the log to read.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_launch_that_exits_is_an_error_naming_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let e = launch(Path::new("/bin/false"), dir.path(), None, None).await.unwrap_err().to_string();
        assert!(e.contains("exited") && e.contains("filmagic.log"), "{e}");
    }

    /// The launch waits for a new, answering endpoint: a stale one already in the file is ignored.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_launch_returns_the_new_endpoint_not_the_stale_one() {
        let dir = tempfile::tempdir().unwrap();
        let stale = endpoint(dead_port());
        control_file::write(dir.path(), &stale).unwrap();
        let port = fake_app("c".repeat(64)).await;
        let fresh = ControlEndpoint { port, token: "c".repeat(64), pid: 2, version: String::new() };
        let (d, f) = (dir.path().to_path_buf(), fresh.clone());
        // the "app" writes its file a moment after it starts
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            control_file::write(&d, &f).unwrap();
        });
        // a program that takes the app's arguments and keeps running, as the app does
        let exe = dir.path().join("fake-app.sh");
        std::fs::write(&exe, "#!/bin/sh\nsleep 5\n").unwrap();
        std::fs::set_permissions(&exe, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
        let got = launch(&exe, dir.path(), Some("p.fcproj"), Some(&stale)).await;
        assert_eq!(got.unwrap(), fresh);
    }
}
