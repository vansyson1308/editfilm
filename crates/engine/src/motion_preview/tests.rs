//! `preview.motion` on the demo project.

use serde_json::json;

use crate::Session;
use crate::media_test_util::tmp_dir;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

#[test]
fn writes_a_small_animated_gif_of_a_short_range() {
    let mut s = demo();
    let dir = tmp_dir("motion-gif");
    let path = dir.join("p.gif").to_string_lossy().into_owned();
    let r = s.execute("preview.motion", json!({"path": path, "fromSeconds": 1.0, "toSeconds": 2.0, "fps": 10, "width": 160})).unwrap();
    assert_eq!((r["format"].as_str(), r["width"].as_u64(), r["frames"].as_u64()), (Some("gif"), Some(160), Some(10)), "{r}");
    let bytes = std::fs::read(&path).unwrap();
    assert!(bytes.starts_with(b"GIF89a"), "an animated GIF");
    assert_eq!(r["bytes"].as_u64(), Some(bytes.len() as u64));
    // reading it moved nothing
    assert!(s.history.undo.is_empty());
}

#[test]
fn writes_an_mp4_too() {
    let mut s = demo();
    let dir = tmp_dir("motion-mp4");
    let path = dir.join("p.mp4").to_string_lossy().into_owned();
    let r = s.execute("preview.motion", json!({"path": path, "fromSeconds": 0.0, "toSeconds": 0.5, "width": 128})).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(bytes.get(4..8), Some(&b"ftyp"[..]), "{r}");
    assert_eq!(r["format"], "mp4");
}

#[test]
fn hostile_parameters_are_refused_with_what_to_do() {
    let mut s = demo();
    let dir = tmp_dir("motion-hostile");
    let gif = dir.join("p.gif").to_string_lossy().into_owned();
    for p in [
        json!({}),
        json!({"path": ""}),
        json!({"path": dir.join("p.png").to_string_lossy()}),
        json!({"path": gif, "fromSeconds": 0, "toSeconds": 30}),
        json!({"path": gif, "fromSeconds": 2, "toSeconds": 1}),
        json!({"path": gif, "fromSeconds": -1}),
        json!({"path": gif, "fromSeconds": "soon"}),
        json!({"path": gif, "fromSeconds": 1e300}),
        json!({"path": gif, "fps": 0}),
        json!({"path": gif, "fps": 1000}),
        json!({"path": gif, "width": 10}),
        json!({"path": gif, "width": 1e9}),
        json!({"path": gif, "width": "big"}),
    ] {
        let e = s.execute("preview.motion", p.clone()).expect_err(&p.to_string()).to_string();
        assert!(!e.is_empty());
    }
    let e = s.execute("preview.motion", json!({"path": gif, "fromSeconds": 0, "toSeconds": 30})).unwrap_err().to_string();
    assert!(e.contains("10 s at most"), "{e}");
}

#[test]
fn the_longest_side_and_the_gif_frames_are_capped() {
    let mut s = demo();
    let dir = tmp_dir("motion-caps");
    // a vertical sequence: 640 is its height, not its width
    s.execute("file.newSequence", json!({"name": "Vertical", "fps": 24, "width": 1080, "height": 1920, "video": 1, "audio": 1})).unwrap();
    s.execute("graphics.newRectangle", json!({"position": [540, 960], "size": [400, 400], "seconds": 10.0, "time": 0})).unwrap();
    let mp4 = dir.join("v.mp4").to_string_lossy().into_owned();
    let r = s.execute("preview.motion", json!({"path": mp4, "fromSeconds": 0.0, "toSeconds": 0.5, "width": 640})).unwrap();
    assert_eq!((r["width"].as_u64(), r["height"].as_u64()), (Some(360), Some(640)), "{r}");
    assert!(r["notes"].as_array().unwrap().iter().any(|n| n.as_str().is_some_and(|n| n.contains("delete"))), "{r}");
    // 10 s at 30 fps would be 300 GIF frames: the rate comes down to fit 60
    let gif = dir.join("v.gif").to_string_lossy().into_owned();
    let r = s.execute("preview.motion", json!({"path": gif, "fromSeconds": 0.0, "toSeconds": 10.0, "fps": 30, "width": 64})).unwrap();
    assert_eq!(r["fps"], 6.0, "{r}");
    assert!(r["frames"].as_u64().unwrap() <= 60, "{r}");
    assert!(r["notes"][0].as_str().unwrap().contains("lowered"), "{r}");
}
