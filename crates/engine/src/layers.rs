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
//!
//! A region is given in fractions of the clip's own picture (0…1 from its top-left corner), so it
//! does not depend on the size a frame was looked at:
//! `{"shape":"rectangle"|"ellipse","x":…,"y":…,"w":…,"h":…}` or `{"points":[[x,y],…]}`.
//!
//! What these commands cannot do is invent pixels. A clean plate is a statistic of the frames that
//! exist: it needs a camera that does not move and a subject that does. Nothing here replaces an
//! object with a different one, or shows what was never photographed.

use filmcraft_project::{ClipId, ItemId, TrackItem, TrackKind};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, NamedKeys, bad, clip_p, has_seq, str_p, u64_p};
use crate::{EngineError, Result, Session};

type Run = fn(&mut Session, &Value) -> Result<Value>;

/// Most bytes of frames one clean plate is made from.
const PLATE_BYTES: usize = 768 << 20;

const REGION_DOC: &str = r#"{"shape":"rectangle"|"ellipse","x":0..1,"y":0..1,"w":0..1,"h":0..1}|{"points":[[x,y],…]}"#;

fn edit(id: &'static str, label: &'static str, params: &'static str, run: Run) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: has_seq, run, journal: true, named: NamedKeys::NONE }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        edit(
            "layers.extract",
            "Extract Region to a Layer",
            r#"{"clip":id,"region":{"shape":"rectangle"|"ellipse","x":0..1,"y":0..1,"w":0..1,"h":0..1}|{"points":[[x,y],…]},"feather":px=12,"follow":bool=false (track the subject through the clip),"cutOut":bool=false (cut the region out of the original),"name":str?}"#,
            extract,
        ),
        edit(
            "layers.cleanPlate",
            "Make a Clean Plate",
            r#"{"clip":id,"path":str (PNG to write),"samples":n=15,"region":{"shape":"rectangle"|"ellipse","x":0..1,"y":0..1,"w":0..1,"h":0..1}|{"points":[[x,y],…]}? (cover only this; default: the whole frame),"feather":px=24,"place":bool=true}"#,
            clean_plate,
        ),
        edit(
            "layers.lift",
            "Lift Subject to a Layer",
            r#"{"clip":id,"region":{"shape":"rectangle"|"ellipse","x":0..1,"y":0..1,"w":0..1,"h":0..1}|{"points":[[x,y],…]},"path":str (PNG of the plate),"feather":px=12,"samples":n=15,"follow":bool=false}"#,
            lift,
        ),
        edit(
            "layers.retime",
            "Retime a Region",
            r#"{"clip":id,"region":{"shape":"rectangle"|"ellipse","x":0..1,"y":0..1,"w":0..1,"h":0..1}|{"points":[[x,y],…]},"speed":percent,"feather":px=12,"interpolation":"frameSampling|frameBlending|opticalFlow"?}"#,
            retime,
        ),
        edit(
            "layers.adjust",
            "Adjust a Region",
            r#"{"clip":id,"region":{"shape":"rectangle"|"ellipse","x":0..1,"y":0..1,"w":0..1,"h":0..1}|{"points":[[x,y],…]},"effect":id|name,"params":{param:value,…}?,"feather":px=12,"follow":bool=false}"#,
            adjust,
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
        .ok_or_else(|| bad(cmd, format!("region `{what}` must be a fraction of the frame (0 to 1)")))
}

/// A region of a clip's picture as `masks.add` parameters, in clip pixels.
fn region_p(p: &Value, size: (u32, u32), cmd: &str) -> Result<Value> {
    let (w, h) = (f64::from(size.0), f64::from(size.1));
    let r = p.get("region").filter(|r| r.is_object()).ok_or_else(|| bad(cmd, format!("need `region`: {REGION_DOC}")))?;
    if let Some(points) = r.get("points") {
        let pts = points.as_array().filter(|a| (3..=64).contains(&a.len())).ok_or_else(|| bad(cmd, "region `points` must be 3 to 64 points"))?;
        let mut path = Vec::with_capacity(pts.len());
        for pt in pts {
            let xy = pt.as_array().filter(|a| a.len() == 2).ok_or_else(|| bad(cmd, "each region point is [x, y]"))?;
            path.push(json!([fraction(xy.first(), "points", cmd)? * w, fraction(xy.get(1), "points", cmd)? * h]));
        }
        return Ok(json!({"shape": "bezier", "path": path}));
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
        return Err(bad(cmd, "the region is empty: `w` and `h` are fractions of the frame, greater than 0"));
    }
    Ok(json!({"shape": shape, "center": [(x + rw / 2.0) * w, (y + rh / 2.0) * h], "size": [rw * w, rh * h]}))
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
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (track, it) = q.find_item(clip).ok_or_else(|| bad(cmd, format!("no clip {} in the active sequence", clip.0)))?;
    let tr = q.track(track).ok_or_else(|| bad(cmd, "the clip's track is gone"))?;
    if tr.kind != TrackKind::Video {
        return Err(bad(cmd, "the clip is a sound clip; a region needs a picture"));
    }
    if tr.locked {
        return Err(bad(cmd, format!("track {} is locked", tr.name)));
    }
    let size = filmcraft_render::source_size(&s.project, it.item).filter(|(w, h)| *w > 0 && *h > 0).ok_or_else(|| bad(cmd, "the clip has no picture"))?;
    Ok((it.clone(), size))
}

/// Add a mask of `shape` (as [`region_p`] gives it) to `effect` of `clip`. Returns the mask index.
fn add_mask(s: &mut Session, clip: ClipId, effect: Value, shape: &Value, feather: f64, extra: Value) -> Result<u64> {
    let mut add = shape.clone();
    add["clip"] = json!(clip.0);
    add["effect"] = effect.clone();
    let mask = run(s, "masks.add", add)?["mask"].as_u64().ok_or_else(|| EngineError::Other("the mask was not created".into()))?;
    let mut set = json!({"clip": clip.0, "effect": effect, "mask": mask, "feather": feather});
    if let (Some(set), Some(extra)) = (set.as_object_mut(), extra.as_object()) {
        set.extend(extra.clone());
    }
    run(s, "masks.set", set)?;
    Ok(mask)
}

/// Track mask `mask` of `effect` on `clip` from the clip's first frame to its last, now.
fn follow(s: &mut Session, it: &TrackItem, clip: ClipId, effect: Value, mask: u64) -> Result<Value> {
    let r = run(s, "masks.track", json!({"clip": clip.0, "effect": effect, "mask": mask, "time": it.start.0, "direction": "forward", "wait": true}))?;
    let st = &r["status"];
    Ok(json!({"frames": r["frames"], "tracked": st["done"], "status": st["status"], "error": st["result"]["error"]}))
}

/// Copy `it` onto a new video track above everything, unlinked from its sound. Returns the copy.
fn copy_above(s: &mut Session, it: &TrackItem, cmd: &str) -> Result<ClipId> {
    let r = run(s, "timeline.move", json!({"moves": [{"clip": it.id.0, "track": "new", "time": it.start.0}], "copy": true, "linked": false}))?;
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
    let mask = add_mask(s, clip, json!("opacity"), shape, feather, json!({}))?;
    let tracking = if do_follow { Some(follow(s, it, clip, json!("opacity"), mask)?) } else { None };
    Ok(Layer { clip, mask, tracking })
}

fn extract(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "layers.extract";
    let (it, size) = picture_clip(s, p, CMD)?;
    let shape = region_p(p, size, CMD)?;
    let feather = px_p(p, "feather", 12.0, CMD)?;
    let (do_follow, cut_out) = (flag_p(p, "follow", false, CMD)?, flag_p(p, "cutOut", false, CMD)?);
    let name = name_p(p, CMD)?;
    s.edit_group("Extract Layer", |s| {
        let layer = extract_layer(s, &it, &shape, feather, do_follow, CMD)?;
        if let Some(n) = name {
            rename(s, layer.clip, n)?;
        }
        let mut out = json!({"layer": layer.clip.0, "track": track_name(s, layer.clip), "mask": layer.mask, "of": it.id.0, "tracking": layer.tracking});
        if cut_out {
            // the same shape, inverted, on the original; when the layer's mask follows the subject the hole follows too
            let hole = add_mask(s, it.id, json!("opacity"), &shape, feather, json!({"inverted": true}))?;
            if do_follow {
                copy_mask_path(s, layer.clip, layer.mask, it.id, hole)?;
            }
            out["cutOut"] = json!({"clip": it.id.0, "mask": hole});
        }
        // the layer is what the next command most likely works on
        s.state.selection = vec![layer.clip];
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
    item: ItemId,
    clip: Option<ClipId>,
    report: Value,
}

fn samples_p(p: &Value, cmd: &str) -> Result<usize> {
    match p.get("samples") {
        None | Some(Value::Null) => Ok(15),
        Some(_) => u64_p(p, "samples")
            .filter(|n| (3..=filmcraft_render::plate::MAX_PLATE_SAMPLES as u64).contains(n))
            .map(|n| n as usize)
            .ok_or_else(|| bad(cmd, format!("`samples` must be 3 to {}", filmcraft_render::plate::MAX_PLATE_SAMPLES))),
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
fn median_plate(s: &Session, it: &TrackItem, samples: usize, cmd: &str) -> Result<(Vec<u8>, usize, usize, filmcraft_render::plate::Stillness, usize)> {
    let provider = s.media.full_res_provider(s.project.clone(), s.services.clone());
    let mut frames: Vec<Vec<u8>> = Vec::new();
    let mut dims: Option<(usize, usize)> = None;
    let mut n = samples;
    let mut k = 0;
    while k < n {
        // the middle of each of `n` equal parts of the clip
        let t = it.start + Tick(it.duration.0 / (n as i64 * 2) * (2 * k as i64 + 1));
        let img = filmcraft_render::render_item(&s.project, it.item, it.source_time_at(t), 1.0, &provider)
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
    let (plate, still) = filmcraft_render::plate::temporal_median(&frames, w, h).ok_or_else(|| bad(cmd, "the plate could not be made from these frames"))?;
    Ok((plate, w, h, still, frames.len()))
}

/// The steps of `layers.cleanPlate`: make the plate, write and import it, and (with `place`) lay it
/// on a new track over the clip, masked to `shape` when one is given.
fn make_plate(s: &mut Session, it: &TrackItem, path: &str, samples: usize, cover: Option<(&Value, f64)>, place: bool, cmd: &str) -> Result<Plate> {
    let (plate, w, h, still, used) = median_plate(s, it, samples, cmd)?;
    let (w32, h32) = (u32::try_from(w).map_err(|_| bad(cmd, "the frame is too wide"))?, u32::try_from(h).map_err(|_| bad(cmd, "the frame is too tall"))?);
    let bytes = filmcraft_export::encode_png(plate, w32, h32).map_err(|e| EngineError::Other(e.to_string()))?;
    s.services.write_file(path, &bytes).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    let imported = run(s, "file.import", json!({"paths": [path]}))?;
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
    let mut clip = None;
    if place {
        let placed = run(s, "timeline.place", json!({"item": item.0, "track": "new", "time": it.start.0, "duration": it.duration.0, "audio": false}))?;
        let id = placed["clips"].get(0).and_then(Value::as_u64).map(ClipId).ok_or_else(|| EngineError::Other("the plate could not be placed".into()))?;
        // the plate sits exactly where the clip's picture sits: same Motion, same fit to the frame
        let (motion, fit) = (it.effects.iter().find(|e| e.effect == "motion").cloned(), it.scale_to_frame);
        s.edit_sequence("Align Plate", |q, _, _| {
            if let Some((_, pl)) = q.find_item_mut(id) {
                pl.scale_to_frame = fit;
                pl.name = format!("{} (plate)", it.name);
                if let (Some(m), Some(slot)) = (motion, pl.effects.iter_mut().find(|e| e.effect == "motion")) {
                    *slot = m;
                }
            }
            Ok(())
        })?;
        if let Some((shape, feather)) = cover {
            // a little wider than the region, so the feathered edge of what it hides is covered too
            add_mask(s, id, json!("opacity"), shape, feather, json!({"expansion": feather}))?;
        }
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
    Ok(Plate { item, clip, report })
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
        Some(_) => Some(region_p(p, size, CMD)?),
    };
    if shape.is_some() && !place {
        return Err(bad(CMD, "a `region` masks the placed plate: it needs `place` true"));
    }
    s.edit_group("Clean Plate", |s| {
        let plate = make_plate(s, &it, &path, samples, shape.as_ref().map(|sh| (sh, feather)), place, CMD)?;
        if let Some(c) = plate.clip {
            s.state.selection = vec![c];
        }
        let _ = plate.item;
        Ok(plate.report)
    })
}

fn lift(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "layers.lift";
    let (it, size) = picture_clip(s, p, CMD)?;
    let shape = region_p(p, size, CMD)?;
    let path = path_p(p, CMD)?;
    let samples = samples_p(p, CMD)?;
    let feather = px_p(p, "feather", 12.0, CMD)?;
    let do_follow = flag_p(p, "follow", false, CMD)?;
    s.edit_group("Lift Layer", |s| {
        // bottom to top: the clip, the plate over the place the subject stood, the subject
        let plate = make_plate(s, &it, &path, samples, Some((&shape, feather)), true, CMD)?;
        let layer = extract_layer(s, &it, &shape, feather, do_follow, CMD)?;
        if let (true, Some(patch)) = (do_follow, plate.clip) {
            // the patch covers wherever the subject goes
            copy_mask_path(s, layer.clip, layer.mask, patch, 0)?;
        }
        s.state.selection = vec![layer.clip];
        Ok(json!({
            "layer": layer.clip.0,
            "track": track_name(s, layer.clip),
            "mask": layer.mask,
            "of": it.id.0,
            "tracking": layer.tracking,
            "plate": plate.report,
            "note": "move, scale or re-time the layer (its Motion, `clip.speedDuration`), or switch it off (`clip.enable`) to take the subject out; the plate hides the original underneath",
        }))
    })
}

fn retime(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "layers.retime";
    let (it, size) = picture_clip(s, p, CMD)?;
    let shape = region_p(p, size, CMD)?;
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
                .filter(|m| filmcraft_project::TimeInterpolation::from_name(m).is_some())
                .ok_or_else(|| bad(CMD, "`interpolation` must be frameSampling, frameBlending or opticalFlow"))?
                .to_string(),
        ),
    };
    s.edit_group("Retime Region", |s| {
        let layer = extract_layer(s, &it, &shape, feather, false, CMD)?;
        let mut sp = json!({"clips": [layer.clip.0], "speed": speed, "ripple": false});
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
            "note": "inside the region the shot plays at the new speed, outside at its own. It reads as one picture while the subject stays inside the region and the camera holds still; for a subject that leaves it, `layers.lift` first and re-time the lifted layer",
        }))
    })
}

fn adjust(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "layers.adjust";
    let (it, size) = picture_clip(s, p, CMD)?;
    let shape = region_p(p, size, CMD)?;
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
        let mask = add_mask(s, it.id, json!(index), &shape, feather, json!({}))?;
        let tracking = if do_follow { Some(follow(s, &it, it.id, json!(index), mask)?) } else { None };
        Ok(json!({"clip": it.id.0, "effect": id, "index": index, "mask": mask, "tracking": tracking}))
    })
}
