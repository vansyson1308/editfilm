use super::*;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub(crate) const TOKEN: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";

/// A stand-in for the app's control server holding `token`: it answers `control.hello` with the
/// proof, refuses requests without the credential, answers `engine.execute` with `{"inApp": true}`
/// (`project.inspect` with `{"path": project}`) and records every line it receives.
pub(crate) async fn fake_app(token: &str, project: Option<&str>) -> (u16, Arc<tokio::sync::Mutex<Vec<Value>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let (log, token, project) = (seen.clone(), token.to_string(), project.map(str::to_string));
    tokio::spawn(async move {
        while let Ok((s, _)) = listener.accept().await {
            let (log, token, project) = (log.clone(), token.clone(), project.clone());
            tokio::spawn(async move {
                let (r, mut w) = s.into_split();
                let mut lines = BufReader::new(r).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let req: Value = serde_json::from_str(&line).unwrap_or_default();
                    log.lock().await.push(req.clone());
                    let reply = if req["method"] == "control.hello" {
                        let proof = control_file::proof(&token, port, req["params"]["nonce"].as_str().unwrap_or_default()).unwrap();
                        json!({"id": req["id"], "ok": true, "result": {"app": "filmagic", "port": port, "proof": proof}})
                    } else if req["token"] != json!(token) {
                        json!({"ok": false, "error": "control authentication failed"})
                    } else if req["params"]["command"] == "project.inspect" {
                        json!({"id": req["id"], "ok": true, "result": {"path": project}})
                    } else if req["method"] == "ui.inspect" {
                        json!({"id": req["id"], "ok": true, "result": {"dialog": null}})
                    } else {
                        json!({"id": req["id"], "ok": true, "result": {"inApp": true}})
                    };
                    let _ = w.write_all(format!("{reply}\n").as_bytes()).await;
                }
            });
        }
    });
    (port, seen)
}

/// Make this process a live app in `data_dir`'s control folder with an endpoint on `port`: hold the
/// instance lock (returned) and write the endpoint (`token` as its session credential).
pub(crate) fn be_live_app(data_dir: &Path, port: u16, token: Option<&str>) -> control_file::InstanceLock {
    let dir = control_file::control_dir(data_dir).unwrap();
    let lock = control_file::InstanceLock::acquire(&dir).unwrap();
    let e = ControlEndpoint { port, pid: std::process::id(), version: String::new(), token: token.map(str::to_string) };
    control_file::write(&dir, &e).unwrap();
    lock
}

fn endpoint(port: u16, token: Option<&str>) -> ControlEndpoint {
    ControlEndpoint { port, pid: std::process::id(), version: String::new(), token: token.map(str::to_string) }
}

/// A port nothing listens on.
fn dead_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

#[tokio::test]
async fn a_live_app_that_proves_itself_is_found() {
    let data = tempfile::tempdir().unwrap();
    let (port, _) = fake_app(TOKEN, None).await;
    let _lock = be_live_app(data.path(), port, Some(TOKEN));
    let found = find_app(Some(data.path()), None).await;
    assert_eq!(found, Found::Live(LiveApp { endpoint: endpoint(port, Some(TOKEN)), token: TOKEN.into() }));
    assert_eq!(choose(None, false, &found, None), Start::Bridge { addr: format!("127.0.0.1:{port}"), token: Some(TOKEN.into()), from_file: true });
}

/// S1 (#17 review): a program listening on a live app's endpoint port that cannot prove the
/// credential is not attached to, and never receives the credential.
#[tokio::test]
async fn an_impostor_on_the_port_is_not_attached_to() {
    let data = tempfile::tempdir().unwrap();
    let other = "b".repeat(64);
    let (port, seen) = fake_app(&other, None).await;
    let _lock = be_live_app(data.path(), port, Some(TOKEN));
    let found = find_app(Some(data.path()), None).await;
    assert!(matches!(&found, Found::NotAnswering { .. }), "{found:?}");
    assert!(matches!(choose(None, false, &found, None), Start::Headless { why: Some(w) } if w.contains("does not answer")));
    let seen = seen.lock().await;
    assert!(!seen.is_empty() && seen.iter().all(|r| r["method"] == "control.hello" && r.get("token").is_none()), "{seen:?}");
}

/// An endpoint left by an app that ended (its instance lock is free) is ignored and removed, with
/// no network: no answer is needed to tell (#17).
#[tokio::test]
async fn an_ended_app_is_ignored_and_cleaned_up() {
    let data = tempfile::tempdir().unwrap();
    let dir = control_file::control_dir(data.path()).unwrap();
    let dead = std::process::id().wrapping_add(7);
    control_file::write(&dir, &ControlEndpoint { port: dead_port(), pid: dead, version: String::new(), token: Some(TOKEN.into()) }).unwrap();
    // a crash leaves the lock file, unlocked
    std::fs::write(dir.join(format!("instance-{dead}.lock")), b"").unwrap();
    assert_eq!(find_app(Some(data.path()), None).await, Found::Missing);
    assert!(control_file::endpoints(&dir).is_empty(), "the ended app's file is gone");
    // an endpoint with no lock file at all is asked, and does not answer
    control_file::write(&dir, &ControlEndpoint { port: dead_port(), pid: dead, version: String::new(), token: Some(TOKEN.into()) }).unwrap();
    assert!(matches!(find_app(Some(data.path()), None).await, Found::NotAnswering { .. }));
    assert_eq!(find_app(None, None).await, Found::NoDataDir);
}

/// S3 (#17 review): Filmagic open without `--control` is reported as such.
#[tokio::test]
async fn an_app_without_control_server_is_reported() {
    let data = tempfile::tempdir().unwrap();
    let dir = control_file::control_dir(data.path()).unwrap();
    let _lock = control_file::InstanceLock::acquire(&dir).unwrap();
    let found = find_app(Some(data.path()), None).await;
    assert_eq!(found, Found::NoControl(vec![std::process::id()]));
    assert!(found.why_not().unwrap().contains("without its control server"));
}

/// S6 (#17 review): an app with an inherited credential writes none; a client needs the variable.
#[tokio::test]
async fn an_inherited_credential_comes_from_the_environment() {
    let data = tempfile::tempdir().unwrap();
    let (port, _) = fake_app(TOKEN, None).await;
    let _lock = be_live_app(data.path(), port, None);
    let found = find_app(Some(data.path()), None).await;
    assert!(matches!(&found, Found::NeedsToken(e) if e.port == port), "{found:?}");
    assert!(found.why_not().unwrap().contains("FILMAGIC_CONTROL_TOKEN"));
    assert!(matches!(find_app(Some(data.path()), Some(TOKEN)).await, Found::Live(_)));
}

#[tokio::test]
async fn an_unusable_folder_entry_is_reported() {
    let data = tempfile::tempdir().unwrap();
    let dir = control_file::control_dir(data.path()).unwrap();
    let _lock = control_file::InstanceLock::acquire(&dir).unwrap();
    std::fs::create_dir(dir.join(format!("app-{}.json", std::process::id()))).unwrap();
    let found = find_app(Some(data.path()), None).await;
    // the lock is live and its endpoint unusable: reported as an app without a control server
    assert!(matches!(found, Found::NoControl(_)), "{found:?}");
}

/// The auto-attach decision (#17): explicit flags win, and the live app is attached otherwise.
#[test]
fn the_start_decision() {
    let live = Found::Live(LiveApp { endpoint: endpoint(4000, Some(TOKEN)), token: TOKEN.into() });
    assert_eq!(choose(None, true, &live, None), Start::Headless { why: None }, "--headless / --demo / --project");
    let env = "e".repeat(64);
    assert_eq!(
        choose(Some("127.0.0.1:4000"), false, &live, Some(env.clone())),
        Start::Bridge { addr: "127.0.0.1:4000".into(), token: Some(env), from_file: false }
    );
    assert_eq!(
        choose(Some("127.0.0.1:4000"), false, &live, Some(String::new())),
        Start::Bridge { addr: "127.0.0.1:4000".into(), token: Some(TOKEN.into()), from_file: false }
    );
    assert_eq!(choose(Some("127.0.0.1:4001"), false, &live, None), Start::Bridge { addr: "127.0.0.1:4001".into(), token: None, from_file: false });
    for addr in ["", ":", "127.0.0.1:", "127.0.0.1:-1", "127.0.0.1:99999", "nonsense"] {
        assert!(matches!(choose(Some(addr), false, &live, None), Start::Bridge { token: None, .. }), "{addr}");
    }
    for f in [Found::Missing, Found::NoDataDir, Found::NoControl(vec![1]), Found::Invalid("x".into()), Found::NeedsToken(endpoint(1, None))] {
        assert!(matches!(choose(None, false, &f, None), Start::Headless { why: Some(_) }), "{f:?}");
    }
}

#[test]
fn a_cli_bridge_takes_the_variable_or_the_live_apps_credential_for_its_port() {
    let data = tempfile::tempdir().unwrap();
    assert_eq!(token_for("127.0.0.1:4000", Some(data.path()), None), None);
    let lock = be_live_app(data.path(), 4000, Some(TOKEN));
    assert_eq!(token_for("127.0.0.1:4000", Some(data.path()), None), Some(TOKEN.into()));
    assert_eq!(token_for("127.0.0.1:4000", Some(data.path()), Some("e".repeat(64))), Some("e".repeat(64)));
    assert_eq!(token_for("127.0.0.1:4001", Some(data.path()), None), None);
    assert_eq!(token_for("127.0.0.1:4000", None, None), None);
    // an app that exits removes its endpoint, then drops its lock
    let dir = control_file::control_dir(data.path()).unwrap();
    assert!(control_file::remove_own(&dir, std::process::id(), 4000).unwrap());
    drop(lock);
    assert_eq!(token_for("127.0.0.1:4000", Some(data.path()), None), None, "an app that ended");
}

#[test]
fn the_app_is_launched_with_a_free_port_the_data_dir_and_the_project() {
    let a = launch_args(Path::new("/data"), Some("-odd name.fcproj"));
    let a: Vec<_> = a.iter().map(|s| s.to_string_lossy().into_owned()).collect();
    assert_eq!(a, ["--control", "0", "--data-dir", "/data", "--", "-odd name.fcproj"]);
    assert_eq!(launch_args(Path::new("/data"), None).len(), 4);
}

/// A malformed inherited credential would keep the launched app's server closed: refused at once
/// instead of after the full wait.
#[test]
fn a_launch_with_a_bad_inherited_credential_is_refused_at_once() {
    assert!(check_launch_env(Some("short")).unwrap_err().to_string().contains("64 hex"));
    assert!(check_launch_env(Some(&"z".repeat(64))).is_err());
    for ok in [None, Some(""), Some(TOKEN)] {
        assert!(check_launch_env(ok).is_ok(), "{ok:?}");
    }
}

/// A launched program that exits before answering is reported with the log to read.
#[cfg(unix)]
#[tokio::test]
async fn a_launch_that_exits_is_an_error_naming_the_log() {
    let data = tempfile::tempdir().unwrap();
    let e = launch(Path::new("/bin/false"), data.path(), None, None).await.unwrap_err().to_string();
    assert!(e.contains("exited") && e.contains("filmagic.log"), "{e}");
}

/// The wait accepts only a new app that proves itself: one live before the launch is not it.
#[tokio::test]
async fn the_wait_returns_the_new_app_only() {
    let data = tempfile::tempdir().unwrap();
    let dir = control_file::control_dir(data.path()).unwrap();
    let (port, _) = fake_app(TOKEN, None).await;
    let lock = be_live_app(data.path(), port, Some(TOKEN));
    let me = std::process::id();
    let log = data.path().join("filmagic.log");
    let e = wait_for_new(&dir, &[me], None, Duration::from_millis(600), || None, &log).await.unwrap_err().to_string();
    assert!(e.contains("did not answer"), "{e}");
    drop(lock);
    // the "app" comes up a moment after the launch
    let d = data.path().to_path_buf();
    let later = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        be_live_app(&d, port, Some(TOKEN))
    });
    let got = wait_for_new(&dir, &[], None, Duration::from_secs(10), || None, &log).await.unwrap();
    assert_eq!(got.endpoint.port, port);
    drop(later.await.unwrap());
}
