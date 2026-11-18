//! `clip.speedRamp`: a smooth speed change in one command (#21), such as 100 % easing down to
//! 40 % for a slow-motion moment.
//!
//! The ramp is Time Remapping keyframes on the clip's Speed ([`filmagic_project::remap`]), which
//! the renderer plays: the media time shown at each moment is the integral of the speed curve, so
//! the speed glides instead of jumping between razored pieces.
//!
//! Two forms:
//!
//! - **points** `"segments":[{"at":ticks,"speed":percent,"ease":…}]`: at sequence time `at` the
//!   clip plays at `speed` (per cent of normal speed, 0 = hold the frame), easing from the point
//!   before; before the first point the clip plays at the first point's speed, after the last at
//!   the last one's.
//! - **from/to** `"from":percent?,"to":percent,"at":ticks?,"duration":ticks?`: from `from` (the
//!   clip's speed when left out) to `to`, over `duration` (1 s) starting at `at` (the clip's
//!   start).
//!
//! `ease` is `linear`, `in` (leaves the old speed gently), `out` (arrives at the new speed
//! gently), `inOut` (both; the default) or `hold` (jumps at `at`).
//!
//! `ripple:true` lets the clip's end move, so it still shows all of its media (a slowed clip gets
//! longer and later clips move; sync-locked tracks follow). `ripple:false` (the default) keeps the
//! clip's length: a slowed clip no longer reaches the end of its media, a sped-up one needs more
//! media and is refused when there is none.
//!
//! Linked sound (`audio`): `mute` (default) switches the clip's linked sound clips off (Clip ▸
//! Enable), since sound cannot follow a varying speed here; their length follows the picture's.
//! `keep` leaves them on at their own speed: they drift from the picture after the ramp starts.
//!
//! `clear:true` removes a ramp (the clip plays at its constant speed again).

use filmagic_project::{ClipId, Interpolation, Keyframe, ParamValue, TrackItem, TrackKind};
use filmagic_time::{FrameRate, Tick};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, NamedKeys, bad, has_seq};
use crate::{EngineError, Result, Session};

const CMD: &str = "clip.speedRamp";
/// Most points one ramp may have.
pub const MAX_POINTS: usize = 64;
/// Highest speed a point may ask for, per cent.
pub const MAX_SPEED: f64 = 10_000.0;

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: CMD,
        label: "Speed Ramp…",
        menu: &[],
        shortcut: None,
        params: r#"{"clip":id? (a video clip; default: the selected one),"segments":[{"at":ticks (sequence time),"speed":percent (0 = hold),"ease":"linear|in|out|inOut|hold"="inOut"}]? (points form),"from":percent? (default: the clip's speed),"to":percent? (from/to form),"at":ticks? (ramp start, sequence time; default: the clip's start),"duration":ticks? (ramp length; default 1 s),"ease":"linear|in|out|inOut|hold"="inOut","ripple":bool=false (let the clip's end move so it still shows all its media),"audio":"mute|keep"="mute" (linked sound),"clear":bool=false (remove the ramp)}"#,
        enabled: has_seq,
        run: speed_ramp,
        journal: true,
        named: NamedKeys::CLIP,
    }]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ease {
    Linear,
    In,
    Out,
    InOut,
    Hold,
}

impl Ease {
    fn parse(v: Option<&Value>) -> Result<Ease> {
        match v {
            None | Some(Value::Null) => Ok(Ease::InOut),
            Some(Value::String(s)) => match s.as_str() {
                "linear" => Ok(Ease::Linear),
                "in" | "easeIn" => Ok(Ease::In),
                "out" | "easeOut" => Ok(Ease::Out),
                "inOut" | "easeInOut" | "smooth" => Ok(Ease::InOut),
                "hold" | "step" => Ok(Ease::Hold),
                other => Err(bad(CMD, format!("unknown ease `{other}`: use linear, in, out, inOut or hold"))),
            },
            Some(_) => Err(bad(CMD, "`ease` must be a string: linear, in, out, inOut or hold")),
        }
    }
    fn name(self) -> &'static str {
        match self {
            Ease::Linear => "linear",
            Ease::In => "in",
            Ease::Out => "out",
            Ease::InOut => "inOut",
            Ease::Hold => "hold",
        }
    }
}

/// One point of the ramp: at sequence time `at` the clip plays at `speed` per cent, eased from the
/// point before by `ease`.
#[derive(Clone, Copy, Debug)]
struct Point {
    at: Tick,
    speed: f64,
    ease: Ease,
}

fn speed_of(v: Option<&Value>, key: &str) -> Result<Option<f64>> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            let x = v.as_f64().filter(|x| x.is_finite()).ok_or_else(|| bad(CMD, format!("`{key}` must be a number (per cent)")))?;
            if !(0.0..=MAX_SPEED).contains(&x) {
                return Err(bad(
                    CMD,
                    format!("`{key}` must be between 0 and {MAX_SPEED} per cent (0 holds the frame; to play backwards use clip.speedDuration with reverse)"),
                ));
            }
            Ok(Some(x))
        }
    }
}

fn ticks_of(v: Option<&Value>, key: &str) -> Result<Option<Tick>> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            v.as_i64().map(|t| Some(Tick(t))).ok_or_else(|| bad(CMD, format!("`{key}` must be a time: ticks, or {{\"seconds\"|\"frame\"|\"timecode\"}}")))
        }
    }
}

fn flag(p: &Value, key: &str, default: bool) -> Result<bool> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(bad(CMD, format!("`{key}` must be true or false"))),
    }
}

/// The points asked for, in timeline order, inside the clip `it`.
fn points(p: &Value, it: &TrackItem) -> Result<Vec<Point>> {
    let current = it.speed.abs() * 100.0;
    let from_to = ["from", "to", "duration"].iter().any(|k| p.get(*k).is_some_and(|v| !v.is_null()));
    let mut pts = match p.get("segments") {
        Some(Value::Array(a)) => {
            if from_to {
                return Err(bad(CMD, "give either `segments` or `from`/`to`/`duration`, not both"));
            }
            if a.is_empty() || a.len() > MAX_POINTS {
                return Err(bad(CMD, format!("`segments` must hold 1 to {MAX_POINTS} points")));
            }
            a.iter()
                .map(|seg| {
                    let Value::Object(_) = seg else { return Err(bad(CMD, "each segment is an object {\"at\",\"speed\",\"ease\"?}")) };
                    let at = ticks_of(seg.get("at"), "at")?.ok_or_else(|| bad(CMD, "each segment needs `at` (sequence time)"))?;
                    let speed = speed_of(seg.get("speed"), "speed")?.ok_or_else(|| bad(CMD, "each segment needs `speed` (per cent)"))?;
                    Ok(Point { at, speed, ease: Ease::parse(seg.get("ease"))? })
                })
                .collect::<Result<Vec<_>>>()?
        }
        Some(Value::Null) | None => {
            let to = speed_of(p.get("to"), "to")?.ok_or_else(|| bad(CMD, "need `segments`, or `to` (the speed to ramp to, per cent)"))?;
            let from = speed_of(p.get("from"), "from")?.unwrap_or(current);
            let at = ticks_of(p.get("at"), "at")?.unwrap_or(it.start);
            let dur = ticks_of(p.get("duration"), "duration")?.unwrap_or(Tick::from_seconds_f64(1.0));
            if dur <= Tick::ZERO {
                return Err(bad(CMD, "`duration` must be longer than zero"));
            }
            let end = Tick(at.0.checked_add(dur.0).ok_or_else(|| bad(CMD, "`at` + `duration` is out of range"))?);
            vec![Point { at, speed: from, ease: Ease::Linear }, Point { at: end, speed: to, ease: Ease::parse(p.get("ease"))? }]
        }
        Some(_) => return Err(bad(CMD, "`segments` must be a list of {\"at\",\"speed\",\"ease\"?}")),
    };
    if !pts.windows(2).all(|w| w[0].at < w[1].at) {
        return Err(bad(CMD, "the points must be in time order, each later than the one before"));
    }
    let (Some(first), Some(last)) = (pts.first().copied(), pts.last().copied()) else { return Err(bad(CMD, "need at least one point")) };
    if first.at < it.start || last.at > it.end() {
        return Err(bad(
            CMD,
            format!(
                "the ramp ({:.3}–{:.3} s) must lie within the clip ({:.3}–{:.3} s of the sequence); `at` is sequence time",
                first.at.seconds(),
                last.at.seconds(),
                it.start.seconds(),
                it.end().seconds()
            ),
        ));
    }
    // the clip's first frame is the anchor: it stays where it is
    if first.at > it.start {
        pts.insert(0, Point { at: it.start, speed: first.speed, ease: Ease::Linear });
    }
    // a hold is a jump: the old speed up to a tick before the point
    let mut out: Vec<Point> = Vec::with_capacity(pts.len() * 2);
    for (i, pt) in pts.iter().enumerate() {
        if i > 0 && pt.ease == Ease::Hold {
            let prev = out.last().copied().unwrap_or(*pt);
            let just_before = pt.at - Tick(1);
            if just_before > prev.at {
                out.push(Point { at: just_before, speed: prev.speed, ease: Ease::Linear });
            }
            out.push(Point { ease: Ease::Linear, ..*pt });
        } else {
            out.push(*pt);
        }
    }
    Ok(out)
}

/// Remap keyframes (linear media time, per cent of the clip's own speed) for `pts` on `it`.
fn keyframes(it: &TrackItem, pts: &[Point]) -> Vec<Keyframe> {
    let base = it.speed.abs().max(1e-9);
    let n = pts.len();
    // ease flags per point, in timeline order: (leaves gently, arrives gently)
    let mut leaves = vec![false; n];
    let mut arrives = vec![false; n];
    for i in 1..n {
        match pts[i].ease {
            Ease::In => leaves[i - 1] = true,
            Ease::Out => arrives[i] = true,
            Ease::InOut => {
                leaves[i - 1] = true;
                arrives[i] = true;
            }
            Ease::Linear | Ease::Hold => {}
        }
    }
    let mut keys: Vec<Keyframe> = pts
        .iter()
        .enumerate()
        .map(|(i, pt)| {
            let mut k = Keyframe::new(it.linear_source_time_at(pt.at), ParamValue::Float(pt.speed / base));
            // in media order a reversed clip's timeline segments run the other way
            let (out_ease, in_ease) = if it.reverse { (arrives[i], leaves[i]) } else { (leaves[i], arrives[i]) };
            k.interp = match (out_ease, in_ease) {
                (true, true) => Interpolation::AutoBezier,
                (true, false) => Interpolation::EaseOut,
                (false, true) => Interpolation::EaseIn,
                (false, false) => Interpolation::Linear,
            };
            k
        })
        .collect();
    keys.sort_by_key(|k| k.time);
    keys.dedup_by_key(|k| k.time);
    keys
}

fn time_json(t: Tick, rate: FrameRate) -> Value {
    json!({"seconds": round6(t.seconds()), "ticks": t.0, "frame": rate.frame_at(t)})
}

fn round6(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

/// A whole number of frames near `d`, at least one.
fn whole_frames(d: f64, fd: Tick) -> Tick {
    let f = fd.0.max(1) as f64;
    let n = (d / f).round().clamp(1.0, (Tick::MAX.0 / 4) as f64 / f);
    Tick((n * f) as i64)
}

/// The clip with the ramp `keys` (None: no ramp) and its new length and source In.
struct Retimed {
    item: TrackItem,
    /// Media the clip shows, lowest and highest.
    media: (Tick, Tick),
}

fn set_keys(it: &mut TrackItem, keys: Option<Vec<Keyframe>>) {
    if it.effect("time_remap").is_none()
        && let Some(def) = filmagic_project::find_effect("time_remap")
    {
        it.effects.push(def.instance());
    }
    if let Some(e) = it.effect_mut("time_remap") {
        e.enabled = true;
        if let Some(p) = e.param_mut("speed") {
            match keys {
                Some(k) => {
                    p.value = k.first().map(|k| k.value.clone()).unwrap_or(ParamValue::Float(100.0));
                    p.keyframes = k;
                }
                None => {
                    p.keyframes.clear();
                    p.value = ParamValue::Float(100.0);
                }
            }
        }
    }
}

/// The media the clip shows: (lowest, highest) media time.
fn shown_media(it: &TrackItem) -> (Tick, Tick) {
    let (a, b) = (it.source_time_at(it.start), it.source_out());
    if it.reverse {
        // a reversed clip starts a tick before its out point and ends at its source In
        let low = match it.remap() {
            Some(r) => r.shown(it.source_in),
            None => it.source_in,
        };
        (low, a + Tick(1))
    } else {
        (a, b)
    }
}

fn retime(old: &TrackItem, keys: Option<Vec<Keyframe>>, ripple: bool, fd: Tick, media_end: Option<Tick>) -> Result<Retimed> {
    let s = old.speed.abs();
    let (lo, hi) = shown_media(old);
    let mut it = old.clone();
    // the linear media time of the clip's first frame: fixed, so the ramp's keyframes stay put
    let top = old.linear_source_time_at(old.start);
    set_keys(&mut it, keys);
    if ripple {
        let needed = |it: &TrackItem, from: Tick, target: Tick, down: bool| -> Result<Tick> {
            match it.remap() {
                Some(r) => r.linear_reaching(from, target, down).ok_or_else(|| {
                    bad(CMD, "the ramp ends at 0 % (a hold): with `ripple` the clip would never reach the end of its media; end on a speed above 0, or pass ripple:false")
                }),
                None => Ok(target),
            }
        };
        if old.reverse {
            let low = needed(&it, top, lo, true)?;
            it.duration = whole_frames((top + Tick(1) - low).0 as f64 / s, fd);
            it.source_in = top + Tick(1) - Tick((it.duration.0 as f64 * s).round() as i64);
            // a rounded-up last frame must not run before the media's start
            // (a few ticks of rounding are not a frame: a reversed clip ends a tick short of its In)
            if shown_media(&it).0 < Tick(-4) && it.duration > fd {
                it.duration -= fd;
                it.source_in = top + Tick(1) - Tick((it.duration.0 as f64 * s).round() as i64);
            }
        } else {
            let end = needed(&it, old.source_in, hi, false)?;
            it.duration = whole_frames((end - old.source_in).0 as f64 / s, fd);
            // a rounded-up last frame must not run past the media
            if let Some(m) = media_end
                && it.source_out() > m
                && it.duration > fd
            {
                it.duration -= fd;
            }
        }
    }
    let media = shown_media(&it);
    if media.0 < Tick(-4) || media_end.is_some_and(|m| media.1 > m + Tick(4)) {
        return Err(bad(
            CMD,
            format!(
                "at this speed the clip needs {:.3} s of media it does not have{}",
                if media.0 < Tick(-4) { -media.0.seconds() } else { (media.1 - media_end.unwrap_or(media.1)).seconds() },
                if ripple { "" } else { "; pass ripple:true so the clip gets shorter instead, or ramp to a lower speed" }
            ),
        ));
    }
    Ok(Retimed { item: it, media })
}

/// A linked sound clip retimed with the picture: its length changes by `delta` (with ripple),
/// within its media, and `mute` switches it off.
fn retime_partner(p: &TrackItem, delta: Tick, ripple: bool, mute: bool, fd: Tick, media_end: Option<Tick>) -> TrackItem {
    let mut n = p.clone();
    if mute {
        n.enabled = false;
    }
    if ripple && delta != Tick::ZERO {
        let s = p.speed.abs().max(1e-9);
        let mut d = Tick(p.duration.0.saturating_add(delta.0)).max(fd.min(p.duration));
        if p.frame_hold.is_none() {
            if p.reverse {
                // keep the media at its start: its source In moves with the length
                let top = p.linear_source_time_at(p.start) + Tick(1);
                d = d.min(Tick((top.0 as f64 / s).floor() as i64)).max(Tick(1));
                n.source_in = top - Tick((d.0 as f64 * s).round() as i64);
            } else if let Some(m) = media_end {
                let room = Tick(((m - p.source_in).0 as f64 / s).floor() as i64);
                d = d.min(room.max(Tick(1)));
            }
        }
        n.duration = d;
    }
    n
}

fn speed_ramp(s: &mut Session, p: &Value) -> Result<Value> {
    if !p.is_object() && !p.is_null() {
        return Err(bad(CMD, "params must be an object"));
    }
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let rate = q.settings.frame_rate;
    let fd = rate.frame_duration();
    let clip = match p.get("clip") {
        None | Some(Value::Null) => {
            match s.state.selection.iter().copied().find(|c| q.find_item(*c).is_some_and(|(t, _)| q.track(t).is_some_and(|t| t.kind == TrackKind::Video))) {
                Some(c) => c,
                None => return Err(bad(CMD, "need `clip` (a video clip id), or select one")),
            }
        }
        Some(v) => ClipId(v.as_u64().ok_or_else(|| bad(CMD, "`clip` must be a clip id"))?),
    };
    let (tid, old) = q.find_item(clip).ok_or_else(|| bad(CMD, format!("no clip {} in the active sequence", clip.0)))?;
    let track = q.track(tid).ok_or_else(|| bad(CMD, "the clip's track is gone"))?;
    if track.kind != TrackKind::Video {
        return Err(bad(CMD, "a speed ramp is set on the picture: name the video clip (its linked sound follows)"));
    }
    if track.locked {
        return Err(EngineError::Edit(filmagic_edit::EditError::Locked));
    }
    if old.frame_hold.is_some() {
        return Err(bad(CMD, "the clip is a frame hold (it shows one frame): remove the hold (clip.frameHoldOptions) before ramping its speed"));
    }
    if !(old.speed.is_finite() && old.speed.abs() > 1e-9) {
        return Err(bad(CMD, "the clip's own speed is zero or not a number; set it with clip.speedDuration first"));
    }
    let old = old.clone();
    let clear = flag(p, "clear", false)?;
    let ripple = flag(p, "ripple", false)?;
    let mute = match p.get("audio") {
        None | Some(Value::Null) => true,
        Some(Value::String(a)) if a == "mute" => true,
        Some(Value::String(a)) if a == "keep" => false,
        Some(_) => return Err(bad(CMD, "`audio` must be \"mute\" or \"keep\"")),
    };
    let pts = if clear { Vec::new() } else { points(p, &old)? };
    let media_end = crate::media_duration(&s.project, &s.media, old.item);
    let keys = (!clear).then(|| keyframes(&old, &pts));
    let new = retime(&old, keys, ripple, fd, media_end)?;
    let delta = new.item.duration - old.duration;
    // linked sound on unlocked tracks follows; on a locked track it stays as it is
    let mut partners: Vec<TrackItem> = Vec::new();
    let mut locked_partners = 0usize;
    if let Some(link) = old.link {
        for tr in q.audio_tracks.iter() {
            for a in tr.items.iter().filter(|a| a.link == Some(link) && a.id != old.id) {
                if tr.locked {
                    locked_partners += 1;
                    continue;
                }
                let media = crate::media_duration(&s.project, &s.media, a.item);
                partners.push(retime_partner(a, delta, ripple, mute && !clear, fd, media));
            }
        }
    }
    let mut warnings: Vec<String> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    let old_media = shown_media(&old);
    if !clear && !ripple {
        let lost = (old_media.1 - old_media.0) - (new.media.1 - new.media.0);
        if lost > fd {
            warnings.push(format!(
                "the clip keeps its length, so the last {:.2} s of its media no longer show; pass ripple:true to let the clip grow and keep them",
                lost.seconds()
            ));
        }
    }
    if !partners.is_empty() && !clear {
        if mute {
            notes
                .push("the linked sound was switched off (sound does not follow a varying speed); clip.enable turns it back on, or pass audio:\"keep\"".into());
        } else {
            warnings.push("the linked sound plays at its own speed, so it drifts from the picture after the ramp starts".into());
        }
    }
    if clear && !partners.is_empty() && partners.iter().any(|a| !a.enabled) {
        notes.push("the linked sound stays as it is: clip.enable turns switched-off sound back on".into());
    }
    if locked_partners > 0 {
        warnings.push(format!("{locked_partners} linked sound clip(s) on locked tracks were left as they are"));
    }
    if new.item.effects.iter().any(|e| e.effect != "time_remap" && e.is_animated()) {
        notes.push("keyframes of the clip's other effects follow its media, so they move with the ramp".into());
    }
    if !clear {
        notes.push("the ramp is Time Remapping keyframes on the clip's Speed; clip.speedRamp with clear:true removes it".into());
    }
    let end_before = old.end().max(new.item.end());
    let before = crate::story::looks_across(s, old.start, end_before);
    let mut retimed = vec![new.item.clone()];
    retimed.extend(partners.iter().cloned());
    let label = if clear { "Remove Speed Ramp" } else { "Speed Ramp" };
    s.edit_sequence(label, |q, _, _| filmagic_edit::retime_group(q, &retimed, ripple).map_err(EngineError::from))?;
    let after = crate::story::looks_across(s, old.start, end_before.max(new.item.end()));
    let speeds: Vec<Value> = pts
        .iter()
        .map(|pt| json!({"at": time_json(pt.at, rate), "clipSeconds": round6((pt.at - old.start).seconds()), "speed": pt.speed, "ease": pt.ease.name()}))
        .collect();
    let it = &new.item;
    Ok(json!({
        "clip": clip.0,
        "cleared": clear,
        "points": speeds,
        "keyframes": it.effect("time_remap").and_then(|e| e.param("speed")).map_or(0, |p| p.keyframes.len()),
        "duration": time_json(it.duration, rate),
        "previousDuration": time_json(old.duration, rate),
        "end": time_json(it.end(), rate),
        "rippled": ripple && delta != Tick::ZERO,
        "laterClipsMoved": if ripple { round6(delta.seconds()) } else { 0.0 },
        "media": {"from": round6(new.media.0.seconds()), "to": round6(new.media.1.seconds()), "seconds": round6((new.media.1 - new.media.0).seconds())},
        "audio": {"clips": partners.iter().map(|a| a.id.0).collect::<Vec<_>>(), "action": if partners.is_empty() { "none" } else if clear { "unchanged" } else if mute { "muted" } else { "kept" }},
        "warnings": warnings,
        "notes": notes,
        "pictureChange": crate::story::change_json(&before, &after),
    }))
}
