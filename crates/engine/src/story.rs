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

use filmcraft_project::{ClipId, ItemId, ItemKind, Label, Marker, MarkerId, MarkerKind, Project, Sequence, TrackItem, TrackKind};
use filmcraft_render::scene::{Signature, difference, signature};
use filmcraft_render::story::{Attention, Change, Glance, Pace, Sheet, attention, contact_sheet, drift, eye_travel, medoid, rhythm, screen_direction, settles};
use filmcraft_time::{FrameRate, Tick};
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
            params: r#"{"path":str,"item":id? (frames across this clip's rushes; default: one frame per shot of the active sequence),"count":n=12 (item),"fromSeconds":f64?,"toSeconds":f64?,"columns":n=4,"width":px=320,"max":n=48,"labels":bool=true}"#,
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
        query("story.compare", "Compare With Another Cut", r#"{"with":sequenceItemId}"#, compare),
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

/// Refuse a board whose `cells` frames of `width`×`height` pixels would exceed what one sheet holds,
/// before any frame is rendered.
fn board_budget(cells: usize, width: f64, size: (u32, u32), cmd: &str) -> Result<()> {
    let height = width * f64::from(size.1.max(1)) / f64::from(size.0.max(1));
    if cells as f64 * width * height > filmcraft_render::story::MAX_SHEET_PIXELS as f64 {
        return Err(bad(cmd, "the board would be too large; lower `width`, `max` or `count`"));
    }
    Ok(())
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

/// The shots of a sequence, in order, without leading or trailing black.
pub fn shots(p: &Project, q: &Sequence) -> Result<Vec<Shot>> {
    // top track first: the clip seen is the topmost one
    let layers: Vec<Vec<(Tick, Tick, ClipId)>> = q
        .video_tracks
        .iter()
        .rev()
        .filter(|t| t.enabled)
        .map(|t| {
            let mut v: Vec<(Tick, Tick, ClipId)> = t.items.iter().filter(|it| is_footage(p, it)).map(|it| (it.start, it.end(), it.id)).collect();
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
        let mut top = None;
        for (layer, i) in layers.iter().zip(at.iter_mut()) {
            while layer.get(*i).is_some_and(|c| c.1 <= a) {
                *i += 1;
            }
            if let Some(c) = layer.get(*i).filter(|c| c.0 <= a) {
                top = Some(c.2);
                break;
            }
        }
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
    let n_windows = ((total / window).ceil() as usize).clamp(1, 600);
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
            let len = (total - i as f64 * window).min(window).max(1e-6);
            let cpm = f64::from(*k) * 60.0 / len;
            json!({"startSeconds": round3(t0.seconds() + i as f64 * window), "cuts": k, "cutsPerMinute": round3(cpm), "pace": Pace::of(cpm).label()})
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
        notes.push(format!("{frantic} window(s) cut faster than 40 a minute, beyond the range of blink rates: the audience cannot read each shot there"));
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
}

fn look(s: &Session, scale: f32, t: Tick) -> Result<Look> {
    let img = s.try_render_program_at(scale, t)?;
    let rgba = img.over_black_rgba8();
    Ok(Look { glance: Glance::from_rgba(&rgba, img.w, img.h), sig: signature(&rgba, img.w, img.h) })
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
    let mix = filmcraft_render::audio::mix_sequence(&s.project, q, start, frames, &provider);
    let n = mix.frames();
    let chans = mix.channels.len().max(1) as f32;
    let mono: Vec<f32> = (0..n).map(|i| mix.channels.iter().map(|c| c.get(i).copied().unwrap_or(0.0)).sum::<f32>() / chans).collect();
    let db = filmcraft_render::story::levels_db(&mono, sr);
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

/// The audio clip linked to a video clip, if any.
fn linked_audio(q: &Sequence, clip: ClipId) -> Option<&TrackItem> {
    let link = q.find_item(clip)?.1.link?;
    q.audio_tracks.iter().flat_map(|t| t.items.iter()).find(|it| it.link == Some(link))
}

/// How sound and picture change at a cut from `out` to `inc` at `t`: together ("straight"), sound
/// trailing the picture ("L"), sound leading it ("J"), both ("L+J"), or `None` without linked sound.
fn split_state(q: &Sequence, out: Option<ClipId>, inc: Option<ClipId>, t: Tick, half_frame: Tick) -> Option<&'static str> {
    let trailing = out.and_then(|c| linked_audio(q, c)).map(|a| a.end() > t + half_frame);
    let leading = inc.and_then(|c| linked_audio(q, c)).map(|a| a.start < t - half_frame);
    match (trailing, leading) {
        (None, None) => None,
        (l, j) => Some(match (l.unwrap_or(false), j.unwrap_or(false)) {
            (false, false) => "straight",
            (true, false) => "L",
            (false, true) => "J",
            (true, true) => "L+J",
        }),
    }
}

/// The video transition a cut at `t` sits in, if any: (effect id, start, end).
fn transition_at(q: &Sequence, t: Tick) -> Option<(String, Tick, Tick)> {
    q.video_tracks.iter().flat_map(|tr| tr.transitions.iter()).find(|x| x.start <= t && t <= x.end()).map(|x| (x.effect.effect.clone(), x.start, x.end()))
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
    let transition = transition_at(q, t);
    let (leave, arrive) = transition.as_ref().map_or((t, t), |(_, a, b)| ((*a).max(before.start + fd).min(t), (*b).min(after.end - fd).max(t)));
    let out1 = look(s, scale, (leave - fd).max(before.start))?;
    let out0 = look(s, scale, (leave - fd - fd).max(before.start))?;
    let in0 = look(s, scale, arrive)?;
    let in1 = look(s, scale, (arrive + fd).min((after.end - fd).max(arrive)))?;
    let diff = difference(&out1.sig, &in0.sig);
    let change = Change::of(diff);
    let (eye_out, eye_in): (Attention, Attention) = (attention(&out1.glance, Some(&out0.glance)), attention(&in0.glance, Some(&in1.glance)));
    let travel = (eye_out.strength > 0.0 && eye_in.strength > 0.0).then(|| eye_travel(eye_out, eye_in));
    let (dir_out, dir_in) = (screen_direction(drift(&out0.glance, &out1.glance)), screen_direction(drift(&in0.glance, &in1.glance)));
    let reverses = dir_out != 0 && dir_in != 0 && dir_out != dir_in;
    let pause = sound_at(s, q, t);
    let split = split_state(q, before.clip, after.clip, t, half);
    let frames = |sh: &Shot| (sh.end - sh.start).0 / fd.0.max(1);
    let flash = frames(before).min(frames(after)) < 8;

    let mut issues: Vec<String> = Vec::new();
    match change {
        Change::Jump => issues.push(
            "jump cut: the picture changes too little to read as a new shot and too much to pass as the same one; cut to a clearly different framing, or do not cut".into(),
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
        "change": {"difference": round3(f64::from(diff)), "kind": change.label()},
        "eye": {
            "out": [round3(f64::from(eye_out.x)), round3(f64::from(eye_out.y))],
            "in": [round3(f64::from(eye_in.x)), round3(f64::from(eye_in.y))],
            "travel": travel.map(|d| round3(f64::from(d))),
        },
        "direction": {"out": dir_out, "in": dir_in, "reverses": reverses},
        "light": {"out": round3(f64::from(out1.glance.mean())), "in": round3(f64::from(in0.glance.mean()))},
        "sound": {"pause": pause, "edit": split},
        "transition": transition.map(|x| x.0),
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
    let next = (matched > pairs.len()).then(|| pairs.last().map(|(_, b)| round3(b.start.seconds() + rate.frame_duration().seconds()))).flatten();
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
            "straightCutsOnAPause": straight_on_pause,
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

fn phrase_points(pr: &Probe, min_pause: f64) -> Vec<BlinkPoint> {
    let Some(sound) = &pr.sound else { return Vec::new() };
    let sr = pr.sample_rate;
    let step = filmcraft_render::story::level_step(sr);
    let origin = pr.from - pr.lead;
    let (a, b) = (origin.to_units_floor(i64::from(sr)), pr.to.to_units_floor(i64::from(sr)));
    // five seconds at a time, in whole level steps, so a long clip never sits in memory at once
    let chunk = (sr as usize * 5 / step).max(1) * step;
    let mut levels: Vec<f32> = Vec::new();
    let mut at = a;
    while at < b {
        let n = usize::try_from(b - at).unwrap_or(0).min(chunk);
        let n = n / step * step;
        if n == 0 {
            break;
        }
        match sound(at, n) {
            Some(mono) => levels.extend(filmcraft_render::story::levels_db(&mono, sr)),
            None => levels.extend(std::iter::repeat_n(-100.0, n / step)),
        }
        at += n as i64;
    }
    let step_s = step as f64 / f64::from(sr);
    let fd = pr.rate.frame_duration();
    filmcraft_render::story::pauses_in_levels(&levels, step_s, min_pause, 0.4)
        .into_iter()
        .map(|pz| {
            let (ps, pe) = (origin + Tick::from_seconds_f64(pz.start), origin + Tick::from_seconds_f64(pz.end));
            // just after the phrase ends: two frames in, never past the middle of the pause
            let t = pr.rate.snap((ps + fd + fd).min(ps + Tick((pe - ps).0 / 2)));
            let strength = 0.6 * ((pz.end - pz.start) / 1.0).clamp(0.0, 1.0) + 0.4 * (f64::from(pz.drop_db) / 30.0).clamp(0.0, 1.0);
            BlinkPoint { t, kind: "phrase", strength, pause: Some((ps, pe)) }
        })
        .collect()
}

fn movement_points(pr: &Probe) -> Vec<BlinkPoint> {
    let Some(picture) = &pr.picture else { return Vec::new() };
    let fd = pr.rate.frame_duration();
    // about eight looks a second are enough to see a movement start and stop
    let stride = ((pr.rate.as_f64() / 8.0).round() as i64).clamp(1, 60);
    let step = Tick(fd.0.saturating_mul(stride).max(1));
    let n = ((pr.to - pr.from).0 / step.0).clamp(0, (MAX_BLINK_SECONDS * 8.0) as i64 + 16);
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
    settles(&activity)
        .into_iter()
        .map(|(i, strength)| BlinkPoint { t: pr.from + Tick(step.0.saturating_mul(i as i64)), kind: "movement", strength: f64::from(strength), pause: None })
        .collect()
}

fn find_blink_points(pr: &Probe, min_pause: f64) -> Vec<BlinkPoint> {
    let mut points = phrase_points(pr, min_pause);
    let near = Tick(pr.rate.frame_duration().0.saturating_mul(4));
    for m in movement_points(pr) {
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
    points
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

fn frame_scale(src: &filmcraft_media::SharedSource) -> f32 {
    src.info().video.as_ref().map_or(1.0, |v| (ANALYSIS_SIDE / v.width.max(v.height).max(1) as f32).min(1.0))
}

/// A probe for a timeline clip: its own picture, and its (or its linked partner's) sound.
fn clip_probe(s: &Session, p: &Value, clip: ClipId, picture: bool) -> Result<Probe> {
    const CMD: &str = "story.blinkPoints";
    let (_, q) = active(s, CMD)?;
    let (track, it) = q.find_item(clip).ok_or_else(|| bad(CMD, format!("no clip {} in the active sequence", clip.0)))?;
    let is_video = q.track(track).is_some_and(|t| t.kind == TrackKind::Video);
    let partner = |kind: TrackKind| -> Option<&TrackItem> {
        let link = it.link?;
        q.tracks(kind).iter().flat_map(|t| t.items.iter()).find(|x| x.link == Some(link) && x.id != it.id)
    };
    let (video, audio) = if is_video { (Some(it), partner(TrackKind::Audio)) } else { (partner(TrackKind::Video), Some(it)) };
    let (from, to) = blink_span(p, it.start, it.end(), CMD)?;
    let sr = q.settings.sample_rate.clamp(8_000, 192_000);
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let sound = audio.filter(|a| s.source(a.item).is_some_and(|src| src.info().has_audio())).map(|a| {
        let a = a.clone();
        Box::new(move |start: i64, frames: usize| filmcraft_render::audio::clip_signal(&a, start, frames, sr, &provider).map(|ch| mono(&ch, frames)))
            as Box<dyn Fn(i64, usize) -> Option<Vec<f32>>>
    });
    let picture = video.filter(|_| picture).and_then(|v| {
        let src = s.source(v.item)?;
        src.info().video.as_ref()?;
        let (v, scale) = (v.clone(), frame_scale(&src));
        Some(Box::new(move |t: Tick| {
            let f = src.video_frame(filmcraft_media::FrameRequest { time: v.source_time_at(t), scale }).ok()?;
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
    let duration = crate::commands::full_duration(s, pi);
    let (from, to) = blink_span(p, Tick::ZERO, duration, CMD)?;
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
            let f = src.video_frame(filmcraft_media::FrameRequest { time: t, scale }).ok()?;
            Some((f.to_rgba8(), f.width as usize, f.height as usize))
        }) as Box<dyn Fn(Tick) -> Option<(Vec<u8>, usize, usize)>>
    });
    Ok(Probe { from, to, lead: from.clamp(Tick::ZERO, Tick::from_seconds_f64(2.0)), rate, sound, sample_rate: sr, picture, media_time: None })
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
    let points = find_blink_points(&probe, min_pause);
    let rate = probe.rate;
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
        "analysed": {"sound": probe.sound.is_some(), "picture": probe.picture.is_some()},
        "truncated": points.len() > MAX_LISTED,
        "points": listed,
        "markers": added,
        "note": "each point is a place a cut could go: where a thought or a movement completes. Cutting before a phrase ends keeps the audience on the words; holding past it lets them read the face. Which branch to take is the editor's choice",
    }))
}

/// A board cell before it is laid out.
struct Cell {
    image: filmcraft_render::Image,
    label: String,
    info: Value,
}

fn clock(t: Tick) -> String {
    let sec = t.seconds().max(0.0) as u64;
    format!("{}:{:02}", sec / 60, sec % 60)
}

/// One cell per shot of the active sequence: the frame most like the others sampled in the shot.
fn sequence_cells(s: &Session, p: &Value, width: f64, max: usize) -> Result<(Vec<Cell>, bool)> {
    const CMD: &str = "story.board";
    let (_, q) = active(s, CMD)?;
    let rate = q.frame_rate();
    let (from, to) = (seconds_bound(p, "fromSeconds", CMD)?, seconds_bound(p, "toSeconds", CMD)?);
    let all = shots(&s.project, q)?;
    let wanted: Vec<(usize, &Shot)> =
        all.iter().enumerate().filter(|(_, sh)| sh.clip.is_some() && from.is_none_or(|f| sh.end > f) && to.is_none_or(|x| sh.start < x)).collect();
    let truncated = wanted.len() > max;
    board_budget(wanted.len().min(max), width, (q.settings.width, q.settings.height), CMD)?;
    let scale = (width / f64::from(q.settings.width.max(1))).clamp(0.01, 1.0) as f32;
    let mut cells = Vec::new();
    for (i, sh) in wanted.into_iter().take(max) {
        // five looks across the shot (never its very first or last frame: those sit in transitions)
        let len = (sh.end - sh.start).0;
        let mut frames: Vec<(Tick, filmcraft_render::Image, Signature)> = Vec::new();
        for k in 1..=5 {
            let t = rate.snap(sh.start + Tick(len / 6 * k));
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
    Ok((cells, truncated))
}

/// `count` frames spread evenly across a project item's media.
fn item_cells(s: &Session, p: &Value, item: ItemId, width: f64, count: usize) -> Result<Vec<Cell>> {
    const CMD: &str = "story.board";
    let pi = s.project.item(item).ok_or_else(|| bad(CMD, format!("no project item {}", item.0)))?;
    let duration = crate::commands::full_duration(s, pi);
    let (from, to) = (
        seconds_bound(p, "fromSeconds", CMD)?.map_or(Tick::ZERO, |t| t.min(duration)),
        seconds_bound(p, "toSeconds", CMD)?.map_or(duration, |t| t.min(duration)),
    );
    if to <= from {
        return Err(bad(CMD, "nothing to show between `fromSeconds` and `toSeconds`"));
    }
    let rate = pi.frame_rate().sane();
    let (w, h) = filmcraft_render::source_size(&s.project, item).ok_or_else(|| bad(CMD, format!("`{}` has no picture", pi.name)))?;
    board_budget(count, width, (w, h), CMD)?;
    let scale = (width / f64::from(w.max(1))).clamp(0.01, 1.0) as f32;
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let mut cells = Vec::new();
    for k in 0..count {
        // the middle of each of `count` equal parts
        let t = rate.snap(from + Tick((to - from).0 / (count as i64 * 2) * (2 * k as i64 + 1)));
        let Some(image) = filmcraft_render::render_item(&s.project, item, t, scale, &provider) else { continue };
        cells.push(Cell { image, label: format!("{}  {}", k + 1, clock(t)), info: json!({"index": k + 1, "at": time_json(t, rate)}) });
    }
    if cells.is_empty() {
        return Err(bad(CMD, format!("no frame of `{}` could be decoded", pi.name)));
    }
    Ok(cells)
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
    let columns = count_p(p, "columns", 4, 1..=12, CMD)?;
    let width = count_p(p, "width", 320, 64..=1280, CMD)? as f64;
    let labels = flag_p(p, "labels", true, CMD)?;
    let (cells, truncated, of) = match item_p(p, "item") {
        Some(item) => (item_cells(s, p, item, width, count_p(p, "count", 12, 1..=96, CMD)?)?, false, "item"),
        None => {
            let (c, t) = sequence_cells(s, p, width, count_p(p, "max", 48, 1..=96, CMD)?)?;
            (c, t, "sequence")
        }
    };
    let Some(first) = cells.first() else { return Err(bad(CMD, "there is no picture to lay out")) };
    // a source may decode a little smaller or larger than asked: the cell is the asked width, shaped like the frame
    let cell_w = width as usize;
    let cell_h = ((width * first.image.h as f64 / first.image.w.max(1) as f64).round() as usize).clamp(8, 4 * cell_w);
    let sheet = Sheet { columns: columns.min(cells.len()), cell_w, cell_h, gap: 6 };
    let info: Vec<Value> = cells.iter().map(|c| c.info.clone()).collect();
    let laid: Vec<(filmcraft_render::Image, String)> = cells.into_iter().map(|c| (c.image, if labels { c.label } else { String::new() })).collect();
    let img = contact_sheet(&laid, sheet).ok_or_else(|| bad(CMD, "the board would be too large; lower `width`, `max` or `count`"))?;
    let bytes = filmcraft_export::still::encode(&img, filmcraft_export::still::StillFormat::Png, 8).map_err(|e| EngineError::Other(e.to_string()))?;
    s.services.write_file(&path, &bytes).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    Ok(json!({
        "path": path,
        "of": of,
        "width": img.w,
        "height": img.h,
        "columns": sheet.columns,
        "truncated": truncated,
        "cells": info,
        "note": "read it like a page: left to right, top to bottom. Frames that sit far apart in the film sit side by side here",
    }))
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
    let (a_out, a_in) = (
        linked_audio(q, out).ok_or_else(|| bad(CMD, "the outgoing clip has no linked sound to split from its picture"))?,
        linked_audio(q, inc).ok_or_else(|| bad(CMD, "the incoming clip has no linked sound to split from its picture"))?,
    );
    // L: sound trails (the sound cut moves later, or the picture cut earlier). J: the reverse.
    let (left, right, delta) = match (roll_video, kind) {
        (false, "L") => (a_out.id, a_in.id, frames),
        (false, _) => (a_out.id, a_in.id, -frames),
        (true, "L") => (out, inc, -frames),
        (true, _) => (out, inc, frames),
    };
    if !roll_video && a_out.end() != a_in.start {
        return Err(bad(
            CMD,
            "the two clips' sounds do not meet at one cut on one track (already split, or on different tracks); roll them with `timeline.roll`",
        ));
    }
    let roll = crate::commands::find("timeline.roll").ok_or_else(|| EngineError::UnknownCommand("timeline.roll".into()))?.run;
    let r = s.edit_group("Split Edit", |s| {
        let r = roll(s, &json!({"left": left.0, "right": right.0, "deltaFrames": delta, "followLinks": false}))?;
        if r["delta"].as_i64().unwrap_or(0) == 0 {
            return Err(bad(CMD, "the clips have no spare media beyond the cut to extend into (no handles)"));
        }
        Ok(r)
    })?;
    let moved = Tick(r["delta"].as_i64().unwrap_or(0));
    Ok(json!({
        "kind": kind,
        "cut": time_json(cut, rate),
        "rolled": if roll_video { "video" } else { "audio" },
        "left": left.0,
        "right": right.0,
        "frames": moved.0 / rate.frame_duration().0.max(1),
        "asked": delta,
        "clamped": moved.0 / rate.frame_duration().0.max(1) != delta,
    }))
}

fn shadow_cut(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "story.shadowCut";
    let (id, _) = active(s, CMD)?;
    let item = s.project.item(id).ok_or_else(|| bad(CMD, "no active sequence"))?.clone();
    let name = match p.get("name") {
        None | Some(Value::Null) => {
            let prefix = format!("{} (shadow cut ", item.name);
            let n = s.project.items.values().filter(|i| i.name.starts_with(&prefix)).count() + 1;
            format!("{prefix}{n})")
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
    Ok(json!({
        "this": {"sequence": id.0, "rhythm": rhythm_json(&mine)},
        "other": {"sequence": other_id.0, "rhythm": rhythm_json(&theirs)},
        "secondsDifference": round3((end(&mine) - end(&theirs)).seconds()),
        "cutsShared": shared,
        "cutsOnlyHere": list(&only_here),
        "cutsOnlyThere": list(&only_there),
        "truncated": only_here.len() > MAX_LISTED || only_there.len() > MAX_LISTED,
        "identical": only_here.is_empty() && only_there.is_empty() && mine.len() == theirs.len(),
    }))
}
