use serde_json::json;

use crate::Session;
use filmcraft_project::ClipId;
use filmcraft_time::Tick;

fn demo() -> (Session, ClipId) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    let it = q.video_tracks[0].items[0].clone();
    // middle of the first V1 clip (past its fade from black)
    s.set_playhead(it.start + Tick(it.duration.0 / 2));
    s.state.selection = vec![it.id];
    (s, it.id)
}

fn px(img: &filmcraft_render::Image, fx: f32, fy: f32) -> [f32; 4] {
    img.get(((img.w as f32 * fx) as usize).min(img.w - 1), ((img.h as f32 * fy) as usize).min(img.h - 1))
}

fn dist(a: [f32; 4], b: [f32; 4]) -> f32 {
    (0..3).map(|k| (a[k] - b[k]).abs()).fold(0.0, f32::max)
}

#[test]
fn masked_effect_applies_only_inside_the_mask() {
    let (mut s, clip) = demo();
    let plain = s.render_program(0.25).unwrap();
    s.execute("effects.apply", json!({"effect": "invert"})).unwrap();
    let inverted = s.render_program(0.25).unwrap();
    // ellipse mask on Invert, in the left half of the frame
    let r = s.execute("masks.add", json!({"effect": "invert", "shape": "ellipse", "center": [480, 540], "size": [600, 600]})).unwrap();
    assert_eq!(r["name"], "Mask (1)");
    assert_eq!(r["mask"], 0);
    let masked = s.render_program(0.25).unwrap();
    // inside: inverted; outside: untouched
    assert!(dist(px(&masked, 0.25, 0.5), px(&inverted, 0.25, 0.5)) < 1e-4);
    assert!(dist(px(&masked, 0.8, 0.5), px(&plain, 0.8, 0.5)) < 1e-4);
    assert!(dist(px(&plain, 0.25, 0.5), px(&inverted, 0.25, 0.5)) > 0.05, "invert changes the picture");
    // inverted mask swaps the regions
    s.execute("masks.set", json!({"clip": clip.0, "effect": "invert", "mask": 0, "inverted": true})).unwrap();
    let inv = s.render_program(0.25).unwrap();
    assert!(dist(px(&inv, 0.25, 0.5), px(&plain, 0.25, 0.5)) < 1e-4);
    assert!(dist(px(&inv, 0.8, 0.5), px(&inverted, 0.8, 0.5)) < 1e-4);
    // undo ×2 → no mask
    s.undo();
    s.undo();
    let back = s.render_program(0.25).unwrap();
    assert_eq!(back.px, inverted.px);
    assert!(s.state.selected_mask.is_none(), "selection follows undo");
}

#[test]
fn opacity_mask_cuts_out_the_clip() {
    let (mut s, clip) = demo();
    // V1 only: an opacity mask leaves black outside
    s.execute("masks.add", json!({"clip": clip.0, "shape": "polygon", "center": [960, 540], "size": [960, 540]})).unwrap();
    s.execute("masks.set", json!({"feather": 0})).unwrap();
    let img = s.render_program(0.25).unwrap();
    assert!(px(&img, 0.5, 0.5)[3] > 0.99, "inside opaque");
    let corner = px(&img, 0.05, 0.05);
    assert!(corner[0] + corner[1] + corner[2] < 1e-4, "outside cut: {corner:?}");
    let list = s.execute("masks.list", json!({})).unwrap();
    let m = &list["masks"][0];
    assert_eq!(m["effectId"], "opacity");
    assert_eq!(m["path"]["vertices"].as_array().unwrap().len(), 4);
    assert_eq!(m["selected"], true);
}

#[test]
fn keyframed_mask_path_interpolates() {
    let (mut s, clip) = demo();
    let it = s.active_sequence().unwrap().find_item(clip).unwrap().1.clone();
    s.set_playhead(it.start);
    s.execute("masks.add", json!({"effect": "opacity", "shape": "polygon", "center": [400, 400], "size": [200, 200]})).unwrap();
    s.execute("effects.toggleAnimation", json!({"clip": clip.0, "effect": "opacity", "mask": 0, "param": "path"})).unwrap();
    let rate = s.active_sequence().unwrap().settings.frame_rate;
    let t1 = it.start + rate.tick_of(10);
    s.set_playhead(t1);
    s.execute("masks.translate", json!({"delta": [100, -50]})).unwrap();
    // feather keyframes too, through the generic keyframe command
    s.execute("effects.toggleAnimation", json!({"clip": clip.0, "effect": "opacity", "mask": 0, "param": "feather"})).unwrap();
    s.execute("effects.setParam", json!({"clip": clip.0, "effect": "opacity", "mask": 0, "param": "feather", "value": 30.0})).unwrap();
    let mid = it.start + rate.tick_of(5);
    let l = s.execute("masks.list", json!({"time": mid.0})).unwrap();
    let v0 = &l["masks"][0]["path"]["vertices"][0]["p"];
    assert!((v0[0].as_f64().unwrap() - 350.0).abs() < 1e-6, "{v0}");
    assert!((v0[1].as_f64().unwrap() - 275.0).abs() < 1e-6, "{v0}");
    assert_eq!(l["masks"][0]["pathKeyframes"].as_array().unwrap().len(), 2);
    let end = s.execute("masks.list", json!({"time": t1.0})).unwrap();
    assert_eq!(end["masks"][0]["feather"], 30.0);
    // vertex edit at a keyframe replaces that keyframe; handles mirror unless broken
    s.execute("masks.moveVertex", json!({"vertex": 1, "handle": "out", "delta": [0, 40]})).unwrap();
    let e = s.execute("masks.list", json!({})).unwrap();
    let v1 = &e["masks"][0]["path"]["vertices"][1];
    assert_eq!(v1["out"], json!([0.0, 40.0]));
    assert_eq!(v1["in"], json!([-0.0, -40.0]));
    assert_eq!(e["masks"][0]["pathKeyframes"].as_array().unwrap().len(), 2);
    // add / remove vertices keep all keyframes interpolable
    s.execute("masks.addVertex", json!({"after": 0, "at": [500, 220]})).unwrap();
    let a = s.execute("masks.list", json!({"time": mid.0})).unwrap();
    assert_eq!(a["masks"][0]["path"]["vertices"].as_array().unwrap().len(), 5);
    s.execute("masks.removeVertex", json!({"vertex": 1})).unwrap();
    let a = s.execute("masks.list", json!({"time": mid.0})).unwrap();
    assert_eq!(a["masks"][0]["path"]["vertices"].as_array().unwrap().len(), 4);
}

#[test]
fn add_vertex_on_animated_path_clamps_oversized_index_and_rejects_empty_path() {
    let (mut s, clip) = demo();
    let it = s.active_sequence().unwrap().find_item(clip).unwrap().1.clone();
    let rate = s.active_sequence().unwrap().settings.frame_rate;
    s.set_playhead(it.start);
    s.execute("masks.add", json!({"effect": "opacity", "shape": "polygon", "center": [400, 400], "size": [200, 200]})).unwrap();
    s.execute("effects.toggleAnimation", json!({"clip": clip.0, "effect": "opacity", "mask": 0, "param": "path"})).unwrap();
    s.set_playhead(it.start + rate.tick_of(10));
    s.execute("masks.translate", json!({"delta": [5, 0]})).unwrap();
    // an index past the last vertex appends instead of panicking
    s.execute("masks.addVertex", json!({"after": 100, "at": [1, 1]})).unwrap();
    let l = s.execute("masks.list", json!({})).unwrap();
    assert_eq!(l["masks"][0]["path"]["vertices"].as_array().unwrap().len(), 5);

    // an empty animated path is a parameter error and leaves the mask alone
    s.set_playhead(it.start);
    s.execute("masks.add", json!({"effect": "opacity", "path": {"vertices": [], "closed": false}})).unwrap();
    s.execute("effects.toggleAnimation", json!({"clip": clip.0, "effect": "opacity", "mask": 1, "param": "path"})).unwrap();
    s.set_playhead(it.start + rate.tick_of(10));
    s.execute("masks.translate", json!({"mask": 1, "delta": [5, 0]})).unwrap();
    let e = s.execute("masks.addVertex", json!({"mask": 1, "after": 0, "at": [1, 1]})).unwrap_err().to_string();
    assert!(e.contains("no vertices"), "{e}");
    let l = s.execute("masks.list", json!({})).unwrap();
    assert_eq!(l["masks"][1]["path"]["vertices"].as_array().unwrap().len(), 0);

    // a static empty path still takes its first vertex
    s.execute("masks.add", json!({"effect": "opacity", "path": {"vertices": [], "closed": false}})).unwrap();
    s.execute("masks.addVertex", json!({"mask": 2, "after": 0, "at": [1, 1]})).unwrap();
    let l = s.execute("masks.list", json!({})).unwrap();
    assert_eq!(l["masks"][2]["path"]["vertices"].as_array().unwrap().len(), 1);
}

#[test]
fn merged_drag_is_one_undo_step_and_project_round_trips() {
    let (mut s, _) = demo();
    s.execute("effects.apply", json!({"effect": "gaussian_blur"})).unwrap();
    s.execute("masks.add", json!({"effect": "gaussian_blur", "shape": "ellipse"})).unwrap();
    let n = s.history.undo.len();
    for _ in 0..5 {
        s.execute("masks.translate", json!({"delta": [3, 1], "merge": "drag-1"})).unwrap();
    }
    assert_eq!(s.history.undo.len(), n + 1);
    s.execute("masks.set", json!({"mode": "subtract", "trackMethod": "position", "expansion": -4})).unwrap();
    let bytes = filmcraft_format::encode(&s.project, false);
    let back = filmcraft_format::decode(&bytes).unwrap().project;
    assert_eq!(&back, &*s.project);
    let l = s.execute("masks.list", json!({})).unwrap();
    assert_eq!(l["masks"][0]["mode"], "Subtract");
    assert_eq!(l["masks"][0]["trackMethod"], "Position");
    assert!(s.execute("masks.add", json!({"effect": "motion"})).is_err(), "Motion has no masks");
}

// ---------------------------------------------------------------- tracking

/// A 320×240 24 fps clip of a textured disc moving under a known similarity motion over a static
/// textured background.
struct Moving {
    info: filmcraft_media::MediaInfo,
}

fn texture(u: f64, v: f64) -> f64 {
    let s = (u * 0.21).sin() * (v * 0.17).cos() + 0.5 * ((u + v) * 0.43).sin() + 0.35 * ((u * 0.9 - v * 0.6).sin() * (v * 0.75).cos());
    let (iu, iv) = ((u / 6.0).floor() as i64, (v / 6.0).floor() as i64);
    let mut x = (iu.wrapping_mul(73_856_093) ^ iv.wrapping_mul(19_349_663)) as u64;
    x ^= x >> 13;
    x = x.wrapping_mul(0x5bd1_e995);
    let h = (x >> 40) as f64 / (1u64 << 24) as f64;
    (0.5 + 0.22 * s + 0.12 * h).clamp(0.0, 1.0)
}

/// Ground-truth pose of the disc at frame `k`: translation, 1.5°/frame rotation, +1 %/frame scale.
fn truth(k: f64) -> filmcraft_geom::Affine {
    filmcraft_geom::Affine::motion(
        filmcraft_geom::Vec2::new(120.0 + 3.0 * k, 110.0 + 1.2 * k),
        filmcraft_geom::Vec2::new(1.0 + 0.01 * k, 1.0 + 0.01 * k),
        1.5 * k,
        filmcraft_geom::Vec2::ZERO,
    )
}

impl filmcraft_media::MediaSource for Moving {
    fn info(&self) -> &filmcraft_media::MediaInfo {
        &self.info
    }
    fn video_frame(&self, req: filmcraft_media::FrameRequest) -> filmcraft_media::Result<std::sync::Arc<filmcraft_frame::VideoFrame>> {
        let rate = filmcraft_time::FrameRate::FPS_24;
        let k = rate.frame_at(req.time) as f64;
        let inv = truth(k).inverse().unwrap();
        let (w, h) = (320usize, 240usize);
        let mut px = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let mut acc = 0.0;
                for (ox, oy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                    let p = filmcraft_geom::Vec2::new(x as f64 + ox, y as f64 + oy);
                    let o = inv.apply(p);
                    acc += if o.length() < 70.0 { texture(o.x + 200.0, o.y + 300.0) } else { 0.25 + 0.1 * texture(p.x * 0.5 + 900.0, p.y * 0.5) };
                }
                let v = (acc / 4.0 * 255.0).round() as u8;
                px[(y * w + x) * 4..][..4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        Ok(std::sync::Arc::new(filmcraft_frame::VideoFrame::rgba8(w as u32, h as u32, px)))
    }
    fn audio(&self, _start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<filmcraft_frame::AudioBuffer> {
        Ok(filmcraft_frame::AudioBuffer::silence(sample_rate, 2, frames))
    }
}

fn tracking_session() -> (Session, ClipId) {
    session_of(320, 240, |info| std::sync::Arc::new(Moving { info }))
}

/// A plain block (a figure in a plain shirt) standing still at the middle of a 24 fps clip, in
/// front of water that drifts right and down. The water has the detail and the block has none, so
/// the motion found inside a mask around the block is the water's. At `zoom` 1 the clip is
/// 320×240, the block 26×68 and the water moves 2.5 px right and 1 px down a frame; `zoom` scales
/// the clip, the block and the water's speed, and leaves the water's grain as fine as it was.
/// Frames come at the scale they are asked for.
///
/// With `walk` (px a frame at `zoom` 1) it is the other way round: the block has a grain of its
/// own and moves right at that speed, and the water stands still.
struct Adrift {
    info: filmcraft_media::MediaInfo,
    zoom: f64,
    walk: f64,
}

impl filmcraft_media::MediaSource for Adrift {
    fn info(&self) -> &filmcraft_media::MediaInfo {
        &self.info
    }
    fn video_frame(&self, req: filmcraft_media::FrameRequest) -> filmcraft_media::Result<std::sync::Arc<filmcraft_frame::VideoFrame>> {
        let k = filmcraft_time::FrameRate::FPS_24.frame_at(req.time) as f64;
        let scale = if req.scale > 0.0 && req.scale <= 1.0 { req.scale as f64 } else { 1.0 };
        let (w, h) = ((320.0 * self.zoom * scale).round() as usize, (240.0 * self.zoom * scale).round() as usize);
        // frame pixels per clip pixel
        let per = w as f64 / (320.0 * self.zoom);
        let z = self.zoom;
        let walking = self.walk != 0.0;
        // how far the block and the water have moved, in clip pixels
        let (block, water) = if walking { (self.walk * z * k, (0.0, 0.0)) } else { (0.0, (2.5 * z * k, z * k)) };
        let mut px = vec![255u8; w * h * 4];
        for (i, o) in px.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            // in clip pixels
            let (x, y) = ((i % w) as f64 / per, (i / w) as f64 / per);
            let c = if (x - block - 160.0 * z).abs() < 13.0 * z && (y - 120.0 * z).abs() < 34.0 * z {
                let v = if walking { 0.55 + 0.9 * texture(x - block + 500.0, y + 90.0) } else { 1.0 };
                [0.78 * v, 0.72 * v, 0.6 * v]
            } else {
                let v = 0.45 + 1.1 * texture(x - water.0 + 40.0, y - water.1 + 700.0);
                [0.22 * v, 0.42 * v, 0.6 * v]
            };
            for (o, c) in o.iter_mut().zip(c) {
                *o = (c.clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        }
        Ok(std::sync::Arc::new(filmcraft_frame::VideoFrame::rgba8(w as u32, h as u32, px)))
    }
    fn audio(&self, _start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<filmcraft_frame::AudioBuffer> {
        Ok(filmcraft_frame::AudioBuffer::silence(sample_rate, 2, frames))
    }
}

/// A session with the 320×240 [`Adrift`] clip (two seconds) alone on V1, selected.
pub(crate) fn adrift_session() -> (Session, ClipId) {
    session_of(320, 240, |info| std::sync::Arc::new(Adrift { info, zoom: 1.0, walk: 0.0 }))
}

/// A session with one two-second `w`×`h` clip on V1 of a sequence of that size, selected, whose
/// frames come from `source`.
fn session_of(w: u32, h: u32, source: impl FnOnce(filmcraft_media::MediaInfo) -> std::sync::Arc<dyn filmcraft_media::MediaSource>) -> (Session, ClipId) {
    use filmcraft_media::MediaSource;
    let rate = filmcraft_time::FrameRate::FPS_24;
    let g =
        filmcraft_media::generators::GeneratorSource::new(filmcraft_media::Generator::ColorMatte { color: [0.5, 0.5, 0.5, 1.0] }, w, h, rate, rate.tick_of(48));
    let info = g.info().clone();
    let mut p = filmcraft_project::Project::new("track");
    let item = p.add_item(
        "moving",
        filmcraft_project::Label::Iris,
        filmcraft_project::ItemKind::Media(Box::new(filmcraft_project::MediaClip {
            media: filmcraft_project::MediaRef::Generator(g.generator.clone()),
            info: info.clone(),
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
            identity: None,
        })),
        None,
    );
    let seq = p.new_sequence("s", filmcraft_project::SequenceSettings { width: w, height: h, frame_rate: rate, ..Default::default() }, 1, 0, None);
    let ti =
        p.make_track_item(item, filmcraft_project::TrackKind::Video, Tick::ZERO, filmcraft_time::TimeRange::new(Tick::ZERO, rate.tick_of(48)), rate).unwrap();
    let clip = ti.id;
    p.sequence_mut(seq).unwrap().video_tracks[0].items.push(ti);
    let mut s = Session { project: std::sync::Arc::new(p), ..Default::default() };
    s.state.active_sequence = Some(seq);
    s.state.selection = vec![clip];
    s.media.insert(item, source(info));
    (s, clip)
}

#[test]
fn tracking_follows_known_motion_forward_and_backward() {
    let (mut s, _) = tracking_session();
    let rate = filmcraft_time::FrameRate::FPS_24;
    s.set_playhead(rate.tick_of(10));
    // a circle of radius 45 inside the disc at frame 10
    let c = truth(10.0).apply(filmcraft_geom::Vec2::ZERO);
    s.execute("masks.add", json!({"effect": "opacity", "shape": "ellipse", "center": [c.x, c.y], "size": [90, 90]})).unwrap();
    let path0 = s.execute("masks.list", json!({})).unwrap()["masks"][0]["path"].clone();
    let p0 = crate::masks::path_from_json(&path0).unwrap();
    // object coordinates of the mask vertices (they ride on the disc)
    let inv10 = truth(10.0).inverse().unwrap();
    let obj: Vec<filmcraft_geom::Vec2> = p0.vertices.iter().map(|v| inv10.apply(v.p)).collect();
    let r = s.execute("masks.track", json!({"direction": "forward", "frames": 12, "wait": true})).unwrap();
    assert_eq!(r["frames"], 12, "{r}");
    assert!(s.mask_jobs.is_empty());
    let r = s.execute("masks.track", json!({"direction": "backward", "frames": 8, "wait": true, "method": "positionScaleRotation"})).unwrap();
    assert_eq!(r["frames"], 8);
    let l = s.execute("masks.list", json!({})).unwrap();
    let keys: Vec<i64> = l["masks"][0]["pathKeyframes"].as_array().unwrap().iter().map(|v| v.as_i64().unwrap()).collect();
    assert_eq!(keys.len(), 21, "frames 2..=22: {keys:?}");
    let mut worst = 0.0f64;
    for f in [2i64, 6, 10, 14, 18, 22] {
        let t = rate.tick_of(f);
        let m = s.execute("masks.list", json!({"time": t.0})).unwrap();
        let path = crate::masks::path_from_json(&m["masks"][0]["path"]).unwrap();
        for (v, o) in path.vertices.iter().zip(&obj) {
            worst = worst.max((v.p - truth(f as f64).apply(*o)).length());
        }
    }
    eprintln!("mask tracking: max vertex error {worst:.3} px over -8..+12 frames");
    assert!(worst < 1.5, "max vertex error {worst} px");
    // the two tracking runs are two undo steps
    let h = s.execute("history.list", json!({})).unwrap();
    let undo: Vec<&str> = h["undo"].as_array().unwrap().iter().filter_map(|v| v.as_str()).collect();
    assert_eq!(&undo[undo.len() - 2..], ["Track Mask", "Track Mask"], "{undo:?}");
}

/// The corners (x0, y0, x1, y1) of the box around mask 0's path at frame `frame`.
fn mask_box(s: &mut Session, frame: i64) -> (f64, f64, f64, f64) {
    let m = s.execute("masks.list", json!({"time": filmcraft_time::FrameRate::FPS_24.tick_of(frame).0})).unwrap();
    let path = crate::masks::path_from_json(&m["masks"][0]["path"]).unwrap();
    let fold = |pick: fn(filmcraft_geom::Vec2) -> f64, f: fn(f64, f64) -> f64, from: f64| path.vertices.iter().map(|v| pick(v.p)).fold(from, f);
    (fold(|p| p.x, f64::min, f64::MAX), fold(|p| p.y, f64::min, f64::MAX), fold(|p| p.x, f64::max, f64::MIN), fold(|p| p.y, f64::max, f64::MIN))
}

/// A mask around a small plain subject follows the busy background behind it instead, one good
/// fit after another. Tracking notices that the mask is no longer on what it started on, stops,
/// keeps the frames on which the mask still held its subject, and says so. `zoom` 4 is a
/// 1280×960 clip, tracked at three quarters of its size.
#[test]
fn tracking_stops_when_the_mask_slides_off_its_subject() {
    for (zoom, method) in [(1.0, "position"), (1.0, "positionRotation"), (1.0, "positionScaleRotation"), (4.0, "position"), (4.0, "positionScaleRotation")] {
        let what = format!("×{zoom} {method}");
        let (mut s, _) = session_of((320.0 * zoom) as u32, (240.0 * zoom) as u32, |info| std::sync::Arc::new(Adrift { info, zoom, walk: 0.0 }));
        s.execute("masks.add", json!({"effect": "opacity", "shape": "rectangle", "center": [160.0 * zoom, 120.0 * zoom], "size": [60.0 * zoom, 100.0 * zoom]}))
            .unwrap();
        let r = s.execute("masks.track", json!({"direction": "forward", "method": method, "wait": true})).unwrap();
        assert_eq!(r["frames"], 47, "{what}: {r}");
        let st = &r["status"];
        assert_eq!(st["finished"], true, "{what}: {r}");
        let error = st["result"]["error"].as_str().unwrap_or_else(|| panic!("{what}: tracking reported no error: {r}"));
        assert!(error.starts_with("lost track after ") && error.contains("are no longer inside it"), "{what}: {error}");
        let status = st["status"].as_str().unwrap();
        assert!(status.starts_with("lost track after ") && status.ends_with("frame(s) tracked)"), "{what}: {status}");
        // the frames before it are kept, as Mask Path keyframes in one undo step
        let tracked = st["done"].as_u64().unwrap();
        assert!(error.starts_with(&format!("lost track after {tracked} frame(s)")), "{what}: {error}");
        assert!((2..20).contains(&tracked), "{what}: stopped after {tracked} of 47 frames");
        let l = s.execute("masks.list", json!({})).unwrap();
        assert_eq!(l["masks"][0]["pathKeyframes"].as_array().unwrap().len() as u64, tracked + 1, "{what}: {l}");
        assert!(s.mask_jobs.is_empty());
        let h = s.execute("history.list", json!({})).unwrap();
        assert_eq!(h["undo"].as_array().unwrap().last().unwrap(), "Track Mask", "{h}");
        // on the last frame kept, the whole subject (26×68 at the middle, standing still) is under the mask
        let (x0, y0, x1, y1) = mask_box(&mut s, tracked as i64);
        let (sx, sy) = (13.0 * zoom, 34.0 * zoom);
        assert!(
            x0 <= 160.0 * zoom - sx && x1 >= 160.0 * zoom + sx && y0 <= 120.0 * zoom - sy && y1 >= 120.0 * zoom + sy,
            "{what}: after {tracked} frames the mask is at {x0:.0}..{x1:.0} × {y0:.0}..{y1:.0}"
        );
    }
}

/// A subject that is followed a long way is not called lost, on a clip the tracker reads at its
/// own size and on a 1280×960 one it reads at three quarters: the first frame's picture is looked
/// for where the mask has gone in the frame as read, not in the clip's pixels.
#[test]
fn a_followed_subject_is_checked_where_the_mask_is_at_any_clip_size() {
    for zoom in [1.0, 4.0] {
        let (mut s, _) = session_of((320.0 * zoom) as u32, (240.0 * zoom) as u32, |info| std::sync::Arc::new(Adrift { info, zoom, walk: 2.0 }));
        // a mask inside the 26×68 block, so what moves inside it is the block
        s.execute("masks.add", json!({"effect": "opacity", "shape": "rectangle", "center": [160.0 * zoom, 120.0 * zoom], "size": [20.0 * zoom, 60.0 * zoom]}))
            .unwrap();
        let r = s.execute("masks.track", json!({"direction": "forward", "method": "position", "frames": 40, "wait": true})).unwrap();
        assert!(r["status"]["result"]["error"].is_null(), "×{zoom}: {r}");
        let status = r["status"]["status"].as_str().unwrap();
        assert!(status.starts_with("Tracked 40 frame(s)") && !status.contains("not checked"), "×{zoom}: {status}");
        // the block has moved 80 px (at zoom 1) to the right, and the mask with it
        let (x0, y0, x1, y1) = mask_box(&mut s, 40);
        let (cx, cy) = ((x0 + x1) / 2.0 / zoom, (y0 + y1) / 2.0 / zoom);
        assert!((cx - 240.0).abs() < 2.0 && (cy - 120.0).abs() < 2.0, "×{zoom}: the mask is at {cx:.1}, {cy:.1}");
    }
}

/// Frames tracked while the mask may be coming off its subject are held back, not thrown away:
/// a run that ends before tracking finds the subject lost keeps them.
#[test]
fn frames_in_doubt_are_kept_when_the_run_ends_without_losing_the_subject() {
    let (mut s, _) = adrift_session();
    s.execute("masks.add", json!({"effect": "opacity", "shape": "rectangle", "center": [160, 120], "size": [60, 100]})).unwrap();
    // the whole clip loses the subject and keeps fewer frames than this run asks for
    let lost = s.execute("masks.track", json!({"direction": "forward", "method": "position", "wait": true})).unwrap()["status"]["done"].as_u64().unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    let frames = lost + 2;
    let r = s.execute("masks.track", json!({"direction": "forward", "method": "position", "frames": frames, "wait": true})).unwrap();
    assert!(r["status"]["result"]["error"].is_null(), "{r}");
    assert_eq!(r["status"]["done"], frames, "{r}");
    let l = s.execute("masks.list", json!({})).unwrap();
    assert_eq!(l["masks"][0]["pathKeyframes"].as_array().unwrap().len() as u64, frames + 1, "{l}");
}

/// A mask whose inside has the colours of what is around it has nothing to be checked against:
/// it is tracked as before, and the status says it was not checked.
#[test]
fn a_run_with_nothing_to_check_against_says_so() {
    let (mut s, _) = adrift_session();
    // water in water
    s.execute("masks.add", json!({"effect": "opacity", "shape": "rectangle", "center": [60, 60], "size": [60, 100]})).unwrap();
    let r = s.execute("masks.track", json!({"direction": "forward", "method": "position", "frames": 6, "wait": true})).unwrap();
    assert!(r["status"]["result"]["error"].is_null(), "{r}");
    let status = r["status"]["status"].as_str().unwrap();
    assert!(status.starts_with("Tracked 6 frame(s)") && status.contains("not checked against the first frame"), "{status}");
    // a mask with something of its own is checked, and says nothing of the kind
    let (mut s, _) = tracking_session();
    let c = truth(0.0).apply(filmcraft_geom::Vec2::ZERO);
    s.execute("masks.add", json!({"effect": "opacity", "shape": "ellipse", "center": [c.x, c.y], "size": [170, 170]})).unwrap();
    let r = s.execute("masks.track", json!({"direction": "forward", "frames": 6, "wait": true})).unwrap();
    let status = r["status"]["status"].as_str().unwrap();
    assert!(status.starts_with("Tracked 6 frame(s)") && !status.contains("not checked"), "{status}");
}

#[test]
fn tracking_runs_in_the_background_and_can_be_cancelled() {
    let (mut s, _) = tracking_session();
    let c = truth(0.0).apply(filmcraft_geom::Vec2::ZERO);
    s.execute("masks.add", json!({"effect": "opacity", "shape": "ellipse", "center": [c.x, c.y], "size": [90, 90]})).unwrap();
    let r = s.execute("masks.track", json!({"direction": "forward"})).unwrap();
    let job = r["job"].as_u64().unwrap();
    assert_eq!(r["frames"], 47);
    assert!(s.execute("masks.track", json!({})).is_err(), "one job per mask");
    s.execute("jobs.cancel", json!({"job": job})).unwrap();
    let t0 = std::time::Instant::now();
    while !s.mask_jobs.is_empty() && t0.elapsed().as_secs() < 60 {
        s.poll_persistence();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(s.mask_jobs.is_empty());
    let jobs = s.execute("jobs.list", json!({})).unwrap();
    let j = jobs.as_array().unwrap().iter().find(|j| j["id"] == job).unwrap().clone();
    assert_eq!(j["finished"], true);
    let n = s.execute("masks.list", json!({})).unwrap()["masks"][0]["pathKeyframes"].as_array().unwrap().len();
    assert!(n < 48, "stopped early ({n} keyframes)");
    assert!(s.execute("masks.track", json!({"direction": "backward"})).is_err(), "nothing before the first frame");
}

/// #813: a rectangle mask with rounded corners (created rounded, or rounded afterwards) cuts the
/// clip's corners away while the edges stay where they were; one undo step, every keyframe.
#[test]
fn rounded_rectangle_masks_cut_their_corners() {
    let (mut s, clip) = demo();
    // a 960×540 opacity mask in the middle of the 1920×1080 frame, rounded by 200 px
    s.execute("masks.add", json!({"clip": clip.0, "shape": "rectangle", "center": [960, 540], "size": [960, 540], "cornerRadius": 200})).unwrap();
    s.execute("masks.set", json!({"feather": 0})).unwrap();
    let list = s.execute("masks.list", json!({})).unwrap();
    assert_eq!(list["masks"][0]["path"]["vertices"].as_array().unwrap().len(), 8);
    let img = s.render_program(0.25).unwrap();
    // just inside the rectangle's corner: cut away; middle of an edge and the centre: kept
    let corner = px(&img, 0.26, 0.26);
    assert!(corner[0] + corner[1] + corner[2] < 1e-4, "rounded corner cut: {corner:?}");
    assert!(px(&img, 0.5, 0.26)[3] > 0.99, "top edge kept");
    assert!(px(&img, 0.5, 0.5)[3] > 0.99, "inside kept");

    // round an existing 4-point mask: same picture, one undo step back to sharp corners
    s.undo();
    s.undo();
    s.execute("masks.add", json!({"clip": clip.0, "shape": "polygon", "center": [960, 540], "size": [960, 540]})).unwrap();
    s.execute("masks.set", json!({"feather": 0})).unwrap();
    let sharp = s.render_program(0.25).unwrap();
    assert!(px(&sharp, 0.26, 0.26)[3] > 0.99, "a sharp corner keeps the clip");
    let r = s.execute("masks.roundCorners", json!({"radius": 200})).unwrap();
    assert_eq!(r["corners"], 4);
    assert_eq!(s.render_program(0.25).unwrap().px, img.px, "rounding afterwards = creating rounded");
    // nothing left to round; bad radii are refused without changing the mask
    assert!(s.execute("masks.roundCorners", json!({"radius": 50})).is_err());
    for radius in [json!(0), json!(-5), json!("big"), json!(null)] {
        assert!(s.execute("masks.roundCorners", json!({"radius": radius})).is_err(), "{radius}");
    }
    s.undo();
    assert_eq!(s.render_program(0.25).unwrap().px, sharp.px, "one undo step restores the sharp corners");
}

#[test]
fn rounding_an_animated_mask_rounds_every_keyframe_and_only_the_chosen_corners() {
    let (mut s, clip) = demo();
    let it = s.active_sequence().unwrap().find_item(clip).unwrap().1.clone();
    s.set_playhead(it.start);
    s.execute("masks.add", json!({"effect": "opacity", "shape": "polygon", "center": [400, 400], "size": [200, 200]})).unwrap();
    s.execute("effects.toggleAnimation", json!({"clip": clip.0, "effect": "opacity", "mask": 0, "param": "path"})).unwrap();
    let rate = s.active_sequence().unwrap().settings.frame_rate;
    s.set_playhead(it.start + rate.tick_of(10));
    s.execute("masks.translate", json!({"delta": [300, 0]})).unwrap();
    // only vertices 0 and 2 (top-left, bottom-right), out-of-range indices ignored
    let r = s.execute("masks.roundCorners", json!({"radius": 30, "vertices": [0, 2, 99]})).unwrap();
    assert_eq!(r["corners"], 2);
    let m = &s.active_sequence().unwrap().find_item(clip).unwrap().1.effects.iter().find(|e| e.effect == "opacity").unwrap().masks[0];
    assert_eq!(m.path.keyframes.len(), 2);
    for k in &m.path.keyframes {
        let filmcraft_project::ParamValue::Path(p) = &k.value else { panic!("path keyframe") };
        assert_eq!(p.len(), 6, "both keyframes rounded the same corners");
        assert_eq!(p.vertices.iter().filter(|v| v.is_corner()).count(), 2, "vertices 1 and 3 stay sharp");
    }
    // halfway the path still interpolates (equal vertex counts)
    let l = s.execute("masks.list", json!({"time": (it.start + rate.tick_of(5)).0})).unwrap();
    let v = l["masks"][0]["path"]["vertices"].as_array().unwrap();
    assert_eq!(v.len(), 6);
    assert!((v[0]["p"][0].as_f64().unwrap() - 450.0).abs() < 1e-6, "{}", v[0]);
}

/// `path_to_json` → `path_from_json` keeps the Bézier tangents (they used to come back as zero, so
/// a pen mask's dragged-out curves were committed as corners; #853).
#[test]
fn path_json_round_trip_keeps_tangents() {
    use filmcraft_geom::Vec2;
    use filmcraft_project::mask::{MaskPath, MaskVertex};
    let path = MaskPath {
        vertices: vec![
            MaskVertex { p: Vec2::new(10.0, 20.0), t_in: Vec2::new(-5.0, 0.0), t_out: Vec2::new(5.0, 0.0) },
            MaskVertex::corner(Vec2::new(80.0, 20.0)),
            MaskVertex { p: Vec2::new(40.0, 90.0), t_in: Vec2::new(3.0, -4.0), t_out: Vec2::new(-3.0, 4.0) },
        ],
        closed: true,
    };
    assert_eq!(crate::masks::path_from_json(&crate::masks::path_to_json(&path)), Some(path.clone()));
    // the serialized project form still parses
    assert_eq!(crate::masks::path_from_json(&serde_json::to_value(&path).unwrap()), Some(path));
}

/// `masks.moveVertex` with `vertices` moves several points by the same delta in one edit (the
/// Program monitor's multi-vertex drag, #3150); a bad index leaves every vertex where it was.
#[test]
fn move_vertex_moves_a_list_of_vertices_together() {
    let (mut s, _) = demo();
    s.execute("masks.add", json!({"effect": "opacity", "shape": "polygon", "center": [400, 400], "size": [200, 200]})).unwrap();
    let pts = |s: &mut Session| -> Vec<(f64, f64)> {
        let l = s.execute("masks.list", json!({})).unwrap();
        l["masks"][0]["path"]["vertices"].as_array().unwrap().iter().map(|v| (v["p"][0].as_f64().unwrap(), v["p"][1].as_f64().unwrap())).collect()
    };
    let before = pts(&mut s);
    let n = s.history.undo.len();
    // two frames of one drag, a vertex listed twice moves once
    for _ in 0..2 {
        s.execute("masks.moveVertex", json!({"vertices": [0, 2, 2], "delta": [10, -5], "merge": "vertex-1"})).unwrap();
    }
    assert_eq!(s.history.undo.len(), n + 1, "one undo step for the drag");
    let after = pts(&mut s);
    for i in [0, 2] {
        assert_eq!(after[i], (before[i].0 + 20.0, before[i].1 - 10.0), "vertex {i}");
    }
    for i in [1, 3] {
        assert_eq!(after[i], before[i], "vertex {i} stays");
    }
    // an index past the last vertex moves nothing
    let e = s.execute("masks.moveVertex", json!({"vertices": [1, 9], "delta": [10, 0]})).unwrap_err().to_string();
    assert!(e.contains("no vertex 9"), "{e}");
    assert_eq!(pts(&mut s), after);
    // a list moves points, by a delta
    assert!(s.execute("masks.moveVertex", json!({"vertices": [1], "to": [0, 0]})).is_err());
    assert!(s.execute("masks.moveVertex", json!({"vertices": [1], "handle": "in", "delta": [1, 0]})).is_err());
    assert!(s.execute("masks.moveVertex", json!({"vertices": "1", "delta": [1, 0]})).is_err());
    s.undo();
    assert_eq!(pts(&mut s), before);
}

#[test]
fn background_tracking_is_applied_without_a_ui_frame_loop() {
    // headless sessions (CLI, MCP) never call `poll_persistence`: commands alone must apply the job (#3302)
    let (mut s, _) = tracking_session();
    let c = truth(0.0).apply(filmcraft_geom::Vec2::ZERO);
    s.execute("masks.add", json!({"effect": "opacity", "shape": "ellipse", "center": [c.x, c.y], "size": [90, 90]})).unwrap();
    let r = s.execute("masks.track", json!({"direction": "forward", "frames": 6})).unwrap();
    let job = r["job"].clone();
    let t0 = std::time::Instant::now();
    loop {
        let jobs = s.execute("jobs.list", json!({})).unwrap();
        let done = jobs.as_array().unwrap().iter().any(|j| j["id"] == job && j["finished"] == true);
        if done || t0.elapsed().as_secs() > 60 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let l = s.execute("masks.list", json!({})).unwrap();
    let keys = l["masks"][0]["pathKeyframes"].as_array().unwrap().len();
    assert_eq!(keys, 7, "{l}");
    assert!(s.mask_jobs.is_empty());
    s.execute("masks.track", json!({"direction": "forward", "frames": 2, "wait": true})).unwrap();
}

#[test]
fn replacing_the_project_drops_pending_tracking_jobs() {
    // a job started on the old project must not write into the new one
    let (mut s, _) = tracking_session();
    let c = truth(0.0).apply(filmcraft_geom::Vec2::ZERO);
    s.execute("masks.add", json!({"effect": "opacity", "shape": "ellipse", "center": [c.x, c.y], "size": [90, 90]})).unwrap();
    let job = s.execute("masks.track", json!({"direction": "forward"})).unwrap()["job"].clone();
    assert_eq!(s.mask_jobs.len(), 1);
    s.execute("file.newProject", json!({"name": "Fresh"})).unwrap();
    assert!(s.mask_jobs.is_empty(), "stale job survived file.newProject");
    for _ in 0..20 {
        s.poll_persistence();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(s.mask_jobs.is_empty());
    let h = s.execute("history.list", json!({})).unwrap();
    assert!(!h["undo"].as_array().unwrap().iter().any(|v| v == "Track Mask"), "{h}");
    // the new project has no masks and no Mask Path keyframes
    if let Ok(l) = s.execute("masks.list", json!({})) {
        assert!(l["masks"].as_array().is_none_or(|m| m.is_empty()), "{l}");
    }
    let project = serde_json::to_string(&*s.project).unwrap().to_lowercase();
    assert!(!project.contains("mask"), "stale mask data in new project");
    // the discarded job was told to stop (the worker may not have noticed yet on a slow machine,
    // so its `finished` flag is not waited for)
    let j = s.jobs.iter().find(|j| json!(j.id) == job).expect("the tracking job is listed");
    assert!(j.progress.cancel.load(std::sync::atomic::Ordering::Relaxed), "{}", j.to_json());
}

#[test]
fn every_project_replacing_path_drops_pending_tracking_jobs() {
    // the epoch check covers paths that never call `discard_jobs`: recover (`install_project`), demo, close, template
    let tpl = std::env::temp_dir().join(format!("filmcraft-mask-epoch-{}.fcproj", std::process::id())).to_string_lossy().to_string();
    for path in ["recover", "demo", "close", "template"] {
        let (mut s, _) = tracking_session();
        s.execute("file.saveAsTemplate", json!({"path": tpl})).unwrap();
        let c = truth(0.0).apply(filmcraft_geom::Vec2::ZERO);
        s.execute("masks.add", json!({"effect": "opacity", "shape": "ellipse", "center": [c.x, c.y], "size": [90, 90]})).unwrap();
        s.execute("masks.track", json!({"direction": "forward"})).unwrap();
        assert_eq!(s.mask_jobs.len(), 1);
        match path {
            "recover" => crate::commands::install_project(&mut s, filmcraft_project::Project::new("Recovered"), None, false),
            "demo" => drop(s.execute("file.openDemoProject", json!({})).unwrap()),
            "close" => drop(s.execute("file.closeProject", json!({"force": true})).unwrap()),
            _ => drop(s.execute("file.newProjectFromTemplate", json!({"template": tpl, "force": true})).unwrap()),
        }
        for _ in 0..20 {
            s.poll_persistence();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(s.mask_jobs.is_empty(), "stale job survived {path}");
        let h = s.execute("history.list", json!({})).unwrap();
        assert!(!h["undo"].as_array().unwrap().iter().any(|v| v == "Track Mask"), "{path}: {h}");
        let project = serde_json::to_string(&*s.project).unwrap();
        assert!(!project.contains("pathKeyframes") && !project.to_lowercase().contains("\"masks\":[{"), "{path}: stale mask data");
    }
    let _ = std::fs::remove_file(&tpl);
}
