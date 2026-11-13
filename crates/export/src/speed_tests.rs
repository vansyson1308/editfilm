//! Export speed (#18): what the export result says about the encoder and renderer that did the
//! work, and the notes that tell an agent how to make it faster.

use super::*;
use crate::job::speed_notes;
use crate::pipeline::PLAIN_FRAMES;
use crate::tests::project;

fn tmp(name: &str) -> String {
    let d = std::env::temp_dir().join(format!("fc-export-speed-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name).to_string_lossy().to_string()
}

/// A stand-in for a hardware encoder that declines (NVENC without an NVIDIA GPU): only for the
/// exports of this file, recognised by their file name.
fn declining_hardware(format: Format, _: u32, _: u32, _: FrameRate, s: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>> {
    if format == Format::H264 && s.hardware_encoding == HardwareEncoding::Auto && s.path.contains("hw-declines") {
        note_hw_encode_declined_because("Test H.264: no device");
    }
    None
}

#[test]
fn an_export_reports_its_encoder_and_stages() {
    let (p, seq, m) = project();
    let s = ExportSettings { format: Format::H264, path: tmp("plain.mp4"), ..Default::default() };
    let r = export(&p, seq, &s, &m, &Progress::default()).unwrap();
    let e = r.encoder.as_ref().expect("an H.264 export names its encoder");
    assert_eq!(e.name, "Filmagic H.264 (software)");
    assert!(!e.hardware && !e.hardware_requested && !e.gpu_requested);
    assert!(e.hardware_declined.is_empty());
    assert_eq!((e.gpu_frames, e.cpu_frames), (0, r.frames));
    for stage in ["render", "encode"] {
        assert!(r.stages.get(stage).is_some_and(|s| *s > 0.0), "{stage}: {:?}", r.stages);
    }
    assert!(r.notes.iter().all(|n| !n.contains("hardwareEncoding")), "{:?}", r.notes);
    // the job result agents read: one `encoder` object and the stage times
    let v = serde_json::to_value(&r).unwrap();
    assert_eq!(v["encoder"]["name"], "Filmagic H.264 (software)");
    assert_eq!(v["encoder"]["hardwareRequested"], false);
    assert!(v["stages"]["encode"].as_f64().is_some(), "{v}");
}

/// `hardwareEncoding: auto` on a machine whose hardware encoder declines (Windows without an
/// NVIDIA GPU): the software encoder writes the file, and the result says so and why.
#[test]
fn a_declined_hardware_encoder_is_reported_with_its_reason() {
    register_encoder(declining_hardware);
    let (p, seq, m) = project();
    let s = ExportSettings { format: Format::H264, path: tmp("hw-declines.mp4"), hardware_encoding: HardwareEncoding::Auto, ..Default::default() };
    let r = export(&p, seq, &s, &m, &Progress::default()).unwrap();
    let e = r.encoder.as_ref().unwrap();
    assert_eq!(e.name, "Filmagic H.264 (software)");
    assert!(e.hardware_requested && !e.hardware);
    assert_eq!(e.hardware_declined, vec!["Test H.264: no device".to_string()]);
    assert!(r.notes.iter().any(|n| n.contains("hardwareEncoding auto") && n.contains("Test H.264: no device")), "{:?}", r.notes);
    // the reasons belong to that export: the next one, with hardware off, has none
    let s = ExportSettings { format: Format::H264, path: tmp("hw-declines-off.mp4"), ..Default::default() };
    let r = export(&p, seq, &s, &m, &Progress::default()).unwrap();
    assert!(r.encoder.unwrap().hardware_declined.is_empty());
}

fn report(hardware_requested: bool, gpu_requested: bool, gpu_frames: u64) -> EncoderReport {
    EncoderReport {
        name: "x".into(),
        hardware: false,
        hardware_requested,
        hardware_declined: vec![],
        gpu_requested,
        gpu_frames,
        cpu_frames: 100,
        ..Default::default()
    }
}

fn stages(pairs: &[(&str, f64)]) -> std::collections::BTreeMap<String, f64> {
    pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

#[test]
fn speed_notes_name_the_side_that_held_the_export_back() {
    // render-bound (the encoder waited for most of the export): try the GPU or a draft
    let n = speed_notes(&report(false, false, 0), &stages(&[("render", 9.0), ("encode", 4.0), ("wait", 5.0)]), 10.0, Format::H264, true);
    assert!(n.iter().any(|n| n.contains("gpuRendering auto") && n.contains("draft")), "{n:?}");
    // encode-bound: try the hardware encoder or a draft
    let n = speed_notes(&report(false, false, 0), &stages(&[("render", 5.0), ("encode", 9.5), ("wait", 0.1)]), 10.0, Format::H264, true);
    assert!(n.iter().any(|n| n.contains("hardwareEncoding auto")), "{n:?}");
    // asked for and not available: say what happened, not what to try
    let n = speed_notes(&report(true, true, 0), &stages(&[("encode", 9.5), ("wait", 0.1)]), 10.0, Format::H264, true);
    assert!(n.iter().any(|n| n.contains("software encoder wrote this export") && n.contains("no hardware H.264 encoder")), "{n:?}");
    assert!(n.iter().any(|n| n.contains("rendered on the CPU")), "{n:?}");
    // short exports, other formats and hostile timings give no advice and do not panic
    assert!(speed_notes(&report(false, false, 0), &stages(&[("wait", 1.0)]), 1.0, Format::H264, true).is_empty());
    assert!(speed_notes(&report(false, false, 0), &stages(&[("encode", 9.5)]), 10.0, Format::ProRes, true).is_empty());
    for secs in [f64::NAN, -1.0, f64::INFINITY, 0.0] {
        let _ = speed_notes(&report(false, false, 0), &stages(&[("encode", f64::NAN), ("wait", f64::INFINITY)]), secs, Format::H264, true);
    }
}

/// A sequence of one clip of `src` at its own size and rate (Motion resolved as the app does).
fn clip_project(src: filmagic_media::SharedSource) -> (Arc<Project>, ItemId, filmagic_render::SourceMap) {
    use filmagic_project::{ItemKind, Label, MediaClip, MediaRef, SequenceSettings, TrackKind};
    let info = src.info().clone();
    let v = info.video.clone().unwrap();
    let mut p = Project::new("plain");
    let clip = MediaClip {
        media: MediaRef::Generator(filmagic_media::Generator::ColorMatte { color: [0.0; 4] }),
        info: info.clone(),
        interpret: Default::default(),
        mark_in: None,
        mark_out: None,
        markers: vec![],
        offline: false,
        proxy: None,
        identity: None,
    };
    let item = p.add_item("decoded", Label::Iris, ItemKind::Media(Box::new(clip)), None);
    let seq = p.new_sequence("s", SequenceSettings { width: v.width, height: v.height, frame_rate: v.frame_rate, ..Default::default() }, 1, 1, None);
    let mut it = p.make_track_item(item, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, info.duration), v.frame_rate).unwrap();
    for e in &mut it.effects {
        filmagic_project::resolve_auto_points(e, (v.width, v.height), (v.width, v.height));
    }
    p.sequence_mut(seq).unwrap().video_tracks[0].items.push(it);
    let mut m = filmagic_render::SourceMap::default();
    m.0.insert(item, src);
    (Arc::new(p), seq, m)
}

/// Run a stepped export to the end; `float` makes every frame go through its float image.
fn stepped_export(p: &Arc<Project>, seq: ItemId, s: &ExportSettings, m: &filmagic_render::SourceMap, float: bool) -> Report {
    let progress = Progress::default();
    let mut ex = Exporter::new(p.clone(), seq, s, &progress).unwrap();
    if float {
        ex.force_float_frames();
    }
    loop {
        if let Step::Done(r) = ex.step(m, &progress).unwrap() {
            return r;
        }
    }
}

/// A plain cut of 8-bit video goes to the encoder as the picture's own sRGB bytes, without its
/// float image (#18). The file is byte for byte the one the float path writes.
#[test]
fn plain_cuts_skip_the_float_picture_and_write_the_same_file() {
    // real decoded 8-bit 4:2:0 frames: a moving demo scene through our own H.264 encoder and decoder
    let (w, h, rate) = (320, 180, FrameRate::FPS_24);
    let scene = filmagic_media::Generator::Demo(filmagic_media::DemoScene::CityNight);
    let generated = filmagic_media::generators::GeneratorSource::new(scene, w, h, rate, rate.tick_of(12));
    let (gp, gseq, gm) = clip_project(Arc::new(generated));
    let src_path = tmp("plain-src.mp4");
    export(&gp, gseq, &ExportSettings { format: Format::H264, path: src_path.clone(), include_audio: false, ..Default::default() }, &gm, &Progress::default())
        .unwrap();
    let decoded = filmagic_codecs::open_bytes("plain-src.mp4", std::fs::read(&src_path).unwrap().into()).unwrap();
    let (p, seq, m) = clip_project(decoded);
    for (name, extra) in [
        ("plain", ExportSettings::default()),
        ("limited", {
            let mut s = ExportSettings::default();
            s.effects.video_limiter.enabled = true;
            s.effects.video_limiter.min_percent = 10.0;
            s.effects.video_limiter.max_percent = 80.0;
            s
        }),
    ] {
        let fast_path = tmp(&format!("{name}-fast.mp4"));
        let float_path = tmp(&format!("{name}-float.mp4"));
        let base = ExportSettings { format: Format::H264, include_audio: false, ..extra };
        let before = PLAIN_FRAMES.load(Ordering::Relaxed);
        stepped_export(&p, seq, &ExportSettings { path: fast_path.clone(), ..base.clone() }, &m, false);
        assert!(PLAIN_FRAMES.load(Ordering::Relaxed) >= before + 12, "{name}: the plain frames took the shortcut");
        stepped_export(&p, seq, &ExportSettings { path: float_path.clone(), ..base }, &m, true);
        let (a, b) = (std::fs::read(&fast_path).unwrap(), std::fs::read(&float_path).unwrap());
        assert!(a == b, "{name}: the shortcut changed the file ({} vs {} bytes)", a.len(), b.len());
    }
}
