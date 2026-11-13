//! Which Filmagic an MCP server drives: the open desktop app, or a headless session (#17).
//!
//! Every running desktop app holds an instance lock in its per-user control folder, and one started
//! with `--control` also writes its endpoint there (`filmagic_engine::control_file`).
//! `filmagic-cli mcp` without a mode flag looks there. When a live app's server proves with
//! `control.hello` that it holds the credential, the MCP server drives that app. Otherwise it works
//! headless. The decision is made once, at start: a server never moves an agent's edits to another
//! document behind its back. Mid-session, the `app_open` tool moves a headless session into the app
//! on purpose (save, launch, attach), and `app_status` says which one the agent is driving.

use std::path::{Path, PathBuf};
use std::time::Duration;

use filmagic_engine::control_file::{self, ControlEndpoint};

use crate::AutomationError;
use crate::bridge::hello;

/// How long a live app gets to answer `control.hello` before it counts as not answering.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(2);
/// How long `app_open` waits for a launched app's control server.
pub const LAUNCH_TIMEOUT: Duration = Duration::from_secs(90);
/// The environment variable naming the desktop app's executable, when it is not next to
/// `filmagic-cli` (on macOS: `Filmagic.app/Contents/MacOS/filmagic`).
pub const APP_ENV: &str = "FILMAGIC_APP";
/// The environment variable of a fixed control credential.
pub const TOKEN_ENV: &str = "FILMAGIC_CONTROL_TOKEN";

/// A live app that proved it holds `token`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveApp {
    pub endpoint: ControlEndpoint,
    pub token: String,
}

/// What the control folder says about running apps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// A live app answered `control.hello` with the proof for its credential.
    Live(LiveApp),
    /// A live app uses `FILMAGIC_CONTROL_TOKEN`, and this process does not have it.
    NeedsToken(ControlEndpoint),
    /// Filmagic is running without a control server (process ids): agents cannot drive it.
    NoControl(Vec<u32>),
    /// A live app's endpoint does not answer as that app (it hangs, or another program has its port).
    NotAnswering { endpoint: ControlEndpoint, why: String },
    /// The folder or a file in it cannot be used.
    Invalid(String),
    /// No Filmagic is running.
    Missing,
    /// There is no control folder to look in.
    NoDataDir,
}

impl Found {
    /// The live app, if any.
    pub fn live(&self) -> Option<&LiveApp> {
        match self {
            Self::Live(a) => Some(a),
            _ => None,
        }
    }

    /// Why there is no app to attach to, as one sentence, or `None` for a live one.
    pub fn why_not(&self) -> Option<String> {
        match self {
            Self::Live(_) => None,
            Self::NeedsToken(e) => Some(format!(
                "the Filmagic app on {} uses FILMAGIC_CONTROL_TOKEN, which this MCP server's environment does not have; start the agent from the shell that set it",
                e.addr()
            )),
            Self::NoControl(pids) => Some(format!(
                "Filmagic is open (process {}) without its control server, so agents cannot drive that window: close it and start it with `--control <port>`, or let `app_open` start one",
                pids.iter().map(u32::to_string).collect::<Vec<_>>().join(", ")
            )),
            Self::NotAnswering { endpoint, why } => Some(format!("the Filmagic app on {} does not answer as that app ({why})", endpoint.addr())),
            Self::Invalid(why) => Some(format!("the control folder cannot be used: {why}")),
            Self::Missing => Some("no Filmagic app is running".into()),
            Self::NoDataDir => {
                Some("there is no control folder (no data directory; on Windows, no %LOCALAPPDATA%): set FILMAGIC_DATA_DIR or --data-dir".into())
            }
        }
    }
}

/// Look for a running app through the control folder of `data_dir`. `env_token` is
/// `FILMAGIC_CONTROL_TOKEN`, for an app whose endpoint holds no credential.
pub async fn find_app(data_dir: Option<&Path>, env_token: Option<&str>) -> Found {
    let Some(dir) = data_dir.and_then(control_file::control_dir) else { return Found::NoDataDir };
    find_in(&dir, env_token).await
}

/// [`find_app`] in a given control folder.
pub async fn find_in(dir: &Path, env_token: Option<&str>) -> Found {
    let env_token = env_token.filter(|t| !t.is_empty());
    // Also removes what apps that ended left behind.
    let live = control_file::live_instances(dir);
    let mut needs_token = None;
    let mut not_answering = None;
    let mut invalid = None;
    let mut with_endpoint = Vec::new();
    for entry in control_file::endpoints(dir) {
        let endpoint = match entry {
            Ok(e) => e,
            Err(why) => {
                invalid.get_or_insert(why);
                continue;
            }
        };
        // an app that ended: its file is gone now; no lock file at all: ask the server
        if !live.contains(&endpoint.pid) && control_file::lock_state(dir, endpoint.pid) == Some(false) {
            continue;
        }
        with_endpoint.push(endpoint.pid);
        let Some(token) = endpoint.token.clone().or_else(|| env_token.map(str::to_string)) else {
            needs_token.get_or_insert(endpoint);
            continue;
        };
        match hello(&endpoint.addr(), Some(&token), HELLO_TIMEOUT).await {
            Ok(_) => return Found::Live(LiveApp { endpoint, token }),
            Err(e) => {
                not_answering.get_or_insert(Found::NotAnswering { endpoint, why: e.to_string() });
            }
        }
    }
    let without: Vec<u32> = live.into_iter().filter(|p| !with_endpoint.contains(p)).collect();
    if let Some(e) = needs_token {
        Found::NeedsToken(e)
    } else if !without.is_empty() {
        Found::NoControl(without)
    } else if let Some(f) = not_answering {
        f
    } else if let Some(why) = invalid {
        Found::Invalid(why)
    } else {
        Found::Missing
    }
}

/// How the MCP server starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Start {
    /// Drive the app at `addr` with `token`. `from_file`: found through the control folder.
    Bridge { addr: String, token: Option<String>, from_file: bool },
    /// Work headless; `why` says why no app was attached (`None`: asked for).
    Headless { why: Option<String> },
}

/// The start-up decision of `filmagic-cli mcp`:
/// - `--bridge ADDR` drives that address (the credential: `FILMAGIC_CONTROL_TOKEN`, else the live
///   app's on that port);
/// - `--headless`, `--demo` or `--project` work headless;
/// - otherwise attach to the live app, or work headless when there is none.
pub fn choose(bridge: Option<&str>, headless: bool, found: &Found, env_token: Option<String>) -> Start {
    let env_token = env_token.filter(|t| !t.is_empty());
    if let Some(addr) = bridge {
        let file_token = found.live().filter(|a| Some(a.endpoint.port) == port_of(addr)).map(|a| a.token.clone());
        return Start::Bridge { addr: addr.to_string(), token: env_token.or(file_token), from_file: false };
    }
    if headless {
        return Start::Headless { why: None };
    }
    match found {
        Found::Live(a) => Start::Bridge { addr: a.endpoint.addr(), token: Some(a.token.clone()), from_file: true },
        other => Start::Headless { why: other.why_not() },
    }
}

/// The port of `127.0.0.1:<port>`.
fn port_of(addr: &str) -> Option<u16> {
    addr.rsplit_once(':')?.1.parse().ok()
}

/// The credential for `--bridge addr` outside MCP (`filmagic-cli exec --bridge …`): `env`
/// (`FILMAGIC_CONTROL_TOKEN`) when set, else the session credential of the live app on that port.
/// The bridge checks the server with `control.hello` before it sends the credential.
pub fn token_for(addr: &str, data_dir: Option<&Path>, env: Option<String>) -> Option<String> {
    if let Some(t) = env.filter(|t| !t.is_empty()) {
        return Some(t);
    }
    let dir = data_dir.and_then(control_file::control_dir)?;
    let port = port_of(addr)?;
    control_file::endpoints(&dir).into_iter().flatten().find(|e| e.port == port && control_file::lock_state(&dir, e.pid) != Some(false)).and_then(|e| e.token)
}

/// The desktop app's executable: `FILMAGIC_APP`, else `filmagic` next to this program (as in the
/// Windows and Linux archives and `target/<profile>/`; on macOS the app is inside its bundle, so
/// set `FILMAGIC_APP`). Never a path an agent chose.
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
        Err(format!(
            "the Filmagic app is not at {}; set {APP_ENV} in the MCP server's environment to the path of the filmagic executable (on macOS, Filmagic.app/Contents/MacOS/filmagic)",
            app.display()
        ))
    }
}

/// Refuse a launch whose app would keep its control server closed: a set but malformed
/// `FILMAGIC_CONTROL_TOKEN` is inherited by the app, which then waits for nobody.
pub fn check_launch_env(env_token: Option<&str>) -> Result<(), AutomationError> {
    match env_token.filter(|t| !t.is_empty()) {
        Some(t) if !control_file::is_valid_token(t) => Err(AutomationError::BadRequest(format!(
            "{TOKEN_ENV} in the MCP server's environment is not 64 hex characters, so an app started from here would keep its control server closed; fix or unset it and restart the agent"
        ))),
        _ => Ok(()),
    }
}

/// The arguments `app_open` starts the app with: a free control port, the same data directory (so
/// its endpoint is where this server looks), and the project.
pub fn launch_args(data_dir: &Path, project: Option<&str>) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = vec!["--control".into(), "0".into(), "--data-dir".into(), data_dir.as_os_str().to_owned()];
    if let Some(p) = project {
        // after `--`, so a file name that starts with `-` is still a file
        args.push("--".into());
        args.push(p.into());
    }
    args
}

/// Start the app `exe` with [`launch_args`] and wait until a new app's control server proves itself.
/// The app outlives this server: it gets no standard streams of ours (they carry MCP) and its own
/// process group.
pub async fn launch(exe: &Path, data_dir: &Path, project: Option<&str>, env_token: Option<&str>) -> Result<LiveApp, AutomationError> {
    check_launch_env(env_token)?;
    let dir = control_file::control_dir(data_dir).ok_or_else(|| AutomationError::Other(Found::NoDataDir.why_not().unwrap_or_default()))?;
    let before: Vec<u32> = control_file::live_instances(&dir);
    let mut cmd = std::process::Command::new(exe);
    cmd.args(launch_args(data_dir, project)).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    detach(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| AutomationError::Other(format!("could not start the Filmagic app {}: {e}", exe.display())))?;
    wait_for_new(
        &dir,
        &before,
        env_token,
        LAUNCH_TIMEOUT,
        || child.try_wait().ok().flatten().map(|s| s.to_string()),
        &data_dir.join("Logs").join("filmagic.log"),
    )
    .await
}

/// Wait until an app not in `before` proves itself in `dir`. `exited` says whether the launched
/// process ended (and how).
async fn wait_for_new(
    dir: &Path,
    before: &[u32],
    env_token: Option<&str>,
    timeout: Duration,
    mut exited: impl FnMut() -> Option<String>,
    log: &Path,
) -> Result<LiveApp, AutomationError> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(status) = exited() {
            return Err(AutomationError::Other(format!("the Filmagic app exited ({status}) before its control server answered; its log is {}", log.display())));
        }
        for e in control_file::endpoints(dir).into_iter().flatten() {
            if before.contains(&e.pid) || control_file::lock_state(dir, e.pid) == Some(false) {
                continue;
            }
            let Some(token) = e.token.clone().or_else(|| env_token.filter(|t| !t.is_empty()).map(str::to_string)) else { continue };
            if hello(&e.addr(), Some(&token), HELLO_TIMEOUT).await.is_ok() {
                return Ok(LiveApp { endpoint: e, token });
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(AutomationError::Other(format!(
                "the Filmagic app started but its control server did not answer within {} s; it is still running. Its log is {}",
                timeout.as_secs(),
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
pub(crate) mod tests;
