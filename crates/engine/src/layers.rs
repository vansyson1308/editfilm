//! Layers inside a shot: treat a region of a clip as a layer of its own.
//!
//! A clip is one picture, but an edit often needs only part of it: move the figure, slow the
//! skater and leave the rest at speed, grade the car and not the dust, take the passer-by out.
//! These commands build that from parts the editor already has (masks, mask tracking, tracks, clip
//! speed, effects), in one call and one undo step each, so a person or an agent can say what they
//! want done to a region instead of assembling it.
//!
//! | Id | What it does |
//! |---|---|
//! | `layers.extract` | Copies the clip onto a new track above and masks the copy to the region: the region is now a clip of its own to move, scale, grade, re-time or switch off. Optionally the mask follows the subject, and the region is cut out of the original. |
//! | `layers.cleanPlate` | Makes a still of the shot's background with whatever moved through it taken out (the per-pixel median over the shot), and lays it over the clip, whole or only over a region (which removes what was there). |
//! | `layers.lift` | Both: the subject goes onto its own layer and the plate fills the place it left, so the layer can be moved without leaving a double. |
//! | `layers.retime` | Plays the region at another speed while the rest of the shot keeps its own. |
//! | `layers.adjust` | Applies an effect to the region only. |
//! | `layers.transform` | Makes a layer larger or smaller, or shifts it, in place: the layer's Motion is keyed so that a point of its mask (the subject's feet, by default) stays where it stood while the subject moves through the shot. |
//!
//! A region is given in fractions (0…1 from the top-left corner), so it does not depend on the
//! size a frame was looked at: `{"shape":"rectangle"|"ellipse","x":…,"y":…,"w":…,"h":…}` or
//! `{"points":[[x,y],…]}`. The fractions are of the clip's own picture, which is what a mask is
//! drawn on; with `"space":"frame"` they are of the programme frame as rendered, and are carried
//! back through the clip's placement (Motion, Scale to Frame Size, pixel shape). The two are the
//! same thing while the clip fills the frame; they part as soon as it is moved, zoomed or turned.
//!
//! What these commands cannot do is invent pixels. A clean plate is a statistic of the frames that
//! exist: it needs a camera that does not move and a subject that does. Nothing here replaces an
//! object with a different one, or shows what was never photographed.

use filmagic_geom::{Affine, Vec2};
use filmagic_project::{ClipId, ItemId, Keyframe, MaskMode, MaskPath, Param, ParamValue, TrackItem, TrackKind};
use filmagic_time::Tick;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, NamedKeys, bad, clip_p, has_seq, str_p, u64_p};
use crate::{EngineError, Result, Session};

type Run = fn(&mut Session, &Value) -> Result<Value>;

/// Most bytes of frames one clean plate is made from.
const PLATE_BYTES: usize = 768 << 20;

const REGION_DOC: &str = r#"{"shape":"rectangle"|"ellipse","x":0..1,"y":0..1,"w":0..1,"h":0..1}|{"points":[[x,y],…]}, either with "space":"clip"|"frame"="clip" (fractions of the clip's own picture, or of the programme frame as rendered) and "atSeconds":f64? (the frame the region was read from, for `space` "frame"; default: the clip's first)"#;

fn edit(id: &'static str, label: &'static str, params: &'static str, run: Run) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: has_seq, run, journal: true, named: NamedKeys::NONE }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        edit(
            "layers.extract",
            "Extract Region to a Layer",
            r#"{"clip":id,"region":{"shape":"rectangle"|"ellipse","x":0..1,"y":0..1,"w":0..1,"h":0..1}|{"points":[[x,y],…]}, either with "space":"clip"|"frame"="clip" (fractions of the clip's own picture, or of the programme frame as rendered) and "atSeconds":f64? (the frame the region was read from, for `space` "frame"; default: the clip's first),"feather":px=12,"follow":bool=false (track the subject through the clip),"cutOut":bool=false (cut the region out of the original),"name":str?}"#,
            extract,
        ),
        edit(
            "layers.cleanPlate",
            "Make a Clean Plate",
            r#"{"clip":id,"path":str (PNG to write),"samples":n=15,"region":{"shape":"rectangle"|"ellipse","x":0..1,"y":0..1,"w":0..1,"h":0..1}|{"points":[[x,y],…]}, either with "space":"clip"|"frame"="clip" (fractions of the clip's own picture, or of the programme frame as rendered) and "atSeconds":f64? (the frame the region was read from, for `space` "frame"; default: the clip's first)? (cover only this; default: the whole frame),"feather":px=24,"place":bool=true}"#,
            clean_plate,
        ),
        edit(
            "layers.lift",
            "Lift Subject to a Layer",
            r#"{"clip":id,"region":{"shape":"rectangle"|"ellipse","x":0..1,"y":0..1,"w":0..1,"h":0..1}|{"points":[[x,y],…]}, either with "space":"clip"|"frame"="clip" (fractions of the clip's own picture, or of the programme frame as rendered) and "atSeconds":f64? (the frame the region was read from, for `space` "frame"; default: the clip's first),"path":str (PNG of the plate),"feather":px=12,"samples":n=15,"follow":bool=false}"#,
            lift,
        ),
        edit(
            "layers.retime",
            "Retime a Region",
            r#"{"clip":id,"region":{"shape":"rectangle"|"ellipse","x":0..1,"y":0..1,"w":0..1,"h":0..1}|{"points":[[x,y],…]}, either with "space":"clip"|"frame"="clip" (fractions of the clip's own picture, or of the programme frame as rendered) and "atSeconds":f64? (the frame the region was read from, for `space` "frame"; default: the clip's first),"speed":percent,"feather":px=12,"interpolation":"frameSampling|frameBlending|opticalFlow"?}"#,
            retime,
        ),
        edit(
            "layers.adjust",
            "Adjust a Region",
            r#"{"clip":id,"region":{"shape":"rectangle"|"ellipse","x":0..1,"y":0..1,"w":0..1,"h":0..1}|{"points":[[x,y],…]}, either with "space":"clip"|"frame"="clip" (fractions of the clip's own picture, or of the programme frame as rendered) and "atSeconds":f64? (the frame the region was read from, for `space` "frame"; default: the clip's first),"effect":id|name,"params":{param:value,…}?,"feather":px=12,"follow":bool=false}"#,
            adjust,
        ),
        edit(
            "layers.transform",
            "Transform a Layer in Place",
            r#"{"layer":id (a clip with an opacity mask: `layers.extract` and `layers.lift` return one; `clip` names it too),"scale":percent? (the layer's Motion Scale, 0.1 to 10000),"offset":[dx,dy]? (sequence pixels, from where the subject is now),"anchor":"bottom"|"center"|"top"|[fx,fy]="bottom" (the point of the mask's bounds, as seen in the frame, that stays where it is),"mask":n? (which opacity mask; default: the last that keeps a region)}"#,
            transform,
        ),
    ]
}

/// Run a registered command's body as a step of a larger edit.
fn run(s: &mut Session, id: &str, p: Value) -> Result<Value> {
    let f = crate::commands::find(id).ok_or_else(|| EngineError::UnknownCommand(id.into()))?.run;
    f(s, &p)
}

fn fraction(v: Option<&Value>, what: &str, cmd: &str) -> Result<f64> {
    v.and_then(Value::as_f64)
        .filter(|x| x.is_finite() && (-1.0..=2.0).contains(x))
        .ok_or_else(|| bad(cmd, format!("region `{what}` must be a fraction (0 to 1) of the clip's own picture, or of the frame with `space` \"frame\"")))
}

/// Where a region's fractions are measured.
enum Space {
    /// The clip's own picture: (width, height) in its pixels.
    Clip(f64, f64),
    /// The programme frame, (width, height), and the way back from it into the clip's pixels.
    Frame(f64, f64, filmagic_geom::Affine),
}

impl Space {
    /// A point given in fractions, in clip pixels.
    fn point(&self, x: f64, y: f64) -> (f64, f64) {
        match self {
            Space::Clip(w, h) => (x * w, y * h),
            Space::Frame(w, h, back) => {
                let p = back.apply(filmagic_geom::Vec2::new(x * w, y * h));
                (p.x, p.y)
            }
        }
    }
}

/// The space a region is given in: the clip's picture, or (`"space":"frame"`) the programme frame
/// at the clip's first frame or at `atSeconds`.
fn space_p(s: &Session, r: &Value, it: &TrackItem, size: (u32, u32), cmd: &str) -> Result<Space> {
    let (w, h) = (f64::from(size.0), f64::from(size.1));
    match r.get("space") {
        None | Some(Value::Null) => return Ok(Space::Clip(w, h)),
        Some(v) => match v.as_str() {
            Some("clip") => return Ok(Space::Clip(w, h)),
            Some("frame") => {}
            _ => {
                return Err(bad(
                    cmd,
                    "region `space` must be \"clip\" (fractions of the clip's own picture) or \"frame\" (fractions of the programme frame as rendered)",
                ));
            }
        },
    }
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let at = match r.get("atSeconds") {
        None | Some(Value::Null) => it.start,
        Some(v) => {
            let t = v
                .as_f64()
                .filter(|x| x.is_finite() && (0.0..=1e7).contains(x))
                .map(Tick::from_seconds_f64)
                .ok_or_else(|| bad(cmd, "region `atSeconds` must be seconds from 0"))?;
            if t < it.start || t >= it.end() {
                return Err(bad(cmd, format!("region `atSeconds` must lie inside the clip ({:.3} to {:.3} s)", it.start.seconds(), it.end().seconds())));
            }
            t
        }
    };
    let placed = filmagic_render::motion_matrix(q, it, size, filmagic_render::source_par(&s.project, it.item), it.effect_time_at(at));
    let back = placed.inverse().ok_or_else(|| bad(cmd, "the clip is scaled to nothing at that frame: a region of the frame has no place in it"))?;
    Ok(Space::Frame(f64::from(q.settings.width.max(1)), f64::from(q.settings.height.max(1)), back))
}

/// A region of a clip's picture as `masks.add` parameters, in clip pixels.
fn region_p(s: &Session, p: &Value, it: &TrackItem, size: (u32, u32), cmd: &str) -> Result<Value> {
    let (w, h) = (f64::from(size.0), f64::from(size.1));
    let r = p.get("region").filter(|r| r.is_object()).ok_or_else(|| bad(cmd, format!("need `region`: {REGION_DOC}")))?;
    let space = space_p(s, r, it, size, cmd)?;
    // a region read off the frame must fall on the clip: beside it there is nothing to mask
    let on_picture = |pts: &[(f64, f64)]| {
        let (x0, x1) = pts.iter().fold((f64::MAX, f64::MIN), |m, p| (m.0.min(p.0), m.1.max(p.0)));
        let (y0, y1) = pts.iter().fold((f64::MAX, f64::MIN), |m, p| (m.0.min(p.1), m.1.max(p.1)));
        if matches!(space, Space::Frame(..)) && (x1 <= 0.0 || y1 <= 0.0 || x0 >= w || y0 >= h) {
            return Err(bad(cmd, "the region lies beside the clip's picture: at that place in the frame the clip shows nothing"));
        }
        Ok(())
    };
    let polygon = |pts: &[(f64, f64)]| json!({"shape": "bezier", "path": pts.iter().map(|(x, y)| json!([x, y])).collect::<Vec<_>>()});
    if let Some(points) = r.get("points") {
        let pts = points.as_array().filter(|a| (3..=64).contains(&a.len())).ok_or_else(|| bad(cmd, "region `points` must be 3 to 64 points"))?;
        let mut path = Vec::with_capacity(pts.len());
        for pt in pts {
            let xy = pt.as_array().filter(|a| a.len() == 2).ok_or_else(|| bad(cmd, "each region point is [x, y]"))?;
            path.push(space.point(fraction(xy.first(), "points", cmd)?, fraction(xy.get(1), "points", cmd)?));
        }
        on_picture(&path)?;
        return Ok(polygon(&path));
    }
    let shape = match r.get("shape") {
        None | Some(Value::Null) => "rectangle",
        Some(v) => match v.as_str() {
            Some("rectangle" | "rect") => "rectangle",
            Some("ellipse") => "ellipse",
            _ => return Err(bad(cmd, "region `shape` must be \"rectangle\" or \"ellipse\" (or give `points`)")),
        },
    };
    let (x, y, rw, rh) = (fraction(r.get("x"), "x", cmd)?, fraction(r.get("y"), "y", cmd)?, fraction(r.get("w"), "w", cmd)?, fraction(r.get("h"), "h", cmd)?);
    if rw < 0.002 || rh < 0.002 {
        return Err(bad(cmd, "the region is empty: `w` and `h` are fractions greater than 0"));
    }
    // the outline, as seen: four corners, or the ellipse inside them
    let outline: Vec<(f64, f64)> = if shape == "rectangle" {
        vec![(x, y), (x + rw, y), (x + rw, y + rh), (x, y + rh)]
    } else {
        (0..32)
            .map(|i| {
                let a = f64::from(i) * std::f64::consts::TAU / 32.0;
                (x + rw / 2.0 * (1.0 + a.cos()), y + rh / 2.0 * (1.0 + a.sin()))
            })
            .collect()
    };
    let placed: Vec<(f64, f64)> = outline.iter().map(|(px, py)| space.point(*px, *py)).collect();
    on_picture(&placed)?;
    match &space {
        // a clip that is turned or skewed in the frame: the shape is no longer upright in the clip's own picture
        Space::Frame(_, _, back) if back.b.abs() > 1e-9 || back.c.abs() > 1e-9 => Ok(polygon(&placed)),
        _ => {
            let (c, far) = (space.point(x + rw / 2.0, y + rh / 2.0), space.point(x + rw, y + rh));
            let near = space.point(x, y);
            Ok(json!({"shape": shape, "center": [c.0, c.1], "size": [(far.0 - near.0).abs(), (far.1 - near.1).abs()]}))
        }
    }
}

fn px_p(p: &Value, key: &str, default: f64, cmd: &str) -> Result<f64> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v.as_f64().filter(|x| (0.0..=1000.0).contains(x)).ok_or_else(|| bad(cmd, format!("`{key}` must be 0 to 1000 pixels"))),
    }
}

fn flag_p(p: &Value, key: &str, default: bool, cmd: &str) -> Result<bool> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v.as_bool().ok_or_else(|| bad(cmd, format!("`{key}` must be true or false"))),
    }
}

/// The video clip a command works on, and the size of its picture.
fn picture_clip(s: &Session, p: &Value, cmd: &str) -> Result<(TrackItem, (u32, u32))> {
    let clip = clip_p(p, "clip").ok_or_else(|| bad(cmd, "need `clip`: the id of a video clip in the timeline"))?;
    picture_clip_of(s, clip, cmd)
}

/// [`picture_clip`] for a clip id already read from the parameters.
fn picture_clip_of(s: &Session, clip: ClipId, cmd: &str) -> Result<(TrackItem, (u32, u32))> {
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (track, it) = q.find_item(clip).ok_or_else(|| bad(cmd, format!("no clip {} in the active sequence", clip.0)))?;
    let tr = q.track(track).ok_or_else(|| bad(cmd, "the clip's track is gone"))?;
    if tr.kind != TrackKind::Video {
        return Err(bad(cmd, "the clip is a sound clip; a region needs a picture"));
    }
    if tr.locked {
        return Err(bad(cmd, format!("track {} is locked", tr.name)));
    }
    if it.start < Tick::ZERO {
        return Err(bad(cmd, "the clip starts before the beginning of the sequence; move it to 0 or later first"));
    }
    if it.multicam.is_some() {
        return Err(bad(cmd, "a multi-camera clip shows one of several angles; flatten it or work on the angle's own clip"));
    }
    let size = filmagic_render::source_size(&s.project, it.item).filter(|(w, h)| *w > 0 && *h > 0).ok_or_else(|| bad(cmd, "the clip has no picture"))?;
    Ok((it.clone(), size))
}

/// Most frames a mask is tracked through inside one command (two minutes at 25 fps): tracking
/// holds the session until it is done.
const MAX_FOLLOW_FRAMES: i64 = 3000;

/// Refuse `follow` on a clip too long to track inside one command.
fn follow_fits(s: &Session, it: &TrackItem, cmd: &str) -> Result<()> {
    let frames = it.duration.0 / s.sequence_rate().frame_duration().0.max(1);
    if frames > MAX_FOLLOW_FRAMES {
        return Err(bad(
            cmd,
            format!(
                "`follow` tracks every frame before it returns, and this clip has {frames} (the limit is {MAX_FOLLOW_FRAMES}). Cut the clip down to the stretch that matters, or add the mask without `follow` and run `masks.track` in the background"
            ),
        ));
    }
    Ok(())
}

/// What a clip's edges do that a layer made from it will not: transitions belong to the clip they
/// were put on.
fn edge_warnings(s: &Session, it: &TrackItem, what: &str) -> Vec<String> {
    let Some(q) = s.active_sequence() else { return Vec::new() };
    let on_clip = q.video_tracks.iter().flat_map(|t| t.transitions.iter()).filter(|x| x.from == Some(it.id) || x.to == Some(it.id)).count();
    if on_clip == 0 {
        return Vec::new();
    }
    vec![format!(
        "the clip has {on_clip} transition(s) at its edges; {what} does not fade with them and will show at full strength through the transition. Trim it clear of the transition or add a matching one"
    )]
}

/// Whether `x` was made from `base` and lies over it: a layer (an unlinked copy of the same
/// footage within the clip's span, masked down to a region) or a plate (a still over exactly the
/// clip's span). A second clip of the same footage that is not masked is an edit of its own, a
/// punch-in or a cutaway back to the same take, and is not part of the stack.
pub(crate) fn of_its_stack(p: &filmagic_project::Project, base: &TrackItem, x: &TrackItem) -> bool {
    let within = x.start >= base.start && x.end() <= base.end();
    let still =
        p.item(x.item).is_some_and(|i| matches!(&i.kind, filmagic_project::ItemKind::Media(m) if matches!(m.info.kind, filmagic_media::MediaKind::Still)));
    let masked = x.effects.iter().any(|e| e.effect == "opacity" && e.masks.iter().any(|m| m.mode != filmagic_project::MaskMode::None));
    let layer = x.item == base.item && x.link.is_none() && masked;
    x.id != base.id && within && (layer || (still && x.start == base.start && x.end() == base.end()))
}

/// Whether `path` is a file some media of the project is read from.
pub(crate) fn is_project_media(s: &Session, path: &str) -> bool {
    s.project
        .items
        .values()
        .any(|i| matches!(&i.kind, filmagic_project::ItemKind::Media(m) if matches!(&m.media, filmagic_project::MediaRef::File { path: used } if used == path)))
}

/// A new video track for something made from `clip`: above the clip and above the layers and
/// plates already made from it, and under everything else that lies over it (a cutaway, a title),
/// so that what is put there covers the clip and nothing that was laid over it on purpose.
fn track_above(s: &mut Session, clip: ClipId, cmd: &str) -> Result<u64> {
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (index, base) = q
        .video_tracks
        .iter()
        .enumerate()
        .find_map(|(i, t)| t.items.iter().find(|x| x.id == clip).map(|x| (i, x)))
        .ok_or_else(|| bad(cmd, format!("no clip {} on a video track", clip.0)))?;
    // the tracks straight above that hold only parts of this clip's own stack over its span
    let stack = q.video_tracks.iter().skip(index + 1).take_while(|t| {
        let mut there = t.items.iter().filter(|x| x.start < base.end() && x.end() > base.start).peekable();
        there.peek().is_some() && there.all(|x| of_its_stack(&s.project, base, x))
    });
    let top = index + stack.count();
    let added = run(s, "sequence.addTracks", json!({"video": 1, "audio": 0, "videoAfter": top + 1}))?;
    added["video"].get(0).and_then(Value::as_u64).ok_or_else(|| bad(cmd, "a track could not be added above the clip"))
}

/// What a region mask is for.
#[derive(Clone, Copy, PartialEq)]
enum MaskUse {
    /// Keep only the region (of whatever the masks already there keep).
    Keep,
    /// Take the region out.
    Hole,
}

/// Add a mask of `shape` (as [`region_p`] gives it) to `effect` of `clip`. Returns the mask index.
///
/// Masks on one effect combine in order, and a plain new mask is added to the ones before it: a
/// second cut-out would fill the first, and a region on a clip that is already masked would widen
/// it. So a hole is a Subtract mask, and a region kept on an effect that has masks already is an
/// Intersect mask.
fn add_mask(s: &mut Session, clip: ClipId, effect: Value, shape: &Value, feather: f64, usage: MaskUse, extra: Value) -> Result<u64> {
    let masked_already = s
        .active_sequence()
        .and_then(|q| q.find_item(clip))
        .and_then(|(_, it)| match &effect {
            Value::Number(n) => it.effects.get(n.as_u64().unwrap_or(0) as usize),
            Value::String(id) => it.effects.iter().find(|e| &e.effect == id),
            _ => None,
        })
        .is_some_and(|e| e.masks.iter().any(|m| m.mode != filmagic_project::MaskMode::None));
    let mode = match (usage, masked_already) {
        (MaskUse::Hole, _) => "subtract",
        (MaskUse::Keep, true) => "intersect",
        (MaskUse::Keep, false) => "add",
    };
    let mut add = shape.clone();
    add["clip"] = json!(clip.0);
    add["effect"] = effect.clone();
    let mask = run(s, "masks.add", add)?["mask"].as_u64().ok_or_else(|| EngineError::Other("the mask was not created".into()))?;
    let mut set = json!({"clip": clip.0, "effect": effect, "mask": mask, "feather": feather, "mode": mode});
    if let (Some(set), Some(extra)) = (set.as_object_mut(), extra.as_object()) {
        set.extend(extra.clone());
    }
    run(s, "masks.set", set)?;
    Ok(mask)
}

/// Track mask `mask` of `effect` on `clip` from the clip's first frame to its last, now.
fn follow(s: &mut Session, it: &TrackItem, clip: ClipId, effect: Value, mask: u64) -> Result<Value> {
    // from the clip's first frame on the timeline to its last: in a reversed clip that runs backward through the media
    let direction = if it.reverse { "backward" } else { "forward" };
    let r = run(s, "masks.track", json!({"clip": clip.0, "effect": effect, "mask": mask, "time": it.start.0, "direction": direction, "wait": true}))?;
    let st = &r["status"];
    Ok(json!({"frames": r["frames"], "tracked": st["done"], "status": st["status"], "error": st["result"]["error"]}))
}

/// Copy the clip onto a new video track above it and above what was already made from it
/// ([`track_above`]), unlinked from its sound. Returns the copy.
fn copy_above(s: &mut Session, it: &TrackItem, cmd: &str) -> Result<ClipId> {
    let track = track_above(s, it.id, cmd)?;
    let r = run(s, "timeline.move", json!({"moves": [{"clip": it.id.0, "track": track, "time": it.start.0}], "copy": true, "linked": false}))?;
    r["copied"].get(0).and_then(Value::as_u64).map(ClipId).ok_or_else(|| bad(cmd, "the clip could not be copied onto a new track"))
}

fn track_name(s: &Session, clip: ClipId) -> Value {
    s.active_sequence().and_then(|q| q.find_item(clip).and_then(|(t, _)| q.track(t))).map_or(Value::Null, |t| json!(t.name))
}

/// Give `clip` a name (the layer's label in the timeline).
fn rename(s: &mut Session, clip: ClipId, name: String) -> Result<()> {
    s.edit_sequence("Name Layer", |q, _, _| {
        if let Some((_, it)) = q.find_item_mut(clip) {
            it.name = name;
        }
        Ok(())
    })
}

fn name_p(p: &Value, cmd: &str) -> Result<Option<String>> {
    match p.get("name") {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_str()
            .map(str::trim)
            .filter(|x| !x.is_empty() && x.len() <= 255 && !x.chars().any(char::is_control))
            .map(|x| Some(x.to_string()))
            .ok_or_else(|| bad(cmd, "`name` must be 1 to 255 bytes of text")),
    }
}

/// What `extract_layer` made.
struct Layer {
    clip: ClipId,
    mask: u64,
    tracking: Option<Value>,
}

/// The steps of `layers.extract`, for use inside a larger edit.
fn extract_layer(s: &mut Session, it: &TrackItem, shape: &Value, feather: f64, do_follow: bool, cmd: &str) -> Result<Layer> {
    let clip = copy_above(s, it, cmd)?;
    let mask = add_mask(s, clip, json!("opacity"), shape, feather, MaskUse::Keep, json!({}))?;
    let tracking = if do_follow { Some(follow(s, it, clip, json!("opacity"), mask)?) } else { None };
    Ok(Layer { clip, mask, tracking })
}

fn extract(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "layers.extract";
    let (it, size) = picture_clip(s, p, CMD)?;
    let shape = region_p(s, p, &it, size, CMD)?;
    let feather = px_p(p, "feather", 12.0, CMD)?;
    let (do_follow, cut_out) = (flag_p(p, "follow", false, CMD)?, flag_p(p, "cutOut", false, CMD)?);
    let name = name_p(p, CMD)?;
    if do_follow {
        follow_fits(s, &it, CMD)?;
    }
    let warnings = edge_warnings(s, &it, "the layer");
    let before = crate::story::looks_across(s, it.start, it.end());
    s.edit_group("Extract Layer", |s| {
        let layer = extract_layer(s, &it, &shape, feather, do_follow, CMD)?;
        if let Some(n) = name {
            rename(s, layer.clip, n)?;
        }
        let mut out =
            json!({"layer": layer.clip.0, "track": track_name(s, layer.clip), "mask": layer.mask, "of": it.id.0, "tracking": layer.tracking, "warnings": warnings});
        if cut_out {
            // the same shape taken out of the original; when the layer's mask follows the subject the hole follows too
            let hole = add_mask(s, it.id, json!("opacity"), &shape, feather, MaskUse::Hole, json!({}))?;
            if do_follow {
                copy_mask_path(s, layer.clip, layer.mask, it.id, hole)?;
            }
            out["cutOut"] = json!({"clip": it.id.0, "mask": hole});
        }
        // the layer is what the next command most likely works on
        s.state.selection = vec![layer.clip];
        // a layer shows what was already there: until it is moved or changed this reads `none`
        out["pictureChange"] = crate::story::change_json(&before, &crate::story::looks_across(s, it.start, it.end()));
        Ok(out)
    })
}

/// Give the opacity mask `to_mask` of clip `to` the (tracked) path of `from_mask` of clip `from`.
fn copy_mask_path(s: &mut Session, from: ClipId, from_mask: u64, to: ClipId, to_mask: u64) -> Result<()> {
    s.edit_sequence("Follow Layer", |q, _, _| {
        let path = q
            .find_item(from)
            .and_then(|(_, i)| i.effects.iter().find(|e| e.effect == "opacity"))
            .and_then(|e| e.masks.get(from_mask as usize))
            .map(|m| m.path.clone());
        let target =
            q.find_item_mut(to).and_then(|(_, i)| i.effects.iter_mut().find(|e| e.effect == "opacity")).and_then(|e| e.masks.get_mut(to_mask as usize));
        if let (Some(path), Some(m)) = (path, target) {
            m.path = path;
        }
        Ok(())
    })
}

/// What `make_plate` made.
struct Plate {
    clip: Option<ClipId>,
    /// The mask that keeps the plate to its region, when it has one.
    mask: Option<u64>,
    report: Value,
}

fn samples_p(p: &Value, cmd: &str) -> Result<usize> {
    match p.get("samples") {
        None | Some(Value::Null) => Ok(15),
        Some(_) => u64_p(p, "samples")
            .filter(|n| (3..=filmagic_render::plate::MAX_PLATE_SAMPLES as u64).contains(n))
            .map(|n| n as usize)
            .ok_or_else(|| bad(cmd, format!("`samples` must be 3 to {}", filmagic_render::plate::MAX_PLATE_SAMPLES))),
    }
}

fn path_p(p: &Value, cmd: &str) -> Result<String> {
    let path = str_p(p, "path")
        .filter(|x| !x.trim().is_empty())
        .map(crate::export_tools::expand_home)
        .ok_or_else(|| bad(cmd, "need `path`: where to write the plate (a .png)"))?;
    if !path.to_ascii_lowercase().ends_with(".png") {
        return Err(bad(cmd, "`path` must end in .png"));
    }
    Ok(path)
}

/// The median of `samples` frames across the part of its media the clip shows: (RGBA8, width,
/// height, how well the frames agreed, frames used).
fn median_plate(s: &Session, it: &TrackItem, samples: usize, cmd: &str) -> Result<(Vec<u8>, usize, usize, filmagic_render::plate::Stillness, usize)> {
    let provider = s.media.full_res_provider(s.project.clone(), s.services.clone());
    let mut frames: Vec<Vec<u8>> = Vec::new();
    let mut dims: Option<(usize, usize)> = None;
    let mut n = samples;
    let mut k = 0;
    while k < n {
        // the middle of each of `n` equal parts of the clip
        let t = it.start + Tick(it.duration.0 / (n as i64 * 2) * (2 * k as i64 + 1));
        let img = filmagic_render::render_item(&s.project, it.item, it.source_time_at(t), 1.0, &provider)
            .ok_or_else(|| bad(cmd, "a frame of the clip could not be decoded"))?;
        if img.w == 0 || img.h == 0 {
            return Err(bad(cmd, "the clip has no picture"));
        }
        match dims {
            None => {
                dims = Some((img.w, img.h));
                // as many frames as fit the memory budget, never fewer than three
                let per = img.w.saturating_mul(img.h).saturating_mul(4).max(1);
                n = n.min(PLATE_BYTES / per);
                if n < 3 {
                    return Err(bad(cmd, format!("a {}×{} frame is too large to make a plate from", img.w, img.h)));
                }
                if n < samples {
                    // the sample times change with the count: start over with the smaller one
                    frames.clear();
                    k = 0;
                    dims = Some((img.w, img.h));
                    continue;
                }
            }
            Some(d) if d != (img.w, img.h) => return Err(bad(cmd, "the clip's frames change size; a plate needs them all alike")),
            Some(_) => {}
        }
        frames.push(img.over_black_rgba8());
        k += 1;
    }
    let (w, h) = dims.ok_or_else(|| bad(cmd, "the clip has no picture"))?;
    let (plate, still) = filmagic_render::plate::temporal_median(&frames, w, h).ok_or_else(|| bad(cmd, "the plate could not be made from these frames"))?;
    Ok((plate, w, h, still, frames.len()))
}

/// The steps of `layers.cleanPlate`: make the plate, write and import it, and (with `place`) lay it
/// on a new track over the clip, masked to `shape` when one is given.
fn make_plate(s: &mut Session, it: &TrackItem, path: &str, samples: usize, cover: Option<(&Value, f64)>, place: bool, cmd: &str) -> Result<Plate> {
    // a path a project item already reads would change that item under its clips, and undo could not put it back
    if is_project_media(s, path) {
        return Err(bad(cmd, format!("{path} is already media of this project; write the plate to another file")));
    }
    let (plate, w, h, still, used) = median_plate(s, it, samples, cmd)?;
    let (w32, h32) = (u32::try_from(w).map_err(|_| bad(cmd, "the frame is too wide"))?, u32::try_from(h).map_err(|_| bad(cmd, "the frame is too tall"))?);
    let bytes = filmagic_export::encode_png(plate, w32, h32).map_err(|e| EngineError::Other(e.to_string()))?;
    s.services.write_file(path, &bytes).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    // one still, whatever numbered files lie beside it
    let imported = run(s, "file.import", json!({"paths": [path], "imageSequence": false}))?;
    let item = imported["items"]
        .get(0)
        .and_then(Value::as_u64)
        .map(ItemId)
        .ok_or_else(|| EngineError::Other(format!("{path}: the plate was written but could not be imported: {}", imported["errors"])))?;
    let mut warnings: Vec<String> = Vec::new();
    if still.settled < 0.9 {
        warnings.push(format!(
            "only {:.0}% of the picture holds still across the clip: the camera moves or the light changes, and the plate will smear. A clean plate needs a locked-off shot",
            still.settled * 100.0
        ));
    }
    if still.disturbed < 0.002 {
        warnings.push("nothing moves in this clip: the plate is the clip's own frame, with whatever stands in it".into());
    }
    if s.active_sequence().is_some_and(|q| q.settings.color.working != filmagic_color::WorkingSpace::Rec709) {
        warnings.push("the plate is made in Rec. 709; in this HDR sequence its colour will not match the clip exactly".into());
    }
    // pixels that are not square: the still is given the clip's pixel shape, or it would sit narrower or wider than the clip
    if let Some((num, den)) = filmagic_render::source_par(&s.project, it.item).filter(|(n, d)| n != d && *n > 0 && *d > 0) {
        run(s, "clip.interpretFootage", json!({"items": [item.0], "pixelAspect": [num, den]}))?;
    }
    let mut clip = None;
    let mut patch_mask = None;
    if place {
        let track = track_above(s, it.id, cmd)?;
        let placed = run(s, "timeline.place", json!({"item": item.0, "track": track, "time": it.start.0, "duration": it.duration.0, "audio": false}))?;
        let id = placed["clips"].get(0).and_then(Value::as_u64).map(ClipId).ok_or_else(|| EngineError::Other("the plate could not be placed".into()))?;
        // The plate stands in for the clip's picture: it takes everything that shapes that picture.
        // Its effects (Motion, Opacity, the grade, a flip, masks) and the clock they are keyed to: a
        // keyframe sits at a media time, so the still plays "from" the same media time at the same
        // speed and direction, or a tracked mask and an animated Motion would run out of step.
        let model = it.clone();
        s.edit_sequence("Align Plate", |q, _, _| {
            if let Some((_, pl)) = q.find_item_mut(id) {
                pl.name = format!("{} (plate)", model.name);
                pl.scale_to_frame = model.scale_to_frame;
                pl.source_in = model.source_in;
                pl.speed = model.speed;
                pl.reverse = model.reverse;
                pl.frame_hold = model.frame_hold;
                pl.hold_filters = model.hold_filters;
                pl.effects = model.effects.clone();
            }
            Ok(())
        })?;
        if let Some((shape, feather)) = cover {
            // a little wider than the region, so the feathered edge of what it hides is covered too
            patch_mask = Some(add_mask(s, id, json!("opacity"), shape, feather, MaskUse::Keep, json!({"expansion": feather}))?);
        }
        warnings.extend(edge_warnings(s, it, "the plate"));
        clip = Some(id);
    }
    let report = json!({
        "item": item.0,
        "clip": clip.map(|c| c.0),
        "track": clip.map_or(Value::Null, |c| track_name(s, c)),
        "path": path,
        "width": w,
        "height": h,
        "samples": used,
        "stillness": {"settled": (f64::from(still.settled) * 1000.0).round() / 1000.0, "disturbed": (f64::from(still.disturbed) * 1000.0).round() / 1000.0},
        "warnings": warnings,
    });
    Ok(Plate { clip, mask: patch_mask, report })
}

fn clean_plate(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "layers.cleanPlate";
    let (it, size) = picture_clip(s, p, CMD)?;
    let path = path_p(p, CMD)?;
    let samples = samples_p(p, CMD)?;
    let place = flag_p(p, "place", true, CMD)?;
    let feather = px_p(p, "feather", 24.0, CMD)?;
    let shape = match p.get("region") {
        None | Some(Value::Null) => None,
        Some(_) => Some(region_p(s, p, &it, size, CMD)?),
    };
    if shape.is_some() && !place {
        return Err(bad(CMD, "a `region` masks the placed plate: it needs `place` true"));
    }
    let before = crate::story::looks_across(s, it.start, it.end());
    s.edit_group("Clean Plate", |s| {
        let plate = make_plate(s, &it, &path, samples, shape.as_ref().map(|sh| (sh, feather)), place, CMD)?;
        if let Some(c) = plate.clip {
            s.state.selection = vec![c];
        }
        let mut report = plate.report;
        if place {
            // how much of the picture the plate changed: nothing, when there was nothing to take out
            report["pictureChange"] = crate::story::change_json(&before, &crate::story::looks_across(s, it.start, it.end()));
        }
        Ok(report)
    })
}

fn lift(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "layers.lift";
    let (it, size) = picture_clip(s, p, CMD)?;
    let shape = region_p(s, p, &it, size, CMD)?;
    let path = path_p(p, CMD)?;
    let samples = samples_p(p, CMD)?;
    let feather = px_p(p, "feather", 12.0, CMD)?;
    let do_follow = flag_p(p, "follow", false, CMD)?;
    if do_follow {
        follow_fits(s, &it, CMD)?;
    }
    let before = crate::story::looks_across(s, it.start, it.end());
    s.edit_group("Lift Layer", |s| {
        // bottom to top, each directly above the last: the clip, the plate over the place the subject stood, the subject
        let plate = make_plate(s, &it, &path, samples, Some((&shape, feather)), true, CMD)?;
        let layer = extract_layer(s, &it, &shape, feather, do_follow, CMD)?;
        if let (true, Some(patch), Some(patch_mask)) = (do_follow, plate.clip, plate.mask) {
            // the patch covers wherever the subject goes
            copy_mask_path(s, layer.clip, layer.mask, patch, patch_mask)?;
        }
        s.state.selection = vec![layer.clip];
        Ok(json!({
            "layer": layer.clip.0,
            "track": track_name(s, layer.clip),
            "mask": layer.mask,
            "of": it.id.0,
            "tracking": layer.tracking,
            "warnings": plate.report["warnings"].clone(),
            // the subject is back where it stood, over its plate: this reads `none` until the layer is moved
            "pictureChange": crate::story::change_json(&before, &crate::story::looks_across(s, it.start, it.end())),
            "plate": plate.report,
            "note": "scale or shift the layer where it stands (`layers.transform`), move or re-time it (its Motion, `clip.speedDuration`), or switch it off (`clip.enable`) to take the subject out; the plate hides the original underneath",
        }))
    })
}

fn retime(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "layers.retime";
    let (it, size) = picture_clip(s, p, CMD)?;
    let shape = region_p(s, p, &it, size, CMD)?;
    let feather = px_p(p, "feather", 12.0, CMD)?;
    let speed = p
        .get("speed")
        .and_then(Value::as_f64)
        .filter(|x| (1.0..=10_000.0).contains(x))
        .ok_or_else(|| bad(CMD, "need `speed`: per cent of normal speed, 1 to 10000 (50 = half speed)"))?;
    let interpolation = match p.get("interpolation") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_str()
                .filter(|m| filmagic_project::TimeInterpolation::from_name(m).is_some())
                .ok_or_else(|| bad(CMD, "`interpolation` must be frameSampling, frameBlending or opticalFlow"))?
                .to_string(),
        ),
    };
    let warnings = edge_warnings(s, &it, "the layer");
    let before = crate::story::looks_across(s, it.start, it.end());
    s.edit_group("Retime Region", |s| {
        let layer = extract_layer(s, &it, &shape, feather, false, CMD)?;
        // the layer keeps the shot's direction: Speed/Duration plays forward unless told otherwise
        let mut sp = json!({"clips": [layer.clip.0], "speed": speed, "reverse": it.reverse, "ripple": false});
        if let Some(m) = &interpolation {
            sp["interpolation"] = json!(m);
        }
        run(s, "clip.speedDuration", sp)?;
        // slower, the layer would run on past the shot: it ends where the shot ends
        let end = s.active_sequence().and_then(|q| q.find_item(layer.clip)).map(|(_, l)| l.end()).unwrap_or(it.end());
        if end > it.end() {
            run(s, "timeline.trim", json!({"clip": layer.clip.0, "edge": "out", "mode": "regular", "delta": (it.end() - end).0, "linked": false}))?;
        }
        let covers = s.active_sequence().and_then(|q| q.find_item(layer.clip)).map(|(_, l)| l.duration).unwrap_or_default();
        s.state.selection = vec![layer.clip];
        Ok(json!({
            "layer": layer.clip.0,
            "track": track_name(s, layer.clip),
            "mask": layer.mask,
            "of": it.id.0,
            "speed": speed,
            "seconds": (covers.seconds() * 1000.0).round() / 1000.0,
            "untilShotEnd": covers >= it.duration,
            "warnings": warnings,
            "pictureChange": crate::story::change_json(&before, &crate::story::looks_across(s, it.start, it.end())),
            "note": "inside the region the shot plays at the new speed, outside at its own. It reads as one picture while the subject stays inside the region and the camera holds still; for a subject that leaves it, `layers.lift` first and re-time the lifted layer",
        }))
    })
}

fn adjust(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "layers.adjust";
    let (it, size) = picture_clip(s, p, CMD)?;
    let shape = region_p(s, p, &it, size, CMD)?;
    let feather = px_p(p, "feather", 12.0, CMD)?;
    let do_follow = flag_p(p, "follow", false, CMD)?;
    let effect =
        str_p(p, "effect").filter(|e| !e.trim().is_empty()).ok_or_else(|| bad(CMD, "need `effect`: an effect id or name (`effects.list`)"))?.to_string();
    let params: Vec<(String, Value)> = match p.get("params") {
        None | Some(Value::Null) => Vec::new(),
        Some(v) => v
            .as_object()
            .filter(|o| o.len() <= 200)
            .map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .ok_or_else(|| bad(CMD, "`params` must be an object of parameter ids and values"))?,
    };
    if do_follow {
        follow_fits(s, &it, CMD)?;
    }
    let before = crate::story::looks_across(s, it.start, it.end());
    s.edit_group("Adjust Region", |s| {
        let applied = run(s, "effects.apply", json!({"clips": [it.id.0], "effect": effect}))?;
        let index =
            applied["applied"].get(0).and_then(|a| a["index"].as_u64()).ok_or_else(|| bad(CMD, format!("`{effect}` is not an effect for a picture clip")))?;
        // a sound effect lands on the linked sound clip instead: that is not a region of this picture
        if applied["applied"][0]["clip"].as_u64() != Some(it.id.0) {
            return Err(bad(CMD, format!("`{effect}` is not an effect for a picture clip")));
        }
        let id = applied["applied"][0]["effect"].clone();
        for (param, value) in &params {
            run(s, "effects.setParam", json!({"clip": it.id.0, "effect": index, "param": param, "value": value, "time": it.start.0}))?;
        }
        let mask = add_mask(s, it.id, json!(index), &shape, feather, MaskUse::Keep, json!({}))?;
        let tracking = if do_follow { Some(follow(s, &it, it.id, json!(index), mask)?) } else { None };
        // what it did to the picture: an adjustment nobody can see is worth knowing about
        let after = crate::story::looks_across(s, it.start, it.end());
        Ok(json!({"clip": it.id.0, "effect": id, "index": index, "mask": mask, "tracking": tracking, "warnings": [], "pictureChange": crate::story::change_json(&before, &after)}))
    })
}

/// Furthest `layers.transform` shifts a layer in one call, in sequence pixels.
const MAX_OFFSET: f64 = 100_000.0;

/// How close (sequence pixels) a layer's edge may come to the place its subject stood and still
/// count as covering it.
const COVER_SLACK: f64 = 0.5;

/// The point of a mask's bounds that `layers.transform` holds still, in fractions of the bounds
/// from their top-left corner. The default is the bottom centre: where a standing subject meets
/// the ground.
fn anchor_p(p: &Value, cmd: &str) -> Result<(f64, f64)> {
    const DOC: &str = "`anchor` must be \"bottom\", \"center\" or \"top\", or [fx, fy]: fractions (0 to 1) of the mask's bounds from their top-left corner";
    match p.get("anchor") {
        None | Some(Value::Null) => Ok((0.5, 1.0)),
        Some(Value::String(a)) => match a.as_str() {
            "bottom" => Ok((0.5, 1.0)),
            "center" | "centre" => Ok((0.5, 0.5)),
            "top" => Ok((0.5, 0.0)),
            _ => Err(bad(cmd, DOC)),
        },
        Some(v) => {
            let a = v.as_array().filter(|a| a.len() == 2).ok_or_else(|| bad(cmd, DOC))?;
            let f = |i: usize| a.get(i).and_then(Value::as_f64).filter(|x| (0.0..=1.0).contains(x)).ok_or_else(|| bad(cmd, DOC));
            Ok((f(0)?, f(1)?))
        }
    }
}

fn offset_p(p: &Value, cmd: &str) -> Result<Option<Vec2>> {
    let wrong = || bad(cmd, format!("`offset` must be [dx, dy] in sequence pixels, each within ±{MAX_OFFSET}"));
    match p.get("offset") {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            let a = v.as_array().filter(|a| a.len() == 2).ok_or_else(wrong)?;
            let d = |i: usize| a.get(i).and_then(Value::as_f64).filter(|x| (-MAX_OFFSET..=MAX_OFFSET).contains(x)).ok_or_else(wrong);
            Ok(Some(Vec2::new(d(0)?, d(1)?)))
        }
    }
}

fn scale_p(p: &Value, cmd: &str) -> Result<Option<f64>> {
    match p.get("scale") {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_f64().filter(|x| (0.1..=10_000.0).contains(x)).map(Some).ok_or_else(|| {
            bad(cmd, "`scale` must be per cent, 0.1 to 10000: the layer's Motion Scale (150 = half as large again as the clip shows it at 100)")
        }),
    }
}

/// The bounds of a path's outline (its curves, not their handles): (top-left, bottom-right).
/// `None` for a path with no points or with points that are not numbers.
fn outline_bounds(path: &MaskPath) -> Option<(Vec2, Vec2)> {
    let pts = path.flatten(0.25);
    let lo = pts.iter().fold(Vec2::new(f64::INFINITY, f64::INFINITY), |m, p| Vec2::new(m.x.min(p.x), m.y.min(p.y)));
    let hi = pts.iter().fold(Vec2::new(f64::NEG_INFINITY, f64::NEG_INFINITY), |m, p| Vec2::new(m.x.max(p.x), m.y.max(p.y)));
    // a point that is not a number is passed over by `min` and `max`: it must not go unnoticed
    let numbers = pts.iter().all(|p| p.x.is_finite() && p.y.is_finite());
    (numbers && lo.x.is_finite() && lo.y.is_finite() && hi.x.is_finite() && hi.y.is_finite()).then_some((lo, hi))
}

/// The point of a mask path that `layers.transform` holds still while the layer is placed by
/// `placed`: (in the clip's own pixels, in the frame).
///
/// The bounds are the ones a viewer sees, so the bottom of a layer that is turned in the frame is
/// its bottom in the frame.
fn fixed_point(path: &MaskPath, placed: &Affine, anchor: (f64, f64)) -> Option<(Vec2, Vec2)> {
    let at = |(lo, hi): (Vec2, Vec2)| Vec2::new(lo.x + (hi.x - lo.x) * anchor.0, lo.y + (hi.y - lo.y) * anchor.1);
    let (clip, frame) = match placed.inverse() {
        Some(back) => {
            let frame = at(outline_bounds(&path.transformed(placed))?);
            (back.apply(frame), frame)
        }
        // scaled to nothing, there is nothing to see: the mask's bounds in the clip's own picture
        None => {
            let clip = at(outline_bounds(path)?);
            (clip, placed.apply(clip))
        }
    };
    (clip.x.is_finite() && clip.y.is_finite() && frame.x.is_finite() && frame.y.is_finite()).then_some((clip, frame))
}

/// Whether a project item is a still image.
fn is_still(p: &filmagic_project::Project, item: ItemId) -> bool {
    p.item(item).is_some_and(|i| matches!(&i.kind, filmagic_project::ItemKind::Media(m) if matches!(m.info.kind, filmagic_media::MediaKind::Still)))
}

/// Most frames `layers.transform` works a layer's place out on one by one (eleven minutes at
/// 30 fps): each is a keyframe in the project.
const MAX_PLACED_FRAMES: i64 = 20_000;

fn transform(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "layers.transform";
    // `clip` names the layer too, as it does for every other command, but never in place of a `layer` that is not an id
    let id = match p.get("layer") {
        None | Some(Value::Null) => clip_p(p, "clip"),
        Some(_) => clip_p(p, "layer"),
    }
    .ok_or_else(|| bad(CMD, "need `layer`: the id of a clip with an opacity mask (`layers.extract` and `layers.lift` return one as `layer`)"))?;
    let (it, size) = picture_clip_of(s, id, CMD)?;
    if s.project.item(it.item).is_some_and(|i| matches!(i.kind, filmagic_project::ItemKind::AdjustmentLayer { .. })) {
        return Err(bad(CMD, "an adjustment layer's masks are drawn on the frame and its Motion does not move them: it has no subject to hold in place"));
    }
    let (scale, offset, anchor) = (scale_p(p, CMD)?, offset_p(p, CMD)?, anchor_p(p, CMD)?);
    if scale.is_none() && offset.is_none() {
        return Err(bad(CMD, "need `scale` (per cent), `offset` ([dx, dy] in sequence pixels) or both"));
    }
    let shift = offset.unwrap_or(Vec2::ZERO);
    let masks = it.effect("opacity").map_or(&[][..], |e| e.masks.as_slice());
    let mask_i = match p.get("mask") {
        // the mask `layers.extract` adds is the last one that keeps a region; a hole cut into the layer later comes after it
        None | Some(Value::Null) => masks.iter().rposition(|m| !m.inverted && !matches!(m.mode, MaskMode::None | MaskMode::Subtract)).ok_or_else(|| {
            bad(CMD, "the clip has no opacity mask that keeps a region, so there is no subject to hold in place: `layers.extract` or `layers.lift` makes a layer (or name a mask with `mask`)")
        })?,
        Some(v) => v
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n < masks.len())
            .ok_or_else(|| bad(CMD, format!("`mask` must be the index of one of the clip's opacity masks (it has {})", masks.len())))?,
    };
    let mask = masks.get(mask_i).ok_or_else(|| bad(CMD, "the mask is gone"))?;
    let motion = it.effect("motion").ok_or_else(|| bad(CMD, "the clip has no Motion to set"))?;
    if !motion.enabled {
        return Err(bad(CMD, "the clip's Motion is switched off, so nothing set on it would show; switch it on first (`effects.toggleEnabled`)"));
    }
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let par = filmagic_render::source_par(&s.project, it.item);
    let place = |item: &TrackItem, t: Tick| filmagic_render::motion_matrix(q, item, size, par, t);
    let first = it.effect_time_at(it.start);
    let animated = |id: &str| motion.param(id).is_some_and(Param::is_animated);
    let uniform = motion.param("uniform_scale").and_then(|p| p.value.as_bool()).unwrap_or(true);
    let was = motion.f64_at("scale", first);
    if scale.is_some() && (animated("scale") || (!uniform && animated("scale_width"))) {
        return Err(bad(
            CMD,
            "the layer's Scale is keyframed, and `scale` sets one size for the whole layer. Take the keyframes off first (`effects.toggleAnimation`), or give only `offset`",
        ));
    }
    // a scale it already has leaves only the shift to do
    let rescale = scale.filter(|sc| (sc - was).abs() > 1e-9);

    // the layer as it will be
    let mut after = it.clone();
    let centre = Vec2::new(f64::from(q.settings.width) / 2.0, f64::from(q.settings.height) / 2.0);
    let unplaced = || bad(CMD, "the layer's place in the frame cannot be worked out: its Motion or its mask path holds a value that is not a number");
    let position = match rescale {
        // the same size: every position moves by the offset, and its keyframes keep their easing
        None => {
            let mut pos = motion.param("position").cloned().unwrap_or_else(|| Param::new(ParamValue::Vec2(centre)));
            pos.map_values(|v| match v {
                // a position never set is the middle of the frame
                ParamValue::Vec2(v) => ParamValue::Vec2(if v.x.is_nan() { centre } else { v } + shift),
                other => other,
            });
            pos
        }
        Some(sc) => {
            let Some(m) = after.effect_mut("motion") else { return Err(bad(CMD, "the clip has no Motion to set")) };
            m.params.insert("scale".into(), Param::new(ParamValue::Float(sc)));
            if !uniform {
                // the width keeps its proportion to the height
                let width = Some(m.f64_at("scale_width", first) * sc / was).filter(|w| w.is_finite() && was > 0.0).map_or(sc, |w| w.clamp(0.0, 10_000.0));
                m.params.insert("scale_width".into(), Param::new(ParamValue::Float(width)));
            }
            // With the position at the origin, what is left of the placement says where a point of
            // the scaled picture falls around it: the position is the rest of the way to where the
            // anchor has to be.
            m.params.insert("position".into(), Param::new(ParamValue::Vec2(Vec2::ZERO)));
            let key = |t: Tick| -> Result<ParamValue> {
                let (in_clip, in_frame) = fixed_point(&mask.path_at(t), &place(&it, t), anchor).ok_or_else(unplaced)?;
                let pos = in_frame + shift - place(&after, t).apply(in_clip);
                if pos.x.is_finite() && pos.y.is_finite() { Ok(ParamValue::Vec2(pos)) } else { Err(unplaced()) }
            };
            let path_keys = mask.path.keyframes.as_slice();
            // How the position moves away from each keyframe of the path when only the path moves:
            // with the path's easing, and not at all where the path has another number of points
            // at its next keyframe (it does not move toward that one, it jumps).
            let rides = |i: usize, k: &Keyframe| {
                let points = |k: &Keyframe| k.value.as_path().map(MaskPath::len);
                let held = path_keys.get(i + 1).is_some_and(|next| points(next) != points(k));
                (if held { filmagic_project::Interpolation::Hold } else { k.interp }, k.out_influence, k.in_influence)
            };
            // A position already keyed that way goes with the path (this command leaves it so):
            // scaled again, it still does. The Scale is one value here, a keyframed one was refused.
            let goes_with_path = motion.param("position").is_none_or(|pos| {
                pos.keyframes.is_empty()
                    || (pos.keyframes.len() == path_keys.len()
                        && pos
                            .keyframes
                            .iter()
                            .zip(path_keys.iter().enumerate())
                            .all(|(at, (i, k))| at.time == k.time && (at.interp, at.out_influence, at.in_influence) == rides(i, k)))
            });
            let keys = if animated("anchor") || animated("rotation") || !goes_with_path {
                // The layer's own Motion is keyframed. Where the anchor falls then turns on two
                // animations at once, each with its own keyframes and easing, and no keyframes of
                // the position at theirs follow both in between: a position for every frame the
                // layer is on screen, and for every keyframe either of them has.
                let rate = s.sequence_rate();
                let frames = it.duration.0 / rate.frame_duration().0.max(1);
                if frames > MAX_PLACED_FRAMES {
                    return Err(bad(
                        CMD,
                        format!(
                            "the layer's Motion is keyframed, so its place is worked out on every frame, and it has {frames} (the limit is {MAX_PLACED_FRAMES}). Cut the layer down to the stretch that matters, or take the keyframes off its Motion"
                        ),
                    ));
                }
                let mut times: Vec<Tick> = path_keys.iter().map(|k| k.time).collect();
                for id in ["position", "anchor", "rotation"] {
                    times.extend(motion.param(id).into_iter().flat_map(|p| p.keyframes.iter().map(|k| k.time)));
                }
                // the frames of the sequence's own grid from the layer's first to its last
                let mut frame = rate.frame_at(it.start);
                if rate.tick_of(frame) < it.start {
                    frame = frame.saturating_add(1);
                }
                let on_screen = times.len();
                for _ in 0..=frames.saturating_add(1) {
                    let at = rate.tick_of(frame);
                    if at >= it.end() {
                        break;
                    }
                    times.push(it.effect_time_at(at));
                    frame = frame.saturating_add(1);
                }
                if times.len() == on_screen {
                    // shorter than a frame and between two of them
                    times.push(first);
                }
                times.sort_unstable();
                times.dedup();
                times.into_iter().map(|t| Ok(Keyframe::new(t, key(t)?))).collect::<Result<Vec<_>>>()?
            } else {
                // Only the mask path moves: a position at each of its keyframes, moving between
                // them as the path does.
                let mut keys = Vec::with_capacity(path_keys.len());
                for (i, k) in path_keys.iter().enumerate() {
                    let mut at = Keyframe::new(k.time, key(k.time)?);
                    (at.interp, at.out_influence, at.in_influence) = rides(i, k);
                    keys.push(at);
                }
                keys
            };
            match keys.first() {
                Some(k) => Param { value: k.value.clone(), keyframes: keys },
                None => Param::new(key(first)?),
            }
        }
    };
    if !std::iter::once(&position.value)
        .chain(position.keyframes.iter().map(|k| &k.value))
        .all(|v| v.as_vec2().is_some_and(|v| v.x.is_finite() && v.y.is_finite()))
    {
        return Err(unplaced());
    }
    let keyframes = position.keyframes.len();
    let Some(m) = after.effect_mut("motion") else { return Err(bad(CMD, "the clip has no Motion to set")) };
    m.params.insert("position".into(), position);
    let set: Vec<(String, Param)> =
        ["position", "scale", "scale_width"].into_iter().filter_map(|k| m.params.get(k).map(|v| (k.to_string(), v.clone()))).collect();
    // Asked for what it already is, the layer is left alone: no undo step for a change that is
    // none. A position never set reads as the middle of the frame, which is where it is.
    let settled = |p: &Param| {
        let mut p = p.clone();
        p.map_values(|v| match v {
            ParamValue::Vec2(v) if v.x.is_nan() => ParamValue::Vec2(centre),
            other => other,
        });
        p
    };
    let unset = Param::new(ParamValue::Vec2(centre));
    let changes = set.iter().any(|(k, v)| match k.as_str() {
        "position" => settled(v) != settled(motion.param(k).unwrap_or(&unset)),
        _ => motion.param(k) != Some(v),
    });

    // Smaller or shifted, the layer no longer covers the place its subject stood, and the clip it
    // was made from shows there. Only that clip can: a layer whose clip is gone, switched off or
    // on a track that is switched off has nothing behind it to show twice.
    let tracks = q.video_tracks.as_slice();
    let own = tracks.iter().position(|t| t.items.iter().any(|x| x.id == it.id)).unwrap_or(0);
    let base = tracks.iter().take(own).enumerate().find_map(|(i, t)| t.items.iter().find(|x| of_its_stack(&s.project, x, &it)).map(|x| (i, t, x)));
    let seen = |item: &TrackItem, t: Tick| outline_bounds(&mask.path_at(t).transformed(&place(item, t)));
    let at_times: Vec<Tick> =
        after.effect("motion").and_then(|m| m.param("position")).map(|p| p.keyframes.iter().map(|k| k.time).collect()).unwrap_or_default();
    let mut warnings: Vec<String> = Vec::new();
    if let Some((under, _, base)) = base.filter(|(_, t, b)| t.enabled && b.enabled) {
        let uncovers = at_times.iter().copied().chain(at_times.is_empty().then_some(first)).any(|t| match (seen(base, t), seen(&after, t)) {
            (Some(stood), Some(now)) => {
                stood.0.x < now.0.x - COVER_SLACK || stood.0.y < now.0.y - COVER_SLACK || stood.1.x > now.1.x + COVER_SLACK || stood.1.y > now.1.y + COVER_SLACK
            }
            _ => false,
        });
        // a plate: a still of something else that is shown between the clip and the layer for as
        // long as the layer runs. A still under the clip is a backdrop, and hides nothing of it.
        let plate = tracks
            .iter()
            .take(own)
            .skip(under + 1)
            .filter(|t| t.enabled)
            .flat_map(|t| t.items.iter())
            .any(|x| x.enabled && x.item != it.item && is_still(&s.project, x.item) && x.start <= it.start && x.end() >= it.end());
        if uncovers && !plate {
            warnings.push(
                "the layer no longer covers the place its subject stood, and no plate lies between it and the clip it was made from: the clip shows there (the subject a second time, or the hole it was cut from). `layers.lift` makes the layer with a plate that fills the place"
                    .into(),
            );
        }
    }
    let holds = fixed_point(&mask.path_at(first), &place(&after, first), anchor).map(|(_, f)| json!([round2(f.x), round2(f.y)]));
    let placed_at = after.effect("motion").map(|m| m.vec2_at("position", first)).map(|v| json!([round2(v.x), round2(v.y)]));

    let before = crate::story::looks_across(s, it.start, it.end());
    if changes {
        s.edit_sequence("Transform Layer", |q, _, _| {
            let (_, layer) = q.find_item_mut(id).ok_or(filmagic_edit::EditError::NoItem(id))?;
            let m = layer.effect_mut("motion").ok_or_else(|| bad(CMD, "the clip has no Motion to set"))?;
            m.params.extend(set);
            Ok(())
        })?;
    }
    s.state.selection = vec![id];
    Ok(json!({
        "layer": id.0,
        "mask": mask_i,
        "scale": rescale.unwrap_or(was),
        "was": was,
        "anchor": [anchor.0, anchor.1],
        "offset": [shift.x, shift.y],
        // where the layer's Motion puts it, and where the anchor is in the frame, at the layer's first frame
        "position": placed_at,
        "anchorAt": holds,
        "keyframes": keyframes,
        "changed": changes,
        "warnings": warnings,
        "pictureChange": crate::story::change_json(&before, &crate::story::looks_across(s, it.start, it.end())),
    }))
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}
