//! Export wall time of a 1080p H.264 + AAC clip, exported to H.264 + AAC (MP4) through the same
//! `file.exportMedia` command the app runs, on the CPU (no GPU renderer, no hardware encoder).
//!
//! ```text
//! cargo run --release -p filmagic-engine --example bench_export -- [--runs N] [--secs S] [--format h264] [--keep out.mp4] [--source file.mp4]
//!     [--scenario clip|edit|demo] [--set '{"draft":true}'] [--hw auto|off] [--gpu auto|off] [--touch]
//! ```
//!
//! Scenarios: `clip` (the default) exports the source clip as it is; `edit` cuts it every two
//! seconds, puts a cross dissolve (video) and a constant-power crossfade (audio) on every cut, a
//! Lumetri grade on every other shot and a title over the whole sequence (the commands an agent
//! sends); `demo` exports the demo project's Main Edit (procedural footage: no decoding). `--set`
//! merges more `file.exportMedia` parameters into every run. `--touch` (the `edit` scenario) changes
//! the grade of one shot before every run after the first: with `--set '{"smartRender":true}'` that
//! is a re-export after a small change.
//!
//! The source is made once in `target/fixtures/export-bench/` unless `--source` names a file:
//! with ffmpeg when it is installed (the clip the documented numbers use), otherwise a procedural
//! demo scene with audio written by Filmagic's own H.264 and AAC encoders. Prints each run, the median and minimum, and the stage times of the
//! last run.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;

use filmagic_engine::Session;
use filmagic_export::{ExportSettings, Format};
use filmagic_media::generators::GeneratorSource;
use filmagic_media::{DemoScene, Generator, MediaSource, SharedSource};
use filmagic_project::{ItemId, ItemKind, Label, MediaClip, MediaRef, Project, SequenceSettings, TrackKind};
use filmagic_time::{FrameRate, Tick, TimeRange};
use serde_json::{Value, json};

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

/// The source clip: `secs` seconds of a procedural demo scene with its audio at 1080p23.976,
/// written once as H.264 + AAC by Filmagic's own encoders (no ffmpeg needed).
fn generated_fixture(secs: u32) -> Option<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/fixtures/export-bench");
    let path = dir.join(format!("citynight1080_{secs}s.mp4"));
    if path.exists() {
        return Some(path);
    }
    std::fs::create_dir_all(&dir).ok()?;
    let tmp = dir.join(format!(".tmp{}.mp4", std::process::id()));
    let (w, h, rate) = (1920, 1080, FrameRate::FPS_23_976);
    let dur = rate.tick_of(rate.frame_at(Tick::from_seconds_f64(f64::from(secs))));
    let scene = Generator::Demo(DemoScene::CityNight);
    let src = GeneratorSource::new(scene.clone(), w, h, rate, dur);
    let mut p = Project::new("fixture");
    let clip = MediaClip {
        media: MediaRef::Generator(scene),
        info: src.info().clone(),
        interpret: Default::default(),
        mark_in: None,
        mark_out: None,
        markers: vec![],
        offline: false,
        proxy: None,
        identity: None,
    };
    let item = p.add_item("scene", Label::Iris, ItemKind::Media(Box::new(clip)), None);
    let seq = p.new_sequence("s", SequenceSettings { width: w, height: h, frame_rate: rate, ..Default::default() }, 1, 1, None);
    let range = TimeRange::new(Tick::ZERO, dur);
    let mut v = p.make_track_item(item, TrackKind::Video, Tick::ZERO, range, rate)?;
    for e in &mut v.effects {
        filmagic_project::resolve_auto_points(e, (w, h), (w, h));
    }
    let a = p.make_track_item(item, TrackKind::Audio, Tick::ZERO, range, rate)?;
    let q = p.sequence_mut(seq)?;
    q.video_tracks[0].items.push(v);
    q.audio_tracks[0].items.push(a);
    let settings = ExportSettings { format: Format::H264, path: tmp.to_string_lossy().into_owned(), bitrate_kbps: 20_000, ..Default::default() };
    let shared: SharedSource = Arc::new(src);
    let provider = move |id: ItemId| (id == item).then(|| shared.clone());
    eprintln!("generating {}", path.display());
    filmagic_export::export(&Arc::new(p), seq, &settings, &provider, &Default::default()).ok()?;
    std::fs::rename(&tmp, &path).ok()?;
    Some(path)
}

/// The source made by ffmpeg (a fixture generator only): `testsrc2` with noise and a sine tone,
/// `libx264` CRF 20. None without ffmpeg.
fn ffmpeg_fixture(secs: u32) -> Option<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/fixtures/export-bench");
    let path = dir.join(format!("src1080_{secs}s.mp4"));
    if path.exists() {
        return Some(path);
    }
    std::fs::create_dir_all(&dir).ok()?;
    let tmp = dir.join(format!(".tmp{}.mp4", std::process::id()));
    let src = format!("testsrc2=s=1920x1080:r=24000/1001:d={secs},noise=alls=6:allf=t");
    let ok = Command::new(std::env::var_os("FILMAGIC_FFMPEG").unwrap_or_else(|| "ffmpeg".into()))
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", &src, "-f", "lavfi", "-i", &format!("sine=f=440:d={secs}")])
        .args(["-c:a", "aac", "-shortest", "-c:v", "libx264", "-preset", "fast", "-crf", "20", "-pix_fmt", "yuv420p"])
        .arg(&tmp)
        .status()
        .ok()?
        .success();
    if !ok {
        let _ = std::fs::remove_file(&tmp);
        return None;
    }
    std::fs::rename(&tmp, &path).ok()?;
    Some(path)
}

fn session(src: &Path) -> Session {
    let mut s = Session::default();
    let r = s.execute("file.import", json!({ "paths": [src.to_string_lossy()] })).expect("import");
    let item = ItemId(r["items"].as_array().and_then(|a| a.first()).and_then(Value::as_u64).expect("imported item"));
    let info = s.project.item(item).and_then(|i| i.as_media()).map(|m| m.info.clone()).expect("media");
    let v = info.video.clone().expect("video stream");
    let rate = v.frame_rate;
    s.edit("bench", |p, st| {
        let settings = SequenceSettings { width: v.width, height: v.height, frame_rate: rate, ..SequenceSettings::default() };
        let seq = p.new_sequence("Bench", settings, 1, 1, None);
        let d = p.item(item).map(|i| i.duration()).unwrap_or(Tick::ZERO);
        let range = TimeRange::new(Tick::ZERO, d);
        let mut vi = p.make_track_item(item, TrackKind::Video, Tick::ZERO, range, rate).expect("video item");
        for e in &mut vi.effects {
            filmagic_project::resolve_auto_points(e, (v.width, v.height), (v.width, v.height));
        }
        let ai = p.make_track_item(item, TrackKind::Audio, Tick::ZERO, range, rate).expect("audio item");
        let q = p.sequence_mut(seq).expect("sequence");
        q.video_tracks[0].items.push(vi);
        q.audio_tracks[0].items.push(ai);
        st.active_sequence = Some(seq);
        st.open_sequences = vec![seq];
        Ok(())
    })
    .expect("sequence");
    s
}

/// Cut the bench sequence into a typical edit: a cut every two seconds with a cross dissolve and
/// an audio crossfade on each, a Lumetri grade on every other shot and a title on V2.
fn make_edit(s: &mut Session) {
    let run = |s: &mut Session, id: &str, p: Value| s.execute(id, p).unwrap_or_else(|e| panic!("{id}: {e}"));
    let seq = s.state.active_sequence.expect("bench sequence");
    let dur = s.project.sequence(seq).map(|q| q.duration()).expect("sequence");
    let step = Tick::from_seconds_f64(2.0);
    let mut t = step;
    while t + Tick::from_seconds_f64(1.0) < dur {
        run(s, "timeline.razor", json!({"time": t.0}));
        t += step;
    }
    let clips = |s: &Session, video: bool| -> Vec<u64> {
        let q = s.project.sequence(seq).expect("sequence");
        let tracks = if video { &q.video_tracks } else { &q.audio_tracks };
        tracks[0].items.iter().map(|i| i.id.0).collect()
    };
    let video = clips(s, true);
    for (k, c) in video.iter().enumerate() {
        if k + 1 < video.len() {
            run(s, "sequence.applyVideoTransition", json!({"clip": c, "effect": "cross_dissolve", "edge": "out", "frames": 12}));
        }
        if k % 2 == 1 {
            run(s, "effects.apply", json!({"clips": [c], "effect": "lumetri"}));
            for (param, value) in [("temperature", 18.0), ("contrast", 22.0), ("saturation", 120.0)] {
                let _ = s.execute("effects.setParam", json!({"clip": c, "effect": "lumetri", "param": param, "value": value}));
            }
        }
    }
    let audio = clips(s, false);
    for c in audio.iter().take(audio.len().saturating_sub(1)) {
        run(s, "sequence.applyAudioTransition", json!({"clip": c, "effect": "constant_power", "edge": "out", "frames": 12}));
    }
    s.set_playhead(Tick::ZERO);
    run(s, "graphics.newText", json!({"text": "Filmagic export bench", "size": 96, "seconds": dur.seconds(), "track": 1, "time": 0}));
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let runs: usize = arg(&args, "--runs").and_then(|v| v.parse().ok()).unwrap_or(7).max(1);
    let secs: u32 = arg(&args, "--secs").and_then(|v| v.parse().ok()).unwrap_or(10).max(1);
    let format = arg(&args, "--format").unwrap_or_else(|| "h264".into());
    let src = match arg(&args, "--source") {
        Some(p) => PathBuf::from(p),
        None => ffmpeg_fixture(secs).or_else(|| generated_fixture(secs)).expect("could not write the source fixture (or pass --source)"),
    };
    let keep = arg(&args, "--keep").map(PathBuf::from);
    let dir = std::env::temp_dir().join(format!("filmagic-bench-export-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let out = keep.clone().unwrap_or_else(|| dir.join(format!("out.{}", if format == "prores" { "mov" } else { "mp4" })));
    let scenario = arg(&args, "--scenario").unwrap_or_else(|| "clip".into());
    let mut s = match scenario.as_str() {
        "demo" => {
            let mut s = Session::default();
            s.execute("file.openDemoProject", json!({})).expect("demo project");
            s
        }
        "edit" => {
            let mut s = session(&src);
            make_edit(&mut s);
            s
        }
        _ => session(&src),
    };
    let extra: Value = arg(&args, "--set").map(|v| serde_json::from_str(&v).expect("--set takes a JSON object")).unwrap_or_else(|| json!({}));
    let hw = arg(&args, "--hw").unwrap_or_else(|| "off".into());
    let gpu = arg(&args, "--gpu").unwrap_or_else(|| "off".into());
    let mut times = Vec::new();
    let mut stages = Vec::new();
    let touch = args.iter().any(|a| a == "--touch");
    for i in 0..runs {
        if touch && i > 0 {
            // a small change between runs: one graded shot gets another temperature
            let seq = s.state.active_sequence.expect("sequence");
            let shot = s.project.sequence(seq).and_then(|q| q.video_tracks[0].items.get(1).map(|c| c.id.0)).expect("a second shot");
            s.execute("effects.setParam", json!({"clip": shot, "effect": "lumetri", "param": "temperature", "value": 10.0 + i as f64})).expect("touch");
        }
        let s0 = filmagic_export::stage_times();
        let t0 = Instant::now();
        let mut params = json!({"path": out.to_string_lossy(), "format": format, "hardwareEncoding": hw, "gpuRendering": gpu, "wait": true});
        if let (Some(p), Some(e)) = (params.as_object_mut(), extra.as_object()) {
            p.extend(e.clone());
        }
        let r = s.execute("file.exportMedia", params);
        let dt = t0.elapsed().as_secs_f64();
        let r = r.unwrap_or_else(|e| panic!("export: {e}"));
        if i == 0 {
            eprintln!("result: {}", r["result"]);
        }
        if let Some(e) = s.jobs.last().and_then(|j| j.progress.error.lock().ok().and_then(|g| g.clone())) {
            panic!("export: {e}");
        }
        let bytes = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
        let enc = &r["result"]["encoder"];
        let segments = if enc["segmentsReused"].is_null() {
            String::new()
        } else {
            format!(", segments reused {} encoded {}", enc["segmentsReused"], enc["segmentsEncoded"])
        };
        eprintln!("run {i}: {dt:.3} s, {bytes} bytes{segments}");
        times.push(dt);
        stages = filmagic_export::stage_times().iter().zip(&s0).map(|((st, b), (_, a))| (st.name(), (b - a) as f64 / 1e9)).collect();
    }
    let mut sorted = times.clone();
    sorted.sort_by(f64::total_cmp);
    println!("export {format} ({scenario}): runs {runs}, median {:.3} s, min {:.3} s", sorted[runs / 2], sorted[0]);
    println!("stages (last run): {}", stages.iter().map(|(n, s)| format!("{n} {s:.3}")).collect::<Vec<_>>().join(", "));
    if keep.is_none() {
        let _ = std::fs::remove_dir_all(&dir);
    }
}
