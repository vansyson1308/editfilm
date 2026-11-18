//! Sports tools on synthetic footage: a dot or a block moving over a dark, slightly grainy floor.

use std::sync::Arc;

use filmagic_geom::Vec2;
use filmagic_project::{ClipId, ItemId};
use filmagic_time::{FrameRate, Tick};
use serde_json::{Value, json};

use crate::Session;

const RATE: FrameRate = FrameRate::FPS_24;

/// What moves: centre (clip pixels) and radius at frame `k`, or nothing.
type Mover = fn(f64) -> Option<(f64, f64, f64)>;

struct Scene {
    info: filmagic_media::MediaInfo,
    w: u32,
    h: u32,
    mover: Mover,
    /// square instead of a disc
    square: bool,
    /// per-frame sensor noise (0..1 of the encoded value)
    noise: f64,
    /// sleep per frame, for the background-job tests
    slow: Option<std::time::Duration>,
}

fn hash(x: u64) -> f64 {
    let mut z = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
}

impl filmagic_media::MediaSource for Scene {
    fn info(&self) -> &filmagic_media::MediaInfo {
        &self.info
    }
    fn video_frame(&self, req: filmagic_media::FrameRequest) -> filmagic_media::Result<Arc<filmagic_frame::VideoFrame>> {
        if let Some(d) = self.slow {
            std::thread::sleep(d);
        }
        let frame = RATE.frame_at(req.time);
        let k = frame as f64;
        let scale = if req.scale > 0.0 && req.scale <= 1.0 { f64::from(req.scale) } else { 1.0 };
        let (w, h) = ((f64::from(self.w) * scale).round() as usize, (f64::from(self.h) * scale).round() as usize);
        let per = w as f64 / f64::from(self.w);
        let m = (self.mover)(k);
        let mut px = vec![255u8; w * h * 4];
        for (i, o) in px.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let (x, y) = (((i % w) as f64 + 0.5) / per, ((i / w) as f64 + 0.5) / per);
            let grain = 0.03 * ((x * 0.31).sin() * (y * 0.17).cos());
            let noise = if self.noise > 0.0 { self.noise * (hash((frame as u64) << 32 | i as u64) - 0.5) } else { 0.0 };
            let mut v = [0.18 + grain + noise, 0.2 + grain + noise, 0.22 + grain + noise];
            if let Some((cx, cy, r)) = m {
                let inside = if self.square { (x - cx).abs() <= r && (y - cy).abs() <= r } else { (x - cx).powi(2) + (y - cy).powi(2) <= r * r };
                if inside {
                    // a little pattern of its own, so a box around it has detail
                    let t = 0.85 + 0.1 * ((x - cx) * 0.9).sin() * ((y - cy) * 0.7).cos();
                    v = [t, t, t * 0.95];
                }
            }
            for (o, c) in o.iter_mut().zip(v) {
                *o = (c.clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        }
        Ok(Arc::new(filmagic_frame::VideoFrame::rgba8(w as u32, h as u32, px)))
    }
    fn audio(&self, _start: i64, frames: usize, sample_rate: u32) -> filmagic_media::Result<filmagic_frame::AudioBuffer> {
        Ok(filmagic_frame::AudioBuffer::silence(sample_rate, 2, frames))
    }
}

struct Setup {
    w: u32,
    h: u32,
    seq: (u32, u32),
    frames: i64,
    mover: Mover,
    square: bool,
    noise: f64,
    slow: Option<std::time::Duration>,
}

impl Setup {
    fn new(w: u32, h: u32, frames: i64, mover: Mover) -> Setup {
        Setup { w, h, seq: (w, h), frames, mover, square: false, noise: 0.0, slow: None }
    }

    /// A session with the scene as one clip alone on V1 of a sequence `seq` big, selected.
    fn session(self) -> (Session, ClipId, ItemId) {
        use filmagic_media::MediaSource;
        let Setup { w, h, seq, frames, mover, square, noise, slow } = self;
        let g = filmagic_media::generators::GeneratorSource::new(
            filmagic_media::Generator::ColorMatte { color: [0.5, 0.5, 0.5, 1.0] },
            w,
            h,
            RATE,
            RATE.tick_of(frames),
        );
        let info = g.info().clone();
        let mut p = filmagic_project::Project::new("sports");
        let item = p.add_item(
            "match",
            filmagic_project::Label::Iris,
            filmagic_project::ItemKind::Media(Box::new(filmagic_project::MediaClip {
                media: filmagic_project::MediaRef::Generator(g.generator.clone()),
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
        let sid =
            p.new_sequence("Match", filmagic_project::SequenceSettings { width: seq.0, height: seq.1, frame_rate: RATE, ..Default::default() }, 1, 0, None);
        let mut ti = p
            .make_track_item(item, filmagic_project::TrackKind::Video, Tick::ZERO, filmagic_time::TimeRange::new(Tick::ZERO, RATE.tick_of(frames)), RATE)
            .unwrap();
        for e in &mut ti.effects {
            filmagic_project::resolve_auto_points(e, seq, (w, h));
        }
        let clip = ti.id;
        p.sequence_mut(sid).unwrap().video_tracks[0].items.push(ti);
        let mut s = Session { project: Arc::new(p), ..Default::default() };
        s.state.active_sequence = Some(sid);
        s.state.selection = vec![clip];
        s.media.insert(item, Arc::new(Scene { info, w, h, mover, square, noise, slow }));
        (s, clip, sid)
    }
}

/// A disc sliding left to right across a 640×360 frame over 2 s.
fn sliding(k: f64) -> Option<(f64, f64, f64)> {
    Some((100.0 + 9.0 * k, 180.0, 22.0))
}

/// A small ball on a lob across a 320×180 frame.
fn lob(k: f64) -> Option<(f64, f64, f64)> {
    Some((30.0 + 6.0 * k, 150.0 - 7.0 * k + 0.18 * k * k, 3.5))
}

fn still(_: f64) -> Option<(f64, f64, f64)> {
    Some((320.0, 180.0, 30.0))
}

fn nothing(_: f64) -> Option<(f64, f64, f64)> {
    None
}

fn item_at(s: &Session, seq: ItemId, clip: ClipId) -> filmagic_project::TrackItem {
    s.project.sequence(seq).unwrap().find_item(clip).unwrap().1.clone()
}

/// Where source point `p` lands in the sequence frame at frame `k`.
fn on_screen(s: &Session, seq: ItemId, clip: ClipId, k: i64, p: Vec2) -> Vec2 {
    let q = s.project.sequence(seq).unwrap();
    let it = q.find_item(clip).unwrap().1;
    let src = s.project.source_size(it.item).unwrap();
    let t = RATE.tick_of(k);
    filmagic_render::motion_matrix(q, it, src, s.project.source_par(it.item), it.effect_time_at(t)).apply(p)
}

fn project_json(s: &Session) -> String {
    serde_json::to_string(&*s.project).unwrap()
}

#[test]
fn reframe_to_a_new_vertical_sequence_follows_a_moving_subject() {
    let (mut s, clip, seq) = Setup::new(640, 360, 48, sliding).session();
    let r = s.execute("clip.autoReframe", json!({"output": "sequence", "aspect": "9:16", "wait": true})).unwrap();
    assert_eq!(r["pictureChange"]["reads"], "strong", "{r}");
    let new = ItemId(r["sequence"]["id"].as_u64().unwrap());
    assert_eq!((r["sequence"]["width"].as_u64(), r["sequence"]["height"].as_u64()), (Some(360), Some(640)), "{r}");
    let c = &r["clips"][0];
    assert!(c["coverage"].as_f64().unwrap() > 0.9, "{c}");
    assert!(c["inFrame"].as_f64().unwrap() > 0.9, "{c}");
    // the copy is the reframed one; the source sequence is untouched
    let copy = s.project.sequence(new).unwrap().video_tracks[0].items[0].clone();
    assert_ne!(copy.id, clip, "the copy has its own id");
    assert!(item_at(&s, seq, clip).effect("motion").unwrap().param("position").unwrap().keyframes.is_empty());
    // the disc stays well inside the vertical frame from the first frame to the last
    for k in (0..48).step_by(4) {
        let (x, y, _) = sliding(k as f64).unwrap();
        let p = on_screen(&s, new, copy.id, k, Vec2::new(x, y));
        assert!(p.x > 0.2 * 360.0 && p.x < 0.8 * 360.0, "frame {k}: the subject is at x {:.1} of 360", p.x);
        assert!((p.y - 320.0).abs() < 1.0, "frame {k}: y {:.1}", p.y);
    }
    // one undo step
    s.undo();
    assert!(s.project.sequence(new).is_none());
}

#[test]
fn reframe_keyframes_in_a_vertical_sequence_follow_a_seeded_subject() {
    let mut setup = Setup::new(640, 360, 48, sliding);
    setup.seq = (360, 640);
    setup.square = true;
    let (mut s, clip, seq) = setup.session();
    let (x0, y0, _) = sliding(0.0).unwrap();
    // at frame 0 the clip fills the 360-wide frame's height: where the disc shows in the sequence
    let seed = on_screen(&s, seq, clip, 0, Vec2::new(x0, y0));
    let before = item_at(&s, seq, clip);
    let fit = before.effect("motion").unwrap().f64_at("scale", Tick::ZERO);
    let r = s
        .execute("clip.autoReframe", json!({"subject": {"box": [seed.x - 22.0 * fit / 100.0, seed.y - 22.0 * fit / 100.0, 44.0 * fit / 100.0, 44.0 * fit / 100.0], "at": {"frame": 0}}, "smoothing": 0.2, "wait": true}))
        .unwrap();
    let c = &r["clips"][0];
    assert!(c["coverage"].as_f64().unwrap() > 0.95, "{c}");
    assert!(c["keyframes"].as_u64().unwrap() >= 2, "{c}");
    for k in (0..48).step_by(3) {
        let (x, y, _) = sliding(k as f64).unwrap();
        let p = on_screen(&s, seq, clip, k, Vec2::new(x, y));
        assert!((p.x - 180.0).abs() < 0.3 * 360.0, "frame {k}: x {:.1}", p.x);
        assert!((p.y - 320.0).abs() < 1.0, "frame {k}: y {:.1}", p.y);
    }
    // the picture fills the frame at every sample: the crop never leaves the source
    let it = item_at(&s, seq, clip);
    for k in 0..48 {
        let tl = on_screen(&s, seq, clip, k, Vec2::ZERO);
        let br = on_screen(&s, seq, clip, k, Vec2::new(640.0, 360.0));
        assert!(tl.x <= 0.5 && tl.y <= 0.5 && br.x >= 359.5 && br.y >= 639.5, "frame {k}: {tl:?}–{br:?}");
    }
    assert!(it.effect("motion").unwrap().param("position").unwrap().is_animated());
}

#[test]
fn reframe_of_a_still_noisy_scene_does_not_jitter() {
    let mut setup = Setup::new(640, 360, 48, still);
    setup.noise = 0.02;
    let (mut s, clip, seq) = setup.session();
    let r = s.execute("clip.autoReframe", json!({"output": "sequence", "wait": true})).unwrap();
    let new = ItemId(r["sequence"]["id"].as_u64().unwrap());
    let copy = s.project.sequence(new).unwrap().video_tracks[0].items[0].clone();
    let pos = copy.effect("motion").unwrap().param("position").unwrap().clone();
    assert!(pos.keyframes.len() <= 1, "a still scene needs no keyframes: {:?}", pos.keyframes.len());
    let first = on_screen(&s, new, copy.id, 0, Vec2::new(320.0, 180.0));
    for k in 1..48 {
        let p = on_screen(&s, new, copy.id, k, Vec2::new(320.0, 180.0));
        assert!((p - first).length() < 1e-6, "frame {k} moved by {:?}", p - first);
    }
    let _ = (clip, seq);
}

#[test]
fn reframe_with_nothing_moving_stays_centred_and_says_so() {
    let (mut s, _, _) = Setup::new(640, 360, 24, nothing).session();
    let r = s.execute("clip.autoReframe", json!({"output": "sequence", "aspect": "1:1", "wait": true})).unwrap();
    assert_eq!((r["sequence"]["width"].as_u64(), r["sequence"]["height"].as_u64()), (Some(360), Some(360)));
    let notes = r["clips"][0]["notes"].to_string();
    assert!(notes.contains("centred"), "{r}");
}

#[test]
fn reframe_refuses_hostile_parameters_and_changes_nothing() {
    let (mut s, clip, _) = Setup::new(640, 360, 24, sliding).session();
    let before = project_json(&s);
    let bad = [
        json!({"smoothing": "a lot"}),
        json!({"smoothing": 1e300}),
        json!({"deadZone": -1}),
        json!({"maxPan": 0}),
        json!({"zoom": 1e9}),
        json!({"aspect": "0:16"}),
        json!({"aspect": "16"}),
        json!({"aspect": 0.5625}),
        json!({"aspect": "1e400:1"}),
        json!({"aspect": "9:16"}),
        json!({"output": "film"}),
        json!({"output": "sequence", "name": ""}),
        json!({"subject": 7}),
        json!({"subject": {"point": [1e300, 0]}}),
        json!({"subject": {"point": [10]}}),
        json!({"subject": {"box": [0, 0, -5, 5]}}),
        json!({"subject": {"point": [5000, 5000], "at": 0}}),
        json!({"subject": {"point": [100, 100], "at": {"seconds": 99}}}),
        json!({"clip": 999_999}),
        json!({"clip": "first"}),
        json!({"sampleEvery": 0}),
        json!({"sampleEvery": "often"}),
        json!({"start": {"seconds": 0.5}, "end": {"seconds": 0.2}}),
        json!({"start": {"seconds": 50}}),
        json!({"start": -5}),
        json!({"wait": "yes"}),
        json!({"output": "sequence", "clips": "all"}),
        json!({"output": "sequence", "clips": [clip.0, 424_242]}),
        json!([1, 2]),
    ];
    for p in bad {
        let r = s.execute("clip.autoReframe", json!(p));
        assert!(r.is_err(), "{p} was accepted: {r:?}");
        assert_eq!(project_json(&s), before, "{p} changed the project");
    }
    assert!(s.sports_jobs.is_empty() && s.jobs.is_empty(), "a refused call starts no job");
}

#[test]
fn reframe_runs_as_a_job_that_can_be_cancelled_or_applied_later() {
    let mut setup = Setup::new(640, 360, 48, sliding);
    setup.slow = Some(std::time::Duration::from_millis(20));
    let (mut s, _, _) = setup.session();
    let before = project_json(&s);
    let r = s.execute("clip.autoReframe", json!({"output": "sequence"})).unwrap();
    let job = r["job"].as_u64().unwrap();
    assert_eq!(r["poll"], "jobs.list");
    s.execute("jobs.cancel", json!({"job": job})).unwrap();
    wait_finished(&s, job);
    s.execute("jobs.list", json!({})).unwrap();
    assert_eq!(project_json(&s), before, "a cancelled reframe changes nothing");
    assert!(s.sports_jobs.is_empty());

    // left to finish, it is applied before the next command and reported in jobs.list
    let r = s.execute("clip.autoReframe", json!({"output": "sequence"})).unwrap();
    let job = r["job"].as_u64().unwrap();
    wait_finished(&s, job);
    let list = s.execute("jobs.list", json!({})).unwrap();
    let entry = list.as_array().unwrap().iter().find(|j| j["id"] == job).cloned().unwrap();
    assert!(entry["report"]["sequence"]["id"].as_u64().is_some(), "{entry}");
    assert_ne!(project_json(&s), before);
}

fn wait_finished(s: &Session, job: u64) {
    let t0 = std::time::Instant::now();
    while !s.jobs.iter().find(|j| j.id == job).is_some_and(|j| j.progress.finished.load(std::sync::atomic::Ordering::Relaxed)) {
        assert!(t0.elapsed().as_secs() < 60, "the job did not finish");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn track_point_follows_a_small_ball() {
    let (mut s, _, _) = Setup::new(320, 180, 30, lob).session();
    let (x, y, _) = lob(0.0).unwrap();
    let r = s.execute("sports.trackPoint", json!({"point": [x, y], "size": 10, "at": 0, "end": {"frame": 30}, "wait": true})).unwrap();
    assert_eq!(r["coverage"], 1.0, "{r}");
    let pts = r["points"].as_array().unwrap();
    assert_eq!(pts.len(), 30);
    for (k, p) in pts.iter().enumerate() {
        let (x, y, _) = lob(k as f64).unwrap();
        let (px, py) = (p["x"].as_f64().unwrap(), p["y"].as_f64().unwrap());
        assert!((px - x).abs() < 1.5 && (py - y).abs() < 1.5, "frame {k}: ({px:.1}, {py:.1}) vs ({x:.1}, {y:.1})");
    }
    assert!(r.get("pictureChange").is_none(), "tracking changes nothing");
}

/// Linear RGB of the Program monitor at sequence pixel (x, y) of a `w`-wide frame rendered at 1.
fn pixel(img: &filmagic_render::Image, x: f64, y: f64) -> [f32; 4] {
    img.get((x as usize).min(img.w - 1), (y as usize).min(img.h - 1))
}

fn differs(a: [f32; 4], b: [f32; 4]) -> f32 {
    (0..3).map(|k| (a[k] - b[k]).abs()).fold(0.0, f32::max)
}

#[test]
fn ball_trail_pixels_land_behind_the_ball() {
    let (mut s, _, seq) = Setup::new(320, 180, 30, lob).session();
    let (x, y, _) = lob(0.0).unwrap();
    let r = s
        .execute("sports.ballTrail", json!({"point": [x, y], "size": 10, "at": 0, "end": {"frame": 30}, "length": 8, "width": 6, "glow": false, "color": "#FF2020", "opacity": 100, "fade": 0, "wait": true}))
        .unwrap();
    assert!(matches!(r["pictureChange"]["reads"].as_str(), Some("visible" | "strong")), "{r}");
    assert_eq!(r["layers"], 8, "{r}");
    let g = ClipId(r["clip"].as_u64().unwrap());
    let it = item_at(&s, seq, g);
    assert_eq!(it.graphic_layers().count(), 8, "the trail is made of editable shape layers");
    let k = 20;
    s.set_playhead(RATE.tick_of(k));
    let img = s.render_program(1.0).unwrap();
    // between where the ball was 3 and 4 frames ago: the trail's red
    let (a, b) = (lob((k - 3) as f64).unwrap(), lob((k - 4) as f64).unwrap());
    let mid = pixel(&img, (a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
    assert!(mid[0] > 0.6 && mid[1] < 0.1 && mid[2] < 0.1, "trail pixel {mid:?}");
    // ahead of the ball, and far from the path: the floor
    let (n, _, _) = lob((k + 4) as f64).unwrap();
    let ahead = pixel(&img, n, lob((k + 4) as f64).unwrap().1);
    assert!(ahead[0] < 0.2, "ahead of the ball {ahead:?}");
    let far = pixel(&img, 300.0, 170.0);
    assert!(far[0] < 0.2, "{far:?}");
    // the new graphic is selected; a second overlay still follows the footage, not the graphic
    assert!(s.state.selection.contains(&g));
    let r = s.execute("sports.trackPoint", json!({"point": [x, y], "size": 10, "at": 0, "end": {"frame": 5}, "wait": true})).unwrap();
    assert_eq!(r["coverage"], 1.0, "{r}");
    // undo removes it in one step
    s.undo();
    assert!(s.project.sequence(seq).unwrap().find_item(g).is_none());
}

#[test]
fn ground_ring_and_spotlight_follow_given_points() {
    let (mut s, _, seq) = Setup::new(320, 180, 24, still).session();
    let plain = {
        s.set_playhead(RATE.tick_of(12));
        s.render_program(1.0).unwrap()
    };
    let pts: Vec<Value> = (0..24).map(|k| json!({"time": {"frame": k}, "x": 100.0 + 4.0 * k as f64, "y": 120.0})).collect();
    let r = s
        .execute(
            "sports.groundRing",
            json!({"points": pts, "diameter": 60, "squash": 0.4, "width": 4, "fill": 0, "glow": false, "color": "#00FF00", "spotlight": 70}),
        )
        .unwrap();
    assert!(matches!(r["pictureChange"]["reads"].as_str(), Some("visible" | "strong")), "{r}");
    assert!(r["spotlightClip"].as_u64().is_some(), "{r}");
    let img = s.render_program(1.0).unwrap();
    let cx = 100.0 + 4.0 * 12.0;
    // the ring's right edge, the ellipse squashed to 24 px high
    let edge = pixel(&img, cx + 30.0, 120.0);
    assert!(edge[1] > 0.5 && edge[0] < 0.3, "ring {edge:?}");
    // its top is 12 px up, not 30
    let top = pixel(&img, cx, 120.0 - 12.0);
    assert!(top[1] > 0.5, "squashed ring top {top:?}");
    // far from the player: darker; in the spot above the ring: unchanged
    let far = (pixel(&img, 300.0, 20.0), pixel(&plain, 300.0, 20.0));
    assert!(far.0[0] < 0.5 * far.1[0], "darkened {far:?}");
    let spot = (pixel(&img, cx, 120.0 - 66.0), pixel(&plain, cx, 120.0 - 66.0));
    assert!(differs(spot.0, spot.1) < 0.02, "inside the spot {spot:?}");
    // one undo removes both clips
    let n = s.project.sequence(seq).unwrap().video_tracks.iter().map(|t| t.items.len()).sum::<usize>();
    assert_eq!(n, 3);
    s.undo();
    let n = s.project.sequence(seq).unwrap().video_tracks.iter().map(|t| t.items.len()).sum::<usize>();
    assert_eq!(n, 1);
}

#[test]
fn path_line_draws_an_arc_on_a_freeze_frame() {
    let (mut s, _, _) = Setup::new(320, 180, 48, nothing).session();
    // points on y = 50 + (x − 160)² / 144
    let pts = json!([[40, 150], [100, 75], [160, 50], [220, 75], [280, 150]]);
    let r = s
        .execute("sports.pathLine", json!({"points": pts, "curve": "arc", "time": {"frame": 24}, "duration": {"seconds": 1}, "width": 5, "glow": false, "color": "#2040FF", "arrow": true}))
        .unwrap();
    assert!(matches!(r["pictureChange"]["reads"].as_str(), Some("visible" | "strong")), "{r}");
    assert!(r["layers"].as_u64().unwrap() >= 3, "{r}");
    s.set_playhead(RATE.tick_of(30));
    let img = s.render_program(1.0).unwrap();
    // the fitted parabola passes through the apex
    let apex = pixel(&img, 160.0, 50.0);
    assert!(apex[2] > 0.5 && apex[0] < 0.2, "apex {apex:?}");
    let off = pixel(&img, 160.0, 120.0);
    assert!(off[2] < 0.3, "below the arc {off:?}");
    // before `time` there is no line
    s.set_playhead(RATE.tick_of(10));
    let img = s.render_program(1.0).unwrap();
    assert!(pixel(&img, 160.0, 50.0)[2] < 0.3);
}

#[test]
fn overlays_refuse_hostile_parameters_and_change_nothing() {
    let (mut s, _, _) = Setup::new(320, 180, 30, lob).session();
    let before = project_json(&s);
    let two = json!([{"time": 0, "x": 1, "y": 1}, {"time": {"frame": 2}, "x": 9, "y": 9}]);
    let cases: Vec<(&str, Value)> = vec![
        ("sports.ballTrail", json!({})),
        ("sports.ballTrail", json!({"point": "middle"})),
        ("sports.ballTrail", json!({"point": [1e300, 2]})),
        ("sports.ballTrail", json!({"point": [5000, 10], "at": 0})),
        ("sports.ballTrail", json!({"box": [10, 10, -4, 4], "at": 0})),
        ("sports.ballTrail", json!({"box": [10, 10, 4], "at": 0})),
        ("sports.ballTrail", json!({"point": [30, 150], "at": {"seconds": 99}})),
        ("sports.ballTrail", json!({"point": [30, 150], "start": {"frame": 10}, "end": {"frame": 5}})),
        ("sports.ballTrail", json!({"point": [30, 150], "clip": 777})),
        ("sports.ballTrail", json!({"point": [30, 150], "length": 61})),
        ("sports.ballTrail", json!({"point": [30, 150], "width": -1})),
        ("sports.ballTrail", json!({"point": [30, 150], "color": "nope"})),
        ("sports.ballTrail", json!({"point": [30, 150], "size": 1e9})),
        ("sports.ballTrail", json!({"point": [30, 150], "track": "top"})),
        ("sports.ballTrail", json!({"point": [30, 150], "wait": 1})),
        ("sports.ballTrail", json!({"points": [[1, 2], [3, 4]]})),
        ("sports.ballTrail", json!({"points": [{"time": 0, "x": 1, "y": 1}]})),
        ("sports.ballTrail", json!({"points": "many"})),
        ("sports.ballTrail", json!({"points": [{"time": 0, "x": 1, "y": 1}, {"time": {"seconds": 36000}, "x": 9, "y": 9}]})),
        ("sports.groundRing", json!({"points": two, "squash": 0})),
        ("sports.groundRing", json!({"points": two, "spotlight": 101})),
        ("sports.groundRing", json!({"points": two, "spotSize": [0, 5]})),
        ("sports.groundRing", json!({"points": two, "offset": [1]})),
        ("sports.pathLine", json!({"points": [[1, 1]]})),
        ("sports.pathLine", json!({"points": [[1, 1], [1, 1]]})),
        ("sports.pathLine", json!({"points": [[1, 1], ["a", 1]]})),
        ("sports.pathLine", json!({"points": [[1, 1], [9, 9]], "curve": "zigzag"})),
        ("sports.pathLine", json!({"points": [[1, 1], [9, 9]], "time": -3})),
        ("sports.trackPoint", json!({"points": two})),
        ("sports.trackPoint", json!({"point": [30, 150], "end": {"seconds": 1e6}, "start": 0, "at": 0, "clip": "x"})),
    ];
    for (cmd, p) in cases {
        let r = s.execute(cmd, p.clone());
        assert!(r.is_err(), "{cmd} {p} was accepted: {r:?}");
        assert_eq!(project_json(&s), before, "{cmd} {p} changed the project");
    }
    // too many points
    let many: Vec<Value> = (0..7201).map(|k| json!([k % 300, 5])).collect();
    assert!(s.execute("sports.pathLine", json!({"points": many})).is_err());
    assert!(s.jobs.is_empty(), "a refused call starts no job");
}

#[test]
fn tracking_a_plain_area_is_refused_with_advice() {
    let (mut s, _, _) = Setup::new(320, 180, 30, nothing).session();
    let e = s.execute("sports.trackPoint", json!({"point": [160, 90], "size": 6, "at": 0, "wait": true})).unwrap_err().to_string();
    assert!(e.contains("plain"), "{e}");
}

#[test]
fn overlays_follow_their_subject_into_a_reframed_sequence() {
    let (mut s, _, _) = Setup::new(640, 360, 48, sliding).session();
    let pts: Vec<Value> = (0..48).map(|k| json!({"time": {"frame": k}, "x": sliding(k as f64).unwrap().0, "y": 180 + 22})).collect();
    let ring = s.execute("sports.groundRing", json!({"points": pts, "diameter": 50, "glow": false})).unwrap();
    let g = ClipId(ring["clip"].as_u64().unwrap());
    let r = s.execute("clip.autoReframe", json!({"output": "sequence", "wait": true})).unwrap();
    let new = ItemId(r["sequence"]["id"].as_u64().unwrap());
    let q = s.project.sequence(new).unwrap();
    let footage = q.video_tracks[0].items[0].id;
    // the ring's copy: the graphic on V2 of the new sequence
    let ring_copy = q.video_tracks[1].items[0].id;
    assert_ne!(ring_copy, g);
    for k in (0..48).step_by(5) {
        let (x, y, _) = sliding(k as f64).unwrap();
        // where the disc's feet are in the new frame, through the footage's crop and through the
        // ring graphic's (its canvas is the old frame)
        let via_footage = on_screen(&s, new, footage, k, Vec2::new(x, y + 22.0));
        let via_graphic = on_screen(&s, new, ring_copy, k, Vec2::new(x, y + 22.0));
        assert!((via_footage - via_graphic).length() < 0.6, "frame {k}: {via_footage:?} vs {via_graphic:?}");
    }
    // with "keep", the graphic is left as it was
    let r = s.execute("clip.autoReframe", json!({"output": "sequence", "graphics": "keep", "wait": true})).unwrap();
    let kept = ItemId(r["sequence"]["id"].as_u64().unwrap());
    let q = s.project.sequence(kept).unwrap();
    let it = &q.video_tracks[1].items[0];
    assert!(!it.effect("motion").unwrap().param("position").unwrap().is_animated());
    assert!(s.execute("clip.autoReframe", json!({"output": "sequence", "graphics": "move"})).is_err());
}

/// A disc drifting slowly back and forth across a long clip.
fn drifting(k: f64) -> Option<(f64, f64, f64)> {
    Some((320.0 + 200.0 * (k / 150.0).sin(), 180.0, 22.0))
}

#[test]
fn a_graphic_follows_the_crop_over_a_clip_longer_than_the_sample_cap() {
    let (mut s, _, _) = Setup::new(640, 360, 4800, drifting).session();
    let pts: Vec<Value> = (0..4800).step_by(4).map(|k| json!({"time": {"frame": k}, "x": drifting(k as f64).unwrap().0, "y": 202})).collect();
    s.execute("sports.groundRing", json!({"points": pts, "diameter": 50, "glow": false})).unwrap();
    let r = s.execute("clip.autoReframe", json!({"output": "sequence", "subject": {"box": [298, 158, 44, 44], "at": 0}, "wait": true})).unwrap();
    assert!(r["clips"][0]["coverage"].as_f64().unwrap() > 0.9, "{r}");
    let new = ItemId(r["sequence"]["id"].as_u64().unwrap());
    let q = s.project.sequence(new).unwrap();
    let (footage, ring) = (q.video_tracks[0].items[0].id, q.video_tracks[1].items[0].id);
    for k in [100, 2000, 3900, 4100, 4500, 4700, 4795] {
        let (x, y, _) = drifting(k as f64).unwrap();
        let p = Vec2::new(x, y + 22.0);
        let (a, b) = (on_screen(&s, new, footage, k, p), on_screen(&s, new, ring, k, p));
        assert!((a - b).length() < 1.0, "frame {k}: {a:?} vs {b:?}");
    }
}

#[test]
fn a_path_line_from_untimed_points_starts_at_the_playhead_or_its_freeze_frame() {
    let (mut s, clip, seq) = Setup::new(320, 180, 240, nothing).session();
    s.set_playhead(RATE.tick_of(120));
    let pts = json!([[40, 150], [160, 50], [280, 150]]);
    let r = s.execute("sports.pathLine", json!({"points": pts, "duration": {"seconds": 1}})).unwrap();
    assert!((r["start"].as_f64().unwrap() - 5.0).abs() < 1e-9, "{r}");
    assert!((r["end"].as_f64().unwrap() - 6.0).abs() < 1e-9, "{r}");
    // over a freeze frame, the line starts with it and lasts as long by default
    std::sync::Arc::make_mut(&mut s.project).sequence_mut(seq).unwrap().find_item_mut(clip).unwrap().1.frame_hold = Some(RATE.tick_of(30));
    s.set_playhead(RATE.tick_of(200));
    let r = s.execute("sports.pathLine", json!({"points": pts})).unwrap();
    assert_eq!((r["start"].as_f64(), r["end"].as_f64()), (Some(0.0), Some(10.0)), "{r}");
    // timed points still go after the stretch they span
    let timed = json!([{"time": {"frame": 10}, "x": 40, "y": 150}, {"time": {"frame": 20}, "x": 280, "y": 150}]);
    let r = s.execute("sports.pathLine", json!({"points": timed, "duration": {"frame": 24}})).unwrap();
    assert!((r["start"].as_f64().unwrap() - 21.0 / 24.0).abs() < 1e-9, "{r}");
}
