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
    assert!(w.iter().any(|w| w.contains("runs off the frame edge")), "{w:?}");
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
        json!({"clip": video}),
    ] {
        assert!(s.execute("layout.check", p.clone()).is_err(), "{p}");
    }
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
