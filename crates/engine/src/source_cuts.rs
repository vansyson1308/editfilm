//! Cuts hidden inside a source file (#16).
//!
//! A clip of a long recording, a compilation or a screen capture can hold shot changes of its
//! own. An edit that does not know about them can slow down two shots at once, freeze a frame of
//! the wrong shot or razor two frames away from a cut that was already there. This module finds
//! those cuts with the same picture measure as Scene Edit Detection ([`filmagic_render::scene`],
//! default sensitivity, look-alike cuts included), per project item.
//!
//! Finding them means decoding every frame, which for long-GOP H.264 costs tens of milliseconds a
//! frame. So decoding never happens on the caller's thread unless the caller asks to wait:
//!
//! - **`media.sourceCuts {item|clip, fromSeconds?, toSeconds?, wait=false}`** reports the cuts
//!   already found and, for what is not checked yet, starts a background job (`jobs.list`,
//!   `jobs.cancel`) of at most [`MAX_JOB_FRAMES`] frames; `wait: true` runs it before answering;
//! - after **`timeline.razor`, `sequence.addEdit`, `sequence.addEditAllTracks`,
//!   `clip.speedDuration`, `timeline.rateStretch`, `clip.frameHold`, `clip.frameHoldOptions`,
//!   `clip.insertFrameHoldSegment`** and any `effects.*` command that retimes a clip (Time
//!   Remapping), [`annotate`] warns from what is already known when the edit lands within
//!   [`NEAR_FRAMES`] frames of a source cut, or when the retimed range straddles one. Where the
//!   range has not been checked yet it says so in `notes` and queues the check in the background:
//!   the next edit or `media.sourceCuts` reports it. An edit never waits for a decode;
//! - item boards (`story.board`) mark the cuts already known, and queue the rest.
//!
//! Frame differences are cached per media item ([`Cache`]), keyed by what the media is (its
//! reference, identity, duration and size): a relinked or refreshed file starts afresh.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use filmagic_project::{ClipId, ItemId, Project, TrackItem};
use filmagic_render::scene::{SceneOptions, Signature, cuts_with_look_alikes, signature};
use filmagic_time::{FrameRate, Tick};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, NamedKeys, always, bad, bool_p, clip_p, item_p};
use crate::{EngineError, Result, Session};

/// An edit this many media frames or fewer from a source cut is "near" it.
pub const NEAR_FRAMES: i64 = 3;
/// Frames one background job decodes at most (4 minutes of 30 fps media).
pub const MAX_JOB_FRAMES: i64 = 4 * 60 * 30;
/// Frame differences kept for all items together; past it the cache starts afresh.
const MAX_CACHED_FRAMES: usize = 2_000_000;
/// Frames looked at on each side of a range: the detector compares a step with its neighbours.
const CONTEXT: i64 = 6;
/// Longest side of the frames compared (the measure needs no detail).
const SIDE: f64 = 96.0;
/// Frames decoded between two writes to the shared cache.
const BATCH: usize = 24;

/// Commands after which an edit point is checked against the source's cuts.
const RAZORS: [&str; 3] = ["timeline.razor", "sequence.addEdit", "sequence.addEditAllTracks"];
/// Commands after which a retimed or held clip is checked.
const RETIMES: [&str; 5] = ["clip.speedDuration", "timeline.rateStretch", "clip.frameHold", "clip.frameHoldOptions", "clip.insertFrameHoldSegment"];

/// Whether [`annotate`] looks at the result of `id`.
pub(crate) fn watches(id: &str) -> bool {
    RAZORS.contains(&id) || RETIMES.contains(&id) || id.starts_with("effects.")
}

/// Frame differences of one media item.
struct Entry {
    /// What the item's media was when it was analysed (a relink or refresh starts afresh).
    key: String,
    rate: FrameRate,
    /// `diffs[f]`: the difference between frames `f - 1` and `f` (0 for frame 0).
    diffs: BTreeMap<i64, f32>,
}

/// What is waiting to be checked in one item's media: merged ranges of frames, in order.
struct Request {
    root: ItemId,
    key: String,
    rate: FrameRate,
    src: filmagic_media::SharedSource,
    ranges: Vec<(i64, i64)>,
}

/// The one background worker of a session, while it runs.
struct Worker {
    job: u64,
    progress: Arc<filmagic_export::Progress>,
}

#[derive(Default)]
struct Inner {
    media: HashMap<ItemId, Entry>,
    /// Work for the worker, one request per media item.
    queue: Vec<Request>,
    worker: Option<Worker>,
    /// Jobs this cache started, to prune from the session's list once finished.
    jobs: Vec<u64>,
    /// Worker threads running now, and the most ever running at once.
    live: usize,
    most_live: usize,
}

/// Source-cut analysis per media item (session state, not saved), shared with the one background
/// worker that fills it.
#[derive(Clone, Default)]
pub struct Cache {
    inner: Arc<Mutex<Inner>>,
}

impl Cache {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    /// Forget what was found in `item`'s media (its file changed).
    pub fn forget(&self, item: ItemId) {
        let mut inner = self.lock();
        inner.media.remove(&item);
        inner.queue.retain(|r| r.root != item);
    }
    /// Whether job `id` is a cut finder: background work, shown only when nothing else runs.
    pub fn is_background_job(&self, id: u64) -> bool {
        self.lock().jobs.contains(&id)
    }
    /// The most worker threads that ever ran at once (one, by design).
    pub fn most_workers(&self) -> usize {
        self.lock().most_live
    }
}

/// Ranges `(a, b)` kept sorted, with an overlapping or touching range merged in.
fn merge_range(ranges: &mut Vec<(i64, i64)>, a: i64, b: i64) {
    let (mut lo, mut hi) = (a, b);
    ranges.retain(|&(x, y)| {
        let apart = y.saturating_add(1) < lo || x > hi.saturating_add(1);
        if !apart {
            lo = lo.min(x);
            hi = hi.max(y);
        }
        apart
    });
    let at = ranges.partition_point(|r| r.0 < lo);
    ranges.insert(at, (lo, hi));
}

/// Frames waiting in the queue, all items together.
fn queued_frames(inner: &Inner) -> i64 {
    inner.queue.iter().flat_map(|r| r.ranges.iter()).map(|(a, b)| b - a + 1).sum()
}

/// Remove the session's finished cut-finder jobs from `jobs.list` and the status bar.
pub(crate) fn prune_jobs(s: &mut Session) {
    let finished: Vec<u64> = {
        let inner = s.source_cuts.lock();
        inner.jobs.iter().copied().filter(|id| s.jobs.iter().find(|j| j.id == *id).is_none_or(|j| j.progress.finished.load(Ordering::Relaxed))).collect()
    };
    if finished.is_empty() {
        return;
    }
    s.jobs.retain(|j| !finished.contains(&j.id));
    s.source_cuts.lock().jobs.retain(|id| !finished.contains(id));
}

/// A new project: stop the worker, forget the cuts and the jobs (the old project's item ids mean
/// nothing in the new one). The worker may still finish its frame: it writes into the old cache,
/// which nothing reads.
pub(crate) fn reset(s: &mut Session) {
    let ids: Vec<u64> = {
        let inner = s.source_cuts.lock();
        if let Some(w) = &inner.worker {
            w.progress.cancel.store(true, Ordering::Relaxed);
        }
        inner.jobs.clone()
    };
    s.jobs.retain(|j| !ids.contains(&j.id));
    s.source_cuts = Cache::default();
}

/// What a check found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Found {
    /// Media frames that start a new shot, in order.
    pub cuts: Vec<i64>,
    /// Frames `lo..=hi` actually checked (from the requested start).
    pub checked: Option<(i64, i64)>,
    /// The whole requested range was checked.
    pub complete: bool,
    pub rate: FrameRate,
}

/// The media an item shows.
struct Media {
    root: ItemId,
    rate: FrameRate,
    frames: i64,
    key: String,
}

/// The media item a project item shows (a subclip's parent) and its frame rate and frame count;
/// `None` for an item without a moving picture.
fn media_of(p: &Project, item: ItemId) -> Option<Media> {
    let (root, m, _) = p.resolve_media(item)?;
    let v = m.info.video.as_ref()?;
    if matches!(m.info.kind, filmagic_media::MediaKind::Still | filmagic_media::MediaKind::AudioOnly) || v.width == 0 {
        return None;
    }
    let rate = m.frame_rate().sane();
    let frames = rate.frame_at(m.info.duration).max(0);
    let key = format!("{:?}|{:?}|{}|{:?}", m.media, m.identity, m.info.duration.0, m.info.file_size);
    Some(Media { root, rate, frames, key })
}

/// The last frame from `lo` (in order, up to `hi`) whose difference is known.
fn known_through(entry: &Entry, lo: i64, hi: i64) -> Option<i64> {
    let mut last = None;
    for (&f, _) in entry.diffs.range(lo..=hi) {
        let expected = last.map_or(lo, |l: i64| l + 1);
        if f != expected {
            break;
        }
        last = Some(f);
    }
    last
}

/// The cuts among the known differences of `entry` that start frames `lo..=hi`.
fn cuts_known(entry: &Entry, lo: i64, hi: i64) -> Vec<i64> {
    let opts = SceneOptions::default();
    let mut out = Vec::new();
    // contiguous runs of known differences around the range
    let mut run: Vec<(i64, f32)> = Vec::new();
    let mut flush = |run: &mut Vec<(i64, f32)>| {
        if let Some(&(first, _)) = run.first() {
            // index 0 stands for the frame before the run, so the run's first step can be a cut
            let diffs: Vec<f32> = std::iter::once(0.0).chain(run.iter().map(|r| r.1)).collect();
            for i in cuts_with_look_alikes(&diffs, &opts) {
                let f = first + i as i64 - 1;
                if (lo..=hi).contains(&f) && f > 0 {
                    out.push(f);
                }
            }
        }
        run.clear();
    };
    for (&f, &d) in entry.diffs.range(lo.saturating_sub(CONTEXT)..=hi.saturating_add(CONTEXT)) {
        if run.last().is_some_and(|r| r.0 + 1 != f) {
            flush(&mut run);
        }
        run.push((f, d));
    }
    flush(&mut run);
    out.sort_unstable();
    out.dedup();
    out
}

/// The frames to look at for `lo..=hi`: the range clamped to the media, and with its context.
fn spans(m: &Media, lo: i64, hi: i64) -> Option<((i64, i64), (i64, i64))> {
    let last = m.frames.checked_sub(1)?;
    let (lo, hi) = (lo.clamp(0, last), hi.clamp(0, last));
    (hi >= lo).then(|| ((lo, hi), (lo.saturating_sub(CONTEXT).max(0), hi.saturating_add(CONTEXT).min(last))))
}

/// What is already known of the cuts that start media frames `lo..=hi` of `item`'s media. Never
/// decodes. `None` for an item without a moving picture.
pub fn cached(s: &Session, item: ItemId, lo: i64, hi: i64) -> Option<Found> {
    let m = media_of(&s.project, item)?;
    let Some(((lo, hi), (a, b))) = spans(&m, lo, hi) else { return Some(Found { complete: true, rate: m.rate, ..Default::default() }) };
    let inner = s.source_cuts.lock();
    let mut found = Found { rate: m.rate, ..Default::default() };
    let Some(entry) = inner.media.get(&m.root).filter(|e| e.key == m.key && e.rate == m.rate) else { return Some(found) };
    let known = known_through(entry, a, b);
    // the range is checked up to where the context after it is known too
    let through = known.map(|k| if k >= b { hi } else { k.saturating_sub(CONTEXT) });
    found.checked = through.filter(|t| *t >= lo).map(|t| (lo, t.min(hi)));
    found.complete = known.is_some_and(|k| k >= b);
    if let Some((x, y)) = found.checked {
        found.cuts = cuts_known(entry, x, y);
    }
    Some(found)
}

/// The cache entry a check fills: its item, media key and frame rate.
struct Target {
    cache: Cache,
    root: ItemId,
    key: String,
    rate: FrameRate,
}

/// Frames the worker takes from the queue at a time (new requests get in between).
const CHUNK: i64 = 240;
/// Most frames waiting in the queue; past it a request is refused with a note.
pub const MAX_QUEUED_FRAMES: i64 = 4 * MAX_JOB_FRAMES;

/// Decode what is missing of frames `a..=b` of a media item into the shared cache, at most
/// [`MAX_JOB_FRAMES`] frames, stopping when `progress` is cancelled; adds what it decodes to
/// `progress.done`.
fn fill(t: &Target, src: &dyn filmagic_media::MediaSource, a: i64, b: i64, progress: &filmagic_export::Progress) -> std::result::Result<u64, String> {
    let (cache, root, key, rate) = (&t.cache, t.root, t.key.as_str(), t.rate);
    let scale = src.info().video.as_ref().map_or(1.0, |v| (SIDE / f64::from(v.width.max(v.height).max(1))).min(1.0)) as f32;
    let known: Vec<i64> = {
        let mut inner = cache.lock();
        if inner.media.values().map(|e| e.diffs.len()).sum::<usize>() > MAX_CACHED_FRAMES {
            inner.media.clear();
        }
        let entry = inner.media.entry(root).or_insert_with(|| Entry { key: key.to_string(), rate, diffs: BTreeMap::new() });
        if entry.key != key || entry.rate != rate {
            *entry = Entry { key: key.to_string(), rate, diffs: BTreeMap::new() };
        }
        entry.diffs.range(a..=b).map(|(f, _)| *f).collect()
    };
    let missing: Vec<i64> = (a..=b).filter(|f| known.binary_search(f).is_err()).take(MAX_JOB_FRAMES as usize).collect();
    let mut prev: Option<(i64, Signature)> = None;
    let mut batch: Vec<(i64, f32)> = Vec::with_capacity(BATCH);
    let mut done = 0u64;
    let flush = |batch: &mut Vec<(i64, f32)>| {
        let mut inner = cache.lock();
        if let Some(e) = inner.media.get_mut(&root).filter(|e| e.key == key) {
            e.diffs.extend(batch.drain(..));
        }
    };
    for &f in &missing {
        if progress.cancel.load(Ordering::Relaxed) {
            flush(&mut batch);
            return Err("stopped".into());
        }
        // a run of missing frames starts here: its first difference needs the frame before it
        let before = match prev.take() {
            Some((pf, sig)) if pf + 1 == f => Some(sig),
            _ if f > 0 => Some(decode(src, rate, f - 1, scale)?),
            _ => None,
        };
        let sig = decode(src, rate, f, scale)?;
        let d = before.as_ref().map_or(0.0, |p| filmagic_render::scene::difference(p, &sig));
        batch.push((f, if d.is_finite() { d } else { 0.0 }));
        prev = Some((f, sig));
        done += 1;
        progress.done.fetch_add(1, Ordering::Relaxed);
        if batch.len() >= BATCH {
            flush(&mut batch);
        }
    }
    flush(&mut batch);
    Ok(done)
}

fn decode(src: &dyn filmagic_media::MediaSource, rate: FrameRate, f: i64, scale: f32) -> std::result::Result<Signature, String> {
    let frame = src.video_frame(filmagic_media::FrameRequest { time: rate.tick_of(f), scale }).map_err(|e| format!("frame {f} cannot be decoded: {e}"))?;
    Ok(signature(&frame.to_rgba8(), frame.width as usize, frame.height as usize))
}

/// The next chunk of work, taken from the queue; `None` when there is none (or the worker was
/// stopped), and then the worker is gone, decided under the same lock a new request takes.
fn next_chunk(cache: &Cache, progress: &filmagic_export::Progress) -> Option<(Target, filmagic_media::SharedSource, i64, i64)> {
    let mut inner = cache.lock();
    if progress.cancel.load(Ordering::Relaxed) {
        inner.queue.clear();
    }
    inner.queue.retain(|r| !r.ranges.is_empty());
    let Some(r) = inner.queue.first_mut() else {
        inner.worker = None;
        return None;
    };
    let (a, b) = r.ranges.first().copied()?;
    let end = b.min(a.saturating_add(CHUNK - 1));
    if end >= b {
        r.ranges.remove(0);
    } else if let Some(first) = r.ranges.first_mut() {
        first.0 = end + 1;
    }
    let target = Target { cache: cache.clone(), root: r.root, key: r.key.clone(), rate: r.rate };
    Some((target, r.src.clone(), a, end))
}

/// The worker: drains the queue chunk by chunk, then ends.
fn work(cache: Cache, progress: Arc<filmagic_export::Progress>, result: Arc<Mutex<Option<std::result::Result<filmagic_export::Report, String>>>>) {
    let t0 = web_time::Instant::now();
    let mut errors: Vec<String> = Vec::new();
    while let Some((target, src, a, b)) = next_chunk(&cache, &progress) {
        *progress.status.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = format!("frames {a}–{b} of item {}", target.root.0);
        if let Err(e) = fill(&target, &*src, a, b, &progress) {
            if e == "stopped" {
                continue;
            }
            // this item's media cannot be read: the rest of its ranges would fail too
            cache.lock().queue.retain(|r| r.root != target.root);
            errors.push(format!("item {}: {e}", target.root.0));
        }
    }
    let secs = t0.elapsed().as_secs_f64();
    let n = progress.done.load(Ordering::Relaxed);
    // frames already known were skipped: the job is done at what it decoded
    progress.total.store(n, Ordering::Relaxed);
    let stopped = progress.cancel.load(Ordering::Relaxed);
    *progress.status.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = if stopped {
        "stopped".into()
    } else if errors.is_empty() {
        format!("checked {n} frame(s) in {secs:.1}s")
    } else {
        errors.join("; ")
    };
    let out = if stopped {
        Err("stopped".to_string())
    } else {
        Ok(filmagic_export::Report { path: String::new(), frames: n, seconds: secs, bytes: 0, render_fps: n as f64 / secs.max(1e-6), ..Default::default() })
    };
    *result.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(out);
    progress.finished.store(true, Ordering::Relaxed);
}

/// A check that was started, or why none could be.
enum Queued {
    /// Queued for the session's background worker (its job id).
    Job(u64),
    /// It ran before returning (`wait`).
    Ran,
    /// Nothing to do: the range is known.
    Known,
    /// Not queued: the queue is full, or this host has no threads (the web build).
    Not(String),
}

/// Queue media frames `lo..=hi` of `item` (with context) for the session's one background worker,
/// starting it if it is not running; `wait` checks them here before returning instead.
fn queue(s: &mut Session, item: ItemId, lo: i64, hi: i64, wait: bool) -> Result<Queued> {
    let m = media_of(&s.project, item).ok_or_else(|| bad(CMD, "the item has no moving picture"))?;
    let Some((_, (a, b))) = spans(&m, lo, hi) else { return Ok(Queued::Known) };
    if cached(s, item, lo, hi).is_some_and(|f| f.complete) {
        return Ok(Queued::Known);
    }
    prune_jobs(s);
    let src = s
        .source(m.root)
        .ok_or_else(|| EngineError::Other(format!("{CMD}: the media of item {} is offline or cannot be read; relink it (`media.relink`)", item.0)))?;
    if wait {
        let target = Target { cache: s.source_cuts.clone(), root: m.root, key: m.key.clone(), rate: m.rate };
        fill(&target, &*src, a, b, &filmagic_export::Progress::default()).map_err(|e| EngineError::Other(format!("{CMD}: {e}")))?;
        return Ok(Queued::Ran);
    }
    if !background_threads() {
        return Ok(Queued::Not("the web build has no background threads: call `media.sourceCuts` with `\"wait\": true` to look for them".into()));
    }
    let new_frames = b - a + 1;
    let mut inner = s.source_cuts.lock();
    if queued_frames(&inner) + new_frames > MAX_QUEUED_FRAMES {
        return Ok(Queued::Not(format!(
            "{MAX_QUEUED_FRAMES} frames are already waiting to be checked: try again when job {} has finished",
            inner.worker.as_ref().map_or(0, |w| w.job)
        )));
    }
    match inner.queue.iter_mut().find(|r| r.root == m.root && r.key == m.key) {
        Some(r) => merge_range(&mut r.ranges, a, b),
        None => inner.queue.push(Request { root: m.root, key: m.key.clone(), rate: m.rate, src, ranges: vec![(a, b)] }),
    }
    if let Some(w) = &inner.worker {
        w.progress.total.fetch_add(new_frames.max(0) as u64, Ordering::Relaxed);
        return Ok(Queued::Job(w.job));
    }
    // no worker: start the one
    let id = s.jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1;
    let job = crate::Job { id, label: JOB_LABEL.into(), progress: Default::default(), result: Default::default() };
    job.progress.total.store(new_frames.max(0) as u64, Ordering::Relaxed);
    inner.worker = Some(Worker { job: id, progress: job.progress.clone() });
    inner.jobs.push(id);
    inner.live += 1;
    inner.most_live = inner.most_live.max(inner.live);
    drop(inner);
    let (cache, prog, res) = (s.source_cuts.clone(), job.progress.clone(), job.result.clone());
    let run = move || {
        let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(cache.clone(), prog.clone(), res.clone()))).is_ok();
        let mut inner = cache.lock();
        inner.live = inner.live.saturating_sub(1);
        if !ok {
            // a panic while decoding: the next request starts a new worker
            inner.worker = None;
            inner.queue.clear();
            drop(inner);
            let msg = "internal error while looking for cuts (see the crash log); the check was stopped".to_string();
            *prog.error.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(msg.clone());
            *res.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Err(msg));
            prog.finished.store(true, Ordering::Relaxed);
        }
    };
    s.jobs.push(job);
    if let Err(e) = std::thread::Builder::new().name("filmagic-source-cuts".into()).spawn(run) {
        let mut inner = s.source_cuts.lock();
        inner.worker = None;
        inner.live = inner.live.saturating_sub(1);
        return Err(EngineError::Other(e.to_string()));
    }
    Ok(Queued::Job(id))
}

thread_local! {
    /// Tests: behave as the web build does, which has no background threads.
    static NO_THREADS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether the cut finder may run in the background here. The web build has no threads: there an
/// edit or a board only says how to look for the cuts, and never decodes on the caller's thread.
fn background_threads() -> bool {
    !cfg!(target_arch = "wasm32") && !NO_THREADS.with(std::cell::Cell::get)
}

#[cfg(test)]
pub(crate) fn pretend_no_threads(on: bool) {
    NO_THREADS.with(|c| c.set(on));
}

/// The label of the cut finder's job (translated by the interface).
pub const JOB_LABEL: &str = "Find Cuts in Source";

fn secs(rate: FrameRate, f: i64) -> f64 {
    (rate.tick_of(f).seconds() * 1000.0).round() / 1000.0
}

/// The media frames a timeline clip shows, first and last (in media order).
fn shown(it: &TrackItem, rate: FrameRate) -> (i64, i64) {
    if let Some(h) = it.frame_hold {
        let f = rate.frame_at(h);
        return (f, f);
    }
    let (a, b) = (it.source_in, it.source_out());
    let (a, b) = (a.min(b), a.max(b));
    (rate.frame_at(a), rate.frame_at(b - Tick(1)).max(rate.frame_at(a)))
}

/// Timeline time at which a (moving) clip shows media time `mt`, at its clip speed (an animated
/// Time Remapping curve is not followed: see [`remapped`]).
fn timeline_at(it: &TrackItem, mt: Tick) -> Tick {
    let speed = it.speed.abs();
    if !speed.is_finite() || speed <= 1e-9 {
        return it.start;
    }
    let rel = if it.reverse { it.source_out() - mt } else { mt - it.source_in };
    let t = it.start + Tick((rel.0 as f64 / speed).round() as i64);
    t.clamp(it.start, it.end())
}

/// What retiming changes (a trim or a split does not: it changes what is shown, not how fast).
fn timing(it: &TrackItem) -> (f64, bool, Option<Tick>, Option<&filmagic_project::EffectInstance>) {
    (it.speed, it.reverse, it.frame_hold, it.effect("time_remap"))
}

/// The clip's Time Remapping speed is not 100 % somewhere (a constant 100 %, keyframed or not, is
/// no retiming).
fn remapped(it: &TrackItem) -> bool {
    let not_100 = |v: &filmagic_project::ParamValue| v.as_f64().is_some_and(|x| (x - 100.0).abs() > 1e-3);
    it.effect("time_remap")
        .filter(|e| e.enabled)
        .and_then(|e| e.param("speed"))
        .is_some_and(|p| not_100(&p.value) || p.keyframes.iter().any(|k| not_100(&k.value)))
}

/// A clip that plays its media other than once through at normal speed.
fn retimed(it: &TrackItem) -> bool {
    it.frame_hold.is_some() || it.reverse || (it.speed - 1.0).abs() > 1e-9 || remapped(it)
}

/// What one check adds to a result.
#[derive(Default)]
struct Report {
    warnings: Vec<String>,
    notes: Vec<String>,
    cuts: Vec<Value>,
    /// Clips whose range was not known yet: (item, lo, hi, clip).
    unknown: Vec<(ItemId, i64, i64, ClipId)>,
}

fn label(it: &TrackItem) -> String {
    format!("clip {} (\"{}\")", it.id.0, it.name)
}

/// What is known about `lo..=hi` of `it`'s media; what is not is noted to be checked.
fn known_for(s: &Session, it: &TrackItem, lo: i64, hi: i64, r: &mut Report) -> Option<Found> {
    let f = cached(s, it.item, lo, hi)?;
    if !f.complete {
        r.unknown.push((it.item, lo, hi, it.id));
    }
    Some(f)
}

/// An edit point at media frame `at` of `it`.
/// `whole` is the clip as it was before the edit split it: a cut on either side of the edit maps
/// to the timeline through it (the new piece starts at the edit, so a cut before it would be
/// clamped to the edit itself).
fn check_point(s: &Session, it: &TrackItem, whole: &TrackItem, at: i64, t: Tick, r: &mut Report) {
    let Some(f) = known_for(s, it, at - NEAR_FRAMES, at + NEAR_FRAMES, r) else { return };
    let Some(&c) = f.cuts.iter().min_by_key(|c| (**c - at).abs()) else { return };
    let off = at - c;
    let (ms, ts) = (secs(f.rate, c), t.seconds());
    r.cuts.push(
        json!({"clip": it.id.0, "item": it.item.0, "kind": "near", "mediaSeconds": ms, "framesFromEdit": off, "editSeconds": (ts * 1000.0).round() / 1000.0}),
    );
    if off == 0 {
        r.notes.push(format!("the edit at {ts:.3} s on {} lands exactly on a cut in its source ({ms:.3} s of media)", label(it)));
        return;
    }
    let side = if off > 0 { format!("{off} frame(s) after") } else { format!("{} frame(s) before", -off) };
    let what = if off > 0 { "the piece before it ends with frames of the next shot" } else { "the piece after it starts with frames of the previous shot" };
    r.warnings.push(format!(
        "the edit at {ts:.3} s on {} lands {side} a cut in its source at {ms:.3} s of media: {what}. Move the edit onto the cut (timeline {:.3} s) if a clean shot change was meant",
        label(it),
        timeline_at(whole, f.rate.tick_of(c)).seconds()
    ));
}

/// A retimed or held clip.
fn check_range(s: &Session, it: &TrackItem, r: &mut Report) {
    let Some(m) = media_of(&s.project, it.item) else { return };
    let rate = m.rate;
    let (lo, hi) = shown(it, rate);
    if it.frame_hold.is_some() {
        let Some(f) = known_for(s, it, lo - NEAR_FRAMES, lo + NEAR_FRAMES, r) else { return };
        if let Some(&c) = f.cuts.iter().min_by_key(|c| (**c - lo).abs()) {
            let ms = secs(rate, c);
            r.cuts.push(json!({"clip": it.id.0, "item": it.item.0, "kind": "near", "mediaSeconds": ms, "framesFromEdit": lo - c}));
            let which = if lo >= c { "the first frames of the shot after it" } else { "the last frames of the shot before it" };
            r.warnings.push(format!(
                "{} holds media frame {:.3} s, {} frame(s) from a cut in its source at {ms:.3} s: the held frame is one of {which}; check it is the shot you meant",
                label(it),
                secs(rate, lo),
                (lo - c).abs()
            ));
        }
        return;
    }
    let Some(f) = known_for(s, it, lo, hi, r) else { return };
    // with an animated remap curve the clip speed does not say where on the timeline a frame plays
    let approx = remapped(it);
    for &c in f.cuts.iter().filter(|c| **c > lo) {
        let (ms, ts) = (secs(rate, c), timeline_at(it, rate.tick_of(c)).seconds());
        let (before, after) = (c - lo, hi + 1 - c);
        let mut v = json!({"clip": it.id.0, "item": it.item.0, "kind": "inside", "mediaSeconds": ms, "timelineSeconds": (ts * 1000.0).round() / 1000.0, "framesBefore": before, "framesAfter": after});
        if approx {
            v["timelineApproximate"] = json!(true);
        }
        r.cuts.push(v);
        let about = if approx { "about " } else { "" };
        r.warnings.push(format!(
            "{} straddles a cut in its source at {ms:.3} s of media (timeline {about}{ts:.3} s): it plays {before} frame(s) of one shot and {after} of the next. Razor at the cut, or trim the clip to one side of it",
            label(it)
        ));
    }
}

/// After `id` ran on the active sequence, add source-cut warnings to its result, from what is
/// already known; what is not is queued to be checked in the background.
pub(crate) fn annotate(s: &mut Session, id: &str, before: &Project, result: &mut Value) {
    let Some(seq) = s.state.active_sequence else { return };
    let Some(q) = s.project.sequence(seq) else { return };
    let old: HashMap<ClipId, &TrackItem> =
        before.sequence(seq).map(|q| q.video_tracks.iter().flat_map(|t| t.items.iter()).map(|it| (it.id, it)).collect()).unwrap_or_default();
    let razor = RAZORS.contains(&id);
    // a new piece, where it starts in its media, the timeline time, and the clip it was cut from
    let mut points: Vec<(TrackItem, i64, Tick, TrackItem)> = Vec::new();
    let mut ranges: Vec<TrackItem> = Vec::new();
    for it in q.video_tracks.iter().flat_map(|t| t.items.iter()) {
        let Some(m) = media_of(&s.project, it.item) else { continue };
        match old.get(&it.id) {
            // a new piece: its start is the edit point
            None if razor => {
                let (a, b) = shown(it, m.rate);
                // the clip it was cut from: the same media, on screen where the edit is
                let whole = old.values().find(|o| o.item == it.item && o.start < it.start && it.start < o.end()).map_or_else(|| it.clone(), |o| (*o).clone());
                points.push((it.clone(), if it.reverse { b + 1 } else { a }, it.start, whole));
            }
            // retimed now (a clip set back to 100 % plays its frames once: nothing to say)
            None if retimed(it) => ranges.push(it.clone()),
            Some(o) if !razor && timing(o) != timing(it) && retimed(it) => ranges.push(it.clone()),
            _ => {}
        }
    }
    if points.is_empty() && ranges.is_empty() {
        return;
    }
    let mut r = Report::default();
    for (it, at, t, whole) in &points {
        check_point(s, it, whole, *at, *t, &mut r);
    }
    for it in &ranges {
        check_range(s, it, &mut r);
    }
    for (item, lo, hi, clip) in std::mem::take(&mut r.unknown) {
        match queue(s, item, lo, hi, false) {
            Ok(Queued::Job(job)) => r.notes.push(format!(
                "cuts inside the source of clip {} are not known yet for this range: the background check (job {job}, `jobs.list`) looks for them; once it is finished or no longer listed, `media.sourceCuts` {{\"clip\":{}}} lists them and the next edit warns about them",
                clip.0, clip.0
            )),
            Ok(Queued::Not(why)) => r.notes.push(format!("cuts inside the source of clip {} are not known yet for this range: {why}", clip.0)),
            Ok(_) => {}
            Err(e) => r.notes.push(format!("cuts inside the source of clip {} could not be checked: {e}", clip.0)),
        }
    }
    if r.warnings.is_empty() && r.notes.is_empty() {
        return;
    }
    merge_into(result, r);
}

fn merge_into(result: &mut Value, r: Report) {
    if !result.is_object() {
        let old = std::mem::take(result);
        *result = if old.is_null() { json!({}) } else { json!({"result": old}) };
    }
    let Some(o) = result.as_object_mut() else { return };
    for (key, items) in [("warnings", r.warnings), ("notes", r.notes)] {
        if items.is_empty() {
            continue;
        }
        let list = o.entry(key).or_insert_with(|| json!([]));
        if let Value::String(one) = list {
            *list = json!([one.clone()]);
        }
        if let Some(a) = list.as_array_mut() {
            a.extend(items.into_iter().map(Value::from));
        }
    }
    if !r.cuts.is_empty() {
        o.insert("sourceCuts".into(), Value::Array(r.cuts));
    }
}

/// [`annotate`] that cannot take the command's result down: a panic while reading the source
/// becomes a note.
pub(crate) fn annotate_guarded(s: &mut Session, id: &str, before: &Project, result: &mut Value) {
    let mut out = result.clone();
    let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| annotate(s, id, before, &mut out))).is_ok();
    if ok {
        *result = out;
    } else {
        merge_into(
            result,
            Report {
                notes: vec!["cuts inside the source could not be checked (internal error, see the crash log); the edit itself is done".into()],
                ..Default::default()
            },
        );
    }
}

/// Seconds parameter: finite and not negative.
fn seconds_p(p: &Value, key: &str) -> Result<Option<f64>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            v.as_f64().filter(|x| x.is_finite() && *x >= 0.0).map(Some).ok_or_else(|| bad(CMD, format!("`{key}` must be a number of seconds, 0 or more")))
        }
    }
}

const CMD: &str = "media.sourceCuts";

fn source_cuts_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    let clip = match p.get("clip") {
        None | Some(Value::Null) => None,
        Some(_) => Some(clip_p(p, "clip").ok_or_else(|| bad(CMD, "`clip` must be a timeline clip id"))?),
    };
    let item = match p.get("item") {
        None | Some(Value::Null) => None,
        Some(_) => Some(item_p(p, "item").ok_or_else(|| bad(CMD, "`item` must be a project item id"))?),
    };
    let wait = match p.get("wait") {
        None | Some(Value::Null) => false,
        Some(_) => bool_p(p, "wait").ok_or_else(|| bad(CMD, "`wait` must be true or false"))?,
    };
    let it = match (clip, item) {
        (Some(c), None) => {
            let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
            Some(
                q.find_item(c)
                    .map(|(_, it)| it.clone())
                    .ok_or_else(|| bad(CMD, format!("no clip {} in the active sequence (`sequence_inspect` lists the clips)", c.0)))?,
            )
        }
        (None, Some(_)) => None,
        _ => return Err(bad(CMD, "need a project `item` or a timeline `clip` (one of them)")),
    };
    let item = it.as_ref().map_or_else(|| item.unwrap_or(ItemId(0)), |c| c.item);
    let pi = s.project.item(item).ok_or_else(|| bad(CMD, format!("no project item {} (`project_inspect` lists them)", item.0)))?;
    let name = pi.name.clone();
    let Some(m) = media_of(&s.project, item) else {
        return Err(bad(CMD, format!("`{name}` has no moving picture to look for cuts in")));
    };
    let rate = m.rate;
    let (mut lo, mut hi) = it.as_ref().map_or((0, m.frames.saturating_sub(1)), |c| shown(c, rate));
    if let Some(x) = seconds_p(p, "fromSeconds")? {
        lo = lo.max(rate.frame_at(Tick::from_seconds_f64(x.min(1e9))));
    }
    if let Some(x) = seconds_p(p, "toSeconds")? {
        hi = hi.min(rate.frame_at(Tick::from_seconds_f64(x.min(1e9))));
    }
    if hi < lo {
        return Err(bad(CMD, "nothing to check between `fromSeconds` and `toSeconds` (media seconds)"));
    }
    let queued = queue(s, item, lo, hi, wait)?;
    let f = cached(s, item, lo, hi).ok_or_else(|| bad(CMD, format!("`{name}` has no moving picture")))?;
    let cuts: Vec<Value> = f
        .cuts
        .iter()
        .map(|&c| {
            let mut v = json!({"mediaSeconds": secs(rate, c), "frame": c, "time": rate.tick_of(c).0});
            if let Some(it) = it.as_ref().filter(|it| it.frame_hold.is_none()) {
                v["timelineSeconds"] = json!((timeline_at(it, rate.tick_of(c)).seconds() * 1000.0).round() / 1000.0);
                if remapped(it) {
                    v["timelineApproximate"] = json!(true);
                }
            }
            v
        })
        .collect();
    let mut out = json!({
        "item": item.0,
        "name": name,
        "frameRate": rate.as_f64(),
        "complete": f.complete,
        "cuts": cuts,
        "note": "cuts are where the picture changes to another shot inside the file (Scene Edit Detection's measure at sensitivity 50); a razor, speed change or frame hold near one warns",
    });
    if let Some((a, b)) = f.checked {
        out["checked"] = json!({"fromSeconds": secs(rate, a), "toSeconds": secs(rate, b + 1)});
    }
    if let Some(c) = it {
        out["clip"] = json!(c.id.0);
    }
    if !f.complete {
        let from = f.checked.map_or(lo, |c| c.1 + 1);
        out["next"] = json!({"fromSeconds": secs(rate, from)});
        match queued {
            Queued::Job(job) => {
                out["job"] = json!(job);
                out["hint"] = json!(format!(
                    "the background check (job {job}) looks for them: poll `jobs.list` until it is finished or no longer listed, then call `media.sourceCuts` again with the same parameters"
                ));
            }
            Queued::Not(why) => out["hint"] = json!(why),
            _ => {
                out["hint"] = json!(format!("{MAX_JOB_FRAMES} frames are checked per job; call again from `next` for the rest"));
            }
        }
    }
    Ok(out)
}

/// The source cuts already known in media time `lo..=hi` of `item`, for boards: (cuts, whether
/// all of the range is known, the background job started for the rest).
pub(crate) fn known_cuts(s: &mut Session, item: ItemId, lo: Tick, hi: Tick) -> Option<(Vec<Tick>, bool, Option<u64>)> {
    let rate = media_of(&s.project, item)?.rate;
    let (a, b) = (rate.frame_at(lo), rate.frame_at(hi));
    let f = cached(s, item, a, b)?;
    let job = if f.complete { None } else { queue(s, item, a, b, false).ok().and_then(|q| if let Queued::Job(j) = q { Some(j) } else { None }) };
    Some((f.cuts.iter().map(|c| rate.tick_of(*c)).collect(), f.complete, job))
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: CMD,
        label: "Find Cuts in Source",
        menu: &[],
        shortcut: None,
        params: r#"{"item":id | "clip":id (a timeline clip: the media it shows, with timelineSeconds),"fromSeconds":f64? (media seconds),"toSeconds":f64?,"wait":bool=false (false: report what is known and queue the rest for the session's one background checker, `jobs.list`; true: check up to 7200 frames before answering, which can take minutes on long-GOP H.264; the web build has no background checker and needs true)} -> {cuts:[{mediaSeconds,frame,timelineSeconds?}],checked,complete,job?,next?}"#,
        enabled: always,
        run: source_cuts_cmd,
        journal: false,
        named: NamedKeys::NONE,
    }]
}

#[cfg(test)]
mod tests;
