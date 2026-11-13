//! Smart render (#18): a re-export reuses the segments whose pictures did not change, and the file
//! is byte for byte the one a full render (an empty cache) writes.

use super::*;
use crate::smart::{Digest, decode_segment_file, encode_segment_file, picture_digest, prune, segment_frames};
use filmagic_media::generators::GeneratorSource;
use filmagic_media::{DemoScene, Generator, MediaSource};
use filmagic_project::{ItemKind, Label, MediaClip, MediaRef, SequenceSettings, TrackKind};
use filmagic_render::SourceMap;

const W: u32 = 320;
const H: u32 = 180;
const FRAMES: i64 = 24;
/// Frames per segment in these tests: four segments of six frames.
const KEYINT: u32 = 6;

fn scratch(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("fc-smart-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn add(p: &mut Project, g: &GeneratorSource) -> ItemId {
    let info = g.info().clone();
    let clip = MediaClip {
        media: MediaRef::Generator(g.generator.clone()),
        info: info.clone(),
        interpret: Default::default(),
        mark_in: None,
        mark_out: None,
        markers: vec![],
        offline: false,
        proxy: None,
        identity: None,
    };
    p.add_item(&info.name, Label::Iris, ItemKind::Media(Box::new(clip)), None)
}

/// A moving demo scene with a tone; `patch` puts a red matte over frames `patch.0 .. patch.1` on V2
/// (the "small change" of a re-export).
fn project(patch: Option<(i64, i64)>) -> (Arc<Project>, ItemId, SourceMap) {
    let r = FrameRate::FPS_24;
    let scene = GeneratorSource::new(Generator::Demo(DemoScene::CityNight), W, H, r, r.tick_of(FRAMES));
    let tone = GeneratorSource::new(Generator::Tone { hz: 440.0, db: -12.0 }, W, H, r, r.tick_of(FRAMES));
    let matte = GeneratorSource::new(Generator::ColorMatte { color: [1.0, 0.0, 0.0, 1.0] }, W, H, r, r.tick_of(FRAMES));
    let mut p = Project::new("smart");
    let (si, ti, mi) = (add(&mut p, &scene), add(&mut p, &tone), add(&mut p, &matte));
    let seq = p.new_sequence("s", SequenceSettings { width: W, height: H, frame_rate: r, ..Default::default() }, 2, 1, None);
    let v = p.make_track_item(si, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, r.tick_of(FRAMES)), r).unwrap();
    let a = p.make_track_item(ti, TrackKind::Audio, Tick::ZERO, TimeRange::new(Tick::ZERO, r.tick_of(FRAMES)), r).unwrap();
    let q = p.sequence_mut(seq).unwrap();
    q.video_tracks[0].items.push(v);
    q.audio_tracks[0].items.push(a);
    if let Some((f0, f1)) = patch {
        let m = p.make_track_item(mi, TrackKind::Video, r.tick_of(f0), TimeRange::new(Tick::ZERO, r.tick_of(f1 - f0)), r).unwrap();
        p.sequence_mut(seq).unwrap().video_tracks[1].items.push(m);
    }
    let mut map = SourceMap::default();
    map.0.insert(si, Arc::new(scene));
    map.0.insert(ti, Arc::new(tone));
    map.0.insert(mi, Arc::new(matte));
    (Arc::new(p), seq, map)
}

fn settings(path: &std::path::Path, cache: &std::path::Path) -> ExportSettings {
    ExportSettings {
        format: Format::H264,
        path: path.to_string_lossy().into_owned(),
        keyframe_distance: Some(KEYINT),
        smart_render: true,
        smart_render_dir: Some(cache.to_string_lossy().into_owned()),
        ..Default::default()
    }
}

/// Export and return the file and the (reused, encoded) segment counts.
fn run(p: &Arc<Project>, seq: ItemId, m: &SourceMap, s: &ExportSettings) -> (Vec<u8>, (u64, u64), Report) {
    let r = export(p, seq, s, m, &Progress::default()).unwrap();
    let e = r.encoder.clone().unwrap();
    assert_eq!(e.name, "Filmagic H.264 (software, smart render)");
    let bytes = std::fs::read(&s.path).unwrap();
    (bytes, (e.segments_reused.unwrap(), e.segments_encoded.unwrap()), r)
}

#[test]
fn a_re_export_reuses_every_segment_and_writes_the_same_file() {
    let dir = scratch("same");
    let cache = dir.join("cache");
    let (p, seq, m) = project(None);
    let (cold, stats, _) = run(&p, seq, &m, &settings(&dir.join("cold.mp4"), &cache));
    assert_eq!(stats, (0, 4), "an empty cache: every segment encoded");
    let (warm, stats, _) = run(&p, seq, &m, &settings(&dir.join("warm.mp4"), &cache));
    assert_eq!(stats, (4, 0), "nothing changed: every segment from the cache");
    assert!(cold == warm, "the cached segments changed the file");
    // the stream decodes to every frame, with a keyframe at each segment
    let src = filmagic_codecs::open_bytes("warm.mp4", warm.into()).unwrap();
    let v = src.info().video.clone().unwrap();
    assert_eq!((v.width, v.height), (W, H));
    assert!((src.info().duration.seconds() - 1.0).abs() < 0.05, "{}", src.info().duration.seconds());
    for f in [0, 5, 6, 13, 23] {
        let req = filmagic_media::FrameRequest::full(FrameRate::FPS_24.tick_of(f));
        assert!(src.video_frame(req).is_ok(), "frame {f} decodes");
    }
}

/// The owner's case: one caption changed. Only the segments with a changed picture are encoded
/// again, and the file is exactly what a full render of the changed edit writes.
#[test]
fn a_small_change_re_encodes_only_its_segments_and_matches_a_full_render() {
    let dir = scratch("change");
    let cache = dir.join("cache");
    let (p, seq, m) = project(None);
    let _ = run(&p, seq, &m, &settings(&dir.join("first.mp4"), &cache));
    // frames 7..10 change: inside the second segment (6..12) only
    let (p2, seq2, m2) = project(Some((7, 10)));
    let (smart, stats, _) = run(&p2, seq2, &m2, &settings(&dir.join("smart.mp4"), &cache));
    assert_eq!(stats, (3, 1), "only the changed segment is encoded");
    let (full, stats, _) = run(&p2, seq2, &m2, &settings(&dir.join("full.mp4"), &dir.join("empty-cache")));
    assert_eq!(stats, (0, 4));
    assert!(smart == full, "the smart re-export differs from a full render of the same edit");
    // a change across a segment boundary touches both segments
    let (p3, seq3, m3) = project(Some((11, 13)));
    let (_, stats, _) = run(&p3, seq3, &m3, &settings(&dir.join("across.mp4"), &cache));
    assert_eq!(stats, (2, 2));
}

/// Other encoder settings never reuse a segment: the key covers the whole configuration.
#[test]
fn other_encoder_settings_do_not_reuse_segments() {
    let dir = scratch("settings");
    let cache = dir.join("cache");
    let (p, seq, m) = project(None);
    let _ = run(&p, seq, &m, &settings(&dir.join("a.mp4"), &cache));
    for (name, s) in [
        ("crf", ExportSettings { bitrate_mode: BitrateMode::Crf, crf: 30.0, ..settings(&dir.join("crf.mp4"), &cache) }),
        ("preset", ExportSettings { h264_preset: H264Preset::Speed, ..settings(&dir.join("speed.mp4"), &cache) }),
        ("bitrate", ExportSettings { bitrate_kbps: 1234, ..settings(&dir.join("kbps.mp4"), &cache) }),
        ("profile", ExportSettings { h264_profile: H264Profile::Main, ..settings(&dir.join("main.mp4"), &cache) }),
    ] {
        let (_, stats, _) = run(&p, seq, &m, &s);
        assert_eq!(stats.0, 0, "{name}: a segment of other settings was reused");
    }
}

/// A damaged, truncated, foreign or empty cache file is a miss: the segment is encoded again and
/// the file is the same as ever.
#[test]
fn damaged_cache_files_are_encoded_again() {
    let dir = scratch("damaged");
    let cache = dir.join("cache");
    let (p, seq, m) = project(None);
    let (cold, _, _) = run(&p, seq, &m, &settings(&dir.join("cold.mp4"), &cache));
    let mut files: Vec<_> = std::fs::read_dir(&cache).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|x| x == "seg")).collect();
    files.sort();
    assert_eq!(files.len(), 4);
    // a flipped bit, a truncated file, an empty file, and another segment's file under this name
    let mut flipped = std::fs::read(&files[0]).unwrap();
    let mid = flipped.len() / 2;
    flipped[mid] ^= 0x10;
    std::fs::write(&files[0], flipped).unwrap();
    let truncated = std::fs::read(&files[1]).unwrap();
    std::fs::write(&files[1], &truncated[..truncated.len() / 3]).unwrap();
    std::fs::write(&files[2], b"").unwrap();
    std::fs::copy(&files[0], &files[3]).unwrap();
    let (warm, stats, _) = run(&p, seq, &m, &settings(&dir.join("warm.mp4"), &cache));
    assert_eq!(stats, (0, 4), "no damaged file was trusted");
    assert!(cold == warm);
    // and they were replaced by good copies
    let (_, stats, _) = run(&p, seq, &m, &settings(&dir.join("again.mp4"), &cache));
    assert_eq!(stats, (4, 0));
}

/// Mutation fuzz of the segment file parser: truncations, bit flips and corrupt sizes never panic
/// and never give packets for a file that is not exactly what was written.
#[test]
fn segment_files_survive_hostile_bytes() {
    let key: Digest = [7; 32];
    let packets = vec![
        EncodedPacket { data: vec![1, 2, 3, 4, 5], key: true, duration: 1001, composition_offset: 1001 },
        EncodedPacket { data: vec![], key: false, duration: 1001, composition_offset: -5 },
        EncodedPacket { data: vec![9; 300], key: false, duration: 1001, composition_offset: 0 },
    ];
    let good = encode_segment_file(&key, &packets);
    let back = decode_segment_file(&good, &key, 3).expect("a good file reads back");
    assert_eq!(back.len(), 3);
    for (a, b) in back.iter().zip(&packets) {
        assert_eq!((&a.data, a.key, a.duration, a.composition_offset), (&b.data, b.key, b.duration, b.composition_offset));
    }
    assert!(decode_segment_file(&good, &[8; 32], 3).is_none(), "another key");
    assert!(decode_segment_file(&good, &key, 2).is_none(), "another picture count");
    for n in 0..good.len() {
        assert!(decode_segment_file(&good[..n], &key, 3).is_none(), "truncated to {n}");
    }
    for i in 0..good.len() {
        for bit in [0x01, 0x80] {
            let mut b = good.clone();
            b[i] ^= bit;
            let r = std::panic::catch_unwind(|| decode_segment_file(&b, &key, 3));
            assert!(r.expect("no panic").is_none(), "flipped bit at {i}");
        }
    }
    // corrupt sizes with a valid checksum: lengths past the end, a huge count, bad flags
    let reseal = |mut body: Vec<u8>| {
        use sha2::Digest as _;
        let sum: Digest = sha2::Sha256::digest(&body).into();
        body.extend_from_slice(&sum);
        body
    };
    let body = good[..good.len() - 32].to_vec();
    for (at, bytes) in [(40usize, u32::MAX.to_le_bytes()), (44 + 9, u32::MAX.to_le_bytes()), (44 + 9, 6u32.to_le_bytes())] {
        let mut b = body.clone();
        b[at..at + 4].copy_from_slice(&bytes);
        let r = std::panic::catch_unwind(|| decode_segment_file(&reseal(b), &key, 3));
        assert!(r.expect("no panic").is_none(), "corrupt field at {at}");
    }
    let mut b = body.clone();
    b[44] = 2;
    assert!(decode_segment_file(&reseal(b), &key, 3).is_none(), "bad flags");
    assert!(decode_segment_file(&reseal(body.clone()), &key, 3).is_some(), "the resealed original still reads");
    assert!(decode_segment_file(&[], &key, 0).is_none());
}

#[test]
fn segment_lengths_and_digests_are_bounded() {
    assert_eq!(segment_frames(48, 1920, 1080), 48);
    // 4K: the memory budget, not the keyframe distance
    let n = segment_frames(48, 3840, 2160);
    assert!((10..48).contains(&n), "{n}");
    for (k, w, h) in [(0, 1920, 1080), (48, 0, 0), (u32::MAX, u32::MAX, u32::MAX), (1, 1, 1)] {
        assert!(segment_frames(k, w, h) >= 1);
    }
    // planes cannot run into one another
    assert_ne!(picture_digest(&[1, 2], &[3], &[4]), picture_digest(&[1], &[2, 3], &[4]));
    assert_eq!(picture_digest(&[1, 2], &[3], &[4]), picture_digest(&[1, 2], &[3], &[4]));
}

#[test]
fn the_cache_is_trimmed_oldest_first() {
    let dir = scratch("prune");
    for (i, name) in ["a", "b", "c"].iter().enumerate() {
        std::fs::write(dir.join(format!("{name}.seg")), vec![0u8; 1000]).unwrap();
        std::fs::write(dir.join(format!("{name}.txt")), vec![0u8; 1000]).unwrap();
        let t = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000 + i as u64 * 1000);
        std::fs::File::options().write(true).open(dir.join(format!("{name}.seg"))).unwrap().set_modified(t).unwrap();
    }
    prune(&dir, 2500);
    assert!(!dir.join("a.seg").exists() && dir.join("b.seg").exists() && dir.join("c.seg").exists());
    assert!(dir.join("a.txt").exists(), "only segments are removed");
    prune(&dir.join("missing"), 0);
}

/// Smart render that cannot apply (two-pass) says so in the result and exports as usual.
#[test]
fn smart_render_that_cannot_apply_says_so() {
    let dir = scratch("twopass");
    let (p, seq, m) = project(None);
    let s = ExportSettings { bitrate_mode: BitrateMode::Vbr2Pass, ..settings(&dir.join("two.mp4"), &dir.join("cache")) };
    let r = export(&p, seq, &s, &m, &Progress::default()).unwrap();
    assert_eq!(r.encoder.as_ref().unwrap().name, "Filmagic H.264 (software)");
    assert!(r.notes.iter().any(|n| n.contains("smartRender: not used")), "{:?}", r.notes);
}

/// ffmpeg (an external oracle only) decodes a smart-rendered file, cached segments and all, with
/// strict error detection: no warning, every frame, a keyframe at the start of every segment.
#[test]
fn ffmpeg_decodes_smart_rendered_files_without_complaint() {
    let Some(ffmpeg) = filmagic_testkit::ffmpeg_or_skip("smart render oracle") else { return };
    let dir = scratch("oracle");
    let cache = dir.join("cache");
    let (p, seq, m) = project(None);
    let _ = run(&p, seq, &m, &settings(&dir.join("first.mp4"), &cache));
    let (p2, seq2, m2) = project(Some((7, 10)));
    let path = dir.join("smart.mp4");
    let (_, stats, _) = run(&p2, seq2, &m2, &settings(&path, &cache));
    assert_eq!(stats, (3, 1));
    let out = std::process::Command::new(&ffmpeg)
        .args(["-v", "warning", "-err_detect", "+crccheck+bitstream+buffer+explode", "-i"])
        .arg(&path)
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
        .output()
        .unwrap();
    assert!(out.status.success() && out.stderr.is_empty(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
    let picture = (W * H * 3 / 2) as usize;
    assert_eq!(out.stdout.len(), picture * FRAMES as usize, "every frame decodes");
    if let Some(ffprobe) = filmagic_testkit::ffprobe_or_skip("smart render keyframes") {
        let out = std::process::Command::new(ffprobe)
            .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "frame=key_frame", "-of", "csv=p=0"])
            .arg(&path)
            .output()
            .unwrap();
        let keys: Vec<usize> =
            String::from_utf8_lossy(&out.stdout).lines().enumerate().filter(|(_, l)| l.trim().trim_end_matches(',') == "1").map(|(i, _)| i).collect();
        assert_eq!(keys, vec![0, 6, 12, 18], "one keyframe per segment");
    }
}
