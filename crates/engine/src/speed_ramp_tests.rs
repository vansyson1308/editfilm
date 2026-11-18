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
    // a graphic has no frames to ramp
    let title = s.execute("graphics.newText", json!({"text": "x"})).unwrap()["clip"].as_u64().unwrap();
    let snap = (*s.project).clone();
    let e = s.execute("clip.speedRamp", json!({"clip": title, "to": 50})).unwrap_err().to_string();
    assert!(e.contains("no frames to ramp"), "{e}");
    assert_eq!(*s.project, snap);
    // a locked track is refused too
    s.execute("timeline.setTrack", json!({"track": "V1", "locked": true})).unwrap();
    assert!(s.active_sequence().unwrap().video_tracks[0].locked);
    assert!(s.execute("clip.speedRamp", json!({"clip": c, "to": 50})).is_err());
    let _ = m;
}

#[test]
fn smart_render_exports_the_ramped_pictures() {
    // smart render keys its segments on the pictures it encodes: a ramp changes them, so a
    // re-export after the ramp shows the ramped frames, not the cached ones
    let (mut s, c, _) = numbered();
    let dir = tmp_dir("speed-ramp-smart");
    let export = |s: &mut Session, name: &str| {
        let out = dir.join(name);
        let r = s
            .execute(
                "file.exportMedia",
                json!({"path": out.to_string_lossy(), "format": "h264", "smartRender": true, "range": "custom", "startSeconds": 0.0, "endSeconds": 112.0 / 25.0, "wait": true}),
            )
            .unwrap();
        assert_eq!(r["result"]["frames"], 112, "{r}");
        out
    };
    let before = export(&mut s, "before.mp4");
    ramp(&mut s, json!({"clip": c, "segments": [{"at": {"frame": 0}, "speed": 100}, {"at": {"frame": 32}, "speed": 50, "ease": "inOut"}], "ripple": true}));
    let after = export(&mut s, "after.mp4");
    let again = export(&mut s, "again.mp4");
    assert_ne!(std::fs::read(&before).unwrap(), std::fs::read(&after).unwrap());
    assert_eq!(std::fs::read(&after).unwrap(), std::fs::read(&again).unwrap(), "the second export reuses every segment");
    // read the exported file back: frame 80 shows media frame 24 + 0.5 × 48 = 48
    let item = s.execute("file.import", json!({"paths": [after.to_string_lossy()]})).unwrap()["items"][0].as_u64().unwrap();
    let src = s.media.source_for(&s.project, filmagic_project::ItemId(item), &*s.services).unwrap();
    let rate = src.info().frame_rate();
    let f = src.video_frame(filmagic_media::FrameRequest::full(rate.tick_of(80))).unwrap();
    let px = f.to_rgba8();
    let red = f64::from(px[((f.height as usize / 2) * f.width as usize + f.width as usize / 2) * 4]);
    assert!((red / 4.0 - 48.0).abs() <= 1.5, "frame 80 shows media frame {}", red / 4.0);
    let _ = std::fs::remove_dir_all(dir);
}

/// The remap keys of a clip: (linear media seconds, per cent of its own speed).
fn remap_keys(it: &TrackItem) -> Vec<(f64, f64)> {
    let p = it.effect("time_remap").and_then(|e| e.param("speed")).unwrap();
    p.keyframes.iter().map(|k| ((k.time.seconds() * 1e6).round() / 1e6, k.value.as_f64().unwrap())).collect()
}

#[test]
fn a_hold_survives_a_slowed_clip() {
    // at 50 % and 25 % two sequence times a tick apart fall on one linear tick: the hold kept
    // only the earlier key, and the speed after the jump stayed the old one
    for base in [50.0, 25.0] {
        let (mut s, c, _) = numbered();
        s.execute("clip.speedDuration", json!({"clips": [c], "speed": base, "ripple": true})).unwrap();
        ramp(&mut s, json!({"clip": c, "segments": [{"at": {"seconds": 0}, "speed": base}, {"at": {"seconds": 2}, "speed": 20, "ease": "hold"}]}));
        let it = clip(&s, c);
        let keys = remap_keys(&it);
        let last = keys.last().unwrap();
        assert!((last.1 - 20.0 / base * 100.0).abs() < 1e-9, "base {base}: {keys:?}");
        // the speed jumps at 2 s: 0.2 media seconds a second after it, the base speed before
        let fd = rate().frame_duration();
        let per_frame = |k: i64| (it.source_time_at(rate().tick_of(k + 1)) - it.source_time_at(rate().tick_of(k))).0 as f64 / fd.0 as f64;
        assert!((per_frame(25) - base / 100.0).abs() < 1e-6, "base {base}: {}", per_frame(25));
        assert!((per_frame(55) - 0.2).abs() < 1e-6, "base {base}: {}", per_frame(55));
    }
}

#[test]
fn keyframes_are_set_and_played_on_the_linear_clock() {
    let (mut s, c, _) = numbered();
    // a freeze from 1 s to 3 s, then normal speed
    ramp(
        &mut s,
        json!({"clip": c, "segments": [{"at": {"seconds": 0}, "speed": 100}, {"at": {"seconds": 1}, "speed": 0, "ease": "hold"}, {"at": {"seconds": 2.0}, "speed": 100, "ease": "hold"}], "ripple": true}),
    );
    // a Time Remapping key set at 2.4 s lands at the linear time of 2.4 s, not where its media is
    s.execute("effects.setParam", json!({"clip": c, "effect": "time_remap", "param": "speed", "value": 50.0, "time": {"seconds": 2.4}})).unwrap();
    let it = clip(&s, c);
    assert!(remap_keys(&it).iter().any(|k| (k.0 - 2.4).abs() < 1e-6 && k.1 == 50.0), "{:?}", remap_keys(&it));
    // Motion keys inside the freeze both stay, and animate through it
    s.execute("effects.setParam", json!({"clip": c, "effect": "motion", "param": "scale", "value": 50.0, "time": {"seconds": 1.2}, "keyframe": true})).unwrap();
    s.execute("effects.setParam", json!({"clip": c, "effect": "motion", "param": "scale", "value": 150.0, "time": {"seconds": 1.8}, "keyframe": true}))
        .unwrap();
    let it = clip(&s, c);
    let scale = it.effect("motion").and_then(|e| e.param("scale")).unwrap();
    assert_eq!(scale.keyframes.len(), 2, "{:?}", scale.keyframes);
    let at = |sec: f64| scale.f64_at(it.effect_time_at(Tick::from_seconds_f64(sec)));
    assert!((at(1.2) - 50.0).abs() < 1e-6 && (at(1.8) - 150.0).abs() < 1e-6);
    assert!((at(1.5) - 100.0).abs() < 1e-6, "it animates while the frame is frozen: {}", at(1.5));
    assert_eq!(it.source_time_at(Tick::from_seconds_f64(1.2)), it.source_time_at(Tick::from_seconds_f64(1.8)), "the frame is frozen");
}

#[test]
fn razored_ramps_join_and_re_ramp_from_their_first_frame() {
    let (mut s, c, _) = numbered();
    ramp(&mut s, json!({"clip": c, "from": 100, "to": 50, "at": {"seconds": 0}, "duration": {"seconds": 1}, "ripple": true}));
    s.execute("timeline.razor", json!({"time": {"seconds": 1.6}, "track": "V1"})).unwrap();
    let q = s.active_sequence().unwrap().clone();
    let (a, b) = (q.video_tracks[0].items[0].clone(), q.video_tracks[0].items[1].clone());
    assert!(filmagic_edit::through::is_through_edit(&a, &b), "the cut through a ramp is a through edit");
    // the right piece, ramped again, starts on the frame it showed
    let first = b.source_time_at(b.start);
    assert_ne!(first, b.linear_source_time_at(b.start), "the piece's linear clock is ahead of its first frame");
    ramp(&mut s, json!({"clip": b.id.0, "from": 50, "to": 100, "duration": {"seconds": 0.5}, "ripple": true}));
    let nb = clip(&s, b.id.0);
    assert_eq!(nb.source_time_at(nb.start), first);
    // and so does it cleared
    s.execute("edit.undo", json!({})).unwrap();
    ramp(&mut s, json!({"clip": b.id.0, "clear": true, "ripple": true}));
    let nb = clip(&s, b.id.0);
    assert!(!nb.has_speed_ramp());
    assert_eq!(nb.source_time_at(nb.start), first);
}

#[test]
fn clearing_nothing_changes_nothing_and_over_cap_speeds_are_refused() {
    let (mut s, c, _) = numbered();
    let undo = s.history.undo.len();
    let r = ramp(&mut s, json!({"clip": c, "clear": true}));
    assert_eq!(r["cleared"], false);
    assert_eq!(s.history.undo.len(), undo, "no undo step");
    // all-100 % keys are no ramp
    ramp(&mut s, json!({"clip": c, "from": 100, "to": 100}));
    assert!(!clip(&s, c).has_speed_ramp());
    // a speed over 1000× the clip's own is refused, not capped in silence
    s.execute("clip.speedDuration", json!({"clips": [c], "speed": 1, "ripple": true})).unwrap();
    let e = s.execute("clip.speedRamp", json!({"clip": c, "to": 5000})).unwrap_err().to_string();
    assert!(e.contains("1000 times"), "{e}");
    // a second ramp says it replaced the first
    let (mut s, c, _) = numbered();
    ramp(&mut s, json!({"clip": c, "to": 50}));
    let r = ramp(&mut s, json!({"clip": c, "to": 70}));
    assert!(r["notes"].as_array().unwrap().iter().any(|n| n.as_str().unwrap().contains("replaced")), "{r}");
}

#[test]
fn ramping_a_clip_with_followed_keys_warns() {
    let (mut s, c, _) = numbered();
    for k in 0..10 {
        s.execute(
            "effects.setParam",
            json!({"clip": c, "effect": "motion", "param": "position", "value": [8.0 + k as f64, 8.0], "time": {"frame": k * 4}, "keyframe": true}),
        )
        .unwrap();
    }
    let r = ramp(&mut s, json!({"clip": c, "to": 50}));
    assert!(r["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("no longer line up")), "{r}");
}

#[test]
fn a_ramped_sound_clip_is_flagged_and_can_be_cleared() {
    // only a hand-edited project gives a sound clip a ramp: it is silent, and says so
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let a = s.active_sequence().unwrap().audio_tracks[0].items[0].id;
    {
        let id = s.state.active_sequence.unwrap();
        let q = std::sync::Arc::make_mut(&mut s.project).sequence_mut(id).unwrap();
        let (_, it) = q.find_item_mut(a).unwrap();
        if it.effect("time_remap").is_none() {
            it.effects.push(filmagic_project::find_effect("time_remap").unwrap().instance());
        }
        let p = it.effect_mut("time_remap").unwrap().param_mut("speed").unwrap();
        p.keyframes = vec![filmagic_project::Keyframe::new(Tick(0), filmagic_project::ParamValue::Float(50.0))];
        assert!(it.has_speed_ramp());
    }
    let listed = s.execute("sequence.clips", json!({})).unwrap();
    let entry = listed["tracks"].as_array().unwrap().iter().flat_map(|t| t["clips"].as_array().unwrap().iter()).find(|c| c["clip"] == a.0).unwrap().clone();
    assert!(entry["speedRamp"].as_str().unwrap().starts_with("silent"), "{entry}");
    let mix = s.execute("mix.check", json!({})).unwrap();
    assert!(mix["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("speed ramp")), "{mix}");
    let r = s.execute("clip.speedRamp", json!({"clip": a.0, "clear": true})).unwrap();
    assert_eq!(r["cleared"], true);
    assert!(!clip(&s, a.0).has_speed_ramp());
}
