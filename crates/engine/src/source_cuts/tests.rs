//! Cuts inside a source file: a movie of three demo scenes (cuts at frames 20 and 36, 24 fps)
//! made with our own ProRes encoder.

use filmagic_media::DemoScene;
use filmagic_project::{ClipId, ItemId};
use serde_json::{Value, json};

use super::{MAX_EDIT_FRAMES, check};
use crate::Session;
use crate::media_test_util::{make_shots_movie, session_with, tmp_dir};

/// A session whose sequence holds the three-shot movie on V1 from 0; (session, item, clip).
fn three_shots(name: &str) -> (Session, ItemId, ClipId) {
    let dir = tmp_dir(name);
    let path = dir.join("shots.mov");
    make_shots_movie(&path, &[(DemoScene::OceanSunset, 20), (DemoScene::CityNight, 16), (DemoScene::Forest, 14)], 160, 90);
    let (s, items, _) = session_with(&[&path]);
    let c = s.active_sequence().unwrap().video_tracks[0].items[0].id;
    (s, items[0], c)
}

fn warnings(v: &Value) -> Vec<String> {
    v["warnings"].as_array().map(|a| a.iter().filter_map(|w| w.as_str().map(String::from)).collect()).unwrap_or_default()
}

#[test]
fn lists_the_cuts_of_an_item_and_reuses_what_it_decoded() {
    let (mut s, item, clip) = three_shots("srccuts-list");
    let r = s.execute("media.sourceCuts", json!({"item": item.0})).unwrap();
    let frames: Vec<i64> = r["cuts"].as_array().unwrap().iter().map(|c| c["frame"].as_i64().unwrap()).collect();
    assert_eq!(frames, vec![20, 36], "{r}");
    assert_eq!(r["complete"], true);
    assert!((r["cuts"][0]["mediaSeconds"].as_f64().unwrap() - 20.0 / 24.0).abs() < 1e-3);
    // a second look decodes nothing
    let mut budget = 0;
    let again = check(&mut s, item, 0, 49, &mut budget).unwrap();
    assert_eq!((again.cuts, again.complete), (vec![20, 36], true));
    // through a clip: its timeline times too
    let r = s.execute("media.sourceCuts", json!({"clip": clip.0})).unwrap();
    assert!((r["cuts"][1]["timelineSeconds"].as_f64().unwrap() - 36.0 / 24.0).abs() < 1e-3, "{r}");
}

#[test]
fn a_razor_a_few_frames_from_a_source_cut_warns() {
    let (mut s, _, clip) = three_shots("srccuts-razor");
    let r = s.execute("timeline.razor", json!({"clip": clip.0, "frame": 22})).unwrap();
    let w = warnings(&r);
    assert_eq!(w.len(), 1, "{r}");
    assert!(w[0].contains("2 frame(s) after a cut in its source at 0.833 s"), "{}", w[0]);
    assert_eq!(r["cuts"], 1, "the command's own result is kept: {r}");
    assert_eq!(r["sourceCuts"][0]["framesFromEdit"], 2);
}

#[test]
fn a_razor_on_or_far_from_a_source_cut_does_not_warn() {
    let (mut s, _, clip) = three_shots("srccuts-razor-ok");
    let r = s.execute("timeline.razor", json!({"clip": clip.0, "frame": 20})).unwrap();
    assert!(warnings(&r).is_empty(), "{r}");
    assert!(r["notes"][0].as_str().unwrap().contains("exactly on a cut"), "{r}");
    let r = s.execute("timeline.razor", json!({"frame": 28})).unwrap();
    assert!(r.get("warnings").is_none() && r.get("notes").is_none(), "{r}");
}

#[test]
fn slowing_a_clip_across_a_source_cut_warns_where_the_cut_is() {
    let (mut s, _, clip) = three_shots("srccuts-speed");
    // the first 30 frames: shot one, then six frames of shot two
    s.execute("timeline.razor", json!({"clip": clip.0, "frame": 30})).unwrap();
    let r = s.execute("clip.speedDuration", json!({"clips": [clip.0], "speed": 50, "ripple": true})).unwrap();
    let w = warnings(&r);
    assert_eq!(w.len(), 1, "{r}");
    assert!(w[0].contains("straddles a cut in its source at 0.833 s"), "{}", w[0]);
    assert_eq!(r["sourceCuts"][0]["kind"], "inside");
    // at half speed the cut plays at twice its media time
    assert!((r["sourceCuts"][0]["timelineSeconds"].as_f64().unwrap() - 40.0 / 24.0).abs() < 1e-3, "{r}");
}

#[test]
fn slowing_a_clip_inside_one_shot_does_not_warn() {
    let (mut s, _, clip) = three_shots("srccuts-speed-ok");
    s.execute("timeline.razor", json!({"clip": clip.0, "frame": 16})).unwrap();
    let r = s.execute("clip.speedDuration", json!({"clips": [clip.0], "speed": 50, "ripple": true})).unwrap();
    assert!(r.is_null(), "unchanged result: {r}");
}

#[test]
fn a_frame_hold_next_to_a_source_cut_warns() {
    let (mut s, _, clip) = three_shots("srccuts-hold");
    let t = s.sequence_rate().tick_of(37);
    let r = s.execute("clip.frameHold", json!({"clips": [clip.0], "time": t.0})).unwrap();
    let w = warnings(&r);
    assert!(w.iter().any(|w| w.contains("holds media frame") && w.contains("1 frame(s) from a cut in its source at 1.500 s")), "{r}");
    assert!(r["clips"].is_array(), "{r}");
}

#[test]
fn a_check_past_its_budget_says_what_was_left() {
    let (mut s, item, _) = three_shots("srccuts-budget");
    let mut budget = 10;
    let f = check(&mut s, item, 0, 49, &mut budget).unwrap();
    assert!(!f.complete);
    assert_eq!(budget, 0);
    assert!(f.checked.is_some_and(|(a, b)| a == 0 && b < 49), "{f:?}");
    assert!(MAX_EDIT_FRAMES > 0);
}

#[test]
fn hostile_parameters_are_refused_with_what_to_do() {
    let (mut s, item, clip) = three_shots("srccuts-hostile");
    for p in [
        json!({}),
        json!({"item": item.0, "clip": clip.0}),
        json!({"item": "five"}),
        json!({"item": -3}),
        json!({"item": 987_654}),
        json!({"clip": 987_654}),
        json!({"item": item.0, "fromSeconds": -1}),
        json!({"item": item.0, "fromSeconds": "soon"}),
        json!({"item": item.0, "fromSeconds": 2.0, "toSeconds": 1.0}),
    ] {
        let e = s.execute("media.sourceCuts", p.clone()).expect_err(&p.to_string());
        assert!(!e.to_string().is_empty());
    }
    // huge seconds are clamped to the media, not overflowed
    let r = s.execute("media.sourceCuts", json!({"item": item.0, "fromSeconds": 0, "toSeconds": 1e300})).unwrap();
    assert_eq!(r["complete"], true, "{r}");
    let r = s.execute("media.sourceCuts", json!({"item": item.0, "fromSeconds": 1e300}));
    assert!(r.is_err());
}

#[test]
fn time_remapping_a_clip_across_a_source_cut_warns() {
    let (mut s, _, clip) = three_shots("srccuts-remap");
    s.execute("timeline.razor", json!({"clip": clip.0, "frame": 30})).unwrap();
    let r = s.execute("effects.addKeyframe", json!({"clip": clip.0, "effect": "time_remap", "param": "speed", "time": 0})).unwrap();
    assert!(warnings(&r).iter().any(|w| w.contains("straddles a cut in its source at 0.833 s")), "{r}");
    // another effect change on the same clip does not repeat it
    let r = s.execute("effects.setParam", json!({"clip": clip.0, "effect": "opacity", "param": "opacity", "value": 80.0})).unwrap();
    assert!(r.get("warnings").is_none(), "{r}");
}

#[test]
fn an_item_board_marks_the_cuts_inside_its_source() {
    let (mut s, item, _) = three_shots("srccuts-board");
    let dir = tmp_dir("srccuts-board-out");
    let path = dir.join("b.png").to_string_lossy().into_owned();
    let r = s.execute("story.board", json!({"path": path, "item": item.0, "count": 5})).unwrap();
    assert_eq!(r["sourceCuts"], json!([0.833, 1.5]), "{r}");
    assert_eq!(r["sourceCutsComplete"], true);
    let shots: Vec<i64> = r["cells"].as_array().unwrap().iter().map(|c| c["sourceShot"].as_i64().unwrap()).collect();
    assert_eq!(shots, vec![1, 1, 2, 2, 3], "{r}");
    assert_eq!(r["cells"][2]["cutBefore"], json!([0.833]));
}

#[test]
fn a_board_samples_a_short_range_densely() {
    let (mut s, item, _) = three_shots("srccuts-dense");
    let dir = tmp_dir("srccuts-dense-out");
    let path = dir.join("b.png").to_string_lossy().into_owned();
    // the item: 12 frames per second across one second
    let r = s.execute("story.board", json!({"path": path, "item": item.0, "fps": 12, "fromSeconds": 0.5, "toSeconds": 1.5})).unwrap();
    assert_eq!(r["cells"].as_array().unwrap().len(), 12, "{r}");
    assert_eq!(r["step"], 0.083);
    // the sequence: every frame of half a second
    let r = s.execute("story.board", json!({"path": path, "fps": 24, "fromSeconds": 0.5, "toSeconds": 1.0})).unwrap();
    let at: Vec<i64> = r["cells"].as_array().unwrap().iter().map(|c| c["at"]["frame"].as_i64().unwrap()).collect();
    assert_eq!(at, (12..24).collect::<Vec<_>>(), "{r}");
    let r = s.execute("story.board", json!({"path": path, "step": 0.25, "toSeconds": 1.0})).unwrap();
    assert_eq!(r["cells"].as_array().unwrap().len(), 4, "{r}");
    for p in [
        json!({"path": path, "fps": 0}),
        json!({"path": path, "fps": -12}),
        json!({"path": path, "fps": 1e9}),
        json!({"path": path, "fps": "fast"}),
        json!({"path": path, "step": 0}),
        json!({"path": path, "fps": 12, "step": 0.1}),
        // 2 s at 60 fps is more than a board holds
        json!({"path": path, "fps": 60, "toSeconds": 2.0}),
        json!({"path": path, "fps": 12, "fromSeconds": 5.0, "toSeconds": 1.0}),
    ] {
        let e = s.execute("story.board", p.clone()).expect_err(&p.to_string());
        assert!(!e.to_string().is_empty());
    }
    let e = s.execute("story.board", json!({"path": path, "fps": 60, "toSeconds": 2.0})).unwrap_err().to_string();
    assert!(e.contains("at most 1.60 s apart"), "{e}");
}
