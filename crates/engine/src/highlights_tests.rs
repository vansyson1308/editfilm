//! `story.highlights` on synthetic media (#21): clicks at a rally's cadence, a noise swell for a
//! crowd, and a moving shape for motion.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::Session;
use crate::media_test_util::tmp_dir;

const SR: u32 = 48_000;

/// Quiet noise with a click at each of `clicks` (seconds) and a crowd-like swell over `swell`.
fn sound(seconds: f64, clicks: &[f64], swell: Option<(f64, f64)>, rate: u32) -> Vec<f32> {
    let n = (seconds * f64::from(rate)) as usize;
    let mut seed = 0x9e37_79b9u32;
    let mut noise = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        (seed as f32 / u32::MAX as f32) * 2.0 - 1.0
    };
    let mut x: Vec<f32> = (0..n).map(|_| 0.003 * noise()).collect();
    for &c in clicks {
        let a = (c * f64::from(rate)) as usize;
        for (k, v) in x.iter_mut().skip(a).take((rate / 300) as usize).enumerate() {
            *v += 0.6 * (-(k as f32) / (rate as f32 / 1600.0)).exp() * if k % 2 == 0 { 1.0 } else { -1.0 };
        }
    }
    if let Some((a, b)) = swell {
        for (i, v) in x.iter_mut().enumerate() {
            let t = i as f64 / f64::from(rate);
            if t >= a && t <= b {
                *v += 0.1 * ((t - a).min(b - t) / 0.3).min(1.0) as f32 * noise();
            }
        }
    }
    x
}

fn wav(dir: &Path, name: &str, x: &[f32], rate: u32) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, filmagic_media::wav::write_wav16(x, 1, rate)).unwrap();
    p
}

fn import(s: &mut Session, p: &Path) -> u64 {
    s.execute("file.import", json!({"paths": [p.to_string_lossy()]})).unwrap()["items"][0].as_u64().unwrap()
}

/// A 5-hit rally at 0.8 s with cheering after it, two lone hits, then a 3-hit rally.
fn match_clicks() -> Vec<f64> {
    let mut c: Vec<f64> = (0..5).map(|i| 4.0 + 0.8 * f64::from(i)).collect();
    c.extend([14.0, 19.0]);
    c.extend((0..3).map(|i| 25.0 + f64::from(i)));
    c
}

fn run(s: &mut Session, p: Value) -> Value {
    s.execute("story.highlights", p.clone()).unwrap_or_else(|e| panic!("{p}: {e}"))
}

#[test]
fn rallies_and_cheering_in_a_sound_file() {
    let dir = tmp_dir("highlights-item");
    let path = wav(&dir, "match.wav", &sound(32.0, &match_clicks(), Some((7.8, 10.5)), SR), SR);
    let mut s = Session::default();
    let item = import(&mut s, &path);
    let r = run(&mut s, json!({"item": item, "wait": true}));
    assert_eq!(r["status"], "done", "{r}");
    assert_eq!(r["timeBase"], "media");
    assert_eq!(r["impacts"], 10, "{r}");
    let times: Vec<f64> = r["impactTimes"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
    for (t, c) in times.iter().zip(match_clicks()) {
        assert!((t - c).abs() < 0.05, "{t} vs {c}");
    }
    let first = &r["segments"][0];
    assert_eq!((first["kind"].as_str(), first["impacts"].as_u64()), (Some("rally"), Some(5)), "{r}");
    let cues: Vec<&str> = first["cues"].as_array().unwrap().iter().map(|c| c.as_str().unwrap()).collect();
    assert!(cues.contains(&"rally") && cues.contains(&"cheer"), "{cues:?}");
    assert!(first["suggestedIn"]["seconds"].as_f64().unwrap() <= 3.5);
    assert!(first["suggestedOut"]["seconds"].as_f64().unwrap() >= 10.0, "the cheering is kept: {first}");
    assert!((first["cadenceSeconds"].as_f64().unwrap() - 0.8).abs() < 0.05);
    assert_eq!(r["cheers"].as_array().unwrap().len(), 1);
    assert_eq!(r["analysed"], json!({"sound": true, "picture": false}));
    // the same media again: from the cache, in another mode, with no media read
    let again = run(&mut s, json!({"item": item, "wait": true, "mode": "rally", "limit": 1}));
    assert_eq!(again["cached"], true);
    assert_eq!(again["segments"].as_array().unwrap().len(), 1);
    assert_eq!(again["segments"][0]["start"], first["start"]);
    // markers on the item's media, in one undo step
    let undo = s.history.undo.len();
    let r = run(&mut s, json!({"item": item, "wait": true, "markers": true}));
    let added = r["markers"].as_array().unwrap().len();
    assert!(added >= 2, "{r}");
    let marks = s.project.item(filmagic_project::ItemId(item)).unwrap().as_media().unwrap().markers.len();
    assert_eq!(marks, added);
    assert_eq!(s.history.undo.len(), undo + 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_timeline_clip_is_read_in_sequence_time_with_markers() {
    let dir = tmp_dir("highlights-clip");
    let path = wav(&dir, "match.wav", &sound(32.0, &match_clicks(), Some((7.8, 10.5)), SR), SR);
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name": "M", "video": 1, "audio": 1, "fps": 25})).unwrap();
    let item = import(&mut s, &path);
    // placed 10 s into the sequence, from 2 s into the media
    let c = s.execute("timeline.place", json!({"item": item, "track": "A1", "seconds": 10.0})).unwrap()["clips"][0].as_u64().unwrap();
    s.execute("timeline.trim", json!({"clip": c, "edge": "in", "mode": "regular", "deltaFrames": 50})).unwrap();
    let it = s.active_sequence().unwrap().find_item(filmagic_project::ClipId(c)).unwrap().1.clone();
    assert!((it.source_in.seconds() - 2.0).abs() < 1e-6, "{:?}", it.source_in.seconds());
    let offset = it.start.seconds() - it.source_in.seconds();
    let r = run(&mut s, json!({"clip": c, "wait": true, "markers": true}));
    assert_eq!(r["timeBase"], "sequence");
    let first = &r["segments"][0];
    assert_eq!(first["kind"], "rally", "{r}");
    let media_start = first["media"]["start"].as_f64().unwrap();
    assert!((first["start"]["seconds"].as_f64().unwrap() - (media_start + offset)).abs() < 0.05, "{first}");
    let t0 = r["impactTimes"][0].as_f64().unwrap();
    assert!((t0 - (4.0 + offset)).abs() < 0.05, "{t0} vs {}", 4.0 + offset);
    let n = r["markers"].as_array().unwrap().len();
    assert_eq!(s.active_sequence().unwrap().markers.len(), n);
    assert!(s.active_sequence().unwrap().markers.iter().all(|m| m.name.starts_with("Highlight") && m.duration.0 > 0));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn moving_shapes_are_a_motion_peak() {
    let dir = tmp_dir("highlights-motion");
    // 25 fps, 160 frames: a square crosses the frame between frames 60 and 100
    let paths: Vec<String> = (0..160u32)
        .map(|n| {
            let p = dir.join(format!("m{n:04}.png"));
            let mut img = image::RgbaImage::from_pixel(128, 72, image::Rgba([40, 40, 40, 255]));
            if (60..100).contains(&n) {
                let x0 = (n - 60) * 2;
                for y in 20..50 {
                    for x in x0..(x0 + 30).min(128) {
                        img.put_pixel(x, y, image::Rgba([250, 250, 250, 255]));
                    }
                }
            }
            img.save(&p).unwrap();
            p.to_string_lossy().to_string()
        })
        .collect();
    let mut s = Session::default();
    s.execute("prefs.set", json!({"key": "media.indeterminateTimebase", "value": "25"})).unwrap();
    let item = s.execute("file.import", json!({"paths": [paths[0]], "imageSequence": true})).unwrap()["items"][0].as_u64().unwrap();
    let r = run(&mut s, json!({"item": item, "wait": true, "fps": 10}));
    assert_eq!(r["analysed"], json!({"sound": false, "picture": true}), "{r}");
    let first = &r["segments"][0];
    assert_eq!(first["kind"], "peak", "{r}");
    assert!(first["cues"].as_array().unwrap().contains(&json!("motion")));
    let (a, b) = (first["start"]["seconds"].as_f64().unwrap(), first["end"]["seconds"].as_f64().unwrap());
    assert!(a <= 2.5 && b >= 3.8 && a >= 1.5 && b <= 4.8, "{a}..{b}");
    // without the picture there is nothing to read
    let e = s.execute("story.highlights", json!({"item": item, "picture": false})).unwrap_err().to_string();
    assert!(e.contains("no sound"), "{e}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_job_runs_in_the_background_and_can_be_cancelled() {
    let dir = tmp_dir("highlights-job");
    let rate = 8_000;
    let short = wav(&dir, "short.wav", &sound(12.0, &[2.0, 2.8, 3.6, 4.4], None, rate), rate);
    let long = wav(&dir, "long.wav", &sound(1800.0, &[], None, rate), rate);
    let mut s = Session::default();
    let item = import(&mut s, &short);
    let r = run(&mut s, json!({"item": item}));
    assert_eq!(r["status"], "running", "{r}");
    let job = r["job"].as_u64().unwrap();
    let mut result = Value::Null;
    for _ in 0..2000 {
        result = run(&mut s, json!({"job": job}));
        if result["status"] != "running" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(result["status"], "done", "{result}");
    assert_eq!(result["impacts"], 4);
    assert!(s.execute("jobs.list", json!({})).unwrap().as_array().unwrap().iter().any(|j| j["id"] == job));
    // a long one, cancelled at once
    let item = import(&mut s, &long);
    let r = run(&mut s, json!({"item": item, "fromSeconds": 0.0, "toSeconds": 1800.0}));
    let job = r["job"].as_u64().unwrap();
    s.execute("jobs.cancel", json!({"job": job})).unwrap();
    let mut result = Value::Null;
    for _ in 0..4000 {
        result = run(&mut s, json!({"job": job}));
        if result["status"] != "running" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(result["status"], "cancelled", "{result}");
    assert!(s.project.item(filmagic_project::ItemId(item)).unwrap().as_media().unwrap().markers.is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn hostile_parameters_are_refused_without_a_change() {
    let dir = tmp_dir("highlights-hostile");
    let path = wav(&dir, "x.wav", &sound(3.0, &[1.0], None, SR), SR);
    let mut s = Session::default();
    let item = import(&mut s, &path);
    let snap = (*s.project).clone();
    let cases = [
        json!({}),
        json!({"item": item, "clip": 1}),
        json!({"item": "x"}),
        json!({"item": -1}),
        json!({"item": 999_999}),
        json!({"clip": 999_999}),
        json!({"item": item, "mode": "fast"}),
        json!({"item": item, "mode": 1}),
        json!({"item": item, "cadence": [2.0, 1.0]}),
        json!({"item": item, "cadence": [0.0, 1.0]}),
        json!({"item": item, "cadence": "fast"}),
        json!({"item": item, "cadence": [1.0]}),
        json!({"item": item, "minHits": 0}),
        json!({"item": item, "minHits": 1e12}),
        json!({"item": item, "limit": 0}),
        json!({"item": item, "limit": 1000}),
        json!({"item": item, "fps": 0}),
        json!({"item": item, "fps": 1000}),
        json!({"item": item, "minSeconds": 10, "maxSeconds": 5}),
        json!({"item": item, "minSeconds": -1}),
        json!({"item": item, "fromSeconds": -1}),
        json!({"item": item, "fromSeconds": 2.0, "toSeconds": 1.0}),
        json!({"item": item, "fromSeconds": "a"}),
        json!({"item": item, "toSeconds": 1e300}),
        json!({"item": item, "wait": "yes"}),
        json!({"item": item, "markers": 1}),
        json!({"job": 424_242}),
        json!({"job": "x"}),
        json!([1]),
    ];
    for p in &cases {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.execute("story.highlights", p.clone())));
        assert!(r.is_ok(), "panicked on {p}");
        assert!(r.unwrap().is_err(), "accepted {p}");
        assert_eq!(*s.project, snap, "{p} changed the project");
    }
    // a span beyond the cap is refused with what to send instead
    let e = crate::highlights::span(&json!({}), filmagic_time::Tick::ZERO, filmagic_time::Tick::from_seconds_f64(7300.0)).unwrap_err().to_string();
    assert!(e.contains("fromSeconds") && e.contains("7200"), "{e}");
    assert!(crate::highlights::span(&json!({"toSeconds": 7200.0}), filmagic_time::Tick::ZERO, filmagic_time::Tick::from_seconds_f64(7300.0)).is_ok());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_replaced_project_or_a_late_cancel_keeps_no_result() {
    let dir = tmp_dir("highlights-epoch");
    let path = wav(&dir, "match.wav", &sound(12.0, &[2.0, 2.8, 3.6, 4.4], None, SR), SR);
    // the job finishes, then the project is replaced before its result is polled
    let mut s = Session::default();
    let item = import(&mut s, &path);
    let job = run(&mut s, json!({"item": item, "markers": true}))["job"].as_u64().unwrap();
    let finished = |s: &Session| s.jobs.iter().find(|j| j.id == job).is_some_and(|j| j.progress.finished.load(std::sync::atomic::Ordering::Relaxed));
    for _ in 0..4000 {
        if finished(&s) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(finished(&s));
    s.document_epoch = s.document_epoch.wrapping_add(1);
    let again = import(&mut s, &path);
    let r = run(&mut s, json!({"job": job}));
    assert_eq!(r["status"], "failed", "{r}");
    assert!(r["error"].as_str().unwrap().contains("replaced"), "{r}");
    for id in [item, again] {
        assert!(s.project.item(filmagic_project::ItemId(id)).is_none_or(|i| i.as_media().is_none_or(|m| m.markers.is_empty())), "no markers were written");
    }
    // cancelled after its cues were stored: dropped
    let job = run(&mut s, json!({"item": again, "markers": true}))["job"].as_u64().unwrap();
    for _ in 0..4000 {
        if s.jobs.iter().find(|j| j.id == job).is_some_and(|j| j.progress.finished.load(std::sync::atomic::Ordering::Relaxed)) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    s.execute("jobs.cancel", json!({"job": job})).unwrap();
    let r = run(&mut s, json!({"job": job}));
    assert_eq!(r["status"], "cancelled", "{r}");
    assert!(s.project.item(filmagic_project::ItemId(again)).unwrap().as_media().unwrap().markers.is_empty());
    let _ = std::fs::remove_dir_all(dir);
}
