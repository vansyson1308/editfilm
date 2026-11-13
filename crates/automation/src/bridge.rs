//! Client for the desktop app's JSON-lines control protocol. A connection per request in flight,
//! reused once its reply is in; reconnect on failure.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use crate::AutomationError;

/// How often a blocking export's app job is polled.
const JOB_POLL: Duration = Duration::from_millis(250);
/// Idle connections kept for reuse; the app serves at most 16 at once.
const MAX_IDLE: usize = 4;

type Conn = (BufReader<tokio::net::tcp::OwnedReadHalf>, tokio::net::tcp::OwnedWriteHalf);

pub struct BridgeClient {
    addr: String,
    /// Inherited from the same owner-controlled environment as the desktop app.
    token: Option<String>,
    /// Connections with no request in flight. A request takes one (or opens one) for its whole
    /// round trip and puts it back only once the reply is read, so a slow or abandoned request
    /// never holds up the others or leaves its late reply for the next one to read (#1538).
    idle: Mutex<Vec<Conn>>,
    next_id: AtomicU64,
}

impl BridgeClient {
    /// `addr` such as `127.0.0.1:9876` (loopback only). Reads `FILMAGIC_CONTROL_TOKEN`
    /// from the environment; the desktop app requires this credential on every request.
    pub fn new(addr: impl Into<String>) -> Result<Self, AutomationError> {
        Self::with_token(addr, std::env::var("FILMAGIC_CONTROL_TOKEN").ok())
    }

    /// [`Self::new`] with the credential given, e.g. a live app's session credential (#17).
    pub fn with_token(addr: impl Into<String>, token: Option<String>) -> Result<Self, AutomationError> {
        let addr = addr.into();
        check_loopback(&addr)?;
        Ok(Self { addr, token, idle: Mutex::new(Vec::new()), next_id: AtomicU64::new(1) })
    }

    /// The app's address, `127.0.0.1:<port>`.
    pub fn addr(&self) -> &str {
        &self.addr
    }

    /// Whether the Filmagic app that holds this credential answers at this address
    /// (`control.hello`, answered by the app's server thread even while it starts or is busy),
    /// within `timeout`. The credential itself is not sent.
    pub async fn hello(&self, timeout: Duration) -> Result<Value, AutomationError> {
        hello(&self.addr, self.token.as_deref(), timeout).await
    }

    /// Run engine command `id` in the app. A blocking export (`file.exportMedia` with
    /// `wait: true`, see [`crate::long_job::LONG_COMMANDS`]) is started as an app job and polled
    /// here until it finishes, so the app keeps repainting (its status bar shows the progress) and
    /// answering other requests, and no request waits on the control server's 60 s reply limit
    /// (#91, #92). The reply is the same as a blocking export's: `{job, path, result}`, or the
    /// export's error.
    pub async fn execute(&self, id: &str, mut params: Value) -> Result<Value, AutomationError> {
        if !crate::long_job::is_long(id, &params) {
            return self.call("engine.execute", json!({"command": id, "params": params})).await;
        }
        if let Some(p) = params.as_object_mut() {
            p.insert("wait".into(), json!(false));
        }
        let mut start = self.call("engine.execute", json!({"command": id, "params": params})).await?;
        let Some(job) = start.get("job").and_then(Value::as_u64) else { return Ok(start) };
        let (result, offline) = loop {
            tokio::time::sleep(JOB_POLL).await;
            let jobs = self.call("engine.execute", json!({"command": "jobs.list", "params": {}})).await?;
            let Some(j) = jobs.as_array().and_then(|a| a.iter().find(|j| j["id"].as_u64() == Some(job))).cloned() else {
                break (Value::Null, None);
            };
            if j["finished"].as_bool() == Some(true) {
                break (j["result"].clone(), j.get("offlineMedia").cloned());
            }
        };
        if let Some(e) = result.get("error").and_then(Value::as_str) {
            return Err(AutomationError::App(format!("export failed: {e}")));
        }
        if let Some(o) = start.as_object_mut() {
            o.insert("result".into(), result);
            if let Some(off) = offline {
                o.insert("offlineMedia".into(), off);
            }
        }
        Ok(start)
    }

    /// Call a control method; returns `result` or the app's error.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, AutomationError> {
        for attempt in 0..2 {
            // A retry gets a fresh connection: the failed one may have been closed by an app restart.
            let idle = if attempt == 0 { self.idle.lock().await.pop() } else { None };
            let mut conn = match idle {
                Some(c) => c,
                None => self.connect().await?,
            };
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let line = format!("{}\n", json!({"id": id, "token": self.token, "method": method, "params": params}));
            let res: Result<Value, AutomationError> = async {
                conn.1.write_all(line.as_bytes()).await.map_err(|e| AutomationError::Bridge(e.to_string()))?;
                let mut buf = String::new();
                tokio::time::timeout(Duration::from_secs(90), conn.0.read_line(&mut buf))
                    .await
                    .map_err(|_| AutomationError::Bridge("timeout".into()))?
                    .map_err(|e| AutomationError::Bridge(e.to_string()))?;
                serde_json::from_str(&buf).map_err(|e| AutomationError::Bridge(format!("bad reply: {e}")))
            }
            .await;
            match res {
                Ok(v) => {
                    let mut idle = self.idle.lock().await;
                    if idle.len() < MAX_IDLE {
                        idle.push(conn);
                    }
                    drop(idle);
                    return if v.get("ok").and_then(Value::as_bool) == Some(true) {
                        Ok(v.get("result").cloned().unwrap_or(Value::Null))
                    } else {
                        Err(AutomationError::App(v.get("error").and_then(Value::as_str).unwrap_or("error").to_string()))
                    };
                }
                Err(_) if attempt == 0 => {}
                Err(e) => return Err(e),
            }
        }
        Err(AutomationError::Bridge("unreachable".into()))
    }

    async fn connect(&self) -> Result<Conn, AutomationError> {
        let s = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(&self.addr))
            .await
            .map_err(|_| AutomationError::Bridge(format!("timed out connecting to {}", self.addr)))?
            .map_err(|e| AutomationError::Bridge(not_answering(&self.addr, &e)))?;
        let (r, w) = s.into_split();
        let mut conn = (BufReader::new(r), w);
        // Before the credential goes out on a new connection, the server proves it holds it: the
        // port may belong to another program since the app that had it exited (#17).
        if let Some(token) = &self.token {
            tokio::time::timeout(HELLO_TIMEOUT, handshake(&mut conn, &self.addr, token))
                .await
                .map_err(|_| AutomationError::Bridge(format!("{} did not answer control.hello within {} s", self.addr, HELLO_TIMEOUT.as_secs())))??;
        }
        Ok(conn)
    }
}

/// How long a server gets to answer `control.hello`.
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);

/// 32 random bytes from the operating system, as 64 hex characters.
fn new_nonce() -> Result<String, AutomationError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| AutomationError::Other(format!("no random nonce: {e}")))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// The port of `127.0.0.1:<port>`.
fn port_of(addr: &str) -> Option<u16> {
    addr.rsplit_once(':')?.1.parse().ok()
}

/// `control.hello` on `conn`: send a fresh nonce, read the reply and check its proof against `token`
/// and the port of `addr`. The reply's `result` when the server is the app that holds `token`.
async fn handshake(conn: &mut Conn, addr: &str, token: &str) -> Result<Value, AutomationError> {
    let port = port_of(addr).ok_or_else(|| AutomationError::BadRequest(format!("no port in `{addr}`")))?;
    let nonce = new_nonce()?;
    let line = format!("{}\n", json!({"id": 0, "method": "control.hello", "params": {"nonce": nonce}}));
    conn.1.write_all(line.as_bytes()).await.map_err(|e| AutomationError::Bridge(e.to_string()))?;
    let mut buf = String::new();
    // bounded: a program that is not Filmagic may send anything
    (&mut conn.0).take(64 * 1024).read_line(&mut buf).await.map_err(|e| AutomationError::Bridge(e.to_string()))?;
    let impostor = || {
        AutomationError::Bridge(format!(
            "{addr} is not the Filmagic app this credential belongs to (its control.hello proof does not match); nothing was sent to it. Another program may be using the port of an app that has exited"
        ))
    };
    let v: Value = serde_json::from_str(&buf).map_err(|_| impostor())?;
    let result = v.get("result").filter(|_| v.get("ok").and_then(Value::as_bool) == Some(true)).ok_or_else(impostor)?;
    let proof = result.get("proof").and_then(Value::as_str).ok_or_else(impostor)?;
    if result.get("app").and_then(Value::as_str) != Some("filmagic") || !filmagic_engine::control_file::verify_proof(token, port, &nonce, proof) {
        return Err(impostor());
    }
    Ok(result.clone())
}

/// The error when nothing listens at `addr`: usually the user closed the app (#17).
fn not_answering(addr: &str, e: &std::io::Error) -> String {
    format!(
        "Filmagic is not answering at {addr} ({e}): the app was closed, or was not started with `--control`. From MCP, call the `app_open` tool to start it again with your project; otherwise start it with `filmagic --control <port>`. The credential comes from the app's control folder (for its own session credential), or from FILMAGIC_CONTROL_TOKEN set to the same 64 hex characters on both sides (see docs/control-protocol.md)"
    )
}

/// Refuse a non-loopback address: the control server only listens on 127.0.0.1.
fn check_loopback(addr: &str) -> Result<(), AutomationError> {
    let host = addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(addr);
    if !matches!(host, "127.0.0.1" | "localhost" | "[::1]" | "::1") {
        return Err(AutomationError::BadRequest(format!("bridge address must be loopback, got `{addr}`")));
    }
    Ok(())
}

/// `control.hello` on a fresh connection, within `timeout`: `Ok` only when the server proves it is
/// the Filmagic app that holds `token`. The credential is never sent, so asking a program that only
/// took over the port reveals nothing to it.
pub async fn hello(addr: &str, token: Option<&str>, timeout: Duration) -> Result<Value, AutomationError> {
    check_loopback(addr)?;
    let token = token.ok_or_else(|| AutomationError::Bridge(format!("no credential to check {addr} with: set FILMAGIC_CONTROL_TOKEN to the app's value")))?;
    let exchange = async {
        let s = TcpStream::connect(addr).await.map_err(|e| AutomationError::Bridge(not_answering(addr, &e)))?;
        let (r, w) = s.into_split();
        let mut conn = (BufReader::new(r), w);
        handshake(&mut conn, addr, token).await
    };
    tokio::time::timeout(timeout, exchange).await.map_err(|_| AutomationError::Bridge(format!("{addr} did not answer within {} ms", timeout.as_millis())))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::net::TcpListener;

    /// The credential the stand-in apps hold.
    const TEST_TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    /// A stand-in's reply to `control.hello` for `TEST_TOKEN` on `port`, or `None` for other requests.
    fn hello_reply(req: &Value, port: u16) -> Option<Value> {
        (req["method"] == "control.hello").then(|| {
            let nonce = req["params"]["nonce"].as_str().unwrap_or_default();
            let proof = filmagic_engine::control_file::proof(TEST_TOKEN, port, nonce).unwrap();
            json!({"id": req["id"], "ok": true, "result": {"app": "filmagic", "port": port, "proof": proof}})
        })
    }

    /// A stand-in for the app's control server: the export starts job 7, which `jobs.list` reports
    /// running twice and then finished with `result`. Every request is recorded.
    async fn fake_app(result: Value) -> (String, Arc<Mutex<Vec<Value>>>) {
        fake_app_with(result, None).await
    }

    /// [`fake_app`] whose finished job also lists `offline` as its `offlineMedia`.
    async fn fake_app_with(result: Value, offline: Option<Value>) -> (String, Arc<Mutex<Vec<Value>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            let (s, _) = listener.accept().await.unwrap();
            let (r, mut w) = s.into_split();
            let mut lines = BufReader::new(r).lines();
            let mut polls = 0;
            while let Ok(Some(line)) = lines.next_line().await {
                let req: Value = serde_json::from_str(&line).unwrap();
                if let Some(reply) = hello_reply(&req, port) {
                    w.write_all(format!("{reply}\n").as_bytes()).await.unwrap();
                    continue;
                }
                log.lock().await.push(req.clone());
                let reply = match req["params"]["command"].as_str() {
                    Some("file.exportMedia") => json!({"job": 7, "path": "/tmp/out.mov"}),
                    Some("jobs.list") => {
                        polls += 1;
                        let finished = polls >= 3;
                        let mut j = json!({"id": 7, "finished": finished, "result": if finished { result.clone() } else { Value::Null }});
                        if let Some(o) = offline.clone().filter(|_| finished) {
                            j["offlineMedia"] = o;
                        }
                        json!([j])
                    }
                    _ => json!(null),
                };
                let line = format!("{}\n", json!({"id": req["id"], "ok": true, "result": reply}));
                w.write_all(line.as_bytes()).await.unwrap();
            }
        });
        (addr, seen)
    }

    /// A refused connection names the launch command and the token requirement (#3436): the
    /// reporter spent two ports and a debug-level console capture on a bare "connection refused".
    #[tokio::test]
    async fn refused_connection_error_names_the_token_requirement() {
        // Bind and drop: connecting to the freed address is deterministically refused.
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        drop(listener);
        let b = BridgeClient::new(addr.as_str()).unwrap();
        let err = b.call("ui.inspect", json!({})).await.unwrap_err().to_string();
        assert!(err.contains("--control"), "{err}");
        assert!(err.contains("FILMAGIC_CONTROL_TOKEN"), "{err}");
    }

    /// A blocking bridge export runs as an app job (#91, #92): the app never sees `wait: true` (it
    /// would encode on its UI thread and the reply would time out after 60 s), and the caller still
    /// gets the finished result.
    #[tokio::test]
    async fn a_blocking_export_is_polled_as_a_job() {
        let (addr, seen) = fake_app(json!({"frames": 642})).await;
        let b = BridgeClient::new(addr).unwrap();
        let r = b.execute("file.exportMedia", json!({"path": "/tmp/out.mov", "wait": true})).await.unwrap();
        assert_eq!(r, json!({"job": 7, "path": "/tmp/out.mov", "result": {"frames": 642}}));
        let seen = seen.lock().await;
        assert_eq!(seen[0]["params"]["params"]["wait"], json!(false));
        assert!(seen.iter().all(|q| q["params"]["params"]["wait"] != json!(true)));
        assert_eq!(seen.iter().filter(|q| q["params"]["command"] == "file.exportMedia").count(), 1, "started once");
        assert_eq!(seen.iter().filter(|q| q["params"]["command"] == "jobs.list").count(), 3);
    }

    /// The offline media a polled export drew comes back as in a blocking export's reply (#1054).
    #[tokio::test]
    async fn a_polled_export_reports_its_offline_media() {
        let off = json!([{"item": 8, "status": "missing"}]);
        let (addr, _) = fake_app_with(json!({"frames": 3}), Some(off.clone())).await;
        let b = BridgeClient::new(addr).unwrap();
        let r = b.execute("file.exportMedia", json!({"path": "/tmp/out.mov", "wait": true})).await.unwrap();
        assert_eq!(r["offlineMedia"], off, "{r}");
    }

    #[tokio::test]
    async fn a_failed_blocking_export_is_an_error() {
        let (addr, _) = fake_app(json!({"error": "encode: disk full"})).await;
        let b = BridgeClient::new(addr).unwrap();
        let e = b.execute("file.exportMedia", json!({"path": "/tmp/out.mov", "wait": true})).await.unwrap_err();
        assert!(e.to_string().contains("export failed: encode: disk full"), "{e}");
    }

    /// A stand-in for the app that serves any number of connections and answers each request on
    /// its own, as the app does: `slow` after `slow_reply` (never when `None`), anything else at
    /// once with its command id. The ids of the requests it receives are sent to the returned channel.
    async fn slow_app(slow_reply: Option<Duration>) -> (String, tokio::sync::mpsc::UnboundedReceiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let (seen, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Ok((s, _)) = listener.accept().await {
                let seen = seen.clone();
                tokio::spawn(async move {
                    let (r, w) = s.into_split();
                    let w = Arc::new(Mutex::new(w));
                    let mut lines = BufReader::new(r).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        let req: Value = serde_json::from_str(&line).unwrap();
                        let cmd = req["params"]["command"].as_str().unwrap_or_default().to_string();
                        let _ = seen.send(cmd.clone());
                        let delay = if cmd == "slow" { slow_reply } else { Some(Duration::ZERO) };
                        let w = w.clone();
                        tokio::spawn(async move {
                            let Some(d) = delay else { return };
                            tokio::time::sleep(d).await;
                            let line = format!("{}\n", json!({"id": req["id"], "ok": true, "result": cmd}));
                            let _ = w.lock().await.write_all(line.as_bytes()).await;
                        });
                    }
                });
            }
        });
        (addr, rx)
    }

    /// A command the app is slow to answer holds up only itself: a later call is answered at once
    /// instead of queueing behind it, and behind every call queued after it (#1538).
    #[tokio::test]
    async fn a_slow_command_does_not_block_later_calls() {
        let (addr, mut seen) = slow_app(None).await;
        let b = Arc::new(BridgeClient::new(addr).unwrap());
        let slow = tokio::spawn({
            let b = b.clone();
            async move { b.execute("slow", json!({})).await }
        });
        assert_eq!(seen.recv().await.as_deref(), Some("slow"));
        let fast = tokio::time::timeout(Duration::from_secs(5), b.execute("fast", json!({}))).await;
        assert_eq!(fast.expect("the later call waited on the slow one").unwrap(), json!("fast"));
        slow.abort();
    }

    /// A call given up while it waits leaves no late reply on a connection for the next call to read
    /// as its own (#1538).
    #[tokio::test]
    async fn an_abandoned_call_leaves_no_late_reply_behind() {
        let (addr, mut seen) = slow_app(Some(Duration::from_millis(100))).await;
        let b = Arc::new(BridgeClient::new(addr).unwrap());
        let slow = tokio::spawn({
            let b = b.clone();
            async move { b.execute("slow", json!({})).await }
        });
        assert_eq!(seen.recv().await.as_deref(), Some("slow"));
        slow.abort();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(b.execute("fast", json!({})).await.unwrap(), json!("fast"));
    }

    /// Everything else is forwarded unchanged.
    #[tokio::test]
    async fn other_commands_are_forwarded() {
        let (addr, seen) = fake_app(Value::Null).await;
        let b = BridgeClient::new(addr).unwrap();
        b.execute("file.exportMedia", json!({"path": "/tmp/out.mov"})).await.unwrap();
        b.execute("sequence.inspect", json!({})).await.unwrap();
        let seen = seen.lock().await;
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[1]["params"]["command"], "sequence.inspect");
    }

    #[tokio::test]
    async fn credential_is_sent_on_every_request() {
        let (addr, seen) = fake_app(Value::Null).await;
        let b = BridgeClient::with_token(addr, Some(TEST_TOKEN.to_string())).unwrap();
        let token = TEST_TOKEN.to_string();
        b.call("ui.inspect", json!({})).await.unwrap();
        b.execute("file.saveAs", json!({"path": "/owner/project.fcproj"})).await.unwrap();
        let seen = seen.lock().await;
        assert_eq!(seen.len(), 2);
        assert!(seen.iter().all(|request| request["token"] == token));
        assert_eq!(seen[1]["params"]["params"]["path"], "/owner/project.fcproj");
    }

    /// A program on the app's port that is not the app: it answers hello with what it can (nothing,
    /// junk, a wrong proof, or the real app's proof for another port, relayed) and records what it
    /// is sent. Returns its address and the lines it received.
    async fn impostor(answer: fn(&Value, u16) -> Option<Value>) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            while let Ok((s, _)) = listener.accept().await {
                let log = log.clone();
                tokio::spawn(async move {
                    let (r, mut w) = s.into_split();
                    let mut lines = BufReader::new(r).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        log.lock().await.push(line.clone());
                        let req: Value = serde_json::from_str(&line).unwrap_or_default();
                        match answer(&req, port) {
                            Some(reply) => {
                                let _ = w.write_all(format!("{reply}\n").as_bytes()).await;
                            }
                            None => {
                                let _ = w.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").await;
                            }
                        }
                    }
                });
            }
        });
        (addr, seen)
    }

    /// S1 (#17 review): a client sends the credential only to a server that proves it holds it. A
    /// listener on the port, however it answers hello, gets neither the credential nor a command.
    #[tokio::test]
    async fn an_impostor_never_receives_the_credential() {
        let answers: [fn(&Value, u16) -> Option<Value>; 5] = [
            |_, _| None,
            |req, _| Some(json!({"id": req["id"], "ok": true, "result": {"app": "filmagic"}})),
            |req, port| Some(json!({"id": req["id"], "ok": true, "result": {"app": "filmagic", "port": port, "proof": "0".repeat(64)}})),
            // the real app's proof, relayed from the port it listens on: bound to that port, not this one
            |req, port| hello_reply(req, port.wrapping_add(1)),
            // the right proof from something that does not say it is Filmagic
            |req, port| {
                hello_reply(req, port).map(|mut v| {
                    v["result"]["app"] = json!("other");
                    v
                })
            },
        ];
        for (n, answer) in answers.into_iter().enumerate() {
            let (addr, seen) = impostor(answer).await;
            let b = BridgeClient::with_token(addr.as_str(), Some(TEST_TOKEN.to_string())).unwrap();
            let e = b.call("engine.execute", json!({"command": "file.saveAs", "params": {"path": "/tmp/x.fcproj"}})).await.unwrap_err().to_string();
            assert!(e.contains("not the Filmagic app") || e.contains("did not answer"), "answer {n}: {e}");
            assert!(hello(&addr, Some(TEST_TOKEN), Duration::from_secs(2)).await.is_err(), "answer {n}");
            let seen = seen.lock().await;
            assert!(seen.iter().all(|l| !l.contains(TEST_TOKEN) && l.contains("control.hello")), "answer {n} received {seen:?}");
        }
    }

    /// The real stand-in passes the same check.
    #[tokio::test]
    async fn the_app_passes_hello() {
        let (addr, _) = fake_app(Value::Null).await;
        let r = hello(&addr, Some(TEST_TOKEN), Duration::from_secs(2)).await.unwrap();
        assert_eq!(r["app"], "filmagic");
        assert!(hello(&addr, None, Duration::from_secs(2)).await.unwrap_err().to_string().contains("no credential"));
    }
}
