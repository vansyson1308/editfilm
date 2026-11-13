//! `layout.check` and `graphics.anchor` on a 320×180 still: flat grey with a patch of sharp
//! black and white stripes in the top-right corner (standing in for a burned-in scoreboard).

use serde_json::{Value, json};

use crate::Session;
use crate::media_test_util::{session_with, tmp_dir};

const W: usize = 320;
const H: usize = 180;
/// The busy patch: x 220..300, y 20..60.
const PATCH: [usize; 4] = [220, 20, 300, 60];

fn scoreboard_session(name: &str) -> Session {
    let dir = tmp_dir(name);
    let path = dir.join("picture.png");
    let mut img = filmagic_render::Image::new(W, H);
    for y in 0..H {
        for x in 0..W {
            let inside = (PATCH[0]..PATCH[2]).contains(&x) && (PATCH[1]..PATCH[3]).contains(&y);
            let v = if inside { if (x / 2 + y / 3) % 2 == 0 { 1.0 } else { 0.0 } } else { 0.45 };
            let i = (y * W + x) * 4;
            img.px[i..i + 4].copy_from_slice(&[v, v, v, 1.0]);
        }
    }
    let png = filmagic_export::still::encode(&img, filmagic_export::still::StillFormat::Png, 8).unwrap();
    std::fs::write(&path, png).unwrap();
    session_with(&[&path]).0
}

fn shape(s: &mut Session, pos: [f64; 2], size: [f64; 2]) -> u64 {
    let r = s.execute("graphics.newRectangle", json!({"position": pos, "size": size, "newClip": true, "seconds": 2.0, "time": 0})).unwrap();
    r["clip"].as_u64().unwrap()
}

fn item(r: &Value, clip: u64) -> &Value {
    r["items"].as_array().unwrap().iter().find(|i| i["clip"] == clip).unwrap()
}

#[test]
fn reports_overlaps_safe_areas_and_a_title_over_a_scoreboard() {
    let mut s = scoreboard_session("layout-check");
    // over the scoreboard
    let over = shape(&mut s, [260.0, 40.0], [60.0, 24.0]);
    // overlapping it, partly
    let cross = shape(&mut s, [230.0, 50.0], [40.0, 30.0]);
    // half off the left edge
    let off = shape(&mut s, [5.0, 100.0], [40.0, 20.0]);
    // in the calm middle, inside title safe
    let calm = shape(&mut s, [160.0, 100.0], [30.0, 20.0]);
    let r = s.execute("layout.check", json!({})).unwrap();
    assert_eq!(r["ok"], false, "{r}");
    assert_eq!(r["safeAreas"]["title"]["box"], json!([32.0, 18.0, 288.0, 162.0]));
    assert_eq!(item(&r, over)["under"]["reads"], "textLike", "{r}");
    assert_eq!(item(&r, calm)["under"]["reads"], "calm", "{r}");
    assert_eq!(item(&r, calm)["outside"], Value::Null);
    assert_eq!(item(&r, off)["outside"], "frame");
    // the scoreboard title reaches past the title-safe right edge (288) too
    assert_eq!(item(&r, over)["outside"], "titleSafe");
    let pairs: Vec<(u64, u64)> =
        r["overlaps"].as_array().unwrap().iter().map(|o| (o["a"]["clip"].as_u64().unwrap(), o["b"]["clip"].as_u64().unwrap())).collect();
    assert_eq!(pairs.len(), 1, "{r}");
    assert!(pairs[0] == (over, cross) || pairs[0] == (cross, over));
    let w: Vec<&str> = r["warnings"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert!(w.iter().any(|w| w.contains("text-like") && w.contains(&format!("graphic clip {over}"))), "{w:?}");
    assert!(w.iter().any(|w| w.contains("touches or runs off the frame edge")), "{w:?}");
    assert!(w.iter().any(|w| w.contains("overlap")), "{w:?}");
    // one clip only
    let r = s.execute("layout.check", json!({"clip": calm})).unwrap();
    assert_eq!((r["ok"].clone(), r["items"].as_array().unwrap().len()), (json!(true), 1), "{r}");
}

#[test]
fn captions_are_checked_too() {
    let mut s = scoreboard_session("layout-captions");
    s.execute("captions.add", json!({"text": "Rally seven", "time": 0, "durationSeconds": 1.5})).unwrap();
    let r = s.execute("layout.check", json!({})).unwrap();
    let caps: Vec<&Value> = r["items"].as_array().unwrap().iter().filter(|i| i["kind"] == "caption").collect();
    assert_eq!(caps.len(), 1, "{r}");
    assert!(caps[0]["samples"].as_array().is_some_and(|a| !a.is_empty()), "the caption draws: {r}");
}

#[test]
fn anchor_puts_a_graphic_in_a_corner_of_a_safe_area() {
    let mut s = scoreboard_session("layout-anchor");
    let c = shape(&mut s, [160.0, 90.0], [40.0, 20.0]);
    let r = s.execute("graphics.anchor", json!({"clip": c, "anchor": "topRight"})).unwrap();
    assert_eq!(r["box"], json!([248.0, 18.0, 288.0, 38.0]), "{r}");
    let r = s.execute("graphics.anchor", json!({"clip": c, "anchor": "bottomLeft", "safe": "action", "margin": 4})).unwrap();
    assert_eq!(r["box"], json!([20.0, 147.0, 60.0, 167.0]), "{r}");
    // what layout.check measures agrees
    let r = s.execute("layout.check", json!({"clip": c})).unwrap();
    assert_eq!(r["items"][0]["outside"], "titleSafe", "inside action safe, outside title safe: {r}");
    assert_eq!(s.history.undo.last().map(|h| h.0.as_str()), Some("Anchor Graphic"));
}

#[test]
fn a_template_can_be_applied_at_an_anchor() {
    let mut s = scoreboard_session("layout-template");
    let id = filmagic_project::gtemplate::builtin_templates().first().map(|t| t.id.clone()).unwrap();
    let undo = s.history.undo.len();
    let r = s.execute("graphics.template.apply", json!({"template": id, "anchor": "bottomLeft"})).unwrap();
    let b = r["anchored"]["box"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect::<Vec<_>>();
    assert!((b[0] - 32.0).abs() < 0.5, "{r}");
    assert_eq!(s.history.undo.len(), undo + 1, "one undo step");
    let e = s.execute("graphics.template.apply", json!({"template": id, "anchor": "nowhere"})).unwrap_err().to_string();
    assert!(e.contains("topLeft"), "{e}");
    assert_eq!(s.history.undo.len(), undo + 1, "a refused anchor leaves nothing behind");
}

#[test]
fn hostile_parameters_are_refused() {
    let mut s = scoreboard_session("layout-hostile");
    let c = shape(&mut s, [160.0, 90.0], [40.0, 20.0]);
    let video = s.active_sequence().unwrap().video_tracks[0].items[0].id.0;
    for p in [
        json!({"fromSeconds": -1}),
        json!({"fromSeconds": "now"}),
        json!({"toSeconds": 1e30}),
        json!({"fromSeconds": 3, "toSeconds": 1}),
        json!({"clip": 987_654}),
        json!({"clip": "x"}),
    ] {
        assert!(s.execute("layout.check", p.clone()).is_err(), "{p}");
    }
    // footage has no layout of its own: an empty check that says what to pass
    let r = s.execute("layout.check", json!({"clip": video})).unwrap();
    assert!(r["items"].as_array().is_some_and(Vec::is_empty) && r["notes"][0].as_str().unwrap().contains("not a graphic"), "{r}");
    for p in [
        json!({"clip": c}),
        json!({"clip": c, "anchor": 3}),
        json!({"clip": c, "anchor": "upLeft"}),
        json!({"clip": c, "anchor": "top", "safe": "inner"}),
        json!({"clip": c, "anchor": "top", "safe": 1}),
        json!({"clip": c, "anchor": "top", "margin": -2}),
        json!({"clip": c, "anchor": "top", "margin": 1e9}),
        json!({"clip": c, "anchor": "top", "margin": 200}),
        json!({"clip": 987_654, "anchor": "top"}),
    ] {
        assert!(s.execute("graphics.anchor", p.clone()).is_err(), "{p}");
    }
}

/// Inside a dip to black, a graphic alone is still only its own pixels (#16 review: the hidden
/// clips' transition filled the frame, so a title read as a full-frame box that ran off it).
#[test]
fn transitions_do_not_make_an_overlay_fill_the_frame() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let (w, h) = s.active_sequence().map(|q| (f64::from(q.settings.width), f64::from(q.settings.height))).unwrap();
    let has_transitions = s.active_sequence().unwrap().video_tracks.iter().any(|t| !t.transitions.is_empty());
    assert!(has_transitions, "the demo has transitions to check against");
    // a title inside the opening dip to black (0–1 s) and a caption inside the one at 12.9–13.9 s
    s.execute("graphics.newText", json!({"text": "Opening", "size": 80, "position": [960, 540], "time": 0, "seconds": 0.9, "newClip": true})).unwrap();
    let at = filmagic_time::Tick::from_seconds_f64(12.95).0;
    s.execute("captions.add", json!({"text": "Late line", "time": at, "durationSeconds": 0.8})).unwrap();
    let r = s.execute("layout.check", json!({})).unwrap();
    assert_eq!(r["items"].as_array().map(Vec::len), Some(2), "{r}");
    for item in r["items"].as_array().unwrap() {
        for sample in item["samples"].as_array().unwrap() {
            let b: Vec<f64> = sample["box"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
            assert!(!(b[0] <= 0.0 && b[1] <= 0.0 && b[2] >= w && b[3] >= h), "a full-frame box: {item}");
        }
    }
}

#[test]
fn a_bar_across_the_frame_is_a_note_and_captions_outside_are_said_once() {
    let mut s = scoreboard_session("layout-bar");
    let bar = shape(&mut s, [160.0, 170.0], [320.0, 20.0]);
    for (k, t) in [0.0, 0.5].iter().enumerate() {
        let tick = filmagic_time::Tick::from_seconds_f64(*t).0;
        s.execute("captions.add", json!({"text": format!("Line {k}"), "time": tick, "durationSeconds": 0.4})).unwrap();
    }
    let r = s.execute("layout.check", json!({})).unwrap();
    assert_eq!(item(&r, bar)["outside"], "spansFrame", "{r}");
    let w: Vec<&str> = r["warnings"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert!(!w.iter().any(|w| w.contains(&format!("graphic clip {bar} ")) && w.contains("frame edge")), "{w:?}");
    assert!(r["notes"].as_array().unwrap().iter().any(|n| n.as_str().is_some_and(|n| n.contains("full width"))), "{r}");
    let safe_lines: Vec<&&str> = w.iter().filter(|w| w.contains("caption") && w.contains("safe area")).collect();
    assert_eq!(safe_lines.len(), 1, "captions outside a safe area are reported together: {w:?}");
    assert!(safe_lines[0].starts_with("2 caption(s)"), "{w:?}");
}

#[test]
fn anchor_refuses_a_clip_its_motion_moves_and_fits_a_vertical_frame() {
    let mut s = scoreboard_session("layout-anchor-motion");
    let c = shape(&mut s, [160.0, 90.0], [40.0, 20.0]);
    s.execute("effects.setParam", json!({"clip": c, "effect": "motion", "param": "scale", "value": 150.0})).unwrap();
    let e = s.execute("graphics.anchor", json!({"clip": c, "anchor": "topLeft"})).unwrap_err().to_string();
    assert!(e.contains("Motion") && e.contains("effects.reset"), "{e}");
    // a 9:16 sequence: the title-safe area is the vertical frame's
    s.execute("file.newSequence", json!({"name": "Vertical", "fps": 24, "width": 180, "height": 320, "video": 2, "audio": 1})).unwrap();
    let r = s.execute("graphics.newRectangle", json!({"position": [90, 160], "size": [40, 20], "newClip": true, "seconds": 2.0, "time": 0})).unwrap();
    let v = r["clip"].as_u64().unwrap();
    let r = s.execute("graphics.anchor", json!({"clip": v, "anchor": "bottomRight"})).unwrap();
    // 20 % margin: 18 px each side across, 32 px each side down
    assert_eq!(r["box"], json!([122.0, 268.0, 162.0, 288.0]), "{r}");
    assert!(r["note"].as_str().unwrap().contains("playhead"), "{r}");
}
