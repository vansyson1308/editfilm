//! `layers.*` (a region of a clip as a layer of its own) and `color.relight`.

use filmagic_project::{ClipId, ItemId, ParamValue};
use filmagic_time::Tick;
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

fn clip(s: &Session, id: u64) -> &filmagic_project::TrackItem {
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
    assert_eq!(r["of"].as_u64(), Some(c));
    assert_eq!(s.history.undo.len(), undo + 1, "one undo step");
    assert_eq!(s.history.undo.last().unwrap().0, "Extract Layer");
    let q = s.active_sequence().unwrap();
    assert_eq!(q.video_tracks.len(), 4, "a new track");
    assert!(q.video_tracks[1].items.iter().any(|i| i.id.0 == layer), "directly above the clip's own track");
    assert_eq!(r["track"].as_str(), Some(q.video_tracks[1].name.as_str()));
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
    assert_eq!((hole.mode, hole.inverted), (filmagic_project::MaskMode::Subtract, false), "a hole takes away from what is there");
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
        std::fs::write(&wav, filmagic_media::wav::write_wav16(&vec![0.1; 4800], 1, 48_000)).unwrap();
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
                    "layers.lift" | "layers.cleanPlate" => p["path"] = json!(std::env::temp_dir().join("filmagic-never-written.png").to_string_lossy()),
                    _ => {}
                }
                p
            })
            .collect();
        refuses(&mut s, id, &cases);
    }
    assert!(!std::env::temp_dir().join("filmagic-never-written.png").exists());
    // a locked track is left alone
    s.execute("timeline.setTrack", json!({"track": "V1", "locked": true})).unwrap();
    assert!(s.active_sequence().unwrap().video_tracks[0].locked);
    for (id, extra) in [("layers.extract", json!({})), ("layers.retime", json!({"speed": 50})), ("layers.adjust", json!({"effect": "tint"}))] {
        let mut p = json!({"clip": c, "region": region});
        p.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        assert!(s.execute(id, p).unwrap_err().to_string().contains("locked"), "{id}");
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
    assert_eq!(r["pictureChange"]["reads"], "strong", "the block is gone: {r}");
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
    let second = dir.join("whole.png").to_string_lossy().into_owned();
    let r = s.execute("layers.cleanPlate", json!({"clip": c, "path": second, "samples": 5, "place": false})).unwrap();
    assert!(r["clip"].is_null() && r["item"].is_u64());
    assert_eq!(s.active_sequence().unwrap().video_tracks.len(), before);
    // a file the project already reads is not written over: the plate on the timeline would change under the edit
    let on_disk = std::fs::read(&path).unwrap();
    refuses(&mut s, "layers.cleanPlate", &[json!({"clip": c, "path": path, "samples": 5, "place": false}), json!({"clip": c, "path": second, "samples": 5})]);
    assert_eq!(std::fs::read(&path).unwrap(), on_disk);
    let path = dir.join("third.png").to_string_lossy().into_owned();
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
    assert_eq!(l.time_interpolation, filmagic_project::TimeInterpolation::FrameBlending);
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
    // the result says what it did to the picture: red turned grey is a strong change
    assert_eq!(r["pictureChange"]["reads"], "strong", "{r}");
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
    // an adjustment too small to see says so, instead of leaving the caller to believe it worked
    let r = s
        .execute("layers.adjust", json!({"clip": c, "region": {"x": 0.6, "y": 0.0, "w": 0.3, "h": 1.0}, "effect": "lumetri", "params": {"exposure": 0.02}}))
        .unwrap();
    assert_eq!(r["pictureChange"]["reads"], "none", "{r}");
    assert!(r["pictureChange"]["hint"].as_str().unwrap().contains("nothing a viewer can see"), "{r}");
    s.execute("edit.undo", json!({})).unwrap();
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
    assert!(["visible", "strong"].contains(&r["pictureChange"]["reads"].as_str().unwrap()), "{r}");
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
    let r = s.execute("color.relight", json!({"clip": c, "time": "18:30", "strength": 0, "lightStrength": 0})).unwrap();
    assert_eq!((grade(&s, c, "temperature"), grade(&s, c, "exposure"), grade(&s, c, "saturation")), (0.0, 0.0, 100.0));
    // the change it reports is against the picture as this call found it (the half-strength grade)
    assert_ne!(r["pictureChange"]["reads"], "none", "{r}");
    let r = s.execute("color.relight", json!({"clip": c, "time": "18:30", "strength": 0, "lightStrength": 0})).unwrap();
    assert_eq!(r["pictureChange"]["reads"], "none", "the same again changes nothing: {r}");
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
    // an end earlier in the day is the next morning: through the night, never back through noon
    let r = s.execute("color.relight", json!({"clip": c, "time": "21:00", "toTime": "09:00"})).unwrap();
    assert_eq!((r["keyframes"].as_u64(), r["hours"].as_f64()), (Some(7), Some(12.0)), "{r}");
    assert!(r["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("through midnight")), "{r}");
    let night = clip(&s, c).effects.iter().find(|e| e.effect == "lumetri").unwrap().params["exposure"].clone();
    let dark = |frame: i64| night.f64_at(clip(&s, c).effect_time_at(rate.tick_of(frame)));
    assert!(dark(4) < -1.0 && dark(12) < -1.5 && dark(24) < -1.0, "dark all night: {} {} {}", dark(4), dark(12), dark(24));
    assert!(dark(47) > -0.2, "light by nine: {}", dark(47));
    // a static relight afterwards clears the animation
    s.execute("color.relight", json!({"clip": c, "time": "12:00"})).unwrap();
    let e = clip(&s, c).effects.iter().find(|e| e.effect == "lumetri").unwrap().clone();
    assert!(e.params["temperature"].keyframes.is_empty());
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(clip(&s, c).effects.iter().find(|e| e.effect == "lumetri").unwrap().params["temperature"].keyframes.len(), 7);
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

/// A full-frame blue matte over the first second of the shot, on the track above it.
fn cover(s: &mut Session) -> u64 {
    let blue = s.execute("file.newColorMatte", json!({"color": "#0000ff", "width": 320, "height": 180, "seconds": 1.0})).unwrap()["item"].as_u64().unwrap();
    s.execute("timeline.place", json!({"item": blue, "track": "V2", "seconds": 0.0})).unwrap()["clips"][0].as_u64().unwrap()
}

const BLUE: [f32; 3] = [0.0, 0.0, 1.0];

#[test]
fn a_layer_stays_under_what_already_covers_its_clip() {
    let (mut s, c, _) = shot();
    cover(&mut s);
    assert!(is(px(&s, 0, 40, 90), BLUE) && is(px(&s, 30, 190, 90), RED), "covered for a second, then seen");
    let dir = tmp_dir("layers-under");
    let region = json!({"x": 0.0, "y": 0.25, "w": 0.3, "h": 0.5});
    let r = s.execute("layers.extract", json!({"clip": c, "region": region, "feather": 0})).unwrap();
    assert!(is(px(&s, 0, 40, 90), BLUE), "the layer does not jump over the cover: {:?}", px(&s, 0, 40, 90));
    assert!(r["warnings"].as_array().unwrap().is_empty());
    s.execute("edit.undo", json!({})).unwrap();
    for (cmd, p) in [
        ("layers.lift", json!({"clip": c, "region": region, "feather": 0, "samples": 9, "path": dir.join("a.png").to_string_lossy()})),
        ("layers.cleanPlate", json!({"clip": c, "samples": 9, "path": dir.join("b.png").to_string_lossy()})),
        ("layers.retime", json!({"clip": c, "region": region, "feather": 0, "speed": 50})),
    ] {
        s.execute(cmd, p).unwrap();
        assert!(is(px(&s, 0, 40, 90), BLUE), "{cmd} put its layer over the cover: {:?}", px(&s, 0, 40, 90));
        // the stack is the clip, then what was made from it, then the cover
        let q = s.active_sequence().unwrap();
        let of = |id: u64| q.video_tracks.iter().position(|t| t.items.iter().any(|i| i.id.0 == id)).unwrap();
        assert_eq!(of(c), 0, "{cmd}");
        assert!(
            q.video_tracks.last().unwrap().items.is_empty() && q.video_tracks.iter().rev().nth(1).unwrap().items.len() == 1,
            "{cmd}: the cover is still on top of all that has a picture"
        );
        s.execute("edit.undo", json!({})).unwrap();
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_second_cut_out_does_not_fill_the_first() {
    let (mut s, c, _) = shot();
    let (left, right) = (json!({"x": 0.0, "y": 0.0, "w": 0.25, "h": 1.0}), json!({"x": 0.75, "y": 0.0, "w": 0.25, "h": 1.0}));
    let a = s.execute("layers.extract", json!({"clip": c, "region": left, "feather": 0, "cutOut": true})).unwrap()["layer"].as_u64().unwrap();
    let b = s.execute("layers.extract", json!({"clip": c, "region": right, "feather": 0, "cutOut": true})).unwrap()["layer"].as_u64().unwrap();
    // The second layer is a copy of the clip as it was, hole and all: its region is kept out of what is left.
    let masks = &clip(&s, b).effects.iter().find(|e| e.effect == "opacity").unwrap().masks;
    assert_eq!(masks.iter().map(|m| m.mode).collect::<Vec<_>>(), [filmagic_project::MaskMode::Subtract, filmagic_project::MaskMode::Intersect]);
    assert!(is(px(&s, 0, 40, 90), RED) && is(px(&s, 0, 300, 90), GREY3) && is(px(&s, 0, 160, 90), GREY3), "together they are the picture");
    s.execute("clip.enable", json!({"clips": [a, b]})).unwrap();
    assert_eq!(px(&s, 0, 40, 90)[3], 0.0, "the first hole is still a hole");
    assert_eq!(px(&s, 0, 300, 90)[3], 0.0, "and so is the second");
    assert!(is(px(&s, 0, 160, 90), GREY3), "between them the clip shows");
    // the second layer alone is its own region and nothing of the first
    s.execute("clip.enable", json!({"clips": [b, c]})).unwrap();
    assert!(is(px(&s, 0, 300, 90), GREY3));
    assert_eq!((px(&s, 0, 40, 90)[3], px(&s, 0, 160, 90)[3]), (0.0, 0.0));
}

#[test]
fn an_adjustment_inside_a_masked_effect_narrows_it() {
    let (mut s, c, _) = shot();
    // two regions of one effect instance cannot be made by the command (each call adds its own
    // effect), but a region kept on a clip that is already cut down must not bring the rest back
    let top = json!({"x": 0.0, "y": 0.0, "w": 1.0, "h": 0.5});
    s.execute("layers.extract", json!({"clip": c, "region": top, "feather": 0, "cutOut": true})).unwrap();
    let dir = tmp_dir("layers-narrow");
    let r = s
        .execute(
            "layers.cleanPlate",
            json!({"clip": c, "samples": 9, "feather": 0, "region": {"x": 0.0, "y": 0.0, "w": 0.5, "h": 1.0}, "path": dir.join("p.png").to_string_lossy()}),
        )
        .unwrap();
    let plate = r["clip"].as_u64().unwrap();
    // the plate stands in for the clip, hole included: only the lower left is its own
    let layer = s.active_sequence().unwrap().video_tracks.iter().flat_map(|t| &t.items).find(|i| i.id.0 != c && i.id.0 != plate).unwrap().id.0;
    s.execute("clip.enable", json!({"clips": [c, layer]})).unwrap();
    assert!(is(px(&s, 0, 40, 140), GREY3), "{:?}", px(&s, 0, 40, 140));
    assert_eq!(px(&s, 0, 40, 40)[3], 0.0, "the plate does not fill the hole the clip has");
    assert_eq!(px(&s, 0, 280, 140)[3], 0.0, "nor reach outside its region");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_plate_keeps_step_with_a_trimmed_clip_that_moves() {
    let (mut s, c, shot_seq) = shot();
    s.execute("edit.clear", json!({"clips": [c]})).unwrap();
    // the second half of the shot, sliding 80 px to the right while it plays
    let c =
        s.execute("timeline.place", json!({"item": shot_seq, "track": "V1", "seconds": 0.0, "audio": false, "sourceIn": {"seconds": 1.0}})).unwrap()["clips"]
            [0]
        .as_u64()
        .unwrap();
    let (first, last) = (clip(&s, c).start, s.sequence_rate().tick_of(23));
    assert_eq!(clip(&s, c).source_in, Tick(254_016_000_000));
    s.execute("effects.setParam", json!({"clip": c, "effect": "motion", "param": "position", "value": [160.0, 90.0], "time": first.0, "keyframe": true}))
        .unwrap();
    s.execute("effects.setParam", json!({"clip": c, "effect": "motion", "param": "position", "value": [240.0, 90.0], "time": last.0, "keyframe": true}))
        .unwrap();
    assert_eq!(px(&s, 23, 40, 20)[3], 0.0, "the clip has slid away from the left edge");
    let dir = tmp_dir("layers-step");
    let r = s.execute("layers.cleanPlate", json!({"clip": c, "samples": 9, "path": dir.join("p.png").to_string_lossy()})).unwrap();
    let plate = clip(&s, r["clip"].as_u64().unwrap());
    assert_eq!((plate.source_in, plate.speed, plate.reverse), (Tick(254_016_000_000), 1.0, false));
    // the plate slides with the clip it stands in for: it never shows where the clip is not
    assert!(is(px(&s, 0, 40, 20), GREY3));
    assert_eq!(px(&s, 23, 40, 20)[3], 0.0, "the plate stayed behind: {:?}", px(&s, 23, 40, 20));
    assert!(is(px(&s, 23, 120, 20), GREY3));
    // and it took the block out wherever the clip now shows it
    for f in [0, 12, 23] {
        let img = s.try_render_program_at(1.0, s.sequence_rate().tick_of(f)).unwrap();
        let red = (0..img.w).filter(|x| is(img.get(*x, 90), RED)).count();
        assert_eq!(red, 0, "frame {f}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_reversed_shot_keeps_its_direction_in_a_retimed_region() {
    let (mut s, c, _) = shot();
    s.execute("clip.speedDuration", json!({"clips": [c], "speed": 100, "reverse": true})).unwrap();
    assert!(clip(&s, c).reverse && is(px(&s, 0, 280, 90), RED), "the block now starts on the right");
    let r = s.execute("layers.retime", json!({"clip": c, "region": {"x": 0.5, "y": 0.0, "w": 0.5, "h": 1.0}, "speed": 50, "feather": 0})).unwrap();
    let l = clip(&s, r["layer"].as_u64().unwrap());
    assert!(l.reverse, "the layer plays the way the shot does");
    assert_eq!((l.speed, l.start, l.end()), (0.5, clip(&s, c).start, clip(&s, c).end()));
    // both start from the shot's last frame; ten frames in, the slowed half has gone back half as far
    assert!(is(px(&s, 0, 280, 90), RED));
    assert!(is(px(&s, 10, 254, 90), RED), "{:?}", px(&s, 10, 254, 90));
}

#[test]
fn a_clip_too_long_to_track_is_refused_before_anything_is_made() {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name": "long", "width": 320, "height": 180, "fps": 24.0})).unwrap();
    let m = s.execute("file.newColorMatte", json!({"color": "#808080", "width": 320, "height": 180, "seconds": 200.0})).unwrap()["item"].as_u64().unwrap();
    let c = s.execute("timeline.place", json!({"item": m, "track": "V1", "seconds": 0.0})).unwrap()["clips"][0].as_u64().unwrap();
    let region = json!({"x": 0.1, "y": 0.1, "w": 0.3, "h": 0.3});
    let dir = tmp_dir("layers-long");
    let path = dir.join("p.png").to_string_lossy().into_owned();
    for (cmd, p) in [
        ("layers.extract", json!({"clip": c, "region": region, "follow": true})),
        ("layers.lift", json!({"clip": c, "region": region, "follow": true, "path": path})),
        ("layers.adjust", json!({"clip": c, "region": region, "follow": true, "effect": "gaussian_blur"})),
    ] {
        let e = s.execute(cmd, p.clone()).unwrap_err().to_string();
        assert!(e.contains("4800") && e.contains("3000"), "{cmd}: {e}");
        refuses(&mut s, cmd, &[p]);
    }
    assert!(!std::path::Path::new(&path).exists(), "no plate was written for a lift that was refused");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_night_shot_stays_night_across_midnight() {
    let (mut s, c, _) = shot();
    let r = s.execute("color.relight", json!({"clip": c, "time": "22:00", "toTime": "02:00"})).unwrap();
    // ten at night, midnight (once, though the table has it as 0 and as 24), two in the morning
    assert_eq!((r["keyframes"].as_u64(), r["hours"].as_f64(), r["toTime"].as_str()), (Some(3), Some(4.0), Some("02:00")), "{r}");
    let e = clip(&s, c).effects.iter().find(|e| e.effect == "lumetri").unwrap().clone();
    let (temp, exposure) = (&e.params["temperature"], &e.params["exposure"]);
    assert_eq!(temp.keyframes.len(), 3);
    assert!(temp.keyframes.windows(2).all(|w| w[0].time < w[1].time));
    let rate = s.sequence_rate();
    for frame in [0, 12, 24, 36, 47] {
        let t = clip(&s, c).effect_time_at(rate.tick_of(frame));
        assert!(
            temp.f64_at(t) < -35.0 && exposure.f64_at(t) < -1.3,
            "frame {frame}: noon in the middle of the night ({}, {})",
            temp.f64_at(t),
            exposure.f64_at(t)
        );
    }
    // midnight to midnight, and an hour to itself, are no move at all
    for p in [json!({"clip": c, "time": "24:00", "toTime": "00:00"}), json!({"clip": c, "time": 7.5, "toTime": 7.5})] {
        let r = s.execute("color.relight", p).unwrap();
        assert_eq!((r["keyframes"].as_u64(), r["hours"].as_f64()), (Some(0), Some(0.0)), "{r}");
        assert!(r["toTime"].is_null() && r["warnings"].as_array().unwrap().iter().all(|w| !w.as_str().unwrap().contains("midnight")));
    }
}

#[test]
fn a_held_frame_takes_the_light_of_the_first_hour() {
    let (mut s, c, _) = shot();
    s.execute("timeline.select", json!({"clips": [c]})).unwrap();
    s.execute("clip.frameHoldOptions", json!({"holdOn": "in", "holdFilters": true})).unwrap();
    assert!(clip(&s, c).frame_hold.is_some() && clip(&s, c).hold_filters);
    let r = s.execute("color.relight", json!({"clip": c, "time": "12:00", "toTime": "21:00"})).unwrap();
    assert_eq!(r["keyframes"], 0, "one moment of media has no time for the light to change in: {r}");
    let e = clip(&s, c).effects.iter().find(|e| e.effect == "lumetri").unwrap().clone();
    assert!(e.params["exposure"].keyframes.is_empty());
    // the light the result names (noon), not the last stop's (night)
    assert_eq!(e.params["exposure"].value, ParamValue::Float(crate::relight::grade_at(12.0, 1.0)[2]));
    assert_eq!(e.params["temperature"].value, ParamValue::Float(0.0));
}

#[test]
fn relight_says_what_it_writes_over_and_respects_a_lock() {
    let (mut s, c, _) = shot();
    // a clip nobody has graded: nothing to warn about
    let r = s.execute("color.relight", json!({"clip": c, "time": "18:00"})).unwrap();
    assert!(r["warnings"].as_array().unwrap().is_empty(), "{r}");
    s.execute("edit.undo", json!({})).unwrap();
    // a grade someone made by hand is replaced, and the result says so
    s.execute("effects.apply", json!({"clips": [c], "effect": "lumetri"})).unwrap();
    s.execute("effects.setParam", json!({"clip": c, "effect": "lumetri", "param": "exposure", "value": 0.8})).unwrap();
    let r = s.execute("color.relight", json!({"clip": c, "time": "18:00"})).unwrap();
    assert!(r["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("already had Color Grading")), "{r}");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(grade(&s, c, "exposure"), 0.8, "undo brings the old grade back");
    // a locked track is left alone, even when both effects are already there
    s.execute("color.relight", json!({"clip": c, "time": "18:00"})).unwrap();
    s.execute("timeline.setTrack", json!({"track": "V1", "locked": true})).unwrap();
    assert!(s.active_sequence().unwrap().video_tracks[0].locked);
    refuses(&mut s, "color.relight", &[json!({"clip": c, "time": "06:00"}), json!({"clip": c, "time": "06:00", "toTime": "09:00"})]);
}

/// The bounds of a clip's first opacity mask at its first keyframe, in clip pixels.
fn mask_bounds(s: &Session, id: u64) -> (f64, f64, f64, f64, usize) {
    let mask = &clip(s, id).effects.iter().find(|e| e.effect == "opacity").unwrap().masks[0];
    let ParamValue::Path(path) = &mask.path.value else { panic!("no path") };
    let (xs, ys): (Vec<f64>, Vec<f64>) = path.vertices.iter().map(|v| (v.p.x, v.p.y)).unzip();
    let r = |v: f64| (v * 100.0).round() / 100.0;
    (
        r(xs.iter().copied().fold(f64::MAX, f64::min)),
        r(ys.iter().copied().fold(f64::MAX, f64::min)),
        r(xs.iter().copied().fold(f64::MIN, f64::max)),
        r(ys.iter().copied().fold(f64::MIN, f64::max)),
        xs.len(),
    )
}

#[test]
fn a_region_read_off_the_frame_lands_on_the_clip_as_placed() {
    let (mut s, c, _) = shot();
    // the shot at half size in the top-left quarter of the frame: its block now crosses at y = 45
    s.execute("effects.setParam", json!({"clip": c, "effect": "motion", "param": "scale", "value": 50.0})).unwrap();
    s.execute("effects.setParam", json!({"clip": c, "effect": "motion", "param": "position", "value": [80.0, 45.0]})).unwrap();
    assert!(is(px(&s, 0, 20, 45), RED) && px(&s, 0, 200, 120)[3] == 0.0);
    // the left of the frame around the block, as seen: frame pixels 0…48 by 27…63
    let seen = json!({"space": "frame", "x": 0.0, "y": 0.15, "w": 0.15, "h": 0.2});
    let layer = s.execute("layers.extract", json!({"clip": c, "region": seen, "feather": 0})).unwrap()["layer"].as_u64().unwrap();
    // which is the clip's own pixels 0…96 by 54…126
    assert_eq!(mask_bounds(&s, layer), (0.0, 54.0, 96.0, 126.0, 4));
    s.execute("clip.enable", json!({"clips": [c]})).unwrap();
    assert!(is(px(&s, 0, 20, 45), RED) && is(px(&s, 0, 40, 60), GREY3), "the layer is what was pointed at");
    assert_eq!((px(&s, 0, 60, 45)[3], px(&s, 0, 20, 70)[3]), (0.0, 0.0), "and nothing beside it");
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    // the same numbers without `space` are fractions of the clip's own picture: another place
    let layer =
        s.execute("layers.extract", json!({"clip": c, "region": {"x": 0.0, "y": 0.15, "w": 0.15, "h": 0.2}, "feather": 0})).unwrap()["layer"].as_u64().unwrap();
    assert_eq!(mask_bounds(&s, layer), (0.0, 27.0, 48.0, 63.0, 4));
    s.execute("edit.undo", json!({})).unwrap();
    // points go the same way
    let r = s
        .execute("layers.extract", json!({"clip": c, "region": {"space": "frame", "points": [[0.0, 0.15], [0.15, 0.15], [0.15, 0.35]]}, "feather": 0}))
        .unwrap();
    assert_eq!(mask_bounds(&s, r["layer"].as_u64().unwrap()), (0.0, 54.0, 96.0, 126.0, 3));
    s.execute("edit.undo", json!({})).unwrap();
    // a clip turned in the frame: an upright box as seen is a turned one in the clip's own picture
    s.execute("effects.setParam", json!({"clip": c, "effect": "motion", "param": "rotation", "value": 90.0})).unwrap();
    let r = s
        .execute("layers.extract", json!({"clip": c, "region": {"space": "frame", "shape": "ellipse", "x": 0.2, "y": 0.2, "w": 0.1, "h": 0.1}, "feather": 0}))
        .unwrap();
    let (x0, y0, x1, y1, n) = mask_bounds(&s, r["layer"].as_u64().unwrap());
    assert_eq!(n, 32, "an outline, turned with the clip");
    // 32 by 18 frame pixels at half size and a quarter turn: 36 by 64 of the clip's
    assert!(((x1 - x0) - 36.0).abs() < 0.5 && ((y1 - y0) - 64.0).abs() < 0.5, "{:?}", (x0, y0, x1, y1));
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    // where the frame shows nothing of the clip there is nothing to take
    let region = |r: Value| json!({"clip": c, "region": r});
    refuses(
        &mut s,
        "layers.extract",
        &[
            region(json!({"space": "frame", "x": 0.6, "y": 0.6, "w": 0.2, "h": 0.2})),
            region(json!({"space": "frame", "points": [[0.6, 0.6], [0.9, 0.6], [0.9, 0.9]]})),
            region(json!({"space": "screen", "x": 0.1, "y": 0.1, "w": 0.2, "h": 0.2})),
            region(json!({"space": 3, "x": 0.1, "y": 0.1, "w": 0.2, "h": 0.2})),
            region(json!({"space": "frame", "atSeconds": 99.0, "x": 0.1, "y": 0.1, "w": 0.2, "h": 0.2})),
            region(json!({"space": "frame", "atSeconds": "now", "x": 0.1, "y": 0.1, "w": 0.2, "h": 0.2})),
        ],
    );
    // a clip that moves: the region is read at the frame it was seen in
    let last = s.sequence_rate().tick_of(47);
    s.execute("effects.setParam", json!({"clip": c, "effect": "motion", "param": "position", "value": [80.0, 45.0], "time": 0, "keyframe": true})).unwrap();
    s.execute("effects.setParam", json!({"clip": c, "effect": "motion", "param": "position", "value": [240.0, 45.0], "time": last.0, "keyframe": true}))
        .unwrap();
    let late = json!({"space": "frame", "atSeconds": last.seconds(), "x": 0.5, "y": 0.15, "w": 0.15, "h": 0.2});
    let layer = s.execute("layers.extract", json!({"clip": c, "region": late, "feather": 0})).unwrap()["layer"].as_u64().unwrap();
    assert_eq!(mask_bounds(&s, layer), (0.0, 54.0, 96.0, 126.0, 4), "the same part of the clip, seen 160 px further right at the end");
}

#[test]
fn what_is_made_from_a_clip_stacks_in_the_order_it_was_made() {
    let (mut s, c, _) = shot();
    let cover_clip = cover(&mut s);
    let dir = tmp_dir("layers-stack");
    // a plate over the left half takes the block out of it
    s.execute(
        "layers.cleanPlate",
        json!({"clip": c, "samples": 9, "feather": 0, "region": {"x": 0.0, "y": 0.0, "w": 0.5, "h": 1.0}, "path": dir.join("p.png").to_string_lossy()}),
    )
    .unwrap();
    assert!(is(px(&s, 30, 60, 90), GREY3), "frame 30: past the cover, the left half is plate");
    // a layer of the clip made afterwards lies over that plate, not under it: the block is back inside the layer
    let region = json!({"x": 0.0, "y": 0.25, "w": 0.3, "h": 0.5});
    let layer = s.execute("layers.extract", json!({"clip": c, "region": region, "feather": 0})).unwrap()["layer"].as_u64().unwrap();
    // at frame 5 the block is at x = 66; the cover still hides everything there
    assert!(is(px(&s, 5, 66, 90), BLUE));
    s.execute("clip.enable", json!({"clips": [layer]})).unwrap();
    let q = s.active_sequence().unwrap();
    let of = |id: u64| q.video_tracks.iter().position(|t| t.items.iter().any(|i| i.id.0 == id)).unwrap();
    let plate = q.video_tracks[1].items[0].id.0;
    assert_eq!((of(c), of(plate), of(layer)), (0, 1, 2), "the clip, its plate, its layer");
    assert!(q.video_tracks[3].items.iter().any(|i| i.item != clip(&s, c).item), "and the cover above them all");
    s.execute("clip.enable", json!({"clips": [layer]})).unwrap();
    assert!(is(px(&s, 30, 60, 90), GREY3) && is(px(&s, 5, 66, 90), BLUE), "with the layer back on, the cover still covers");
    // a second layer goes above the first
    let second = s.execute("layers.retime", json!({"clip": c, "region": region, "feather": 0, "speed": 200})).unwrap()["layer"].as_u64().unwrap();
    let q = s.active_sequence().unwrap();
    let of = |id: u64| q.video_tracks.iter().position(|t| t.items.iter().any(|i| i.id.0 == id)).unwrap();
    assert_eq!((of(c), of(plate), of(layer), of(second)), (0, 1, 2, 3));
    assert_eq!(q.video_tracks.len(), 6, "three tracks to begin with and three made");
    assert_eq!(of(cover_clip), 4, "the cover has moved up with each");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn another_clip_of_the_same_footage_is_not_part_of_the_stack() {
    let (mut s, c, shot_seq) = shot();
    // a punch-in: the same shot again on the track above for the first second, twice the size
    let punch =
        s.execute("timeline.place", json!({"item": shot_seq, "track": "V2", "seconds": 0.0, "audio": false, "duration": {"seconds": 1.0}})).unwrap()["clips"]
            [0]
        .as_u64()
        .unwrap();
    s.execute("effects.setParam", json!({"clip": punch, "effect": "motion", "param": "scale", "value": 200.0})).unwrap();
    let before: Vec<[f32; 4]> = [(0, 40, 90), (10, 100, 60), (30, 60, 90)].iter().map(|&(f, x, y)| px(&s, f, x, y)).collect();
    let layer =
        s.execute("layers.extract", json!({"clip": c, "region": {"x": 0.0, "y": 0.25, "w": 0.4, "h": 0.5}, "feather": 0})).unwrap()["layer"].as_u64().unwrap();
    let q = s.active_sequence().unwrap();
    let of = |id: u64| q.video_tracks.iter().position(|t| t.items.iter().any(|i| i.id.0 == id)).unwrap();
    assert_eq!((of(c), of(layer), of(punch)), (0, 1, 2), "the layer goes under the punch-in, which was laid over the clip on purpose");
    // and the picture is what it was: no unscaled patch over the punch-in
    let after: Vec<[f32; 4]> = [(0, 40, 90), (10, 100, 60), (30, 60, 90)].iter().map(|&(f, x, y)| px(&s, f, x, y)).collect();
    assert_eq!(before, after);
    // a layer made from the clip is part of its stack: the next one goes over it, still under the punch-in
    let second =
        s.execute("layers.extract", json!({"clip": c, "region": {"x": 0.5, "y": 0.25, "w": 0.4, "h": 0.5}, "feather": 0})).unwrap()["layer"].as_u64().unwrap();
    let q = s.active_sequence().unwrap();
    let of = |id: u64| q.video_tracks.iter().position(|t| t.items.iter().any(|i| i.id.0 == id)).unwrap();
    assert_eq!((of(c), of(layer), of(second), of(punch)), (0, 1, 2, 3));
}

#[test]
fn relight_leaves_a_regions_own_grade_alone() {
    let (mut s, c, _) = shot();
    // a grade on a region only (a face lifted a stop)
    let region = json!({"x": 0.0, "y": 0.0, "w": 0.3, "h": 0.3});
    s.execute("layers.adjust", json!({"clip": c, "region": region, "effect": "lumetri", "params": {"exposure": 1.0}, "feather": 0})).unwrap();
    let outside_before = px(&s, 30, 300, 170);
    let r = s.execute("color.relight", json!({"clip": c, "time": "22:00", "lightStrength": 0})).unwrap();
    assert!(r["warnings"].as_array().unwrap().is_empty(), "nothing over the whole picture was replaced: {r}");
    let grades: Vec<&filmagic_project::EffectInstance> = clip(&s, c).effects.iter().filter(|e| e.effect == "lumetri").collect();
    assert_eq!(grades.len(), 2, "the region's grade, and the shot's");
    let (masked, whole) = (grades.iter().find(|e| !e.masks.is_empty()).unwrap(), grades.iter().find(|e| e.masks.is_empty()).unwrap());
    assert_eq!(masked.params["exposure"].value, ParamValue::Float(1.0), "the region keeps its own grade");
    assert_eq!(whole.params["exposure"].value, ParamValue::Float(crate::relight::grade_at(22.0, 1.0)[2]));
    // night falls on the whole picture, not on the region alone
    let outside = px(&s, 30, 300, 170);
    assert!(outside[0] < outside_before[0] * 0.6, "{outside:?} against {outside_before:?}");
    // again: the same two effects, no third
    s.execute("color.relight", json!({"clip": c, "time": "06:30", "lightStrength": 0})).unwrap();
    assert_eq!(clip(&s, c).effects.iter().filter(|e| e.effect == "lumetri").count(), 2);
}

#[test]
fn no_light_switches_a_light_off_and_says_so() {
    let (mut s, c, _) = shot();
    s.execute("color.relight", json!({"clip": c, "time": "17:00", "lightStrength": 0.8})).unwrap();
    let lit = clip(&s, c).effects.iter().find(|e| e.effect == "lighting_effects").unwrap().clone();
    assert!(lit.enabled);
    let r = s.execute("color.relight", json!({"clip": c, "time": "17:00", "lightStrength": 0})).unwrap();
    assert!(r["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("switched off")), "{r}");
    let off = clip(&s, c).effects.iter().find(|e| e.effect == "lighting_effects").unwrap().clone();
    assert!(!off.enabled);
    assert_eq!(off.params, lit.params, "its settings are as they were");
    // already off: nothing more to say
    let r = s.execute("color.relight", json!({"clip": c, "time": "17:00", "lightStrength": 0})).unwrap();
    assert!(!r["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("Lighting Effects")), "{r}");
    // and a clip that never had one gets none
    let (mut bare, b, _) = shot();
    bare.execute("color.relight", json!({"clip": b, "time": "17:00", "lightStrength": 0})).unwrap();
    assert!(!clip(&bare, b).effects.iter().any(|e| e.effect == "lighting_effects"));
}

#[test]
fn a_whole_day_passes_from_midnight_to_midnight() {
    let (mut s, c, _) = shot();
    let r = s.execute("color.relight", json!({"clip": c, "time": "00:00", "toTime": "24:00"})).unwrap();
    // every hour of the table: midnight, eleven in between, midnight
    assert_eq!((r["keyframes"].as_u64(), r["hours"].as_f64(), r["toTime"].as_str()), (Some(13), Some(24.0), Some("24:00")), "{r}");
    let e = clip(&s, c).effects.iter().find(|e| e.effect == "lumetri").unwrap().clone();
    let rate = s.sequence_rate();
    let exposure = |frame: i64| e.params["exposure"].f64_at(clip(&s, c).effect_time_at(rate.tick_of(frame)));
    assert!(exposure(0) < -1.5 && exposure(47) < -1.5 && exposure(24) > -0.1, "night, noon, night: {} {} {}", exposure(0), exposure(24), exposure(47));
    // the other way about it is no time at all
    let r = s.execute("color.relight", json!({"clip": c, "time": "24:00", "toTime": "00:00"})).unwrap();
    assert_eq!((r["keyframes"].as_u64(), r["hours"].as_f64()), (Some(0), Some(0.0)));
}

#[test]
fn every_layer_command_says_what_it_did_to_the_picture_and_what_to_watch() {
    let (mut s, c, _) = shot();
    let dir = tmp_dir("layers-report");
    let region = json!({"x": 0.0, "y": 0.25, "w": 0.3, "h": 0.5});
    for (cmd, p, reads) in [
        // a layer shows what was already there
        ("layers.extract", json!({"clip": c, "region": region, "feather": 0}), "none"),
        ("layers.extract", json!({"clip": c, "region": region, "feather": 0, "cutOut": true}), "none"),
        // the subject is back over its plate
        ("layers.lift", json!({"clip": c, "region": region, "feather": 0, "samples": 9, "path": dir.join("a.png").to_string_lossy()}), "none"),
        // half speed inside the region: the block is somewhere else
        ("layers.retime", json!({"clip": c, "region": {"x": 0.0, "y": 0.0, "w": 0.6, "h": 1.0}, "feather": 0, "speed": 50}), "strong"),
        ("layers.adjust", json!({"clip": c, "region": region, "feather": 0, "effect": "tint", "params": {"amount": 100}}), "strong"),
    ] {
        let r = s.execute(cmd, p.clone()).unwrap();
        assert!(r["warnings"].is_array(), "{cmd}: {r}");
        assert_eq!(r["pictureChange"]["reads"], reads, "{cmd} {p}: {}", r["pictureChange"]);
        s.execute("edit.undo", json!({})).unwrap();
    }
    let _ = std::fs::remove_dir_all(dir);
}
