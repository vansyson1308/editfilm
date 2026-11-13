//! Audio safety (#15): speech-aware Auto-Match with a gain cap, `audio.speechSegments`, speech
//! warnings on razor and trim, ducking depth and speech triggers, and `mix.check`. Every signal is
//! synthetic and generated here (speech-like voiced tones with pauses, noise bursts, tones).

use std::sync::Arc;

use filmagic_audio_dsp::speech::test_signals::{bursts, noise, speech, tone};
use filmagic_project::{ClipId, TrackItem};
use filmagic_time::Tick;
use serde_json::{Value, json};

use super::*;

const SR: u32 = 48_000;

fn add(a: &[f32], b: &[f32]) -> Vec<f32> {
    a.iter().zip(b).map(|(x, y)| x + y).collect()
}

/// A session with one sequence and each signal on its own audio track (A1, A2…) at `start` seconds.
fn session_with(tracks: &[(Vec<f32>, f64)]) -> (Session, Vec<ClipId>, Vec<filmagic_project::ItemId>) {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name": "audio", "audio": tracks.len().max(2), "video": 1})).unwrap();
    let mut clips = Vec::new();
    let mut items = Vec::new();
    for (k, (x, start)) in tracks.iter().enumerate() {
        let inter: Vec<f32> = x.iter().flat_map(|v| [*v, *v]).collect();
        let bytes: Arc<[u8]> = crate::previews::write_wav_f32(&inter, SR).into();
        let item = crate::commands::import_bytes(&mut s, &format!("/sig{k}.wav"), bytes, None).unwrap();
        let r = s.execute("timeline.place", json!({"item": item.0, "audioTrack": format!("A{}", k + 1), "seconds": start})).unwrap();
        clips.push(ClipId(r["clips"][0].as_u64().unwrap()));
        items.push(item);
    }
    s.execute("edit.deselectAll", json!({})).unwrap();
    (s, clips, items)
}

fn item(s: &Session, c: ClipId) -> TrackItem {
    s.active_sequence().unwrap().find_item(c).unwrap().1.clone()
}

fn segs(v: &Value) -> Vec<(f64, f64)> {
    v["segments"].as_array().unwrap().iter().map(|g| (g["start"].as_f64().unwrap(), g["end"].as_f64().unwrap())).collect()
}

fn near(got: &[(f64, f64)], want: &[(f64, f64)], tol: f64) {
    assert_eq!(got.len(), want.len(), "segments {got:?}, expected {want:?}");
    for ((a, b), (x, y)) in got.iter().zip(want) {
        assert!((a - x).abs() <= tol && (b - y).abs() <= tol, "segment [{a}, {b}], expected [{x}, {y}]");
    }
}

fn err(s: &mut Session, cmd: &str, p: Value) -> String {
    match s.execute(cmd, p.clone()) {
        Ok(v) => panic!("{cmd} {p} should fail, got {v}"),
        Err(e) => e.to_string(),
    }
}

/// #15 (15): a car-door clip with no speech was pushed +27.8 dB towards the dialogue target.
#[test]
fn auto_match_does_not_push_a_car_door_to_the_dialogue_target() {
    let door = bursts(8.0, SR, 0.02, &[1.0, 4.0], 0.4);
    let voice = speech(8.0, SR, 0.1, |t| (0.5..3.0).contains(&t) || (4.0..7.0).contains(&t));
    let (mut s, c, _) = session_with(&[(door, 0.0), (voice, 0.0)]);
    for k in &c {
        s.execute("essentialSound.setType", json!({"clips": [k.0], "type": "dialogue"})).unwrap();
    }
    let r = s.execute("essentialSound.autoMatch", json!({"clips": [c[0].0, c[1].0], "target": -16.0})).unwrap();
    println!("{r:#}");
    let rows = r["clips"].as_array().unwrap();
    let door_row = rows.iter().find(|x| x["clip"] == c[0].0).unwrap();
    assert_eq!(door_row["skipped"], "no speech", "{r}");
    assert_eq!(item(&s, c[0]).gain_db, 0.0, "the car door keeps its level");
    let w = r["warnings"].as_array().unwrap();
    assert!(w.iter().any(|x| x.as_str().unwrap().contains(&format!("clip {}", c[0].0)) && x.as_str().unwrap().contains("essentialSound.setType")), "{r}");
    // the voice is matched over its speech
    let voice_row = rows.iter().find(|x| x["clip"] == c[1].0).unwrap();
    assert_eq!(voice_row["measuredOver"], "speech");
    assert!((voice_row["speechSeconds"].as_f64().unwrap() - 5.5).abs() < 0.15, "{voice_row}");
    assert!(voice_row["gainDb"].as_f64().unwrap().abs() <= 12.0);

    // typed as a sound effect it gets the SFX target, never the dialogue `target`, capped at 12 dB
    s.execute("essentialSound.setType", json!({"clips": [c[0].0], "type": "sfx"})).unwrap();
    let r = s.execute("essentialSound.autoMatch", json!({"clips": [c[0].0], "target": -16.0})).unwrap();
    let row = &r["clips"][0];
    println!("{row}");
    assert_eq!(row["targetLufs"], -21.0, "the SFX preference target");
    let wanted = row["wantedGainDb"].as_f64().unwrap();
    assert!(wanted > 12.0, "this quiet door would need {wanted} dB");
    assert_eq!(row["gainDb"], 12.0);
    assert_eq!(row["capped"], true);
    assert!(r["warnings"][0].as_str().unwrap().contains("capped"), "{r}");
    assert_eq!(item(&s, c[0]).gain_db, 12.0);
    // an explicit per-type target and a wider cap
    let r = s.execute("essentialSound.autoMatch", json!({"clips": [c[0].0], "targets": {"sfx": -30.0}, "maxGainDb": 30})).unwrap();
    assert_eq!(r["clips"][0]["targetLufs"], -30.0);
    assert!(r["clips"][0].get("capped").is_none(), "{r}");
}

#[test]
fn auto_match_rejects_hostile_parameters() {
    let voice = speech(4.0, SR, 0.1, |t| (0.5..3.5).contains(&t));
    let (mut s, c, _) = session_with(&[(voice, 0.0)]);
    s.execute("essentialSound.setType", json!({"clips": [c[0].0], "type": "dialogue"})).unwrap();
    for (p, want) in [
        (json!({"maxGainDb": "x"}), "maxGainDb"),
        (json!({"maxGainDb": -1}), "maxGainDb"),
        (json!({"maxGainDb": 1e9}), "maxGainDb"),
        (json!({"maxGainDb": true}), "maxGainDb"),
        (json!({"target": "loud"}), "`target` must be a loudness in LUFS"),
        (json!({"target": 5.0}), "`target` must be a loudness in LUFS"),
        (json!({"target": -1000}), "`target` must be a loudness in LUFS"),
        (json!({"targets": []}), "`targets` must be an object"),
        (json!({"targets": {"bogus": -10}}), "unknown audio type `bogus`"),
        (json!({"targets": {"music": "x"}}), "`targets.music` must be"),
        (json!({"targets": {"music": 1e300}}), "`targets.music` must be"),
    ] {
        let mut q = p.clone();
        q["clips"] = json!([c[0].0]);
        let e = err(&mut s, "essentialSound.autoMatch", q);
        assert!(e.contains(want), "{p}: {e}");
    }
    let e = err(&mut s, "essentialSound.autoMatch", json!({"clips": [999_999]}));
    assert!(e.contains("no clips"), "{e}");
    assert_eq!(item(&s, c[0]).gain_db, 0.0, "nothing changed");
    assert!(s.history.undo.iter().all(|h| h.0 != "Auto-Match Loudness"));
}

#[test]
fn speech_segments_of_a_clip_an_item_and_a_range() {
    let voice = speech(8.0, SR, 0.4, |t| (1.0..3.2).contains(&t) || (4.0..6.5).contains(&t));
    let door = bursts(10.0, SR, 0.8, &[0.5, 9.2], 0.4);
    let (mut s, c, items) = session_with(&[(voice, 2.0), (door, 0.0)]);
    let want_seq = [(3.0, 5.2), (6.0, 8.5)];
    let r = s.execute("audio.speechSegments", json!({"clip": c[0].0})).unwrap();
    println!("clip: {r}");
    near(&segs(&r), &want_seq, 0.06);
    assert_eq!(r["timeBase"], "sequence");
    assert_eq!(r["pauses"].as_array().unwrap().len(), 1);
    assert!(r["segments"][0]["confidence"].as_f64().unwrap() > 0.5);
    // the project item, in source seconds
    let r = s.execute("audio.speechSegments", json!({"item": items[0].0})).unwrap();
    println!("item: {r}");
    near(&segs(&r), &[(1.0, 3.2), (4.0, 6.5)], 0.06);
    assert_eq!(r["timeBase"], "source");
    // a range of one track, and of the whole mix (the bursts are not speech)
    let r = s.execute("audio.speechSegments", json!({"startSeconds": 0, "endSeconds": 10, "track": "A1"})).unwrap();
    near(&segs(&r), &want_seq, 0.06);
    let r = s.execute("audio.speechSegments", json!({"startSeconds": 2.5, "endSeconds": 10})).unwrap();
    println!("mix from 2.5 s: {r}");
    near(&segs(&r), &want_seq, 0.06);
    let r = s.execute("audio.speechSegments", json!({"clip": c[1].0})).unwrap();
    assert!(segs(&r).is_empty(), "car-door bursts are not speech: {r}");
    // hostile parameters
    for (p, want) in [
        (json!({"clip": 999_999}), "no clip 999999"),
        (json!({"clip": "x"}), "`clip` must be a clip id"),
        (json!({"clip": -1}), "no clip 0"),
        (json!({"item": 999_999}), "no project item 999999"),
        (json!({"item": c[0].0 + 10_000}), "no project item"),
        (json!({"startSeconds": -1}), "`startSeconds` must be a number of seconds"),
        (json!({"startSeconds": "x"}), "`startSeconds` must be a number of seconds"),
        (json!({"startSeconds": 1e300}), "is empty"),
        (json!({"startSeconds": 5, "endSeconds": 1}), "is empty"),
        (json!({"minConfidence": 2}), "`minConfidence` must be a number from 0 to 1"),
        (json!({"minConfidence": "high"}), "`minConfidence` must be a number from 0 to 1"),
        (json!({"bridgeSeconds": 5}), "`bridgeSeconds` must be"),
        (json!({"bridgeSeconds": -0.1}), "`bridgeSeconds` must be"),
        (json!({"item": items[0].0, "stream": 7}), "`stream` must be an audio stream index"),
        (json!({"item": items[0].0, "stream": -1}), "`stream` must be an audio stream index"),
        (json!({"track": "A99"}), "no track A99"),
        (json!({"track": "V1"}), "`track` must be an audio track"),
    ] {
        let e = err(&mut s, "audio.speechSegments", p.clone());
        assert!(e.contains(want), "{p}: {e}");
    }
}

/// #15 (20): a razor or trim inside a sentence warns, with the nearest pauses.
#[test]
fn razor_and_trim_warn_when_a_cut_lands_inside_speech() {
    let voice = speech(10.0, SR, 0.4, |t| (1.0..3.2).contains(&t) || (4.0..6.5).contains(&t));
    let music = add(&tone(10.0, SR, 220.0, 0.2), &tone(10.0, SR, 330.0, 0.1));
    let (mut s, c, _) = session_with(&[(voice, 0.0), (music.clone(), 0.0)]);
    s.execute("essentialSound.setType", json!({"clips": [c[1].0], "type": "music"})).unwrap();
    let f = |v: &Value| v.as_f64().unwrap();
    // the check can be turned off per call, and is off for the desktop app's own clicks
    let r = s.execute("timeline.razor", json!({"seconds": 2.0, "track": "A1", "speechCheck": false})).unwrap();
    assert!(r.get("speechCuts").is_none(), "{r}");
    s.execute("edit.undo", json!({})).unwrap();
    s.cut_speech_check = false;
    let r = s.execute("timeline.razor", json!({"seconds": 2.0, "track": "A1"})).unwrap();
    assert!(r.get("speechCuts").is_none(), "{r}");
    s.execute("edit.undo", json!({})).unwrap();
    s.cut_speech_check = true;
    // inside the first sentence: the cut points suggested are in the pauses around it
    let r = s.execute("timeline.razor", json!({"seconds": 2.0, "track": "A1"})).unwrap();
    println!("{r:#}");
    let cut = &r["speechCuts"][0];
    assert_eq!(cut["clip"], c[0].0);
    assert!((f(&cut["segment"][0]) - 1.0).abs() < 0.06 && (f(&cut["segment"][1]) - 3.2).abs() < 0.06, "{cut}");
    assert!((f(&cut["pauseBefore"]) - 0.9).abs() < 0.06, "0.1 s before the speech, its pause starting at the clip start: {cut}");
    assert_eq!(cut["pauseBeforeRange"][0], Value::Null, "{cut}");
    assert!((f(&cut["pauseAfter"]) - 3.6).abs() < 0.06, "the middle of the 3.2–4.0 s pause: {cut}");
    assert!((f(&cut["pauseAfterRange"][0]) - 3.2).abs() < 0.06 && (f(&cut["pauseAfterRange"][1]) - 4.0).abs() < 0.06, "{cut}");
    let w = r["warnings"][0].as_str().unwrap();
    assert!(w.contains("inside speech") && w.contains("3.6") && w.contains("edit.undo"), "{w}");
    // in the pause: no warning
    let r = s.execute("timeline.razor", json!({"seconds": 3.6, "track": "A1"})).unwrap();
    assert!(r.get("warnings").is_none(), "{r}");
    // music is never a sentence
    let r = s.execute("timeline.razor", json!({"seconds": 5.0, "track": "A2"})).unwrap();
    assert!(r.get("warnings").is_none(), "{r}");
    // trimming the last part's out edge to 5.0 s lands in the second sentence; the check looks
    // at the media past the new edge too
    let last = s.active_sequence().unwrap().audio_tracks[0].items.iter().max_by_key(|i| i.start).unwrap().id;
    let end = item(&s, last).end();
    let r = s.execute("timeline.trim", json!({"clip": last.0, "edge": "out", "delta": (Tick::from_seconds_f64(5.0) - end).0})).unwrap();
    println!("{r:#}");
    let cut = &r["speechCuts"][0];
    assert!((f(&cut["pauseBefore"]) - 3.6).abs() < 0.06 && (f(&cut["pauseAfter"]) - 6.6).abs() < 0.06, "{cut}");
    // Add Edit warns the same way
    let r = s.execute("sequence.addEdit", json!({"seconds": 4.5})).unwrap();
    assert!(r["speechCuts"].as_array().is_some_and(|a| !a.is_empty()), "{r}");
}

/// A linked pair of clips from one recording (a stereo L/R breakout) warns once per cut.
#[test]
fn a_linked_pair_warns_once() {
    let voice = speech(6.0, SR, 0.4, |t| (1.0..4.0).contains(&t));
    let (mut s, c, items) = session_with(&[(voice, 0.0)]);
    // the same file again on A2, linked to the first clip
    let r = s.execute("timeline.place", json!({"item": items[0].0, "audioTrack": "A2", "seconds": 0.0})).unwrap();
    let second = ClipId(r["clips"][0].as_u64().unwrap());
    s.execute("clip.link", json!({"clips": [c[0].0, second.0]})).unwrap();
    assert!(item(&s, c[0]).link.is_some() && item(&s, c[0]).link == item(&s, second).link, "the clips are linked");
    let r = s.execute("timeline.razor", json!({"seconds": 2.0})).unwrap();
    assert_eq!(r["cuts"], 2, "{r}");
    assert_eq!(r["speechCuts"].as_array().map(Vec::len), Some(1), "{r}");
}

/// #15 (16): ducking takes a depth and, under dialogue, follows speech rather than room tone.
#[test]
fn ducking_takes_a_depth_and_follows_speech_not_room_tone() {
    let dlg = add(&speech(10.0, SR, 0.4, |t| (2.0..4.0).contains(&t) || (6.0..7.5).contains(&t)), &noise(10.0, SR, 0.05, 11));
    let music = add(&tone(10.0, SR, 220.0, 0.2), &tone(10.0, SR, 330.0, 0.1));
    let (mut s, c, _) = session_with(&[(dlg, 0.0), (music, 0.0)]);
    s.execute("essentialSound.setType", json!({"clips": [c[0].0], "type": "dialogue"})).unwrap();
    s.execute("essentialSound.setType", json!({"clips": [c[1].0], "type": "music"})).unwrap();
    let r = s.execute("essentialSound.generateDucking", json!({"clips": [c[1].0], "depthDb": 18})).unwrap();
    println!("{r:#}");
    let row = &r["clips"][0];
    assert_eq!(row["by"], "speech");
    assert_eq!(row["depthDb"], 18.0);
    let regions: Vec<(f64, f64)> = row["regions"].as_array().unwrap().iter().map(|x| (x[0].as_f64().unwrap(), x[1].as_f64().unwrap())).collect();
    near(&regions, &[(2.0, 4.0), (6.0, 7.5)], 0.08);
    let min = row["keyframes"].as_array().unwrap().iter().map(|k| k["levelDb"].as_f64().unwrap()).fold(f64::INFINITY, f64::min);
    assert_eq!(min, -18.0);
    assert_eq!(item(&s, c[1]).essential.unwrap().ducking.reduce_db, -18.0, "the depth is kept in the settings");
    // the level detector hears the room tone and ducks the whole clip
    let r = s.execute("essentialSound.generateDucking", json!({"clips": [c[1].0], "by": "level"})).unwrap();
    assert_eq!(r["clips"][0]["by"], "level");
    let n = r["clips"][0]["regions"].as_array().unwrap().len();
    println!("level regions: {}", r["clips"][0]["regions"]);
    assert_eq!(n, 1, "room tone holds the music down throughout");
    for (p, want) in [
        (json!({"depthDb": 100}), "`depthDb` must be"),
        (json!({"depthDb": -5}), "a positive number"),
        (json!({"depthDb": "x"}), "`depthDb` must be"),
        (json!({"by": "foo"}), "`by` must be \"speech\""),
        (json!({"by": 3}), "`by` must be \"speech\""),
    ] {
        let mut q = p.clone();
        q["clips"] = json!([c[1].0]);
        let e = err(&mut s, "essentialSound.generateDucking", q);
        assert!(e.contains(want), "{p}: {e}");
    }
}

/// #15 (16): `mix.check` measures the music under the dialogue, where the dialogue speaks.
#[test]
fn mix_check_reports_music_under_dialogue() {
    let dlg = add(&speech(10.0, SR, 0.4, |t| (1.0..4.0).contains(&t) || (5.0..8.5).contains(&t)), &noise(10.0, SR, 0.08, 5));
    let music = add(&tone(10.0, SR, 220.0, 0.08), &tone(10.0, SR, 330.0, 0.04));
    let (mut s, c, _) = session_with(&[(dlg, 0.0), (music, 0.0)]);
    s.execute("essentialSound.setType", json!({"clips": [c[0].0], "type": "dialogue"})).unwrap();
    s.execute("essentialSound.setType", json!({"clips": [c[1].0], "type": "music"})).unwrap();
    let r = s.execute("mix.check", json!({})).unwrap();
    println!("{r:#}");
    let speech_l = r["roles"]["Dialogue"]["speech"]["loudness"]["integratedLufs"].as_f64().unwrap();
    let whole_l = r["roles"]["Dialogue"]["loudness"]["integratedLufs"].as_f64().unwrap();
    assert!(speech_l > whole_l + 0.5, "speech {speech_l} LUFS should read above the dialogue track with its room tone {whole_l} LUFS");
    let under = r["roles"]["Music"]["underSpeech"]["loudness"]["integratedLufs"].as_f64().unwrap();
    let b = &r["musicUnderDialogue"];
    let diff = b["differenceDb"].as_f64().unwrap();
    assert!((diff - (speech_l - under)).abs() < 0.02, "{b}");
    assert_eq!(b["targetDb"], json!([15.0, 20.0]));
    assert!(diff < 15.0, "this music is too loud: {diff} dB");
    assert_eq!(b["verdict"], "musicTooLoud");
    assert!((b["musicGainChangeDb"].as_f64().unwrap() - (diff - 15.0)).abs() < 0.02);
    // the ducking depth suggested is what is missing to the middle of the range, not the range
    let depth = format!("{{\"depthDb\": {:.0}}}", 17.5 - diff);
    assert!(r["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains(&depth)), "{depth}: {}", r["warnings"]);
    assert_eq!(r["tracks"].as_array().unwrap().len(), 2);
    assert!(r["mix"]["truePeakDbtp"].as_f64().is_some() && r["mix"]["lraLu"].as_f64().is_some());
    // duck by 10 dB: the difference grows by about that much
    s.execute("essentialSound.generateDucking", json!({"clips": [c[1].0], "depthDb": 10})).unwrap();
    let after = s.execute("mix.check", json!({})).unwrap()["musicUnderDialogue"]["differenceDb"].as_f64().unwrap();
    println!("difference {diff:.2} dB → {after:.2} dB after ducking 10 dB");
    assert!((8.0..=10.5).contains(&(after - diff)), "{diff} → {after}");
    let wide = s.execute("mix.check", json!({"targetDb": [0, 60]})).unwrap();
    assert_eq!(wide["musicUnderDialogue"]["verdict"], "ok");
    for (p, want) in [
        (json!({"targetDb": [20, 15]}), "`targetDb` must be [min, max]"),
        (json!({"targetDb": [-1, 5]}), "`targetDb` must be [min, max]"),
        (json!({"targetDb": "x"}), "`targetDb` must be [min, max]"),
        (json!({"targetDb": [1, 2, 3]}), "`targetDb` must be [min, max]"),
        (json!({"targetDb": [1, "x"]}), "`targetDb` must be [min, max]"),
        (json!({"startSeconds": -1}), "`startSeconds` must be a number of seconds"),
        (json!({"startSeconds": 3, "endSeconds": 2}), "is empty"),
    ] {
        let e = err(&mut s, "mix.check", p.clone());
        assert!(e.contains(want), "{p}: {e}");
    }
}

#[test]
fn mix_check_without_roles_says_what_to_do() {
    let voice = speech(4.0, SR, 0.4, |t| (0.5..3.5).contains(&t));
    let (mut s, _, _) = session_with(&[(voice, 0.0)]);
    let r = s.execute("mix.check", json!({})).unwrap();
    assert_eq!(r["musicUnderDialogue"]["verdict"], "unknown");
    let notes = r["notes"].as_array().unwrap();
    assert!(notes.iter().any(|n| n.as_str().unwrap().contains("essentialSound.setType")), "{r}");
    // an empty sequence: an error that says why
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name": "empty"})).unwrap();
    let e = err(&mut s, "mix.check", json!({}));
    assert!(e.contains("empty"), "{e}");
}

/// #15 (17): an export reports the true peak of what it wrote, and warns when it can clip.
#[test]
fn export_reports_its_true_peak_and_warns_without_the_limiter() {
    let dir = filmagic_testkit::workspace_root().join("target").join("export-tests").join(format!("engine-peak-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = |n: &str| dir.join(n).to_string_lossy().to_string();
    // a 440 Hz tone at +2.7 dBFS (a float WAV holds it)
    let (mut s, _, _) = session_with(&[(tone(1.0, SR, 440.0, 1.364), 0.0)]);
    let r = s.execute("file.exportMedia", json!({"path": path("hot.wav"), "format": "wav", "wait": true})).unwrap();
    println!("{:#}", r["result"]["audio"]);
    let a = &r["result"]["audio"];
    assert_eq!(a["limiter"], "truePeak");
    assert!(a["truePeakDbtp"].as_f64().unwrap() <= -0.9, "{a}");
    assert!((a["limitedDb"].as_f64().unwrap() - 3.8).abs() < 0.15, "{a}");
    let r = s.execute("file.exportMedia", json!({"path": path("hot_off.wav"), "format": "wav", "wait": true, "truePeakLimiter": false})).unwrap();
    let a = &r["result"]["audio"];
    assert_eq!(a["limiter"], "off");
    assert!(a["truePeakDbtp"].as_f64().unwrap() > 2.0, "{a}");
    assert!(r["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("may clip")), "the warning reaches the caller: {r}");
    for bad in [json!("yes"), json!(1), json!([true])] {
        let e = err(&mut s, "file.exportMedia", json!({"path": path("bad.wav"), "format": "wav", "wait": true, "truePeakLimiter": bad}));
        assert!(e.contains("truePeakLimiter"), "{e}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn peak(ch: &[Vec<f32>]) -> f32 {
    ch.iter().flat_map(|c| c.iter()).fold(0.0f32, |m, v| m.max(v.abs()))
}

/// #15 review: the export's True-Peak Limiter is for deliveries. A proxy, transcode or Project
/// Manager copy (which replaces the original media) and AAF/OMF clip audio keep the peaks of a
/// tone at 0.97 of full scale (-0.26 dBFS: above the -1 dBTP ceiling, under clipping).
#[test]
fn internal_renders_keep_peaks_above_the_limiter_ceiling() {
    let dir = crate::media_test_util::tmp_dir("hot-internal");
    let (mut s, c, items) = session_with(&[(tone(2.0, SR, 440.0, 0.97), 0.0)]);
    let item_id = items[0];
    // a transcode of a range (Project Manager consolidate and trim call this)
    let m = s.project.item(item_id).and_then(|i| i.as_media()).cloned().unwrap();
    let src = s.media.provider(s.project.clone(), s.services.clone());
    let src = filmagic_render::SourceProvider::source(&src, item_id).unwrap();
    let out = dir.join("copy.wav").to_string_lossy().into_owned();
    let range = filmagic_time::TimeRange::new(Tick::ZERO, Tick::from_seconds_f64(1.0));
    crate::proxies::transcode(&m, "hot", src, Some(range), &out, &crate::proxies::PRESETS[0], &Default::default()).unwrap();
    let copy = filmagic_codecs::open_bytes("copy.wav", std::fs::read(&out).unwrap().into()).unwrap();
    let p = peak(&copy.audio(0, 24_000, SR).unwrap().channels);
    assert!(p > 0.96, "the transcode was limited: peak {p}");
    // OMF with rendered clip audio (the per-clip render), then read back
    let path = dir.join("hot.omf").to_string_lossy().into_owned();
    let seq = s.state.active_sequence.unwrap();
    s.execute("file.exportOmf", json!({"path": path, "renderAudioEffects": true, "handles": 0, "sequence": seq.0})).unwrap();
    let r = s.execute("file.importAaf", json!({"path": path})).unwrap();
    let docs = r["documents"].as_array().cloned().unwrap_or_default();
    let back = r["sequences"][0]
        .as_u64()
        .or_else(|| docs.first().and_then(|d| d["sequences"][0].as_u64()))
        .map(filmagic_project::ItemId)
        .unwrap_or_else(|| panic!("{r}"));
    let a = s.project.sequence(back).unwrap().audio_tracks[0].items[0].clone();
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let got = filmagic_render::SourceProvider::source(&provider, a.item).unwrap().audio(a.source_in.to_units_floor(48_000), 24_000, SR).unwrap();
    let p = peak(&got.channels);
    assert!(p > 0.96, "the OMF clip audio was limited: peak {p} (clip {})", c[0].0);
    let _ = std::fs::remove_dir_all(dir);
}

/// Ducking under two Dialogue tracks that speak in turn and overlap: the music ducks under the
/// union of their speech.
#[test]
fn ducking_follows_the_speech_of_overlapping_dialogue_tracks() {
    let a = speech(10.0, SR, 0.4, |t| (1.0..3.0).contains(&t) || (5.0..6.0).contains(&t));
    let b = speech(10.0, SR, 0.3, |t| (2.5..4.0).contains(&t) || (7.5..8.5).contains(&t));
    let music = add(&tone(10.0, SR, 220.0, 0.2), &tone(10.0, SR, 330.0, 0.1));
    let (mut s, c, _) = session_with(&[(a, 0.0), (b, 0.0), (music, 0.0)]);
    for k in [0, 1] {
        s.execute("essentialSound.setType", json!({"clips": [c[k].0], "type": "dialogue"})).unwrap();
    }
    s.execute("essentialSound.setType", json!({"clips": [c[2].0], "type": "music"})).unwrap();
    s.execute("essentialSound.set", json!({"clips": [c[2].0], "key": "ducking.fadeS", "value": 0.2})).unwrap();
    let r = s.execute("essentialSound.generateDucking", json!({"clips": [c[2].0], "depthDb": 12})).unwrap();
    let row = &r["clips"][0];
    assert_eq!(row["by"], "speech");
    assert_eq!(row["triggers"].as_array().unwrap().len(), 2);
    // keyframes: ducked across 1–4 s (the two overlapping turns), 5–6 s and 7.5–8.5 s
    let lvl = item(&s, c[2]).effect("volume").unwrap().param("level").unwrap().clone();
    let at = |t: f64| lvl.f64_at(item(&s, c[2]).source_time_at(Tick::from_seconds_f64(t)));
    for t in [1.5, 2.8, 3.5, 5.5, 8.0] {
        assert_eq!(at(t), -12.0, "ducked at {t} s: {row}");
    }
    for t in [0.1, 4.5, 6.75, 9.6] {
        let v = at(t);
        assert!(v > -12.0, "open (or ramping) at {t} s: {v}");
    }
    assert_eq!(at(9.6), 0.0);
}

/// Music too far under the dialogue: raise it, never duck it further. Solo is ignored.
#[test]
fn mix_check_says_raise_quiet_music_and_ignores_solo() {
    let dlg = speech(8.0, SR, 0.4, |t| (1.0..3.0).contains(&t) || (4.0..7.0).contains(&t));
    let music = tone(8.0, SR, 220.0, 0.002);
    let (mut s, c, _) = session_with(&[(dlg, 0.0), (music, 0.0)]);
    s.execute("essentialSound.setType", json!({"clips": [c[0].0], "type": "dialogue"})).unwrap();
    s.execute("essentialSound.setType", json!({"clips": [c[1].0], "type": "music"})).unwrap();
    s.execute("timeline.setTrack", json!({"track": "A1", "solo": true})).unwrap();
    let r = s.execute("mix.check", json!({})).unwrap();
    println!("{r:#}");
    let b = &r["musicUnderDialogue"];
    assert_eq!(b["verdict"], "musicTooQuiet", "{b}");
    assert!(b["musicGainChangeDb"].as_f64().unwrap() > 0.0, "raise it: {b}");
    let w: Vec<&str> = r["warnings"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert!(w.iter().any(|x| x.contains("raise it")) && !w.iter().any(|x| x.contains("depthDb\":")), "{w:?}");
    // the soloed dialogue track does not silence the music's measurement
    assert!(r["roles"]["Music"]["loudness"]["integratedLufs"].as_f64().is_some(), "{r}");
    assert!(r["notes"].as_array().unwrap().iter().any(|n| n.as_str().unwrap().contains("soloed")), "{r}");
}

/// A whole film is checked in parts that finish well inside an MCP client's timeout.
#[test]
fn mix_check_asks_for_parts_of_a_long_sequence() {
    let (mut s, _, items) = session_with(&[(speech(2.0, SR, 0.4, |_| true), 0.0)]);
    s.execute("timeline.place", json!({"item": items[0].0, "audioTrack": "A2", "seconds": 700.0})).unwrap();
    let e = err(&mut s, "mix.check", json!({}));
    assert!(e.contains("check at most 600 s per call") && e.contains("\"endSeconds\": 600"), "{e}");
    assert!(s.execute("mix.check", json!({"startSeconds": 0, "endSeconds": 600})).is_ok());
    assert!(s.execute("mix.check", json!({"startSeconds": 600})).is_ok(), "the rest of it");
}

/// `target` is the dialogue target: music given with it gets its own, and the result says so.
#[test]
fn auto_match_notes_when_target_did_not_apply() {
    let voice = speech(4.0, SR, 0.1, |t| (0.5..3.5).contains(&t));
    let music = add(&tone(4.0, SR, 220.0, 0.05), &tone(4.0, SR, 330.0, 0.03));
    let (mut s, c, _) = session_with(&[(voice, 0.0), (music, 0.0)]);
    s.execute("essentialSound.setType", json!({"clips": [c[0].0], "type": "dialogue"})).unwrap();
    s.execute("essentialSound.setType", json!({"clips": [c[1].0], "type": "music"})).unwrap();
    let r = s.execute("essentialSound.autoMatch", json!({"clips": [c[0].0, c[1].0], "target": -16.0})).unwrap();
    let n = r["notes"][0].as_str().unwrap_or_else(|| panic!("{r}"));
    assert!(n.contains(&format!("clip {} (Music)", c[1].0)) && n.contains("`targets`"), "{n}");
    let row = r["clips"].as_array().unwrap().iter().find(|x| x["clip"] == c[1].0).unwrap().clone();
    assert_eq!(row["targetLufs"], -25.0, "the Music preference target: {row}");
}
