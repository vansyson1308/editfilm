//! `layers.*` (a region of a clip as a layer of its own) and `color.relight`.

use filmcraft_project::{ClipId, ItemId, ParamValue};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::Session;
use crate::media_test_util::tmp_dir;

const GREY: f32 = 0.2158; // #808080 in linear light

/// A locked-off 320×180, two-second shot: a grey background and a red 40×22 block crossing it
/// from x = 40 to x = 280 at mid-height, nested as one clip on V1 of a second sequence.
/// Returns (session, the clip, the shot's sequence item).
fn shot() -> (Session, u64, u64) {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name": "shot", "width": 320, "height": 180, "fps": 24.0})).unwrap();
    let shot_seq = s.state.active_sequence.unwrap().0;
    let matte = |s: &mut Session, color: &str| {
        s.execute("file.newColorMatte", json!({"color": color, "width": 320, "height": 180, "seconds": 2.0})).unwrap()["item"].as_u64().unwrap()
    };
    let (bg, blob) = (matte(&mut s, "#808080"), matte(&mut s, "#ff0000"));
    s.execute("timeline.place", json!({"item": bg, "track": "V1", "seconds": 0.0})).unwrap();
    let b = s.execute("timeline.place", json!({"item": blob, "track": "V2", "seconds": 0.0})).unwrap()["clips"][0].as_u64().unwrap();
    s.execute("effects.setParam", json!({"clip": b, "effect": "motion", "param": "scale", "value": 12.5})).unwrap();
    let last = s.sequence_rate().tick_of(47);
    s.execute("effects.setParam", json!({"clip": b, "effect": "motion", "param": "position", "value": [40.0, 90.0], "time": 0, "keyframe": true})).unwrap();
    s.execute("effects.setParam", json!({"clip": b, "effect": "motion", "param": "position", "value": [280.0, 90.0], "time": last.0, "keyframe": true}))
        .unwrap();
    s.execute("file.newSequence", json!({"name": "main", "width": 320, "height": 180, "fps": 24.0})).unwrap();
    let clip = s.execute("timeline.place", json!({"item": shot_seq, "track": "V1", "seconds": 0.0, "audio": false})).unwrap()["clips"][0].as_u64().unwrap();
    (s, clip, shot_seq)
}

/// The programme's pixel at (`x`, `y`) of frame `frame` (premultiplied linear RGBA).
fn px(s: &Session, frame: i64, x: usize, y: usize) -> [f32; 4] {
    s.try_render_program_at(1.0, s.sequence_rate().tick_of(frame)).unwrap().get(x, y)
}

fn is(p: [f32; 4], want: [f32; 3]) -> bool {
    p.iter().zip(want).all(|(a, b)| (a - b).abs() < 0.02) && (p[3] - 1.0).abs() < 0.02
}

const RED: [f32; 3] = [1.0, 0.0, 0.0];
const GREY3: [f32; 3] = [GREY, GREY, GREY];

fn clip(s: &Session, id: u64) -> &filmcraft_project::TrackItem {
    s.active_sequence().unwrap().find_item(ClipId(id)).unwrap().1
}

fn refuses(s: &mut Session, id: &str, cases: &[Value]) {
    let snap = (*s.project).clone();
    let undo = s.history.undo.len();
    for p in cases {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.execute(id, p.clone())));
        assert!(r.is_ok(), "{id} panicked on {p}");
        assert!(r.unwrap().is_err(), "{id} accepted {p}");
        assert_eq!(*s.project, snap, "{id} changed the project on {p}");
        assert_eq!(s.history.undo.len(), undo, "{id} left an undo step on {p}");
    }
}

#[test]
fn the_test_shot_is_what_the_tests_assume() {
    let (s, _, _) = shot();
    assert!(is(px(&s, 0, 40, 90), RED), "{:?}", px(&s, 0, 40, 90));
    assert!(is(px(&s, 0, 200, 90), GREY3));
    assert!(is(px(&s, 47, 280, 90), RED));
    assert!(is(px(&s, 47, 40, 90), GREY3));
}

#[test]
fn a_region_becomes_a_layer_of_its_own() {
    let (mut s, c, _) = shot();
    let undo = s.history.undo.len();
    // the left third of the frame, where the block starts
    let r = s.execute("layers.extract", json!({"clip": c, "region": {"x": 0.0, "y": 0.25, "w": 0.3, "h": 0.5}, "feather": 0, "name": "block"})).unwrap();
    let layer = r["layer"].as_u64().unwrap();
    assert_eq!((r["track"].as_str(), r["of"].as_u64()), (Some("Video 4"), Some(c)));
    assert_eq!(s.history.undo.len(), undo + 1, "one undo step");
    assert_eq!(s.history.undo.last().unwrap().0, "Extract Layer");
    let q = s.active_sequence().unwrap();
    assert_eq!(q.video_tracks.len(), 4, "a new track above the three");
    let (l, o) = (clip(&s, layer), clip(&s, c));
    assert_eq!((l.start, l.duration, l.source_in, l.item, l.name.as_str()), (o.start, o.duration, o.source_in, o.item, "block"));
    assert_eq!(l.link, None, "the layer is not tied to the shot's sound");
    let mask = &l.effects.iter().find(|e| e.effect == "opacity").unwrap().masks[0];
    let ParamValue::Path(path) = &mask.path.value else { panic!("no path") };
    let (xs, ys): (Vec<f64>, Vec<f64>) = path.vertices.iter().map(|v| (v.p.x, v.p.y)).unzip();
    let bounds = (
        xs.iter().copied().fold(f64::MAX, f64::min),
        ys.iter().copied().fold(f64::MAX, f64::min),
        xs.iter().copied().fold(f64::MIN, f64::max),
        ys.iter().copied().fold(f64::MIN, f64::max),
    );
    assert_eq!(bounds, (0.0, 45.0, 96.0, 135.0), "the region in the clip's own pixels");
    assert_eq!(mask.feather.value, ParamValue::Float(0.0));
    assert!(o.effects.iter().find(|e| e.effect == "opacity").unwrap().masks.is_empty(), "the original is untouched");
    assert_eq!(s.state.selection, vec![ClipId(layer)]);
    // the picture has not changed: the layer shows what was already there
    assert!(is(px(&s, 0, 40, 90), RED) && is(px(&s, 0, 200, 90), GREY3));
    // with the original off, only the region is left
    s.execute("clip.enable", json!({"clips": [c]})).unwrap();
    assert!(is(px(&s, 0, 40, 90), RED) && is(px(&s, 0, 80, 60), GREY3), "inside the region");
    assert_eq!(px(&s, 0, 200, 90)[3], 0.0, "outside it, nothing");
    s.execute("clip.enable", json!({"clips": [c]})).unwrap();
    // the layer moves on its own: 60 px down, the block is seen twice (the original still shows it)
    s.execute("effects.setParam", json!({"clip": layer, "effect": "motion", "param": "position", "value": [160.0, 150.0]})).unwrap();
    assert!(is(px(&s, 0, 40, 150), RED) && is(px(&s, 0, 40, 90), RED));
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active_sequence().unwrap().video_tracks.len(), 3, "undo takes the layer and its track away");
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(s.active_sequence().unwrap().video_tracks.len(), 4);
}

#[test]
fn a_cut_out_leaves_a_hole_in_the_original() {
    let (mut s, c, _) = shot();
    let r = s
        .execute("layers.extract", json!({"clip": c, "region": {"shape": "ellipse", "x": 0.0, "y": 0.2, "w": 0.3, "h": 0.6}, "feather": 0, "cutOut": true}))
        .unwrap();
    let layer = r["layer"].as_u64().unwrap();
    assert_eq!(r["cutOut"]["clip"].as_u64(), Some(c));
    let hole = &clip(&s, c).effects.iter().find(|e| e.effect == "opacity").unwrap().masks[0];
    assert!(hole.inverted);
    // together they are the whole picture again
    assert!(is(px(&s, 0, 40, 90), RED) && is(px(&s, 0, 200, 90), GREY3));
    // the layer off: the original has a hole where the ellipse was
    s.execute("clip.enable", json!({"clips": [layer]})).unwrap();
    assert_eq!(px(&s, 0, 48, 90)[3], 0.0);
    assert!(is(px(&s, 0, 200, 90), GREY3));
}

#[test]
fn regions_take_points_and_refuse_nonsense() {
    let (mut s, c, _) = shot();
    let r = s.execute("layers.extract", json!({"clip": c, "region": {"points": [[0.0, 0.0], [0.5, 0.0], [0.0, 1.0]]}, "feather": 0})).unwrap();
    let layer = r["layer"].as_u64().unwrap();
    s.execute("clip.enable", json!({"clips": [c]})).unwrap();
    assert!(px(&s, 0, 20, 20)[3] > 0.9, "inside the triangle");
    assert_eq!(px(&s, 0, 300, 170)[3], 0.0, "outside it");
    let _ = layer;
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    let sound = {
        // a sound clip to refuse
        let dir = tmp_dir("layers-sound");
        let wav = dir.join("a.wav");
        std::fs::write(&wav, filmcraft_media::wav::write_wav16(&vec![0.1; 4800], 1, 48_000)).unwrap();
        let item = s.execute("file.import", json!({"paths": [wav.to_string_lossy()]})).unwrap()["items"][0].as_u64().unwrap();
        let id = s.execute("timeline.place", json!({"item": item, "track": "A1", "seconds": 0.0})).unwrap()["clips"][0].as_u64().unwrap();
        let _ = std::fs::remove_dir_all(dir);
        id
    };
    let region = json!({"x": 0.1, "y": 0.1, "w": 0.2, "h": 0.2});
    for id in ["layers.extract", "layers.retime", "layers.adjust", "layers.lift", "layers.cleanPlate"] {
        let mut cases = vec![
            json!({}),
            json!({"clip": c}),
            json!({"clip": 999_999, "region": region}),
            json!({"clip": "first", "region": region}),
            json!({"clip": sound, "region": region}),
            json!({"clip": c, "region": "all"}),
            json!({"clip": c, "region": {}}),
            json!({"clip": c, "region": {"x": 0.1, "y": 0.1, "w": 0.0, "h": 0.2}}),
            json!({"clip": c, "region": {"x": 0.1, "y": 0.1, "w": -0.5, "h": 0.2}}),
            json!({"clip": c, "region": {"x": 1e300, "y": 0.1, "w": 0.2, "h": 0.2}}),
            json!({"clip": c, "region": {"x": "left", "y": 0.1, "w": 0.2, "h": 0.2}}),
            json!({"clip": c, "region": {"shape": "star", "x": 0.1, "y": 0.1, "w": 0.2, "h": 0.2}}),
            json!({"clip": c, "region": {"points": [[0.0, 0.0], [1.0, 1.0]]}}),
            json!({"clip": c, "region": {"points": [[0.0, 0.0], [1.0], [0.5, 0.5]]}}),
            json!({"clip": c, "region": {"points": [[0.0, 0.0], [1.0, f64::MAX], [0.5, 0.5]]}}),
            json!({"clip": c, "region": {"points": vec![[0.1, 0.1]; 100]}}),
            json!({"clip": c, "region": region, "feather": -1}),
            json!({"clip": c, "region": region, "feather": 1e9}),
            json!({"clip": c, "region": region, "feather": "soft"}),
        ];
        if id == "layers.cleanPlate" {
            // its region is optional, so the cases without one are not refusals
            cases.retain(|p| p.get("region").is_some() && p.get("clip") == Some(&json!(c)) || p.get("clip") != Some(&json!(c)));
            cases.retain(|p| *p != json!({}) || true);
        }
        // complete the ones that need more than a region with valid extras, so the refusal is the region's
        let cases: Vec<Value> = cases
            .into_iter()
            .map(|mut p| {
                match id {
                    "layers.retime" => p["speed"] = json!(50),
                    "layers.adjust" => p["effect"] = json!("black_white"),
                    "layers.lift" | "layers.cleanPlate" => p["path"] = json!(std::env::temp_dir().join("filmcraft-never-written.png").to_string_lossy()),
                    _ => {}
                }
                p
            })
            .collect();
        refuses(&mut s, id, &cases);
    }
    assert!(!std::env::temp_dir().join("filmcraft-never-written.png").exists());
    // a locked track is left alone
    s.execute("sequence.toggleTrackLock", json!({"track": "V1"})).ok();
    if s.active_sequence().unwrap().video_tracks[0].locked {
        assert!(s.execute("layers.extract", json!({"clip": c, "region": region})).unwrap_err().to_string().contains("locked"));
    }
    let mut empty = Session::default();
    for id in ["layers.extract", "layers.retime", "layers.adjust", "layers.lift", "layers.cleanPlate", "color.relight"] {
        assert!(empty.execute(id, json!({"clip": 1})).is_err(), "{id} without a sequence");
    }
}

#[test]
fn extract_flags_must_be_flags() {
    let (mut s, c, _) = shot();
    let region = json!({"x": 0.1, "y": 0.1, "w": 0.2, "h": 0.2});
    refuses(
        &mut s,
        "layers.extract",
        &[
            json!({"clip": c, "region": region, "follow": "yes"}),
            json!({"clip": c, "region": region, "cutOut": 1}),
            json!({"clip": c, "region": region, "name": ""}),
            json!({"clip": c, "region": region, "name": 5}),
            json!({"clip": c, "region": region, "name": "a\u{0}b"}),
        ],
    );
}

#[test]
fn a_layer_can_follow_its_subject() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let c = s.active_sequence().unwrap().video_tracks[0].items[3].id.0;
    // a short stretch keeps the test quick: the clip's first eight frames
    let frames = clip(&s, c).duration.0 / s.sequence_rate().frame_duration().0;
    s.execute("timeline.trim", json!({"clip": c, "edge": "out", "mode": "regular", "deltaFrames": 8 - frames, "linked": false})).unwrap();
    let r = s
        .execute("layers.extract", json!({"clip": c, "region": {"x": 0.3, "y": 0.3, "w": 0.4, "h": 0.4}, "feather": 0, "follow": true, "cutOut": true}))
        .unwrap();
    let t = &r["tracking"];
    assert_eq!(t["frames"], 7, "{r}");
    let layer = r["layer"].as_u64().unwrap();
    let path_keys = |id: u64| clip(&s, id).effects.iter().find(|e| e.effect == "opacity").unwrap().masks[0].path.keyframes.len();
    if t["error"].is_null() {
        assert_eq!(path_keys(layer), 8, "a mask path keyframe per frame: {r}");
        assert_eq!(path_keys(c), 8, "the hole follows the layer");
    } else {
        // footage with too little detail in the region: the tracker says so and keeps what it has
        assert!(t["error"].as_str().unwrap().contains("lost track"), "{r}");
    }
    assert_eq!(s.history.undo.last().unwrap().0, "Extract Layer");
    // a nested sequence has no media of its own to track: refused whole, nothing half-made
    let (mut n, nested, _) = shot();
    let tracks = n.active_sequence().unwrap().video_tracks.len();
    refuses(&mut n, "layers.extract", &[json!({"clip": nested, "region": {"x": 0.03, "y": 0.35, "w": 0.2, "h": 0.3}, "follow": true})]);
    assert_eq!(n.active_sequence().unwrap().video_tracks.len(), tracks);
}

#[test]
fn a_clean_plate_takes_out_what_moved_through() {
    let (mut s, c, _) = shot();
    let dir = tmp_dir("layers-plate");
    let path = dir.join("plate.png").to_string_lossy().into_owned();
    let (undo, items) = (s.history.undo.len(), s.project.items.len());
    let r = s.execute("layers.cleanPlate", json!({"clip": c, "path": path, "samples": 9})).unwrap();
    assert_eq!((r["width"].as_u64(), r["height"].as_u64(), r["samples"].as_u64()), (Some(320), Some(180), Some(9)));
    assert!(r["stillness"]["settled"].as_f64().unwrap() > 0.99, "{r}");
    let disturbed = r["stillness"]["disturbed"].as_f64().unwrap();
    assert!(disturbed > 0.01 && disturbed < 0.2, "the block's path: {r}");
    assert!(r["warnings"].as_array().unwrap().is_empty(), "{r}");
    assert!(std::fs::read(&path).unwrap().starts_with(b"\x89PNG"));
    assert_eq!(s.project.items.len(), items + 1, "the plate is in the project");
    assert_eq!(s.history.undo.len(), undo + 1);
    let plate = r["clip"].as_u64().unwrap();
    let (pl, o) = (clip(&s, plate), clip(&s, c));
    assert_eq!((pl.start, pl.duration), (o.start, o.duration), "it covers the shot");
    assert_eq!(pl.item, ItemId(r["item"].as_u64().unwrap()));
    assert!(pl.name.ends_with("(plate)"));
    // the block is gone from every frame
    for f in [0, 12, 24, 47] {
        for x in [40, 100, 160, 220, 280] {
            assert!(is(px(&s, f, x, 90), GREY3), "frame {f} x {x}: {:?}", px(&s, f, x, 90));
        }
    }
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.items.len(), items);
    assert!(is(px(&s, 0, 40, 90), RED), "undo brings the shot back");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_plate_over_a_region_removes_only_what_was_there() {
    let (mut s, c, _) = shot();
    let dir = tmp_dir("layers-patch");
    let path = dir.join("patch.png").to_string_lossy().into_owned();
    // cover the left half: the block is hidden while it is there and seen once it leaves
    let r = s
        .execute("layers.cleanPlate", json!({"clip": c, "path": path, "samples": 9, "region": {"x": 0.0, "y": 0.0, "w": 0.5, "h": 1.0}, "feather": 0}))
        .unwrap();
    let plate = r["clip"].as_u64().unwrap();
    assert_eq!(clip(&s, plate).effects.iter().find(|e| e.effect == "opacity").unwrap().masks.len(), 1);
    assert!(is(px(&s, 0, 40, 90), GREY3), "hidden on the left");
    assert!(is(px(&s, 47, 280, 90), RED), "seen on the right");
    // without placing: just the still in the project
    let before = s.active_sequence().unwrap().video_tracks.len();
    let r = s.execute("layers.cleanPlate", json!({"clip": c, "path": path, "samples": 5, "place": false})).unwrap();
    assert!(r["clip"].is_null() && r["item"].is_u64());
    assert_eq!(s.active_sequence().unwrap().video_tracks.len(), before);
    refuses(
        &mut s,
        "layers.cleanPlate",
        &[
            json!({"clip": c}),
            json!({"clip": c, "path": ""}),
            json!({"clip": c, "path": dir.join("plate.jpg").to_string_lossy()}),
            json!({"clip": c, "path": path, "samples": 2}),
            json!({"clip": c, "path": path, "samples": 1000}),
            json!({"clip": c, "path": path, "samples": "many"}),
            json!({"clip": c, "path": path, "place": "no"}),
            json!({"clip": c, "path": path, "place": false, "region": {"x": 0.0, "y": 0.0, "w": 0.5, "h": 1.0}}),
            json!({"clip": c, "path": dir.join("no/such/folder/p.png").to_string_lossy()}),
        ],
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_still_shot_and_a_moving_camera_are_called_out() {
    let dir = tmp_dir("layers-warn");
    let path = dir.join("p.png").to_string_lossy().into_owned();
    // nothing moves: the plate is just the frame
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name": "still", "width": 64, "height": 36, "fps": 24.0})).unwrap();
    let m = s.execute("file.newColorMatte", json!({"color": "#336699", "width": 64, "height": 36, "seconds": 1.0})).unwrap()["item"].as_u64().unwrap();
    let c = s.execute("timeline.place", json!({"item": m, "track": "V1", "seconds": 0.0})).unwrap()["clips"][0].as_u64().unwrap();
    let r = s.execute("layers.cleanPlate", json!({"clip": c, "path": path, "samples": 3})).unwrap();
    assert!(r["warnings"][0].as_str().unwrap().contains("nothing moves"), "{r}");
    // the demo's footage moves all over: not a locked-off shot
    let mut d = Session::default();
    d.execute("file.openDemoProject", json!({})).unwrap();
    let c = d.active_sequence().unwrap().video_tracks[0].items[2].id.0;
    let r = d.execute("layers.cleanPlate", json!({"clip": c, "path": path, "samples": 5, "place": false})).unwrap();
    let settled = r["stillness"]["settled"].as_f64().unwrap();
    if settled < 0.9 {
        assert!(r["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("locked-off")), "{r}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_lifted_subject_moves_without_leaving_a_double() {
    let (mut s, c, _) = shot();
    let dir = tmp_dir("layers-lift");
    let path = dir.join("plate.png").to_string_lossy().into_owned();
    let undo = s.history.undo.len();
    // the block at the first frame: x 20…60, y 79…101
    let r =
        s.execute("layers.lift", json!({"clip": c, "region": {"x": 0.03, "y": 0.35, "w": 0.2, "h": 0.3}, "path": path, "feather": 0, "samples": 9})).unwrap();
    let (layer, patch) = (r["layer"].as_u64().unwrap(), r["plate"]["clip"].as_u64().unwrap());
    assert_eq!(s.history.undo.len(), undo + 1, "one undo step");
    let q = s.active_sequence().unwrap();
    let track_of = |id: u64| q.video_tracks.iter().position(|t| t.items.iter().any(|i| i.id.0 == id)).unwrap();
    assert!(track_of(c) < track_of(patch) && track_of(patch) < track_of(layer), "clip, plate, subject from the bottom up");
    // nothing has changed yet
    assert!(is(px(&s, 0, 40, 90), RED) && is(px(&s, 0, 200, 90), GREY3));
    // move the subject 60 px down: it is there, and the place it left is background
    s.execute("effects.setParam", json!({"clip": layer, "effect": "motion", "param": "position", "value": [160.0, 150.0]})).unwrap();
    assert!(is(px(&s, 0, 40, 150), RED), "{:?}", px(&s, 0, 40, 150));
    assert!(is(px(&s, 0, 40, 90), GREY3), "no double: {:?}", px(&s, 0, 40, 90));
    // switch the layer off: the subject is out of the shot
    s.execute("clip.enable", json!({"clips": [layer]})).unwrap();
    assert!(is(px(&s, 0, 40, 150), GREY3) && is(px(&s, 0, 40, 90), GREY3));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_region_plays_at_its_own_speed() {
    let (mut s, c, _) = shot();
    let undo = s.history.undo.len();
    // the left half at half speed
    let r = s.execute("layers.retime", json!({"clip": c, "region": {"x": 0.0, "y": 0.0, "w": 0.5, "h": 1.0}, "speed": 50, "feather": 0})).unwrap();
    let layer = r["layer"].as_u64().unwrap();
    assert_eq!(s.history.undo.len(), undo + 1);
    let (l, o) = (clip(&s, layer), clip(&s, c));
    assert_eq!(l.speed, 0.5);
    assert_eq!((l.start, l.end()), (o.start, o.end()), "the slowed layer ends with the shot");
    assert_eq!((o.speed, r["untilShotEnd"].as_bool()), (1.0, Some(true)), "the shot keeps its speed");
    // at frame 20 the shot's block is at x = 142; in the slowed half it is where frame 10 had it, x = 91
    assert!(is(px(&s, 20, 91, 90), RED), "{:?}", px(&s, 20, 91, 90));
    assert!(is(px(&s, 20, 142, 90), GREY3), "the region covers the shot's own block: {:?}", px(&s, 20, 142, 90));
    // the right half still runs at speed: at frame 40 the block is at x = 244
    assert!(is(px(&s, 40, 244, 90), RED));
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active_sequence().unwrap().video_tracks.len(), 3);

    // twice as fast: the layer runs out halfway through the shot
    let r = s
        .execute("layers.retime", json!({"clip": c, "region": {"x": 0.0, "y": 0.0, "w": 0.5, "h": 1.0}, "speed": 200, "interpolation": "frameBlending"}))
        .unwrap();
    let l = clip(&s, r["layer"].as_u64().unwrap());
    assert_eq!((l.speed, l.duration, r["untilShotEnd"].as_bool()), (2.0, Tick(clip(&s, c).duration.0 / 2), Some(false)));
    assert_eq!(l.time_interpolation, filmcraft_project::TimeInterpolation::FrameBlending);
    let region = json!({"x": 0.0, "y": 0.0, "w": 0.5, "h": 1.0});
    refuses(
        &mut s,
        "layers.retime",
        &[
            json!({"clip": c, "region": region}),
            json!({"clip": c, "region": region, "speed": 0}),
            json!({"clip": c, "region": region, "speed": -50}),
            json!({"clip": c, "region": region, "speed": 1e12}),
            json!({"clip": c, "region": region, "speed": "slow"}),
            json!({"clip": c, "region": region, "speed": 50, "interpolation": "magic"}),
            json!({"clip": c, "region": region, "speed": 50, "interpolation": 3}),
        ],
    );
}

#[test]
fn an_effect_touches_only_its_region() {
    let (mut s, c, _) = shot();
    let undo = s.history.undo.len();
    let before = px(&s, 47, 280, 90);
    // black and white over the left third, where the block starts
    let r =
        s.execute("layers.adjust", json!({"clip": c, "region": {"x": 0.0, "y": 0.0, "w": 0.33, "h": 1.0}, "effect": "Black & White", "feather": 0})).unwrap();
    assert_eq!(r["effect"], "black_white");
    assert_eq!(s.history.undo.len(), undo + 1);
    let e = &clip(&s, c).effects[r["index"].as_u64().unwrap() as usize];
    assert_eq!((e.effect.as_str(), e.masks.len()), ("black_white", 1));
    let grey = px(&s, 0, 40, 90);
    assert!((grey[0] - grey[1]).abs() < 1e-3 && (grey[1] - grey[2]).abs() < 1e-3 && grey[0] > 0.1 && grey[0] < 0.4, "the red block reads grey there: {grey:?}");
    assert_eq!(px(&s, 47, 280, 90), before, "and stays red outside the region");
    s.execute("edit.undo", json!({})).unwrap();
    assert!(is(px(&s, 0, 40, 90), RED));

    // parameters go with it
    let r = s
        .execute(
            "layers.adjust",
            json!({"clip": c, "region": {"x": 0.0, "y": 0.0, "w": 0.33, "h": 1.0}, "effect": "tint", "params": {"amount": 100, "white": "#00ff00"}}),
        )
        .unwrap();
    let e = &clip(&s, c).effects[r["index"].as_u64().unwrap() as usize];
    assert_eq!(e.params["amount"].value, ParamValue::Float(100.0));
    let region = json!({"x": 0.0, "y": 0.0, "w": 0.33, "h": 1.0});
    refuses(
        &mut s,
        "layers.adjust",
        &[
            json!({"clip": c, "region": region}),
            json!({"clip": c, "region": region, "effect": ""}),
            json!({"clip": c, "region": region, "effect": "no such effect"}),
            json!({"clip": c, "region": region, "effect": 7}),
            json!({"clip": c, "region": region, "effect": "tint", "params": [1, 2]}),
            json!({"clip": c, "region": region, "effect": "tint", "params": {"nope": 1}}),
            json!({"clip": c, "region": region, "effect": "tint", "params": {"amount": 1e9}}),
            json!({"clip": c, "region": region, "effect": "tint", "params": {"amount": "lots"}}),
            // a sound effect has no picture to mask
            json!({"clip": c, "region": region, "effect": "Parametric Equalizer"}),
            // a transition is not an effect on a clip
            json!({"clip": c, "region": region, "effect": "Cross Dissolve"}),
        ],
    );
    // on a clip with linked sound, a sound effect would land on the sound: refused, and the sound is untouched
    let mut d = Session::default();
    d.execute("file.openDemoProject", json!({})).unwrap();
    let v = d.active_sequence().unwrap().video_tracks[0].items[0].id.0;
    refuses(&mut d, "layers.adjust", &[json!({"clip": v, "region": region, "effect": "Parametric Equalizer"})]);
}

fn mean_rgb(s: &Session, frame: i64) -> [f32; 3] {
    let img = s.try_render_program_at(0.25, s.sequence_rate().tick_of(frame)).unwrap();
    let mut sum = [0f64; 3];
    for p in img.px.chunks(4) {
        for c in 0..3 {
            sum[c] += f64::from(p[c]);
        }
    }
    sum.map(|v| (v / (img.w * img.h) as f64) as f32)
}

fn grade(s: &Session, c: u64, id: &str) -> f64 {
    clip(s, c).effects.iter().find(|e| e.effect == "lumetri").unwrap().params[id].value.as_f64().unwrap()
}

#[test]
fn relight_grades_toward_the_hour() {
    let (mut s, c, _) = shot();
    let noon = mean_rgb(&s, 30);
    let undo = s.history.undo.len();
    let r = s.execute("color.relight", json!({"clip": c, "time": "18:30", "lightStrength": 0})).unwrap();
    assert_eq!((r["time"].as_str(), r["keyframes"].as_u64()), (Some("18:30"), Some(0)));
    assert!(r["toTime"].is_null());
    assert_eq!(s.history.undo.len(), undo + 1, "one undo step");
    assert_eq!((grade(&s, c, "temperature"), grade(&s, c, "saturation")), (48.0, 115.0));
    assert_eq!(r["grade"]["temperature"], 48.0);
    let gold = mean_rgb(&s, 30);
    assert!(gold[0] / gold[2] > noon[0] / noon[2] * 1.1, "golden hour is warmer: {gold:?} vs {noon:?}");
    // night: darker and bluer than the shot
    s.execute("color.relight", json!({"clip": c, "time": 23})).unwrap();
    let night = mean_rgb(&s, 30);
    assert!(night[1] < noon[1] * 0.6 && night[2] / night[0] > noon[2] / noon[0], "{night:?} vs {noon:?}");
    // half strength is halfway to the hour; none is the shot as it was
    s.execute("color.relight", json!({"clip": c, "time": "18:30", "strength": 0.5, "lightStrength": 0})).unwrap();
    assert_eq!(grade(&s, c, "temperature"), 24.0);
    s.execute("color.relight", json!({"clip": c, "time": "18:30", "strength": 0, "lightStrength": 0})).unwrap();
    assert_eq!((grade(&s, c, "temperature"), grade(&s, c, "exposure"), grade(&s, c, "saturation")), (0.0, 0.0, 100.0));
    let back = mean_rgb(&s, 30);
    assert!(back.iter().zip(noon).all(|(a, b)| (a - b).abs() < 0.01), "{back:?} vs {noon:?}");
    // relighting again reuses the two effects instead of stacking more
    assert_eq!(clip(&s, c).effects.iter().filter(|e| e.effect == "lumetri").count(), 1);
}

#[test]
fn relight_adds_a_light_where_the_sun_stands() {
    let (mut s, c, _) = shot();
    let r = s.execute("color.relight", json!({"clip": c, "time": 12.0, "strength": 0, "lightStrength": 1})).unwrap();
    let sun = r["light"]["at"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect::<Vec<_>>();
    assert!((sun[0] - 0.485).abs() < 0.03 && sun[1] < 0.2, "overhead at noon: {sun:?}");
    let e = clip(&s, c).effects.iter().find(|e| e.effect == "lighting_effects").unwrap().clone();
    assert_eq!(e.params["l1_type"].value, ParamValue::Choice(2));
    assert!(e.enabled);
    // brighter under the light than in the far corner
    let img = s.try_render_program_at(1.0, Tick::ZERO).unwrap();
    let (near, far) = (img.get((sun[0] * 320.0) as usize, (sun[1] * 180.0) as usize + 4)[1], img.get(4, 176)[1]);
    assert!(near > far * 1.15, "{near} vs {far}");
    // the light goes where it is put
    let r = s.execute("color.relight", json!({"clip": c, "time": 12.0, "strength": 0, "light": [0.1, 0.9]})).unwrap();
    assert_eq!(r["light"]["at"], json!([0.1, 0.9]));
    let img = s.try_render_program_at(1.0, Tick::ZERO).unwrap();
    assert!(img.get(32, 162)[1] > img.get(300, 10)[1]);
    // and off again
    s.execute("color.relight", json!({"clip": c, "time": 12.0, "strength": 0, "lightStrength": 0})).unwrap();
    assert!(!clip(&s, c).effects.iter().find(|e| e.effect == "lighting_effects").unwrap().enabled);
}

#[test]
fn a_day_passes_during_a_shot() {
    let (mut s, c, _) = shot();
    let r = s.execute("color.relight", json!({"clip": c, "time": "09:00", "toTime": "21:00"})).unwrap();
    assert_eq!(r["toTime"], "21:00");
    // nine o'clock, nine in the evening and the seven hours of the table between them
    assert_eq!(r["keyframes"], 9, "{r}");
    let e = clip(&s, c).effects.iter().find(|e| e.effect == "lumetri").unwrap().clone();
    let temp = &e.params["temperature"];
    assert_eq!(temp.keyframes.len(), 9);
    assert!(temp.keyframes.windows(2).all(|w| w[0].time < w[1].time));
    // warm in the evening, cold at night
    let rate = s.sequence_rate();
    let at = |frame: i64| temp.f64_at(clip(&s, c).effect_time_at(rate.tick_of(frame)));
    assert!(at(0) > 0.0 && at(0) < 20.0, "{}", at(0));
    assert!(at(38) > 30.0, "golden hour near the end: {}", at(38));
    assert!(at(47) < -20.0, "night at the end: {}", at(47));
    // and backwards
    let r = s.execute("color.relight", json!({"clip": c, "time": "21:00", "toTime": "09:00"})).unwrap();
    assert_eq!(r["keyframes"], 9);
    // a static relight afterwards clears the animation
    s.execute("color.relight", json!({"clip": c, "time": "12:00"})).unwrap();
    let e = clip(&s, c).effects.iter().find(|e| e.effect == "lumetri").unwrap().clone();
    assert!(e.params["temperature"].keyframes.is_empty());
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(clip(&s, c).effects.iter().find(|e| e.effect == "lumetri").unwrap().params["temperature"].keyframes.len(), 9);
}

#[test]
fn relight_refuses_hostile_parameters() {
    let (mut s, c, _) = shot();
    refuses(
        &mut s,
        "color.relight",
        &[
            json!({}),
            json!({"clip": c}),
            json!({"time": "12:00"}),
            json!({"clip": 999_999, "time": "12:00"}),
            json!({"clip": c, "time": "25:00"}),
            json!({"clip": c, "time": "12:61"}),
            json!({"clip": c, "time": "24:01"}),
            json!({"clip": c, "time": "noon"}),
            json!({"clip": c, "time": "-1:00"}),
            json!({"clip": c, "time": -1}),
            json!({"clip": c, "time": 1e300}),
            json!({"clip": c, "time": true}),
            json!({"clip": c, "time": "12:00", "toTime": 99}),
            json!({"clip": c, "time": "12:00", "strength": 2}),
            json!({"clip": c, "time": "12:00", "strength": "full"}),
            json!({"clip": c, "time": "12:00", "lightStrength": -0.1}),
            json!({"clip": c, "time": "12:00", "light": [0.5]}),
            json!({"clip": c, "time": "12:00", "light": [0.5, 1e99]}),
            json!({"clip": c, "time": "12:00", "light": "left"}),
        ],
    );
}

#[test]
fn the_day_table_is_whole_and_in_order() {
    use crate::relight::{DAY, grade_at, sun_at};
    assert_eq!((DAY[0].0, DAY[DAY.len() - 1].0), (0.0, 24.0));
    assert!(DAY.windows(2).all(|w| w[0].0 < w[1].0));
    assert_eq!(DAY[0].1, DAY[DAY.len() - 1].1, "midnight meets midnight");
    for h in 0..=240 {
        let hour = f64::from(h) / 10.0;
        let g = grade_at(hour, 1.0);
        // inside every parameter's own range
        assert!(g[0].abs() <= 100.0 && g[1].abs() <= 100.0 && g[2].abs() <= 5.0 && g[3].abs() <= 100.0 && (0.0..=200.0).contains(&g[4]), "{hour}: {g:?}");
        let (pos, color, k) = sun_at(hour);
        assert!((0.0..=1.0).contains(&pos.x) && (0.0..=1.0).contains(&pos.y) && color.iter().all(|c| (0.0..=1.0).contains(c)) && k > 0.0, "{hour}");
    }
    assert_eq!(grade_at(12.0, 0.0), [0.0, 0.0, 0.0, 0.0, 100.0, 0.0, 0.0]);
    // out-of-range and non-finite hours are clamped, not indexed
    for h in [-5.0, 99.0, f64::NAN, f64::INFINITY] {
        assert!(grade_at(h, 1.0).iter().all(|v| v.is_finite()));
        assert!(sun_at(h).0.x.is_finite());
    }
}

#[test]
fn the_layer_commands_are_registered() {
    for id in ["layers.extract", "layers.cleanPlate", "layers.lift", "layers.retime", "layers.adjust", "color.relight"] {
        let spec = crate::commands::find(id).unwrap_or_else(|| panic!("{id} is not registered"));
        assert!(spec.journal && spec.params.contains("\"clip\":id"));
    }
}
