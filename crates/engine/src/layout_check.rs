//! `layout.check` and `graphics.anchor` (#16): where graphics, titles and captions sit on the
//! frame, measured from the picture.
//!
//! `layout.check {fromSeconds?, toSeconds?, clip?}` looks at every graphic clip (titles, shapes,
//! lower thirds, templates) and every caption in the range and reports:
//!
//! - **overlaps**: two of them on screen at the same time whose drawn areas cross;
//! - **safe areas**: one that reaches outside the title-safe area, the action-safe area or the
//!   frame. The areas come from Project Settings ▸ General (default: title safe 20 %, action safe
//!   10 %: the percentage of the frame's width and height left as a margin, half on each side, as
//!   the Program monitor's safe margins draw them);
//! - **busy picture**: one placed over a part of the underlying picture that is busy or looks like
//!   text (a burned-in scoreboard, a channel bug, a sign). This is a **heuristic**: the share of
//!   strong luma edges under the overlay and the spread of its luma. It finds sharp, high-contrast
//!   detail, not meaning: a striped shirt reads as busy too, and soft text can be missed.
//!
//! Each overlay's drawn area is measured, not computed: the frame is rendered with that overlay
//! alone, and its pixels with any opacity are its box. Rolls, pins, Motion and opacity are all
//! counted that way.
//!
//! `graphics.anchor {clip?, anchor, safe="title", margin=0}` moves a graphic's layers together so
//! their box sits in a corner, at an edge or in the centre of the title-safe area, the
//! action-safe area or the frame.

use std::sync::Arc;

use filmagic_project::{ClipId, ItemKind, Project, ProjectSettings, Sequence, TrackItem};
use filmagic_time::Tick;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, NamedKeys, bad, clip_p, has_seq};
use crate::{EngineError, Result, Session};

const CMD: &str = "layout.check";
/// Most overlays one call looks at.
pub const MAX_OVERLAYS: usize = 60;
/// Longest side of the frames measured.
const SIDE: f32 = 480.0;
/// Alpha above which a pixel counts as drawn.
const DRAWN: f32 = 0.04;
/// A luma step (0..1) between neighbours this large is a strong edge.
const EDGE: f32 = 0.12;
/// Strong-edge share from which the picture under an overlay is busy.
pub const BUSY_EDGES: f64 = 0.06;
/// Strong-edge share and luma spread from which it looks like text or a graphic.
pub const TEXT_EDGES: f64 = 0.10;
pub const TEXT_CONTRAST: f64 = 0.12;
/// Overlaps smaller than this share of the smaller box are ignored (anti-aliased edges touching).
const OVERLAP_SHARE: f64 = 0.02;

/// A safe area as `[x0, y0, x1, y1]` in pixels of a `w`×`h` frame: `pct` (horizontal, vertical)
/// per cent of the frame is margin, half on each side.
pub fn safe_rect(pct: (f64, f64), w: f64, h: f64) -> [f64; 4] {
    let clamp = |v: f64| if v.is_finite() { v.clamp(0.0, 90.0) } else { 0.0 };
    let (mx, my) = (w * clamp(pct.0) / 200.0, h * clamp(pct.1) / 200.0);
    [mx, my, w - mx, h - my]
}

/// The named safe area of the project's settings: `title`, `action` or `frame`.
pub fn named_safe(st: &ProjectSettings, name: &str, w: f64, h: f64) -> Option<[f64; 4]> {
    match name {
        "title" => Some(safe_rect(st.title_safe, w, h)),
        "action" => Some(safe_rect(st.action_safe, w, h)),
        "frame" | "none" => Some([0.0, 0.0, w, h]),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Graphic(ClipId),
    /// (caption track index, caption id)
    Caption(usize, ClipId),
}

struct Overlay {
    kind: Kind,
    name: String,
    start: Tick,
    end: Tick,
    /// The project with only this overlay showing.
    alone: Arc<Project>,
}

/// Whether a timeline clip is a graphic (a title, a shape, a template).
fn is_overlay_clip(p: &Project, it: &TrackItem) -> bool {
    it.enabled && (matches!(p.item(it.item).map(|i| &i.kind), Some(ItemKind::Graphic { .. })) || it.effects.iter().any(filmagic_project::graphic::is_layer))
}

/// The project with every picture clip and caption track of `seq` hidden but `keep` (and, for
/// `footage`, with only what is not an overlay showing).
fn variant(p: &Project, seq: filmagic_project::ItemId, keep: Option<Kind>, footage: bool) -> Option<Arc<Project>> {
    let mut v = p.clone();
    let overlay: Vec<bool> = {
        let q = p.sequence(seq)?;
        q.video_tracks.iter().flat_map(|t| t.items.iter()).map(|it| is_overlay_clip(p, it)).collect()
    };
    let q = v.sequence_mut(seq)?;
    let mut k = 0;
    for t in q.video_tracks.iter_mut() {
        for it in t.items.iter_mut() {
            let is_overlay = overlay.get(k).copied().unwrap_or(false);
            k += 1;
            it.enabled = if footage { it.enabled && !is_overlay } else { keep == Some(Kind::Graphic(it.id)) };
        }
    }
    for (i, t) in q.caption_tracks.iter_mut().enumerate() {
        let wanted = matches!(keep, Some(Kind::Caption(ti, _)) if ti == i);
        if footage || !wanted {
            t.enabled = false;
        }
        if wanted && let Some(Kind::Caption(_, id)) = keep {
            t.captions.retain(|c| c.id == id);
        }
    }
    Some(Arc::new(v))
}

fn scale_of(q: &Sequence) -> f32 {
    (SIDE / q.settings.width.max(q.settings.height).max(1) as f32).min(1.0)
}

fn render(s: &Session, p: &Arc<Project>, seq: filmagic_project::ItemId, t: Tick, captions: bool) -> Option<filmagic_render::Image> {
    let q = p.sequence(seq)?;
    let provider = s.media.provider(p.clone(), s.services.clone());
    let opts = filmagic_render::RenderOptions { scale: scale_of(q), captions, ..Default::default() };
    Some(filmagic_render::render_sequence(p, seq, q.frame_rate().snap(t.max(Tick::ZERO)), opts, &provider))
}

/// Box `[x0, y0, x1, y1]` of the drawn pixels of `img`, in pixels of the `w`×`h` sequence frame.
fn drawn_box(img: &filmagic_render::Image, w: f64, h: f64) -> Option<[f64; 4]> {
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
    for y in 0..img.h {
        for x in 0..img.w {
            if img.px.get((y * img.w + x) * 4 + 3).is_some_and(|a| *a > DRAWN) {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
            }
        }
    }
    if x0 == usize::MAX || img.w == 0 || img.h == 0 {
        return None;
    }
    let (kx, ky) = (w / img.w as f64, h / img.h as f64);
    Some([x0 as f64 * kx, y0 as f64 * ky, x1 as f64 * kx, y1 as f64 * ky].map(|v| v.round()))
}

/// What the picture under a box is like: (strong-edge share, luma spread).
pub fn busyness(img: &filmagic_render::Image, b: [f64; 4], w: f64, h: f64) -> Option<(f64, f64)> {
    if img.w < 2 || img.h < 2 || w <= 0.0 || h <= 0.0 {
        return None;
    }
    let (kx, ky) = (img.w as f64 / w, img.h as f64 / h);
    let x0 = ((b[0] * kx).floor().max(0.0) as usize).min(img.w - 1);
    let y0 = ((b[1] * ky).floor().max(0.0) as usize).min(img.h - 1);
    let x1 = ((b[2] * kx).ceil().max(0.0) as usize).min(img.w);
    let y1 = ((b[3] * ky).ceil().max(0.0) as usize).min(img.h);
    let luma = |x: usize, y: usize| {
        let p = img.get(x, y);
        let a = p[3].clamp(0.0, 1.0);
        // over black, as the frame is seen
        (0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]).clamp(0.0, 1.0) * if a > 0.0 { 1.0 } else { 0.0 }
    };
    let (mut edges, mut n, mut sum, mut sq) = (0u64, 0u64, 0.0f64, 0.0f64);
    for y in y0..y1 {
        for x in x0..x1 {
            let l = luma(x, y);
            let right = if x + 1 < img.w { (luma(x + 1, y) - l).abs() } else { 0.0 };
            let down = if y + 1 < img.h { (luma(x, y + 1) - l).abs() } else { 0.0 };
            if right.max(down) > EDGE {
                edges += 1;
            }
            n += 1;
            sum += f64::from(l);
            sq += f64::from(l) * f64::from(l);
        }
    }
    if n == 0 {
        return None;
    }
    let mean = sum / n as f64;
    let spread = (sq / n as f64 - mean * mean).max(0.0).sqrt();
    Some((edges as f64 / n as f64, spread))
}

fn round3(v: f64) -> f64 {
    if v.is_finite() { (v * 1000.0).round() / 1000.0 } else { 0.0 }
}

fn area(b: [f64; 4]) -> f64 {
    (b[2] - b[0]).max(0.0) * (b[3] - b[1]).max(0.0)
}

fn intersect(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    [a[0].max(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].min(b[3])]
}

/// How far a box reaches outside `r`, per side: (left, top, right, bottom), 0 when inside.
fn outside(b: [f64; 4], r: [f64; 4]) -> [f64; 4] {
    [(r[0] - b[0]).max(0.0), (r[1] - b[1]).max(0.0), (b[2] - r[2]).max(0.0), (b[3] - r[3]).max(0.0)]
}

fn kind_json(k: Kind) -> Value {
    match k {
        Kind::Graphic(c) => json!({"kind": "graphic", "clip": c.0}),
        Kind::Caption(t, c) => json!({"kind": "caption", "captionTrack": t, "caption": c.0}),
    }
}

fn who(o: &Overlay) -> String {
    match o.kind {
        Kind::Graphic(c) => format!("graphic clip {} (\"{}\")", c.0, o.name),
        Kind::Caption(_, c) => format!("caption {} (\"{}\")", c.0, o.name),
    }
}

fn seconds_p(p: &Value, key: &str) -> Result<Option<Tick>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_f64()
            .filter(|x| x.is_finite() && (0.0..=1e7).contains(x))
            .map(|x| Some(Tick::from_seconds_f64(x)))
            .ok_or_else(|| bad(CMD, format!("`{key}` must be seconds of the sequence, 0 or more"))),
    }
}

fn overlays(s: &Session, seq: filmagic_project::ItemId, from: Tick, to: Tick, only: Option<ClipId>) -> Vec<Overlay> {
    let Some(q) = s.project.sequence(seq) else { return Vec::new() };
    let mut out = Vec::new();
    for t in q.video_tracks.iter().filter(|t| t.enabled) {
        for it in t.items.iter().filter(|it| is_overlay_clip(&s.project, it) && it.start < to && it.end() > from) {
            if only.is_some_and(|c| c != it.id) {
                continue;
            }
            let kind = Kind::Graphic(it.id);
            if let Some(alone) = variant(&s.project, seq, Some(kind), false) {
                out.push(Overlay { kind, name: it.name.clone(), start: it.start.max(from), end: it.end().min(to), alone });
            }
        }
    }
    if only.is_none() {
        for (i, t) in q.caption_tracks.iter().enumerate().filter(|(_, t)| t.enabled) {
            for c in t.captions.iter().filter(|c| c.start < to && c.start + c.duration > from) {
                let kind = Kind::Caption(i, c.id);
                let name: String = c.text.chars().take(40).collect();
                if let Some(alone) = variant(&s.project, seq, Some(kind), false) {
                    out.push(Overlay { kind, name, start: c.start.max(from), end: (c.start + c.duration).min(to), alone });
                }
            }
        }
    }
    out.sort_by_key(|o| o.start);
    out
}

fn check(s: &mut Session, p: &Value) -> Result<Value> {
    let seq = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let q = s.project.sequence(seq).ok_or(EngineError::NoSequence)?;
    let rate = q.frame_rate();
    let (w, h) = (f64::from(q.settings.width.max(1)), f64::from(q.settings.height.max(1)));
    let end = q.duration();
    let only = match p.get("clip") {
        None | Some(Value::Null) => None,
        Some(_) => {
            let c = clip_p(p, "clip").ok_or_else(|| bad(CMD, "`clip` must be a timeline clip id"))?;
            let (_, it) = q.find_item(c).ok_or_else(|| bad(CMD, format!("no clip {} in the active sequence (`sequence_inspect` lists them)", c.0)))?;
            if !is_overlay_clip(&s.project, it) {
                return Err(bad(CMD, format!("clip {} is not a graphic, title or caption; leave out `clip` to check them all", c.0)));
            }
            Some(c)
        }
    };
    let from = seconds_p(p, "fromSeconds")?.unwrap_or(Tick::ZERO);
    let to = seconds_p(p, "toSeconds")?.unwrap_or(end).min(end.max(Tick(1)));
    if to <= from {
        return Err(bad(CMD, "nothing to check between `fromSeconds` and `toSeconds` (or the sequence is empty)"));
    }
    let st = s.project.settings.clone();
    let (title, action) = (safe_rect(st.title_safe, w, h), safe_rect(st.action_safe, w, h));
    let frame = [0.0, 0.0, w, h];
    let mut all = overlays(s, seq, from, to, only);
    let truncated = all.len() > MAX_OVERLAYS;
    all.truncate(MAX_OVERLAYS);
    let footage = variant(&s.project, seq, None, true).ok_or(EngineError::NoSequence)?;
    let mut items = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    // each overlay's box at its middle frame, for the overlap check
    let mut mids: Vec<Option<[f64; 4]>> = Vec::new();
    let last_frame = |o: &Overlay| rate.snap((o.end - Tick(1)).max(o.start));
    for o in &all {
        let mid = rate.snap(o.start + Tick((o.end - o.start).0 / 2));
        // the first frame on screen: the frame its start falls in may still show what was before
        let first =
            Some(rate.snap(o.start)).filter(|f| *f >= o.start).unwrap_or_else(|| rate.tick_of(rate.frame_at(o.start).saturating_add(1))).min(last_frame(o));
        let times = [first, mid.max(first), last_frame(o)];
        let captions = matches!(o.kind, Kind::Caption(..));
        let mut samples = Vec::new();
        let mut worst: Option<(&str, [f64; 4], Tick)> = None;
        let mut mid_box = None;
        for (k, t) in times.iter().enumerate() {
            if k > 0 && times[..k].contains(t) {
                continue;
            }
            let Some(img) = render(s, &o.alone, seq, *t, captions) else { continue };
            let Some(b) = drawn_box(&img, w, h) else { continue };
            if *t == mid {
                mid_box = Some(b);
            }
            samples.push(json!({"seconds": round3(t.seconds()), "box": b}));
            let level = if outside(b, frame).iter().any(|v| *v > 0.0) || b[0] <= 0.0 || b[1] <= 0.0 || b[2] >= w || b[3] >= h {
                Some("frame")
            } else if outside(b, action).iter().any(|v| *v > 0.0) {
                Some("actionSafe")
            } else if outside(b, title).iter().any(|v| *v > 0.0) {
                Some("titleSafe")
            } else {
                None
            };
            let rank = |l: &str| match l {
                "frame" => 3,
                "actionSafe" => 2,
                _ => 1,
            };
            if let Some(l) = level
                && worst.is_none_or(|(x, _, _)| rank(l) > rank(x))
            {
                worst = Some((l, b, *t));
            }
        }
        let mut item = kind_json(o.kind);
        item["name"] = json!(o.name);
        item["start"] = json!(round3(o.start.seconds()));
        item["end"] = json!(round3(o.end.seconds()));
        item["samples"] = json!(samples);
        if samples.is_empty() {
            item["drawn"] = json!(false);
            warnings.push(format!(
                "{} draws nothing visible between {:.3} s and {:.3} s (empty text, zero opacity or off the frame)",
                who(o),
                o.start.seconds(),
                o.end.seconds()
            ));
        }
        item["outside"] = worst.map_or(Value::Null, |(l, _, _)| json!(l));
        if let Some((l, b, t)) = worst {
            let (area_name, r) = match l {
                "frame" => ("the frame edge", frame),
                "actionSafe" => ("the action-safe area", action),
                _ => ("the title-safe area", title),
            };
            let by = outside(b, r).map(|v| v.round());
            let what = if l == "frame" { "runs off" } else { "reaches outside" };
            warnings.push(format!(
                "{} {what} {area_name} at {:.3} s: its box [{:.0}, {:.0}, {:.0}, {:.0}] is past it by {:.0} px left, {:.0} top, {:.0} right, {:.0} bottom. Move it inside (`graphics.anchor` {{\"anchor\":\"…\",\"safe\":\"title\"}})",
                who(o),
                t.seconds(),
                b[0], b[1], b[2], b[3],
                by[0], by[1], by[2], by[3]
            ));
        }
        // the picture under it, at its middle frame
        if let Some(b) = mid_box
            && let Some(under) = render(s, &footage, seq, mid, false)
            && let Some((edges, spread)) = busyness(&under, b, w, h)
        {
            let reads = if edges >= TEXT_EDGES && spread >= TEXT_CONTRAST {
                "textLike"
            } else if edges >= BUSY_EDGES {
                "busy"
            } else {
                "calm"
            };
            item["under"] = json!({"edgeDensity": round3(edges), "contrast": round3(spread), "reads": reads, "seconds": round3(mid.seconds())});
            if reads != "calm" {
                let like = if reads == "textLike" { "busy, text-like detail (a burned-in score, logo or sign?)" } else { "busy detail" };
                warnings.push(format!(
                    "{} sits over {like} in the picture at {:.3} s (edge density {:.2}, contrast {:.2}): it may cover something or be hard to read. Look with `render_frame`, then move it (`graphics.anchor`) or give it a background",
                    who(o),
                    mid.seconds(),
                    edges,
                    spread
                ));
            }
        }
        items.push(item);
        mids.push(mid_box);
    }
    // overlaps: two overlays on screen together whose boxes cross, measured where both show
    let mut overlaps = Vec::new();
    for i in 0..all.len() {
        for j in i + 1..all.len() {
            let (a, b) = (&all[i], &all[j]);
            let (lo, hi) = (a.start.max(b.start), a.end.min(b.end));
            if hi <= lo {
                continue;
            }
            let t = rate.snap(lo + Tick((hi - lo).0 / 2)).max(rate.snap(lo));
            let at = |o: &Overlay| render(s, &o.alone, seq, t, matches!(o.kind, Kind::Caption(..))).and_then(|img| drawn_box(&img, w, h));
            let (Some(ba), Some(bb)) = (at(a), at(b)) else { continue };
            let x = intersect(ba, bb);
            let shared = area(x);
            if shared <= 0.0 || shared < OVERLAP_SHARE * area(ba).min(area(bb)) {
                continue;
            }
            overlaps.push(json!({"a": kind_json(a.kind), "b": kind_json(b.kind), "seconds": round3(t.seconds()), "box": x, "area": shared.round()}));
            warnings.push(format!(
                "{} and {} overlap at {:.3} s over [{:.0}, {:.0}, {:.0}, {:.0}]: one covers the other. Move one (`graphics.anchor`) or change when it shows",
                who(a),
                who(b),
                t.seconds(),
                x[0],
                x[1],
                x[2],
                x[3]
            ));
        }
    }
    let mut notes = vec![
        "boxes are measured from the picture: where each overlay alone draws anything".to_string(),
        "`under` is a heuristic (share of strong luma edges, luma spread under the overlay): it flags sharp, high-contrast detail such as burned-in text, not meaning".to_string(),
    ];
    if truncated {
        notes.push(format!("only the first {MAX_OVERLAYS} graphics and captions were checked; pass `fromSeconds`/`toSeconds` to check the rest"));
    }
    if all.is_empty() {
        notes.push("there are no graphics, titles or captions in this range".into());
    }
    Ok(json!({
        "range": {"fromSeconds": round3(from.seconds()), "toSeconds": round3(to.seconds())},
        "frame": [w, h],
        "safeAreas": {
            "title": {"percent": [st.title_safe.0, st.title_safe.1], "box": title},
            "action": {"percent": [st.action_safe.0, st.action_safe.1], "box": action},
            "note": "per cent of the frame's width and height left as margin, half on each side (Project Settings ▸ General)",
        },
        "ok": warnings.is_empty(),
        "items": items,
        "overlaps": overlaps,
        "truncated": truncated,
        "warnings": warnings,
        "notes": notes,
    }))
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: CMD,
        label: "Check Layout",
        menu: &[],
        shortcut: None,
        params: r#"{"fromSeconds":f64?,"toSeconds":f64? (default: the whole sequence),"clip":id? (one graphic only)} -> {ok,items:[{kind,clip|caption,samples:[{seconds,box}],outside:"titleSafe|actionSafe|frame"|null,under:{edgeDensity,contrast,reads:"calm|busy|textLike"}}],overlaps,safeAreas,warnings}"#,
        enabled: has_seq,
        run: check,
        journal: false,
        named: NamedKeys::NONE,
    }]
}

#[cfg(test)]
mod tests;
