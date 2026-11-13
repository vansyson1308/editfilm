//! Cuts hidden inside a source file (#16).
//!
//! A clip of a long recording, a compilation or a screen capture can hold shot changes of its
//! own. An edit that does not know about them can slow down two shots at once, freeze a frame of
//! the wrong shot or razor two frames away from a cut that was already there. This module finds
//! those cuts with the same picture measure as Scene Edit Detection ([`filmagic_render::scene`],
//! default sensitivity, look-alike cuts included), per project item, on demand:
//!
//! - **`media.sourceCuts {item|clip, fromSeconds?, toSeconds?}`** lists the cuts in an item's
//!   media (or the media a timeline clip shows), at most [`MAX_CALL_FRAMES`] frames per call;
//! - after **`timeline.razor`, `sequence.addEdit`, `sequence.addEditAllTracks`,
//!   `clip.speedDuration`, `timeline.rateStretch`, `clip.frameHold`, `clip.frameHoldOptions`,
//!   `clip.insertFrameHoldSegment`** and any `effects.*` command that changes a clip's Time
//!   Remapping, the result gets a warning when the edit lands within [`NEAR_FRAMES`] frames of a
//!   source cut, or when the retimed range straddles one ([`annotate`]). Those checks analyse at
//!   most [`MAX_EDIT_FRAMES`] new frames per command, so an edit never waits long; what was not
//!   checked is named in `notes`.
//!
//! Frame differences are cached per media item ([`Cache`]): a range is decoded once, at a small
//! size, and later checks of it cost nothing.

use std::collections::{BTreeMap, HashMap};

use filmagic_project::{ClipId, ItemId, Project, TrackItem};
use filmagic_render::scene::{SceneOptions, cuts_with_look_alikes, signature};
use filmagic_time::{FrameRate, Tick};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, NamedKeys, always, bad, clip_p, item_p};
use crate::{EngineError, Result, Session};

/// An edit this many media frames or fewer from a source cut is "near" it.
pub const NEAR_FRAMES: i64 = 3;
/// New frames one edit's check may decode (about 8 s of 30 fps media).
pub const MAX_EDIT_FRAMES: i64 = 240;
/// New frames one `media.sourceCuts` call may decode (4 minutes of 30 fps media).
pub const MAX_CALL_FRAMES: i64 = 4 * 60 * 30;
/// Frame differences kept for all items together; past it the cache starts afresh.
const MAX_CACHED_FRAMES: usize = 2_000_000;
/// Frames looked at on each side of a range: the detector compares a step with its neighbours.
const CONTEXT: i64 = 6;
/// Longest side of the frames compared (the measure needs no detail).
const SIDE: f64 = 96.0;

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
    /// What the item's media was when it was analysed (a relink starts the entry afresh).
    key: String,
    rate: FrameRate,
    /// `diffs[f]`: the difference between frames `f - 1` and `f` (0 for frame 0).
    diffs: BTreeMap<i64, f32>,
}

/// Source-cut analysis per media item (session state, not saved).
#[derive(Default)]
pub struct Cache {
    media: HashMap<ItemId, Entry>,
}

impl Cache {
    fn frames(&self) -> usize {
        self.media.values().map(|e| e.diffs.len()).sum()
    }
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
    /// Why the media could not be read, when it could not.
    pub error: Option<String>,
    pub rate: FrameRate,
}

/// The media item a project item shows (a subclip's parent) and its frame rate and frame count;
/// `None` for an item without a moving picture.
fn media_of(p: &Project, item: ItemId) -> Option<(ItemId, FrameRate, i64, String)> {
    let (root, m, _) = p.resolve_media(item)?;
    let v = m.info.video.as_ref()?;
    if matches!(m.info.kind, filmagic_media::MediaKind::Still | filmagic_media::MediaKind::AudioOnly) || v.width == 0 {
        return None;
    }
    let rate = m.frame_rate().sane();
    let frames = rate.frame_at(m.info.duration).max(0);
    Some((root, rate, frames, format!("{:?}", m.media)))
}

/// Decode what is missing of frames `lo..=hi` of `root` into `entry`, spending at most `budget`
/// decodes. Returns the last frame (from `lo`, in order) whose difference is known.
fn fill(s: &Session, root: ItemId, entry: &mut Entry, lo: i64, hi: i64, budget: &mut i64) -> std::result::Result<Option<i64>, String> {
    let mut known = None;
    let Some(src) = s.source(root) else { return Err("its media is offline or cannot be read".into()) };
    let scale = src.info().video.as_ref().map_or(1.0, |v| (SIDE / f64::from(v.width.max(v.height).max(1))).min(1.0)) as f32;
    let mut prev: Option<filmagic_render::scene::Signature> = None;
    let mut f = lo;
    while f <= hi {
        if entry.diffs.contains_key(&f) {
            known = Some(f);
            prev = None;
            f += 1;
            continue;
        }
        // a missing run starts here: its first difference needs the frame before it
        if prev.is_none() && f > 0 {
            if *budget <= 0 {
                break;
            }
            *budget -= 1;
            prev = Some(decode(&*src, entry.rate, f - 1, scale)?);
        }
        if *budget <= 0 {
            break;
        }
        *budget -= 1;
        let sig = decode(&*src, entry.rate, f, scale)?;
        let d = prev.as_ref().map_or(0.0, |p| filmagic_render::scene::difference(p, &sig));
        entry.diffs.insert(f, if d.is_finite() { d } else { 0.0 });
        prev = Some(sig);
        known = Some(f);
        f += 1;
    }
    Ok(known)
}

fn decode(src: &dyn filmagic_media::MediaSource, rate: FrameRate, f: i64, scale: f32) -> std::result::Result<filmagic_render::scene::Signature, String> {
    let frame = src.video_frame(filmagic_media::FrameRequest { time: rate.tick_of(f), scale }).map_err(|e| format!("frame {f} cannot be decoded: {e}"))?;
    Ok(signature(&frame.to_rgba8(), frame.width as usize, frame.height as usize))
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

/// Find the cuts that start media frames `lo..=hi` of `item`'s media, decoding at most `budget`
/// new frames. `None` for an item without a moving picture.
pub fn check(s: &mut Session, item: ItemId, lo: i64, hi: i64, budget: &mut i64) -> Option<Found> {
    let (root, rate, frames, key) = media_of(&s.project, item)?;
    let last = frames.saturating_sub(1);
    let (lo, hi) = (lo.clamp(0, last.max(0)), hi.clamp(0, last.max(0)));
    if hi < lo || frames == 0 {
        return Some(Found { complete: true, rate, ..Default::default() });
    }
    let mut cache = std::mem::take(&mut s.source_cuts);
    if cache.frames() > MAX_CACHED_FRAMES {
        cache.media.clear();
    }
    let entry = cache.media.entry(root).or_insert_with(|| Entry { key: key.clone(), rate, diffs: BTreeMap::new() });
    if entry.key != key || entry.rate != rate {
        *entry = Entry { key, rate, diffs: BTreeMap::new() };
    }
    let (a, b) = (lo.saturating_sub(CONTEXT).max(0), hi.saturating_add(CONTEXT).min(last));
    let mut found = Found { rate, ..Default::default() };
    match fill(s, root, entry, a, b, budget) {
        Ok(known) => {
            // the range is checked up to where the context after it is known too
            let through = known.map(|k| if k >= b { hi } else { k.saturating_sub(CONTEXT) });
            found.checked = through.filter(|t| *t >= lo).map(|t| (lo, t.min(hi)));
            found.complete = known.is_some_and(|k| k >= b);
            if let Some((x, y)) = found.checked {
                found.cuts = cuts_known(entry, x, y);
            }
        }
        Err(e) => found.error = Some(e),
    }
    s.source_cuts = cache;
    Some(found)
}

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

/// Timeline time at which a (moving) clip shows media time `mt`.
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

/// A clip that plays its media other than once through at normal speed.
fn retimed(it: &TrackItem) -> bool {
    it.frame_hold.is_some()
        || it.reverse
        || (it.speed - 1.0).abs() > 1e-9
        || it.effect("time_remap").and_then(|e| e.param("speed")).is_some_and(|p| p.is_animated())
}

/// What one check adds to a result.
#[derive(Default)]
struct Report {
    warnings: Vec<String>,
    notes: Vec<String>,
    cuts: Vec<Value>,
}

fn label(it: &TrackItem) -> String {
    format!("clip {} (\"{}\")", it.id.0, it.name)
}

fn unchecked_note(r: &mut Report, it: &TrackItem, f: &Found, lo: i64, hi: i64) {
    if let Some(e) = &f.error {
        r.notes.push(format!("source cuts of {} were not checked: {e}", label(it)));
    } else if !f.complete {
        let from = f.checked.map_or(lo, |c| c.1 + 1);
        r.notes.push(format!(
            "source cuts of {} were checked only up to {:.3} s of its media ({MAX_EDIT_FRAMES} new frames per edit); run `media.sourceCuts` {{\"clip\":{},\"fromSeconds\":{:.3},\"toSeconds\":{:.3}}} to check the rest",
            label(it),
            secs(f.rate, from),
            it.id.0,
            secs(f.rate, from),
            secs(f.rate, hi + 1)
        ));
    }
}

/// An edit point at media frame `at` of `it`.
fn check_point(s: &mut Session, it: &TrackItem, at: i64, t: Tick, budget: &mut i64, r: &mut Report) {
    let Some(f) = check(s, it.item, at - NEAR_FRAMES, at + NEAR_FRAMES, budget) else { return };
    unchecked_note(r, it, &f, at - NEAR_FRAMES, at + NEAR_FRAMES);
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
        timeline_at(it, f.rate.tick_of(c)).seconds()
    ));
}

/// A retimed or held clip.
fn check_range(s: &mut Session, it: &TrackItem, budget: &mut i64, r: &mut Report) {
    let Some((_, rate, _, _)) = media_of(&s.project, it.item) else { return };
    let (lo, hi) = shown(it, rate);
    if it.frame_hold.is_some() {
        let Some(f) = check(s, it.item, lo - NEAR_FRAMES, lo + NEAR_FRAMES, budget) else { return };
        unchecked_note(r, it, &f, lo - NEAR_FRAMES, lo + NEAR_FRAMES);
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
    let Some(f) = check(s, it.item, lo, hi, budget) else { return };
    unchecked_note(r, it, &f, lo, hi);
    for &c in f.cuts.iter().filter(|c| **c > lo) {
        let (ms, ts) = (secs(rate, c), timeline_at(it, rate.tick_of(c)).seconds());
        let (before, after) = (c - lo, hi + 1 - c);
        r.cuts.push(json!({"clip": it.id.0, "item": it.item.0, "kind": "inside", "mediaSeconds": ms, "timelineSeconds": (ts * 1000.0).round() / 1000.0, "framesBefore": before, "framesAfter": after}));
        r.warnings.push(format!(
            "{} straddles a cut in its source at {ms:.3} s of media (timeline {ts:.3} s): it plays {before} frame(s) of one shot and {after} of the next at this speed. Razor at {ts:.3} s, or trim the clip to one side of the cut",
            label(it)
        ));
    }
}

/// After `id` ran on the active sequence, add source-cut warnings to its result.
pub(crate) fn annotate(s: &mut Session, id: &str, before: &Project, result: &mut Value) {
    let Some(seq) = s.state.active_sequence else { return };
    let Some(q) = s.project.sequence(seq) else { return };
    let old: HashMap<ClipId, &TrackItem> =
        before.sequence(seq).map(|q| q.video_tracks.iter().flat_map(|t| t.items.iter()).map(|it| (it.id, it)).collect()).unwrap_or_default();
    let razor = RAZORS.contains(&id);
    let mut points: Vec<(TrackItem, i64, Tick)> = Vec::new();
    let mut ranges: Vec<TrackItem> = Vec::new();
    for it in q.video_tracks.iter().flat_map(|t| t.items.iter()) {
        let Some((_, rate, _, _)) = media_of(&s.project, it.item) else { continue };
        match old.get(&it.id) {
            // a new piece: its start is the edit point
            None if razor => {
                let (a, b) = shown(it, rate);
                points.push((it.clone(), if it.reverse { b + 1 } else { a }, it.start));
            }
            None if retimed(it) => ranges.push(it.clone()),
            None => {}
            Some(o) if !razor && timing(o) != timing(it) => ranges.push(it.clone()),
            Some(_) => {}
        }
    }
    if points.is_empty() && ranges.is_empty() {
        return;
    }
    let mut budget = MAX_EDIT_FRAMES;
    let mut r = Report::default();
    for (it, at, t) in &points {
        check_point(s, it, *at, *t, &mut budget, &mut r);
    }
    for it in &ranges {
        check_range(s, it, &mut budget, &mut r);
    }
    if r.warnings.is_empty() && r.notes.is_empty() {
        return;
    }
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
    let Some((_, rate, frames, _)) = media_of(&s.project, item) else {
        return Err(bad(CMD, format!("`{name}` has no moving picture to look for cuts in")));
    };
    let (mut lo, mut hi) = it.as_ref().map_or((0, frames.saturating_sub(1)), |c| shown(c, rate));
    if let Some(x) = seconds_p(p, "fromSeconds")? {
        lo = lo.max(rate.frame_at(Tick::from_seconds_f64(x.min(1e9))));
    }
    if let Some(x) = seconds_p(p, "toSeconds")? {
        hi = hi.min(rate.frame_at(Tick::from_seconds_f64(x.min(1e9))));
    }
    if hi < lo {
        return Err(bad(CMD, "nothing to check between `fromSeconds` and `toSeconds` (media seconds)"));
    }
    let mut budget = MAX_CALL_FRAMES;
    let f = check(s, item, lo, hi, &mut budget).ok_or_else(|| bad(CMD, format!("`{name}` has no moving picture")))?;
    if let Some(e) = f.error {
        return Err(EngineError::Other(format!("{CMD}: `{name}`: {e}; relink it (`file.relink`) or pick another item")));
    }
    let cuts: Vec<Value> = f
        .cuts
        .iter()
        .map(|&c| {
            let mut v = json!({"mediaSeconds": secs(rate, c), "frame": c, "time": rate.tick_of(c).0});
            if let Some(it) = it.as_ref().filter(|it| it.frame_hold.is_none()) {
                v["timelineSeconds"] = json!((timeline_at(it, rate.tick_of(c)).seconds() * 1000.0).round() / 1000.0);
            }
            v
        })
        .collect();
    let (a, b) = f.checked.unwrap_or((lo, lo.saturating_sub(1)));
    let mut out = json!({
        "item": item.0,
        "name": name,
        "frameRate": rate.as_f64(),
        "checked": {"fromSeconds": secs(rate, a), "toSeconds": secs(rate, b + 1)},
        "complete": f.complete,
        "cuts": cuts,
        "note": "cuts are where the picture changes to another shot inside the file (Scene Edit Detection's measure at sensitivity 50); a razor, speed change or frame hold near one warns",
    });
    if let Some(c) = it {
        out["clip"] = json!(c.id.0);
    }
    if !f.complete {
        out["next"] =
            json!({"fromSeconds": secs(rate, b + 1), "hint": format!("{MAX_CALL_FRAMES} frames are checked per call; call again from here for the rest")});
    }
    Ok(out)
}

/// The source cuts already known (or found within `budget`) in media frames `lo..=hi` of
/// `item`, for boards: (cuts in media time, whether all of the range was checked).
pub(crate) fn known_cuts(s: &mut Session, item: ItemId, lo: Tick, hi: Tick, budget: i64) -> Option<(Vec<Tick>, bool)> {
    let (_, rate, _, _) = media_of(&s.project, item)?;
    let mut b = budget;
    let f = check(s, item, rate.frame_at(lo), rate.frame_at(hi), &mut b)?;
    if f.error.is_some() {
        return None;
    }
    Some((f.cuts.iter().map(|c| rate.tick_of(*c)).collect(), f.complete))
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: CMD,
        label: "Find Cuts in Source",
        menu: &[],
        shortcut: None,
        params: r#"{"item":id | "clip":id (a timeline clip: the media it shows, with timelineSeconds),"fromSeconds":f64? (media seconds),"toSeconds":f64?} -> {cuts:[{mediaSeconds,frame,timelineSeconds?}],checked,complete,next?}; up to 7200 frames per call"#,
        enabled: always,
        run: source_cuts_cmd,
        journal: false,
        named: NamedKeys::NONE,
    }]
}

#[cfg(test)]
mod tests;
