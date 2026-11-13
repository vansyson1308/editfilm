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
    project_len(FRAMES, patch)
}

/// [`project`] `frames` long.
fn project_len(frames: i64, patch: Option<(i64, i64)>) -> (Arc<Project>, ItemId, SourceMap) {
    let r = FrameRate::FPS_24;
    let scene = GeneratorSource::new(Generator::Demo(DemoScene::CityNight), W, H, r, r.tick_of(frames));
    let tone = GeneratorSource::new(Generator::Tone { hz: 440.0, db: -12.0 }, W, H, r, r.tick_of(frames));
    let matte = GeneratorSource::new(Generator::ColorMatte { color: [1.0, 0.0, 0.0, 1.0] }, W, H, r, r.tick_of(frames));
    let mut p = Project::new("smart");
    let (si, ti, mi) = (add(&mut p, &scene), add(&mut p, &tone), add(&mut p, &matte));
    let seq = p.new_sequence("s", SequenceSettings { width: W, height: H, frame_rate: r, ..Default::default() }, 2, 1, None);
    let v = p.make_track_item(si, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, r.tick_of(frames)), r).unwrap();
    let a = p.make_track_item(ti, TrackKind::Audio, Tick::ZERO, TimeRange::new(Tick::ZERO, r.tick_of(frames)), r).unwrap();
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
    let back = decode_segment_file(&good, &key, 3, 1001).expect("a good file reads back");
    assert_eq!(back.len(), 3);
    for (a, b) in back.iter().zip(&packets) {
        assert_eq!((&a.data, a.key, a.duration, a.composition_offset), (&b.data, b.key, b.duration, b.composition_offset));
    }
    assert!(decode_segment_file(&good, &[8; 32], 3, 1001).is_none(), "another key");
    assert!(decode_segment_file(&good, &key, 2, 1001).is_none(), "another picture count");
    assert!(decode_segment_file(&good, &key, 3, 1000).is_none(), "another frame duration");
    let no_key: Vec<EncodedPacket> =
        packets.iter().map(|p| EncodedPacket { data: p.data.clone(), key: false, duration: p.duration, composition_offset: p.composition_offset }).collect();
    assert!(decode_segment_file(&encode_segment_file(&key, &no_key), &key, 3, 1001).is_none(), "no keyframe first");
    for n in 0..good.len() {
        assert!(decode_segment_file(&good[..n], &key, 3, 1001).is_none(), "truncated to {n}");
    }
    for i in 0..good.len() {
        for bit in [0x01, 0x80] {
            let mut b = good.clone();
            b[i] ^= bit;
            let r = std::panic::catch_unwind(|| decode_segment_file(&b, &key, 3, 1001));
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
        let r = std::panic::catch_unwind(|| decode_segment_file(&reseal(b), &key, 3, 1001));
        assert!(r.expect("no panic").is_none(), "corrupt field at {at}");
    }
    let mut b = body.clone();
    b[44] = 2;
    assert!(decode_segment_file(&reseal(b), &key, 3, 1001).is_none(), "bad flags");
    assert!(decode_segment_file(&reseal(body.clone()), &key, 3, 1001).is_some(), "the resealed original still reads");
    assert!(decode_segment_file(&[], &key, 0, 1001).is_none());
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

/// The cache is trimmed least recently used first, and only of files named and starting like
/// segments: the directory can be one an agent chose, holding other software's files. Temporary
/// files an export left behind go once they are stale.
#[test]
fn the_cache_is_trimmed_least_recently_used_first_and_only_of_segments() {
    let dir = scratch("prune");
    let at = |s: u64| std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000 + s);
    let touch = |name: &str, t: std::time::SystemTime| std::fs::File::options().write(true).open(dir.join(name)).unwrap().set_modified(t).unwrap();
    let ours = |c: char| format!("{}.seg", std::iter::repeat_n(c, 64).collect::<String>());
    let mut seg = encode_segment_file(&[1; 32], &[EncodedPacket { data: vec![0; 900], key: true, duration: 1, composition_offset: 0 }]);
    seg.truncate(1000);
    for (i, c) in ['a', 'b', 'c'].into_iter().enumerate() {
        std::fs::write(dir.join(ours(c)), &seg).unwrap();
        touch(&ours(c), at(i as u64 * 1000));
    }
    // named like a segment but not one of ours, and another program's .seg file: both older than ours
    std::fs::write(dir.join(ours('d')), vec![7u8; 1000]).unwrap();
    touch(&ours('d'), at(0));
    std::fs::write(dir.join("other-program.seg"), vec![7u8; 1000]).unwrap();
    touch("other-program.seg", at(0));
    // a stale and a fresh temporary file
    let stale = format!(".{}-1-1.tmp", "e".repeat(64));
    let fresh = format!(".{}-1-2.tmp", "f".repeat(64));
    std::fs::write(dir.join(&stale), b"x").unwrap();
    touch(&stale, at(0));
    std::fs::write(dir.join(&fresh), b"x").unwrap();
    prune(&dir, 2500);
    assert!(!dir.join(ours('a')).exists(), "the least recently used segment goes");
    assert!(dir.join(ours('b')).exists() && dir.join(ours('c')).exists());
    assert!(dir.join(ours('d')).exists() && dir.join("other-program.seg").exists(), "files that are not ours stay");
    assert!(!dir.join(&stale).exists() && dir.join(&fresh).exists(), "only stale temporary files go");
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

/// The presentation times ffprobe reads for every video frame of `path`.
fn frame_pts(ffprobe: &std::path::Path, path: &std::path::Path) -> Vec<i64> {
    let out = std::process::Command::new(ffprobe)
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "frame=pts", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.trim().trim_end_matches(',').parse().ok()).collect()
}

/// QC B1: a segment of a single picture (an export one frame longer than a whole number of
/// segments, or a keyframe distance of 1) used to take its decoding time from its own fresh encoder,
/// which cannot know the B-frame delay from one picture: a duplicate presentation time, one missing,
/// and with every segment that short, every frame one frame early against the audio. Now every
/// frame is presented exactly when a normal export presents it, and ffmpeg has nothing to say.
#[test]
fn single_picture_segments_keep_the_stream_timing() {
    let Some(ffprobe) = filmagic_testkit::ffprobe_or_skip("smart render timing") else { return };
    let Some(ffmpeg) = filmagic_testkit::ffmpeg_or_skip("smart render timing") else { return };
    let dir = scratch("timing");
    for (frames, keyint) in [(25i64, 6u32), (13, 6), (24, 1), (7, 1), (1, 6)] {
        let (p, seq, m) = project_len(frames, None);
        let name = format!("{frames}x{keyint}");
        let smart = ExportSettings { keyframe_distance: Some(keyint), ..settings(&dir.join(format!("{name}-smart.mp4")), &dir.join(format!("cache-{name}"))) };
        let normal = ExportSettings {
            smart_render: false,
            smart_render_dir: None,
            path: dir.join(format!("{name}-normal.mp4")).to_string_lossy().into_owned(),
            ..smart.clone()
        };
        let r = export(&p, seq, &smart, &m, &Progress::default()).unwrap();
        assert_eq!(r.encoder.as_ref().unwrap().name, "Filmagic H.264 (software, smart render)");
        export(&p, seq, &normal, &m, &Progress::default()).unwrap();
        let (s, n) = (frame_pts(&ffprobe, std::path::Path::new(&smart.path)), frame_pts(&ffprobe, std::path::Path::new(&normal.path)));
        assert_eq!(s.len(), frames as usize, "{name}: every frame is presented once: {s:?}");
        assert_eq!(s, n, "{name}: the smart export presents its frames when a normal export does");
        assert!(s.windows(2).all(|w| w[1] - w[0] == s[1.min(s.len() - 1)] - s[0]) || s.len() < 2, "{name}: evenly spaced: {s:?}");
        let out = std::process::Command::new(&ffmpeg)
            .args(["-v", "warning", "-err_detect", "+crccheck+bitstream+buffer+explode", "-i", &smart.path, "-f", "null", "-"])
            .output()
            .unwrap();
        assert!(out.status.success() && out.stderr.is_empty(), "{name}: ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
    }
}

/// The `idr_pic_id` of every IDR picture of `path`, in decoding order (ffmpeg's trace_headers).
fn idr_pic_ids(ffmpeg: &std::path::Path, path: &std::path::Path) -> Vec<u32> {
    let out = std::process::Command::new(ffmpeg)
        .args(["-v", "trace", "-i"])
        .arg(path)
        .args(["-c", "copy", "-bsf:v", "trace_headers", "-f", "null", "-"])
        .output()
        .unwrap();
    // one id per picture: the one in its first slice (first_mb_in_slice 0)
    let value = |l: &str| l.split('=').next_back().and_then(|v| v.trim().parse::<u32>().ok());
    let mut first_slice = false;
    let mut ids = Vec::new();
    for l in String::from_utf8_lossy(&out.stderr).lines() {
        if l.contains("first_mb_in_slice") {
            first_slice = value(l) == Some(0);
        } else if l.contains("idr_pic_id") && first_slice {
            ids.extend(value(l));
        }
    }
    ids
}

/// QC S1: two consecutive IDR pictures must not share an `idr_pic_id` (H.264 §7.4.3). Each segment
/// starts its own encoder, so neighbouring segments number theirs from 0 and from 2.
#[test]
fn consecutive_idr_pictures_never_share_an_id_across_segments() {
    let Some(ffmpeg) = filmagic_testkit::ffmpeg_or_skip("smart render idr_pic_id") else { return };
    let dir = scratch("idr");
    for (name, s) in [
        ("keyint1", ExportSettings { keyframe_distance: Some(1), ..settings(&dir.join("k1.mp4"), &dir.join("cache")) }),
        ("intra", ExportSettings { h264_intra_only: true, keyframe_distance: Some(5), ..settings(&dir.join("intra.mp4"), &dir.join("cache")) }),
        ("keyint6", settings(&dir.join("k6.mp4"), &dir.join("cache"))),
    ] {
        let (p, seq, m) = project_len(17, None);
        export(&p, seq, &s, &m, &Progress::default()).unwrap();
        let ids = idr_pic_ids(&ffmpeg, std::path::Path::new(&s.path));
        assert!(ids.len() >= 3, "{name}: {ids:?}");
        assert!(ids.windows(2).all(|w| w[0] != w[1]), "{name}: consecutive IDR pictures share an id: {ids:?}");
    }
}

/// A hit marks the segment as just used (eviction is least recently used), and no temporary file
/// stays in the cache after an export.
#[test]
fn hits_refresh_their_segments_and_no_temporary_file_stays() {
    let dir = scratch("touch");
    let cache = dir.join("cache");
    let (p, seq, m) = project(None);
    let _ = run(&p, seq, &m, &settings(&dir.join("a.mp4"), &cache));
    let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
    let files: Vec<_> = std::fs::read_dir(&cache).unwrap().map(|e| e.unwrap().path()).collect();
    assert!(files.iter().all(|f| f.extension().is_some_and(|x| x == "seg")), "only segments: {files:?}");
    for f in &files {
        std::fs::File::options().write(true).open(f).unwrap().set_modified(old).unwrap();
    }
    let (_, stats, _) = run(&p, seq, &m, &settings(&dir.join("b.mp4"), &cache));
    assert_eq!(stats, (4, 0));
    for f in &files {
        assert!(std::fs::metadata(f).unwrap().modified().unwrap() > old, "{} was not refreshed", f.display());
    }
}

/// QC S2: the cache directory must be the user's own. A directory others can write to, or one that
/// belongs to another user, is not used: the export still runs (and is the same file), nothing is
/// cached, and the result says why.
#[cfg(unix)]
#[test]
fn a_cache_directory_others_can_write_to_is_not_used() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch("shared");
    let shared = dir.join("shared-cache");
    std::fs::create_dir_all(&shared).unwrap();
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();
    assert!(crate::smart::prepare_dir(&shared, false).unwrap_err().contains("writable by other users"));
    let (p, seq, m) = project(None);
    let (a, stats, r) = run(&p, seq, &m, &settings(&dir.join("a.mp4"), &shared));
    assert_eq!(stats, (0, 4));
    assert!(r.notes.iter().any(|n| n.contains("no segment is cached") && n.contains("writable by other users")), "{:?}", r.notes);
    assert_eq!(std::fs::read_dir(&shared).unwrap().count(), 0, "nothing was written there");
    let (b, _, _) = run(&p, seq, &m, &settings(&dir.join("b.mp4"), &dir.join("own-cache")));
    assert!(a == b, "the file does not depend on the cache");
    // the private default is created 0700, and a group-writable private directory is tightened
    let private = dir.join("private");
    crate::smart::prepare_dir(&private, true).unwrap();
    assert_eq!(std::fs::metadata(&private).unwrap().permissions().mode() & 0o777, 0o700);
    std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o775)).unwrap();
    crate::smart::prepare_dir(&private, true).unwrap();
    assert_eq!(std::fs::metadata(&private).unwrap().permissions().mode() & 0o777, 0o700);
    // another user's directory (only where this test may give one away, i.e. as root)
    let theirs = dir.join("theirs");
    std::fs::create_dir_all(&theirs).unwrap();
    std::fs::set_permissions(&theirs, std::fs::Permissions::from_mode(0o755)).unwrap();
    if std::os::unix::fs::chown(&theirs, Some(65534), Some(65534)).is_ok() {
        assert!(crate::smart::prepare_dir(&theirs, false).is_err());
        assert!(crate::smart::prepare_dir(&theirs, true).is_err());
    }
    // the default is the user's own cache directory, never the shared temporary directory
    if let Some(d) = crate::smart::default_dir() {
        assert!(!d.starts_with(std::env::temp_dir()), "{}", d.display());
    }
}

/// QC S5: CBR and VBR say that a smart export holds its rate per segment.
#[test]
fn smart_render_with_a_bitrate_says_how_it_differs() {
    let dir = scratch("vbr");
    let (p, seq, m) = project(None);
    for mode in [BitrateMode::Cbr, BitrateMode::Vbr1Pass] {
        let s = ExportSettings { bitrate_mode: mode, ..settings(&dir.join("v.mp4"), &dir.join("cache")) };
        let (_, _, r) = run(&p, seq, &m, &s);
        assert!(r.notes.iter().any(|n| n.contains("held per segment")), "{mode:?}: {:?}", r.notes);
    }
    let s = ExportSettings { bitrate_mode: BitrateMode::Crf, ..settings(&dir.join("c.mp4"), &dir.join("cache")) };
    let (_, _, r) = run(&p, seq, &m, &s);
    assert!(r.notes.iter().all(|n| !n.contains("held per segment")), "{:?}", r.notes);
}
