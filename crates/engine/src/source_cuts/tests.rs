//! Cuts inside a source file: a movie of three demo scenes (cuts at frames 20 and 36, 24 fps)
//! made with our own ProRes encoder, and a long-GOP H.264 file (ffmpeg, fixture generator only)
//! for what an edit costs.

use std::time::{Duration, Instant};

use filmagic_media::DemoScene;
use filmagic_project::{ClipId, ItemId};
use serde_json::{Value, json};

use super::cached;
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

/// [`three_shots`] with its source's cuts already found (as `media.sourceCuts` with `wait` does).
fn known_shots(name: &str) -> (Session, ItemId, ClipId) {
    let (mut s, item, clip) = three_shots(name);
    let r = s.execute("media.sourceCuts", json!({"item": item.0, "wait": true})).unwrap();
    assert_eq!(r["complete"], true, "{r}");
    (s, item, clip)
}

fn warnings(v: &Value) -> Vec<String> {
    v["warnings"].as_array().map(|a| a.iter().filter_map(|w| w.as_str().map(String::from)).collect()).unwrap_or_default()
}

/// Wait for job `id` to finish (at most a minute).
fn wait_job(s: &Session, id: u64) {
    let t0 = Instant::now();
    while !s.jobs.iter().find(|j| j.id == id).is_some_and(|j| j.progress.finished.load(std::sync::atomic::Ordering::Relaxed)) {
        assert!(t0.elapsed() < Duration::from_secs(60), "job {id} did not finish");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn lists_the_cuts_of_an_item_and_reuses_what_it_decoded() {
    let (mut s, item, clip) = three_shots("srccuts-list");
    let r = s.execute("media.sourceCuts", json!({"item": item.0, "wait": true})).unwrap();
    let frames: Vec<i64> = r["cuts"].as_array().unwrap().iter().map(|c| c["frame"].as_i64().unwrap()).collect();
    assert_eq!(frames, vec![20, 36], "{r}");
    assert_eq!(r["complete"], true);
    assert!((r["cuts"][0]["mediaSeconds"].as_f64().unwrap() - 20.0 / 24.0).abs() < 1e-3);
    // what is cached answers without decoding
    let again = cached(&s, item, 0, 49).unwrap();
    assert_eq!((again.cuts, again.complete), (vec![20, 36], true));
    // through a clip: its timeline times too, and no job
    let jobs = s.jobs.len();
    let r = s.execute("media.sourceCuts", json!({"clip": clip.0})).unwrap();
    assert!((r["cuts"][1]["timelineSeconds"].as_f64().unwrap() - 36.0 / 24.0).abs() < 1e-3, "{r}");
    assert_eq!(s.jobs.len(), jobs, "nothing left to check");
}

#[test]
fn without_wait_the_check_runs_in_the_background() {
    let (mut s, item, _) = three_shots("srccuts-bg");
    let r = s.execute("media.sourceCuts", json!({"item": item.0})).unwrap();
    assert_eq!(r["complete"], false, "{r}");
    let job = r["job"].as_u64().unwrap();
    assert!(r["hint"].as_str().unwrap().contains("jobs.list"), "{r}");
    // asking again while it runs joins the same job
    let r2 = s.execute("media.sourceCuts", json!({"item": item.0})).unwrap();
    assert!(r2["complete"] == true || r2["job"] == job, "{r2}");
    wait_job(&s, job);
    let r = s.execute("media.sourceCuts", json!({"item": item.0})).unwrap();
    assert_eq!((r["complete"].clone(), r["cuts"].as_array().map(Vec::len)), (json!(true), Some(2)), "{r}");
}

#[test]
fn an_edit_never_decodes_it_queues_the_check_and_the_next_edit_warns() {
    let (mut s, _, clip) = three_shots("srccuts-queue");
    let r = s.execute("timeline.razor", json!({"clip": clip.0, "frame": 22})).unwrap();
    assert!(warnings(&r).is_empty(), "nothing is known yet: {r}");
    let note = r["notes"][0].as_str().unwrap();
    assert!(note.contains("background check is running"), "{r}");
    let job: u64 = s.jobs.iter().map(|j| j.id).max().unwrap();
    wait_job(&s, job);
    // the next edit near the same cut knows it
    s.execute("edit.undo", json!({})).unwrap();
    let r = s.execute("timeline.razor", json!({"clip": clip.0, "frame": 22})).unwrap();
    assert!(warnings(&r).iter().any(|w| w.contains("2 frame(s) after a cut in its source at 0.833 s")), "{r}");
}

#[test]
fn a_razor_a_few_frames_from_a_source_cut_warns() {
    let (mut s, _, clip) = known_shots("srccuts-razor");
    let r = s.execute("timeline.razor", json!({"clip": clip.0, "frame": 22})).unwrap();
    let w = warnings(&r);
    assert_eq!(w.len(), 1, "{r}");
    assert!(w[0].contains("2 frame(s) after a cut in its source at 0.833 s"), "{}", w[0]);
    assert_eq!(r["cuts"], 1, "the command's own result is kept: {r}");
    assert_eq!(r["sourceCuts"][0]["framesFromEdit"], 2);
}

#[test]
fn a_razor_on_or_far_from_a_source_cut_does_not_warn() {
    let (mut s, _, clip) = known_shots("srccuts-razor-ok");
    let r = s.execute("timeline.razor", json!({"clip": clip.0, "frame": 20})).unwrap();
    assert!(warnings(&r).is_empty(), "{r}");
    assert!(r["notes"][0].as_str().unwrap().contains("exactly on a cut"), "{r}");
    let r = s.execute("timeline.razor", json!({"frame": 28})).unwrap();
    assert!(r.get("warnings").is_none() && r.get("notes").is_none(), "{r}");
}

#[test]
fn slowing_a_clip_across_a_source_cut_warns_where_the_cut_is() {
    let (mut s, _, clip) = known_shots("srccuts-speed");
    // the first 30 frames: shot one, then six frames of shot two
    s.execute("timeline.razor", json!({"clip": clip.0, "frame": 30})).unwrap();
    let r = s.execute("clip.speedDuration", json!({"clips": [clip.0], "speed": 50, "ripple": true})).unwrap();
    let w = warnings(&r);
    assert_eq!(w.len(), 1, "{r}");
    assert!(w[0].contains("straddles a cut in its source at 0.833 s"), "{}", w[0]);
    assert_eq!(r["sourceCuts"][0]["kind"], "inside");
    // at half speed the cut plays at twice its media time
    assert!((r["sourceCuts"][0]["timelineSeconds"].as_f64().unwrap() - 40.0 / 24.0).abs() < 1e-3, "{r}");
    // set back to 100 %: it plays its frames once, nothing to warn about
    let r = s.execute("clip.speedDuration", json!({"clips": [clip.0], "speed": 100, "ripple": true})).unwrap();
    assert!(warnings(&r).is_empty(), "{r}");
}

#[test]
fn a_reversed_clip_across_a_source_cut_warns_at_its_timeline_time() {
    let (mut s, _, clip) = known_shots("srccuts-reverse");
    s.execute("timeline.razor", json!({"clip": clip.0, "frame": 30})).unwrap();
    let r = s.execute("clip.speedDuration", json!({"clips": [clip.0], "speed": 100, "reverse": true})).unwrap();
    assert_eq!(warnings(&r).len(), 1, "{r}");
    // played backward, frame 20 of 0..30 comes 10 frames in
    let t = r["sourceCuts"][0]["timelineSeconds"].as_f64().unwrap();
    assert!((t - 10.0 / 24.0).abs() < 1.5 / 24.0, "{r}");
}

#[test]
fn slowing_a_clip_inside_one_shot_does_not_warn() {
    let (mut s, _, clip) = known_shots("srccuts-speed-ok");
    s.execute("timeline.razor", json!({"clip": clip.0, "frame": 16})).unwrap();
    let r = s.execute("clip.speedDuration", json!({"clips": [clip.0], "speed": 50, "ripple": true})).unwrap();
    assert!(r.is_null(), "unchanged result: {r}");
}

#[test]
fn a_frame_hold_next_to_a_source_cut_warns() {
    let (mut s, _, clip) = known_shots("srccuts-hold");
    let t = s.sequence_rate().tick_of(37);
    let r = s.execute("clip.frameHold", json!({"clips": [clip.0], "time": t.0})).unwrap();
    let w = warnings(&r);
    assert!(w.iter().any(|w| w.contains("holds media frame") && w.contains("1 frame(s) from a cut in its source at 1.500 s")), "{r}");
    assert!(r["clips"].is_array(), "{r}");
}

#[test]
fn time_remapping_a_clip_across_a_source_cut_warns_and_100_percent_does_not() {
    let (mut s, _, clip) = known_shots("srccuts-remap");
    s.execute("timeline.razor", json!({"clip": clip.0, "frame": 30})).unwrap();
    // a keyframe at a constant 100 % is no retiming
    let r = s.execute("effects.addKeyframe", json!({"clip": clip.0, "effect": "time_remap", "param": "speed", "time": 0})).unwrap();
    assert!(warnings(&r).is_empty(), "{r}");
    let r = s.execute("effects.setParam", json!({"clip": clip.0, "effect": "time_remap", "param": "speed", "value": 50.0, "time": 0})).unwrap();
    assert!(warnings(&r).iter().any(|w| w.contains("straddles a cut in its source at 0.833 s") && w.contains("about")), "{r}");
    assert_eq!(r["sourceCuts"][0]["timelineApproximate"], true);
    // another effect change on the same clip does not repeat it
    let r = s.execute("effects.setParam", json!({"clip": clip.0, "effect": "opacity", "param": "opacity", "value": 80.0})).unwrap();
    assert!(r.get("warnings").is_none(), "{r}");
}

#[test]
fn a_refreshed_or_relinked_file_starts_afresh() {
    let (mut s, item, _) = known_shots("srccuts-refresh");
    assert!(cached(&s, item, 0, 49).unwrap().complete);
    // the file changed on disk and was read again
    s.source_cuts.forget(item);
    assert!(!cached(&s, item, 0, 49).unwrap().complete);
    // a different file behind the same item: the key no longer matches
    s.execute("media.sourceCuts", json!({"item": item.0, "wait": true})).unwrap();
    let p = std::sync::Arc::make_mut(&mut s.project);
    if let Some(m) = p.item_mut(item).and_then(|i| i.as_media_mut()) {
        m.info.file_size = Some(m.info.file_size.unwrap_or(0) + 1);
    }
    assert!(!cached(&s, item, 0, 49).unwrap().complete, "the cache follows what the media is");
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
        json!({"item": item.0, "wait": "yes"}),
    ] {
        let e = s.execute("media.sourceCuts", p.clone()).expect_err(&p.to_string());
        assert!(!e.to_string().is_empty());
    }
    // huge seconds are clamped to the media, not overflowed
    let r = s.execute("media.sourceCuts", json!({"item": item.0, "fromSeconds": 0, "toSeconds": 1e300, "wait": true})).unwrap();
    assert_eq!(r["complete"], true, "{r}");
    let r = s.execute("media.sourceCuts", json!({"item": item.0, "fromSeconds": 1e300}));
    assert!(r.is_err());
}

#[test]
fn an_item_board_marks_the_cuts_already_found() {
    let (mut s, item, _) = three_shots("srccuts-board");
    let dir = tmp_dir("srccuts-board-out");
    let path = dir.join("b.png").to_string_lossy().into_owned();
    // nothing known yet: the board does not wait, it says a check is running
    let r = s.execute("story.board", json!({"path": path, "item": item.0, "count": 5})).unwrap();
    assert_eq!(r["sourceCutsComplete"], false, "{r}");
    assert!(r["notes"][0].as_str().unwrap().contains("background check"), "{r}");
    let job: u64 = s.jobs.iter().map(|j| j.id).max().unwrap();
    wait_job(&s, job);
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

/// The cost an agent saw: long-GOP 1080p H.264, where a decode costs tens of milliseconds. A
/// razor, a speed change and a board of the item return without decoding the source.
#[test]
fn edits_on_long_gop_h264_return_quickly() {
    let Some(ffmpeg) = filmagic_testkit::oracle::ffmpeg_or_skip("source cuts cost") else { return };
    let dir = filmagic_testkit::fixtures_dir("engine/source_cuts");
    let path = dir.join("gop60_1080p30_20s.mp4");
    if !path.exists() {
        let tmp = filmagic_testkit::temp_path(&path);
        let ok = std::process::Command::new(&ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=1920x1080:rate=30:duration=20"])
            .args(["-c:v", "libx264", "-g", "60", "-bf", "2", "-pix_fmt", "yuv420p"])
            .arg(&tmp)
            .status()
            .is_ok_and(|st| st.success());
        if !ok {
            filmagic_testkit::oracle::skip("source cuts cost", "ffmpeg could not encode H.264");
            return;
        }
        std::fs::rename(&tmp, &path).unwrap();
    }
    let (mut s, items, _) = session_with(&[&path]);
    let clip = s.active_sequence().unwrap().video_tracks[0].items[0].id;
    let t0 = Instant::now();
    let r = s.execute("timeline.razor", json!({"clip": clip.0, "seconds": 9.0})).unwrap();
    let razor = t0.elapsed();
    let t0 = Instant::now();
    let r2 = s.execute("clip.speedDuration", json!({"clips": [clip.0], "speed": 50, "ripple": true})).unwrap();
    let speed = t0.elapsed();
    let out = dir.join("board.png").to_string_lossy().into_owned();
    let t0 = Instant::now();
    s.execute("story.board", json!({"path": out, "item": items[0].0, "count": 4, "width": 96})).unwrap();
    let board = t0.elapsed();
    eprintln!("long-GOP H.264: razor {razor:?}, speed {speed:?}, board {board:?}");
    // a decode of all the frames these touch would take many seconds
    assert!(razor < Duration::from_millis(1500) && speed < Duration::from_millis(1500), "razor {razor:?} {r}, speed {speed:?} {r2}");
    assert!(board < Duration::from_secs(10), "board {board:?}");
    // stop the background checks this queued
    for j in &s.jobs {
        j.progress.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// 23.976 fps footage in a 24 fps sequence: the cut is found in the media's own frames and the
/// razor's distance from it is counted in them too, with timeline times from the time parameters.
#[test]
fn mixed_rates_count_in_the_media_frames() {
    let dir = tmp_dir("srccuts-mixed");
    let path = dir.join("shots.mov");
    let media = filmagic_time::FrameRate::FPS_23_976;
    crate::media_test_util::make_shots_movie_at(&path, &[(DemoScene::OceanSunset, 20), (DemoScene::CityNight, 16), (DemoScene::Forest, 14)], 160, 90, media);
    let (mut s, items, _) = session_with(&[&path]);
    let seq = s.state.active_sequence.unwrap();
    std::sync::Arc::make_mut(&mut s.project).sequence_mut(seq).unwrap().settings.frame_rate = filmagic_time::FrameRate::FPS_24;
    let clip = s.active_sequence().unwrap().video_tracks[0].items[0].id;
    let r = s.execute("media.sourceCuts", json!({"item": items[0].0, "wait": true})).unwrap();
    let frames: Vec<i64> = r["cuts"].as_array().unwrap().iter().map(|c| c["frame"].as_i64().unwrap()).collect();
    assert_eq!(frames, vec![20, 36], "{r}");
    assert!((r["cuts"][0]["mediaSeconds"].as_f64().unwrap() - 20.0 * 1001.0 / 24000.0).abs() < 1e-3, "{r}");
    // two media frames after the cut, given in seconds (a time parameter)
    let at = media.tick_of(22).seconds() + 0.001;
    let r = s.execute("timeline.razor", json!({"clip": clip.0, "seconds": at})).unwrap();
    let w = warnings(&r);
    assert!(w.iter().any(|w| w.contains("frame(s) after a cut in its source at 0.834 s")), "{r}");
}
