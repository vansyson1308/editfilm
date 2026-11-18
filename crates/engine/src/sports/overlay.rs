//! `sports.trackPoint`, `sports.ballTrail`, `sports.groundRing` and `sports.pathLine`: follow an
//! object, then draw over it with graphic shape layers keyframed per frame (see [`super`]).

use filmagic_geom::Vec2;
use filmagic_project::graphic::new_shape_layer;
use filmagic_project::{EffectInstance, Mask, MaskPath, Param, ParamValue};
use filmagic_time::{FrameRate, Tick};
use serde_json::{Value, json};

use super::{Analysis, Request as JobRequest, TrackedPoint, color, num, path, point, spans, tick_param, track_doc, wait_p};
use crate::commands::bad;
use crate::{EngineError, Result, Session};

/// Width of the frames a small object is followed in (a pickleball is a few pixels at this size).
const TRACK_WIDTH: f64 = 640.0;
/// Longest gap (frames) bridged by interpolation; a longer one hides the overlay there.
const MAX_GAP: usize = 8;
/// Most layer-frames a trail may key (layers × frames), so the project stays small.
const MAX_TRAIL_KEYS: usize = 40_000;
/// Most segments of a path line.
const MAX_PATH_SEGMENTS: usize = 64;
/// Longest default range followed from `at`.
const DEFAULT_SPAN_SECONDS: f64 = 10.0;

pub(crate) const TRAIL_PARAMS: &str = concat!(
    "{",
    track_doc!(),
    r##","points":[{"time":ticks,"x":px,"y":px}]? (the path itself, instead of tracking),"color":"#RRGGBB"|[r,g,b,a]="#FFE45C","width":px=10 (at the ball),"length":frames=10 (how far back the trail reaches, 1..60),"fade":0..1=1 (how much it fades to its tail),"taper":0..1=0.6 (how much it narrows to its tail),"opacity":0..100=90,"glow":bool=true,"track":index? (video track for the graphic)} → a graphic clip of trail segments (rounded-rectangle shape layers keyframed per frame), coverage, lost [{start, end}], pictureChange"##
);

pub(crate) const RING_PARAMS: &str = concat!(
    "{",
    track_doc!(),
    r##","points":[{"time":ticks,"x":px,"y":px}]? (the path itself, instead of tracking),"color":"#RRGGBB"|[r,g,b,a]="#40E0FF","diameter":px=140,"squash":0.05..1=0.35 (height ÷ width: the ground seen at an angle),"width":px=6 (the ring's line),"fill":0..100=15 (opacity of a fill inside the ring),"glow":bool=true,"offset":[dx,dy]=[0,0] (from the tracked point to the ring's centre),"spotlight":0..100=0 (darken everything outside a spot over the player by this much),"spotSize":[w,h]? (default 1.6 × 3.2 diameters),"spotOffset":[dx,dy]? (from the ring's centre; default up by 1.1 diameters),"feather":px? (the spot's soft edge; default a quarter diameter),"track":index?} → a graphic clip with the ring (an ellipse shape layer keyframed per frame) and, with `spotlight`, a clip below it darkening the frame through an inverted, keyframed Opacity mask; coverage, lost [{start, end}], pictureChange"##
);

pub(crate) const PATH_PARAMS: &str = concat!(
    "{",
    track_doc!(),
    r##","points":[{"time":ticks?,"x":px,"y":px}]|[[x,y]]? (the path to draw, instead of tracking),"time":ticks? (where the drawing starts; default the end of the followed range, e.g. the start of a freeze frame),"duration":ticks? (how long it shows; default the rest of a freeze-frame clip at `time`, else 3 s),"curve":"smooth"|"arc"|"line"="smooth" (arc: a parabola fitted to the points, a ball's flight),"color":"#RRGGBB"|[r,g,b,a]="#FFD21F","width":px=6,"dashed":bool=false,"arrow":bool=true,"glow":bool=true,"reveal":ticks? (a length: draw the line on over this long),"track":index?} → a graphic clip of line segments (rounded-rectangle shape layers) and an arrowhead, pictureChange"##
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Track,
    Trail,
    Ring,
    Path,
}

#[derive(Clone, Debug)]
pub(crate) struct Trail {
    color: [f32; 4],
    width: f64,
    length: usize,
    fade: f64,
    taper: f64,
    opacity: f64,
    glow: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct Ring {
    color: [f32; 4],
    diameter: f64,
    squash: f64,
    width: f64,
    fill: f64,
    glow: bool,
    offset: Vec2,
    spotlight: f64,
    spot_size: Vec2,
    spot_offset: Vec2,
    feather: f64,
}

#[derive(Clone, Debug)]
pub(crate) struct PathLine {
    color: [f32; 4],
    width: f64,
    curve: Curve,
    dashed: bool,
    arrow: bool,
    glow: bool,
    time: Option<Tick>,
    duration: Option<Tick>,
    reveal: Tick,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Curve {
    Smooth,
    Arc,
    Line,
}

#[derive(Clone, Debug)]
pub(crate) enum Style {
    Track,
    Trail(Trail),
    Ring(Ring),
    Path(PathLine),
}

/// What to draw once the path is known.
pub(crate) struct Request {
    cmd: &'static str,
    style: Style,
    /// The video track to put the graphic on.
    track: Option<usize>,
    /// The points come from tracking (one per frame), not from the caller.
    tracked: bool,
    warnings: Vec<String>,
    notes: Vec<String>,
    rate: FrameRate,
}

fn bool_or(p: &Value, key: &str, cmd: &str, default: bool) -> Result<bool> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(v) => Err(bad(cmd, format!("`{key}` must be true or false, got {v}"))),
    }
}

fn vec_or(p: &Value, key: &str, cmd: &str, default: Vec2) -> Result<Vec2> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => point(v).ok_or_else(|| bad(cmd, format!("`{key}` must be [x, y] (finite numbers), got {v}"))),
    }
}

fn style_of(p: &Value, kind: Kind, cmd: &'static str) -> Result<Style> {
    Ok(match kind {
        Kind::Track => Style::Track,
        Kind::Trail => Style::Trail(Trail {
            color: color(p, "color", cmd, [1.0, 0.894, 0.36, 1.0])?,
            width: num(p, "width", cmd, 10.0, 0.5, 500.0)?,
            length: num(p, "length", cmd, 10.0, 1.0, 60.0)?.round() as usize,
            fade: num(p, "fade", cmd, 1.0, 0.0, 1.0)?,
            taper: num(p, "taper", cmd, 0.6, 0.0, 1.0)?,
            opacity: num(p, "opacity", cmd, 90.0, 0.0, 100.0)?,
            glow: bool_or(p, "glow", cmd, true)?,
        }),
        Kind::Ring => {
            let d = num(p, "diameter", cmd, 140.0, 2.0, 4000.0)?;
            let spot = vec_or(p, "spotSize", cmd, Vec2::new(1.6 * d, 3.2 * d))?;
            if !(spot.x > 0.0 && spot.y > 0.0) {
                return Err(bad(cmd, "`spotSize` must be positive"));
            }
            Style::Ring(Ring {
                color: color(p, "color", cmd, [0.25, 0.88, 1.0, 1.0])?,
                diameter: d,
                squash: num(p, "squash", cmd, 0.35, 0.05, 1.0)?,
                width: num(p, "width", cmd, 6.0, 0.5, 500.0)?,
                fill: num(p, "fill", cmd, 15.0, 0.0, 100.0)?,
                glow: bool_or(p, "glow", cmd, true)?,
                offset: vec_or(p, "offset", cmd, Vec2::ZERO)?,
                spotlight: num(p, "spotlight", cmd, 0.0, 0.0, 100.0)?,
                spot_size: spot,
                spot_offset: vec_or(p, "spotOffset", cmd, Vec2::new(0.0, -1.1 * d))?,
                feather: num(p, "feather", cmd, 0.25 * d, 0.0, 4000.0)?,
            })
        }
        Kind::Path => Style::Path(PathLine {
            color: color(p, "color", cmd, [1.0, 0.824, 0.12, 1.0])?,
            width: num(p, "width", cmd, 6.0, 0.5, 500.0)?,
            curve: match p.get("curve") {
                None | Some(Value::Null) => Curve::Smooth,
                Some(v) => match v.as_str() {
                    Some("smooth") => Curve::Smooth,
                    Some("arc") => Curve::Arc,
                    Some("line") => Curve::Line,
                    _ => return Err(bad(cmd, format!("`curve` must be \"smooth\", \"arc\" or \"line\", got {v}"))),
                },
            },
            dashed: bool_or(p, "dashed", cmd, false)?,
            arrow: bool_or(p, "arrow", cmd, true)?,
            glow: bool_or(p, "glow", cmd, true)?,
            time: tick_param(p, "time", cmd)?,
            duration: tick_param(p, "duration", cmd)?.filter(|d| *d > Tick::ZERO),
            reveal: tick_param(p, "reveal", cmd)?.unwrap_or(Tick::ZERO),
        }),
    })
}

/// Points the caller gave: `[x, y]` or `{"time", "x", "y"}`.
fn given_points(p: &Value, cmd: &str, need_time: bool) -> Result<Option<Vec<TrackedPoint>>> {
    let Some(v) = p.get("points").filter(|v| !v.is_null()) else { return Ok(None) };
    let arr = v.as_array().ok_or_else(|| bad(cmd, "`points` must be a list"))?;
    if arr.len() > super::MAX_TRACK_FRAMES {
        return Err(bad(cmd, format!("`points` holds {} points; at most {}", arr.len(), super::MAX_TRACK_FRAMES)));
    }
    let mut out = Vec::with_capacity(arr.len());
    for (i, e) in arr.iter().enumerate() {
        let (at, time) = if let Some(q) = point(e) {
            (q, None)
        } else {
            let x = e.get("x").and_then(Value::as_f64).filter(|x| x.is_finite() && x.abs() < 1e7);
            let y = e.get("y").and_then(Value::as_f64).filter(|y| y.is_finite() && y.abs() < 1e7);
            let (Some(x), Some(y)) = (x, y) else {
                return Err(bad(cmd, format!("points[{i}] must be [x, y] or {{\"time\", \"x\", \"y\"}} with finite numbers, got {e}")));
            };
            let t = match e.get("time") {
                None | Some(Value::Null) => None,
                Some(t) => Some(t.as_i64().filter(|t| *t >= 0).ok_or_else(|| bad(cmd, format!("points[{i}].time must be a time, got {t}")))?),
            };
            (Vec2::new(x, y), t.map(Tick))
        };
        if need_time && time.is_none() {
            return Err(bad(cmd, format!("points[{i}] needs a `time`: this overlay moves with the object")));
        }
        out.push(TrackedPoint { time: time.unwrap_or(Tick(i as i64)), at: Some(at), score: 1.0 });
    }
    if need_time {
        out.sort_by_key(|q| q.time);
        out.dedup_by_key(|q| q.time);
    }
    if out.len() < 2 {
        return Err(bad(cmd, "`points` needs at least two points"));
    }
    Ok(Some(out))
}

pub(crate) fn start(s: &mut Session, p: &Value, kind: Kind, cmd: &'static str) -> Result<Value> {
    if !p.is_object() && !p.is_null() {
        return Err(bad(cmd, "parameters must be an object"));
    }
    let style = style_of(p, kind, cmd)?;
    let wait = wait_p(p, cmd)?;
    let rate = s.sequence_rate();
    let track = match p.get("track") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_u64().filter(|t| *t < 1000).ok_or_else(|| bad(cmd, format!("`track` must be a video track index, got {v}")))? as usize),
    };
    let mut req = Request { cmd, style, track, tracked: false, warnings: Vec::new(), notes: Vec::new(), rate };
    if let Some(pts) = given_points(p, cmd, matches!(kind, Kind::Trail | Kind::Ring))? {
        if kind == Kind::Track {
            return Err(bad(cmd, "`points` is for the drawing commands; sports.trackPoint finds them"));
        }
        let seq = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
        return apply(s, seq, req, pts);
    }
    req.tracked = true;
    let has_seed = p.get("point").is_some_and(|v| !v.is_null()) || p.get("box").is_some_and(|v| !v.is_null());
    if !has_seed {
        return Err(bad(
            cmd,
            "give the object to follow with `point` [x, y] or `box` [x, y, w, h] (sequence pixels at `at`), or the path itself with `points`",
        ));
    }
    let at = tick_param(p, "at", cmd)?.unwrap_or_else(|| s.playhead());
    let it = super::target_clip(s, p, cmd, Some(at))?;
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let fd = q.settings.frame_rate.frame_duration().max(Tick(1));
    let view = super::ClipView::new(s, q, &it).ok_or_else(|| bad(cmd, format!("clip {} has no picture to follow anything in", it.id.0)))?;
    let src = s.source(it.item).ok_or_else(|| bad(cmd, format!("clip {} has no media to analyse", it.id.0)))?;
    if it.frame_hold.is_some() {
        return Err(bad(
            cmd,
            "the clip is a freeze frame: follow the object in the moving clip it was made from (its `start`/`end`), then draw on the freeze frame with `time`",
        ));
    }
    let start = tick_param(p, "start", cmd)?.unwrap_or(at).max(it.start);
    let end = tick_param(p, "end", cmd)?.unwrap_or_else(|| (start + Tick::from_seconds_f64(DEFAULT_SPAN_SECONDS)).min(it.end())).min(it.end());
    if end <= start {
        return Err(bad(
            cmd,
            format!(
                "nothing to follow: the range {:.3}–{:.3} s is empty within clip {} ({:.3}–{:.3} s)",
                start.seconds(),
                end.seconds(),
                it.id.0,
                it.start.seconds(),
                it.end().seconds()
            ),
        ));
    }
    if at < start || at >= end {
        return Err(bad(cmd, format!("`at` ({:.3} s) must be inside the range followed ({:.3}–{:.3} s)", at.seconds(), start.seconds(), end.seconds())));
    }
    let n = ((end - start).0 / fd.0).max(1) as usize;
    if n > super::MAX_TRACK_FRAMES {
        return Err(bad(cmd, format!("the range is {n} frames; at most {} are followed in one job: give a shorter `start`/`end`", super::MAX_TRACK_FRAMES)));
    }
    let times: Vec<Tick> = (0..n).map(|k| start + Tick(fd.0 * k as i64)).collect();
    let seed = (((at - start).0 / fd.0).max(0) as usize).min(n - 1);
    let seed_t = times[seed];
    let k = view.src_per_seq(seed_t);
    let (centre, half) = if let Some(b) = p.get("box").filter(|v| !v.is_null()) {
        let a = b.as_array().filter(|a| a.len() == 4).ok_or_else(|| bad(cmd, format!("`box` must be [x, y, w, h], got {b}")))?;
        let v: Vec<f64> = a.iter().filter_map(Value::as_f64).filter(|x| x.is_finite() && x.abs() < 1e7).collect();
        let [x, y, w, h] = v[..] else { return Err(bad(cmd, format!("`box` must hold four finite numbers, got {b}"))) };
        if !(w >= 2.0 && h >= 2.0) {
            return Err(bad(cmd, "`box` must be at least 2 pixels wide and high"));
        }
        (Vec2::new(x + w / 2.0, y + h / 2.0), (w / 2.0 * k, h / 2.0 * k))
    } else {
        let pv = p.get("point").cloned().unwrap_or(Value::Null);
        let c = point(&pv).ok_or_else(|| bad(cmd, format!("`point` must be [x, y] (finite numbers), got {pv}")))?;
        let size = num(p, "size", cmd, 24.0, 2.0, 2000.0)?;
        (c, (size / 2.0 * k, size / 2.0 * k))
    };
    let at_src = view.to_src(seed_t, centre).ok_or_else(|| bad(cmd, "the clip's Motion squashes it to nothing at `at`"))?;
    if at_src.x < 0.0 || at_src.y < 0.0 || at_src.x > f64::from(view.src.0) || at_src.y > f64::from(view.src.1) {
        return Err(bad(cmd, format!("`point`/`box` ({:.0}, {:.0}) is outside clip {}'s picture at `at`", centre.x, centre.y, it.id.0)));
    }
    let mts: Vec<Tick> = times.iter().map(|t| it.source_time_at(*t)).collect();
    if it.effect("time_remap").is_some_and(|e| e.enabled && e.is_animated()) {
        req.warnings.push("the clip is time-remapped: the object is followed at its unremapped media times".into());
    }
    let src_w = view.src.0;
    let label = match kind {
        Kind::Track => "Track Point",
        Kind::Trail => "Ball Trail",
        Kind::Ring => "Ground Ring",
        Kind::Path => "Path Line",
    };
    let (vv, tt) = (view.clone(), times.clone());
    super::launch(s, cmd, label.to_string(), n as u64, JobRequest::Overlay(req), wait, move |prog| {
        let found = super::follow(&src, src_w, &mts, seed, super::Seed { at: at_src, half }, TRACK_WIDTH, prog)?;
        Ok(Analysis::Points(tt.iter().zip(found).map(|(t, (q, score))| TrackedPoint { time: *t, at: q.map(|q| vv.to_seq(*t, q)), score }).collect()))
    })
}

/// One position per sequence frame from the first known sample to the last, and the first's
/// time. Tracked samples (one per frame) keep their gaps, short ones bridged; points the caller
/// gave are interpolated between. `None` when that span is longer than a job may follow.
fn per_frame(pts: &[TrackedPoint], rate: FrameRate, tracked: bool) -> Option<(Tick, Vec<Option<Vec2>>)> {
    let fd = rate.frame_duration().max(Tick(1));
    let first = pts.iter().position(|q| q.at.is_some())?;
    let last = pts.iter().rposition(|q| q.at.is_some())?;
    if tracked {
        let mut out: Vec<Option<Vec2>> = pts.get(first..=last)?.iter().map(|q| q.at).collect();
        bridge(&mut out, MAX_GAP);
        return Some((pts[first].time, out));
    }
    let known: Vec<(Tick, Vec2)> = pts.iter().filter_map(|q| q.at.map(|a| (q.time, a))).collect();
    let (t_first, t_last) = (known.first()?.0, known.last()?.0);
    let t0 = rate.snap(t_first).max(t_first);
    let n = usize::try_from((t_last - t0).0 / fd.0).ok()?.checked_add(1)?;
    if n > super::MAX_TRACK_FRAMES {
        return None;
    }
    let mut out = vec![None; n];
    let mut j = 0usize;
    for (k, o) in out.iter_mut().enumerate() {
        let t = t0 + Tick(fd.0 * k as i64);
        while j + 1 < known.len() && known[j + 1].0 <= t {
            j += 1;
        }
        let (ta, a) = known[j];
        *o = match known.get(j + 1) {
            _ if t == ta => Some(a),
            Some(&(tb, b)) if t > ta && tb > ta => Some(a + (b - a) * ((t - ta).0 as f64 / (tb - ta).0 as f64)),
            _ => Some(a),
        };
    }
    Some((t0, out))
}

/// Fill runs of `None` of at most `max` between two known values by linear interpolation.
fn bridge(v: &mut [Option<Vec2>], max: usize) {
    let mut last: Option<(usize, Vec2)> = None;
    for i in 0..v.len() {
        if let Some(b) = v[i] {
            if let Some((j, a)) = last
                && i > j + 1
                && i - j - 1 <= max
            {
                for (k, o) in v.iter_mut().enumerate().take(i).skip(j + 1) {
                    *o = Some(a + (b - a) * ((k - j) as f64 / (i - j) as f64));
                }
            }
            last = Some((i, b));
        }
    }
}

/// Key `id` of `e` at `times` with `vals` (component lists), keeping only the keys linear
/// interpolation needs (within `tol`). A constant value becomes the static value. Answers the
/// number of keyframes written.
fn key(e: &mut EffectInstance, id: &str, times: &[Tick], vals: &[Vec<f64>], tol: f64, make: impl Fn(&[f64]) -> ParamValue) -> usize {
    let n = times.len().min(vals.len());
    if n == 0 {
        return 0;
    }
    let ts: Vec<f64> = times.iter().map(|t| t.0 as f64).collect();
    let keep = path::thin(&ts[..n], &vals[..n], tol);
    let constant = keep.len() == 2 && vals[0].iter().zip(&vals[n - 1]).all(|(a, b)| (a - b).abs() <= tol);
    let mut prm = Param::new(make(&vals[0]));
    if n > 1 && !constant {
        for &i in &keep {
            prm.put_keyframe(times[i], make(&vals[i]));
        }
    }
    let count = prm.keyframes.len();
    e.params.insert(id.to_string(), prm);
    count
}

fn set(e: &mut EffectInstance, id: &str, v: ParamValue) {
    e.params.insert(id.to_string(), Param::new(v));
}

fn shape(kind: u32, name: &str, c: [f32; 4]) -> EffectInstance {
    let mut e = new_shape_layer(kind, Vec2::ZERO, Vec2::new(10.0, 10.0), Vec::new());
    set(&mut e, "name", ParamValue::Text(name.into()));
    set(&mut e, "fill_color", ParamValue::Color(c));
    e
}

fn glow(e: &mut EffectInstance, c: [f32; 4], width: f64) {
    set(e, "shadow", ParamValue::Bool(true));
    set(e, "shadow_color", ParamValue::Color(c));
    set(e, "shadow_opacity", ParamValue::Float(80.0));
    set(e, "shadow_distance", ParamValue::Float(0.0));
    set(e, "shadow_size", ParamValue::Float((0.3 * width).min(1000.0)));
    set(e, "shadow_blur", ParamValue::Float((1.5 * width).clamp(1.0, 1000.0)));
}

/// A rounded-rectangle segment from `a` to `b` `w` wide: (centre, size, angle in degrees).
fn segment(a: Vec2, b: Vec2, w: f64) -> (Vec2, Vec2, f64) {
    let d = b - a;
    let ang = d.y.atan2(d.x).to_degrees();
    ((a + b) * 0.5, Vec2::new(d.length() + w, w), if ang.is_finite() { ang } else { 0.0 })
}

/// `deg` moved by whole turns to within half a turn of `prev`.
fn unwrap(prev: f64, deg: f64) -> f64 {
    let mut d = deg;
    while d - prev > 180.0 {
        d -= 360.0;
    }
    while prev - d > 180.0 {
        d += 360.0;
    }
    d
}

fn pt(v: Vec2) -> Vec<f64> {
    vec![v.x, v.y]
}
fn v2(c: &[f64]) -> ParamValue {
    ParamValue::Vec2(Vec2::new(c.first().copied().unwrap_or(0.0), c.get(1).copied().unwrap_or(0.0)))
}
fn fl(c: &[f64]) -> ParamValue {
    ParamValue::Float(c.first().copied().unwrap_or(0.0))
}

/// Coverage and the spans the object was lost, for the report.
fn coverage(pts: &[TrackedPoint], rate: FrameRate) -> (f64, Vec<Value>) {
    let times: Vec<Tick> = pts.iter().map(|q| q.time).collect();
    let found = pts.iter().filter(|q| q.at.is_some()).count();
    (if pts.is_empty() { 0.0 } else { found as f64 / pts.len() as f64 }, spans(&times, rate.frame_duration(), |i| pts.get(i).is_some_and(|q| q.at.is_none())))
}

pub(crate) fn apply(s: &mut Session, seq: filmagic_project::ItemId, req: Request, pts: Vec<TrackedPoint>) -> Result<Value> {
    let Request { cmd, style, track, tracked, mut warnings, mut notes, rate } = req;
    let span_error = || bad(cmd, format!("the points span more than {} frames: draw a shorter stretch", super::MAX_TRACK_FRAMES));
    let (cov, lost) = coverage(&pts, rate);
    if cov < 0.8 && pts.iter().any(|q| q.score < 1.0) {
        warnings.push(format!("the object was found in {:.0} % of the frames: see `lost`", cov * 100.0));
    }
    let mut out = json!({"coverage": cov, "lost": lost});
    if !matches!(style, Style::Track) && s.state.active_sequence != Some(seq) {
        return Err(bad(cmd, "the active sequence changed while the object was being followed; nothing was drawn. Open the sequence and run it again"));
    }
    let fd = rate.frame_duration().max(Tick(1));
    // the stretch drawn on, looked at before and after for `pictureChange`
    let mut looked: Option<(Tick, Tick, Vec<Option<crate::story::Frame>>)> = None;
    let mut look = |s: &Session, from: Tick, len: i64| {
        let to = from + Tick(len.max(1));
        looked = Some((from, to, crate::story::looks_across(s, from, to)));
    };
    let placed = match style {
        Style::Track => {
            out["points"] = json!(
                pts.iter()
                    .map(|q| json!({"time": q.time.0, "seconds": q.time.seconds(), "x": q.at.map(|a| a.x), "y": q.at.map(|a| a.y), "found": q.at.is_some(), "score": q.score}))
                    .collect::<Vec<_>>()
            );
            out["warnings"] = json!(warnings);
            return Ok(out);
        }
        Style::Trail(t) => {
            let (t0, frames) = per_frame(&pts, rate, tracked).ok_or_else(span_error)?;
            if frames.iter().filter(|f| f.is_some()).count() < 2 {
                return Err(bad(cmd, "the object was found in fewer than two frames: nothing to draw"));
            }
            if frames.len().saturating_mul(t.length) > MAX_TRAIL_KEYS {
                return Err(bad(
                    cmd,
                    format!(
                        "{} frames × {} trail segments is too many keyframes; at most {MAX_TRAIL_KEYS}: shorten the range or the trail",
                        frames.len(),
                        t.length
                    ),
                ));
            }
            let layers = trail_layers(&frames, fd, &t);
            let dur = fd.0.saturating_mul(frames.len() as i64);
            look(s, t0, dur);
            place(s, cmd, "Ball Trail", t0, dur, track, layers, Vec::new())?
        }
        Style::Ring(r) => {
            let (t0, frames) = per_frame(&pts, rate, tracked).ok_or_else(span_error)?;
            if frames.iter().all(Option::is_none) {
                return Err(bad(cmd, "the object was not found: nothing to draw"));
            }
            let dur = fd.0.saturating_mul(frames.len() as i64);
            look(s, t0, dur);
            let ring = ring_layer(&frames, fd, &r);
            let mut placed = Vec::new();
            let spot = (r.spotlight > 0.0)
                .then(|| spotlight(&frames, fd, &r, s.active_sequence().map(|q| (q.settings.width, q.settings.height)).unwrap_or((1920, 1080))));
            let r = s.edit_group("Ground Ring", |s| {
                if let Some((layer, mask)) = spot {
                    placed.push(place(s, cmd, "Spotlight", t0, dur, track, vec![layer], vec![mask])?);
                }
                placed.push(place(s, cmd, "Ground Ring", t0, dur, track.map(|t| t + usize::from(placed.len() == 1)), vec![ring], Vec::new())?);
                Ok(())
            });
            r?;
            if placed.len() == 2 {
                out["spotlightClip"] = placed[0]["clip"].clone();
            }
            placed.pop().unwrap_or(Value::Null)
        }
        Style::Path(l) => {
            let known: Vec<Vec2> = pts.iter().filter_map(|q| q.at).collect();
            if known.len() < 2 {
                return Err(bad(cmd, "the path needs at least two known points"));
            }
            let line = path_points(&known, l.curve, &mut notes).ok_or_else(|| bad(cmd, "the points are all in one place: there is no line to draw"))?;
            let last_t = pts.iter().map(|q| q.time).max().unwrap_or_default();
            let at = l.time.unwrap_or_else(|| rate.snap(last_t + fd));
            let dur = l.duration.unwrap_or_else(|| freeze_rest(s, at).unwrap_or(Tick::from_seconds_f64(3.0)));
            let layers = path_layers(&line, &l, dur);
            look(s, at, dur.0);
            place(s, cmd, "Path Line", at, dur.0, track, layers, Vec::new())?
        }
    };
    for k in ["clip", "layers", "keyframes", "start", "end"] {
        out[k] = placed[k].clone();
    }
    out["pictureChange"] = match looked {
        Some((from, to, before)) => crate::story::change_json(&before, &crate::story::looks_across(s, from, to)),
        None => Value::Null,
    };
    out["warnings"] = json!(warnings);
    out["notes"] = json!(notes);
    Ok(out)
}

/// The rest of a freeze-frame clip at `t` (from `t` to its end).
fn freeze_rest(s: &Session, t: Tick) -> Option<Tick> {
    let q = s.active_sequence()?;
    q.video_tracks.iter().rev().find_map(|tr| tr.item_at(t).filter(|i| i.frame_hold.is_some()).map(|i| i.end() - t)).filter(|d| *d > Tick::ZERO)
}

/// Put `layers` in a new graphic clip from `t0` for `dur` ticks (on `track` when given), with
/// `masks` on its Opacity effect. Answers the clip, its layer count and keyframe count.
#[allow(clippy::too_many_arguments)]
fn place(s: &mut Session, cmd: &str, name: &str, t0: Tick, dur: i64, track: Option<usize>, layers: Vec<EffectInstance>, masks: Vec<Mask>) -> Result<Value> {
    let n = layers.len();
    let keys: usize =
        layers.iter().flat_map(|e| e.params.values()).map(|p| p.keyframes.len()).sum::<usize>() + masks.iter().map(|m| m.path.keyframes.len()).sum::<usize>();
    let mut p = json!({"time": t0.0});
    if let Some(t) = track {
        p["track"] = json!(t);
    }
    let dur = Tick(dur.max(1));
    let clip =
        crate::graphics::place_video_clip(s, cmd, name, &p, name, layers, move |pr, (w, h, rate)| (crate::graphics::graphic_source(pr, w, h, rate), dur))?;
    if !masks.is_empty() {
        let before = s.history.undo.len();
        s.edit_sequence(name, |q, _, _| {
            let (_, it) = q.find_item_mut(clip).ok_or(filmagic_edit::EditError::NoItem(clip))?;
            let op = it.effect_mut("opacity").ok_or_else(|| bad(cmd, "the graphic has no Opacity effect"))?;
            op.masks.extend(masks);
            Ok(())
        })?;
        // one undo step with the clip itself
        if s.history.undo.len() > before && before > 0 {
            s.history.undo.pop();
        }
    }
    let it = s.active_sequence().and_then(|q| q.find_item(clip)).map(|(_, i)| (i.start, i.end()));
    Ok(json!({"clip": clip.0, "layers": n, "keyframes": keys, "start": it.map(|r| r.0.seconds()), "end": it.map(|r| r.1.seconds())}))
}

/// The trail: segment `i` joins the ball's position `i` frames ago to `i + 1` frames ago.
fn trail_layers(frames: &[Option<Vec2>], fd: Tick, t: &Trail) -> Vec<EffectInstance> {
    let times: Vec<Tick> = (0..frames.len()).map(|f| Tick(fd.0 * f as i64)).collect();
    let mut out = Vec::with_capacity(t.length);
    for i in 0..t.length {
        let along = i as f64 / t.length as f64;
        let w = (t.width * (1.0 - t.taper * along)).max(0.5);
        let op = t.opacity * (1.0 - t.fade * along);
        let geo: Vec<Option<(Vec2, Vec2, f64)>> = (0..frames.len())
            .map(|f| {
                let a = frames.get(f.checked_sub(i)?).copied().flatten()?;
                let b = frames.get(f.checked_sub(i + 1)?).copied().flatten()?;
                Some(segment(b, a, w))
            })
            .collect();
        // a hidden segment keeps the geometry next to it, so nothing slides in from elsewhere
        let Some(first) = geo.iter().flatten().next().copied() else { continue };
        let mut last = first;
        let mut rot = first.2;
        let (mut pos, mut size, mut rots, mut ops) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for g in &geo {
            if let Some(g) = g {
                last = *g;
            }
            rot = unwrap(rot, last.2);
            pos.push(pt(last.0));
            size.push(pt(last.1));
            rots.push(vec![rot]);
            ops.push(vec![if g.is_some() { op } else { 0.0 }]);
        }
        let mut e = shape(0, &format!("Trail {:02}", i + 1), t.color);
        set(&mut e, "corner_radius", ParamValue::Float(w / 2.0));
        key(&mut e, "position", &times, &pos, 0.25, v2);
        key(&mut e, "size", &times, &size, 0.25, v2);
        key(&mut e, "rotation", &times, &rots, 0.1, fl);
        key(&mut e, "opacity", &times, &ops, 0.5, fl);
        if t.glow {
            glow(&mut e, t.color, w);
        }
        out.push(e);
    }
    out
}

fn hidden_fill(frames: &[Option<Vec2>]) -> Vec<(Vec2, bool)> {
    let first = frames.iter().flatten().next().copied().unwrap_or(Vec2::ZERO);
    let mut last = first;
    frames
        .iter()
        .map(|f| {
            if let Some(v) = f {
                last = *v;
            }
            (last, f.is_some())
        })
        .collect()
}

fn ring_layer(frames: &[Option<Vec2>], fd: Tick, r: &Ring) -> EffectInstance {
    let times: Vec<Tick> = (0..frames.len()).map(|f| Tick(fd.0 * f as i64)).collect();
    let filled = hidden_fill(frames);
    let mut e = shape(1, "Ground Ring", [r.color[0], r.color[1], r.color[2], (r.fill / 100.0) as f32]);
    set(&mut e, "size", ParamValue::Vec2(Vec2::new(r.diameter, r.diameter * r.squash)));
    set(&mut e, "fill", ParamValue::Bool(r.fill > 0.0));
    set(&mut e, "stroke", ParamValue::Bool(true));
    set(&mut e, "stroke_color", ParamValue::Color(r.color));
    set(&mut e, "stroke_width", ParamValue::Float(r.width));
    set(&mut e, "stroke_type", ParamValue::Choice(1));
    let pos: Vec<Vec<f64>> = filled.iter().map(|(p, _)| pt(*p + r.offset)).collect();
    let ops: Vec<Vec<f64>> = filled.iter().map(|(_, on)| vec![if *on { 100.0 } else { 0.0 }]).collect();
    key(&mut e, "position", &times, &pos, 0.25, v2);
    key(&mut e, "opacity", &times, &ops, 0.5, fl);
    if r.glow {
        glow(&mut e, r.color, r.width * 2.0);
    }
    e
}

/// A black layer over the frame and the inverted, keyframed mask that lets the spot through.
fn spotlight(frames: &[Option<Vec2>], fd: Tick, r: &Ring, (w, h): (u32, u32)) -> (EffectInstance, Mask) {
    let (w, h) = (f64::from(w), f64::from(h));
    let mut e = shape(0, "Spotlight", [0.0, 0.0, 0.0, 1.0]);
    set(&mut e, "position", ParamValue::Vec2(Vec2::new(w / 2.0, h / 2.0)));
    set(&mut e, "size", ParamValue::Vec2(Vec2::new(w + 4.0, h + 4.0)));
    set(&mut e, "opacity", ParamValue::Float(r.spotlight));
    let filled = hidden_fill(frames);
    let radius = r.spot_size * 0.5;
    let centre = |p: Vec2| p + r.offset + r.spot_offset;
    let mut mask = Mask::new("Spotlight", MaskPath::ellipse(centre(filled.first().map(|f| f.0).unwrap_or_default()), radius));
    mask.inverted = true;
    mask.feather = Param::new(ParamValue::Float(r.feather));
    let times: Vec<f64> = (0..filled.len()).map(|f| (fd.0 * f as i64) as f64).collect();
    let vals: Vec<Vec<f64>> = filled.iter().map(|(p, _)| pt(centre(*p))).collect();
    let keep = path::thin(&times, &vals, 0.25);
    if keep.len() > 2 || vals.first() != vals.last() {
        for i in keep {
            mask.path.put_keyframe(Tick(fd.0 * i as i64), ParamValue::Path(MaskPath::ellipse(centre(filled[i].0), radius)));
        }
    }
    (e, mask)
}

/// The curve a path line follows, as a dense polyline.
fn path_points(known: &[Vec2], curve: Curve, notes: &mut Vec<String>) -> Option<Vec<Vec2>> {
    // drop repeats (a still ball) so the curve has corners only where the path turns
    let mut pts: Vec<Vec2> = Vec::with_capacity(known.len());
    for &p in known {
        if pts.last().is_none_or(|l| (*l - p).length() > 0.5) {
            pts.push(p);
        }
    }
    if pts.len() < 2 {
        return None;
    }
    let line = match curve {
        Curve::Line => pts,
        Curve::Smooth => {
            // a long tracked path is thinned first: a spline through every frame keeps its noise
            let step = (pts.len() / 24).max(1);
            let mut ctrl: Vec<Vec2> = pts.iter().step_by(step).copied().collect();
            if let (Some(l), Some(c)) = (pts.last(), ctrl.last())
                && (*l - *c).length() > 0.5
            {
                ctrl.push(*l);
            }
            path::catmull_rom(&ctrl, 8)
        }
        Curve::Arc => match path::parabola(&pts, 96) {
            Some(a) => a,
            None => {
                notes.push("no arc fits fewer than three distinct points: drawn as a line".into());
                pts
            }
        },
    };
    (path::length(&line) > 1.0).then_some(line)
}

fn path_layers(line: &[Vec2], l: &PathLine, dur: Tick) -> Vec<EffectInstance> {
    let total = path::length(line);
    // segments about 4 widths long, at most MAX_PATH_SEGMENTS (dashes: every other one)
    let pieces = ((total / (4.0 * l.width)).ceil() as usize).clamp(1, MAX_PATH_SEGMENTS) * if l.dashed { 2 } else { 1 };
    let pieces = pieces.min(MAX_PATH_SEGMENTS * 2);
    let Some(even) = path::resample(line, pieces) else { return Vec::new() };
    let drawn: Vec<usize> = (0..pieces).filter(|i| !l.dashed || i % 2 == 0).collect();
    let reveal = l.reveal.min(dur);
    let mut out = Vec::with_capacity(drawn.len() + 1);
    for (j, &i) in drawn.iter().enumerate() {
        let (a, b) = (even[i], even[i + 1]);
        let (c, size, ang) = segment(a, b, l.width);
        let mut e = shape(0, &format!("Path {:02}", j + 1), l.color);
        set(&mut e, "position", ParamValue::Vec2(c));
        set(&mut e, "size", ParamValue::Vec2(size));
        set(&mut e, "rotation", ParamValue::Float(ang));
        set(&mut e, "corner_radius", ParamValue::Float(l.width / 2.0));
        if reveal > Tick::ZERO {
            let at = |k: f64| Tick((reveal.0 as f64 * k / pieces as f64).round() as i64);
            let mut prm = Param::new(ParamValue::Float(100.0));
            prm.put_keyframe(at(i as f64), ParamValue::Float(0.0));
            prm.put_keyframe(at(i as f64 + 1.0).max(at(i as f64) + Tick(1)), ParamValue::Float(100.0));
            e.params.insert("opacity".into(), prm);
        }
        if l.glow {
            glow(&mut e, l.color, l.width);
        }
        out.push(e);
    }
    if l.arrow
        && let (Some(&tip), Some(&before)) = (even.last(), even.get(even.len().saturating_sub(2)))
    {
        let d = tip - before;
        let (len, half) = (4.0 * l.width, 2.0 * l.width);
        let pts = vec![[0.0, 0.0], [-len as f32, -half as f32], [-len as f32, half as f32]];
        let mut e = new_shape_layer(3, tip, Vec2::new(len, 2.0 * half), pts);
        set(&mut e, "name", ParamValue::Text("Arrow".into()));
        set(&mut e, "fill_color", ParamValue::Color(l.color));
        set(&mut e, "rotation", ParamValue::Float(d.y.atan2(d.x).to_degrees()));
        if reveal > Tick::ZERO {
            let mut prm = Param::new(ParamValue::Float(100.0));
            prm.put_keyframe((reveal - Tick(1)).max(Tick::ZERO), ParamValue::Float(0.0));
            prm.put_keyframe(reveal, ParamValue::Float(100.0));
            e.params.insert("opacity".into(), prm);
        }
        if l.glow {
            glow(&mut e, l.color, l.width);
        }
        out.push(e);
    }
    out
}
