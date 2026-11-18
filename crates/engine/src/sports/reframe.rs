//! `clip.autoReframe`: keep the subject in a crop of another aspect (9:16 for TikTok and Shorts)
//! by writing Motion Position and Scale keyframes (see [`super`]).
//!
//! The subject is found per sample at reduced size: by motion saliency (`"auto"`), or by the
//! template tracker from a point or box the caller gives. The crop's centre follows it through a
//! dead zone, a zero-phase critically damped spring and a pan-speed limit ([`super::path`]), so
//! the camera holds while the subject stays near the middle and eases when it has to move. The
//! path is written as Motion keyframes, thinned to those linear interpolation needs, on the clip
//! (`"output":"keyframes"`, in a sequence that already has the target aspect) or on the clips of a
//! new sequence of the target aspect (`"output":"sequence"`, Premiere's Auto Reframe Sequence).

use std::collections::HashMap;

use filmagic_geom::Vec2;
use filmagic_project::{ClipId, ItemId, Param, ParamValue, Project, TrackItem, TrackKind};
use filmagic_time::Tick;
use serde_json::{Value, json};

use super::{Analysis, ClipView, Request as JobRequest, STOPPED, Seed, lock, num, path, point, spans, tick_param, wait_p};
use crate::commands::bad;
use crate::{EngineError, Result, Session};

const CMD: &str = "clip.autoReframe";
/// Width of the frames motion is measured in.
const MOTION_WIDTH: f64 = 192.0;
/// Width of the frames a seeded subject is followed in.
const TRACK_WIDTH: f64 = 480.0;
/// Most samples one job analyses (all clips together).
const MAX_SAMPLES: usize = 20_000;
/// Tolerance of the keyframe thinning, in sequence pixels.
const KEY_TOLERANCE: f64 = 0.25;

pub(crate) const PARAMS: &str = r#"{"clip":id?,"clips":[id]? (the video clips to reframe; default the selected ones with "keyframes", every video clip with media with "sequence"),"output":"keyframes"|"sequence"="keyframes" (keyframes: Motion keyframes on the clip, in a sequence that already has the target aspect; sequence: a new sequence of the target aspect holding copies of the clips, reframed),"aspect":"9:16"|"4:5"|"1:1"|"w:h"? (default: the sequence's own with "keyframes", 9:16 with "sequence"),"name":str? (the new sequence's name),"graphics":"follow"|"keep"="follow" (with "sequence": a graphic over a reframed clip gets the same crop, so an overlay stays on what it marks; keep: graphics stay as they are),"start":ticks?,"end":ticks? (sequence time; default each clip whole),"subject":{"point":[x,y]|"box":[x,y,w,h],"at":ticks?,"size":px=64}|"auto"="auto" (auto: what moves, a heuristic; point (an object `size` big) or box: follow what is there at `at`, default the playhead; sequence pixels),"smoothing":0..1=0.5,"deadZone":0..0.45=0.08 (share of the crop the subject may move in before the camera follows),"maxPan":crops/s=1.5 (fastest pan, in crop widths per second),"zoom":1..4=1 (crop tighter than the largest that fits),"sampleEvery":frames? (default about 10 samples a second; 15 when following a point),"wait":bool=false} → per clip: samples, coverage (share of samples the subject was found in), inFrame (share of those it stayed inside the crop), subjectLeftFrame [{start, end}] and subjectLost [{start, end}] (seconds), keyframes, scale; the new sequence with "sequence"; pictureChange"#;

#[derive(Clone, Debug)]
enum Mode {
    Keyframes,
    Sequence { name: String, width: u32, height: u32, follow: bool },
}

pub(crate) struct Request {
    mode: Mode,
    warnings: Vec<String>,
    notes: Vec<String>,
}

/// What to write on one clip.
pub(crate) struct ClipPlan {
    clip: ClipId,
    /// (effect time, Motion Position)
    keys: Vec<(Tick, Vec2)>,
    scale: f64,
    anchor: Vec2,
    /// The sequence time analysed.
    span: (Tick, Tick),
    report: Value,
}

#[derive(Clone, Copy, Debug)]
struct Opts {
    smoothing: f64,
    dead_zone: f64,
    max_pan: f64,
    zoom: f64,
}

struct Work {
    view: ClipView,
    src: filmagic_media::SharedSource,
    times: Vec<Tick>,
    step: Tick,
    seed: Option<(usize, Seed)>,
}

/// `"w:h"` as a ratio (width ÷ height).
fn aspect_of(v: &Value) -> Result<f64> {
    let s = v.as_str().ok_or_else(|| bad(CMD, format!("`aspect` must be text like \"9:16\", got {v}")))?;
    let (a, b) = s.split_once([':', 'x', '/']).ok_or_else(|| bad(CMD, format!("`aspect` must be \"w:h\" like \"9:16\", got \"{s}\"")))?;
    let (a, b) = (a.trim().parse::<f64>().ok(), b.trim().parse::<f64>().ok());
    match (a, b) {
        (Some(a), Some(b)) if a.is_finite() && b.is_finite() && a > 0.0 && b > 0.0 && (0.1..=10.0).contains(&(a / b)) => Ok(a / b),
        _ => Err(bad(CMD, format!("`aspect` \"{s}\" must be two positive numbers whose ratio is between 1:10 and 10:1"))),
    }
}

/// The new sequence's frame: the source sequence's short side kept, the long side from `aspect`,
/// both even.
fn reframed_size(w: u32, h: u32, aspect: f64) -> (u32, u32) {
    let short = f64::from(w.min(h).max(2));
    let even = |x: f64| ((x / 2.0).round() as u32).clamp(1, 4096) * 2;
    if aspect < 1.0 { (even(short), even(short / aspect)) } else { (even(short * aspect), even(short)) }
}

pub(crate) fn run(s: &mut Session, p: &Value) -> Result<Value> {
    if !p.is_object() && !p.is_null() {
        return Err(bad(CMD, "parameters must be an object"));
    }
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (sw, sh) = (q.settings.width, q.settings.height);
    let rate = q.settings.frame_rate;
    let fd = rate.frame_duration().max(Tick(1));
    let wait = wait_p(p, CMD)?;
    let opts = Opts {
        smoothing: num(p, "smoothing", CMD, 0.5, 0.0, 1.0)?,
        dead_zone: num(p, "deadZone", CMD, 0.08, 0.0, 0.45)?,
        max_pan: num(p, "maxPan", CMD, 1.5, 0.01, 100.0)?,
        zoom: num(p, "zoom", CMD, 1.0, 1.0, 4.0)?,
    };
    let aspect = p.get("aspect").filter(|v| !v.is_null()).map(aspect_of).transpose()?;
    let mode = match p.get("output").filter(|v| !v.is_null()) {
        None => Mode::Keyframes,
        Some(v) => match v.as_str() {
            Some("keyframes") => Mode::Keyframes,
            Some("sequence") => {
                let (width, height) = reframed_size(sw, sh, aspect.unwrap_or(9.0 / 16.0));
                let seq_name = s.project.item(seq_id).map(|i| i.name.clone()).unwrap_or_default();
                let label = p.get("aspect").and_then(Value::as_str).unwrap_or("9:16");
                let name = match p.get("name") {
                    None | Some(Value::Null) => format!("{seq_name} ({label})"),
                    Some(Value::String(n)) if !n.trim().is_empty() && n.chars().count() <= 256 => n.trim().to_string(),
                    Some(v) => return Err(bad(CMD, format!("`name` must be a non-empty text of at most 256 characters, got {v}"))),
                };
                let follow = match p.get("graphics") {
                    None | Some(Value::Null) => true,
                    Some(v) => match v.as_str() {
                        Some("follow") => true,
                        Some("keep") => false,
                        _ => return Err(bad(CMD, format!("`graphics` must be \"follow\" or \"keep\", got {v}"))),
                    },
                };
                Mode::Sequence { name, width, height, follow }
            }
            _ => return Err(bad(CMD, format!("`output` must be \"keyframes\" or \"sequence\", got {v}"))),
        },
    };
    let (ow, oh) = match &mode {
        Mode::Keyframes => (sw, sh),
        Mode::Sequence { width, height, .. } => (*width, *height),
    };
    if let (Mode::Keyframes, Some(a)) = (&mode, aspect) {
        let own = f64::from(sw) / f64::from(sh.max(1));
        if (a / own - 1.0).abs() > 0.01 {
            return Err(bad(
                CMD,
                format!(
                    "the sequence is {sw}×{sh}, not {}: \"keyframes\" reframes inside the sequence's own frame. Use \"output\":\"sequence\" for a new {} sequence, or change the sequence's frame size first",
                    p.get("aspect").and_then(Value::as_str).unwrap_or("that aspect"),
                    p.get("aspect").and_then(Value::as_str).unwrap_or("")
                ),
            ));
        }
    }
    // the subject
    let subject = p.get("subject").filter(|v| !v.is_null() && v.as_str() != Some("auto"));
    let seed_spec = match subject {
        None => None,
        Some(v) if v.is_object() => {
            let at = tick_param(v, "at", CMD)?.unwrap_or_else(|| s.playhead());
            let (c, size) = if let Some(b) = v.get("box").filter(|b| !b.is_null()) {
                let a: Vec<f64> = b
                    .as_array()
                    .filter(|a| a.len() == 4)
                    .map(|a| a.iter().filter_map(Value::as_f64).filter(|x| x.is_finite() && x.abs() < 1e7).collect())
                    .unwrap_or_default();
                let [x, y, w, h] = a[..] else { return Err(bad(CMD, format!("`subject.box` must be [x, y, w, h] of finite numbers, got {b}"))) };
                if !(w >= 2.0 && h >= 2.0) {
                    return Err(bad(CMD, "`subject.box` must be at least 2 pixels wide and high"));
                }
                (Vec2::new(x + w / 2.0, y + h / 2.0), Vec2::new(w, h))
            } else {
                let pv = v.get("point").cloned().unwrap_or(Value::Null);
                let c = point(&pv).ok_or_else(|| bad(CMD, format!("`subject` must be \"auto\", {{\"point\":[x,y]}} or {{\"box\":[x,y,w,h]}}, got {v}")))?;
                let size = num(v, "size", CMD, 64.0, 2.0, 4000.0)?;
                (c, Vec2::new(size, size))
            };
            Some((at, c, size))
        }
        Some(v) => return Err(bad(CMD, format!("`subject` must be \"auto\", {{\"point\":[x,y]}} or {{\"box\":[x,y,w,h]}}, got {v}"))),
    };
    let range = (tick_param(p, "start", CMD)?, tick_param(p, "end", CMD)?);
    if let (Some(a), Some(b)) = range
        && b <= a
    {
        return Err(bad(CMD, "`end` must be after `start`"));
    }
    // the clips
    let warnings = Vec::new();
    let mut notes = Vec::new();
    let video = |c: ClipId| q.find_item(c).filter(|(t, _)| q.track(*t).is_some_and(|t| t.kind == TrackKind::Video)).map(|(_, it)| it.clone());
    let named: Option<Vec<ClipId>> = match (p.get("clips").filter(|v| !v.is_null()), p.get("clip").filter(|v| !v.is_null())) {
        (Some(v), _) => Some(
            v.as_array()
                .ok_or_else(|| bad(CMD, "`clips` must be a list of clip ids"))?
                .iter()
                .map(|c| c.as_u64().map(ClipId).ok_or_else(|| bad(CMD, format!("`clips` must hold clip ids, got {c}"))))
                .collect::<Result<_>>()?,
        ),
        (None, Some(v)) => Some(vec![v.as_u64().map(ClipId).ok_or_else(|| bad(CMD, format!("`clip` must be a clip id, got {v}")))?]),
        (None, None) => None,
    };
    if let Some(n) = &named
        && let Some(missing) = n.iter().find(|c| video(**c).is_none())
    {
        return Err(bad(CMD, format!("no video clip {} in the active sequence", missing.0)));
    }
    let clips: Vec<TrackItem> = match (&mode, named) {
        (_, Some(n)) => {
            let mut seen = std::collections::BTreeSet::new();
            n.into_iter().filter(|c| seen.insert(*c)).filter_map(video).collect()
        }
        (Mode::Keyframes, None) => {
            let sel: Vec<TrackItem> = s.state.selection.iter().filter_map(|c| video(*c)).filter(|it| s.source(it.item).is_some()).collect();
            if sel.is_empty() {
                return Err(bad(CMD, "name the video clip with `clip` (or `clips`), or select it"));
            }
            sel
        }
        (Mode::Sequence { .. }, None) => {
            let mut out = Vec::new();
            for it in q.video_tracks.iter().flat_map(|tr| tr.items.iter()) {
                if s.source(it.item).is_none() || it.frame_hold.is_some() {
                    if !s.project.item(it.item).is_some_and(|i| matches!(i.kind, filmagic_project::ItemKind::Graphic { .. })) {
                        notes.push(format!("clip {} ({}) has no moving media and is not reframed", it.id.0, it.name));
                    }
                    continue;
                }
                out.push(it.clone());
            }
            out
        }
    };
    if clips.is_empty() {
        return Err(bad(CMD, "there is no video clip with media to reframe"));
    }
    let mut work = Vec::new();
    let mut total = 0usize;
    let frame = super::frame_of(q);
    for it in &clips {
        let view = ClipView::new(s, &frame, it).ok_or_else(|| bad(CMD, format!("clip {} has no picture", it.id.0)))?;
        let src = s.source(it.item).ok_or_else(|| bad(CMD, format!("clip {} has no media to analyse", it.id.0)))?;
        if it.frame_hold.is_some() {
            return Err(bad(CMD, format!("clip {} is a freeze frame: there is no motion to follow", it.id.0)));
        }
        if let Some(m) = it.effect("motion").filter(|m| m.enabled) {
            let rot = m.param("rotation");
            if rot.is_some_and(|r| r.is_animated() || r.value.as_f64().is_some_and(|v| v.abs() > 1e-9)) {
                return Err(bad(CMD, format!("clip {} is rotated (Motion ▸ Rotation): reset its rotation first", it.id.0)));
            }
            if m.param("anchor").is_some_and(Param::is_animated) {
                return Err(bad(CMD, format!("clip {}'s Motion ▸ Anchor Point is animated: remove its keyframes first", it.id.0)));
            }
        }
        if it.has_speed_ramp() {
            notes.push(format!(
                "clip {} has a speed ramp: its shown frames are analysed and the keyframes are timed in its linear media time, so they stay with the sequence; ramp first, then reframe",
                it.id.0
            ));
        }
        let lo = range.0.map_or(it.start, |a| a.max(it.start));
        let hi = range.1.map_or(it.end(), |b| b.min(it.end()));
        if hi <= lo {
            if matches!(mode, Mode::Keyframes) {
                return Err(bad(CMD, format!("`start`/`end` do not overlap clip {} ({:.3}–{:.3} s)", it.id.0, it.start.seconds(), it.end().seconds())));
            }
            continue;
        }
        let seeded = seed_spec.filter(|(at, ..)| *at >= lo && *at < hi);
        let fps = rate.as_f64().max(1.0);
        let step_frames = match p.get("sampleEvery") {
            None | Some(Value::Null) => (fps / if seeded.is_some() { 15.0 } else { 10.0 }).round().max(1.0) as i64,
            Some(_) => num(p, "sampleEvery", CMD, 1.0, 1.0, 120.0)?.round() as i64,
        };
        let step = Tick(fd.0.saturating_mul(step_frames));
        let mut times: Vec<Tick> = Vec::new();
        let mut t = lo;
        while t < hi {
            times.push(t);
            if times.len() > MAX_SAMPLES {
                return Err(bad(CMD, format!("more than {MAX_SAMPLES} samples to analyse: give a shorter `start`/`end` or a larger `sampleEvery`")));
            }
            t += step;
        }
        // the last frame too, so the path reaches the end
        let last = hi - fd;
        if times.last().is_some_and(|l| last > *l) {
            times.push(last);
        }
        total = total.saturating_add(times.len());
        if total > MAX_SAMPLES {
            return Err(bad(
                CMD,
                format!("more than {MAX_SAMPLES} samples to analyse in all: reframe fewer clips, a shorter range or with a larger `sampleEvery`"),
            ));
        }
        let seed = match seeded {
            Some((at, c, size)) => {
                let i = times.iter().rposition(|t| *t <= at).unwrap_or(0);
                let k = view.src_per_seq(times[i]);
                let at_src = view.to_src(times[i], c).ok_or_else(|| bad(CMD, "the clip's Motion squashes it to nothing at `subject.at`"))?;
                if at_src.x < 0.0 || at_src.y < 0.0 || at_src.x > f64::from(view.src.0) || at_src.y > f64::from(view.src.1) {
                    return Err(bad(CMD, format!("the subject ({:.0}, {:.0}) is outside clip {}'s picture at `subject.at`", c.x, c.y, it.id.0)));
                }
                Some((i, Seed { at: at_src, half: (size.x / 2.0 * k, size.y / 2.0 * k) }))
            }
            None => None,
        };
        work.push(Work { view: view.resized(ow, oh), src, times, step, seed });
    }
    if let Some((at, ..)) = seed_spec
        && !work.iter().any(|w| w.seed.is_some())
    {
        return Err(bad(CMD, format!("`subject.at` ({:.3} s) is not inside a clip being reframed", at.seconds())));
    }
    if work.is_empty() {
        return Err(bad(CMD, "`start`/`end` overlap none of the clips"));
    }
    let label = match &mode {
        Mode::Keyframes => "Auto Reframe".to_string(),
        Mode::Sequence { .. } => "Auto Reframe Sequence".to_string(),
    };
    let req = Request { mode, warnings, notes };
    super::launch(s, CMD, label, total as u64, JobRequest::Reframe(req), wait, move |prog| {
        let mut plans = Vec::with_capacity(work.len());
        for w in &work {
            plans.push(analyse(w, opts, prog)?);
        }
        Ok(Analysis::Reframe(plans))
    })
}

/// The subject per sample (source pixels) by motion saliency.
fn motion_subject(w: &Work, mts: &[Tick], prog: &filmagic_export::Progress) -> std::result::Result<Vec<Option<Vec2>>, String> {
    use std::sync::atomic::Ordering;
    let src_w = w.view.src.0;
    let scale = (MOTION_WIDTH / f64::from(src_w.max(1))).min(1.0) as f32;
    let mut out = vec![None; mts.len()];
    let mut prev: Option<super::Luma> = None;
    for (i, mt) in mts.iter().enumerate() {
        if prog.cancel.load(Ordering::Relaxed) {
            return Err(STOPPED.into());
        }
        if let Some((img, k)) = super::luma_frame(&w.src, *mt, scale, src_w) {
            if let Some(a) = &prev {
                out[i] = super::track::motion_centroid(a, &img).map(|c| c * (1.0 / k));
            }
            prev = Some(img);
        }
        let done = prog.done.fetch_add(1, Ordering::Relaxed) + 1;
        *lock(&prog.status) = format!("Frame {done} of {}", prog.total.load(Ordering::Relaxed));
    }
    // the first sample has no frame before it: it takes the second's
    if out.len() > 1 && out[0].is_none() {
        out[0] = out[1];
    }
    Ok(out)
}

fn analyse(w: &Work, o: Opts, prog: &filmagic_export::Progress) -> std::result::Result<ClipPlan, String> {
    // the frames each sample shows (through a speed ramp), worked out here in the job
    let mts = super::shown_times(&w.view.item, &w.times);
    let subject: Vec<Option<Vec2>> = match w.seed {
        Some((i, seed)) => super::follow(&w.src, w.view.src.0, &mts, i, seed, TRACK_WIDTH, prog)?.into_iter().map(|(p, _)| p).collect(),
        None => motion_subject(w, &mts, prog)?,
    };
    let (src_w, src_h) = (f64::from(w.view.src.0), f64::from(w.view.src.1));
    let (sw, sh) = w.view.seq_size();
    let (pr, qr) = w.view.pars();
    let disp_w = src_w * pr;
    let cover = (sw * qr / disp_w).max(sh / src_h);
    let total = cover * o.zoom;
    // source px → sequence px along each axis, and the crop's half size in source px
    let (kx, ky) = (total * pr / qr, total);
    let (hw, hh) = (sw / 2.0 / kx, sh / 2.0 / ky);
    let dt = w.step.seconds().max(1e-6);
    let tau = 0.05 + 1.95 * o.smoothing.powf(1.5);
    let mut notes = Vec::new();
    let axis = |vals: Vec<Option<f64>>, half: f64, size: f64| -> Vec<f64> {
        let mut v = path::fill_gaps(&vals).unwrap_or_else(|| vec![size / 2.0; vals.len()]);
        path::clamp_all(&mut v, half, size - half);
        let v = path::dead_zone(&v, o.dead_zone * 2.0 * half);
        let v = path::spring_smooth(&v, dt, tau);
        let mut v = path::limit_speed(&v, o.max_pan * 2.0 * hw * dt);
        path::clamp_all(&mut v, half, size - half);
        v
    };
    if subject.iter().all(Option::is_none) {
        notes.push("nothing was found moving: the crop stays centred".to_string());
    }
    let cx = axis(subject.iter().map(|s| s.map(|p| p.x)).collect(), hw, src_w);
    let cy = axis(subject.iter().map(|s| s.map(|p| p.y)).collect(), hh, src_h);
    let anchor = Vec2::new(src_w / 2.0, src_h / 2.0);
    let ets: Vec<Tick> = w.times.iter().map(|t| w.view.item.effect_time_at(*t)).collect();
    let pos: Vec<Vec<f64>> = cx.iter().zip(&cy).map(|(x, y)| vec![sw / 2.0 - kx * (x - anchor.x), sh / 2.0 - ky * (y - anchor.y)]).collect();
    let tf: Vec<f64> = ets.iter().map(|t| t.0 as f64).collect();
    // keyframes must be in time order; a reversed clip's effect times run backwards
    let mut order: Vec<usize> = (0..ets.len()).collect();
    order.sort_by_key(|&i| ets[i]);
    let (tf_o, pos_o): (Vec<f64>, Vec<Vec<f64>>) = order.iter().map(|&i| (tf[i], pos[i].clone())).unzip();
    let mut keep = path::thin(&tf_o, &pos_o, KEY_TOLERANCE);
    // a camera that never moves is one value, not two equal keyframes
    if let [a, b] = keep[..]
        && pos_o[a].iter().zip(&pos_o[b]).all(|(x, y)| (x - y).abs() <= KEY_TOLERANCE)
    {
        keep.truncate(1);
    }
    let keys: Vec<(Tick, Vec2)> = keep.iter().map(|&k| (ets[order[k]], Vec2::new(pos_o[k][0], pos_o[k][1]))).collect();
    let fit = (sw * qr / disp_w).min(sh / src_h);
    let scale = if w.view.item.scale_to_frame && fit > 0.0 { 100.0 * total / fit } else { 100.0 * total };
    let found = subject.iter().filter(|s| s.is_some()).count();
    let inside = |i: usize| subject.get(i).copied().flatten().is_some_and(|p| (p.x - cx[i]).abs() <= hw && (p.y - cy[i]).abs() <= hh);
    let in_frame = (0..subject.len()).filter(|&i| inside(i)).count();
    let left = spans(&w.times, w.step, |i| subject.get(i).copied().flatten().is_some() && !inside(i));
    let lost = spans(&w.times, w.step, |i| subject.get(i).copied().flatten().is_none());
    let report = json!({
        "clip": w.view.item.id.0,
        "samples": subject.len(),
        "coverage": if subject.is_empty() { 0.0 } else { found as f64 / subject.len() as f64 },
        "inFrame": if found == 0 { 0.0 } else { in_frame as f64 / found as f64 },
        "subjectLeftFrame": left,
        "subjectLost": lost,
        "keyframes": keys.len(),
        "scale": scale,
        "crop": [2.0 * hw, 2.0 * hh],
        "notes": notes,
    });
    let span = (w.times.first().copied().unwrap_or_default(), w.times.last().map_or(Tick::ZERO, |t| *t + Tick(1)));
    Ok(ClipPlan { clip: w.view.item.id, keys, scale, anchor, span, report })
}

/// Write `plan` on `it`: Position keyframes over the analysed range (keyframes already there
/// outside it stay), Scale (keyed only when it was already animated), the Anchor Point at the
/// middle of the picture, uniform scale.
fn write(it: &mut TrackItem, plan: &ClipPlan) -> Result<()> {
    let (Some(lo), Some(hi)) = (plan.keys.iter().map(|k| k.0).min(), plan.keys.iter().map(|k| k.0).max()) else { return Ok(()) };
    if it.effect("motion").is_none() {
        let m = filmagic_project::find_effect("motion").ok_or_else(|| EngineError::Other("internal: no Motion effect".into()))?.instance();
        it.effects.insert(0, m);
    }
    let m = it.effect_mut("motion").ok_or_else(|| EngineError::Other("internal: no Motion effect".into()))?;
    m.enabled = true;
    let pos = entry(m, "position", ParamValue::Vec2(Vec2::ZERO));
    pos.keyframes.retain(|k| k.time < lo || k.time > hi);
    for (t, v) in &plan.keys {
        pos.put_keyframe(*t, ParamValue::Vec2(*v));
    }
    if let [only] = pos.keyframes.as_slice() {
        // one key is a still frame: keep it static
        let v = only.value.clone();
        pos.keyframes.clear();
        pos.value = v;
    }
    let scale = entry(m, "scale", ParamValue::Float(100.0));
    if scale.is_animated() {
        scale.keyframes.retain(|k| k.time < lo || k.time > hi);
        scale.put_keyframe(lo, ParamValue::Float(plan.scale));
        scale.put_keyframe(hi, ParamValue::Float(plan.scale));
    } else {
        scale.value = ParamValue::Float(plan.scale);
    }
    entry(m, "uniform_scale", ParamValue::Bool(true)).value = ParamValue::Bool(true);
    let anchor = entry(m, "anchor", ParamValue::Vec2(plan.anchor));
    anchor.keyframes.clear();
    anchor.value = ParamValue::Vec2(plan.anchor);
    Ok(())
}

fn entry<'a>(m: &'a mut filmagic_project::EffectInstance, id: &str, v: ParamValue) -> &'a mut Param {
    m.params.entry(id.to_string()).or_insert_with(|| Param::new(v))
}

/// A copy of sequence `from` named `name`, `w`×`h`, whose clips, links, groups and transitions
/// have fresh ids. Answers it and the copy of each clip.
fn copy_sequence(pr: &mut Project, from: ItemId, name: &str, w: u32, h: u32) -> Result<(ItemId, HashMap<ClipId, ClipId>)> {
    let src = pr.sequence(from).ok_or(EngineError::NoSequence)?.clone();
    let mut settings = src.settings.clone();
    settings.width = w;
    settings.height = h;
    settings.validate().map_err(|e| bad(CMD, format!("the new sequence's settings: {e}")))?;
    let bin = pr.root.parent_of(from);
    let nid = pr.new_sequence(name, settings.clone(), 0, 0, bin);
    let mut copy = src;
    copy.settings = settings;
    let mut ids = HashMap::new();
    let mut links = HashMap::new();
    let mut groups = HashMap::new();
    for tr in copy.video_tracks.iter_mut().chain(copy.audio_tracks.iter_mut()) {
        for it in &mut tr.items {
            let new = ClipId(pr.alloc_id());
            ids.insert(it.id, new);
            it.id = new;
            if let Some(l) = it.link {
                it.link = Some(*links.entry(l).or_insert_with(|| pr.alloc_id()));
            }
            if let Some(g) = it.group {
                it.group = Some(*groups.entry(g).or_insert_with(|| pr.alloc_id()));
            }
        }
    }
    for tr in copy.video_tracks.iter_mut().chain(copy.audio_tracks.iter_mut()) {
        let mut kept = Vec::with_capacity(tr.transitions.len());
        for t in std::mem::take(&mut tr.transitions) {
            let map = |c: Option<ClipId>| match c {
                None => Some(None),
                Some(c) => ids.get(&c).map(|n| Some(*n)),
            };
            if let (Some(from), Some(to)) = (map(t.from), map(t.to)) {
                let mut t = t;
                t.from = from;
                t.to = to;
                t.id = filmagic_project::TransitionId(pr.alloc_id());
                kept.push(t);
            }
        }
        tr.transitions = kept;
    }
    copy.check().map_err(EngineError::Other)?;
    *pr.sequence_mut(nid).ok_or(EngineError::NoSequence)? = copy;
    Ok((nid, ids))
}

/// Most samples a followed graphic is keyed at.
const MAX_FOLLOW_SAMPLES: usize = 4000;

/// Give each graphic clip of the new sequence `nid` that lies over a reframed clip the same crop:
/// its Motion becomes (the clip's new Motion) ∘ (its old Motion)⁻¹ ∘ (the graphic's old Motion),
/// sampled every frame where they overlap (every few frames of a long overlap) and thinned. An overlay drawn on the old frame (a ball
/// trail, a ring) then stays on what it marks. Answers notes about graphics left as they were.
fn follow_graphics(pr: &mut Project, old: ItemId, nid: ItemId, ids: &HashMap<ClipId, ClipId>, reframed: &[ClipId]) -> Vec<String> {
    use filmagic_render::motion_matrix;
    let mut notes = Vec::new();
    let (Some(oq), Some(nq)) = (pr.sequence(old).cloned(), pr.sequence(nid).cloned()) else { return notes };
    let back: HashMap<ClipId, ClipId> = ids.iter().map(|(a, b)| (*b, *a)).collect();
    let fd = nq.settings.frame_rate.frame_duration().max(Tick(1));
    let is_graphic = |pr: &Project, it: &TrackItem| pr.item(it.item).is_some_and(|i| matches!(i.kind, filmagic_project::ItemKind::Graphic { .. }));
    let mut writes: Vec<(ClipId, Vec<(Tick, Vec2)>, Vec<(Tick, f64)>)> = Vec::new();
    for (vi, tr) in nq.video_tracks.iter().enumerate() {
        for g in tr.items.iter().filter(|g| is_graphic(pr, g)) {
            let mid = g.start + Tick(g.duration.0 / 2);
            let Some(c) = nq.video_tracks[..vi].iter().rev().find_map(|t| t.item_at(mid).filter(|c| reframed.contains(&c.id))) else { continue };
            let (Some(c_old), Some(g_old)) =
                (back.get(&c.id).and_then(|o| oq.find_item(*o)).map(|x| x.1), back.get(&g.id).and_then(|o| oq.find_item(*o)).map(|x| x.1))
            else {
                continue;
            };
            let gm = g_old.effect("motion");
            if gm.is_some_and(|m| m.params.values().any(filmagic_project::Param::is_animated)) {
                notes.push(format!("graphic {} ({}) has animated Motion and keeps it: check it in the new frame", g.id.0, g.name));
                continue;
            }
            let (Some(gs), Some(cs)) = (pr.source_size(g.item), pr.source_size(c.item)) else { continue };
            let (gpar, cpar) = (pr.source_par(g.item), pr.source_par(c.item));
            let anchor = gm
                .and_then(|m| m.param("anchor"))
                .and_then(|p| p.value.as_vec2())
                .filter(|a| a.x.is_finite() && a.y.is_finite())
                .unwrap_or(Vec2::new(f64::from(gs.0) / 2.0, f64::from(gs.1) / 2.0));
            let fit = if g.scale_to_frame {
                (f64::from(nq.settings.width) / f64::from(gs.0.max(1))).min(f64::from(nq.settings.height) / f64::from(gs.1.max(1)))
            } else {
                1.0
            };
            let (lo, hi) = (g.start.max(c.start), g.end().min(c.end()));
            let mut pos = Vec::new();
            let mut scale = Vec::new();
            // every frame, or every `stride` frames of a long overlap (and its last frame), so the
            // whole of it is followed with at most MAX_FOLLOW_SAMPLES samples
            let frames = usize::try_from((hi - lo).0 / fd.0).unwrap_or(usize::MAX).max(1);
            let stride = Tick(fd.0.saturating_mul(frames.div_ceil(MAX_FOLLOW_SAMPLES).max(1) as i64));
            let last = hi - fd;
            let mut t = lo;
            let mut skewed = false;
            while t < hi && pos.len() <= MAX_FOLLOW_SAMPLES {
                let a = motion_matrix(&nq, c, cs, cpar, c.effect_time_at(t));
                let b = motion_matrix(&oq, c_old, cs, cpar, c_old.effect_time_at(t)).inverse();
                let m = motion_matrix(&oq, g_old, gs, gpar, g_old.effect_time_at(t));
                if let Some(b) = b {
                    let total = a.then_apply(&b).then_apply(&m);
                    if total.b.abs() > 1e-6 || total.c.abs() > 1e-6 || (total.a - total.d).abs() > 1e-6 * total.a.abs().max(1.0) {
                        skewed = true;
                        break;
                    }
                    let et = g.effect_time_at(t);
                    pos.push((et, total.apply(anchor)));
                    scale.push((et, 100.0 * total.a / fit));
                }
                t = if t < last && t + stride > last { last } else { t + stride };
            }
            if skewed {
                notes.push(format!("graphic {} ({}) would need a rotated or stretched crop and keeps its Motion", g.id.0, g.name));
                continue;
            }
            if !pos.is_empty() {
                writes.push((g.id, pos, scale));
            }
        }
    }
    let Some(q) = pr.sequence_mut(nid) else { return notes };
    for (id, pos, scale) in writes {
        let Some((_, it)) = q.find_item_mut(id) else { continue };
        if it.effect("motion").is_none()
            && let Some(d) = filmagic_project::find_effect("motion")
        {
            it.effects.insert(0, d.instance());
        }
        let Some(m) = it.effect_mut("motion") else { continue };
        m.enabled = true;
        let times: Vec<f64> = pos.iter().map(|p| p.0.0 as f64).collect();
        let vals: Vec<Vec<f64>> = pos.iter().zip(&scale).map(|(p, s)| vec![p.1.x, p.1.y, s.1]).collect();
        let keep = path::thin(&times, &vals, KEY_TOLERANCE);
        let still = keep.len() <= 2 && vals.first().zip(vals.last()).is_some_and(|(a, b)| a.iter().zip(b).all(|(x, y)| (x - y).abs() <= KEY_TOLERANCE));
        let p = entry(m, "position", ParamValue::Vec2(Vec2::ZERO));
        p.keyframes.clear();
        p.value = ParamValue::Vec2(pos[0].1);
        if !still {
            for &k in &keep {
                p.put_keyframe(pos[k].0, ParamValue::Vec2(pos[k].1));
            }
        }
        let sc = entry(m, "scale", ParamValue::Float(100.0));
        sc.keyframes.clear();
        sc.value = ParamValue::Float(scale[0].1);
        if !still {
            for &k in &keep {
                sc.put_keyframe(scale[k].0, ParamValue::Float(scale[k].1));
            }
        }
        entry(m, "uniform_scale", ParamValue::Bool(true)).value = ParamValue::Bool(true);
    }
    notes
}

pub(crate) fn apply(s: &mut Session, seq: ItemId, req: Request, plans: Vec<ClipPlan>) -> Result<Value> {
    let Request { mode, warnings, notes } = req;
    let reports: Vec<Value> = plans.iter().map(|p| p.report.clone()).collect();
    let mut out = json!({"clips": reports, "warnings": warnings, "notes": notes});
    match mode {
        Mode::Keyframes => {
            let span = (plans.iter().map(|p| p.span.0).min().unwrap_or_default(), plans.iter().map(|p| p.span.1).max().unwrap_or_default());
            let looked = s.state.active_sequence == Some(seq);
            let before = if looked { crate::story::looks_across(s, span.0, span.1) } else { Vec::new() };
            s.edit("Auto Reframe", |pr, _| {
                let q = pr.sequence_mut(seq).ok_or(EngineError::NoSequence)?;
                for plan in &plans {
                    let (_, it) = q.find_item_mut(plan.clip).ok_or_else(|| bad(CMD, format!("clip {} is gone; nothing was changed", plan.clip.0)))?;
                    write(it, plan)?;
                }
                Ok(())
            })?;
            out["pictureChange"] = if looked { crate::story::change_json(&before, &crate::story::looks_across(s, span.0, span.1)) } else { Value::Null };
        }
        Mode::Sequence { name, width, height, follow } => {
            let mut follow_notes = Vec::new();
            let (nid, n) = s.edit("Auto Reframe Sequence", |pr, st| {
                let (nid, ids) = copy_sequence(pr, seq, &name, width, height)?;
                let q = pr.sequence_mut(nid).ok_or(EngineError::NoSequence)?;
                let mut n = 0;
                for plan in &plans {
                    let Some(new) = ids.get(&plan.clip) else { continue };
                    if let Some((_, it)) = q.find_item_mut(*new) {
                        write(it, plan)?;
                        n += 1;
                    }
                }
                if follow {
                    let reframed: Vec<ClipId> = plans.iter().filter_map(|p| ids.get(&p.clip).copied()).collect();
                    follow_notes = follow_graphics(pr, seq, nid, &ids, &reframed);
                }
                st.project_selection = vec![nid];
                Ok((nid, n))
            })?;
            out["sequence"] = json!({"id": nid.0, "name": name, "width": width, "height": height, "reframed": n});
            // a frame of another size cannot be compared pixel for pixel with the original
            out["pictureChange"] = json!({"reads": "strong", "hint": format!("a new {width}×{height} sequence: look at it (render_preview) rather than compare it with the original")});
            if let Some(a) = out["notes"].as_array_mut() {
                a.push(json!(format!("the new sequence is not opened: sequence.open {{\"item\": {}}} opens it", nid.0)));
                a.extend(follow_notes.into_iter().map(Value::from));
                if !follow {
                    a.push(json!("graphics and titles keep their positions from the old frame: check them in the new one"));
                }
            }
        }
    }
    Ok(out)
}
