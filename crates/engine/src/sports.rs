//! Sports tools (#21): Auto Reframe to a new aspect, and overlays that follow a tracked object
//! (a ball trail, a ground ring with an optional spotlight, a path line on a freeze frame).
//!
//! - `clip.autoReframe` analyses a clip (or every video clip of the sequence) at reduced size,
//!   frame-sampled, finds the subject (motion saliency, or a point / box the caller seeds and the
//!   template tracker follows), smooths a camera path over the whole range and writes it as Motion
//!   Position / Scale keyframes, on the clip or on the clips of a new sequence of the target
//!   aspect.
//! - `sports.trackPoint` follows a small object (a ball) or a box (a player) and answers its path
//!   in sequence pixels, for an agent to inspect or to hand to `sports.pathLine`.
//! - `sports.ballTrail`, `sports.groundRing` and `sports.pathLine` draw with graphic shape
//!   layers (rounded rectangles, an ellipse, a triangle) and an Opacity mask, keyframed per
//!   frame, so the result stays editable in Effect Controls like any graphic. They are code-drawn:
//!   no asset is added.
//!
//! With `points` given, the drawing commands draw at once. Every analysis (following an object,
//! reframing) is a job (`jobs.list` / `jobs.cancel`; `wait:true` runs it in the call). Its
//! result is applied as one undo step when it finishes, and its report (coverage, the times the
//! subject was lost or left the frame, `pictureChange`) is the reply with `wait:true` and the
//! job's `report` in `jobs.list` otherwise. All coordinates are pixels of the sequence frame (what
//! the Program Monitor and frame exports show).

mod overlay;
mod path;
mod reframe;
#[cfg(test)]
mod tests;
mod track;

use std::sync::{Arc, Mutex};

use filmagic_geom::{Affine, Vec2};
use filmagic_project::{ClipId, ItemId, Sequence, TrackItem, TrackKind};
use filmagic_time::Tick;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, NamedKeys, bad, has_seq};
use crate::{EngineError, Result, Session};

pub(crate) use track::Luma;

/// Most reports of finished jobs kept for `jobs.list`.
const MAX_REPORTS: usize = 32;
/// Most frames one tracking job follows (two minutes at 60 fps).
pub(crate) const MAX_TRACK_FRAMES: usize = 7200;

/// A running sports job; [`poll`] applies its result when it finishes.
pub struct PendingSports {
    pub job: u64,
    seq: ItemId,
    epoch: u64,
    request: Request,
    out: Arc<Mutex<Option<Analysis>>>,
}

pub(crate) enum Request {
    Reframe(reframe::Request),
    Overlay(overlay::Request),
}

pub(crate) enum Analysis {
    Reframe(Vec<reframe::ClipPlan>),
    Points(Vec<TrackedPoint>),
}

/// One tracked sample: sequence time, where the object is in sequence pixels (`None`: not found
/// in that frame) and the match score.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TrackedPoint {
    pub time: Tick,
    pub at: Option<Vec2>,
    pub score: f32,
}

pub(crate) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn spec(id: &'static str, label: &'static str, params: &'static str, run: fn(&mut Session, &Value) -> Result<Value>) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: has_seq, run, journal: true, named: NamedKeys::NONE }
}

/// The tracking parameters every overlay command shares.
macro_rules! track_doc {
    () => {
        r#""clip":id? (the video clip to follow the object in; default the selected video clip, else the topmost one at `at`),"point":[x,y]? (the object's centre at `at`, sequence pixels),"box":[x,y,w,h]? (or a box around it: top-left corner and size),"size":px=24 (with `point`: the object's size),"at":ticks? (when `point`/`box` is true; default the playhead),"start":ticks?,"end":ticks? (sequence time to follow it over; default from `at` to the clip's end, at most 10 s),"wait":bool=false"#
    };
}
pub(crate) use track_doc;

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        spec("clip.autoReframe", "Auto Reframe", reframe::PARAMS, reframe::run),
        spec(
            "sports.trackPoint",
            "Track Point",
            concat!("{", track_doc!(), "} → points [{time, seconds, x, y, found, score}] (sequence pixels), coverage, lost [{start, end}] (seconds)"),
            |s, p| overlay::start(s, p, overlay::Kind::Track, "sports.trackPoint"),
        ),
        spec("sports.ballTrail", "Ball Trail", overlay::TRAIL_PARAMS, |s, p| overlay::start(s, p, overlay::Kind::Trail, "sports.ballTrail")),
        spec("sports.groundRing", "Ground Ring", overlay::RING_PARAMS, |s, p| overlay::start(s, p, overlay::Kind::Ring, "sports.groundRing")),
        spec("sports.pathLine", "Path Line", overlay::PATH_PARAMS, |s, p| overlay::start(s, p, overlay::Kind::Path, "sports.pathLine")),
        CommandSpec {
            id: "sports.report",
            label: "Sports Job Report",
            menu: &[],
            shortcut: None,
            params: r#"{"job":id} → the report of a finished clip.autoReframe / sports.* job (status "done"), or {status: "running" | "cancelled" | "failed", error?}"#,
            enabled: crate::commands::always,
            run: report_cmd,
            journal: false,
            named: NamedKeys::NONE,
        },
    ]
}

/// The report of finished job `job`, for `jobs.list`.
pub(crate) fn report(s: &Session, job: u64) -> Option<&Value> {
    s.sports_reports.get(&job)
}

fn store_report(s: &mut Session, job: u64, v: Value) {
    s.sports_reports.insert(job, v);
    while s.sports_reports.len() > MAX_REPORTS {
        let Some(first) = s.sports_reports.keys().next().copied() else { break };
        s.sports_reports.remove(&first);
    }
}

/// Start `work` as a job: in this call with `wait` (and on the web, which has no threads), else on
/// a thread. The reply is the applied result with `wait`, else the job id and how to follow it.
pub(crate) fn launch(
    s: &mut Session,
    cmd: &str,
    label: String,
    total: u64,
    request: Request,
    wait: bool,
    work: impl FnOnce(&filmagic_export::Progress) -> std::result::Result<Analysis, String> + Send + 'static,
) -> Result<Value> {
    use std::sync::atomic::Ordering;
    let seq = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let id = s.next_job_id();
    let job = crate::Job { id, label: label.clone(), progress: Default::default(), result: Default::default() };
    job.progress.total.store(total, Ordering::Relaxed);
    let out: Arc<Mutex<Option<Analysis>>> = Arc::default();
    let (prog, res, slot) = (job.progress.clone(), job.result.clone(), out.clone());
    let run = move || {
        let t0 = web_time::Instant::now();
        let r = work(&prog);
        let secs = t0.elapsed().as_secs_f64();
        let done = prog.done.load(Ordering::Relaxed);
        let r = match r {
            Ok(a) => {
                *lock(&slot) = Some(a);
                *lock(&prog.status) = format!("Analysed {done} frame(s) in {secs:.1}s");
                Ok(filmagic_export::Report {
                    path: String::new(),
                    frames: done,
                    seconds: secs,
                    bytes: 0,
                    render_fps: done as f64 / secs.max(1e-6),
                    extra_files: Vec::new(),
                    ..Default::default()
                })
            }
            Err(e) => {
                *lock(&prog.status) = if e == STOPPED { "Stopped: nothing was changed".into() } else { e.clone() };
                Err(e)
            }
        };
        *lock(&res) = Some(r);
        prog.finished.store(true, Ordering::Relaxed);
    };
    let run = crate::export_tools::guard_job(job.progress.clone(), job.result.clone(), run);
    s.jobs.push(job);
    s.sports_jobs.push(PendingSports { job: id, seq, epoch: s.document_epoch, request, out });
    if wait || cfg!(target_arch = "wasm32") {
        run();
        poll(s);
        if let Some(Err(e)) = s.jobs.iter().find(|j| j.id == id).and_then(|j| lock(&j.result).clone()) {
            return Err(EngineError::Other(format!("{cmd}: {e}")));
        }
        let mut r = report(s, id).cloned().unwrap_or(Value::Null);
        if let Some(err) = r.get("error").and_then(Value::as_str) {
            return Err(EngineError::Other(format!("{cmd}: {err}")));
        }
        if r.is_object() {
            r["job"] = json!(id);
        }
        return Ok(r);
    }
    std::thread::Builder::new().name("filmagic-sports".into()).spawn(run).map_err(|e| EngineError::Other(e.to_string()))?;
    Ok(json!({
        "job": id,
        "frames": total,
        "poll": "jobs.list",
        "cancel": "jobs.cancel",
        // what a long-job caller (MCP, the bridge) reads once the job is done
        "resultCommand": {"command": "sports.report", "params": {"job": id}},
        "notes": [format!("{label} runs in the background: its result is applied when it finishes, and sports.report (or jobs.list) gives its report")],
    }))
}

/// `sports.report {job}`: a sports job's answer. Its report once it is applied (`status`
/// `"done"`), else `{status: "running" | "cancelled" | "failed", error?}`.
fn report_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    use std::sync::atomic::Ordering;
    let id = p.get("job").and_then(Value::as_u64).ok_or_else(|| bad("sports.report", "need `job`, the id a sports command answered"))?;
    if let Some(r) = report(s, id) {
        let mut r = r.clone();
        if let Some(e) = r.get("error").cloned() {
            return Ok(json!({"job": id, "status": "failed", "error": e}));
        }
        r["job"] = json!(id);
        r["status"] = json!("done");
        return Ok(r);
    }
    let j = s.jobs.iter().find(|j| j.id == id).ok_or_else(|| bad("sports.report", format!("no job {id}")))?;
    // still running, or finished and not applied yet (the next poll applies it)
    if !j.progress.finished.load(Ordering::Relaxed) || s.sports_jobs.iter().any(|pj| pj.job == id) {
        return Ok(json!({"job": id, "status": "running"}));
    }
    match lock(&j.result).clone() {
        Some(Err(e)) if e != STOPPED => Ok(json!({"job": id, "status": "failed", "error": e})),
        _ => Ok(json!({"job": id, "status": "cancelled", "error": "the job was stopped before it finished; nothing was changed"})),
    }
}

pub(crate) const STOPPED: &str = "stopped";

/// Apply finished jobs (one undo step each) and drop finished, cancelled or stale ones. Called
/// once per UI frame from [`Session::poll_persistence`], before each top-level command of a
/// headless session, and after synchronous runs.
pub fn poll(s: &mut Session) {
    use std::sync::atomic::Ordering;
    let mut i = 0;
    while i < s.sports_jobs.len() {
        let pj = &s.sports_jobs[i];
        let job = s.jobs.iter().find(|j| j.id == pj.job);
        if pj.epoch != s.document_epoch {
            if let Some(j) = job {
                j.progress.cancel.store(true, Ordering::Relaxed);
            }
            s.sports_jobs.remove(i);
            continue;
        }
        let finished = job.is_none_or(|j| j.progress.finished.load(Ordering::Relaxed));
        if !finished {
            i += 1;
            continue;
        }
        let cancelled = job.is_some_and(|j| j.progress.cancel.load(Ordering::Relaxed));
        let pj = s.sports_jobs.remove(i);
        let Some(analysis) = lock(&pj.out).take().filter(|_| !cancelled) else { continue };
        let r = match (pj.request, analysis) {
            (Request::Reframe(req), Analysis::Reframe(plans)) => reframe::apply(s, pj.seq, req, plans),
            (Request::Overlay(req), Analysis::Points(pts)) => overlay::apply(s, pj.seq, req, pts),
            _ => Err(EngineError::Other("internal: the job's result does not match its request".into())),
        };
        let report = match r {
            Ok(v) => v,
            Err(e) => {
                s.error_toast("sports", e.to_string());
                json!({"error": e.to_string()})
            }
        };
        store_report(s, pj.job, report);
    }
}

// ---------------------------------------------------------------- shared helpers

/// A clip as the analysis sees it: what maps its source pixels to the sequence frame.
#[derive(Clone)]
pub(crate) struct ClipView {
    /// The sequence's settings, in a [`Sequence`] without clips ([`frame_of`]), shared by every
    /// clip of a command.
    seq: Arc<Sequence>,
    pub item: TrackItem,
    pub src: (u32, u32),
    par: Option<(u32, u32)>,
}

/// The frame `q` places clips in: a copy of `q` without its clips (only the settings are read),
/// made once per command.
pub(crate) fn frame_of(q: &Sequence) -> Arc<Sequence> {
    let mut seq = q.clone();
    for tr in seq.video_tracks.iter_mut().chain(seq.audio_tracks.iter_mut()).chain(seq.submix_tracks.iter_mut()) {
        tr.items.clear();
        tr.transitions.clear();
    }
    seq.caption_tracks.clear();
    seq.markers.clear();
    Arc::new(seq)
}

impl ClipView {
    pub(crate) fn new(s: &Session, frame: &Arc<Sequence>, it: &TrackItem) -> Option<ClipView> {
        let src = s.project.source_size(it.item)?;
        if src.0 == 0 || src.1 == 0 {
            return None;
        }
        Some(ClipView { seq: frame.clone(), item: it.clone(), src, par: s.project.source_par(it.item) })
    }

    /// The same clip in a sequence of `w`×`h` (Auto Reframe's new sequence).
    pub(crate) fn resized(&self, w: u32, h: u32) -> ClipView {
        let mut v = self.clone();
        let seq = Arc::make_mut(&mut v.seq);
        seq.settings.width = w;
        seq.settings.height = h;
        v
    }

    pub(crate) fn seq_size(&self) -> (f64, f64) {
        (f64::from(self.seq.settings.width), f64::from(self.seq.settings.height))
    }

    /// Pixel aspect ratios (source, sequence) as width/height factors.
    pub(crate) fn pars(&self) -> (f64, f64) {
        let seq_par = filmagic_project::sane_par(self.seq.settings.par);
        let src_par = self.par.map_or(seq_par, filmagic_project::sane_par);
        (filmagic_project::par_ratio(src_par), filmagic_project::par_ratio(seq_par))
    }

    fn matrix(&self, t: Tick) -> Affine {
        filmagic_render::motion_matrix(&self.seq, &self.item, self.src, self.par, self.item.effect_time_at(t))
    }

    /// Source pixels at sequence time `t` → sequence pixels.
    pub(crate) fn to_seq(&self, t: Tick, p: Vec2) -> Vec2 {
        self.matrix(t).apply(p)
    }

    /// Sequence pixels at sequence time `t` → source pixels (`None` when Motion squashes the clip
    /// to nothing).
    pub(crate) fn to_src(&self, t: Tick, p: Vec2) -> Option<Vec2> {
        let q = self.matrix(t).inverse()?.apply(p);
        (q.x.is_finite() && q.y.is_finite()).then_some(q)
    }

    /// Source pixels per sequence pixel at `t` (for sizes).
    pub(crate) fn src_per_seq(&self, t: Tick) -> f64 {
        let m = self.matrix(t);
        let det = (m.a * m.d - m.b * m.c).abs();
        if det.is_finite() && det > 1e-12 { 1.0 / det.sqrt() } else { 1.0 }
    }
}

/// The video clip a command acts on: `clip`, else the first selected video clip with media, else
/// (with `at`) the topmost video clip with media there.
pub(crate) fn target_clip(s: &Session, p: &Value, cmd: &str, at: Option<Tick>) -> Result<TrackItem> {
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let video = |c: ClipId| q.find_item(c).filter(|(t, _)| q.track(*t).is_some_and(|t| t.kind == TrackKind::Video)).map(|(_, it)| it.clone());
    if let Some(v) = p.get("clip").filter(|v| !v.is_null()) {
        let id = v.as_u64().ok_or_else(|| bad(cmd, format!("`clip` must be a clip id, got {v}")))?;
        return video(ClipId(id)).ok_or_else(|| bad(cmd, format!("no video clip {id} in the active sequence")));
    }
    // a graphic the last overlay made (and selected) has nothing to follow
    if let Some(it) = s.state.selection.iter().filter_map(|c| video(*c)).find(|it| s.source(it.item).is_some()) {
        return Ok(it);
    }
    if let Some(t) = at
        && let Some(it) = q.video_tracks.iter().rev().filter(|tr| tr.enabled).find_map(|tr| tr.item_at(t).filter(|i| s.source(i.item).is_some()))
    {
        return Ok(it.clone());
    }
    Err(bad(cmd, "name the video clip with `clip`, or select it"))
}

/// A finite number parameter within `lo..=hi`; absent is `default`; anything else is an error.
pub(crate) fn num(p: &Value, key: &str, cmd: &str, default: f64, lo: f64, hi: f64) -> Result<f64> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => {
            let x = v.as_f64().filter(|x| x.is_finite()).ok_or_else(|| bad(cmd, format!("`{key}` must be a number, got {v}")))?;
            if x < lo || x > hi {
                return Err(bad(cmd, format!("`{key}` must be between {lo} and {hi}, got {x}")));
            }
            Ok(x)
        }
    }
}

/// `[x, y]` of finite numbers.
pub(crate) fn point(v: &Value) -> Option<Vec2> {
    let a = v.as_array()?;
    if a.len() != 2 {
        return None;
    }
    let (x, y) = (a.first()?.as_f64()?, a.get(1)?.as_f64()?);
    (x.is_finite() && y.is_finite() && x.abs() < 1e7 && y.abs() < 1e7).then(|| Vec2::new(x, y))
}

/// A sequence-time parameter (already normalized to ticks), checked not negative.
pub(crate) fn tick_param(p: &Value, key: &str, cmd: &str) -> Result<Option<Tick>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            let t = v.as_i64().ok_or_else(|| bad(cmd, format!("`{key}` must be a time (ticks or {{\"seconds\": s}}), got {v}")))?;
            if t < 0 {
                return Err(bad(cmd, format!("`{key}` must not be negative")));
            }
            Ok(Some(Tick(t)))
        }
    }
}

/// A colour (`"#RRGGBB"`, `"#RRGGBBAA"` or `[r, g, b, a?]` 0..1).
pub(crate) fn color(p: &Value, key: &str, cmd: &str, default: [f32; 4]) -> Result<[f32; 4]> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => match crate::commands::json_to_param(&filmagic_project::ParamValue::Color(default), v) {
            Some(filmagic_project::ParamValue::Color(c)) if c.iter().all(|x| x.is_finite()) => Ok(c.map(|x| x.clamp(0.0, 1.0))),
            _ => Err(bad(cmd, format!("`{key}` must be a colour like \"#FFD020\" or [1, 0.8, 0.1], got {v}"))),
        },
    }
}

/// `wait`, checked to be a boolean.
pub(crate) fn wait_p(p: &Value, cmd: &str) -> Result<bool> {
    match p.get("wait") {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(v) => Err(bad(cmd, format!("`wait` must be true or false, got {v}"))),
    }
}

/// Spans of consecutive samples for which `flag` holds, as `{start, end}` in seconds (sequence
/// time; `end` is the last such sample plus one sample step).
pub(crate) fn spans(times: &[Tick], step: Tick, flag: impl Fn(usize) -> bool) -> Vec<Value> {
    let mut out = Vec::new();
    let mut open: Option<usize> = None;
    for i in 0..=times.len() {
        let on = i < times.len() && flag(i);
        match (on, open) {
            (true, None) => open = Some(i),
            (false, Some(a)) => {
                let (Some(t0), Some(t1)) = (times.get(a), times.get(i - 1)) else { break };
                out.push(json!({"start": t0.seconds(), "end": (*t1 + step).seconds()}));
                open = None;
            }
            _ => {}
        }
        if out.len() >= 200 {
            break;
        }
    }
    out
}

/// Frames of `src` at `times` (media time) as grey analysis frames `scale` of full size, with the
/// factor from source pixels to analysis pixels. `None` for a frame that cannot be decoded.
pub(crate) fn luma_frame(src: &filmagic_media::SharedSource, time: Tick, scale: f32, src_w: u32) -> Option<(Luma, f64)> {
    let f = src.video_frame(filmagic_media::FrameRequest { time, scale }).ok()?;
    let (w, h) = (f.width as usize, f.height as usize);
    let l = Luma::from_rgba8(w, h, &f.to_rgba8())?;
    Some((l, f64::from(f.width) / f64::from(src_w.max(1))))
}

/// The media time each sequence time in `times` shows on `it`: through its frame hold, else
/// through its Time Remapping curve with one walk along it (each step integrates only from the
/// last keyframe passed), else its linear media time. Worked out inside the analysis job: on a
/// long ramp this is a lot of integrating, which must not hold up the thread that called.
/// Keyframes are written at the other clock, [`TrackItem::effect_time_at`] (linear media time).
pub(crate) fn shown_times(it: &TrackItem, times: &[Tick]) -> Vec<Tick> {
    if let Some(h) = it.frame_hold {
        return vec![h; times.len()];
    }
    match it.remap() {
        Some(r) => {
            let mut w = r.walk();
            times.iter().map(|t| w.shown(it.linear_source_time_at(*t))).collect()
        }
        None => times.iter().map(|t| it.linear_source_time_at(*t)).collect(),
    }
}

/// Where an object is when tracking starts: its centre and half-size, in source pixels.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Seed {
    pub at: Vec2,
    pub half: (f64, f64),
}

/// Follow the object `obj` seeded at sample `seed` through the frames at media times `mts`,
/// analysed `width` pixels wide, forward and backward from the seed. Answers the object's
/// source-pixel position per sample (`None` where it was not found) and its score.
pub(crate) fn follow(
    src: &filmagic_media::SharedSource,
    src_w: u32,
    mts: &[Tick],
    seed: usize,
    obj: Seed,
    width: f64,
    prog: &filmagic_export::Progress,
) -> std::result::Result<Vec<(Option<Vec2>, f32)>, String> {
    let Seed { at, half } = obj;
    use std::sync::atomic::Ordering;
    let scale = (width / f64::from(src_w.max(1))).min(1.0) as f32;
    let mut out = vec![(None, 0.0f32); mts.len()];
    let seed_t = *mts.get(seed).ok_or("internal: the seed is outside the range")?;
    let (first, k) = luma_frame(src, seed_t, scale, src_w).ok_or("can't decode the clip's frame at `at`")?;
    let half_a = ((half.0 * k).round().max(2.0) as i64, (half.1 * k).round().max(2.0) as i64);
    let make = || track::PointTracker::new(&first, at * k, half_a);
    if make().is_none() {
        return Err("there is nothing to follow at `point`/`box`: the area is plain. Give a box around the object, or a larger `size`".into());
    }
    out[seed] = (Some(at), 1.0);
    prog.done.fetch_add(1, Ordering::Relaxed);
    for dir in [1isize, -1] {
        let Some(mut tracker) = make() else { break };
        let mut i = seed as isize + dir;
        while i >= 0 && (i as usize) < mts.len() {
            if prog.cancel.load(Ordering::Relaxed) {
                return Err(STOPPED.into());
            }
            let idx = i as usize;
            if let Some((img, k)) = luma_frame(src, mts[idx], scale, src_w)
                && let Some((p, score)) = tracker.step(&img)
            {
                out[idx] = (Some(p * (1.0 / k)), score);
            }
            let done = prog.done.fetch_add(1, Ordering::Relaxed) + 1;
            *lock(&prog.status) = format!("Frame {done} of {}", prog.total.load(Ordering::Relaxed));
            i += dir;
        }
    }
    Ok(out)
}
