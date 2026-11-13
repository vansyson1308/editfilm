//! Story tools: what an editor weighs about a cut, measured.
//!
//! These commands give a person, or an agent driving the editor, the evidence an editor works
//! from. They never decide: what the audience should feel and whether the story moves are
//! judgements, and the commands say so in their results.
//!
//! | Id | What it answers |
//! |---|---|
//! | `story.rhythm` | How fast is this cut, against the range of human blink rates? Which shots are flashes, which stretches are over-cut, is it a metronome? |
//! | `story.cuts` | For each cut: does the picture change enough to read as a new shot (or is it a jump cut), how far does the eye travel, does screen direction reverse, does it land on a pause, is it a straight cut or a split edit? The six criteria of a good cut, weighted, with the two that cannot be measured (emotion, story) left for the caller to supply. |
//! | `story.blinkPoints` | Where could this clip be cut? The ends of phrases in its sound and the moments its movement completes: the points where a listener would blink. |
//! | `story.board` | One representative frame per shot (or frames across a clip's rushes), laid out as a page: the whole film at a glance. |
//! | `story.splitEdit` | Turn a straight cut into an L-cut or a J-cut, so the picture does not change exactly when the speaker does. |
//! | `story.shadowCut` | Keep the current version aside before trying another path. |
//! | `story.compare` | What changed between this version and another? |
//!
//! Shots are read from the picture: at every moment the topmost enabled clip of footage (media,
//! subclips and nested sequences; titles, graphics and adjustment layers ride on top of a shot and
//! do not start one). A cut is where that clip changes.

use std::collections::BTreeSet;

use filmagic_project::{ClipId, ItemId, ItemKind, Label, Marker, MarkerId, MarkerKind, Project, Sequence, TrackItem, TrackKind};
use filmagic_render::scene::{Signature, difference, signature};
use filmagic_render::story::{Attention, Change, Glance, Pace, Sheet, attention, contact_sheet, drift, eye_travel, medoid, rhythm, screen_direction, settles};
use filmagic_time::{FrameRate, Tick};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, NamedKeys, bad, clip_p, has_seq, item_p, str_p, time_p, u64_p};
use crate::{EngineError, Result, Session};

type Run = fn(&mut Session, &Value) -> Result<Value>;

/// Most clips a sequence may hold for its shots to be read.
pub const MAX_PICTURE_CLIPS: usize = 200_000;
/// Most shots or cuts one result lists.
const MAX_LISTED: usize = 500;
/// Longest side of the frames analysed.
const ANALYSIS_SIDE: f32 = 256.0;
/// Longest stretch `story.blinkPoints` analyses in one call, seconds.
pub const MAX_BLINK_SECONDS: f64 = 600.0;
/// Weights of the six criteria of a cut, in order of importance (per cent).
pub const SIX: [(&str, f64); 6] = [("emotion", 51.0), ("story", 23.0), ("rhythm", 10.0), ("eyeTrace", 7.0), ("planarity", 5.0), ("space", 4.0)];

fn query(id: &'static str, label: &'static str, params: &'static str, run: Run) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: has_seq, run, journal: false, named: NamedKeys::NONE }
}

fn edit(id: &'static str, label: &'static str, params: &'static str, run: Run) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: has_seq, run, journal: true, named: NamedKeys::NONE }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        query("story.rhythm", "Cutting Rhythm", r#"{"window":seconds=10,"flashFrames":n=8}"#, rhythm_cmd),
        query(
            "story.cuts",
            "Analyse Cuts",
            r#"{"time":ticks?|"seconds":f64?|"frame":i64? (only the cut nearest),"fromSeconds":f64?,"toSeconds":f64?,"limit":n=60,"emotion":0..1?,"story":0..1? (your judgement of the one cut at `time`)}"#,
            cuts_cmd,
        ),
        CommandSpec {
            id: "story.blinkPoints",
            label: "Find Blink Points",
            menu: &[],
            shortcut: None,
            params: r#"{"clip":id|"item":id,"fromSeconds":f64?,"toSeconds":f64?,"minPause":seconds=0.25,"picture":bool=true,"markers":bool=false (clip only: add a sequence marker at each point)}"#,
            enabled: crate::commands::always,
            run: blink_points,
            journal: true,
            named: NamedKeys::NONE,
        },
        CommandSpec {
            id: "story.board",
            label: "Story Board",
            menu: &[],
            shortcut: None,
            params: r#"{"path":str,"item":id? (frames across this clip's rushes, cells after a cut inside the source marked "| CUT"; default: one frame per shot of the active sequence),"count":n=12 (item),"fps":f64? | "step":seconds? (sample densely instead: one frame every step across fromSeconds..toSeconds, at most 96 frames),"fromSeconds":f64?,"toSeconds":f64?,"columns":n=4,"width":px=320,"max":n=48,"labels":bool=true}"#,
            enabled: crate::commands::always,
            run: board,
            journal: false,
            named: NamedKeys::NONE,
        },
        edit(
            "story.splitEdit",
            "Split Edit (L-Cut / J-Cut)",
            r#"{"kind":"L"|"J","frames":n=12,"roll":"audio"|"video"="audio","time":ticks?|"seconds":f64?|"frame":i64? (the cut nearest; default: the playhead)}"#,
            split_edit,
        ),
        edit("story.shadowCut", "Keep a Shadow Cut", r#"{"name":str?}"#, shadow_cut),
        query(
            "story.compare",
            "Compare With Another Cut",
            r#"{"with":sequenceItemId,"picture":bool=true (also compare what each shot looks like),"limit":n=60 (shots compared by picture)}"#,
            compare,
        ),
        query("sequence.clips", "List Clips", r#"{"fromSeconds":f64?,"toSeconds":f64?,"track":"V1"|"A2"? (default: every track)}"#, list_clips),
    ]
}

/// A number under `key`: `None` when absent or null, an error when it is anything but a number.
fn num_p(p: &Value, key: &str, cmd: &str) -> Result<Option<f64>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_f64().filter(|x| x.is_finite()).map(Some).ok_or_else(|| bad(cmd, format!("`{key}` must be a number"))),
    }
}

/// A flag under `key`, `default` when absent or null; anything but true or false is an error.
fn flag_p(p: &Value, key: &str, default: bool, cmd: &str) -> Result<bool> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v.as_bool().ok_or_else(|| bad(cmd, format!("`{key}` must be true or false"))),
    }
}

/// The time a caller points at with `time`, `seconds`, `frame` or `timecode`: `None` when none of
/// them is given, an error when one is given and cannot be read (never a silent default).
fn nearest_p(s: &Session, p: &Value, cmd: &str) -> Result<Option<Tick>> {
    let given = ["time", "seconds", "frame", "timecode"].iter().any(|k| p.get(k).is_some_and(|v| !v.is_null()));
    match time_p(s, p, "") {
        Some(t) => Ok(Some(t)),
        None if given => Err(bad(cmd, "the time must be `time` (ticks), `seconds`, `frame` or `timecode`")),
        None => Ok(None),
    }
}

/// Pixels between the cells of a board, and around it.
const BOARD_GAP: usize = 6;

/// The height of a board cell `width` wide for a picture of `size` pixels shaped `par` (width over
/// height): the picture as it is shown, not as it is stored.
fn cell_height(width: f64, size: (usize, usize), par: (u32, u32)) -> usize {
    let shown = size.0.max(1) as f64 * filmagic_project::par_ratio(filmagic_project::sane_par(par));
    ((width * size.1.max(1) as f64 / shown.max(1.0)).round() as usize).clamp(8, 4 * (width as usize).max(2))
}

/// Refuse a board of `cells` frames, `width` wide, in `columns` columns, that would exceed what one
/// sheet holds, before any frame is rendered. The sheet is whole rows with gaps between them, so
/// it is larger than its frames put together.
fn board_budget(cells: usize, columns: usize, width: f64, size: (u32, u32), par: (u32, u32), cmd: &str) -> Result<()> {
    let columns = columns.clamp(1, cells.max(1));
    let rows = cells.div_ceil(columns).max(1);
    // a pixel to spare: a frame may decode a line taller than its nominal shape
    let cell_h = cell_height(width, (size.0 as usize, size.1 as usize), par).saturating_add(1);
    let w = columns.saturating_mul(width as usize).saturating_add(BOARD_GAP.saturating_mul(columns + 1));
    let h = rows.saturating_mul(cell_h).saturating_add(BOARD_GAP.saturating_mul(rows + 1));
    if w.saturating_mul(h) > filmagic_render::story::MAX_SHEET_PIXELS {
        return Err(bad(cmd, format!("the board would be {w}×{h} pixels, more than one sheet holds; lower `width`, `max` or `count`")));
    }
    Ok(())
}

/// The sequence's frame as displayed: width over height, pixel shape included.
fn display_aspect(q: &Sequence) -> f32 {
    let par = filmagic_project::sane_par(q.settings.par);
    (f64::from(q.settings.width.max(1)) * filmagic_project::par_ratio(par) / f64::from(q.settings.height.max(1))) as f32
}

/// The first frame of the sequence's grid at or after `t`.
fn frame_from(rate: FrameRate, t: Tick) -> Tick {
    let f = rate.frame_at(t);
    let on = rate.tick_of(f);
    if on < t { rate.tick_of(f.saturating_add(1)) } else { on }
}

/// The frame of the sequence's grid on screen just before `t`.
fn frame_before(rate: FrameRate, t: Tick) -> Tick {
    rate.snap((t - Tick(1)).max(Tick::ZERO))
}

/// A time as every form a caller might want.
fn time_json(t: Tick, rate: FrameRate) -> Value {
    json!({"time": t.0, "seconds": round3(t.seconds()), "frame": rate.frame_at(t)})
}

fn round3(v: f64) -> f64 {
    if v.is_finite() { (v * 1000.0).round() / 1000.0 } else { 0.0 }
}

/// One shot: a stretch of the sequence showing one clip of footage (`None`: nothing, black).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shot {
    pub start: Tick,
    pub end: Tick,
    pub clip: Option<ClipId>,
}

impl Shot {
    fn seconds(&self) -> f64 {
        (self.end - self.start).seconds()
    }
}

/// Whether a track item is footage (it can start a shot) rather than something laid over one.
fn is_footage(p: &Project, it: &TrackItem) -> bool {
    it.enabled
        && it.duration > Tick::ZERO
        && p.item(it.item).is_some_and(|i| matches!(i.kind, ItemKind::Media(_) | ItemKind::Subclip { .. } | ItemKind::Sequence(_)))
}

/// Whether a clip shows only part of a picture, so that it rides on a shot rather than being one
/// while there is a whole picture on screen:
///
/// - a clip masked down to a region (an Opacity mask): a layer over a shot, or a shot with a piece
///   taken out;
/// - footage with an alpha channel: a logo, a watermark, a lower third rendered elsewhere;
/// - a clip placed over less than half the frame: a picture in a picture, a bug in a corner.
///   (Letterboxed footage covers three quarters of the frame and more, and is a shot.)
fn is_partial(p: &Project, q: &Sequence, it: &TrackItem) -> bool {
    if it.effects.iter().any(|e| e.effect == "opacity" && e.enabled && e.masks.iter().any(|m| m.mode != filmagic_project::MaskMode::None)) {
        return true;
    }
    if p.resolve_media(it.item).is_some_and(|(_, m, _)| m.info.video.as_ref().is_some_and(|v| v.has_alpha)) {
        return true;
    }
    let Some(size) = filmagic_render::source_size(p, it.item).filter(|(w, h)| *w > 0 && *h > 0) else { return false };
    let placed = filmagic_render::motion_matrix(q, it, size, filmagic_render::source_par(p, it.item), it.effect_time_at(it.start));
    let frame = f64::from(q.settings.width.max(1)) * f64::from(q.settings.height.max(1));
    let covered = placed.determinant().abs() * f64::from(size.0) * f64::from(size.1);
    covered.is_finite() && covered < 0.5 * frame
}

/// The shots of a sequence, in order, without leading or trailing black.
///
/// The clip of a shot is the topmost one showing a whole picture. What shows part of one
/// ([`is_partial`]: a layer made by `layers.extract`, `layers.lift` or `layers.retime`, a logo
/// with an alpha channel, a picture in a picture) rides on the shot under it and neither starts
/// nor ends one; with nothing whole on screen, the lowest such clip is the shot.
pub fn shots(p: &Project, q: &Sequence) -> Result<Vec<Shot>> {
    // top track first: the clip seen is the topmost one
    let layers: Vec<Vec<(Tick, Tick, ClipId, bool)>> = q
        .video_tracks
        .iter()
        .rev()
        .filter(|t| t.enabled)
        .map(|t| {
            let mut v: Vec<(Tick, Tick, ClipId, bool)> =
                t.items.iter().filter(|it| is_footage(p, it)).map(|it| (it.start, it.end(), it.id, is_partial(p, q, it))).collect();
            v.sort_by_key(|c| c.0);
            v
        })
        .collect();
    if layers.iter().map(Vec::len).sum::<usize>() > MAX_PICTURE_CLIPS {
        return Err(EngineError::Other(format!("the sequence has more than {MAX_PICTURE_CLIPS} picture clips; its shots are not read")));
    }
    let bounds: BTreeSet<Tick> = layers.iter().flatten().flat_map(|c| [c.0, c.1]).collect();
    let bounds: Vec<Tick> = bounds.into_iter().collect();
    let mut at = vec![0usize; layers.len()];
    let mut out: Vec<Shot> = Vec::new();
    for w in bounds.windows(2) {
        let (Some(&a), Some(&b)) = (w.first(), w.get(1)) else { continue };
        let (mut top, mut part) = (None, None);
        for (layer, i) in layers.iter().zip(at.iter_mut()) {
            while layer.get(*i).is_some_and(|c| c.1 <= a) {
                *i += 1;
            }
            match layer.get(*i).filter(|c| c.0 <= a) {
                Some(c) if c.3 => part = Some(c.2),
                Some(c) => {
                    top = Some(c.2);
                    break;
                }
                None => {}
            }
        }
        let top = top.or(part);
        match out.last_mut() {
            Some(last) if last.clip == top && last.end == a => last.end = b,
            _ => out.push(Shot { start: a, end: b, clip: top }),
        }
    }
    while out.last().is_some_and(|s| s.clip.is_none()) {
        out.pop();
    }
    let lead = out.iter().take_while(|s| s.clip.is_none()).count();
    out.drain(..lead);
    Ok(out)
}

/// The cut times of a list of shots (where one ends and the next begins).
fn cut_times(shots: &[Shot]) -> Vec<Tick> {
    shots.iter().skip(1).map(|s| s.start).collect()
}

fn active<'a>(s: &'a Session, cmd: &str) -> Result<(ItemId, &'a Sequence)> {
    let id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let q = s.project.sequence(id).ok_or_else(|| bad(cmd, "no active sequence"))?;
    Ok((id, q))
}

fn clip_name(q: &Sequence, c: Option<ClipId>) -> Value {
    c.and_then(|c| q.find_item(c)).map_or(Value::Null, |(_, it)| json!(it.name))
}

fn rhythm_json(shots: &[Shot]) -> Value {
    let lengths: Vec<f64> = shots.iter().map(Shot::seconds).collect();
    let r = rhythm(&lengths);
    let pace = Pace::of(r.cuts_per_minute);
    json!({
        "shots": r.shots,
        "cuts": r.cuts,
        "seconds": round3(r.seconds),
        "cutsPerMinute": round3(r.cuts_per_minute),
        "pace": pace.label(),
        "paceNote": pace.note(),
        "shotSeconds": {"mean": round3(r.mean), "median": round3(r.median), "shortest": round3(r.shortest), "longest": round3(r.longest), "variation": round3(r.variation)},
    })
}

fn rhythm_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "story.rhythm";
    let (_, q) = active(s, CMD)?;
    let rate = q.frame_rate();
    let window = num_p(p, "window", CMD)?.unwrap_or(10.0);
    if !window.is_finite() || !(1.0..=3600.0).contains(&window) {
        return Err(bad(CMD, "`window` must be between 1 and 3600 seconds"));
    }
    let flash_frames = match p.get("flashFrames") {
        None | Some(Value::Null) => 8,
        Some(_) => u64_p(p, "flashFrames").filter(|n| (1..=1000).contains(n)).ok_or_else(|| bad(CMD, "`flashFrames` must be between 1 and 1000"))? as i64,
    };
    let all = shots(&s.project, q)?;
    let mut out = rhythm_json(&all);
    let Some((first, last)) = all.first().zip(all.last()) else {
        out["notes"] = json!(["the sequence has no picture clips"]);
        return Ok(out);
    };
    let (t0, t1) = (first.start, last.end);
    let cuts = cut_times(&all);
    // cut rate over time: how the pace moves through the film
    let total = (t1 - t0).seconds();
    // at most 600 windows: a longer film gets wider ones, and the result says how wide
    let window = window.max(total / 600.0);
    // A tail shorter than half a window is counted with the window before it: on its own, one
    // cut in its second and a half would read as forty a minute.
    let whole = (total / window).floor();
    let tail = total - whole * window;
    let n_windows = (whole as usize + usize::from(tail >= window / 2.0 || whole < 1.0)).clamp(1, 600);
    let mut counts = vec![0u32; n_windows];
    for c in &cuts {
        let i = (((*c - t0).seconds() / window) as usize).min(n_windows - 1);
        if let Some(k) = counts.get_mut(i) {
            *k += 1;
        }
    }
    let windows: Vec<Value> = counts
        .iter()
        .enumerate()
        .map(|(i, k)| {
            let len = if i + 1 == n_windows { total - i as f64 * window } else { window }.max(1e-6);
            let cpm = f64::from(*k) * 60.0 / len;
            json!({"startSeconds": round3(t0.seconds() + i as f64 * window), "seconds": round3(len), "cuts": k, "cutsPerMinute": round3(cpm), "pace": Pace::of(cpm).label()})
        })
        .collect();
    let fd = rate.frame_duration().0.max(1);
    let flashes: Vec<Value> = all
        .iter()
        .enumerate()
        .filter(|(_, sh)| sh.clip.is_some() && (sh.end - sh.start).0 / fd < flash_frames)
        .take(MAX_LISTED)
        .map(|(i, sh)| json!({"shot": i + 1, "start": time_json(sh.start, rate), "frames": (sh.end - sh.start).0 / fd, "clip": sh.clip.map(|c| c.0)}))
        .collect();
    let mut notes: Vec<String> = Vec::new();
    let variation = out["shotSeconds"]["variation"].as_f64().unwrap_or(0.0);
    if all.len() >= 6 && variation < 0.15 {
        notes.push("the shots are all nearly the same length: a metronome. Let the content of each shot decide how long it holds".into());
    }
    let frantic = windows.iter().filter(|w| w["pace"] == "frantic").count();
    if frantic > 0 {
        notes.push(format!(
            "{frantic} window(s) cut faster than 40 a minute, beyond the range of blink rates: shots there are felt more than read. A montage, a trailer or a music video is cut that way on purpose; a scene meant to be followed is not"
        ));
    }
    if !flashes.is_empty() {
        notes.push(format!("{} shot(s) are shorter than {flash_frames} frames: flashes, seen but barely read", flashes.len()));
    }
    if all.iter().any(|sh| sh.clip.is_none()) {
        notes.push("the picture goes to black between some clips; each gap is counted as a shot".into());
    }
    out["windowSeconds"] = json!(window);
    out["windows"] = json!(windows);
    out["flashes"] = json!(flashes);
    out["blinkRange"] = json!({"cutsPerMinute": [4, 40], "dialogue": 6, "action": 25});
    out["truncated"] = json!(all.len() > MAX_LISTED);
    out["shotList"] = all
        .iter()
        .enumerate()
        .take(MAX_LISTED)
        .map(|(i, sh)| {
            json!({"shot": i + 1, "start": time_json(sh.start, rate), "end": time_json(sh.end, rate), "seconds": round3(sh.seconds()), "clip": sh.clip.map(|c| c.0), "name": clip_name(q, sh.clip)})
        })
        .collect();
    out["notes"] = json!(notes);
    Ok(out)
}

/// What a frame of the programme looks like, reduced.
struct Look {
    glance: Glance,
    sig: Signature,
    /// The frame drew the offline slate for some clip: it is not the footage.
    offline: bool,
}

fn look(s: &Session, scale: f32, t: Tick) -> Result<Look> {
    let (img, offline) = s.try_render_program_with_offline_at(scale, t)?;
    let rgba = img.over_black_rgba8();
    Ok(Look { glance: Glance::from_rgba(&rgba, img.w, img.h), sig: signature(&rgba, img.w, img.h), offline: !offline.is_empty() })
}

fn analysis_scale(q: &Sequence) -> f32 {
    (ANALYSIS_SIDE / q.settings.width.max(q.settings.height).max(1) as f32).min(1.0)
}

/// Whether the programme's sound pauses at `t`: `Some(true)` in a pause, `Some(false)` when sound
/// carries through, `None` when there is no sound around `t` to tell.
fn sound_at(s: &Session, q: &Sequence, t: Tick) -> Option<bool> {
    let sr = q.settings.sample_rate.clamp(8_000, 192_000);
    let at = t.to_units_floor(i64::from(sr));
    let half = i64::from(sr) * 6 / 10;
    let start = (at - half).max(0);
    let frames = usize::try_from(at + half - start).ok()?;
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let mix = filmagic_render::audio::mix_sequence(&s.project, q, start, frames, &provider);
    let n = mix.frames();
    let chans = mix.channels.len().max(1) as f32;
    let mono: Vec<f32> = (0..n).map(|i| mix.channels.iter().map(|c| c.get(i).copied().unwrap_or(0.0)).sum::<f32>() / chans).collect();
    let db = filmagic_render::story::levels_db(&mono, sr);
    // 10 ms steps; the cut sits at step `cut`
    let cut = usize::try_from((at - start) / i64::from(sr / 100).max(1)).ok()?;
    let before = db.get(cut.saturating_sub(55)..cut.saturating_sub(8))?;
    let loud = before.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    if !loud.is_finite() || loud < -60.0 {
        return None;
    }
    let near = db.get(cut.saturating_sub(6)..(cut + 6).min(db.len()))?;
    let here = near.iter().sum::<f32>() / near.len().max(1) as f32;
    Some(here <= loud - 12.0)
}

/// The clip whose sound belongs to a shot: the shot's own clip when it has linked sound, else the
/// linked picture clip it stands in for. A plate or a layer made from a clip covers exactly that
/// clip and has no sound of its own; the sound is still the clip's underneath. (A cutaway laid
/// over part of a clip is another matter: it is a shot of its own, with no sound.)
fn sound_carrier(q: &Sequence, clip: Option<ClipId>) -> Option<ClipId> {
    let c = clip?;
    let Some((_, top)) = q.find_item(c) else { return Some(c) };
    if top.link.is_some() {
        return Some(c);
    }
    q.video_tracks
        .iter()
        .rev()
        .filter(|t| t.enabled)
        .flat_map(|t| t.items.iter())
        .find(|it| it.enabled && it.link.is_some() && it.start == top.start && it.end() == top.end())
        .map(|it| it.id)
        .or(Some(c))
}

/// Whether `b` carries straight on from `a` on the same track: the next stretch of the same
/// recording, cut but not moved (a through edit). Nothing changes there for eye or ear.
fn carries_on(a: &TrackItem, b: &TrackItem) -> bool {
    a.item == b.item
        && a.end() == b.start
        && a.speed == b.speed
        && !a.reverse
        && !b.reverse
        && a.frame_hold.is_none()
        && b.frame_hold.is_none()
        && a.audio_stream == b.audio_stream
        && a.source_out() == b.source_in
}

/// The stretches of unbroken sound linked to a picture clip, one for each audio track that holds
/// some (a lavalier on one track and a boom on another are both its sound): on each, the linked
/// clip extended through any through edits before and after it. (track index, start, end)
fn sound_spans(q: &Sequence, clip: ClipId) -> Vec<(usize, Tick, Tick)> {
    let Some(link) = q.find_item(clip).and_then(|(_, it)| it.link) else { return Vec::new() };
    let mut out = Vec::new();
    for (ti, track) in q.audio_tracks.iter().enumerate() {
        let mut items: Vec<&TrackItem> = track.items.iter().collect();
        items.sort_by_key(|it| it.start);
        let Some(at) = items.iter().position(|it| it.link == Some(link)) else { continue };
        let (mut first, mut last) = (at, at);
        while first > 0 && items.get(first - 1).zip(items.get(first)).is_some_and(|(a, b)| carries_on(a, b)) {
            first -= 1;
        }
        while items.get(last).zip(items.get(last + 1)).is_some_and(|(a, b)| carries_on(a, b)) {
            last += 1;
        }
        if let (Some(a), Some(b)) = (items.get(first), items.get(last)) {
            out.push((ti, a.start, b.end()));
        }
    }
    out
}

/// How sound and picture change at a cut from `out` to `inc` at `t`: together ("straight"), sound
/// trailing the picture ("L"), sound leading it ("J"), both ("L+J"), one unbroken recording
/// running under the cut ("continuous"), or `None` without linked sound.
fn split_state(q: &Sequence, out: Option<ClipId>, inc: Option<ClipId>, t: Tick, half_frame: Tick) -> Option<&'static str> {
    let (a, b) = (out.map(|c| sound_spans(q, c)).unwrap_or_default(), inc.map(|c| sound_spans(q, c)).unwrap_or_default());
    if a.is_empty() && b.is_empty() {
        return None;
    }
    // a stretch both sides share is one recording running under the cut: nothing changes there for the ear
    let under = |x: &(usize, Tick, Tick)| x.1 < t - half_frame && x.2 > t + half_frame;
    if !a.is_empty() && a.len() == b.len() && a.iter().all(|x| under(x) && b.contains(x)) {
        return Some("continuous");
    }
    let trailing = a.iter().any(|x| x.2 > t + half_frame && !b.contains(x));
    let leading = b.iter().any(|x| x.1 < t - half_frame && !a.contains(x));
    Some(match (trailing, leading) {
        (false, false) => "straight",
        (true, false) => "L",
        (false, true) => "J",
        (true, true) => "L+J",
    })
}

/// The video transition the cut from `out` to `inc` at `t` sits in, if any: (effect id, start,
/// end). A transition on some other clip that merely spans the same time (a title fading in on a
/// higher track) is not this cut's.
fn transition_at(q: &Sequence, t: Tick, out: Option<ClipId>, inc: Option<ClipId>) -> Option<(String, Tick, Tick)> {
    q.video_tracks
        .iter()
        .flat_map(|tr| tr.transitions.iter())
        .find(|x| x.start <= t && t <= x.end() && ((x.from.is_some() && x.from == out) || (x.to.is_some() && x.to == inc)))
        .map(|x| (x.effect.effect.clone(), x.start, x.end()))
}

/// A caller's 0…1 judgement under `key`, if given.
fn judgement(p: &Value, key: &str, cmd: &str) -> Result<Option<f64>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_f64().filter(|x| (0.0..=1.0).contains(x)).map(Some).ok_or_else(|| bad(cmd, format!("`{key}` must be a number from 0 to 1"))),
    }
}

/// The evidence about one cut.
struct CutContext<'a> {
    q: &'a Sequence,
    rate: FrameRate,
    scale: f32,
    before: &'a Shot,
    after: &'a Shot,
    emotion: Option<f64>,
    story: Option<f64>,
}

fn analyse_cut(s: &Session, cx: &CutContext<'_>) -> Result<Value> {
    let CutContext { q, rate, scale, before, after, emotion, story } = *cx;
    let t = after.start;
    let fd = rate.frame_duration();
    let half = Tick(fd.0 / 2);
    // two frames each side: the last two outgoing and the first two incoming
    // Inside a transition neighbouring frames are blends of both shots and look alike by design:
    // the shots are compared where the transition has not begun and where it has ended.
    let transition = transition_at(q, t, before.clip, after.clip);
    let (leave, arrive) = transition.as_ref().map_or((t, t), |(_, a, b)| ((*a).max(before.start + fd).min(t), (*b).min(after.end - fd).max(t)));
    // The programme is seen frame by frame on the sequence's grid, and a cut need not sit on it (a
    // clip re-timed to 66.67 frames, a trim by ticks): the last frame of the outgoing shot is the
    // one on screen just before the cut, the first of the incoming shot the first at or after it.
    let last_out = frame_before(rate, leave);
    let first_in = Some(frame_from(rate, arrive)).filter(|g| *g < after.end).unwrap_or(arrive);
    let out1 = look(s, scale, last_out)?;
    let out0 = look(s, scale, Some(last_out - fd).filter(|g| *g >= before.start).unwrap_or(last_out))?;
    let in0 = look(s, scale, first_in)?;
    let in1 = look(s, scale, Some(first_in + fd).filter(|g| *g < after.end).unwrap_or(first_in))?;
    let offline = out1.offline || in0.offline;
    let diff = difference(&out1.sig, &in0.sig);
    let likeness = filmagic_render::scene::likeness(&out1.sig, &in0.sig);
    let change = Change::of(diff);
    let (eye_out, eye_in): (Attention, Attention) = (attention(&out1.glance, Some(&out0.glance)), attention(&in0.glance, Some(&in1.glance)));
    let travel = (eye_out.strength > 0.0 && eye_in.strength > 0.0).then(|| eye_travel(eye_out, eye_in, display_aspect(q)));
    let (dir_out, dir_in) = (screen_direction(drift(&out0.glance, &out1.glance)), screen_direction(drift(&in0.glance, &in1.glance)));
    let reverses = dir_out != 0 && dir_in != 0 && dir_out != dir_in;
    let pause = sound_at(s, q, t);
    // a layer or a plate over a shot has no sound of its own: the sound is the clip's underneath
    let split = split_state(q, sound_carrier(q, before.clip), sound_carrier(q, after.clip), t, half);
    let frames = |sh: &Shot| (sh.end - sh.start).0 / fd.0.max(1);
    let flash = frames(before).min(frames(after)) < 8;

    let mut issues: Vec<String> = Vec::new();
    if offline {
        issues.push("media is offline at this cut: what was compared is the offline slate, not the footage. Relink the media and ask again".into());
    }
    match change {
        // to or from black there is no second picture to jump to
        Change::Jump if before.clip.is_none() || after.clip.is_none() => {}
        // the same picture, shifted: a jump cut for certain
        Change::Jump if likeness.is_some_and(|l| l >= Change::SAME_PICTURE_FROM) => issues.push(
            "jump cut: the picture changes too little to read as a new shot and too much to pass as the same one; cut to a clearly different framing, or do not cut".into(),
        ),
        // Little differs in sum, yet what is where has changed, or a frame is too even to say (fog,
        // near black). Two frames cannot tell a subject that hopped on a plain backdrop from a new
        // shot in the same tones (two night exteriors).
        Change::Jump => issues.push(
            "the picture changes little in sum, but it is not plainly the same picture shifted: a jump cut on a plain background, or simply a new shot in the same tones. Look at the two frames".into(),
        ),
        Change::Match if before.clip.is_some() && after.clip.is_some() => {
            issues.push("the picture barely changes: either a deliberate match cut or a cut that is not needed".into());
        }
        _ => {}
    }
    if let Some(d) = travel.filter(|d| *d > 0.35) {
        issues.push(format!("the eye has to travel {:.0}% of the frame diagonal to find the new centre of interest", d * 100.0));
    }
    if reverses {
        issues.push("screen direction reverses: movement that ran one way before the cut runs the other way after it".into());
    }
    if flash {
        issues.push(format!("a shot beside this cut is only {} frame(s) long", frames(before).min(frames(after))));
    }
    if split == Some("straight") && pause == Some(true) {
        issues.push(
            "picture and sound both cut on the pause after a phrase: fine once, mechanical as a habit. `story.splitEdit` lets the sound lead or trail the picture".into(),
        );
    }

    // the measurable criteria, 0 (works against the cut) … 1 (supports it); null = not measurable here
    let rhythm_value = match (flash, pause) {
        (true, _) => 0.4,
        (false, Some(true)) => 1.0,
        (false, Some(false)) => 0.7,
        (false, None) => 0.85,
    };
    let eye_value = travel.map(|d| f64::from((1.0 - d / 0.5).clamp(0.0, 1.0)));
    let plane_value = (dir_out != 0 || dir_in != 0).then_some(if reverses { 0.0 } else { 1.0 });
    let values: [Option<f64>; 6] = [emotion, story, Some(rhythm_value), eye_value, plane_value, None];
    let (mut weighted, mut covered) = (0.0, 0.0);
    let mut six = serde_json::Map::new();
    for ((name, weight), value) in SIX.iter().zip(values) {
        if let Some(v) = value {
            weighted += weight * v;
            covered += weight;
        }
        six.insert((*name).to_string(), json!({"weight": weight, "value": value.map(round3)}));
    }
    six.insert("score".into(), if covered > 0.0 { json!(round3(weighted / covered)) } else { Value::Null });
    six.insert("covers".into(), json!(covered));
    six.insert(
        "note".into(),
        json!(if emotion.is_some() && story.is_some() {
            "score: the weighted mean of the criteria with a value, yours included"
        } else {
            "score covers only the measurable criteria. Emotion (51%) and story (23%) outweigh them all: look at the frames and pass `emotion` and `story` (0 to 1) for this cut to complete it. Never give up emotion before story, or story before rhythm"
        }),
    );
    Ok(json!({
        "at": time_json(t, rate),
        "from": {"clip": before.clip.map(|c| c.0), "name": clip_name(q, before.clip), "seconds": round3(before.seconds())},
        "to": {"clip": after.clip.map(|c| c.0), "name": clip_name(q, after.clip), "seconds": round3(after.seconds())},
        "change": {"difference": round3(f64::from(diff)), "likeness": likeness.map(|l| round3(f64::from(l))), "kind": change.label()},
        "eye": {
            "out": [round3(f64::from(eye_out.x)), round3(f64::from(eye_out.y))],
            "in": [round3(f64::from(eye_in.x)), round3(f64::from(eye_in.y))],
            "travel": travel.map(|d| round3(f64::from(d))),
        },
        "direction": {"out": dir_out, "in": dir_in, "reverses": reverses},
        "light": {"out": round3(f64::from(out1.glance.mean())), "in": round3(f64::from(in0.glance.mean()))},
        "sound": {"pause": pause, "edit": split},
        "transition": transition.map(|x| x.0),
        "offline": offline,
        "six": six,
        "issues": issues,
    }))
}

fn cuts_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "story.cuts";
    let (_, q) = active(s, CMD)?;
    let rate = q.frame_rate();
    let all = shots(&s.project, q)?;
    let limit = match p.get("limit") {
        None | Some(Value::Null) => 60,
        Some(_) => u64_p(p, "limit")
            .filter(|n| (1..=MAX_LISTED as u64).contains(n))
            .ok_or_else(|| bad(CMD, format!("`limit` must be between 1 and {MAX_LISTED}")))? as usize,
    };
    let (emotion, story) = (judgement(p, "emotion", CMD)?, judgement(p, "story", CMD)?);
    let bound = |key: &str| -> Result<Option<Tick>> {
        match p.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v
                .as_f64()
                .filter(|x| x.is_finite() && (0.0..=1e7).contains(x))
                .map(|x| Some(Tick::from_seconds_f64(x)))
                .ok_or_else(|| bad(CMD, format!("`{key}` must be seconds from 0"))),
        }
    };
    let (from, to) = (bound("fromSeconds")?, bound("toSeconds")?);
    // pairs of shots around each cut
    let mut pairs: Vec<(&Shot, &Shot)> = all.windows(2).filter_map(|w| Some((w.first()?, w.get(1)?))).collect();
    let total = pairs.len();
    let nearest = nearest_p(s, p, CMD)?;
    if let Some(t) = nearest {
        let best = pairs.iter().copied().min_by_key(|(_, b)| (b.start - t).abs().0);
        pairs = best.into_iter().collect();
    } else {
        if emotion.is_some() || story.is_some() {
            return Err(bad(CMD, "`emotion` and `story` judge one cut: say which with `time`, `seconds` or `frame`"));
        }
        pairs.retain(|(_, b)| from.is_none_or(|f| b.start >= f) && to.is_none_or(|x| b.start <= x));
    }
    let matched = pairs.len();
    pairs.truncate(limit);
    let scale = analysis_scale(q);
    let mut cuts = Vec::with_capacity(pairs.len());
    for (before, after) in &pairs {
        cuts.push(analyse_cut(s, &CutContext { q, rate, scale, before, after, emotion, story })?);
    }
    let count = |f: &dyn Fn(&Value) -> bool| cuts.iter().filter(|c| f(c)).count();
    let straight_on_pause = count(&|c| c["sound"]["edit"] == "straight" && c["sound"]["pause"] == true);
    let with_sound = count(&|c| c["sound"]["edit"].is_string());
    let mut notes: Vec<String> = Vec::new();
    if with_sound >= 4 && straight_on_pause * 4 >= with_sound * 3 {
        notes.push(
            "most cuts change picture and sound together on the pause after a line: the audience sees only who is speaking, never who is listening. Split some edits (`story.splitEdit`) so a cut can land on a reaction".into(),
        );
    }
    // Just past the last cut analysed, rounded up: the cut after a one-frame shot is one frame
    // on, and a value rounded to the millisecond can land beyond it.
    let next = (matched > pairs.len()).then(|| pairs.last().map(|(_, b)| ((b.start + Tick(1)).seconds() * 1e6).ceil() / 1e6)).flatten();
    Ok(json!({
        "cutsInSequence": total,
        "analysed": cuts.len(),
        "truncated": matched > cuts.len(),
        "nextFromSeconds": next,
        "summary": {
            "jump": count(&|c| c["change"]["kind"] == "jump"),
            "match": count(&|c| c["change"]["kind"] == "match"),
            "clean": count(&|c| c["change"]["kind"] == "clean"),
            "directionReversals": count(&|c| c["direction"]["reverses"] == true),
            "offline": count(&|c| c["offline"] == true),
            "straightCutsOnAPause": straight_on_pause,
            "continuousSound": count(&|c| c["sound"]["edit"] == "continuous"),
            "splitEdits": count(&|c| matches!(c["sound"]["edit"].as_str(), Some("L" | "J" | "L+J"))),
        },
        "weights": SIX.iter().map(|(n, w)| json!({"criterion": n, "weight": w})).collect::<Vec<_>>(),
        "notes": notes,
        "cuts": cuts,
    }))
}

/// A stretch of one piece of footage to look for cut points in.
struct Probe {
    /// Analysed span in the caller's time (timeline time for a clip, media time for an item).
    from: Tick,
    to: Tick,
    /// How far before `from` the footage reaches (at most two seconds): sound read as context, so
    /// a phrase that began before the span is still known to be a phrase.
    lead: Tick,
    rate: FrameRate,
    /// Mono samples of `[start, start + frames)` at `sample_rate`, in the caller's time.
    sound: Option<Box<dyn Fn(i64, usize) -> Option<Vec<f32>>>>,
    /// Where there is sound to read, in the caller's time. A clip's sound can be shorter than its
    /// picture or cut at another time (a split edit); outside this there is nothing, and nothing
    /// is not a pause.
    sound_span: (Tick, Tick),
    sample_rate: u32,
    /// RGBA8 frame (pixels, width, height) at a caller's time.
    picture: Option<Box<dyn Fn(Tick) -> Option<(Vec<u8>, usize, usize)>>>,
    /// Media time of a caller's time, when they differ.
    media_time: Option<Box<dyn Fn(Tick) -> Tick>>,
}

/// A candidate cut point.
struct BlinkPoint {
    t: Tick,
    kind: &'static str,
    strength: f64,
    pause: Option<(Tick, Tick)>,
}

/// The cut points the sound gives, and whether any of it could be read.
fn phrase_points(pr: &Probe, min_pause: f64) -> (Vec<BlinkPoint>, bool) {
    let Some(sound) = &pr.sound else { return (Vec::new(), false) };
    let sr = pr.sample_rate;
    let step = filmagic_render::story::level_step(sr);
    let origin = (pr.from - pr.lead).max(pr.sound_span.0);
    let (a, b) = (origin.to_units_floor(i64::from(sr)), pr.to.min(pr.sound_span.1).to_units_floor(i64::from(sr)));
    let mut heard = false;
    // five seconds at a time, in whole level steps, so a long clip never sits in memory at once
    let chunk = (sr as usize * 5 / step).max(1) * step;
    let mut voice_band = filmagic_render::story::VoiceBand::new(sr);
    let (mut whole, mut voice): (Vec<f32>, Vec<f32>) = (Vec::new(), Vec::new());
    let mut at = a;
    while at < b {
        let n = usize::try_from(b - at).unwrap_or(0).min(chunk);
        let n = n / step * step;
        if n == 0 {
            break;
        }
        let read = sound(at, n);
        heard |= read.is_some();
        let mono = read.unwrap_or_else(|| vec![0.0; n]);
        whole.extend(filmagic_render::story::levels_db(&mono, sr));
        voice.extend(voice_band.levels_db(&mono));
        at += n as i64;
    }
    let step_s = step as f64 / f64::from(sr);
    let fd = pr.rate.frame_duration();
    // Speech over an engine, wind or surf shows its pauses only in the band a voice lives in;
    // sound with nothing there (a drum, a door) is read whole.
    let mut found = filmagic_render::story::pauses_in_levels(&voice, step_s, min_pause, 0.4);
    if found.is_empty() {
        found = filmagic_render::story::pauses_in_levels(&whole, step_s, min_pause, 0.4);
    }
    let points = found
        .into_iter()
        .map(|pz| {
            let (ps, pe) = (origin + Tick::from_seconds_f64(pz.start), origin + Tick::from_seconds_f64(pz.end));
            // just after the phrase ends: two frames in, never past the middle of the pause
            let t = pr.rate.snap((ps + fd + fd).min(ps + Tick((pe - ps).0 / 2)));
            let strength = 0.6 * ((pz.end - pz.start) / 1.0).clamp(0.0, 1.0) + 0.4 * (f64::from(pz.drop_db) / 30.0).clamp(0.0, 1.0);
            BlinkPoint { t, kind: "phrase", strength, pause: Some((ps, pe)) }
        })
        .collect();
    (points, heard)
}

/// How many looks `step` apart fit in `span`: every one of them, whatever the frame rate (the
/// span is at most [`MAX_BLINK_SECONDS`] long, and a look is never less than a frame apart).
pub(crate) fn looks_in(span: Tick, step: Tick) -> i64 {
    (span.0 / step.0.max(1)).clamp(0, (MAX_BLINK_SECONDS * 240.0) as i64)
}

/// The cut points the picture gives, and whether any frame could be read.
fn movement_points(pr: &Probe) -> (Vec<BlinkPoint>, bool) {
    let Some(picture) = &pr.picture else { return (Vec::new(), false) };
    let fd = pr.rate.frame_duration();
    // about eight looks a second are enough to see a movement start and stop
    let stride = ((pr.rate.as_f64() / 8.0).round() as i64).clamp(1, 60);
    let step = Tick(fd.0.saturating_mul(stride).max(1));
    let n = looks_in(pr.to - pr.from, step);
    let mut prev: Option<Signature> = None;
    let mut activity: Vec<f32> = Vec::with_capacity(n as usize);
    for i in 0..n {
        let sig = picture(pr.from + Tick(step.0.saturating_mul(i))).map(|(rgba, w, h)| signature(&rgba, w, h));
        activity.push(match (&prev, &sig) {
            (Some(a), Some(b)) => difference(a, b),
            _ => 0.0,
        });
        if sig.is_some() {
            prev = sig;
        }
    }
    let points = settles(&activity, (2.5 * pr.rate.as_f64() / stride as f64).round().max(2.0) as usize)
        .into_iter()
        .map(|(i, strength)| BlinkPoint { t: pr.from + Tick(step.0.saturating_mul(i as i64)), kind: "movement", strength: f64::from(strength), pause: None })
        .collect();
    (points, prev.is_some())
}

/// What a probe found: the points, and whether its sound and its picture could be read at all.
struct Found {
    points: Vec<BlinkPoint>,
    heard: bool,
    seen: bool,
}

fn find_blink_points(pr: &Probe, min_pause: f64) -> Found {
    let (mut points, heard) = phrase_points(pr, min_pause);
    let near = Tick(pr.rate.frame_duration().0.saturating_mul(4));
    let (moves, seen) = movement_points(pr);
    for m in moves {
        // a movement that completes as a phrase ends is one point, and a stronger one
        match points.iter_mut().find(|x| x.kind != "movement" && (x.t - m.t).abs() <= near) {
            Some(x) => {
                x.kind = "both";
                x.strength = (x.strength.max(m.strength) + 0.2).min(1.0);
            }
            None => points.push(m),
        }
    }
    points.sort_by_key(|x| x.t);
    points.retain(|x| x.t > pr.from && x.t < pr.to);
    Found { points, heard, seen }
}

fn mono(channels: &[Vec<f32>], n: usize) -> Vec<f32> {
    let k = channels.len().max(1) as f32;
    (0..n).map(|i| channels.iter().map(|c| c.get(i).copied().unwrap_or(0.0)).sum::<f32>() / k).collect()
}

fn seconds_bound(p: &Value, key: &str, cmd: &str) -> Result<Option<Tick>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_f64()
            .filter(|x| x.is_finite() && (0.0..=1e7).contains(x))
            .map(|x| Some(Tick::from_seconds_f64(x)))
            .ok_or_else(|| bad(cmd, format!("`{key}` must be seconds from 0"))),
    }
}

/// The span to analyse inside `[lo, hi)`: `fromSeconds`…`toSeconds` when given, refused when it
/// is empty or longer than [`MAX_BLINK_SECONDS`].
fn blink_span(p: &Value, lo: Tick, hi: Tick, cmd: &str) -> Result<(Tick, Tick)> {
    let from = seconds_bound(p, "fromSeconds", cmd)?.map_or(lo, |t| t.clamp(lo, hi));
    let to = seconds_bound(p, "toSeconds", cmd)?.map_or(hi, |t| t.clamp(lo, hi));
    if to <= from {
        return Err(bad(cmd, "nothing to analyse between `fromSeconds` and `toSeconds`"));
    }
    if (to - from).seconds() > MAX_BLINK_SECONDS {
        return Err(bad(
            cmd,
            format!(
                "{:.0} s is too long to analyse in one call; pass `fromSeconds` and `toSeconds` at most {MAX_BLINK_SECONDS:.0} s apart",
                (to - from).seconds()
            ),
        ));
    }
    Ok((from, to))
}

fn frame_scale(src: &filmagic_media::SharedSource) -> f32 {
    src.info().video.as_ref().map_or(1.0, |v| (ANALYSIS_SIDE / v.width.max(v.height).max(1) as f32).min(1.0))
}

/// A probe for a timeline clip: its own picture, and its (or its linked partner's) sound.
fn clip_probe(s: &Session, p: &Value, clip: ClipId, picture: bool) -> Result<Probe> {
    const CMD: &str = "story.blinkPoints";
    let (_, q) = active(s, CMD)?;
    let (track, it) = q.find_item(clip).ok_or_else(|| bad(CMD, format!("no clip {} in the active sequence", clip.0)))?;
    let is_video = q.track(track).is_some_and(|t| t.kind == TrackKind::Video);
    let partners = |kind: TrackKind| -> Vec<&TrackItem> {
        let Some(link) = it.link else { return Vec::new() };
        q.tracks(kind).iter().flat_map(|t| t.items.iter()).filter(|x| x.link == Some(link) && x.id != it.id).collect()
    };
    // a picture clip is heard through every sound clip linked to it (a lavalier and a boom are both its sound)
    let (video, audio) = if is_video { (Some(it), partners(TrackKind::Audio)) } else { (partners(TrackKind::Video).first().copied(), vec![it]) };
    let (from, to) = blink_span(p, it.start, it.end(), CMD)?;
    let sr = q.settings.sample_rate.clamp(8_000, 192_000);
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let audible: Vec<TrackItem> = audio.into_iter().filter(|a| s.source(a.item).is_some_and(|src| src.info().has_audio())).cloned().collect();
    // only where all of them have sound: beyond a clip's own edge its signal is nothing at all
    let sound_span = (audible.iter().map(|a| a.start).max().unwrap_or(from), audible.iter().map(TrackItem::end).min().unwrap_or(to));
    let sound = (!audible.is_empty() && sound_span.0.max(from) < sound_span.1.min(to)).then(|| {
        Box::new(move |start: i64, frames: usize| {
            let mut sum: Option<Vec<f32>> = None;
            for a in &audible {
                let Some(ch) = filmagic_render::audio::clip_signal(a, start, frames, sr, &provider) else { continue };
                let one = mono(&ch, frames);
                match &mut sum {
                    Some(acc) => acc.iter_mut().zip(one).for_each(|(x, y)| *x += y),
                    None => sum = Some(one),
                }
            }
            sum
        }) as Box<dyn Fn(i64, usize) -> Option<Vec<f32>>>
    });
    let picture = video.filter(|_| picture).and_then(|v| {
        let src = s.source(v.item)?;
        src.info().video.as_ref()?;
        let (v, scale) = (v.clone(), frame_scale(&src));
        Some(Box::new(move |t: Tick| {
            // only where the picture clip is on the timeline: after a split edit the sound runs
            // past it, over media the picture clip does not show
            if t < v.start || t >= v.end() {
                return None;
            }
            let f = src.video_frame(filmagic_media::FrameRequest { time: v.source_time_at(t), scale }).ok()?;
            Some((f.to_rgba8(), f.width as usize, f.height as usize))
        }) as Box<dyn Fn(Tick) -> Option<(Vec<u8>, usize, usize)>>)
    });
    let media = it.clone();
    Ok(Probe {
        from,
        to,
        lead: (from - it.start).clamp(Tick::ZERO, Tick::from_seconds_f64(2.0)),
        rate: q.frame_rate(),
        sound,
        sound_span,
        sample_rate: sr,
        picture,
        media_time: Some(Box::new(move |t| media.source_time_at(t))),
    })
}

/// A probe for a project item's rushes, in media time.
fn item_probe(s: &Session, p: &Value, item: ItemId, picture: bool) -> Result<Probe> {
    const CMD: &str = "story.blinkPoints";
    let pi = s.project.item(item).ok_or_else(|| bad(CMD, format!("no project item {}", item.0)))?;
    let src = s.source(item).ok_or_else(|| bad(CMD, format!("`{}` has no media to analyse", pi.name)))?;
    let info = src.info().clone();
    let (lo, hi) = item_span(s, item, pi);
    let (from, to) = blink_span(p, lo, hi, CMD)?;
    let rate = info.video.as_ref().map_or(FrameRate::FPS_24, |v| v.frame_rate).sane();
    let sr = info.audio().map_or(48_000, |a| a.sample_rate).clamp(8_000, 192_000);
    let sound = info.has_audio().then(|| {
        let src = src.clone();
        Box::new(move |start: i64, frames: usize| src.audio(start, frames, sr).ok().map(|b| mono(&b.channels, frames)))
            as Box<dyn Fn(i64, usize) -> Option<Vec<f32>>>
    });
    let scale = frame_scale(&src);
    let picture = (picture && info.video.is_some()).then(|| {
        Box::new(move |t: Tick| {
            let f = src.video_frame(filmagic_media::FrameRequest { time: t, scale }).ok()?;
            Some((f.to_rgba8(), f.width as usize, f.height as usize))
        }) as Box<dyn Fn(Tick) -> Option<(Vec<u8>, usize, usize)>>
    });
    // The media is there before a subclip's In point too: it is read as context, so that a phrase
    // already under way where the span begins is known to be a phrase.
    let lead = from.clamp(Tick::ZERO, Tick::from_seconds_f64(2.0));
    Ok(Probe { from, to, lead, rate, sound, sound_span: (Tick::ZERO, Tick(i64::MAX)), sample_rate: sr, picture, media_time: None })
}

/// The stretch of media time a project item covers. A subclip is a stretch of its parent's media
/// and is read from the parent's source, so its times are the parent's: it runs from its In point
/// to its Out point, not from 0.
fn item_span(s: &Session, item: ItemId, pi: &filmagic_project::ProjectItem) -> (Tick, Tick) {
    match s.project.resolve_media(item) {
        Some((_, _, Some(range))) => (range.start, range.end()),
        _ => (Tick::ZERO, crate::commands::full_duration(s, pi)),
    }
}

/// The frame rate of a project item's media (a subclip's is its parent's).
fn item_rate(s: &Session, item: ItemId, pi: &filmagic_project::ProjectItem) -> FrameRate {
    s.project.resolve_media(item).and_then(|(parent, _, _)| s.project.item(parent)).unwrap_or(pi).frame_rate().sane()
}

fn blink_points(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "story.blinkPoints";
    let min_pause = num_p(p, "minPause", CMD)?.unwrap_or(0.25);
    if !min_pause.is_finite() || !(0.05..=10.0).contains(&min_pause) {
        return Err(bad(CMD, "`minPause` must be between 0.05 and 10 seconds"));
    }
    let picture = flag_p(p, "picture", true, CMD)?;
    let markers = flag_p(p, "markers", false, CMD)?;
    let (clip, item) = (clip_p(p, "clip"), item_p(p, "item"));
    let probe = match (clip, item) {
        (Some(c), None) => clip_probe(s, p, c, picture)?,
        (None, Some(i)) if !markers => item_probe(s, p, i, picture)?,
        (None, Some(_)) => return Err(bad(CMD, "`markers` adds sequence markers: it needs a timeline `clip`, not a project `item`")),
        _ => return Err(bad(CMD, "need a timeline `clip` or a project `item` (one of them)")),
    };
    if probe.sound.is_none() && probe.picture.is_none() {
        return Err(bad(CMD, "there is neither sound nor picture to analyse"));
    }
    let Found { points, heard, seen } = find_blink_points(&probe, min_pause);
    let rate = probe.rate;
    let mut notes: Vec<&str> = Vec::new();
    if probe.sound.is_some() && !heard {
        notes.push("the sound could not be read (media offline, or a stream that does not decode): no phrase was looked for");
    } else if probe.sound.is_some() && !points.iter().any(|x| x.kind != "movement") {
        notes.push("the sound has no pause after a phrase here: a steady background, music, or speech that never stops for `minPause`");
    }
    if probe.picture.is_some() && !seen {
        notes.push("no frame of the picture could be read (media offline, or a stream that does not decode): no movement was looked for");
    } else if probe.picture.is_some() && !points.iter().any(|x| x.kind != "phrase") {
        notes.push("nothing in the picture starts and then stops here: it holds still, or it moves evenly all the way through");
    }
    if clip.is_some() && probe.sound.is_some() && (probe.sound_span.0 > probe.from || probe.sound_span.1 < probe.to) {
        notes.push("the clip's linked sound does not run the whole length of the clip (a split edit, or a shorter sound clip): phrases were looked for only where there is sound");
    }
    let listed: Vec<Value> = points
        .iter()
        .take(MAX_LISTED)
        .map(|x| {
            let mut v = json!({"at": time_json(x.t, rate), "kind": x.kind, "strength": round3(x.strength)});
            if let Some((a, b)) = x.pause {
                v["pause"] = json!({"start": time_json(a, rate), "end": time_json(b, rate), "seconds": round3((b - a).seconds())});
            }
            if let Some(m) = &probe.media_time {
                v["mediaTime"] = json!(m(x.t).0);
            }
            v
        })
        .collect();
    let mut added: Vec<u64> = Vec::new();
    if markers && !points.is_empty() {
        let times: Vec<(Tick, String)> = points
            .iter()
            .take(200)
            .map(|x| (x.t, format!("{} (strength {:.2})", if x.kind == "movement" { "a movement completes" } else { "a phrase ends" }, x.strength)))
            .collect();
        added = s.edit_sequence("Mark Blink Points", |q, ctx, _| {
            let mut ids = Vec::new();
            for (t, comment) in times {
                let id = MarkerId(ctx.alloc());
                q.markers.push(Marker {
                    url: String::new(),
                    author: String::new(),
                    custom_color: None,
                    id,
                    start: t,
                    duration: Tick::ZERO,
                    name: "Blink point".into(),
                    comment,
                    kind: MarkerKind::Comment,
                    color: Label::Green,
                });
                ids.push(id.0);
            }
            q.markers.sort_by_key(|m| m.start);
            Ok(ids)
        })?;
    }
    Ok(json!({
        "span": {"from": time_json(probe.from, rate), "to": time_json(probe.to, rate)},
        "timeBase": if clip.is_some() { "sequence" } else { "media" },
        "analysed": {"sound": probe.sound.is_some() && heard, "picture": probe.picture.is_some() && seen},
        "truncated": points.len() > MAX_LISTED,
        "points": listed,
        "markers": added,
        "notes": notes,
        "note": "each point is a place a cut could go: where a thought or a movement completes. Cutting before a phrase ends keeps the audience on the words; holding past it lets them read the face. Which branch to take is the editor's choice",
    }))
}

/// A board cell before it is laid out.
struct Cell {
    image: filmagic_render::Image,
    label: String,
    info: Value,
}

fn clock(t: Tick) -> String {
    let sec = t.seconds().max(0.0) as u64;
    format!("{}:{:02}", sec / 60, sec % 60)
}

/// One cell per shot of the active sequence: the frame most like the others sampled in the shot.
fn sequence_cells(s: &Session, p: &Value, width: f64, max: usize, columns: usize) -> Result<(Vec<Cell>, bool, (u32, u32))> {
    const CMD: &str = "story.board";
    let (_, q) = active(s, CMD)?;
    let rate = q.frame_rate();
    let (from, to) = (seconds_bound(p, "fromSeconds", CMD)?, seconds_bound(p, "toSeconds", CMD)?);
    let all = shots(&s.project, q)?;
    let wanted: Vec<(usize, &Shot)> =
        all.iter().enumerate().filter(|(_, sh)| sh.clip.is_some() && from.is_none_or(|f| sh.end > f) && to.is_none_or(|x| sh.start < x)).collect();
    let truncated = wanted.len() > max;
    let par = filmagic_project::sane_par(q.settings.par);
    board_budget(wanted.len().min(max), columns, width, (q.settings.width, q.settings.height), par, CMD)?;
    let scale = (width / f64::from(q.settings.width.max(1))).clamp(0.01, 1.0) as f32;
    let mut cells = Vec::new();
    for (i, sh) in wanted.into_iter().take(max) {
        // five looks across the shot (never its very first or last frame: those sit in transitions)
        let len = (sh.end - sh.start).0;
        // the frames the shot is on screen for: a shot need not start on the grid, and the frame
        // its start falls in still shows the shot before
        let (first, last) = (frame_from(rate, sh.start), frame_before(rate, sh.end));
        let mut frames: Vec<(Tick, filmagic_render::Image, Signature)> = Vec::new();
        for k in 1..=5 {
            let t = rate.snap(sh.start + Tick(len / 6 * k));
            let t = if first <= last { t.clamp(first, last) } else { t };
            if frames.iter().any(|f| f.0 == t) {
                continue;
            }
            let img = s.try_render_program_at(scale, t)?;
            let sig = signature(&img.over_black_rgba8(), img.w, img.h);
            frames.push((t, img, sig));
        }
        let sigs: Vec<Signature> = frames.iter().map(|f| f.2.clone()).collect();
        let Some(best) = medoid(&sigs).and_then(|m| (m < frames.len()).then(|| frames.swap_remove(m))) else { continue };
        cells.push(Cell {
            image: best.1,
            label: format!("{}  {}", i + 1, clock(sh.start)),
            info: json!({"shot": i + 1, "start": time_json(sh.start, rate), "end": time_json(sh.end, rate), "frame": time_json(best.0, rate), "clip": sh.clip.map(|c| c.0), "name": clip_name(q, sh.clip)}),
        });
    }
    Ok((cells, truncated, par))
}

/// `count` frames spread evenly across a project item's media.
fn item_cells(
    s: &Session,
    p: &Value,
    item: ItemId,
    width: f64,
    count: usize,
    columns: usize,
    step: Option<Tick>,
) -> Result<(Vec<Cell>, (u32, u32), (Tick, Tick))> {
    const CMD: &str = "story.board";
    let pi = s.project.item(item).ok_or_else(|| bad(CMD, format!("no project item {}", item.0)))?;
    let (lo, hi) = item_span(s, item, pi);
    let (from, to) =
        (seconds_bound(p, "fromSeconds", CMD)?.map_or(lo, |t| t.max(lo).min(hi)), seconds_bound(p, "toSeconds", CMD)?.map_or(hi, |t| t.max(lo).min(hi)));
    if to <= from {
        return Err(bad(CMD, "nothing to show between `fromSeconds` and `toSeconds`"));
    }
    let rate = item_rate(s, item, pi);
    let (w, h) = filmagic_render::source_size(&s.project, item).ok_or_else(|| bad(CMD, format!("`{}` has no picture", pi.name)))?;
    // media is decoded as it is stored; anamorphic footage is shown wider than that
    let par = filmagic_render::source_par(&s.project, item).map_or((1, 1), filmagic_project::sane_par);
    let times: Vec<Tick> = match step {
        Some(step) => dense_times(from, to, step, rate, CMD)?,
        // the middle of each of `count` equal parts
        None => (0..count).map(|k| rate.snap(from + Tick((to - from).0 / (count as i64 * 2) * (2 * k as i64 + 1)))).collect(),
    };
    board_budget(times.len(), columns, width, (w, h), par, CMD)?;
    let scale = (width / f64::from(w.max(1))).clamp(0.01, 1.0) as f32;
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let mut cells = Vec::new();
    for (k, t) in times.into_iter().enumerate() {
        let Some(image) = filmagic_render::render_item(&s.project, item, t, scale, &provider) else { continue };
        let label = if step.is_some() { format!("{}  {}", k + 1, fine_clock(t)) } else { format!("{}  {}", k + 1, clock(t)) };
        cells.push(Cell { image, label, info: json!({"index": k + 1, "at": time_json(t, rate)}) });
    }
    if cells.is_empty() {
        return Err(bad(CMD, format!("no frame of `{}` could be decoded", pi.name)));
    }
    Ok((cells, par, (from, to)))
}

/// Most frames a densely sampled board (`fps` / `step`) shows.
const MAX_DENSE: i64 = 96;

/// `fps` or `step` (seconds): sample a range densely instead of one frame per shot or part.
fn dense_step_p(p: &Value, cmd: &str) -> Result<Option<Tick>> {
    let num = |key: &str, lo: f64, hi: f64, what: &str| -> Result<Option<f64>> {
        match p.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v.as_f64().filter(|x| x.is_finite() && (lo..=hi).contains(x)).map(Some).ok_or_else(|| bad(cmd, format!("`{key}` must be {what}"))),
        }
    };
    match (num("fps", 0.01, 120.0, "frames per second, from 0.01 to 120")?, num("step", 1.0 / 120.0, 3600.0, "seconds between frames, from 1/120 to 3600")?) {
        (Some(_), Some(_)) => Err(bad(cmd, "give `fps` or `step`, not both")),
        (Some(f), None) => Ok(Some(Tick::from_seconds_f64(1.0 / f))),
        (None, Some(x)) => Ok(Some(Tick::from_seconds_f64(x))),
        (None, None) => Ok(None),
    }
}

/// Times every `step` from `from` up to `to`, on `rate`'s frames; refused past [`MAX_DENSE`].
fn dense_times(from: Tick, to: Tick, step: Tick, rate: FrameRate, cmd: &str) -> Result<Vec<Tick>> {
    let step = step.max(Tick(1));
    let span = (to - from).0.max(0);
    let n = (span / step.0 + i64::from(span % step.0 != 0)).max(1);
    if n > MAX_DENSE {
        let fit = step.seconds() * MAX_DENSE as f64;
        return Err(bad(
            cmd,
            format!(
                "{:.2} s at one frame every {:.3} s is {n} frames; a board holds {MAX_DENSE}: pass `fromSeconds`/`toSeconds` at most {fit:.2} s apart, or lower `fps`",
                (to - from).seconds(),
                step.seconds()
            ),
        ));
    }
    let mut v: Vec<Tick> = (0..n).map(|k| rate.snap(from + Tick(step.0.saturating_mul(k)))).filter(|t| *t < to.max(from + Tick(1))).collect();
    v.dedup();
    Ok(v)
}

/// A time to the hundredth of a second, for densely sampled boards.
fn fine_clock(t: Tick) -> String {
    let sec = t.seconds().max(0.0);
    format!("{}:{:05.2}", (sec / 60.0).floor() as u64, sec % 60.0)
}

/// Program frames every `step` across `fromSeconds`…`toSeconds` of the active sequence.
fn dense_sequence_cells(s: &Session, p: &Value, width: f64, columns: usize, step: Tick) -> Result<(Vec<Cell>, (u32, u32))> {
    const CMD: &str = "story.board";
    let (_, q) = active(s, CMD)?;
    let rate = q.frame_rate();
    let end = q.duration();
    let from = seconds_bound(p, "fromSeconds", CMD)?.unwrap_or(Tick::ZERO).min(end);
    let to = seconds_bound(p, "toSeconds", CMD)?.unwrap_or(end).min(end);
    if to <= from {
        return Err(bad(CMD, "nothing to show between `fromSeconds` and `toSeconds` (the sequence ends at that time)"));
    }
    let times = dense_times(from, to, step, rate, CMD)?;
    let par = filmagic_project::sane_par(q.settings.par);
    board_budget(times.len(), columns, width, (q.settings.width, q.settings.height), par, CMD)?;
    let scale = (width / f64::from(q.settings.width.max(1))).clamp(0.01, 1.0) as f32;
    let mut cells = Vec::new();
    for (k, t) in times.into_iter().enumerate() {
        let image = s.try_render_program_at(scale, t)?;
        cells.push(Cell { image, label: format!("{}  {}", k + 1, fine_clock(t)), info: json!({"index": k + 1, "at": time_json(t, rate)}) });
    }
    Ok((cells, par))
}

/// Frames of new media a board may decode to mark the cuts inside an item's source.
const BOARD_CUT_FRAMES: i64 = 1440;

/// Mark the cells of an item board that follow a cut inside the source; returns the cuts found
/// and whether the whole span was checked.
fn mark_source_cuts(s: &mut Session, item: ItemId, span: (Tick, Tick), cells: &mut [Cell]) -> Option<(Vec<Tick>, bool)> {
    let (cuts, complete) = crate::source_cuts::known_cuts(s, item, span.0, span.1, BOARD_CUT_FRAMES)?;
    let mut prev = span.0 - Tick(1);
    let mut shot = 1 + cuts.iter().filter(|c| **c <= span.0).count();
    for c in cells.iter_mut() {
        let Some(t) = c.info["at"]["time"].as_i64().map(Tick) else { continue };
        let before: Vec<f64> = cuts.iter().filter(|x| **x > prev && **x <= t).map(|x| round3(x.seconds())).collect();
        if !before.is_empty() {
            shot += before.len();
            c.label.push_str("  | CUT");
            c.info["cutBefore"] = json!(before);
        }
        c.info["sourceShot"] = json!(shot);
        prev = t;
    }
    Some((cuts, complete))
}

fn count_p(p: &Value, key: &str, default: usize, range: std::ops::RangeInclusive<u64>, cmd: &str) -> Result<usize> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(_) => u64_p(p, key)
            .filter(|n| range.contains(n))
            .map(|n| n as usize)
            .ok_or_else(|| bad(cmd, format!("`{key}` must be between {} and {}", range.start(), range.end()))),
    }
}

fn board(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "story.board";
    let path = str_p(p, "path")
        .filter(|x| !x.trim().is_empty())
        .map(crate::export_tools::expand_home)
        .ok_or_else(|| bad(CMD, "need `path`: where to write the PNG"))?;
    if !path.to_ascii_lowercase().ends_with(".png") {
        return Err(bad(CMD, "`path` must end in .png"));
    }
    // writing over a file the project reads would change that media under its clips
    if crate::layers::is_project_media(s, &path) {
        return Err(bad(CMD, format!("{path} is media of this project; write the board to another file")));
    }
    let columns = count_p(p, "columns", 4, 1..=12, CMD)?;
    let width = count_p(p, "width", 320, 64..=1280, CMD)? as f64;
    let labels = flag_p(p, "labels", true, CMD)?;
    let step = dense_step_p(p, CMD)?;
    let mut source = None;
    let (cells, truncated, par, of) = match item_p(p, "item") {
        Some(item) => {
            let (mut c, par, span) = item_cells(s, p, item, width, count_p(p, "count", 12, 1..=96, CMD)?, columns, step)?;
            source = mark_source_cuts(s, item, span, &mut c).map(|x| (item, x));
            (c, false, par, "item")
        }
        None if let Some(st) = step => {
            let (c, par) = dense_sequence_cells(s, p, width, columns, st)?;
            (c, false, par, "sequence")
        }
        None => {
            let (c, t, par) = sequence_cells(s, p, width, count_p(p, "max", 48, 1..=96, CMD)?, columns)?;
            (c, t, par, "sequence")
        }
    };
    let Some(first) = cells.first() else { return Err(bad(CMD, "there is no picture to lay out")) };
    // a source may decode a little smaller or larger than asked: the cell is the asked width, shaped like the frame
    let cell_w = width as usize;
    let cell_h = cell_height(width, (first.image.w, first.image.h), par);
    let sheet = Sheet { columns: columns.min(cells.len()), cell_w, cell_h, gap: BOARD_GAP, pixel_aspect: par };
    let info: Vec<Value> = cells.iter().map(|c| c.info.clone()).collect();
    let laid: Vec<(filmagic_render::Image, String)> = cells.into_iter().map(|c| (c.image, if labels { c.label } else { String::new() })).collect();
    let img = contact_sheet(&laid, sheet).ok_or_else(|| bad(CMD, "the board would be too large; lower `width`, `max` or `count`"))?;
    let bytes = filmagic_export::still::encode(&img, filmagic_export::still::StillFormat::Png, 8).map_err(|e| EngineError::Other(e.to_string()))?;
    s.services.write_file(&path, &bytes).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    let mut out = json!({
        "path": path,
        "of": of,
        "width": img.w,
        "height": img.h,
        "columns": sheet.columns,
        "truncated": truncated,
        "cells": info,
        "note": "read it like a page: left to right, top to bottom. Frames that sit far apart in the film sit side by side here",
    });
    if let Some(step) = step {
        out["step"] = json!(round3(step.seconds()));
    }
    if let Some((item, (cuts, complete))) = source {
        // cells after a cut inside the source are labelled "| CUT" and carry `cutBefore`
        out["sourceCuts"] = json!(cuts.iter().map(|c| round3(c.seconds())).collect::<Vec<_>>());
        out["sourceCutsComplete"] = json!(complete);
        if !complete {
            out["notes"] = json!([format!(
                "cuts inside the source were looked for in part of this span only; run `media.sourceCuts` {{\"item\":{}}} (repeat with its `next`) and draw the board again to mark them all",
                item.0
            )]);
        }
    }
    Ok(out)
}

fn split_edit(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "story.splitEdit";
    let kind = match str_p(p, "kind").map(str::to_ascii_uppercase).as_deref() {
        Some("L") => "L",
        Some("J") => "J",
        _ => {
            return Err(bad(
                CMD,
                "`kind` must be \"L\" (the sound of the outgoing shot runs on under the new picture) or \"J\" (the sound of the incoming shot arrives first)",
            ));
        }
    };
    let frames = count_p(p, "frames", 12, 1..=10_000, CMD)? as i64;
    let roll_video = match p.get("roll") {
        None | Some(Value::Null) => false,
        Some(v) => match v.as_str() {
            Some("audio") => false,
            Some("video") => true,
            _ => return Err(bad(CMD, "`roll` must be \"audio\" or \"video\"")),
        },
    };
    let (_, q) = active(s, CMD)?;
    let rate = q.frame_rate();
    let at = nearest_p(s, p, CMD)?.unwrap_or_else(|| s.playhead());
    let all = shots(&s.project, q)?;
    let (before, after) = all
        .windows(2)
        .filter_map(|w| Some((w.first()?, w.get(1)?)))
        .min_by_key(|(_, b)| (b.start - at).abs().0)
        .ok_or_else(|| bad(CMD, "the sequence has no cut to split"))?;
    let cut = after.start;
    let (Some(out), Some(inc)) = (before.clip, after.clip) else {
        return Err(bad(CMD, "the nearest cut goes to or from black; a split edit needs a clip on both sides"));
    };
    // A plate or a layer over a shot has no sound of its own: the sound is the clip's underneath.
    let (out_c, inc_c) = (sound_carrier(q, Some(out)).unwrap_or(out), sound_carrier(q, Some(inc)).unwrap_or(inc));
    let link_of = |c: ClipId| q.find_item(c).and_then(|(_, it)| it.link);
    let (link_out, link_in) = (
        link_of(out_c).ok_or_else(|| bad(CMD, "the outgoing clip has no linked sound to split from its picture"))?,
        link_of(inc_c).ok_or_else(|| bad(CMD, "the incoming clip has no linked sound to split from its picture"))?,
    );
    // Every audio track on which the two clips' sounds meet at one cut: a clip recorded on two
    // microphones has its sound on two tracks, and a split edit moves both cuts.
    let mut pairs: Vec<(ClipId, ClipId)> = Vec::new();
    let mut apart: Vec<String> = Vec::new();
    for track in &q.audio_tracks {
        let (a, b) = (track.items.iter().find(|x| x.link == Some(link_out)), track.items.iter().find(|x| x.link == Some(link_in)));
        match (a, b) {
            (Some(a), Some(b)) if a.end() == b.start => {
                if track.locked && !roll_video {
                    return Err(bad(CMD, format!("track {} is locked", track.name)));
                }
                pairs.push((a.id, b.id));
            }
            (None, None) => {}
            _ => apart.push(track.name.clone()),
        }
    }
    if roll_video {
        // something made from one of the two clips lies over it and would be left behind
        let stacked = |c: ClipId| {
            q.find_item(c).is_some_and(|(_, base)| {
                q.video_tracks.iter().flat_map(|t| t.items.iter()).any(|x| x.enabled && crate::layers::of_its_stack(&s.project, base, x))
            })
        };
        if out_c != out || inc_c != inc || stacked(out) || stacked(inc) {
            return Err(bad(
                CMD,
                "a layer or a plate lies over one of the shots at this cut: rolling the picture would move it away from the clip it was made from. Roll the sound instead (`roll: \"audio\"`)",
            ));
        }
        pairs = vec![(out, inc)];
    } else if pairs.is_empty() {
        return Err(bad(
            CMD,
            "the two clips' sounds do not meet at one cut on one track (already split, or on different tracks); roll them with `timeline.roll`",
        ));
    }
    let continuous = |(l, r): &(ClipId, ClipId)| q.find_item(*l).zip(q.find_item(*r)).is_some_and(|((_, l), (_, r))| carries_on(l, r));
    if pairs.iter().all(continuous) {
        return Err(bad(
            CMD,
            "the two clips are one continuous recording cut in two: moving the cut between them changes nothing anyone can see or hear. A split edit needs two separately recorded clips (with their own sound) on either side of the cut",
        ));
    }
    // L: sound trails (the sound cut moves later, or the picture cut earlier). J: the reverse.
    let delta = match (roll_video, kind) {
        (false, "L") | (true, "J") => frames,
        _ => -frames,
    };
    let fd = rate.frame_duration().0.max(1);
    let want = Tick(fd.saturating_mul(delta));
    let rolled = pairs.clone();
    // one cut on every track, by one amount: what the shortest handle among them allows
    let moved = s.edit_sequence("Split Edit", |q, ctx, _| {
        let d = crate::commands::clamp_roll_linked(q, &rolled, want, ctx)?;
        if d == Tick::ZERO {
            return Err(bad(CMD, "the clips have no spare media beyond the cut to extend into (no handles)"));
        }
        for &(l, r) in &rolled {
            filmagic_edit::roll(q, l, r, d, ctx)?;
        }
        Ok(d)
    })?;
    let mut warnings: Vec<String> = Vec::new();
    if !roll_video && !apart.is_empty() {
        warnings.push(format!("the clips also have sound on {} that does not meet at this cut; it was left as it is", apart.join(", ")));
    }
    Ok(json!({
        "kind": kind,
        "cut": time_json(cut, rate),
        "rolled": if roll_video { "video" } else { "audio" },
        "left": pairs.first().map(|x| x.0.0),
        "right": pairs.first().map(|x| x.1.0),
        "pairs": pairs.iter().map(|(l, r)| json!([l.0, r.0])).collect::<Vec<_>>(),
        "frames": moved.0 / fd,
        "asked": delta,
        "clamped": moved != want,
        "warnings": warnings,
    }))
}

fn shadow_cut(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "story.shadowCut";
    let (id, _) = active(s, CMD)?;
    let item = s.project.item(id).ok_or_else(|| bad(CMD, "no active sequence"))?.clone();
    let name = match p.get("name") {
        None | Some(Value::Null) => {
            // the first number no shadow cut of this sequence carries (a count would repeat one after a deletion)
            let taken: BTreeSet<&str> = s.project.items.values().map(|i| i.name.as_str()).collect();
            (1..=taken.len() + 1)
                .map(|n| format!("{} (shadow cut {n})", item.name))
                .find(|name| !taken.contains(name.as_str()))
                .unwrap_or_else(|| format!("{} (shadow cut)", item.name))
        }
        Some(v) => v
            .as_str()
            .map(str::trim)
            .filter(|x| !x.is_empty() && x.len() <= 255 && !x.chars().any(char::is_control))
            .ok_or_else(|| bad(CMD, "`name` must be 1 to 255 bytes of text"))?
            .to_string(),
    };
    let copy = s.edit("Keep a Shadow Cut", |pr, _| {
        let bin = pr.root.parent_of(id);
        let nid = pr.add_item(&name, item.label, item.kind.clone(), bin);
        if let Some(c) = pr.item_mut(nid) {
            c.custom_label_color = item.custom_label_color;
        }
        Ok(nid)
    })?;
    Ok(
        json!({"sequence": copy.0, "name": name, "of": id.0, "note": "the copy is set aside as it is now; the active sequence stays open for the next attempt. `story.compare` shows what changed"}),
    )
}

/// The clips of the active sequence, track by track, in a form short enough to read: what an
/// agent needs to pick a clip id without wading through every effect parameter.
fn list_clips(s: &mut Session, p: &Value) -> Result<Value> {
    let (id, _) = active(s, "sequence.clips")?;
    list_clips_in(s, id, p)
}

/// [`list_clips`] of sequence `id`, with only the `track` given (a name such as "V1" or an id).
pub(crate) fn list_clips_of(s: &Session, id: ItemId, track: Option<&Value>) -> Result<Value> {
    let p = track.map_or_else(|| json!({}), |t| json!({"track": t}));
    list_clips_in(s, id, &p)
}

fn list_clips_in(s: &Session, id: ItemId, p: &Value) -> Result<Value> {
    const CMD: &str = "sequence.clips";
    let q = s.project.sequence(id).ok_or(EngineError::NoSequence)?;
    let rate = q.frame_rate();
    let (from, to) = (seconds_bound(p, "fromSeconds", CMD)?, seconds_bound(p, "toSeconds", CMD)?);
    let only = match p.get("track") {
        None | Some(Value::Null) => None,
        Some(_) => Some(crate::commands::track_p(s, p, "track", CMD)?.ok_or_else(|| bad(CMD, "`track` must be a track name such as \"V1\" or a track id"))?),
    };
    let mut total = 0usize;
    let mut tracks = Vec::new();
    for t in q.video_tracks.iter().rev().chain(q.audio_tracks.iter()) {
        if only.is_some_and(|o| o != t.id) {
            continue;
        }
        let mut items: Vec<&TrackItem> = t.items.iter().filter(|it| from.is_none_or(|f| it.end() > f) && to.is_none_or(|x| it.start < x)).collect();
        items.sort_by_key(|it| it.start);
        let clips: Vec<Value> = items
            .iter()
            .take(MAX_LISTED.saturating_sub(total))
            .map(|it| {
                // what was added to the clip: the effects every clip carries are left out
                let added: Vec<&str> =
                    it.effects.iter().filter(|e| e.def().is_none_or(|d| !d.intrinsic) || !e.masks.is_empty()).map(|e| e.effect.as_str()).collect();
                let mut c = json!({
                    "clip": it.id.0,
                    "name": it.name,
                    "item": it.item.0,
                    "start": round3(it.start.seconds()),
                    "end": round3(it.end().seconds()),
                    "startFrame": rate.frame_at(it.start),
                    "endFrame": rate.frame_at(it.end()),
                    "sourceIn": round3(it.source_in.seconds()),
                });
                let extra: [(&str, Option<Value>); 6] = [
                    ("link", it.link.map(Value::from)),
                    ("speed", (it.speed != 1.0).then(|| json!(it.speed))),
                    ("reverse", it.reverse.then_some(Value::Bool(true))),
                    ("disabled", (!it.enabled).then_some(Value::Bool(true))),
                    ("held", it.frame_hold.map(|h| json!(round3(h.seconds())))),
                    ("effects", (!added.is_empty()).then(|| json!(added))),
                ];
                for (key, value) in extra {
                    if let Some(value) = value {
                        c[key] = value;
                    }
                }
                c
            })
            .collect();
        total += clips.len();
        let transitions: Vec<Value> = t
            .transitions
            .iter()
            .filter(|x| from.is_none_or(|f| x.end() > f) && to.is_none_or(|e| x.start < e))
            .take(MAX_LISTED)
            .map(|x| {
                json!({"transition": x.id.0, "effect": x.effect.effect, "start": round3(x.start.seconds()), "end": round3(x.end().seconds()), "from": x.from.map(|c| c.0), "to": x.to.map(|c| c.0)})
            })
            .collect();
        let mut tr = json!({"track": t.name, "id": t.id.0, "kind": if t.kind == TrackKind::Video { "video" } else { "audio" }, "clips": clips});
        if !transitions.is_empty() {
            tr["transitions"] = json!(transitions);
        }
        for (key, on) in
            [("hidden", t.kind == TrackKind::Video && !t.enabled), ("muted", t.kind == TrackKind::Audio && (!t.enabled || t.muted)), ("locked", t.locked)]
        {
            if on {
                tr[key] = Value::Bool(true);
            }
        }
        tracks.push(tr);
    }
    Ok(json!({
        "sequence": id.0,
        "fps": round3(rate.as_f64()),
        "seconds": round3(q.duration().seconds()),
        "tracks": tracks,
        "truncated": total >= MAX_LISTED,
        "note": "video tracks from the top down, then audio. Times are seconds; a clip plays its item from `sourceIn`",
    }))
}

/// The active sequence's frame at `t`, reduced, as RGBA8 (pixels, width, height).
pub(crate) fn program_rgba(s: &Session, t: Tick) -> Option<(Vec<u8>, usize, usize)> {
    let q = s.active_sequence()?;
    let img = s.try_render_program_at(analysis_scale(q), t).ok()?;
    Some((img.over_black_rgba8(), img.w, img.h))
}

/// [`program_rgba`] of another version of the project (the same sequence id, media from the
/// session's pool): for "what if this layer were not there" comparisons.
pub(crate) fn program_rgba_of(s: &Session, project: &std::sync::Arc<Project>, t: Tick, captions: bool) -> Option<Frame> {
    let seq = s.state.active_sequence?;
    let q = project.sequence(seq)?;
    let provider = s.media.provider(project.clone(), s.services.clone());
    let opts = filmagic_render::RenderOptions { scale: analysis_scale(q), captions, ..Default::default() };
    let img = filmagic_render::render_sequence(project, seq, q.frame_rate().snap(t.max(Tick::ZERO)), opts, &provider);
    Some((img.over_black_rgba8(), img.w, img.h))
}

/// A reduced frame of the programme: RGBA8 pixels, width, height.
pub(crate) type Frame = (Vec<u8>, usize, usize);

/// Three looks at the programme across `[start, end)`: just inside each end and in the middle.
pub(crate) fn looks_across(s: &Session, start: Tick, end: Tick) -> Vec<Option<Frame>> {
    // frames the stretch is on screen for: its ends need not fall on the sequence's frames
    let rate = s.sequence_rate();
    let first = Some(frame_from(rate, start)).filter(|g| *g < end).unwrap_or(start);
    let last = frame_before(rate, end).max(first);
    [first, first + Tick((last - first).0 / 2), last].into_iter().map(|t| program_rgba(s, t)).collect()
}

/// How much the picture changed between two sets of looks ([`looks_across`], before and after an
/// edit), as a result field: the look that changed most, and what a viewer would make of it.
/// `null` when nothing could be compared.
pub(crate) fn change_json(before: &[Option<Frame>], after: &[Option<Frame>]) -> Value {
    let worst = before
        .iter()
        .zip(after)
        .filter_map(|(a, b)| {
            let (a, b) = (a.as_ref()?, b.as_ref()?);
            if (a.1, a.2) != (b.1, b.2) {
                return None;
            }
            filmagic_render::story::picture_change(&a.0, &b.0, a.1, a.2)
        })
        .max_by(|x, y| x.strongest.total_cmp(&y.strongest));
    match worst {
        Some(c) => {
            let hint = match c.reads() {
                "none" => "nothing a viewer can see changed",
                "subtle" => "visible only side by side with the original; raise the amount if it is meant to be noticed",
                "visible" => "a viewer will notice it",
                _ => "a strong change",
            };
            json!({"mean": round3(f64::from(c.mean)), "strongest": round3(f64::from(c.strongest)), "reads": c.reads(), "hint": hint})
        }
        None => Value::Null,
    }
}

/// What a shot shows: which item, and which stretch of it. `None` for black, or for a clip that
/// does not play its source evenly forward (reversed or held), which cannot be lined up by time.
struct Shown {
    item: ItemId,
    from: Tick,
    to: Tick,
    speed: f64,
}

fn shown(q: &Sequence, sh: &Shot) -> Option<Shown> {
    let it = q.find_item(sh.clip?)?.1;
    if it.reverse || it.frame_hold.is_some() || !it.speed.is_finite() || it.speed <= 0.0 {
        return None;
    }
    let at = |t: Tick| it.source_in + Tick(((t - it.start).0 as f64 * it.speed).round() as i64);
    Some(Shown { item: it.item, from: at(sh.start), to: at(sh.end), speed: it.speed })
}

/// One sequence's frame at `t`, reduced, as RGBA8.
fn sequence_rgba(s: &Session, id: ItemId, q: &Sequence, t: Tick) -> (Vec<u8>, usize, usize) {
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let opts = filmagic_render::RenderOptions { scale: analysis_scale(q), captions: true, ..Default::default() };
    let img = filmagic_render::render_sequence(&s.project, id, q.frame_rate().snap(t.max(Tick::ZERO)), opts, &provider);
    (img.over_black_rgba8(), img.w, img.h)
}

/// What a shot is when it cannot be lined up by time (it plays reversed, or holds a frame): the
/// clip's footage and how it plays it. Two such shots are the same shot when these are equal.
fn played(q: &Sequence, sh: &Shot) -> Option<(ItemId, Tick, Tick, u64, bool, Option<Tick>)> {
    let it = q.find_item(sh.clip?)?.1;
    Some((it.item, it.source_in, it.duration, it.speed.to_bits(), it.reverse, it.frame_hold))
}

/// What [`compare_shots`] found.
struct ShotsCompared {
    rows: Vec<Value>,
    /// Shots of the other version this one has no shot for.
    removed: Vec<Value>,
    /// More shots than are listed on either side.
    truncated: bool,
    /// The two versions' frames are not the same shape, so no picture could be compared.
    unlike_frames: bool,
    tally: [usize; 4],
}

/// Line the shots of `mine` up with the shots of `theirs` by what they show, and say for each
/// what is different: where it sits, how long it runs, what it looks like.
fn compare_shots(s: &Session, here: (ItemId, &Sequence, &[Shot]), there: (ItemId, &Sequence, &[Shot]), pictures: usize) -> ShotsCompared {
    let ((id, q, mine), (other_id, other, theirs)) = (here, there);
    let rate = q.frame_rate();
    let half = Tick(rate.frame_duration().0 / 2);
    let theirs_shown: Vec<Option<Shown>> = theirs.iter().map(|sh| shown(other, sh)).collect();
    // their shots by the footage they show, so that a film of thousands of shots is lined up
    // against the few that share each clip's footage and not against all of them
    let mut by_item: std::collections::HashMap<ItemId, Vec<usize>> = std::collections::HashMap::new();
    for (j, sh) in theirs.iter().enumerate() {
        if let Some(item) = sh.clip.and_then(|c| other.find_item(c)).map(|(_, it)| it.item) {
            by_item.entry(item).or_default().push(j);
        }
    }
    let mut matched = vec![false; theirs.len()];
    let mut out = Vec::new();
    let (mut compared, mut unlike_frames) = (0usize, false);
    // same, changed, new, and shots of mine in all
    let mut tally = [0usize; 4];
    for (i, sh) in mine.iter().enumerate() {
        if sh.clip.is_none() {
            continue;
        }
        tally[3] += 1;
        let listed = out.len() < MAX_LISTED;
        let mut row = json!({"shot": i + 1, "start": time_json(sh.start, rate), "end": time_json(sh.end, rate), "clip": sh.clip.map(|c| c.0), "name": clip_name(q, sh.clip)});
        let me = shown(q, sh);
        let candidates = sh.clip.and_then(|c| q.find_item(c)).and_then(|(_, it)| by_item.get(&it.item)).map(Vec::as_slice).unwrap_or(&[]);
        let Some(m) = me.as_ref() else {
            // Reversed or held: there is no running time to line up, so it is the same shot only
            // where the other version plays the same stretch the same way.
            let mine_played = played(q, sh);
            let twin = candidates.iter().copied().find(|&j| matched.get(j) == Some(&false) && theirs.get(j).is_some_and(|th| played(other, th) == mine_played));
            match twin.and_then(|j| theirs.get(j).map(|th| (j, th))) {
                Some((j, th)) => {
                    if let Some(slot) = matched.get_mut(j) {
                        *slot = true;
                    }
                    let shift = sh.start - th.start;
                    let moved = shift.abs() > half;
                    tally[usize::from(moved)] += 1;
                    row["status"] = json!(if moved { "changed" } else { "same" });
                    row["what"] = json!(if moved { vec!["moved"] } else { Vec::new() });
                    row["there"] = json!({"shot": j + 1, "start": time_json(th.start, rate), "end": time_json(th.end, rate)});
                    row["shiftSeconds"] = json!(round3(shift.seconds()));
                }
                None => {
                    tally[2] += 1;
                    row["status"] = json!("new");
                    row["what"] = json!([]);
                    row["note"] = json!("plays reversed or held, and the other version has no shot that plays the same stretch the same way");
                }
            }
            if listed {
                out.push(row);
            }
            continue;
        };
        // The shot of the other version that shows the most of the same footage. Footage used
        // twice in a film (a piece of B-roll that comes back) is shown as fully by either use: of
        // those, the one nearest in the film, and one not already taken.
        let best = candidates
            .iter()
            .filter_map(|&j| {
                let (t, th) = (theirs_shown.get(j)?.as_ref()?, theirs.get(j)?);
                let overlap = (m.to.min(t.to) - m.from.max(t.from)).0;
                let free = matched.get(j) == Some(&false);
                (t.item == m.item && (t.speed - m.speed).abs() < 1e-9 && overlap > 0)
                    .then_some((j, (overlap, free, std::cmp::Reverse((th.start - sh.start).abs()))))
            })
            .max_by_key(|(_, key)| *key);
        let Some((j, t, th)) = best.and_then(|(j, _)| Some((j, theirs_shown.get(j)?.as_ref()?, theirs.get(j)?))) else {
            tally[2] += 1;
            row["status"] = json!("new");
            row["what"] = json!([]);
            row["note"] = json!("footage the other version does not show");
            if listed {
                out.push(row);
            }
            continue;
        };
        if let Some(slot) = matched.get_mut(j) {
            *slot = true;
        }
        // the same moment of the footage, on each timeline
        let (lo, hi) = (m.from.max(t.from), m.to.min(t.to));
        let here_at = |x: Tick| sh.start + Tick(((x - m.from).0 as f64 / m.speed).round() as i64);
        let there_at = |x: Tick| th.start + Tick(((x - t.from).0 as f64 / t.speed).round() as i64);
        let shift = here_at(lo) - there_at(lo);
        let (len_here, len_there) = (sh.end - sh.start, th.end - th.start);
        let mut what: Vec<&str> = Vec::new();
        if shift.abs() > half {
            what.push("moved");
        }
        if (len_here - len_there).abs() > half {
            what.push(if len_here < len_there { "shorter" } else { "longer" });
        }
        if compared < pictures && listed {
            compared += 1;
            let mut worst: Option<filmagic_render::story::PictureChange> = None;
            for k in 1..=3 {
                // a quarter, a half and three quarters of the way through what both show
                let x = lo + Tick((hi - lo).0 / 4 * k);
                let (a, b) = (sequence_rgba(s, id, q, here_at(x)), sequence_rgba(s, other_id, other, there_at(x)));
                if (a.1, a.2) != (b.1, b.2) {
                    unlike_frames = true;
                    continue;
                }
                if let Some(c) = filmagic_render::story::picture_change(&a.0, &b.0, a.1, a.2)
                    && worst.is_none_or(|w| c.strongest > w.strongest)
                {
                    worst = Some(c);
                }
            }
            // `null`: asked for and not compared (the versions' frames are not the same shape)
            row["picture"] =
                worst.map_or(Value::Null, |c| json!({"mean": round3(f64::from(c.mean)), "strongest": round3(f64::from(c.strongest)), "reads": c.reads()}));
            if worst.is_some_and(|c| c.reads() != "none") {
                what.push("looks different");
            }
        }
        tally[usize::from(!what.is_empty())] += 1;
        row["status"] = json!(if what.is_empty() { "same" } else { "changed" });
        row["what"] = json!(what);
        row["there"] = json!({"shot": j + 1, "start": time_json(th.start, rate), "end": time_json(th.end, rate)});
        row["shiftSeconds"] = json!(round3(shift.seconds()));
        row["secondsHere"] = json!(round3(len_here.seconds()));
        row["secondsThere"] = json!(round3(len_there.seconds()));
        if listed {
            out.push(row);
        }
    }
    let gone = || theirs.iter().enumerate().filter(|(j, sh)| sh.clip.is_some() && matched.get(*j) == Some(&false));
    let removed: Vec<Value> = gone()
        .take(MAX_LISTED)
        .map(|(j, sh)| json!({"shot": j + 1, "start": time_json(sh.start, rate), "end": time_json(sh.end, rate), "name": clip_name(other, sh.clip)}))
        .collect();
    let truncated = tally[3] > out.len() || gone().count() > removed.len();
    ShotsCompared { rows: out, removed, truncated, unlike_frames, tally: [tally[0], tally[1], tally[2], gone().count()] }
}

fn compare(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "story.compare";
    let (id, q) = active(s, CMD)?;
    let other_id = u64_p(p, "with").map(ItemId).ok_or_else(|| bad(CMD, "need `with`: the project item id of the other sequence"))?;
    let other = s.project.sequence(other_id).ok_or_else(|| bad(CMD, format!("project item {} is not a sequence", other_id.0)))?;
    let rate = q.frame_rate();
    let (mine, theirs) = (shots(&s.project, q)?, shots(&s.project, other)?);
    let (a, b) = (cut_times(&mine), cut_times(&theirs));
    let half = Tick(rate.frame_duration().0 / 2);
    // both lists are in time order: walk them together
    let (mut only_here, mut only_there, mut shared) = (Vec::new(), Vec::new(), 0usize);
    let (mut i, mut j) = (0, 0);
    while let (Some(&x), Some(&y)) = (a.get(i), b.get(j)) {
        if (x - y).abs() <= half {
            shared += 1;
            i += 1;
            j += 1;
        } else if x < y {
            only_here.push(x);
            i += 1;
        } else {
            only_there.push(y);
            j += 1;
        }
    }
    only_here.extend(a.iter().skip(i));
    only_there.extend(b.iter().skip(j));
    let end = |v: &[Shot]| v.last().map_or(Tick::ZERO, |sh| sh.end) - v.first().map_or(Tick::ZERO, |sh| sh.start);
    let list = |v: &[Tick]| v.iter().take(MAX_LISTED).map(|t| time_json(*t, rate)).collect::<Vec<_>>();
    let pictures = if flag_p(p, "picture", true, CMD)? { count_p(p, "limit", 60, 1..=MAX_LISTED as u64, CMD)? } else { 0 };
    let ShotsCompared { rows, removed, truncated: more_shots, unlike_frames, tally } = compare_shots(s, (id, q, &mine), (other_id, other, &theirs), pictures);
    let mut notes: Vec<&str> = Vec::new();
    if unlike_frames {
        notes.push("the two sequences' frames are not the same shape (another size or aspect): their pictures were not compared, and `picture` is null");
    }
    if more_shots {
        notes.push("more shots than are listed: `summary` counts them all");
    }
    // what a shot shows: which footage, from where, how, and with what done to it
    let content = |q: &Sequence, sh: &Shot| {
        sh.clip.and_then(|c| q.find_item(c)).map(|(_, it)| (it.item, it.source_time_at(sh.start), it.speed, it.reverse, it.frame_hold, it.effects.clone()))
    };
    let same_shots = mine.len() == theirs.len()
        && mine.iter().zip(&theirs).all(|(a, b)| (a.start - b.start).abs() <= half && (a.end - b.end).abs() <= half && content(q, a) == content(other, b));
    Ok(json!({
        "shots": rows,
        "shotsOnlyThere": removed,
        "summary": {"same": tally[0], "changed": tally[1], "new": tally[2], "onlyThere": tally[3]},
        "notes": notes,
        "this": {"sequence": id.0, "rhythm": rhythm_json(&mine)},
        "other": {"sequence": other_id.0, "rhythm": rhythm_json(&theirs)},
        "secondsDifference": round3((end(&mine) - end(&theirs)).seconds()),
        "cutsShared": shared,
        "cutsOnlyHere": list(&only_here),
        "cutsOnlyThere": list(&only_there),
        "truncated": more_shots || only_here.len() > MAX_LISTED || only_there.len() > MAX_LISTED,
        "sameShots": same_shots,
        "identical": q.video_tracks == other.video_tracks && q.audio_tracks == other.audio_tracks && q.caption_tracks == other.caption_tracks,
        "note": "`sameShots`: the same stretches of the same footage at the same times, with the same effects on them. `identical`: nothing on any track differs at all, sound and titles included",
    }))
}
