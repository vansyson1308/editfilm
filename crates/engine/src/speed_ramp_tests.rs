//! `clip.speedRamp` and the Time Remapping it is made of (#21).

use filmagic_project::{ClipId, TrackItem};
use filmagic_time::{FrameRate, Tick};
use serde_json::{Value, json};

use crate::Session;
use crate::media_test_util::tmp_dir;

const FPS: i64 = 25;

fn rate() -> FrameRate {
    FrameRate::new(FPS, 1)
}

fn clip(s: &Session, c: u64) -> TrackItem {
    s.active_sequence().unwrap().find_item(ClipId(c)).unwrap().1.clone()
}

/// A 25 fps sequence holding one 64-frame image sequence clip on V1 at 0 whose frame `n` has red
/// `4 n`, then a colour matte after it. Returns (session, the clip, the matte clip).
fn numbered() -> (Session, u64, u64) {
    let dir = tmp_dir("speed-ramp");
    let paths: Vec<String> = (0..64u32)
        .map(|n| {
            let p = dir.join(format!("f{n:04}.png"));
            image::RgbaImage::from_pixel(16, 16, image::Rgba([(4 * n) as u8, 80, 40, 255])).save(&p).unwrap();
            p.to_string_lossy().to_string()
        })
        .collect();
    let mut s = Session::default();
    s.execute("prefs.set", json!({"key": "media.indeterminateTimebase", "value": "25"})).unwrap();
    s.execute("file.newSequence", json!({"name": "Ramp", "video": 2, "audio": 1, "fps": FPS, "width": 16, "height": 16})).unwrap();
    assert_eq!(s.sequence_rate(), rate());
    let item = s.execute("file.import", json!({"paths": [paths[0]], "imageSequence": true})).unwrap()["items"][0].as_u64().unwrap();
    let c = s.execute("timeline.place", json!({"item": item, "track": "V1", "seconds": 0.0})).unwrap()["clips"][0].as_u64().unwrap();
    assert_eq!(clip(&s, c).duration, rate().tick_of(64));
    let matte = s.execute("file.newColorMatte", json!({"color": "#0000ff", "seconds": 1.0})).unwrap()["item"].as_u64().unwrap();
    let m = s.execute("timeline.place", json!({"item": matte, "track": "V1", "frame": 64})).unwrap()["clips"][0].as_u64().unwrap();
    (s, c, m)
}

/// The media frame number the program shows at sequence frame `f` (read from its red channel).
fn shown(s: &Session, f: i64) -> i64 {
    let img = s.try_render_program_at(1.0, rate().tick_of(f)).unwrap();
    let px = img.over_black_rgba8();
    (f64::from(px[(8 * 16 + 8) * 4]) / 4.0).round() as i64
}

/// Hand-computed media time, in source frames, of sequence frame `k` of a clip that plays at
/// 100 % for 16 frames, then slows linearly to 40 % over 16 frames, then stays at 40 %.
fn hand(k: f64) -> f64 {
    if k <= 16.0 {
        k
    } else if k <= 32.0 {
        let u = k - 16.0;
        // ∫ (1 - 0.6 x / 16) dx from 0 to u
        16.0 + u - 0.6 * u * u / 32.0
    } else {
        16.0 + 16.0 * 0.7 + 0.4 * (k - 32.0)
    }
}

fn ramp(s: &mut Session, p: Value) -> Value {
    s.execute("clip.speedRamp", p.clone()).unwrap_or_else(|e| panic!("{p}: {e}"))
}

#[test]
fn every_frame_is_the_integral_of_the_speed_curve() {
    let (mut s, c, _) = numbered();
    let r = ramp(&mut s, json!({"clip": c, "from": 100, "to": 40, "at": {"frame": 16}, "duration": {"frame": 16}, "ease": "linear"}));
    // the two points, and the clip's first frame (the anchor the ramp is measured from)
    assert_eq!(r["keyframes"], 3, "{r}");
    assert_eq!(r["duration"]["frame"], 64, "ripple is off: the clip keeps its length");
    assert!(r["warnings"][0].as_str().unwrap().contains("no longer show"), "{r}");
    assert!(r.get("pictureChange").is_some());
    let it = clip(&s, c);
    let fd = rate().frame_duration();
    for k in 0..64 {
        let want = hand(k as f64);
        let got = it.source_time_at(rate().tick_of(k));
        let want_ticks = (want * fd.0 as f64).round() as i64;
        assert!((got.0 - want_ticks).abs() <= 2, "frame {k}: media {} frames, want {want}", got.0 as f64 / fd.0 as f64);
        // frame sampling shows the source frame the media time falls in
        let frame = want.floor() as i64;
        let near_edge = (want - want.round()).abs() < 1e-6;
        let seen = shown(&s, k);
        assert!(seen == frame || (near_edge && seen == want.round() as i64 - 1), "frame {k}: shows {seen}, want {frame} ({want})");
    }
    // 64 frames reach 16 + 11.2 + 12.8 = 40 media frames: 24 frames of media no longer show
    assert!((r["media"]["seconds"].as_f64().unwrap() - 40.0 / 25.0).abs() < 1e-3, "{r}");
}

#[test]
fn eased_ramps_reach_the_same_frame_and_glide() {
    // a symmetric ease spends the same media as a straight line, so the frames after it agree
    let (mut s, c, _) = numbered();
    ramp(&mut s, json!({"clip": c, "from": 100, "to": 40, "at": {"frame": 16}, "duration": {"frame": 16}}));
    let it = clip(&s, c);
    let fd = rate().frame_duration().0 as f64;
    for k in [32, 40, 50, 63] {
        let got = it.source_time_at(rate().tick_of(k)).0 as f64 / fd;
        assert!((got - hand(k as f64)).abs() < 1e-4, "frame {k}: {got}");
    }
    // the speed (frames advanced per frame) glides: no step larger than the linear one's
    let steps: Vec<f64> = (16..33).map(|k| (it.source_time_at(rate().tick_of(k + 1)) - it.source_time_at(rate().tick_of(k))).0 as f64 / fd).collect();
    assert!(steps.windows(2).all(|w| w[1] <= w[0] + 1e-9), "{steps:?}");
    assert!(steps[0] > 0.98 && steps[16] < 0.42, "{steps:?}");
}

#[test]
fn ripple_keeps_all_the_media_and_moves_what_follows() {
    let (mut s, c, m) = numbered();
    let r = ramp(&mut s, json!({"clip": c, "from": 100, "to": 40, "at": {"frame": 16}, "duration": {"frame": 16}, "ease": "linear", "ripple": true}));
    // 64 media frames: 16 at 100 %, 11.2 over the ramp, the remaining 36.8 at 40 % take 92 frames
    let want = 16 + 16 + 92;
    assert_eq!(r["duration"]["frame"], want, "{r}");
    assert_eq!(r["rippled"], true);
    assert!(r["warnings"].as_array().unwrap().is_empty(), "{r}");
    let it = clip(&s, c);
    assert_eq!(it.duration, rate().tick_of(want));
    assert_eq!(clip(&s, m).start, rate().tick_of(want), "the matte after it moved");
    assert_eq!(shown(&s, want - 1), 63, "the last frame of the media is the clip's last");
    assert_eq!(shown(&s, 0), 0);
    // undo puts everything back in one step
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(clip(&s, c).duration, rate().tick_of(64));
    assert_eq!(clip(&s, m).start, rate().tick_of(64));
    assert!(!clip(&s, c).has_speed_ramp());
}

#[test]
fn export_has_the_ramped_number_of_frames() {
    let (mut s, c, m) = numbered();
    ramp(&mut s, json!({"clip": c, "segments": [{"at": {"frame": 0}, "speed": 100}, {"at": {"frame": 32}, "speed": 50, "ease": "inOut"}], "ripple": true}));
    // 32 frames at a mean of 75 % spend 24 media frames; the other 40 at 50 % take 80 frames
    let it = clip(&s, c);
    assert_eq!(it.duration, rate().tick_of(112));
    assert_eq!(clip(&s, m).start, rate().tick_of(112), "the export range ends where the matte starts");
    let dir = tmp_dir("speed-ramp-export");
    let out = dir.join("ramp.mp4");
    let r = s
        .execute(
            "file.exportMedia",
            json!({"path": out.to_string_lossy(), "format": "h264", "range": "custom", "startSeconds": 0.0, "endSeconds": 112.0 / 25.0, "wait": true}),
        )
        .unwrap();
    assert_eq!(r["result"]["frames"], 112, "{r}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn hold_points_and_zero_speed_freeze_the_frame() {
    let (mut s, c, _) = numbered();
    ramp(
        &mut s,
        json!({"clip": c, "segments": [{"at": {"frame": 10}, "speed": 100}, {"at": {"frame": 11}, "speed": 0, "ease": "hold"}, {"at": {"frame": 21}, "speed": 100, "ease": "hold"}]}),
    );
    let seen: Vec<i64> = (8..24).map(|k| shown(&s, k)).collect();
    // the speed jumps to 0 at frame 11, which is still shown, and back to 100 % at frame 21
    assert_eq!(seen, vec![8, 9, 10, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 12, 13], "{seen:?}");
    // a ramp that ends on a hold never reaches the end of the media: ripple is refused
    let e = s.execute("clip.speedRamp", json!({"clip": c, "to": 0, "ripple": true})).unwrap_err().to_string();
    assert!(e.contains("never reach"), "{e}");
    // a frame-hold clip shows one frame: nothing to ramp
    s.execute("clip.speedRamp", json!({"clip": c, "clear": true})).unwrap();
    s.execute("clip.frameHoldOptions", json!({"clips": [c], "holdOn": "in"})).unwrap();
    assert!(clip(&s, c).frame_hold.is_some());
    let e = s.execute("clip.speedRamp", json!({"clip": c, "to": 50})).unwrap_err().to_string();
    assert!(e.contains("frame hold"), "{e}");
}

#[test]
fn reversed_clips_ramp_from_their_first_frame() {
    let (mut s, c, _) = numbered();
    s.execute("clip.speedDuration", json!({"clips": [c], "speed": 100, "reverse": true})).unwrap();
    assert_eq!(shown(&s, 0), 63);
    let r = ramp(&mut s, json!({"clip": c, "from": 100, "to": 50, "at": {"frame": 8}, "duration": {"frame": 8}, "ease": "linear", "ripple": true}));
    // 8 at 100 %, 6 over the ramp, the other 50 media frames at 50 % take 100 frames
    assert_eq!(r["duration"]["frame"], 116, "{r}");
    let it = clip(&s, c);
    assert!(it.reverse);
    let seen: Vec<i64> = (0..116).map(|k| shown(&s, k)).collect();
    assert_eq!(seen[0], 63, "the first frame stays");
    assert_eq!(seen[8], 55);
    assert!(seen.windows(2).all(|w| w[1] <= w[0]), "plays backwards all the way: {seen:?}");
    assert_eq!(*seen.last().unwrap(), 0, "and reaches the first media frame");
    // halfway through the slow part: 8 + 6 + 25 media frames from the top
    assert_eq!(seen[16 + 50], 63 - 39);
}

#[test]
fn linked_sound_is_muted_or_kept_and_follows_the_length() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let q = s.active_sequence().unwrap().clone();
    let v = q.video_tracks[0].items[0].clone();
    let link = v.link.expect("the demo's clips have linked sound");
    let a = q.audio_tracks.iter().flat_map(|t| t.items.iter()).find(|i| i.link == Some(link)).unwrap().clone();
    let next_audio = q.audio_tracks.iter().flat_map(|t| t.items.iter()).filter(|i| i.start >= a.end()).min_by_key(|i| i.start).map(|i| (i.id, i.start));
    // keep: the sound stays on, unchanged in speed, and warns that it drifts
    let r = ramp(&mut s, json!({"clip": v.id.0, "to": 50, "audio": "keep"}));
    assert_eq!(r["audio"]["action"], "kept");
    assert!(r["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("drifts")), "{r}");
    assert!(clip(&s, a.id.0).enabled);
    s.execute("edit.undo", json!({})).unwrap();
    // mute (default) with ripple: switched off, and its length changes with the picture's
    let r = ramp(&mut s, json!({"clip": v.id.0, "from": 100, "to": 100, "duration": 1, "ripple": true}));
    assert_eq!(r["rippled"], false, "100 % throughout changes nothing in length");
    s.execute("edit.undo", json!({})).unwrap();
    let r = ramp(&mut s, json!({"clip": v.id.0, "segments": [{"at": v.start.0, "speed": 200}], "ripple": true}));
    assert_eq!(r["audio"]["action"], "muted", "{r}");
    assert_eq!(r["audio"]["clips"], json!([a.id.0]));
    let (nv, na) = (clip(&s, v.id.0), clip(&s, a.id.0));
    assert!(!na.enabled, "the linked sound is switched off");
    let half = Tick(v.duration.0 / 2);
    assert!((nv.duration - half).abs() <= rate_of(&s).frame_duration(), "{:?} vs {:?}", nv.duration, half);
    assert_eq!(na.duration - a.duration, nv.duration - v.duration, "the sound's length follows the picture's");
    if let Some((id, start)) = next_audio {
        assert_eq!(clip(&s, id.0).start - start, nv.duration - v.duration, "later sound moved with the ripple");
    }
    s.active_sequence().unwrap().check().unwrap();
    // clear brings back the constant speed (the sound stays as it is, with a note)
    let r = ramp(&mut s, json!({"clip": v.id.0, "clear": true}));
    assert_eq!(r["cleared"], true);
    assert!(!clip(&s, v.id.0).has_speed_ramp());
    assert!(r["notes"].as_array().unwrap().iter().any(|n| n.as_str().unwrap().contains("clip.enable")), "{r}");
}

fn rate_of(s: &Session) -> FrameRate {
    s.sequence_rate()
}

#[test]
fn a_razored_ramp_shows_the_same_frames() {
    let (mut s, c, _) = numbered();
    ramp(&mut s, json!({"clip": c, "from": 100, "to": 40, "at": {"frame": 16}, "duration": {"frame": 16}}));
    let before: Vec<i64> = (0..64).map(|k| shown(&s, k)).collect();
    s.execute("timeline.razor", json!({"frame": 24, "track": "V1"})).unwrap();
    assert_eq!(s.active_sequence().unwrap().video_tracks[0].items.len(), 3);
    let after: Vec<i64> = (0..64).map(|k| shown(&s, k)).collect();
    assert_eq!(before, after);
}

#[test]
fn hostile_parameters_are_refused_without_a_change() {
    let (mut s, c, m) = numbered();
    let audio = {
        let dir = tmp_dir("speed-ramp-wav");
        let wav = dir.join("t.wav");
        std::fs::write(&wav, filmagic_media::wav::write_wav16(&[0.1; 48_000], 1, 48_000)).unwrap();
        let item = s.execute("file.import", json!({"paths": [wav.to_string_lossy()]})).unwrap()["items"][0].as_u64().unwrap();
        s.execute("timeline.place", json!({"item": item, "track": "A1", "seconds": 0.0})).unwrap()["clips"][0].as_u64().unwrap()
    };
    let snap = (*s.project).clone();
    let undo = s.history.undo.len();
    let big = (0..65).map(|i| json!({"at": i, "speed": 50})).collect::<Vec<_>>();
    let cases = [
        json!({"clip": c}),
        json!({"clip": c, "to": "fast"}),
        json!({"clip": c, "to": -10}),
        json!({"clip": c, "to": 1e9}),
        json!({"clip": c, "to": 50, "duration": 0}),
        json!({"clip": c, "to": 50, "duration": -5}),
        json!({"clip": c, "to": 50, "at": i64::MAX, "duration": i64::MAX}),
        json!({"clip": c, "to": 50, "at": {"seconds": -1.0}}),
        json!({"clip": c, "to": 50, "at": {"seconds": 100.0}}),
        json!({"clip": c, "to": 50, "ease": "bouncy"}),
        json!({"clip": c, "to": 50, "ease": 3}),
        json!({"clip": c, "to": 50, "audio": "loud"}),
        json!({"clip": c, "to": 50, "ripple": "yes"}),
        json!({"clip": c, "segments": []}),
        json!({"clip": c, "segments": big}),
        json!({"clip": c, "segments": "fast"}),
        json!({"clip": c, "segments": [1, 2]}),
        json!({"clip": c, "segments": [{"at": 10}]}),
        json!({"clip": c, "segments": [{"speed": 10}]}),
        json!({"clip": c, "segments": [{"at": 20, "speed": 50}, {"at": 10, "speed": 50}]}),
        json!({"clip": c, "segments": [{"at": 10, "speed": 50}], "to": 50}),
        // twice as fast without ripple needs media the clip does not have
        json!({"clip": c, "to": 200, "duration": 1}),
        json!({"clip": 999_999, "to": 50}),
        json!({"clip": -1, "to": 50}),
        json!({"clip": "x", "to": 50}),
        json!({"clip": audio, "to": 50}),
        json!({"to": 50}),
        json!([1, 2]),
    ];
    for p in &cases {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.execute("clip.speedRamp", p.clone())));
        assert!(r.is_ok(), "panicked on {p}");
        assert!(r.unwrap().is_err(), "accepted {p}");
        assert_eq!(*s.project, snap, "{p} changed the project");
        assert_eq!(s.history.undo.len(), undo, "{p} left an undo step");
    }
    // a locked track is refused too
    s.execute("timeline.setTrack", json!({"track": "V1", "locked": true})).unwrap();
    assert!(s.active_sequence().unwrap().video_tracks[0].locked);
    assert!(s.execute("clip.speedRamp", json!({"clip": c, "to": 50})).is_err());
    let _ = m;
}
