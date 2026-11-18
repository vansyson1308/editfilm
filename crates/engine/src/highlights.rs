//! `story.highlights`: the best moments of long footage, found from what it sounds and looks like
//! (#21).
//!
//! Scene detection finds nothing in single-camera footage (a whole match is one shot), so this
//! scores a timeline clip, or a project item's media, over time from cues
//! ([`filmagic_render::highlights`]): impacts (a paddle hit, a bounce), cheering, motion, and,
//! as a heuristic, a scoreboard that changes. It returns ranked segments, each with the cues
//! that fired and a suggested In and Out that does not cut into a rally.
//!
//! The analysis runs as a job (`jobs.list` shows it, `jobs.cancel` stops it); `wait:true` answers
//! when it is done, else `story.highlights {"job": id}` reads the result. Cost is bounded: sound
//! is read decimated to 16 kHz, a few frames a second are sampled at 256 pixels wide, a call
//! analyses at most [`MAX_SPAN_SECONDS`], and the cues of the last few analyses are cached, so
//! asking again with another `mode`, `limit` or cadence reads no media.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use filmagic_project::{ClipId, ItemId, Label, Marker, MarkerId, MarkerKind, TrackItem, TrackKind};
use filmagic_render::highlights::{AUDIO_RATE, AudioAnalyser, Cues, Highlight, Mode, Options, PictureAnalyser, find_highlights, luma_grid};
use filmagic_time::{FrameRate, Tick};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, NamedKeys, bad};
use crate::{EngineError, Result, Session};

const CMD: &str = "story.highlights";
/// Longest stretch one call analyses (two hours).
pub const MAX_SPAN_SECONDS: f64 = 7200.0;
/// Analyses whose cues are kept.
const CACHE: usize = 8;
/// Finished results kept for `{"job": id}`.
const RESULTS: usize = 16;
/// Most segments one result lists, and impact times.
const MAX_LIMIT: u64 = 100;
const MAX_IMPACT_TIMES: usize = 2000;
/// Longest side of the frames analysed.
const ANALYSIS_SIDE: f32 = 256.0;
/// Seconds of sound read at a time.
const CHUNK_SECONDS: usize = 5;

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: CMD,
        label: "Find Highlights",
        menu: &[],
        shortcut: None,
        params: r#"{"clip":id|"item":id (a timeline clip and its linked sound, or a project item's media),"fromSeconds":f64?,"toSeconds":f64? (clip: sequence time; item: media time; at most 7200 s apart),"mode":"auto|rally|peaks"="auto","cadence":[f64,f64]=[0.3,2.5] (seconds between a rally's impacts),"minHits":n=3,"minSeconds":f64=2,"maxSeconds":f64=45,"limit":n=10,"picture":bool=true (motion and scoreboard; false reads only the sound and is much faster),"fps":n=4 (frames sampled a second, 1..15),"markers":bool=false (a marker for each segment),"wait":bool=false,"job":id? (the result of an earlier call that did not wait)}"#,
        enabled: crate::commands::always,
        run: highlights,
        journal: true,
        named: NamedKeys::NONE,
    }]
}

/// The session's highlight jobs, results and cue cache.
#[derive(Default)]
pub struct State {
    pending: Vec<Pending>,
    results: VecDeque<(u64, Value)>,
    cache: VecDeque<(u64, Arc<Cues>)>,
}

/// A running job: what to do with its cues when it finishes.
struct Pending {
    job: u64,
    key: u64,
    ask: Ask,
    cues: Arc<Mutex<Option<Cues>>>,
    /// `Session::document_epoch` when the job started: another project since, and the result
    /// (its markers, its clip and item ids) belongs to no document here.
    epoch: u64,
}

/// Everything about one call except the media: how to read the cues, where they sit, markers.
#[derive(Clone)]
struct Ask {
    opts: Options,
    limit: usize,
    markers: bool,
    target: Target,
    /// Start of the analysed span (sequence time for a clip, media time for an item).
    from: Tick,
    to: Tick,
    rate: FrameRate,
    picture: bool,
    sound: bool,
    notes: Vec<String>,
}

#[derive(Clone)]
enum Target {
    /// A timeline clip of `seq` (`item` maps sequence time to the clip's media time).
    Clip {
        seq: ItemId,
        clip: ClipId,
        item: Box<TrackItem>,
    },
    Item {
        item: ItemId,
    },
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn num(p: &Value, key: &str) -> Result<Option<f64>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_f64().filter(|x| x.is_finite()).map(Some).ok_or_else(|| bad(CMD, format!("`{key}` must be a number"))),
    }
}

fn flag(p: &Value, key: &str, default: bool) -> Result<bool> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(bad(CMD, format!("`{key}` must be true or false"))),
    }
}

fn whole(p: &Value, key: &str, lo: u64, hi: u64, default: u64) -> Result<u64> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v.as_u64().filter(|n| (lo..=hi).contains(n)).ok_or_else(|| bad(CMD, format!("`{key}` must be a whole number from {lo} to {hi}"))),
    }
}

fn options(p: &Value) -> Result<(Options, usize)> {
    let mode = match p.get("mode") {
        None | Some(Value::Null) => Mode::Auto,
        Some(Value::String(m)) if m == "auto" => Mode::Auto,
        Some(Value::String(m)) if m == "rally" => Mode::Rally,
        Some(Value::String(m)) if m == "peaks" => Mode::Peaks,
        Some(_) => return Err(bad(CMD, "`mode` must be \"auto\", \"rally\" or \"peaks\"")),
    };
    let mut o = Options { mode, ..Options::default() };
    if let Some(c) = p.get("cadence").filter(|v| !v.is_null()) {
        let pair = c.as_array().filter(|a| a.len() == 2).and_then(|a| Some((a[0].as_f64()?, a[1].as_f64()?)));
        let Some((a, b)) = pair.filter(|(a, b)| a.is_finite() && b.is_finite() && *a >= 0.05 && a < b && *b <= 30.0) else {
            return Err(bad(CMD, "`cadence` must be [min, max] seconds between impacts, 0.05 ≤ min < max ≤ 30"));
        };
        o.cadence = (a, b);
    }
    o.min_hits = whole(p, "minHits", 2, 1000, 3)? as usize;
    if let Some(v) = num(p, "minSeconds")? {
        o.min_len = v;
    }
    if let Some(v) = num(p, "maxSeconds")? {
        o.max_len = v;
    }
    if !(0.0..=600.0).contains(&o.min_len) || !(0.5..=3600.0).contains(&o.max_len) || o.min_len > o.max_len {
        return Err(bad(CMD, "`minSeconds` must be 0–600 and `maxSeconds` 0.5–3600, with minSeconds ≤ maxSeconds"));
    }
    let limit = whole(p, "limit", 1, MAX_LIMIT, 10)? as usize;
    Ok((o, limit))
}

fn seconds_bound(p: &Value, key: &str) -> Result<Option<Tick>> {
    match num(p, key)? {
        None => Ok(None),
        Some(x) if (0.0..=1e7).contains(&x) => Ok(Some(Tick::from_seconds_f64(x))),
        Some(_) => Err(bad(CMD, format!("`{key}` must be seconds from 0"))),
    }
}

pub(crate) fn span(p: &Value, lo: Tick, hi: Tick) -> Result<(Tick, Tick)> {
    let from = seconds_bound(p, "fromSeconds")?.map_or(lo, |t| t.clamp(lo, hi));
    let to = seconds_bound(p, "toSeconds")?.map_or(hi, |t| t.clamp(lo, hi));
    if to <= from {
        return Err(bad(CMD, "nothing to analyse between `fromSeconds` and `toSeconds`"));
    }
    let len = (to - from).seconds();
    if len > MAX_SPAN_SECONDS {
        return Err(bad(
            CMD,
            format!(
                "{len:.0} s is too long to analyse in one call; pass `fromSeconds` and `toSeconds` at most {MAX_SPAN_SECONDS:.0} s apart and call again for the rest"
            ),
        ));
    }
    Ok((from, to))
}

/// FNV-1a.
fn hash(parts: &[&[u8]]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in parts {
        for b in p.iter().chain(&[0xff]) {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

/// What identifies an item's media: its reference, and for a file its size and modification time.
fn media_stamp(s: &Session, item: ItemId) -> Vec<u8> {
    let Some((root, media, _)) = s.project.resolve_media(item) else { return Vec::new() };
    let mut out = serde_json::to_vec(&(root.0, &media.media)).unwrap_or_default();
    if let filmagic_project::MediaRef::File { path } = &media.media
        && let Ok(m) = std::fs::metadata(path)
    {
        out.extend(m.len().to_le_bytes());
        if let Ok(t) = m.modified().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).map_err(std::io::Error::other)) {
            out.extend(t.as_nanos().to_le_bytes());
        }
    }
    out
}

type Sound = Box<dyn Fn(i64, usize) -> Option<Vec<f32>> + Send>;
type Picture = Box<dyn Fn(Tick) -> Option<Vec<u8>> + Send>;

fn mono(ch: &[Vec<f32>], n: usize) -> Vec<f32> {
    let k = ch.len().max(1) as f32;
    (0..n).map(|i| ch.iter().map(|c| c.get(i).copied().unwrap_or(0.0)).sum::<f32>() / k).collect()
}

fn grid_of(f: &filmagic_frame::VideoFrame) -> Option<Vec<u8>> {
    let rgba = f.to_rgba8();
    luma_grid(&rgba, f.width as usize, f.height as usize)
}

fn frame_scale(src: &filmagic_media::SharedSource) -> f32 {
    src.info().video.as_ref().map_or(1.0, |v| (ANALYSIS_SIDE / v.width.max(v.height).max(1) as f32).min(1.0))
}

/// Readers of a clip's sound (its own, or its linked sound clips') and picture, in sequence time.
fn clip_readers(s: &Session, clip: ClipId, picture: bool) -> Result<(TrackItem, Option<Sound>, Option<Picture>, Vec<u8>, Vec<String>)> {
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (track, it) = q.find_item(clip).ok_or_else(|| bad(CMD, format!("no clip {} in the active sequence", clip.0)))?;
    let is_video = q.track(track).is_some_and(|t| t.kind == TrackKind::Video);
    let partners = |kind: TrackKind| -> Vec<TrackItem> {
        let Some(link) = it.link else { return Vec::new() };
        q.tracks(kind).iter().flat_map(|t| t.items.iter()).filter(|x| x.link == Some(link) && x.id != it.id).cloned().collect()
    };
    let (video, audio) =
        if is_video { (Some(it.clone()), partners(TrackKind::Audio)) } else { (partners(TrackKind::Video).into_iter().next(), vec![it.clone()]) };
    let audible: Vec<TrackItem> = audio.into_iter().filter(|a| a.enabled && s.source(a.item).is_some_and(|src| src.info().has_audio())).collect();
    let mut notes = Vec::new();
    if audible.iter().any(|a| a.start > it.start || a.end() < it.end()) {
        notes.push(
            "the clip's linked sound does not run its whole length (a split edit): impacts and cheering were looked for only where there is sound".to_string(),
        );
    }
    let mut stamp = serde_json::to_vec(&(&video, &audible)).unwrap_or_default();
    for a in &audible {
        stamp.extend(media_stamp(s, a.item));
    }
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let sound = (!audible.is_empty()).then(|| {
        let audible = audible.clone();
        Box::new(move |start: i64, frames: usize| {
            let mut sum: Option<Vec<f32>> = None;
            for a in &audible {
                let Some(ch) = filmagic_render::audio::clip_signal(a, start, frames, AUDIO_RATE, &provider) else { continue };
                let one = mono(&ch, frames);
                match &mut sum {
                    Some(acc) => acc.iter_mut().zip(one).for_each(|(x, y)| *x += y),
                    None => sum = Some(one),
                }
            }
            sum
        }) as Sound
    });
    let pic = match video.filter(|_| picture) {
        Some(v) => {
            stamp.extend(media_stamp(s, v.item));
            s.source(v.item).filter(|src| src.info().video.is_some()).map(|src| {
                let scale = frame_scale(&src);
                Box::new(move |t: Tick| {
                    if t < v.start || t >= v.end() {
                        return None;
                    }
                    let f = src.video_frame(filmagic_media::FrameRequest { time: v.source_time_at(t), scale }).ok()?;
                    grid_of(&f)
                }) as Picture
            })
        }
        None => None,
    };
    Ok((it.clone(), sound, pic, stamp, notes))
}

/// Readers of a project item's media, in media time.
fn item_readers(s: &Session, item: ItemId, picture: bool) -> Result<(Tick, Tick, FrameRate, Option<Sound>, Option<Picture>, Vec<u8>)> {
    let pi = s.project.item(item).ok_or_else(|| bad(CMD, format!("no project item {}", item.0)))?;
    let src = s.source(item).ok_or_else(|| bad(CMD, format!("`{}` has no media to analyse", pi.name)))?;
    let info = src.info().clone();
    let (lo, hi) = match s.project.resolve_media(item) {
        Some((_, _, Some(range))) => (range.start, range.end()),
        _ => (Tick::ZERO, crate::commands::full_duration(s, pi)),
    };
    let rate = info.video.as_ref().map_or(FrameRate::FPS_24, |v| v.frame_rate).sane();
    let sound = info.has_audio().then(|| {
        let src = src.clone();
        Box::new(move |start: i64, frames: usize| src.audio(start, frames, AUDIO_RATE).ok().map(|b| mono(&b.channels, frames))) as Sound
    });
    let pic = (picture && info.video.is_some()).then(|| {
        let scale = frame_scale(&src);
        Box::new(move |t: Tick| src.video_frame(filmagic_media::FrameRequest { time: t, scale }).ok().and_then(|f| grid_of(&f))) as Picture
    });
    Ok((lo, hi, rate, sound, pic, media_stamp(s, item)))
}

fn highlights(s: &mut Session, p: &Value) -> Result<Value> {
    if !p.is_object() && !p.is_null() {
        return Err(bad(CMD, "params must be an object"));
    }
    if let Some(j) = p.get("job").filter(|v| !v.is_null()) {
        let id = j.as_u64().ok_or_else(|| bad(CMD, "`job` must be a job id"))?;
        return job_result(s, id);
    }
    let (opts, limit) = options(p)?;
    let picture = flag(p, "picture", true)?;
    let markers = flag(p, "markers", false)?;
    let wait = flag(p, "wait", false)?;
    let fps = whole(p, "fps", 1, 15, 4)?;
    let clip = match p.get("clip") {
        None | Some(Value::Null) => None,
        Some(v) => Some(ClipId(v.as_u64().ok_or_else(|| bad(CMD, "`clip` must be a clip id"))?)),
    };
    let item = match p.get("item") {
        None | Some(Value::Null) => None,
        Some(v) => Some(ItemId(v.as_u64().ok_or_else(|| bad(CMD, "`item` must be a project item id"))?)),
    };
    let (target, from, to, rate, sound, pic, stamp, notes) = match (clip, item) {
        (Some(c), None) => {
            let seq = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
            let (it, sound, pic, stamp, notes) = clip_readers(s, c, picture)?;
            let (from, to) = span(p, it.start, it.end())?;
            (Target::Clip { seq, clip: c, item: Box::new(it) }, from, to, s.sequence_rate(), sound, pic, stamp, notes)
        }
        (None, Some(i)) => {
            let (lo, hi, rate, sound, pic, stamp) = item_readers(s, i, picture)?;
            let (from, to) = span(p, lo, hi)?;
            (Target::Item { item: i }, from, to, rate, sound, pic, stamp, Vec::new())
        }
        _ => return Err(bad(CMD, "need a timeline `clip` or a project `item` (one of them)")),
    };
    if sound.is_none() && pic.is_none() {
        return Err(bad(CMD, if picture { "there is neither sound nor picture to analyse" } else { "there is no sound to analyse (and `picture` is false)" }));
    }
    let ask = Ask { opts, limit, markers, target, from, to, rate, picture: pic.is_some(), sound: sound.is_some(), notes };
    let key = hash(&[&stamp, &from.0.to_le_bytes(), &to.0.to_le_bytes(), &[u8::from(pic.is_some()), fps as u8]]);
    // cached: no media is read
    if let Some(cues) = s.highlights.cache.iter().find(|c| c.0 == key).map(|c| c.1.clone()) {
        let mut out = report(&cues, &ask);
        out["cached"] = json!(true);
        if markers {
            out["markers"] = json!(add_markers(s, &cues, &ask)?);
        }
        return Ok(out);
    }
    let span_s = (to - from).seconds();
    let chunk = AUDIO_RATE as usize * CHUNK_SECONDS;
    let n_chunks = if sound.is_some() { (span_s * f64::from(AUDIO_RATE) / chunk as f64).ceil() as u64 } else { 0 };
    let step = Tick::from_seconds_f64(1.0 / fps as f64);
    let n_frames = if pic.is_some() { ((to - from).0 / step.0.max(1)).max(1) as u64 } else { 0 };
    let id = s.jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1;
    let job = crate::Job { id, label: "Find Highlights".into(), progress: Default::default(), result: Default::default() };
    job.progress.total.store(n_chunks + n_frames, std::sync::atomic::Ordering::Relaxed);
    let cues: Arc<Mutex<Option<Cues>>> = Arc::default();
    let (prog, res, out) = (job.progress.clone(), job.result.clone(), cues.clone());
    let a0 = from.to_units_floor(i64::from(AUDIO_RATE));
    let run = move || {
        use std::sync::atomic::Ordering;
        let t0 = web_time::Instant::now();
        let mut done = 0u64;
        let mut stopped = false;
        let mut audio = None;
        if let Some(sound) = &sound {
            let mut an = AudioAnalyser::new(span_s);
            let total = (span_s * f64::from(AUDIO_RATE)).ceil() as usize;
            let mut at = 0usize;
            let mut heard = false;
            while at < total {
                if prog.cancel.load(Ordering::Relaxed) {
                    stopped = true;
                    break;
                }
                let n = chunk.min(total - at);
                let x = sound(a0 + at as i64, n);
                heard |= x.is_some();
                an.push(&x.unwrap_or_else(|| vec![0.0; n]));
                at += n;
                done += 1;
                prog.done.store(done, Ordering::Relaxed);
                *lock(&prog.status) = format!("Listening: {:.0} of {:.0} s", at as f64 / f64::from(AUDIO_RATE), span_s);
            }
            audio = heard.then(|| an.finish());
        }
        let mut picture = None;
        if let Some(pic) = &pic
            && !stopped
        {
            let mut an = PictureAnalyser::new();
            let mut seen = false;
            for k in 0..n_frames {
                if prog.cancel.load(Ordering::Relaxed) {
                    stopped = true;
                    break;
                }
                let t = from + Tick(step.0.saturating_mul(k as i64));
                let g = pic(t);
                seen |= g.is_some();
                an.push((t - from).seconds(), g);
                done += 1;
                prog.done.store(done, Ordering::Relaxed);
                if k % 8 == 0 {
                    *lock(&prog.status) = format!("Watching: frame {} of {n_frames}", k + 1);
                }
            }
            picture = seen.then(|| an.finish());
        }
        let secs = t0.elapsed().as_secs_f64();
        let r = if stopped {
            *lock(&prog.status) = "Stopped: nothing was changed".into();
            Err("stopped".to_string())
        } else {
            *lock(&out) = Some(Cues { span: span_s, audio, picture });
            *lock(&prog.status) = format!("Analysed {span_s:.0} s in {secs:.1} s");
            Ok(filmagic_export::Report {
                path: String::new(),
                frames: done,
                seconds: secs,
                bytes: 0,
                render_fps: done as f64 / secs.max(1e-6),
                extra_files: Vec::new(),
                ..Default::default()
            })
        };
        *lock(&res) = Some(r);
        prog.finished.store(true, Ordering::Relaxed);
    };
    let run = crate::export_tools::guard_job(job.progress.clone(), job.result.clone(), run);
    s.jobs.push(job);
    s.highlights.pending.push(Pending { job: id, key, ask, cues, epoch: s.document_epoch });
    if wait || cfg!(target_arch = "wasm32") {
        run();
        poll(s);
        let r = job_result(s, id)?;
        if r.get("status").and_then(Value::as_str) == Some("failed") {
            return Err(EngineError::Other(r["error"].as_str().unwrap_or("the analysis failed").to_string()));
        }
        return Ok(r);
    }
    std::thread::Builder::new().name("filmagic-highlights".into()).spawn(run).map_err(|e| EngineError::Other(e.to_string()))?;
    Ok(json!({
        "job": id,
        "status": "running",
        "seconds": round3(span_s),
        "poll": "jobs.list",
        "cancel": "jobs.cancel",
        "result": format!("story.highlights {{\"job\": {id}}} once it has finished"),
        "resultCommand": {"command": CMD, "params": {"job": id}},
    }))
}

/// The result of job `id`: running, failed, or the segments.
fn job_result(s: &mut Session, id: u64) -> Result<Value> {
    poll(s);
    if let Some(r) = s.highlights.results.iter().find(|r| r.0 == id) {
        return Ok(r.1.clone());
    }
    if s.highlights.pending.iter().any(|p| p.job == id) {
        let j = s.jobs.iter().find(|j| j.id == id).map(crate::Job::to_json).unwrap_or(Value::Null);
        return Ok(json!({"job": id, "status": "running", "progress": j["progress"], "etaSeconds": j["etaSeconds"]}));
    }
    Err(bad(CMD, format!("no highlights job {id} (finished results are kept for the last {RESULTS} calls)")))
}

/// Keep the results of finished jobs, add their markers, and drop cancelled ones. Called once per
/// UI frame from [`Session::poll_persistence`] and after synchronous runs.
pub fn poll(s: &mut Session) {
    use std::sync::atomic::Ordering;
    let mut i = 0;
    while i < s.highlights.pending.len() {
        let id = s.highlights.pending[i].job;
        let job = s.jobs.iter().find(|j| j.id == id);
        let finished = job.is_none_or(|j| j.progress.finished.load(Ordering::Relaxed));
        if !finished {
            i += 1;
            continue;
        }
        let failure = job.and_then(|j| lock(&j.result).clone()).and_then(|r| r.err());
        // cancelled, even after its cues were stored: nothing is kept or marked
        let cancelled = job.is_some_and(|j| j.progress.cancel.load(Ordering::Relaxed));
        let pj = s.highlights.pending.remove(i);
        let replaced = pj.epoch != s.document_epoch;
        let taken = lock(&pj.cues).take().filter(|_| !cancelled && !replaced);
        let failure = if replaced {
            Some("the project was closed or replaced while the analysis ran: its result was dropped".to_string())
        } else if cancelled {
            Some("stopped".to_string())
        } else {
            failure
        };
        let value = match (taken, failure) {
            (Some(cues), _) => {
                let cues = Arc::new(cues);
                let mut out = report(&cues, &pj.ask);
                if pj.ask.markers {
                    match add_markers(s, &cues, &pj.ask) {
                        Ok(ids) => out["markers"] = json!(ids),
                        Err(e) => out["warnings"].as_array_mut().into_iter().for_each(|w| w.push(json!(format!("no markers were added: {e}")))),
                    }
                }
                s.highlights.cache.retain(|c| c.0 != pj.key);
                s.highlights.cache.push_back((pj.key, cues));
                while s.highlights.cache.len() > CACHE {
                    s.highlights.cache.pop_front();
                }
                out
            }
            (None, Some(e)) if e == "stopped" => json!({"job": id, "status": "cancelled", "segments": []}),
            (None, e) => json!({"job": id, "status": "failed", "error": e.unwrap_or_else(|| "the analysis ended without a result".into())}),
        };
        s.highlights.results.push_back((id, value));
        while s.highlights.results.len() > RESULTS {
            s.highlights.results.pop_front();
        }
    }
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// A time of the analysed span (seconds from its start) as ticks of the target's clock, on a frame.
fn at(ask: &Ask, secs: f64) -> Tick {
    let t = ask.from + Tick::from_seconds_f64(secs.max(0.0));
    ask.rate.snap(t.clamp(ask.from, ask.to))
}

fn time_json(ask: &Ask, secs: f64) -> Value {
    let t = at(ask, secs);
    json!({"seconds": round3(t.seconds()), "ticks": t.0, "frame": ask.rate.frame_at(t)})
}

fn segment_json(ask: &Ask, rank: usize, h: &Highlight) -> Value {
    let mut v = json!({
        "rank": rank,
        "score": round3(h.score),
        "kind": if h.rally { "rally" } else { "peak" },
        "start": time_json(ask, h.start),
        "end": time_json(ask, h.end),
        "seconds": round3(h.end - h.start),
        "suggestedIn": time_json(ask, h.suggested.0),
        "suggestedOut": time_json(ask, h.suggested.1),
        "cues": h.cues(),
        "impacts": h.impacts,
        "cadenceSeconds": h.cadence.map(round3),
        "cheerDb": h.cheer_db.map(|d| round3(f64::from(d))),
        "motion": h.motion.map(round3),
        "scoreboardChange": h.board,
        "speech": h.speech,
    });
    if let Target::Clip { item, .. } = &ask.target {
        let m = |s: f64| item.source_time_at(at(ask, s).clamp(item.start, (item.end() - Tick(1)).max(item.start))).seconds();
        v["media"] = json!({"start": round3(m(h.start)), "end": round3(m(h.end))});
    }
    v
}

fn report(cues: &Cues, ask: &Ask) -> Value {
    let found = find_highlights(cues, &ask.opts);
    let segments: Vec<Value> = found.iter().take(ask.limit).enumerate().map(|(i, h)| segment_json(ask, i + 1, h)).collect();
    let impacts: Vec<f64> =
        cues.audio.as_ref().map(|a| a.onsets.iter().filter(|o| o.impact).map(|o| round3(at(ask, o.t).seconds())).collect()).unwrap_or_default();
    let cheers: Vec<Value> = cues
        .audio
        .as_ref()
        .map(|a| {
            a.cheers
                .iter()
                .map(|c| json!({"start": round3(at(ask, c.0).seconds()), "end": round3(at(ask, c.1).seconds()), "riseDb": round3(f64::from(c.2))}))
                .collect()
        })
        .unwrap_or_default();
    let speech: Vec<Value> = cues
        .audio
        .as_ref()
        .map(|a| a.speech.iter().take(500).map(|g| json!({"start": round3(at(ask, g.0).seconds()), "end": round3(at(ask, g.1).seconds())})).collect())
        .unwrap_or_default();
    let board: Vec<Value> = cues
        .picture
        .as_ref()
        .map(|p| p.board.iter().map(|b| json!({"seconds": round3(at(ask, b.t).seconds()), "cells": b.cells})).collect())
        .unwrap_or_default();
    let mut warnings: Vec<String> = Vec::new();
    let mut notes = ask.notes.clone();
    if ask.sound && cues.audio.is_none() {
        warnings.push("the sound could not be read (offline media, or a stream that does not decode): no impact or cheering was looked for".into());
    }
    if !ask.sound {
        notes.push("there is no sound: only the picture was read, so impacts, rallies and cheering are unknown".into());
    }
    if ask.picture && cues.picture.is_none() {
        warnings.push("no frame of the picture could be read: no motion or scoreboard was looked for".into());
    }
    if !ask.picture {
        notes.push("the picture was not read (picture:false or no video): no motion or scoreboard cue".into());
    }
    if impacts.len() > MAX_IMPACT_TIMES {
        notes.push(format!("{} impacts were heard; impactTimes lists the first {MAX_IMPACT_TIMES}", impacts.len()));
    }
    if found.is_empty() {
        notes.push("no moment stood out: no run of impacts at the cadence, no cheering, no burst of motion. Try mode:\"peaks\", a wider `cadence`, or a lower `minHits`".into());
    }
    notes.push("cues are measurements, not judgement: impacts are short sharp sounds (a hit, a bounce, a clap or a door alike), cheering is loud noise-like sound, motion is how much of the picture changes, and the scoreboard cue is a heuristic".into());
    let target = match &ask.target {
        Target::Clip { clip, .. } => json!({"clip": clip.0}),
        Target::Item { item } => json!({"item": item.0}),
    };
    json!({
        "status": "done",
        "of": target,
        "timeBase": if matches!(ask.target, Target::Clip { .. }) { "sequence" } else { "media" },
        "span": {"from": round3(ask.from.seconds()), "to": round3(ask.to.seconds())},
        "analysed": {"sound": cues.audio.is_some(), "picture": cues.picture.is_some()},
        "segments": segments,
        "found": found.len(),
        "impacts": impacts.len(),
        "impactTimes": impacts.iter().take(MAX_IMPACT_TIMES).collect::<Vec<_>>(),
        "cheers": cheers,
        "speech": speech,
        "scoreboardChanges": board,
        "warnings": warnings,
        "notes": notes,
    })
}

/// Markers for the listed segments: sequence markers for a clip, clip markers on the media item
/// for an item (media time). One undo step.
fn add_markers(s: &mut Session, cues: &Cues, ask: &Ask) -> Result<Vec<u64>> {
    let found = find_highlights(cues, &ask.opts);
    let marks: Vec<(Tick, Tick, String, String)> = found
        .iter()
        .take(ask.limit)
        .enumerate()
        .map(|(i, h)| {
            let (a, b) = (at(ask, h.suggested.0), at(ask, h.suggested.1));
            (a, (b - a).max(Tick::ZERO), format!("Highlight {}", i + 1), format!("score {:.2}: {}", h.score, h.cues().join(", ")))
        })
        .collect();
    if marks.is_empty() {
        return Ok(Vec::new());
    }
    let new_marker = |id: u64, (start, duration, name, comment): (Tick, Tick, String, String)| Marker {
        url: String::new(),
        author: String::new(),
        custom_color: None,
        id: MarkerId(id),
        start,
        duration,
        name,
        comment,
        kind: MarkerKind::Comment,
        color: Label::Yellow,
    };
    match &ask.target {
        Target::Clip { seq, .. } => {
            let seq = *seq;
            s.edit("Mark Highlights", move |pr, _| {
                let ids: Vec<u64> = marks.iter().map(|_| pr.alloc_id()).collect();
                let q = pr.sequence_mut(seq).ok_or(EngineError::NoSequence)?;
                for (m, id) in marks.into_iter().zip(&ids) {
                    q.markers.push(new_marker(*id, m));
                }
                q.markers.sort_by_key(|m| m.start);
                Ok(ids)
            })
        }
        Target::Item { item } => {
            let root = s.project.resolve_media(*item).map(|(r, _, _)| r).ok_or_else(|| bad(CMD, "the item has no media to mark"))?;
            s.edit("Mark Highlights", move |pr, _| {
                let ids: Vec<u64> = marks.iter().map(|_| pr.alloc_id()).collect();
                let m = pr.item_mut(root).and_then(|i| i.as_media_mut()).ok_or_else(|| bad(CMD, "the item has no media to mark"))?;
                for (mk, id) in marks.into_iter().zip(&ids) {
                    m.markers.push(new_marker(*id, mk));
                }
                m.markers.sort_by_key(|x| x.start);
                Ok(ids)
            })
        }
    }
}
