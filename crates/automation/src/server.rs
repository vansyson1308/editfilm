//! The MCP server. Tool names are snake_case.

use std::sync::{Arc, Mutex, PoisonError};

use base64::Engine as _;
use filmagic_engine::Session;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock as Content};
use rmcp::{ErrorData as McpError, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{AutomationError, BridgeClient, png_rgba};

pub enum Backend {
    Headless(Arc<Mutex<Session>>),
    Bridge(Arc<BridgeClient>),
}

/// A panic payload as text.
fn panic_message(p: Box<dyn std::any::Any + Send>) -> String {
    p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_else(|| "panic".into())
}

/// A failed blocking task: a panic is reported as an internal error and the session stays usable.
fn join_error(e: tokio::task::JoinError) -> AutomationError {
    match e.try_into_panic() {
        Ok(p) => AutomationError::Other(format!("internal error: {}", panic_message(p))),
        Err(e) => AutomationError::Other(e.to_string()),
    }
}

#[derive(Clone)]
pub struct FilmagicMcp {
    backend: Arc<Backend>,
    /// Bridge `render_frame` moves the app's one playhead and screenshots the one Program monitor, so
    /// calls (which MCP clients may send in parallel) take turns instead of seeing each other's frame.
    render_lock: Arc<tokio::sync::Mutex<()>>,
    tool_router: ToolRouter<Self>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListParams {
    /// Only commands whose id or label contains this text (case-insensitive).
    #[serde(default)]
    pub filter: Option<String>,
    /// Only commands that can run right now.
    #[serde(default)]
    pub enabled_only: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunParams {
    /// Command id, e.g. `sequence.addEdit`, `timeline.trim`, `effects.apply`, `file.import`.
    pub id: String,
    /// Parameters object (see `params` in `command_list`).
    #[serde(default)]
    pub params: Option<Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BatchStep {
    /// Command id.
    pub id: String,
    /// Parameters object.
    #[serde(default)]
    pub params: Option<Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BatchParams {
    /// Commands to run in order.
    pub steps: Vec<BatchStep>,
    /// Stop at the first failing step (default true).
    #[serde(default)]
    pub stop_on_error: Option<bool>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct RenderParams {
    /// Timeline time in seconds (default: the playhead).
    #[serde(default)]
    pub seconds: Option<f64>,
    /// Longest side of the PNG (default 960).
    #[serde(default)]
    pub max_side: Option<u32>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ShotParams {
    /// Panel to crop to (e.g. `Timeline`, `Program`); omit for the whole window.
    #[serde(default)]
    pub panel: Option<String>,
    /// Longest side of the PNG (default 1600).
    #[serde(default)]
    pub max_side: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ClickParams {
    /// Element id from `ui_elements` (e.g. `timeline.track.V1.lock`, `tools.Razor`, `project.item.12`).
    #[serde(default)]
    pub id: Option<String>,
    /// Or screen coordinates in points.
    #[serde(default)]
    pub x: Option<f64>,
    #[serde(default)]
    pub y: Option<f64>,
    /// left | right | middle
    #[serde(default)]
    pub button: Option<String>,
    /// Number of clicks (0–10; defaults to 1). Use 2 for double-click. Larger values are rejected.
    #[serde(default)]
    pub count: Option<u32>,
    /// {"shift":bool,"alt":bool,"command":bool,"ctrl":bool}
    #[serde(default)]
    pub modifiers: Option<Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DragParams {
    /// Start: {"id":..} or {"x":..,"y":..} (optionally "fx"/"fy" fractions inside the element).
    pub from: Value,
    /// End point, same forms.
    pub to: Value,
    /// Movement steps (0–1024; defaults to 12; 0 or 1 uses 2). Larger values are rejected.
    #[serde(default)]
    pub steps: Option<u32>,
    #[serde(default)]
    pub modifiers: Option<Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct KeyParams {
    /// Key with optional modifiers, e.g. `Space`, `Cmd+K`, `Shift+Delete`, `I`, `L`.
    pub key: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TypeParams {
    pub text: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MediaImportParams {
    /// Absolute paths to media files (MP4/MOV, WAV/MP3/FLAC/AIFF/Ogg, PNG/JPEG/…).
    pub paths: Vec<String>,
    /// Bin ID to import into (optional).
    #[serde(default)]
    pub bin: Option<u64>,
    /// Import numbered stills as an image sequence (optional, default false).
    #[serde(default)]
    pub image_sequence: Option<bool>,
}

/// `file.import` params for a `media_import` call.
fn media_import_params(p: MediaImportParams) -> Value {
    let mut params = json!({"paths": p.paths});
    if let Some(bin) = p.bin {
        params["bin"] = json!(bin);
    }
    if let Some(seq) = p.image_sequence {
        params["imageSequence"] = json!(seq);
    }
    params
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ElementsParams {
    /// Only element ids starting with this prefix (e.g. `timeline.clip.`).
    #[serde(default)]
    pub prefix: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ControlParams {
    /// Any control-channel method (e.g. `ui.set`, `ui.scroll`, `ui.timeline.hit`, `ui.playback`).
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct BoardParams {
    /// Project item id: lay out frames across this clip's rushes. Omit for one frame per shot of the active sequence.
    #[serde(default)]
    pub item: Option<u64>,
    /// Frames to show of an `item` (default 12).
    #[serde(default)]
    pub count: Option<u32>,
    /// Most shots of the sequence to show (default 48).
    #[serde(default)]
    pub max: Option<u32>,
    /// Only what lies after this time, in seconds.
    #[serde(default)]
    pub from_seconds: Option<f64>,
    /// Only what lies before this time, in seconds.
    #[serde(default)]
    pub to_seconds: Option<f64>,
    /// Frames per row (default 4).
    #[serde(default)]
    pub columns: Option<u32>,
    /// Width of one frame in pixels (default 320).
    #[serde(default)]
    pub width: Option<u32>,
    /// Whether each frame carries its number and time (default true).
    #[serde(default)]
    pub labels: Option<bool>,
    /// Sample densely instead: this many frames per second across `from_seconds`..`to_seconds` (at most 96 frames).
    #[serde(default)]
    pub fps: Option<f64>,
    /// Or one frame every `step` seconds.
    #[serde(default)]
    pub step: Option<f64>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct InspectParams {
    /// Only this clip id, or a list of clip ids.
    #[serde(default)]
    pub clip: Option<Value>,
    /// Only this track: "V1", "A2"… or a track id.
    #[serde(default)]
    pub track: Option<Value>,
    /// Only clips with this effect (id or name), and only that effect.
    #[serde(default)]
    pub effect: Option<String>,
    /// One short entry per clip (times in seconds, only parameters that differ from their defaults).
    #[serde(default)]
    pub compact: Option<bool>,
}

/// `sequence.inspect` params for an inspection call.
fn inspect_params(p: InspectParams) -> Value {
    let mut v = json!({});
    let fields: [(&str, Option<Value>); 4] =
        [("clip", p.clip), ("track", p.track), ("effect", p.effect.map(Value::from)), ("compact", p.compact.map(Value::from))];
    for (key, value) in fields {
        if let Some(value) = value {
            v[key] = value;
        }
    }
    v
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct MotionParams {
    /// Start, in seconds of the active sequence (default: the playhead).
    #[serde(default)]
    pub from_seconds: Option<f64>,
    /// End, in seconds (default 3 s after the start; at most 10 s after it).
    #[serde(default)]
    pub to_seconds: Option<f64>,
    /// Frames per second, 1 to 30 (default 12 for GIF).
    #[serde(default)]
    pub fps: Option<f64>,
    /// Longest side in pixels, 64 to 640 (default 320 for GIF, 480 for MP4).
    #[serde(default)]
    pub width: Option<u32>,
    /// `gif` (default: returned as an animated image) or `mp4` (written to a file; the path is returned).
    #[serde(default)]
    pub format: Option<String>,
}

/// `story.board` params for a `story_board` call writing to `path`.
fn board_params(p: BoardParams, path: &str) -> Value {
    let mut v = json!({"path": path});
    let fields: [(&str, Option<Value>); 10] = [
        ("fps", p.fps.map(Value::from)),
        ("step", p.step.map(Value::from)),
        ("item", p.item.map(Value::from)),
        ("count", p.count.map(Value::from)),
        ("max", p.max.map(Value::from)),
        ("fromSeconds", p.from_seconds.map(Value::from)),
        ("toSeconds", p.to_seconds.map(Value::from)),
        ("columns", p.columns.map(Value::from)),
        ("width", p.width.map(Value::from)),
        ("labels", p.labels.map(Value::from)),
    ];
    for (key, value) in fields {
        if let Some(value) = value {
            v[key] = value;
        }
    }
    v
}

fn ok_json(v: &Value) -> CallToolResult {
    CallToolResult::success(vec![Content::text(serde_json::to_string_pretty(v).unwrap_or_default())])
}
fn fail(e: impl std::fmt::Display) -> CallToolResult {
    CallToolResult::error(vec![Content::text(e.to_string())])
}
fn png_result(png: &[u8], note: &str) -> CallToolResult {
    CallToolResult::success(vec![Content::image(base64::engine::general_purpose::STANDARD.encode(png), "image/png"), Content::text(note.to_string())])
}
fn wrap(r: Result<Value, AutomationError>) -> Result<CallToolResult, McpError> {
    Ok(match r {
        Ok(v) => ok_json(&v),
        Err(e) => fail(e),
    })
}

/// Parse-error replies queued for the writer before the reader waits for it to catch up.
const MAX_QUEUED_PARSE_ERRORS: usize = 64;

impl FilmagicMcp {
    pub fn headless(session: Session) -> Self {
        Self { backend: Arc::new(Backend::Headless(Arc::new(Mutex::new(session)))), render_lock: Arc::default(), tool_router: Self::tool_router() }
    }
    pub fn bridge(addr: &str) -> Result<Self, AutomationError> {
        Ok(Self { backend: Arc::new(Backend::Bridge(Arc::new(BridgeClient::new(addr)?))), render_lock: Arc::default(), tool_router: Self::tool_router() })
    }

    pub async fn serve_stdio(self) -> Result<(), AutomationError> {
        self.serve_io(tokio::io::stdin(), tokio::io::stdout()).await
    }

    /// Serve MCP over any line stream. A line that isn't JSON gets a JSON-RPC parse error
    /// (`-32700`, id null) instead of being dropped silently; the rest go to the rmcp service. One
    /// writer task owns `output`, so replies never interleave. Parse errors wait for room in a
    /// small queue before more input is read: a client that sends junk and never reads its output
    /// stalls its own input instead of growing the server's memory.
    pub async fn serve_io(
        self,
        input: impl tokio::io::AsyncRead + Unpin + Send + 'static,
        output: impl tokio::io::AsyncWrite + Unpin + Send + 'static,
    ) -> Result<(), AutomationError> {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let (mut to_service, service_in) = tokio::io::duplex(1 << 20);
        let (service_out, from_service) = tokio::io::duplex(1 << 20);
        let (err_tx, mut err_rx) = tokio::sync::mpsc::channel::<String>(MAX_QUEUED_PARSE_ERRORS);
        let reader = tokio::spawn(async move {
            let mut lines = BufReader::new(input).lines();
            // `err_tx.closed()`: the writer is gone (output failed), so stop forwarding requests whose replies cannot be delivered.
            loop {
                let line = tokio::select! {
                    l = lines.next_line() => match l { Ok(Some(l)) => l, _ => break },
                    () = err_tx.closed() => break,
                };
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<Value>(&line) {
                    Ok(_) => {
                        if to_service.write_all(format!("{line}\n").as_bytes()).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let reply = json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": format!("parse error: {e}")}});
                        // the writer is gone (output failed): stop reading
                        if err_tx.send(reply.to_string()).await.is_err() {
                            break;
                        }
                    }
                }
            }
            // EOF: closing the pipe ends the service
            let _ = to_service.shutdown().await;
        });
        let writer = tokio::spawn(async move {
            let mut output = output;
            let mut replies = BufReader::new(from_service).lines();
            loop {
                let line = tokio::select! {
                    l = replies.next_line() => match l { Ok(Some(l)) => l, _ => break },
                    Some(e) = err_rx.recv() => e,
                };
                if output.write_all(format!("{line}\n").as_bytes()).await.is_err() || output.flush().await.is_err() {
                    break;
                }
            }
        });
        let running = self.serve((service_in, service_out)).await.map_err(|e| AutomationError::Other(format!("MCP init: {e}")))?;
        let r = running.waiting().await.map_err(|e| AutomationError::Other(e.to_string()));
        reader.abort();
        let _ = writer.await;
        r.map(|_| ())
    }

    /// Run an engine command on whichever backend.
    pub async fn run(&self, id: &str, params: Value) -> Result<Value, AutomationError> {
        match &*self.backend {
            Backend::Headless(s) => {
                let s = s.clone();
                let id = id.to_string();
                tokio::task::spawn_blocking(move || {
                    // A command that panicked earlier poisoned the lock; the session is still usable.
                    let mut g = s.lock().unwrap_or_else(PoisonError::into_inner);
                    g.execute(&id, params).map_err(AutomationError::from)
                })
                .await
                .map_err(join_error)?
            }
            Backend::Bridge(b) => b.execute(id, params).await,
        }
    }

    /// The project tree plus the active sequence (null when there is none).
    ///
    /// `project.inspect` and `sequence.inspect` are two separate calls (two lock scopes in
    /// headless mode, two round trips to the app in bridge mode), so the active sequence can
    /// change between them if another client, or the app's own UI, switches sequence tabs in
    /// that window (#1477). Pinning `sequence.inspect` to the `activeSequence` id this
    /// `project.inspect` just reported keeps the pair consistent: the returned `sequence`, when
    /// not null, always describes the sequence the returned `project.activeSequence` names.
    async fn document(&self) -> Result<Value, AutomationError> {
        self.document_with(json!({})).await
    }

    /// [`Self::document`] with `sequence.inspect` filters (`clip`, `track`, `effect`, `compact`).
    async fn document_with(&self, filters: Value) -> Result<Value, AutomationError> {
        let project = self.run("project.inspect", json!({})).await?;
        // `ItemId` is a u64 (`item_p` parses the `item` param with `u64_p`): read it as one too,
        // not `as_i64`, so an id above `i64::MAX` is still looked up instead of silently going null.
        let active = project.get("activeSequence").and_then(Value::as_u64);
        let filtered = filters.as_object().is_some_and(|o| !o.is_empty());
        let sequence = match active {
            Some(id) if filtered => {
                let mut p = filters;
                p["item"] = json!(id);
                // a bad filter is the caller's to fix: say so instead of a null sequence
                Some(self.run("sequence.inspect", p).await?)
            }
            Some(id) => self.run("sequence.inspect", json!({"item": id})).await.ok(),
            None => None,
        };
        Ok(json!({"project": project, "sequence": sequence}))
    }

    fn bridge_client(&self) -> Option<Arc<BridgeClient>> {
        match &*self.backend {
            Backend::Bridge(b) => Some(b.clone()),
            Backend::Headless(_) => None,
        }
    }

    async fn ui(&self, method: &str, params: Value) -> Result<CallToolResult, McpError> {
        match self.bridge_client() {
            Some(b) => wrap(b.call(method, params).await),
            None => {
                Ok(fail("this tool drives the live app: run `filmagic-cli mcp --bridge 127.0.0.1:<port>` with the app started as `filmagic --control <port>`"))
            }
        }
    }
}

#[tool_router]
impl FilmagicMcp {
    #[tool(
        title = "List commands",
        description = "List every command (id, label, menu path, shortcut, parameter doc, enabled now). Menus, shortcuts, panels and timeline gestures all map to these ids.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn command_list(&self, Parameters(p): Parameters<ListParams>) -> Result<CallToolResult, McpError> {
        let r = self.run("command.list", json!({})).await.map(|v| {
            let f = p.filter.unwrap_or_default().to_ascii_lowercase();
            let only = p.enabled_only.unwrap_or(false);
            Value::Array(
                v.as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|c| {
                        let id = c["id"].as_str().unwrap_or("").to_ascii_lowercase();
                        let label = c["label"].as_str().unwrap_or("").to_ascii_lowercase();
                        (f.is_empty() || id.contains(&f) || label.contains(&f)) && (!only || c["enabled"].as_bool() == Some(true))
                    })
                    .collect(),
            )
        });
        wrap(r)
    }

    #[tool(
        title = "Run a command",
        description = "Run a command by id with JSON params, e.g. {\"id\":\"timeline.razor\",\"params\":{\"seconds\":3.5}} or {\"id\":\"effects.apply\",\"params\":{\"effect\":\"Gaussian Blur\"}}. Undoable edits land in History.",
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = false, open_world_hint = false)
    )]
    async fn command_run(&self, Parameters(p): Parameters<RunParams>) -> Result<CallToolResult, McpError> {
        wrap(self.run(&p.id, p.params.unwrap_or(json!({}))).await)
    }

    #[tool(
        title = "Run several commands",
        description = "Run several commands in order: {steps: [{id, params?}], stop_on_error?: true}. Returns {completed, failed, results: [{ok, result | error}]}; each edit is its own undo step.",
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = false, open_world_hint = false)
    )]
    async fn command_batch(&self, Parameters(p): Parameters<BatchParams>) -> Result<CallToolResult, McpError> {
        let stop = p.stop_on_error.unwrap_or(true);
        let (mut completed, mut failed, mut results) = (0u32, 0u32, Vec::new());
        for st in p.steps {
            match self.run(&st.id, st.params.unwrap_or(json!({}))).await {
                Ok(v) => {
                    completed += 1;
                    results.push(json!({"ok": true, "result": v}));
                }
                Err(e) => {
                    failed += 1;
                    results.push(json!({"ok": false, "error": e.to_string()}));
                    if stop {
                        break;
                    }
                }
            }
        }
        let v = json!({"completed": completed, "failed": failed, "results": results});
        Ok(if failed > 0 { CallToolResult::error(vec![Content::text(v.to_string())]) } else { ok_json(&v) })
    }

    #[tool(
        title = "Inspect the document",
        description = "The project as JSON: the tree (as project_inspect) plus the active sequence (as sequence_inspect; null when there is none). The same `clip`/`track`/`effect`/`compact` filters as sequence_inspect apply to the sequence.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn doc_inspect(&self, Parameters(p): Parameters<InspectParams>) -> Result<CallToolResult, McpError> {
        wrap(self.document_with(inspect_params(p)).await)
    }

    #[tool(
        title = "Preview motion",
        description = "A few seconds of the active sequence as a small animated GIF (returned as an image) or MP4 (written to a file you delete when done; path returned), for showing a transition, a zoom or slow motion to a person. At most 10 s, 640 px on the longest side, 60 GIF frames, 4 MB. Many vision models see only a GIF's first frame: to look at motion yourself, use story_board with `fps`.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn render_motion(&self, Parameters(p): Parameters<MotionParams>) -> Result<CallToolResult, McpError> {
        let mp4 = match p.format.as_deref().map(str::to_ascii_lowercase).as_deref() {
            None | Some("gif") => false,
            Some("mp4") => true,
            Some(other) => return Ok(fail(format!("render_motion: `format` must be \"gif\" or \"mp4\", not \"{other}\""))),
        };
        // a GIF is handed back and removed; an MP4 is kept for the caller to open
        let dir = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(e) => return Ok(fail(format!("render_motion: {e}"))),
        };
        let path = if mp4 {
            match tempfile::Builder::new().prefix("filmagic-preview-").suffix(".mp4").tempfile().and_then(|f| f.keep().map_err(|e| e.error)) {
                Ok((_, path)) => path,
                Err(e) => return Ok(fail(format!("render_motion: {e}"))),
            }
        } else {
            dir.path().join("preview.gif")
        };
        let mut params = json!({"path": path.to_string_lossy()});
        let fields: [(&str, Option<Value>); 4] = [
            ("fromSeconds", p.from_seconds.map(Value::from)),
            ("toSeconds", p.to_seconds.map(Value::from)),
            ("fps", p.fps.map(Value::from)),
            ("width", p.width.map(Value::from)),
        ];
        for (key, value) in fields {
            if let Some(value) = value {
                params[key] = value;
            }
        }
        let mut info = match self.run("preview.motion", params).await {
            Ok(v) => v,
            Err(e) => {
                if mp4 {
                    let _ = std::fs::remove_file(&path);
                }
                return Ok(fail(e));
            }
        };
        if mp4 {
            return Ok(ok_json(&info));
        }
        let gif = match tokio::fs::read(&path).await {
            Ok(b) => b,
            Err(e) => return Ok(fail(format!("render_motion: {e}"))),
        };
        if let Some(o) = info.as_object_mut() {
            o.remove("path");
        }
        Ok(CallToolResult::success(vec![
            Content::image(base64::engine::general_purpose::STANDARD.encode(gif), "image/gif"),
            Content::text(serde_json::to_string_pretty(&info).unwrap_or_default()),
        ]))
    }

    #[tool(
        title = "Render a preview",
        description = "A still: the active sequence's frame at `seconds` (default: the playhead) as PNG, same as render_frame (bridge mode: Program monitor screenshot). For motion use render_motion.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn render_preview(&self, p: Parameters<RenderParams>) -> Result<CallToolResult, McpError> {
        self.render_frame(p).await
    }

    #[tool(
        title = "Inspect the project tree",
        description = "Project tree (bins and items with ids, types, durations), active sequence, source clip.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn project_inspect(&self) -> Result<CallToolResult, McpError> {
        wrap(self.run("project.inspect", json!({})).await)
    }

    #[tool(
        title = "Inspect the active sequence",
        description = "The active sequence as JSON: settings, tracks, clips (ids, start/duration in ticks and frames, effects), transitions, markers, in/out, playhead, selection. 254016000000 ticks = 1 second. Large: filter with `clip`, `track` or `effect`, or pass `compact: true` for one short entry per clip.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn sequence_inspect(&self, Parameters(p): Parameters<InspectParams>) -> Result<CallToolResult, McpError> {
        wrap(self.run("sequence.inspect", inspect_params(p)).await)
    }

    #[tool(
        title = "Import media",
        description = "Import media files by absolute path (MP4/MOV, WAV/MP3/FLAC/AIFF/Ogg, PNG/JPEG/…).",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = false, open_world_hint = false)
    )]
    async fn media_import(&self, Parameters(p): Parameters<MediaImportParams>) -> Result<CallToolResult, McpError> {
        wrap(self.run("file.import", media_import_params(p)).await)
    }

    #[tool(
        title = "Render a frame",
        description = "Render the active sequence's frame (at `seconds` or the playhead) and return it as PNG.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn render_frame(&self, Parameters(p): Parameters<RenderParams>) -> Result<CallToolResult, McpError> {
        let max = p.max_side.unwrap_or(960);
        match &*self.backend {
            Backend::Headless(s) => {
                let s = s.clone();
                let r = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, AutomationError> {
                    let g = s.lock().unwrap_or_else(PoisonError::into_inner);
                    // read-only: render at `seconds` without moving the playhead (#114)
                    let t = p.seconds.map_or_else(|| g.playhead(), filmagic_time::Tick::from_seconds_f64);
                    let (w, h) =
                        g.active_sequence().map(|q| (q.settings.width, q.settings.height)).ok_or_else(|| AutomationError::Other("no sequence".into()))?;
                    let scale = render_scale(max, w, h);
                    let img = g.try_render_program_at(scale, t).map_err(|e| AutomationError::Other(e.to_string()))?;
                    png_rgba(img.w as u32, img.h as u32, img.over_black_rgba8(), max)
                })
                .await
                .unwrap_or_else(|e| Err(join_error(e)));
                Ok(match r {
                    Ok(png) => png_result(&png, "rendered frame"),
                    Err(e) => fail(e),
                })
            }
            Backend::Bridge(b) => {
                // held until the playhead is back, also for the no-`seconds` screenshot, which must not catch another call's seek
                let _turn = self.render_lock.lock().await;
                let Some(sec) = p.seconds else {
                    return self.screenshot(&b.clone(), Some("Program".into()), max).await;
                };
                // The Program monitor shows the playhead's frame, so the live playhead has to move
                // for the screenshot; put it (and the selection, which may follow it) back after,
                // so the tool stays read-only (#114).
                let before = match self.run("sequence.inspect", json!({})).await {
                    Ok(v) => v,
                    Err(e) => return Ok(fail(e)),
                };
                if let Err(e) = self.run("playhead.set", json!({"seconds": sec})).await {
                    return Ok(fail(e));
                }
                let shot = self.screenshot(&b.clone(), Some("Program".into()), max).await;
                if let Some(t) = before.get("playhead").and_then(Value::as_i64) {
                    let _ = self.run("playhead.set", json!({"time": t})).await;
                }
                // only when it changed: selecting also leaves trim mode
                let now = self.run("sequence.inspect", json!({})).await.ok();
                if let Some(sel) = before.get("selection").filter(|v| v.is_array())
                    && now.as_ref().and_then(|n| n.get("selection")) != Some(sel)
                {
                    let _ = self.run("timeline.select", json!({"clips": sel})).await;
                }
                shot
            }
        }
    }

    #[tool(
        title = "Story board",
        description = "See the film at a glance: one representative frame per shot of the active sequence (or `count` frames across a project `item`'s rushes, cells after a cut inside the source marked \"| CUT\"), laid out left to right, top to bottom, each labelled with its number and time. `fps` or `step` samples a short range densely instead (up to 96 frames). Returns the PNG and, as JSON, what each frame is (shot, times, clip).",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn story_board(&self, Parameters(p): Parameters<BoardParams>) -> Result<CallToolResult, McpError> {
        // The command writes a file (the bridged app can hand back nothing else): a fresh,
        // owner-only folder, as for screenshots, removed when the call returns.
        let dir = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(e) => return Ok(fail(format!("story_board: {e}"))),
        };
        let path = dir.path().join("board.png");
        let info = match self.run("story.board", board_params(p, &path.to_string_lossy())).await {
            Ok(v) => v,
            Err(e) => return Ok(fail(e)),
        };
        let png = match tokio::fs::read(&path).await {
            Ok(b) => b,
            Err(e) => return Ok(fail(format!("story_board: {e}"))),
        };
        // the temporary path means nothing to the caller
        let mut info = info;
        if let Some(o) = info.as_object_mut() {
            o.remove("path");
        }
        Ok(png_result(&png, &serde_json::to_string_pretty(&info).unwrap_or_default()))
    }

    #[tool(
        title = "Live app: UI state",
        description = "Live app: UI state (tool, workspace, panels, timeline zoom, playback, selection, fps).",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn ui_inspect(&self) -> Result<CallToolResult, McpError> {
        self.ui("ui.inspect", json!({})).await
    }

    #[tool(
        title = "Live app: on-screen elements",
        description = "Live app: every on-screen interactive element with id, label and rect (points). Use ids with ui_click/ui_drag.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn ui_elements(&self, Parameters(p): Parameters<ElementsParams>) -> Result<CallToolResult, McpError> {
        self.ui("ui.elements", json!({"prefix": p.prefix.unwrap_or_default()})).await
    }

    /// Forward a click to the live editor; an omitted count is sent as null and uses its default.
    #[tool(
        title = "Live app: click",
        description = "Live app: click an element by id or at x,y (points). Supports right/middle button, double-click (count=2) and modifiers.",
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = false, open_world_hint = false)
    )]
    async fn ui_click(&self, Parameters(p): Parameters<ClickParams>) -> Result<CallToolResult, McpError> {
        let mut v = json!({"button": p.button, "count": p.count, "modifiers": p.modifiers});
        if let Some(id) = p.id {
            v["id"] = json!(id);
        } else {
            v["x"] = json!(p.x);
            v["y"] = json!(p.y);
        }
        self.ui("ui.click", v).await
    }

    /// Forward a drag to the live editor, which applies the step default and validates its bounds.
    #[tool(
        title = "Live app: drag",
        description = "Live app: press-drag-release from one point/element to another (move clips, trim edges, scrub, resize panels, drop project items onto tracks).",
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = false, open_world_hint = false)
    )]
    async fn ui_drag(&self, Parameters(p): Parameters<DragParams>) -> Result<CallToolResult, McpError> {
        self.ui("ui.drag", json!({"from": p.from, "to": p.to, "steps": p.steps, "modifiers": p.modifiers})).await
    }

    #[tool(
        title = "Live app: press a key",
        description = "Live app: press a key or shortcut (e.g. `Space`, `L`, `Cmd+K`, `Shift+Delete`, `Cmd+Z`).",
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = false, open_world_hint = false)
    )]
    async fn ui_key(&self, Parameters(p): Parameters<KeyParams>) -> Result<CallToolResult, McpError> {
        self.ui("ui.key", json!({"key": p.key})).await
    }

    #[tool(
        title = "Live app: type text",
        description = "Live app: type text into the focused field.",
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = false, open_world_hint = false)
    )]
    async fn ui_type(&self, Parameters(p): Parameters<TypeParams>) -> Result<CallToolResult, McpError> {
        self.ui("ui.type", json!({"text": p.text})).await
    }

    #[tool(
        title = "Live app: screenshot",
        description = "Live app: screenshot of the window or one panel (PNG).",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn ui_screenshot(&self, Parameters(p): Parameters<ShotParams>) -> Result<CallToolResult, McpError> {
        match self.bridge_client() {
            Some(b) => self.screenshot(&b, p.panel, p.max_side.unwrap_or(1600)).await,
            None => Ok(fail("ui_screenshot needs bridge mode")),
        }
    }

    #[tool(
        title = "Live app: control-channel call",
        description = "Live app: call any control-channel method directly (ui.set, ui.scroll, ui.timeline.hit, ui.timeline.locate, ui.playback, ui.panel.show, ui.menu.list, …).",
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = false, open_world_hint = false)
    )]
    async fn ui_control(&self, Parameters(p): Parameters<ControlParams>) -> Result<CallToolResult, McpError> {
        self.ui(&p.method, p.params.unwrap_or(json!({}))).await
    }
}

impl FilmagicMcp {
    async fn screenshot(&self, b: &BridgeClient, panel: Option<String>, max: u32) -> Result<CallToolResult, McpError> {
        // The desktop bridge needs a pathname, so use a freshly-created,
        // owner-only directory rather than exposing image bytes at a
        // predictable shared-temp filename.
        let dir = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(e) => return Ok(fail(format!("screenshot: {e}"))),
        };
        let path = dir.path().join("screenshot.png");
        if let Err(e) = b.call("ui.screenshot", json!({"path": path.to_string_lossy(), "panel": panel})).await {
            return Ok(fail(e));
        }
        let bytes = match tokio::fs::read(&path).await {
            Ok(b) => b,
            Err(e) => return Ok(fail(format!("screenshot: {e}"))),
        };
        let bytes = match image::load_from_memory(&bytes) {
            Ok(img) => {
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                png_rgba(w, h, rgba.into_raw(), max).unwrap_or(bytes)
            }
            Err(_) => bytes,
        };
        Ok(png_result(&bytes, "screenshot of the live Filmagic window"))
    }
}

impl FilmagicMcp {
    /// The first argument `tool` doesn't take, as a message naming the ones it does.
    fn unknown_arg(&self, tool: &str, args: Option<&rmcp::model::JsonObject>) -> Option<String> {
        let def = self.tool_router.get(tool)?;
        let props = def.input_schema.get("properties").and_then(Value::as_object).cloned().unwrap_or_default();
        let bad = args?.keys().find(|k| !props.contains_key(*k))?;
        let accepted: Vec<&str> = props.keys().map(String::as_str).collect();
        let accepted = if accepted.is_empty() { "no arguments".to_string() } else { accepted.join(", ") };
        Some(format!("unknown argument \"{bad}\" for {tool}; expected: {accepted}"))
    }
}

const INSTRUCTIONS: &str = "Filmagic video editor (Premiere Pro-class). Every edit is an engine command: `command_list` to discover ids/params, `command_run` to execute (undoable; `command_batch` runs several). `doc_inspect` (or `project_inspect`/`sequence_inspect`; `doc_inspect` and `sequence_inspect` take `compact`/`clip`/`track`/`effect` filters) returns ids you can pass to commands; `render_preview` shows the result. `file.exportMedia` with `wait: true` is stopped, and its partial file deleted, when your client's tool-call timeout ends first: for a whole film, call it without `wait` and poll `jobs.list` until the job is `finished`. In bridge mode the `ui_*` tools drive the live app: `ui_elements` lists clickable ids, `ui_click`/`ui_drag`/`ui_key` operate it, `ui_screenshot` shows it. Time is in ticks: 254016000000 per second (commands also accept `seconds`, `frame` or `timecode`). `sequence.clips` lists the clips of the active sequence in brief (ids, times, what is unusual about each): start there, and use the inspections for detail. To judge an edit rather than only make one: `story_board` shows the film as one frame per shot (or a clip's rushes); `story.blinkPoints` finds where a clip could be cut (where a phrase or a movement completes); `story.rhythm` and `story.cuts` measure the pace and each cut (jump cuts, how far the eye travels, reversed screen direction, straight cuts on a pause) and leave emotion and story, which outweigh everything measured, to you; `story.splitEdit` makes an L- or J-cut; `story.shadowCut` and `story.compare` keep versions and say shot by shot what differs between two. To work on part of a picture: `layers.extract`, `layers.lift`, `layers.cleanPlate`, `layers.retime` and `layers.adjust` take a `region` in fractions of the clip's own picture, or, with `\"space\":\"frame\"`, of the programme frame as you see it rendered (use that when the clip is moved, zoomed or turned); `layers.transform` makes a layer larger or smaller, or shifts it, where it stands (its mask's feet stay put through the shot, which a Motion scale alone does not do); `color.relight` grades a shot toward a time of day. Commands that change the picture report `pictureChange` (`none`, `subtle`, `visible`, `strong`): if what you meant to be seen reads `subtle`, a viewer will not notice it. To check what you made: `story_board` with `fps` lays out a moment frame by frame (the reliable way to see motion yourself; `render_motion` makes a GIF or MP4 of a few seconds for people); `layout.check` finds graphics and captions that overlap, leave the safe areas or cover busy picture such as a burned-in score (`graphics.anchor` moves one into a corner); razors, speed changes and frame holds warn when they land near a cut inside the source file (`media.sourceCuts` lists those cuts). Read `warnings` and `notes` in every result. These work from the frames that exist: none of them generates new picture content.";

/// Resources: the project (as `doc_inspect`) and the command catalog (as `command_list`).
const DOCUMENT_URI: &str = "filmagic://document";
const COMMANDS_URI: &str = "filmagic://commands";

/// MCP 2026-07-28 clients (current Claude Code) require `ttlMs` and `cacheScope` on list and read
/// results; rmcp fills `resultType` and leaves the cache hints to the server, as the generated
/// `tools/list` does. The resource list never changes; the document does, so reads aren't cached.
fn cache_hints(context: &rmcp::service::RequestContext<rmcp::RoleServer>, ttl_ms: u64) -> Option<(u64, rmcp::model::CacheScope)> {
    context.protocol_version().is_some_and(|v| v >= rmcp::model::ProtocolVersion::V_2026_07_28).then_some((ttl_ms, rmcp::model::CacheScope::Private))
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for FilmagicMcp {
    fn get_info(&self) -> rmcp::model::ServerConfig {
        rmcp::model::ServerConfig::new(rmcp::model::ServerCapabilities::builder().enable_tools().enable_resources().build())
            .with_server_info(rmcp::model::Implementation::new("filmagic", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS.to_string())
    }

    /// Strict arguments and long exports in front of the tool router.
    async fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParams,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::CallToolResponse, McpError> {
        if let Some(m) = self.unknown_arg(&request.name, request.arguments.as_ref()) {
            return Err(McpError::invalid_params(m, None));
        }
        // A blocking export reports progress and can be cancelled (docs/agents.md § Long exports).
        // Bridge mode too: the app runs it as a job, so it stays responsive (#91, #92).
        if request.name == "command_run"
            && let Some(a) = &request.arguments
            && let Some(id) = a.get("id").and_then(Value::as_str)
        {
            let params = a.get("params").cloned().filter(Value::is_object).unwrap_or_else(|| json!({}));
            if crate::long_job::is_long(id, &params) {
                return Ok(self.run_long(id, params, &context).await.into());
            }
        }
        let tcc = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
        self.tool_router.call(tcc).await
    }

    async fn list_resources(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ListResourcesResult, McpError> {
        use rmcp::model::Resource;
        let mut r = rmcp::model::ListResourcesResult::with_all_items(vec![
            Resource::new(DOCUMENT_URI, "document")
                .with_title("Project")
                .with_description("The project tree and the active sequence (same as doc_inspect).")
                .with_mime_type("application/json"),
            Resource::new(COMMANDS_URI, "commands")
                .with_title("Command catalog")
                .with_description("Every command with id, label, menu path, shortcut, params and enabled state (same as command_list).")
                .with_mime_type("application/json"),
        ]);
        if let Some((ttl, scope)) = cache_hints(&context, 600_000) {
            r = r.with_ttl_ms(ttl).with_cache_scope(scope);
        }
        Ok(r)
    }

    async fn read_resource(
        &self,
        request: rmcp::model::ReadResourceRequestParams,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ReadResourceResponse, McpError> {
        let v = match request.uri.as_str() {
            DOCUMENT_URI => self.document().await,
            COMMANDS_URI => self.run("command.list", json!({})).await,
            other => return Err(McpError::resource_not_found(format!("resource not found: {other}"), None)),
        }
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let text = serde_json::to_string_pretty(&v).map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let mut r = rmcp::model::ReadResourceResult::new(vec![rmcp::model::ResourceContents::text(text, request.uri).with_mime_type("application/json")]);
        if let Some((ttl, scope)) = cache_hints(&context, 0) {
            r = r.with_ttl_ms(ttl).with_cache_scope(scope);
        }
        Ok(r.into())
    }
}

/// Render scale that fits the longest side of a `w` x `h` sequence into `max_side` (never upscales; 0 = no limit, as in `png_rgba`).
fn render_scale(max_side: u32, w: u32, h: u32) -> f32 {
    if max_side == 0 {
        return 1.0;
    }
    (max_side as f32 / w.max(h).max(1) as f32).min(1.0)
}

#[cfg(test)]
mod tests;
