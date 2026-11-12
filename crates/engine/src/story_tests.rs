//! `story.*`: cutting rhythm, cut analysis, blink points, the board, split edits, shadow cuts.

use filmagic_project::ClipId;
use filmagic_time::Tick;
use serde_json::{Value, json};

use crate::Session;
use crate::media_test_util::tmp_dir;

/// The demo project as it ships: six clips on V1 and, over the end of the second and the start of
/// the third, a small inset (a picture in a picture, 32% in a corner) on V2.
fn demo_as_shipped() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

/// The demo with its inset laid over the whole frame, as a cutaway: a seventh shot. (Set in the
/// project itself, so that the session has no undo step and is not dirty.)
fn demo() -> Session {
    let mut s = demo_as_shipped();
    let id = s.state.active_sequence.unwrap();
    let q = std::sync::Arc::make_mut(&mut s.project).sequence_mut(id).unwrap();
    let motion = q.video_tracks[1].items[0].effects.iter_mut().find(|e| e.effect == "motion").unwrap();
    motion.params.get_mut("scale").unwrap().value = filmagic_project::ParamValue::Float(100.0);
    motion.params.get_mut("position").unwrap().value = filmagic_project::ParamValue::Vec2(filmagic_geom::Vec2::new(960.0, 540.0));
    s
}

#[test]
fn what_shows_part_of_a_picture_rides_on_the_shot_under_it() {
    // the inset as shipped is no shot: under it V1 cuts from the second clip to the third, and that is the cut
    let mut s = demo_as_shipped();
    let q = s.active_sequence().unwrap().clone();
    let v1: Vec<ClipId> = q.video_tracks[0].items.iter().map(|i| i.id).collect();
    let seen: Vec<Option<ClipId>> = crate::story::shots(&s.project, &q).unwrap().iter().map(|sh| sh.clip).collect();
    assert_eq!(seen, v1.iter().copied().map(Some).collect::<Vec<_>>());
    let r = s.execute("story.rhythm", json!({})).unwrap();
    assert_eq!((r["shots"].as_u64(), r["cuts"].as_u64()), (Some(6), Some(5)));
    // a picture in a picture across the whole film (a face camera) hides no cut either
    let (item, end) = (q.video_tracks[0].items[3].item.0, q.duration());
    let pip = s.execute("timeline.place", json!({"item": item, "track": "V3", "seconds": 0.0, "audio": false, "duration": end.0})).unwrap()["clips"][0]
        .as_u64()
        .unwrap();
    s.execute("effects.setParam", json!({"clip": pip, "effect": "motion", "param": "scale", "value": 30.0})).unwrap();
    s.execute("effects.setParam", json!({"clip": pip, "effect": "motion", "param": "position", "value": [1500.0, 200.0]})).unwrap();
    let r = s.execute("story.rhythm", json!({})).unwrap();
    assert_eq!((r["shots"].as_u64(), r["cuts"].as_u64()), (Some(6), Some(5)), "{r}");
    assert_eq!(s.execute("story.cuts", json!({})).unwrap()["cutsInSequence"], 5);
    // Laid over most of the frame it is the picture: one shot, as a letterboxed film would be.
    s.execute("effects.setParam", json!({"clip": pip, "effect": "motion", "param": "scale", "value": 85.0})).unwrap();
    assert_eq!(s.execute("story.rhythm", json!({})).unwrap()["shots"], 1);
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("clip.enable", json!({"clips": [pip]})).unwrap();
    // a logo with an alpha channel, full frame, across the film: still six shots
    let dir = tmp_dir("story-logo");
    let path = dir.join("logo.png");
    let mut px = vec![0u8; 64 * 36 * 4];
    px[..64 * 4 * 6].fill(255);
    std::fs::write(&path, filmagic_export::encode_png(px, 64, 36).unwrap()).unwrap();
    let logo = s.execute("file.import", json!({"paths": [path.to_string_lossy()]})).unwrap()["items"][0].as_u64().unwrap();
    let placed = s.execute("timeline.place", json!({"item": logo, "track": "new", "seconds": 0.0, "duration": end.0})).unwrap()["clips"][0].as_u64().unwrap();
    s.execute("effects.setParam", json!({"clip": placed, "effect": "motion", "param": "scale", "value": 3000.0})).unwrap();
    let r = s.execute("story.rhythm", json!({})).unwrap();
    assert_eq!((r["shots"].as_u64(), r["cuts"].as_u64()), (Some(6), Some(5)), "{r}");
    let _ = std::fs::remove_dir_all(dir);
}

fn secs(v: &Value) -> f64 {
    v["seconds"].as_f64().unwrap()
}

/// Every hostile call is refused, without a panic, a project change or an undo step.
fn refuses(s: &mut Session, id: &str, cases: &[Value]) {
    let snap = (*s.project).clone();
    let undo = s.history.undo.len();
    for p in cases {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.execute(id, p.clone())));
        assert!(r.is_ok(), "{id} panicked on {p}");
        assert!(r.unwrap().is_err(), "{id} accepted {p}");
        assert_eq!(*s.project, snap, "{id} changed the project on {p}");
        assert_eq!(s.history.undo.len(), undo, "{id} left an undo step on {p}");
    }
}

#[test]
fn shots_follow_the_topmost_footage() {
    let s = demo();
    let q = s.active_sequence().unwrap();
    let shots = crate::story::shots(&s.project, q).unwrap();
    // V1 holds six clips; the V2 clip covers the end of the second and the start of the third
    let v1: Vec<ClipId> = q.video_tracks[0].items.iter().map(|i| i.id).collect();
    let over = q.video_tracks[1].items[0].id;
    let seen: Vec<Option<ClipId>> = shots.iter().map(|sh| sh.clip).collect();
    assert_eq!(seen, [v1[0], v1[1], over, v1[2], v1[3], v1[4], v1[5]].map(Some));
    for w in shots.windows(2) {
        assert_eq!(w[0].end, w[1].start, "shots abut");
    }
    assert_eq!(shots[0].start, Tick::ZERO);
    assert_eq!(shots.last().unwrap().end, q.duration());
    // the overlay starts where its clip starts, not where V1 cuts
    assert_eq!(shots[2].start, q.video_tracks[1].items[0].start);
}

#[test]
fn a_hidden_track_a_disabled_clip_and_a_title_start_no_shot() {
    let mut s = demo();
    let before = crate::story::shots(&s.project, s.active_sequence().unwrap()).unwrap().len();
    let over = s.active_sequence().unwrap().video_tracks[1].items[0].id;
    // a title over the first shot rides on it
    s.execute("playhead.set", json!({"seconds": 1.0})).unwrap();
    s.execute("graphics.newText", json!({"text": "Title"})).unwrap();
    assert_eq!(crate::story::shots(&s.project, s.active_sequence().unwrap()).unwrap().len(), before, "a title is not a shot");
    // the V2 overlay disabled: V1 shows through, one shot fewer
    s.execute("clip.enable", json!({"clips": [over.0]})).unwrap();
    assert!(!s.active_sequence().unwrap().find_item(over).unwrap().1.enabled);
    assert_eq!(crate::story::shots(&s.project, s.active_sequence().unwrap()).unwrap().len(), before - 1);
}

#[test]
fn black_between_clips_is_a_shot_but_not_at_the_ends() {
    let mut s = demo();
    let q = s.active_sequence().unwrap();
    let (third, over) = (q.video_tracks[0].items[3].id, q.video_tracks[1].items[0].id);
    s.execute("timeline.select", json!({"clips": [third.0]})).unwrap();
    s.execute("edit.clear", json!({})).unwrap();
    let shots = crate::story::shots(&s.project, s.active_sequence().unwrap()).unwrap();
    assert_eq!(shots.iter().filter(|sh| sh.clip.is_none()).count(), 1, "the lifted clip leaves black");
    assert!(shots.first().unwrap().clip.is_some() && shots.last().unwrap().clip.is_some());
    let _ = over;
    let r = s.execute("story.rhythm", json!({})).unwrap();
    assert!(r["notes"].as_array().unwrap().iter().any(|n| n.as_str().unwrap().contains("black")), "{r}");
}

#[test]
fn rhythm_reads_the_pace_of_the_cut() {
    let mut s = demo();
    let r = s.execute("story.rhythm", json!({})).unwrap();
    assert_eq!((r["shots"].as_u64(), r["cuts"].as_u64()), (Some(7), Some(6)));
    let total = r["seconds"].as_f64().unwrap();
    assert!((total - s.active_sequence().unwrap().duration().seconds()).abs() < 0.01);
    let cpm = r["cutsPerMinute"].as_f64().unwrap();
    assert!((cpm - 6.0 * 60.0 / total).abs() < 0.01, "{cpm}");
    assert_eq!(r["pace"], "brisk");
    assert_eq!(r["shotList"].as_array().unwrap().len(), 7);
    assert_eq!(r["shotList"][0]["name"], "Ocean_Sunset.mp4");
    assert_eq!(r["blinkRange"]["dialogue"], 6);
    // windows cover the film and hold every cut
    let cuts: u64 = r["windows"].as_array().unwrap().iter().map(|w| w["cuts"].as_u64().unwrap()).sum();
    assert_eq!(cuts, 6);
    assert_eq!(r["windows"].as_array().unwrap().len(), (total / 10.0).ceil() as usize);
    assert!(r["flashes"].as_array().unwrap().is_empty());
    // a window too small for the film is widened to keep at most 600 of them, and reported
    let mut long = Session::default();
    long.execute("file.newColorMatte", json!({"color": "#336699", "name": "matte"})).unwrap();
    let item = long.project.items.keys().next().unwrap().0;
    long.execute("file.newSequence", json!({"name": "long", "fps": 24.0})).unwrap();
    long.execute("timeline.place", json!({"item": item, "track": "V1", "seconds": 0.0, "duration": {"seconds": 1800.0}})).unwrap();
    let w = long.execute("story.rhythm", json!({"window": 1})).unwrap();
    assert_eq!((w["windows"].as_array().unwrap().len(), w["windowSeconds"].as_f64()), (600, Some(3.0)), "{}", w["windowSeconds"]);
    // a read-only query: no undo step, no journal entry
    assert!(s.history.undo.is_empty());
    assert!(s.journal.iter().all(|(id, _)| id != "story.rhythm"));
}

#[test]
fn rhythm_flags_flashes_and_a_metronome() {
    let mut s = demo();
    // a five-frame sliver at the head of the last clip
    let q = s.active_sequence().unwrap();
    let rate = q.frame_rate();
    let last = q.video_tracks[0].items[5].start;
    s.execute("sequence.addEdit", json!({"time": (last + rate.tick_of(5)).0})).unwrap();
    let r = s.execute("story.rhythm", json!({})).unwrap();
    let flashes = r["flashes"].as_array().unwrap();
    assert_eq!(flashes.len(), 1, "{r}");
    assert_eq!(flashes[0]["frames"], 5);
    assert!(r["notes"].as_array().unwrap().iter().any(|n| n.as_str().unwrap().contains("flashes")));
    // `flashFrames` moves the bar
    assert!(s.execute("story.rhythm", json!({"flashFrames": 3})).unwrap()["flashes"].as_array().unwrap().is_empty());

    // eight equal shots: a metronome
    let mut m = Session::default();
    m.execute("file.newColorMatte", json!({"color": "#336699", "name": "matte"})).unwrap();
    let item = m.project.items.keys().next().unwrap().0;
    m.execute("file.newSequence", json!({"name": "m", "fps": 24.0})).unwrap();
    for i in 0..8 {
        m.execute("timeline.place", json!({"item": item, "track": "V1", "seconds": f64::from(i), "duration": {"seconds": 1.0}})).unwrap();
    }
    let r = m.execute("story.rhythm", json!({})).unwrap();
    assert_eq!(r["shots"], 8, "{r}");
    assert_eq!(r["pace"], "frantic", "eight one-second shots are 52 cuts a minute");
    assert!(r["notes"].as_array().unwrap().iter().any(|n| n.as_str().unwrap().contains("metronome")), "{r}");
}

#[test]
fn rhythm_and_cuts_refuse_hostile_parameters() {
    let mut s = demo();
    refuses(
        &mut s,
        "story.rhythm",
        &[
            json!({"window": 0}),
            json!({"window": -5}),
            json!({"window": 1e308}),
            json!({"window": "x"}),
            json!({"flashFrames": 0}),
            json!({"flashFrames": 1e9}),
            json!({"flashFrames": -1}),
        ],
    );
    refuses(
        &mut s,
        "story.cuts",
        &[
            json!({"limit": 0}),
            json!({"limit": 100_000}),
            json!({"limit": "all"}),
            json!({"emotion": 2}),
            json!({"seconds": 5, "story": -0.1}),
            json!({"seconds": 5, "emotion": "high"}),
            json!({"emotion": 0.5}),
            json!({"fromSeconds": -1}),
            json!({"toSeconds": "end"}),
            json!({"fromSeconds": 1e300}),
            // a time that cannot be read is refused, not taken as "every cut"
            json!({"seconds": "five"}),
            json!({"frame": 2.5}),
            json!({"time": "00:00:05:00"}),
            json!({"timecode": 12}),
        ],
    );
    refuses(&mut s, "story.splitEdit", &[json!({"kind": "L", "seconds": "five"}), json!({"kind": "L", "frame": []})]);
    let mut empty = Session::default();
    for id in ["story.rhythm", "story.cuts", "story.splitEdit", "story.shadowCut", "story.compare"] {
        assert!(empty.execute(id, json!({})).is_err(), "{id} without a sequence");
    }
}

#[test]
fn an_empty_sequence_has_no_rhythm_and_no_cuts() {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name": "empty"})).unwrap();
    let r = s.execute("story.rhythm", json!({})).unwrap();
    assert_eq!((r["shots"].as_u64(), r["cuts"].as_u64()), (Some(0), Some(0)));
    let c = s.execute("story.cuts", json!({})).unwrap();
    assert_eq!(c["analysed"], 0);
    assert!(s.execute("story.splitEdit", json!({"kind": "L"})).is_err());
}

#[test]
fn every_cut_is_analysed_with_its_evidence() {
    let mut s = demo();
    let r = s.execute("story.cuts", json!({})).unwrap();
    assert_eq!((r["cutsInSequence"].as_u64(), r["analysed"].as_u64()), (Some(6), Some(6)));
    assert_eq!(r["truncated"], false);
    let cuts = r["cuts"].as_array().unwrap();
    let shots = crate::story::shots(&s.project, s.active_sequence().unwrap()).unwrap();
    for (c, sh) in cuts.iter().zip(shots.iter().skip(1)) {
        assert_eq!(c["at"]["time"].as_i64(), Some(sh.start.0));
        assert_eq!(c["to"]["clip"].as_u64(), sh.clip.map(|x| x.0));
        let d = c["change"]["difference"].as_f64().unwrap();
        assert!((0.0..=1.0).contains(&d), "{c}");
        assert!(["match", "jump", "clean"].contains(&c["change"]["kind"].as_str().unwrap()));
        for side in ["out", "in"] {
            for v in c["eye"][side].as_array().unwrap() {
                assert!((0.0..=1.0).contains(&v.as_f64().unwrap()), "{c}");
            }
        }
        // emotion, story and space are never measured; the score says how much it covers
        assert!(c["six"]["emotion"]["value"].is_null() && c["six"]["story"]["value"].is_null() && c["six"]["space"]["value"].is_null());
        assert_eq!(c["six"]["emotion"]["weight"], 51.0);
        let covers = c["six"]["covers"].as_f64().unwrap();
        assert!((10.0..=22.0).contains(&covers), "{c}");
        let score = c["six"]["score"].as_f64().unwrap();
        assert!((0.0..=1.0).contains(&score));
        assert!(c["six"]["note"].as_str().unwrap().contains("Emotion"));
    }
    // the demo's shots are different pictures
    assert!(cuts.iter().filter(|c| c["change"]["kind"] == "clean").count() >= 4, "{r}");
    // V1's cuts change picture and sound together; the overlay has no linked sound
    assert_eq!(cuts[0]["sound"]["edit"], "straight");
    assert!(cuts[1]["sound"]["edit"].is_null() || cuts[1]["sound"]["edit"] == "straight" || cuts[1]["sound"]["edit"] == "L", "{}", cuts[1]);
    // the first cut sits in a cross dissolve
    assert_eq!(cuts[0]["transition"], "cross_dissolve");
    assert_eq!(r["weights"].as_array().unwrap().iter().map(|w| w["weight"].as_f64().unwrap()).sum::<f64>(), 100.0);
    assert!(s.history.undo.is_empty(), "analysis edits nothing");
}

#[test]
fn one_cut_can_be_named_judged_and_ranged() {
    let mut s = demo();
    let third = s.active_sequence().unwrap().video_tracks[0].items[3].start;
    // the cut nearest a time, completed with the caller's judgement
    let r = s.execute("story.cuts", json!({"seconds": third.seconds() + 0.3, "emotion": 1.0, "story": 0.5})).unwrap();
    assert_eq!(r["analysed"], 1);
    let c = &r["cuts"][0];
    assert_eq!(c["at"]["time"].as_i64(), Some(third.0));
    assert_eq!(c["six"]["emotion"]["value"], 1.0);
    assert_eq!(c["six"]["story"]["value"], 0.5);
    let covers = c["six"]["covers"].as_f64().unwrap();
    assert!(covers >= 84.0, "emotion and story are 74 of the 100: {c}");
    // emotion dominates: the same cut judged 0 scores far lower
    let low = s.execute("story.cuts", json!({"seconds": third.seconds(), "emotion": 0.0, "story": 0.5})).unwrap();
    let (hi, lo) = (c["six"]["score"].as_f64().unwrap(), low["cuts"][0]["six"]["score"].as_f64().unwrap());
    assert!(hi - lo > 0.45, "{hi} vs {lo}");
    // a range, a limit and where to continue
    let r = s.execute("story.cuts", json!({"fromSeconds": 9.0, "toSeconds": 20.0})).unwrap();
    assert_eq!(r["analysed"], 3, "{r}");
    let r = s.execute("story.cuts", json!({"limit": 2})).unwrap();
    assert_eq!((r["analysed"].as_u64(), r["truncated"].as_bool()), (Some(2), Some(true)));
    let next = r["nextFromSeconds"].as_f64().unwrap();
    let rest = s.execute("story.cuts", json!({"fromSeconds": next})).unwrap();
    assert_eq!(rest["analysed"], 4, "the rest follow from nextFromSeconds: {rest}");
}

#[test]
fn a_cut_inside_one_setup_is_a_match_and_is_called_out() {
    let mut s = demo();
    // a through edit in the middle of the first clip: the same picture on both sides
    s.execute("sequence.addEdit", json!({"seconds": 2.5})).unwrap();
    let r = s.execute("story.cuts", json!({"seconds": 2.5})).unwrap();
    let c = &r["cuts"][0];
    assert_eq!(c["change"]["kind"], "match", "{c}");
    assert!(c["issues"].as_array().unwrap().iter().any(|i| i.as_str().unwrap().contains("barely changes")), "{c}");
    assert_eq!(c["direction"]["reverses"], false);
    // the sound was cut there too, but it is one recording running on: not a straight cut
    assert_eq!(c["sound"]["edit"], "continuous", "{c}");
    assert_eq!(r["summary"]["continuousSound"], 1);
    // and a split edit of it would change nothing: refused, with the reason
    let e = s.execute("story.splitEdit", json!({"kind": "L", "seconds": 2.5})).unwrap_err().to_string();
    assert!(e.contains("one continuous recording"), "{e}");
    let e = s.execute("story.splitEdit", json!({"kind": "J", "seconds": 2.5, "roll": "video"})).unwrap_err().to_string();
    assert!(e.contains("one continuous recording"), "{e}");
}

#[test]
fn a_cutaway_over_running_sound_reads_as_a_split_edit() {
    let mut s = demo();
    // the third V1 clip's picture, copied over the middle of the first clip: sound stays with the first
    let q = s.active_sequence().unwrap();
    let (first, third) = (q.video_tracks[0].items[0].clone(), q.video_tracks[0].items[2].clone());
    let at = s.sequence_rate().tick_of(48);
    s.execute("timeline.razor", json!({"clip": third.id.0, "time": (third.start + s.sequence_rate().tick_of(24)).0})).unwrap();
    s.execute("timeline.move", json!({"moves": [{"clip": third.id.0, "track": "V3", "time": at.0}], "copy": true, "linked": false})).unwrap();
    let cuts = s.execute("story.cuts", json!({"fromSeconds": 1.0, "toSeconds": 4.0})).unwrap();
    let cuts = cuts["cuts"].as_array().unwrap().clone();
    assert_eq!(cuts.len(), 2, "into the cutaway and back: {cuts:?}");
    assert_eq!(cuts[0]["from"]["clip"].as_u64(), Some(first.id.0));
    assert_eq!((cuts[0]["sound"]["edit"].as_str(), cuts[1]["sound"]["edit"].as_str()), (Some("L"), Some("J")), "{cuts:?}");
    // the same with the sound under it cut in two where the cutaway begins: still one recording
    s.execute("timeline.razor", json!({"clip": first.id.0, "time": at.0})).unwrap();
    let again = s.execute("story.cuts", json!({"fromSeconds": 1.0, "toSeconds": 4.0})).unwrap();
    assert_eq!(again["cuts"][0]["sound"]["edit"], "L", "{again}");
    assert_eq!(again["cuts"][1]["sound"]["edit"], "J", "{again}");
}

#[test]
fn the_clips_of_a_sequence_are_listed_in_brief() {
    let mut s = demo();
    let r = s.execute("sequence.clips", json!({})).unwrap();
    let tracks = r["tracks"].as_array().unwrap();
    // video from the top down, then audio
    let names: Vec<&str> = tracks.iter().map(|t| t["track"].as_str().unwrap()).collect();
    assert_eq!(names, ["Video 3", "Video 2", "Video 1", "Audio 1", "Audio 2", "Audio 3"]);
    let v1 = tracks[2]["clips"].as_array().unwrap();
    let q = s.active_sequence().unwrap().clone();
    assert_eq!(v1.len(), 6);
    for (c, it) in v1.iter().zip(&q.video_tracks[0].items) {
        assert_eq!((c["clip"].as_u64(), c["item"].as_u64(), c["link"].as_u64()), (Some(it.id.0), Some(it.item.0), it.link));
        assert!((c["start"].as_f64().unwrap() - it.start.seconds()).abs() < 1e-3 && (c["sourceIn"].as_f64().unwrap() - it.source_in.seconds()).abs() < 1e-3);
        assert!(c.get("speed").is_none() && c.get("disabled").is_none() && c.get("reverse").is_none(), "only what is unusual is listed: {c}");
        // the fixed effects every clip has are not news; one that was added is
        let added: Vec<&str> = it.effects.iter().filter(|e| e.def().is_none_or(|d| !d.intrinsic)).map(|e| e.effect.as_str()).collect();
        let listed: Vec<&str> = c.get("effects").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
        assert_eq!(listed, added, "{c}");
    }
    assert_eq!(tracks[2]["transitions"].as_array().unwrap().len(), q.video_tracks[0].transitions.len());
    assert!(tracks[0].get("transitions").is_none());
    // far shorter than the full inspection
    let full = s.execute("sequence.inspect", json!({})).unwrap().to_string().len();
    assert!(r.to_string().len() * 4 < full, "{} vs {full}", r.to_string().len());
    // what is unusual about a clip is said
    let c = q.video_tracks[0].items[1].id.0;
    s.execute("clip.enable", json!({"clips": [c]})).unwrap();
    s.execute("clip.speedDuration", json!({"clips": [c], "speed": 50})).unwrap();
    s.execute("effects.apply", json!({"clips": [c], "effect": "Gaussian Blur"})).unwrap();
    let r = s.execute("sequence.clips", json!({"track": "V1", "fromSeconds": 5.0, "toSeconds": 6.0})).unwrap();
    assert_eq!(r["tracks"].as_array().unwrap().len(), 1);
    let listed = r["tracks"][0]["clips"].as_array().unwrap();
    let it = listed.iter().find(|x| x["clip"].as_u64() == Some(c)).unwrap();
    assert_eq!((it["speed"].as_f64(), it["disabled"].as_bool(), it["effects"].clone()), (Some(0.5), Some(true), json!(["gaussian_blur"])), "{it}");
    assert!(listed.len() <= 2, "only what lies in the span: {listed:?}");
    assert!(s.journal.iter().all(|(id, _)| id != "sequence.clips"));
    refuses(&mut s, "sequence.clips", &[json!({"track": "V9"}), json!({"track": 5.5}), json!({"fromSeconds": "x"}), json!({"toSeconds": -1})]);
}

/// A WAV of 220 Hz phrases (sound seconds, silence seconds) over a faint hiss.
fn phrases_wav(dir: &std::path::Path, phrases: &[(f64, f64)]) -> String {
    let rate = 48_000u32;
    let mut samples = Vec::new();
    let mut seed = 7u32;
    let mut hiss = move || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        ((seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * 0.004
    };
    for &(on, off) in phrases {
        for i in 0..(on * f64::from(rate)) as usize {
            samples.push(0.4 * (i as f32 * 220.0 * std::f32::consts::TAU / rate as f32).sin() + hiss());
        }
        for _ in 0..(off * f64::from(rate)) as usize {
            samples.push(hiss());
        }
    }
    let path = dir.join("lines.wav");
    std::fs::write(&path, filmagic_media::wav::write_wav16(&samples, 1, rate)).unwrap();
    path.to_string_lossy().into_owned()
}

/// A session with a spoken-line WAV on A1 at `at` seconds. Returns (session, clip, item).
fn spoken(at: f64) -> (Session, u64, u64, std::path::PathBuf) {
    let dir = tmp_dir("story-blink");
    let wav = phrases_wav(&dir, &[(1.0, 0.5), (0.8, 0.4), (1.2, 0.6), (0.9, 0.0)]);
    let mut s = Session::default();
    let item = s.execute("file.import", json!({"paths": [wav]})).unwrap()["items"][0].as_u64().unwrap();
    s.execute("file.newSequence", json!({"name": "lines", "fps": 24.0})).unwrap();
    let placed = s.execute("timeline.place", json!({"item": item, "track": "A1", "seconds": at})).unwrap();
    let clip = placed["clips"][0].as_u64().unwrap_or_else(|| panic!("{placed}"));
    (s, clip, item, dir)
}

#[test]
fn blink_points_fall_where_phrases_end() {
    let (mut s, clip, item, dir) = spoken(2.0);
    let r = s.execute("story.blinkPoints", json!({"clip": clip})).unwrap();
    assert_eq!(r["timeBase"], "sequence");
    assert_eq!(r["analysed"], json!({"sound": true, "picture": false}));
    let pts = r["points"].as_array().unwrap();
    assert_eq!(pts.len(), 3, "{r}");
    // phrase ends at 1.0, 2.3 and 3.9 s of the clip, which starts at 2 s
    for (pt, end) in pts.iter().zip([3.0, 4.3, 5.9]) {
        assert_eq!(pt["kind"], "phrase");
        assert!((secs(&pt["pause"]["start"]) - end).abs() < 0.05, "{pt}");
        let at = secs(&pt["at"]);
        assert!(at >= end - 0.05 && at <= end + 0.15, "the point sits just after the phrase: {pt}");
        assert!(pt["strength"].as_f64().unwrap() > 0.3);
        // on the frame grid, with the media time it shows
        assert_eq!(pt["at"]["time"].as_i64().unwrap() % s.sequence_rate().frame_duration().0, 0);
        assert_eq!(pt["mediaTime"].as_i64().unwrap(), pt["at"]["time"].as_i64().unwrap() - Tick::from_seconds_f64(2.0).0);
    }
    assert!(s.history.undo.len() == 3, "import, sequence, place: finding points adds no step");
    // the same rushes read in media time
    let r = s.execute("story.blinkPoints", json!({"item": item})).unwrap();
    assert_eq!(r["timeBase"], "media");
    let media: Vec<f64> = r["points"].as_array().unwrap().iter().map(|pt| secs(&pt["pause"]["start"])).collect();
    assert_eq!(media.len(), 3);
    for (m, end) in media.iter().zip([1.0, 2.3, 3.9]) {
        assert!((m - end).abs() < 0.05, "{media:?}");
    }
    // a longer minimum pause keeps only the long ones; a span keeps only what is inside it
    assert_eq!(s.execute("story.blinkPoints", json!({"clip": clip, "minPause": 0.45})).unwrap()["points"].as_array().unwrap().len(), 2);
    assert_eq!(s.execute("story.blinkPoints", json!({"clip": clip, "fromSeconds": 4.0, "toSeconds": 5.0})).unwrap()["points"].as_array().unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn blink_points_become_markers_in_one_undo_step() {
    let (mut s, clip, _, dir) = spoken(0.0);
    let undo = s.history.undo.len();
    let r = s.execute("story.blinkPoints", json!({"clip": clip, "markers": true})).unwrap();
    assert_eq!(r["markers"].as_array().unwrap().len(), 3);
    let q = s.active_sequence().unwrap();
    assert_eq!(q.markers.len(), 3);
    for (m, pt) in q.markers.iter().zip(r["points"].as_array().unwrap()) {
        assert_eq!(m.start.0, pt["at"]["time"].as_i64().unwrap());
        assert_eq!(m.name, "Blink point");
        assert!(m.comment.contains("a phrase ends"));
    }
    assert_eq!(s.history.undo.len(), undo + 1);
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.active_sequence().unwrap().markers.is_empty());
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(s.active_sequence().unwrap().markers.len(), 3);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn blink_points_read_a_picture_clip_through_its_linked_sound() {
    let mut s = demo();
    let v = s.active_sequence().unwrap().video_tracks[0].items[0].clone();
    let r = s.execute("story.blinkPoints", json!({"clip": v.id.0})).unwrap();
    assert_eq!(r["analysed"], json!({"sound": true, "picture": true}), "{r}");
    assert_eq!(r["span"]["from"]["time"].as_i64(), Some(v.start.0));
    for pt in r["points"].as_array().unwrap() {
        let t = pt["at"]["time"].as_i64().unwrap();
        assert!(t > v.start.0 && t < v.end().0, "{pt}");
    }
    // the picture alone can be left out
    let r = s.execute("story.blinkPoints", json!({"clip": v.id.0, "picture": false})).unwrap();
    assert_eq!(r["analysed"]["picture"], false);
    // when nothing is found, the result says what was looked for and not found
    let mut still = Session::default();
    still.execute("file.newColorMatte", json!({"color": "#336699", "name": "matte"})).unwrap();
    let item = still.project.items.keys().next().unwrap().0;
    still.execute("file.newSequence", json!({"name": "still", "fps": 24.0})).unwrap();
    let c = still.execute("timeline.place", json!({"item": item, "track": "V1", "seconds": 0.0})).unwrap()["clips"][0].as_u64().unwrap();
    let r = still.execute("story.blinkPoints", json!({"clip": c})).unwrap();
    assert!(r["points"].as_array().unwrap().is_empty());
    let notes = r["notes"].as_array().unwrap();
    assert!(notes.iter().any(|n| n.as_str().unwrap().contains("holds still")), "{r}");
    assert!(!notes.iter().any(|n| n.as_str().unwrap().contains("sound")), "there was no sound to speak of: {r}");
}

#[test]
fn blink_points_refuse_hostile_parameters() {
    let (mut s, clip, item, dir) = spoken(0.0);
    refuses(
        &mut s,
        "story.blinkPoints",
        &[
            json!({}),
            json!({"clip": clip, "item": item}),
            json!({"clip": 999_999}),
            json!({"clip": "first"}),
            json!({"item": 999_999}),
            json!({"item": item, "markers": true}),
            json!({"clip": clip, "minPause": 0}),
            json!({"clip": clip, "minPause": 1e300}),
            json!({"clip": clip, "minPause": -1}),
            json!({"clip": clip, "fromSeconds": 3.0, "toSeconds": 1.0}),
            json!({"clip": clip, "fromSeconds": 1e9}),
            json!({"clip": clip, "fromSeconds": -2}),
            json!({"clip": clip, "toSeconds": "end"}),
            json!({"clip": clip, "minPause": "short"}),
            json!({"clip": clip, "picture": "yes"}),
            json!({"clip": clip, "markers": 1}),
        ],
    );
    // a sequence is not rushes to analyse
    let seq = s.state.active_sequence.unwrap().0;
    assert!(s.execute("story.blinkPoints", json!({"item": seq})).is_err());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_long_stretch_must_be_analysed_in_parts() {
    let mut s = Session::default();
    s.execute("file.newColorMatte", json!({"color": "#336699", "name": "matte"})).unwrap();
    let item = s.project.items.keys().next().unwrap().0;
    s.execute("file.newSequence", json!({"name": "long", "fps": 24.0})).unwrap();
    let placed = s.execute("timeline.place", json!({"item": item, "track": "V1", "seconds": 0.0, "duration": {"seconds": 1200.0}})).unwrap();
    let clip = placed["clips"][0].as_u64().unwrap();
    let e = s.execute("story.blinkPoints", json!({"clip": clip})).unwrap_err().to_string();
    assert!(e.contains("too long") && e.contains("fromSeconds"), "{e}");
    assert!(s.execute("story.blinkPoints", json!({"clip": clip, "fromSeconds": 100.0, "toSeconds": 110.0})).is_ok());
}

fn png_size(bytes: &[u8]) -> (u32, u32) {
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    (u32::from_be_bytes(bytes[16..20].try_into().unwrap()), u32::from_be_bytes(bytes[20..24].try_into().unwrap()))
}

#[test]
fn the_board_shows_one_frame_per_shot() {
    let mut s = demo();
    let dir = tmp_dir("story-board");
    let path = dir.join("board.png").to_string_lossy().into_owned();
    let r = s.execute("story.board", json!({"path": path, "width": 160})).unwrap();
    assert_eq!(r["of"], "sequence");
    let cells = r["cells"].as_array().unwrap();
    assert_eq!(cells.len(), 7);
    assert_eq!(r["columns"], 4);
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(png_size(&bytes), (r["width"].as_u64().unwrap() as u32, r["height"].as_u64().unwrap() as u32));
    // four 160×90 cells and five 6 px gaps across; two rows down
    assert_eq!(png_size(&bytes), (4 * 160 + 5 * 6, 2 * 90 + 3 * 6));
    let shots = crate::story::shots(&s.project, s.active_sequence().unwrap()).unwrap();
    for (c, sh) in cells.iter().zip(&shots) {
        // the representative frame lies inside its shot
        let f = c["frame"]["time"].as_i64().unwrap();
        assert!(f >= sh.start.0 && f < sh.end.0, "{c}");
        assert_eq!(c["clip"].as_u64(), sh.clip.map(|x| x.0));
    }
    // a span, a cap and a different layout
    let r = s.execute("story.board", json!({"path": path, "width": 96, "columns": 2, "fromSeconds": 14.0})).unwrap();
    assert_eq!(r["cells"].as_array().unwrap().len(), 3, "{r}");
    assert_eq!(r["cells"][0]["shot"], 5);
    let r = s.execute("story.board", json!({"path": path, "width": 96, "max": 2})).unwrap();
    assert_eq!((r["cells"].as_array().unwrap().len(), r["truncated"].as_bool()), (2, Some(true)));
    assert!(s.history.undo.is_empty() && !s.is_dirty(), "a board changes nothing");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_board_lays_out_a_clips_rushes() {
    let mut s = demo();
    let dir = tmp_dir("story-rushes");
    let path = dir.join("rushes.png").to_string_lossy().into_owned();
    let item = s.active_sequence().unwrap().video_tracks[0].items[0].item;
    let r = s.execute("story.board", json!({"path": path, "item": item.0, "count": 6, "columns": 3, "width": 128, "labels": false})).unwrap();
    assert_eq!(r["of"], "item");
    let cells = r["cells"].as_array().unwrap();
    assert_eq!(cells.len(), 6);
    let times: Vec<f64> = cells.iter().map(|c| secs(&c["at"])).collect();
    assert!(times.windows(2).all(|w| w[1] > w[0]), "in order: {times:?}");
    assert_eq!(png_size(&std::fs::read(&path).unwrap()).0, 3 * 128 + 4 * 6, "cells are the asked width whatever size the source decodes at");
    // works with no sequence open
    let mut bare = Session::default();
    bare.execute("file.newColorMatte", json!({"color": "#336699", "name": "matte"})).unwrap();
    let matte = bare.project.items.keys().next().unwrap().0;
    assert!(bare.execute("story.board", json!({"path": path, "item": matte, "count": 2})).is_ok());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_board_refuses_hostile_parameters_and_writes_nothing() {
    let mut s = demo();
    let dir = tmp_dir("story-board-hostile");
    let path = dir.join("b.png").to_string_lossy().into_owned();
    refuses(
        &mut s,
        "story.board",
        &[
            json!({}),
            json!({"path": ""}),
            json!({"path": 5}),
            json!({"path": path, "columns": 0}),
            json!({"path": path, "columns": 1000}),
            json!({"path": path, "width": 1}),
            json!({"path": path, "width": 1e9}),
            json!({"path": path, "max": 0}),
            json!({"path": path, "max": 100_000}),
            json!({"path": path, "item": 999_999}),
            json!({"path": path, "item": 5, "count": 0}),
            json!({"path": path, "item": 5, "count": 5000}),
            json!({"path": path, "item": 5, "fromSeconds": 5.0, "toSeconds": 1.0}),
            json!({"path": path, "fromSeconds": "x"}),
            json!({"path": path, "labels": "no"}),
            // 96 frames 1280 px wide are more than one sheet holds: refused before anything is rendered
            json!({"path": path, "item": 5, "count": 96, "width": 1280}),
            json!({"path": dir.join("no/such/folder/b.png").to_string_lossy()}),
        ],
    );
    assert!(!std::path::Path::new(&path).exists(), "a refused board leaves no file");
    // an audio-only item has no picture
    let score = s.active_sequence().unwrap().audio_tracks[1].items[0].item;
    assert!(s.execute("story.board", json!({"path": path, "item": score.0})).is_err());
    let _ = std::fs::remove_dir_all(dir);
}

/// (end of the outgoing sound, start of the incoming sound, picture cut) around the cut into V1's fourth clip.
fn fourth_cut(s: &Session) -> (Tick, Tick, Tick) {
    let q = s.active_sequence().unwrap();
    (q.audio_tracks[0].items[2].end(), q.audio_tracks[0].items[3].start, q.video_tracks[0].items[3].start)
}

#[test]
fn a_split_edit_lets_sound_trail_or_lead_the_picture() {
    let mut s = demo();
    let rate = s.sequence_rate();
    let (_, _, cut) = fourth_cut(&s);
    let undo = s.history.undo.len();
    // L: the outgoing sound runs on ten frames under the new picture
    let r = s.execute("story.splitEdit", json!({"kind": "L", "frames": 10, "seconds": cut.seconds() + 0.2})).unwrap();
    assert_eq!((r["kind"].as_str(), r["rolled"].as_str(), r["frames"].as_i64()), (Some("L"), Some("audio"), Some(10)));
    assert_eq!(r["cut"]["time"].as_i64(), Some(cut.0));
    let (a_end, b_start, v_cut) = fourth_cut(&s);
    assert_eq!((a_end, b_start, v_cut), (cut + rate.tick_of(10), cut + rate.tick_of(10), cut), "sound moved, picture did not");
    assert_eq!(s.history.undo.len(), undo + 1, "one undo step");
    assert_eq!(s.history.undo.last().unwrap().0, "Split Edit");
    // the analysis now reads it as an L-cut
    let c = s.execute("story.cuts", json!({"time": cut.0})).unwrap();
    assert_eq!(c["cuts"][0]["sound"]["edit"], "L", "{c}");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(fourth_cut(&s), (cut, cut, cut));
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(fourth_cut(&s).0, cut + rate.tick_of(10));
    s.execute("edit.undo", json!({})).unwrap();

    // J: the incoming sound arrives six frames early
    let r = s.execute("story.splitEdit", json!({"kind": "j", "frames": 6, "time": cut.0})).unwrap();
    assert_eq!(r["frames"], -6);
    assert_eq!(fourth_cut(&s), (cut - rate.tick_of(6), cut - rate.tick_of(6), cut));
    assert_eq!(s.execute("story.cuts", json!({"time": cut.0})).unwrap()["cuts"][0]["sound"]["edit"], "J");
    s.execute("edit.undo", json!({})).unwrap();

    // rolling the picture instead: the picture cut moves earlier for an L, the sound stays
    let r = s.execute("story.splitEdit", json!({"kind": "L", "frames": 8, "roll": "video", "time": cut.0})).unwrap();
    assert_eq!(r["rolled"], "video");
    assert_eq!(fourth_cut(&s), (cut, cut, cut - rate.tick_of(8)));
}

#[test]
fn a_split_edit_is_refused_when_it_cannot_be_made() {
    let mut s = demo();
    let (_, _, cut) = fourth_cut(&s);
    refuses(
        &mut s,
        "story.splitEdit",
        &[
            json!({}),
            json!({"kind": "X"}),
            json!({"kind": 1}),
            json!({"kind": "L", "frames": 0}),
            json!({"kind": "L", "frames": -3}),
            json!({"kind": "L", "frames": 1e12}),
            json!({"kind": "L", "roll": "both"}),
            json!({"kind": "L", "roll": 3}),
        ],
    );
    // more than the clips have beyond the cut: the sound goes as far as its media does, and says so
    let r = s.execute("story.splitEdit", json!({"kind": "L", "frames": 10_000, "time": cut.0})).unwrap();
    let got = r["frames"].as_i64().unwrap();
    assert!(got > 0 && got < 10_000, "{r}");
    assert_eq!((r["asked"].as_i64(), r["clamped"].as_bool()), (Some(10_000), Some(true)));
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.execute("story.splitEdit", json!({"kind": "L", "frames": 4, "time": cut.0})).unwrap()["clamped"], false);
    s.execute("edit.undo", json!({})).unwrap();
    // the cut into the V2 overlay: it has no linked sound
    let over = s.active_sequence().unwrap().video_tracks[1].items[0].start;
    let e = s.execute("story.splitEdit", json!({"kind": "L", "time": over.0})).unwrap_err().to_string();
    assert!(e.contains("no linked sound"), "{e}");
    // already split: the sounds no longer meet where the picture cuts, and it says how to go on
    s.execute("story.splitEdit", json!({"kind": "L", "frames": 5, "time": cut.0})).unwrap();
    assert!(s.execute("story.splitEdit", json!({"kind": "L", "frames": 5, "time": cut.0})).is_ok(), "the sound cut is still one cut and can be rolled further");
}

#[test]
fn a_shadow_cut_is_kept_aside_and_compared() {
    let mut s = demo();
    let active = s.state.active_sequence.unwrap();
    let r = s.execute("story.shadowCut", json!({})).unwrap();
    let shadow = r["sequence"].as_u64().unwrap();
    assert_eq!(r["name"], "Main Edit (shadow cut 1)");
    assert_eq!(s.state.active_sequence, Some(active), "the working sequence stays open");
    assert_eq!(s.project.sequence(filmagic_project::ItemId(shadow)), s.project.sequence(active), "the copy is exact");
    assert_eq!(s.execute("story.shadowCut", json!({})).unwrap()["name"], "Main Edit (shadow cut 2)");
    assert_eq!(s.execute("story.shadowCut", json!({"name": "  before the recut "})).unwrap()["name"], "before the recut");

    let same = s.execute("story.compare", json!({"with": shadow})).unwrap();
    assert_eq!(same["identical"], true, "{same}");
    assert_eq!(same["cutsShared"], 6);

    // recut: a new cut at 2 s and the last clip trimmed by a second
    s.execute("sequence.addEdit", json!({"seconds": 2.0})).unwrap();
    let last = s.active_sequence().unwrap().video_tracks[0].items.last().unwrap().id;
    s.execute("timeline.trim", json!({"clip": last.0, "edge": "out", "mode": "regular", "deltaFrames": -24})).unwrap();
    let diff = s.execute("story.compare", json!({"with": shadow})).unwrap();
    assert_eq!(diff["identical"], false);
    assert_eq!(diff["cutsShared"], 6);
    let here = diff["cutsOnlyHere"].as_array().unwrap();
    assert_eq!(here.len(), 1, "{diff}");
    assert!((secs(&here[0]) - 2.0).abs() < 0.05);
    assert!(diff["cutsOnlyThere"].as_array().unwrap().is_empty());
    assert!((diff["secondsDifference"].as_f64().unwrap() + 24.0 / s.sequence_rate().as_f64()).abs() < 0.01, "{diff}");
    assert_eq!((diff["this"]["rhythm"]["cuts"].as_u64(), diff["other"]["rhythm"]["cuts"].as_u64()), (Some(7), Some(6)));
    // the shadow cut did not move
    assert_eq!(crate::story::shots(&s.project, s.project.sequence(filmagic_project::ItemId(shadow)).unwrap()).unwrap().len(), 7);

    // shot by shot: the first shot is cut in two, the last is shorter, nothing looks different
    let shots = diff["shots"].as_array().unwrap();
    assert_eq!(shots.len(), 8);
    assert_eq!((shots[0]["status"].as_str(), shots[0]["what"].clone()), (Some("changed"), json!(["shorter"])), "{}", shots[0]);
    assert_eq!(
        (shots[1]["status"].as_str(), shots[1]["there"]["shot"].as_u64(), shots[1]["shiftSeconds"].as_f64()),
        (Some("changed"), Some(1), Some(0.0)),
        "{}",
        shots[1]
    );
    assert_eq!(shots[2]["status"], "same", "{}", shots[2]);
    assert_eq!(shots[7]["what"], json!(["shorter"]));
    assert!(shots.iter().all(|x| x["picture"]["reads"] == "none"), "{shots:?}");
    assert_eq!(diff["summary"], json!({"same": 5, "changed": 3, "new": 0, "onlyThere": 0}));
    // a grade on one shot is found, and said to be what changed
    let graded = s.active_sequence().unwrap().video_tracks[0].items[3].id;
    s.execute("effects.apply", json!({"clips": [graded.0], "effect": "tint"})).unwrap();
    s.execute("effects.setParam", json!({"clip": graded.0, "effect": "tint", "param": "amount", "value": 100})).unwrap();
    let diff = s.execute("story.compare", json!({"with": shadow})).unwrap();
    let row = diff["shots"].as_array().unwrap().iter().find(|x| x["clip"].as_u64() == Some(graded.0)).unwrap().clone();
    assert_eq!(row["what"], json!(["looks different"]), "{row}");
    assert!(["visible", "strong"].contains(&row["picture"]["reads"].as_str().unwrap()), "{row}");
    // a shot the other version does not have, and one only it has
    s.execute("file.newColorMatte", json!({"color": "#ff0000", "name": "red"})).unwrap();
    let red = s.project.items.values().find(|i| i.name == "red").unwrap().id.0;
    s.execute("timeline.place", json!({"item": red, "track": "V3", "seconds": 23.0, "duration": {"seconds": 1.0}})).unwrap();
    let fifth = s.active_sequence().unwrap().video_tracks[0].items[5].id;
    s.execute("timeline.select", json!({"clips": [fifth.0]})).unwrap();
    s.execute("edit.clear", json!({})).unwrap();
    let diff = s.execute("story.compare", json!({"with": shadow, "picture": false})).unwrap();
    assert!(diff["shots"].as_array().unwrap().iter().any(|x| x["status"] == "new" && x["name"] == "red"), "{diff}");
    assert_eq!(diff["shotsOnlyThere"].as_array().unwrap().len(), 1, "{diff}");
    assert!(diff["shots"].as_array().unwrap().iter().all(|x| x.get("picture").is_none()), "pictures were not asked for");
    refuses(&mut s, "story.compare", &[json!({"with": shadow, "picture": "yes"}), json!({"with": shadow, "limit": 0}), json!({"with": shadow, "limit": 1e9})]);
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();

    // keeping a shadow cut is one undoable step
    let items = s.project.items.len();
    s.execute("story.shadowCut", json!({})).unwrap();
    assert_eq!(s.project.items.len(), items + 1);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.items.len(), items);
}

#[test]
fn shadow_cut_and_compare_refuse_hostile_parameters() {
    let mut s = demo();
    let media = s.active_sequence().unwrap().video_tracks[0].items[0].item.0;
    refuses(
        &mut s,
        "story.shadowCut",
        &[json!({"name": ""}), json!({"name": "   "}), json!({"name": 7}), json!({"name": "a\nb"}), json!({"name": "x".repeat(300)})],
    );
    refuses(&mut s, "story.compare", &[json!({}), json!({"with": "other"}), json!({"with": 999_999}), json!({"with": media}), json!({"with": -1})]);
}

#[test]
fn the_story_commands_are_registered_and_documented() {
    for id in ["story.rhythm", "story.cuts", "story.blinkPoints", "story.board", "story.splitEdit", "story.shadowCut", "story.compare"] {
        let spec = crate::commands::find(id).unwrap_or_else(|| panic!("{id} is not registered"));
        assert!(spec.params.starts_with('{') && !spec.label.is_empty());
    }
    // queries leave no journal entry; edits do
    assert!(!crate::commands::find("story.cuts").unwrap().journal);
    assert!(crate::commands::find("story.splitEdit").unwrap().journal);
}

/// The demo with its first clip sped up, so that every later cut falls between frames.
fn off_grid() -> Session {
    let mut s = demo();
    let first = s.active_sequence().unwrap().video_tracks[0].items[0].id.0;
    s.execute("clip.speedDuration", json!({"clips": [first], "speed": 150, "ripple": true})).unwrap();
    let fd = s.sequence_rate().frame_duration().0;
    let cut = s.active_sequence().unwrap().video_tracks[0].items[4].start;
    assert_ne!(cut.0 % fd, 0, "the test needs a cut between frames");
    s
}

#[test]
fn a_cut_between_frames_is_read_from_the_frames_either_side_of_it() {
    let mut s = off_grid();
    // Desert Dunes to Misty Forest: a hard cut between two different pictures, with no transition
    let cut = s.active_sequence().unwrap().video_tracks[0].items[4].start;
    let r = s.execute("story.cuts", json!({"time": cut.0})).unwrap();
    let c = &r["cuts"][0];
    assert_eq!(c["at"]["time"].as_i64(), Some(cut.0));
    assert!(c["transition"].is_null(), "{c}");
    // the frame the cut falls in still shows the outgoing shot: compared with itself it would be a "match"
    assert_eq!(c["change"]["kind"], "clean", "{c}");
    assert!(!c["issues"].as_array().unwrap().iter().any(|i| i.as_str().unwrap().contains("barely changes")), "{c}");
}

#[test]
fn the_board_shows_each_shot_from_a_frame_of_its_own() {
    let mut s = off_grid();
    // A shot that starts between frames and is on screen for one frame only: the frame its start
    // falls in still shows the shot before it.
    let q = s.active_sequence().unwrap();
    let rate = q.frame_rate();
    let start = q.video_tracks[0].items[4].start;
    let end = rate.tick_of(rate.frame_at(start) + 2);
    s.execute("sequence.addEdit", json!({"time": end.0})).unwrap();
    let dir = tmp_dir("story-board-grid");
    let r = s.execute("story.board", json!({"path": dir.join("b.png").to_string_lossy(), "width": 64})).unwrap();
    let cells = r["cells"].as_array().unwrap();
    assert!(cells.iter().any(|c| c["start"]["time"].as_i64() == Some(start.0)), "{r}");
    for c in cells {
        let (from, to, at) = (c["start"]["time"].as_i64().unwrap(), c["end"]["time"].as_i64().unwrap(), c["frame"]["time"].as_i64().unwrap());
        assert!(from <= at && at < to, "shot {} is shown by a frame of another shot: {c}", c["shot"]);
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_layer_over_a_shot_neither_starts_nor_ends_one() {
    let mut s = demo();
    let before = s.execute("story.rhythm", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    let (clip, cut) = (q.video_tracks[0].items[3].id.0, q.video_tracks[0].items[3].start);
    let sound = s.execute("story.cuts", json!({"time": cut.0})).unwrap()["cuts"][0]["sound"]["edit"].clone();
    assert_eq!(sound, "straight");
    let region = json!({"x": 0.1, "y": 0.1, "w": 0.4, "h": 0.4});
    // twice as fast, the layer ends halfway through the shot: that is not a cut
    s.execute("layers.retime", json!({"clip": clip, "region": region, "speed": 200})).unwrap();
    s.execute("layers.extract", json!({"clip": clip, "region": region, "cutOut": true})).unwrap();
    let after = s.execute("story.rhythm", json!({})).unwrap();
    assert_eq!((after["shots"].clone(), after["cuts"].clone()), (before["shots"].clone(), before["cuts"].clone()), "{after}");
    let shots = crate::story::shots(&s.project, s.active_sequence().unwrap()).unwrap();
    assert!(shots.iter().any(|sh| sh.clip == Some(ClipId(clip))), "the shot is still the clip, not a layer over it");
    // and the cut into it is still read with the clip's sound, and can still be split
    let c = s.execute("story.cuts", json!({"time": cut.0})).unwrap();
    assert_eq!(c["cuts"][0]["sound"]["edit"], "straight", "{c}");
    assert_eq!(c["cuts"][0]["to"]["clip"].as_u64(), Some(clip));
    let r = s.execute("story.splitEdit", json!({"kind": "L", "frames": 6, "time": cut.0})).unwrap();
    assert_eq!(r["frames"], 6);
    assert_eq!(s.execute("story.cuts", json!({"time": cut.0})).unwrap()["cuts"][0]["sound"]["edit"], "L");
}

#[test]
fn a_plate_over_a_shot_keeps_the_shots_sound() {
    let mut s = demo();
    let q = s.active_sequence().unwrap();
    let (clip, cut) = (q.video_tracks[0].items[3].id.0, q.video_tracks[0].items[3].start);
    let dir = tmp_dir("story-plate-sound");
    // a whole-frame plate stands in for the shot's picture; the shot, and its sound, are still the clip's underneath
    let plate =
        s.execute("layers.cleanPlate", json!({"clip": clip, "samples": 3, "path": dir.join("p.png").to_string_lossy()})).unwrap()["clip"].as_u64().unwrap();
    let c = s.execute("story.cuts", json!({"time": cut.0})).unwrap();
    assert_ne!(plate, clip);
    assert_eq!((c["cuts"][0]["to"]["clip"].as_u64(), c["cuts"][0]["sound"]["edit"].as_str()), (Some(clip), Some("straight")), "{c}");
    // rolling the picture would pull the plate away from its clip: refused, with the way to go on
    let e = s.execute("story.splitEdit", json!({"kind": "L", "roll": "video", "time": cut.0})).unwrap_err().to_string();
    assert!(e.contains("plate") && e.contains("audio"), "{e}");
    assert_eq!(s.execute("story.splitEdit", json!({"kind": "J", "frames": 5, "time": cut.0})).unwrap()["frames"], -5);
    assert_eq!(s.execute("story.cuts", json!({"time": cut.0})).unwrap()["cuts"][0]["sound"]["edit"], "J");
    let _ = std::fs::remove_dir_all(dir);
}

/// The demo with a second microphone: the sound of V1's third and fourth clips also on A3.
fn two_microphones() -> Session {
    let mut s = demo();
    s.edit_sequence("second microphone", |q, ctx, _| {
        let copies: Vec<filmagic_project::TrackItem> = q.audio_tracks[0].items.iter().skip(2).take(2).cloned().collect();
        for mut c in copies {
            c.id = ClipId(ctx.alloc());
            q.audio_tracks[2].items.push(c);
        }
        Ok(())
    })
    .unwrap();
    s
}

#[test]
fn a_split_edit_moves_the_cut_on_every_track_of_the_clips_sound() {
    let mut s = two_microphones();
    let rate = s.sequence_rate();
    let (_, _, cut) = fourth_cut(&s);
    assert_eq!(s.execute("story.cuts", json!({"time": cut.0})).unwrap()["cuts"][0]["sound"]["edit"], "straight");
    let r = s.execute("story.splitEdit", json!({"kind": "L", "frames": 10, "time": cut.0})).unwrap();
    assert_eq!((r["pairs"].as_array().unwrap().len(), r["frames"].as_i64()), (2, Some(10)), "{r}");
    assert!(r["warnings"].as_array().unwrap().is_empty());
    let q = s.active_sequence().unwrap();
    for track in [0, 2] {
        let items = &q.audio_tracks[track].items;
        let (a, b) = (items.iter().find(|i| i.link == Some(34)).unwrap(), items.iter().find(|i| i.link == Some(37)).unwrap());
        assert_eq!((a.end(), b.start), (cut + rate.tick_of(10), cut + rate.tick_of(10)), "A{}: both microphones cut together", track + 1);
    }
    assert_eq!(s.history.undo.last().unwrap().0, "Split Edit");
    assert_eq!(s.execute("story.cuts", json!({"time": cut.0})).unwrap()["cuts"][0]["sound"]["edit"], "L");
    s.execute("edit.undo", json!({})).unwrap();
    // one microphone already split: the other still meets at the cut and is rolled; the result says what was left
    let lone = s.active_sequence().unwrap().audio_tracks[2].items.iter().find(|i| i.link == Some(34)).unwrap().id.0;
    s.execute("timeline.trim", json!({"clip": lone, "edge": "out", "mode": "regular", "deltaFrames": -4, "linked": false})).unwrap();
    let r = s.execute("story.splitEdit", json!({"kind": "L", "frames": 3, "time": cut.0})).unwrap();
    assert_eq!(r["pairs"].as_array().unwrap().len(), 1);
    assert!(r["warnings"][0].as_str().unwrap().contains("Audio 3"), "{r}");
    // a locked sound track is not rolled behind its lock
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("timeline.setTrack", json!({"track": "A1", "locked": true})).unwrap();
    assert!(s.active_sequence().unwrap().audio_tracks[0].locked);
    refuses(&mut s, "story.splitEdit", &[json!({"kind": "L", "frames": 3, "time": cut.0})]);
}

#[test]
fn a_transition_on_another_track_is_not_the_cuts() {
    let mut s = demo();
    let cut = s.active_sequence().unwrap().video_tracks[0].items[4].start;
    assert!(s.execute("story.cuts", json!({"time": cut.0})).unwrap()["cuts"][0]["transition"].is_null());
    // a title that fades in across the cut, on the track above
    s.execute("playhead.set", json!({"time": (cut - Tick::from_seconds_f64(0.5)).0})).unwrap();
    s.execute("graphics.newText", json!({"text": "Title"})).unwrap();
    s.edit_sequence("fade the title in", |q, ctx, _| {
        let mut fade = q.video_tracks[0].transitions[1].clone();
        let (ti, title) = q
            .video_tracks
            .iter()
            .enumerate()
            .skip(1)
            .find_map(|(i, t)| t.items.iter().find(|it| it.name.contains("Title") || it.start == cut - Tick::from_seconds_f64(0.5)).map(|it| (i, it.clone())))
            .expect("the title");
        fade.id = filmagic_project::TransitionId(ctx.alloc());
        (fade.start, fade.duration, fade.from, fade.to) = (title.start, Tick::from_seconds_f64(1.0), None, Some(title.id));
        q.video_tracks[ti].transitions.push(fade);
        Ok(())
    })
    .unwrap();
    let c = s.execute("story.cuts", json!({"time": cut.0})).unwrap();
    assert!(c["cuts"][0]["transition"].is_null(), "the title's fade is not this cut's transition: {c}");
    assert_eq!(c["cuts"][0]["change"]["kind"], "clean");
    // the cuts that do sit in a transition still say so
    let first = s.active_sequence().unwrap().video_tracks[0].items[1].start;
    assert_eq!(s.execute("story.cuts", json!({"time": first.0})).unwrap()["cuts"][0]["transition"], "cross_dissolve");
}

#[test]
fn a_short_tail_is_counted_with_the_window_before_it() {
    let mut s = demo();
    // 26.8 s in windows of 5: five and a tail of 1.8 s with nothing of its own to say
    let r = s.execute("story.rhythm", json!({"window": 5})).unwrap();
    let windows = r["windows"].as_array().unwrap();
    assert_eq!(windows.len(), 5, "{r}");
    assert!((windows[4]["seconds"].as_f64().unwrap() - 6.777).abs() < 0.01 && windows[0]["seconds"] == 5.0, "{r}");
    assert_eq!(windows.iter().map(|w| w["cuts"].as_u64().unwrap()).sum::<u64>(), r["cuts"].as_u64().unwrap());
    // a cut in what would have been the tail: one cut in 1.8 s is not thirty-four a minute
    s.execute("sequence.addEdit", json!({"seconds": 26.0})).unwrap();
    let r = s.execute("story.rhythm", json!({"window": 5})).unwrap();
    let last = r["windows"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["cuts"], 2, "{last}");
    assert!(last["cutsPerMinute"].as_f64().unwrap() < 18.0, "{last}");
    assert!(!r["notes"].as_array().unwrap().iter().any(|n| n.as_str().unwrap().contains("faster than 40")), "{r}");
    // a tail of more than half a window stands on its own
    assert_eq!(s.execute("story.rhythm", json!({"window": 4})).unwrap()["windows"].as_array().unwrap().len(), 7);
}

#[test]
fn paging_through_cuts_misses_none_even_after_a_one_frame_shot() {
    let mut s = demo();
    let rate = s.sequence_rate();
    for frame in [24, 25, 26, 40] {
        s.execute("sequence.addEdit", json!({"time": rate.tick_of(frame).0})).unwrap();
    }
    let all = s.execute("story.cuts", json!({"limit": 500})).unwrap();
    let total = all["cutsInSequence"].as_u64().unwrap() as usize;
    let mut seen: Vec<i64> = Vec::new();
    let mut from: Option<f64> = None;
    for _ in 0..total + 2 {
        let mut p = json!({"limit": 1});
        if let Some(f) = from {
            p["fromSeconds"] = json!(f);
        }
        let r = s.execute("story.cuts", p).unwrap();
        seen.extend(r["cuts"].as_array().unwrap().iter().map(|c| c["at"]["time"].as_i64().unwrap()));
        match r["nextFromSeconds"].as_f64() {
            Some(next) => from = Some(next),
            None => break,
        }
    }
    let want: Vec<i64> = all["cuts"].as_array().unwrap().iter().map(|c| c["at"]["time"].as_i64().unwrap()).collect();
    assert_eq!(seen, want, "every cut once, in order");
    assert!(want.contains(&rate.tick_of(25).0) && want.contains(&rate.tick_of(26).0));
}

/// A grey picture clip eight seconds long with the spoken lines (4.9 s) as its linked sound.
fn spoken_picture() -> (Session, u64, u64, std::path::PathBuf) {
    let (mut s, sound, item, dir) = spoken(0.0);
    let matte = s.execute("file.newColorMatte", json!({"color": "#808080", "seconds": 8.0})).unwrap()["item"].as_u64().unwrap();
    let picture =
        s.execute("timeline.place", json!({"item": matte, "track": "V1", "seconds": 0.0, "duration": {"seconds": 8.0}})).unwrap()["clips"][0].as_u64().unwrap();
    s.edit_sequence("link", |q, _, _| {
        for id in [picture, sound] {
            if let Some((_, it)) = q.find_item_mut(ClipId(id)) {
                it.link = Some(9_000_001);
            }
        }
        Ok(())
    })
    .unwrap();
    (s, picture, item, dir)
}

#[test]
fn where_a_clip_has_no_sound_is_not_a_pause() {
    let (mut s, picture, _, dir) = spoken_picture();
    assert!(clip_of(&s, picture).duration.seconds() > 7.9);
    let r = s.execute("story.blinkPoints", json!({"clip": picture, "picture": false})).unwrap();
    // the three phrase ends, as when the sound is read on its own; not one "pause" where the sound clip ends
    let ends: Vec<f64> = r["points"].as_array().unwrap().iter().map(|pt| secs(&pt["pause"]["start"])).collect();
    assert_eq!(ends.len(), 3, "{r}");
    for (got, want) in ends.iter().zip([1.0, 2.3, 3.9]) {
        assert!((got - want).abs() < 0.05, "{ends:?}");
    }
    assert!(r["notes"].as_array().unwrap().iter().any(|n| n.as_str().unwrap().contains("does not run the whole length")), "{r}");
    // a span with no sound in it at all has no sound to analyse, and says so rather than finding a pause
    let r = s.execute("story.blinkPoints", json!({"clip": picture, "fromSeconds": 6.0, "toSeconds": 7.5})).unwrap();
    assert_eq!(r["analysed"]["sound"], false, "{r}");
    assert!(r["points"].as_array().unwrap().iter().all(|pt| pt["kind"] == "movement"), "{r}");
    let _ = std::fs::remove_dir_all(dir);
}

fn clip_of(s: &Session, id: u64) -> filmagic_project::TrackItem {
    s.active_sequence().unwrap().find_item(ClipId(id)).unwrap().1.clone()
}

#[test]
fn a_subclip_is_read_where_it_lies_in_its_media() {
    let (mut s, _, item, dir) = spoken(0.0);
    // the last 2.9 s of the lines: the phrase ends at 2.3 and 3.9 s of the recording
    let (from, to) = (Tick::from_seconds_f64(2.0), Tick::from_seconds_f64(4.9));
    let sub = s.execute("clip.makeSubclip", json!({"item": item, "start": from.0, "end": to.0})).unwrap()["item"].as_u64().unwrap();
    let r = s.execute("story.blinkPoints", json!({"item": sub})).unwrap();
    assert_eq!(r["span"]["from"]["time"].as_i64(), Some(from.0), "{r}");
    let ends: Vec<f64> = r["points"].as_array().unwrap().iter().map(|pt| secs(&pt["pause"]["start"])).collect();
    assert_eq!(ends.len(), 2, "{r}");
    assert!((ends[0] - 2.3).abs() < 0.05 && (ends[1] - 3.9).abs() < 0.05, "{ends:?}");
    // a span is in the media's own time too, and is kept inside the subclip
    let r = s.execute("story.blinkPoints", json!({"item": sub, "fromSeconds": 0.0, "toSeconds": 3.0})).unwrap();
    assert_eq!((r["span"]["from"]["time"].as_i64(), r["points"].as_array().unwrap().len()), (Some(from.0), 1), "{r}");
    let _ = std::fs::remove_dir_all(dir);

    // the board of a subclip shows the subclip's frames, at its media's frame rate
    let mut s = demo();
    let forest = s.project.items.values().find(|i| i.name == "Misty_Forest.mp4").unwrap().id.0;
    let rate = s.project.item(filmagic_project::ItemId(forest)).unwrap().frame_rate();
    let (from, to) = (rate.tick_of(48), rate.tick_of(96));
    let sub = s.execute("clip.makeSubclip", json!({"item": forest, "start": from.0, "end": to.0})).unwrap()["item"].as_u64().unwrap();
    let dir = tmp_dir("story-subclip");
    let r = s.execute("story.board", json!({"item": sub, "count": 4, "width": 64, "path": dir.join("b.png").to_string_lossy()})).unwrap();
    let cells = r["cells"].as_array().unwrap();
    assert_eq!(cells.len(), 4);
    for c in cells {
        let at = c["at"]["time"].as_i64().unwrap();
        assert!(from.0 <= at && at < to.0, "outside the subclip: {c}");
        assert_eq!(at % rate.frame_duration().0, 0, "on the media's own frames: {c}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn two_versions_are_identical_only_when_nothing_differs() {
    let mut s = demo();
    let shadow = s.execute("story.shadowCut", json!({})).unwrap()["sequence"].as_u64().unwrap();
    let both = |s: &mut Session| {
        let r = s.execute("story.compare", json!({"with": shadow, "picture": false})).unwrap();
        (r["sameShots"].as_bool().unwrap(), r["identical"].as_bool().unwrap())
    };
    assert_eq!(both(&mut s), (true, true));
    let q = s.active_sequence().unwrap();
    let (third, sound) = (q.video_tracks[0].items[3].id.0, q.audio_tracks[1].items[0].id.0);
    // another stretch of the same take in the same place: every cut where it was, and not the same film
    s.execute("timeline.slip", json!({"clip": third, "deltaFrames": 6})).unwrap();
    let r = s.execute("story.compare", json!({"with": shadow, "picture": false})).unwrap();
    assert!(r["cutsOnlyHere"].as_array().unwrap().is_empty() && r["cutsOnlyThere"].as_array().unwrap().is_empty());
    assert_eq!((r["sameShots"].as_bool(), r["identical"].as_bool()), (Some(false), Some(false)), "{r}");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(both(&mut s), (true, true));
    // a grade on a shot
    s.execute("effects.apply", json!({"clips": [third], "effect": "tint"})).unwrap();
    assert_eq!(both(&mut s), (false, false));
    s.execute("edit.undo", json!({})).unwrap();
    // the music cut short: the shots are the same, the film is not
    s.execute("timeline.trim", json!({"clip": sound, "edge": "out", "mode": "regular", "deltaFrames": -24, "linked": false})).unwrap();
    assert_eq!(both(&mut s), (true, false));
    s.execute("edit.undo", json!({})).unwrap();
    // the tail of the last shot trimmed
    let last = s.active_sequence().unwrap().video_tracks[0].items.last().unwrap().id.0;
    s.execute("timeline.trim", json!({"clip": last, "edge": "out", "mode": "regular", "deltaFrames": -12})).unwrap();
    assert_eq!(both(&mut s), (false, false));
}

#[test]
fn finding_points_is_journaled_only_when_it_leaves_markers() {
    let (mut s, clip, _, dir) = spoken(0.0);
    let journaled = |s: &Session| s.journal.iter().filter(|(id, _)| id == "story.blinkPoints").count();
    s.execute("story.blinkPoints", json!({"clip": clip})).unwrap();
    s.execute("story.blinkPoints", json!({"clip": clip, "markers": false})).unwrap();
    assert_eq!(journaled(&s), 0, "a question is not an edit");
    s.execute("story.blinkPoints", json!({"clip": clip, "markers": true})).unwrap();
    assert_eq!(journaled(&s), 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_shadow_cut_takes_a_number_none_has() {
    let mut s = demo();
    let first = s.execute("story.shadowCut", json!({})).unwrap()["sequence"].as_u64().unwrap();
    assert_eq!(s.execute("story.shadowCut", json!({})).unwrap()["name"], "Main Edit (shadow cut 2)");
    // the first one renamed (or deleted): the next does not become a second "shadow cut 2"
    s.edit("rename", |pr, _| {
        if let Some(i) = pr.item_mut(filmagic_project::ItemId(first)) {
            i.name = "first assembly".into();
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(s.execute("story.shadowCut", json!({})).unwrap()["name"], "Main Edit (shadow cut 1)");
    assert_eq!(s.execute("story.shadowCut", json!({})).unwrap()["name"], "Main Edit (shadow cut 3)");
    let names: Vec<&str> = s.project.items.values().map(|i| i.name.as_str()).filter(|n| n.contains("shadow cut")).collect();
    let unique: std::collections::BTreeSet<&str> = names.iter().copied().collect();
    assert_eq!(names.len(), unique.len(), "{names:?}");
}

#[test]
fn a_board_too_large_is_refused_for_what_it_would_measure() {
    let mut s = demo();
    let item = s.project.items.values().find(|i| i.name == "Misty_Forest.mp4").unwrap().id.0;
    let dir = tmp_dir("story-board-budget");
    let path = dir.join("b.png").to_string_lossy().into_owned();
    // 17 frames 1280 wide are under sixteen million pixels; the sheet that holds them, five rows of four with gaps, is not
    let e = s.execute("story.board", json!({"item": item, "count": 17, "width": 1280, "columns": 4, "path": path})).unwrap_err().to_string();
    assert!(e.contains("5150"), "the sheet's own size, known before a frame is rendered: {e}");
    assert!(!std::path::Path::new(&path).exists());
    // three rows fit
    let r = s.execute("story.board", json!({"item": item, "count": 12, "width": 1280, "columns": 4, "path": path})).unwrap();
    assert_eq!(r["width"], 5150);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_long_film_is_lined_up_shot_for_shot_beyond_what_is_listed() {
    let mut s = demo();
    let rate = s.sequence_rate();
    // a cut on every frame of the first twenty-odd seconds: well over five hundred shots
    for frame in 1..=520 {
        s.execute("sequence.addEdit", json!({"time": rate.tick_of(frame).0})).unwrap();
    }
    let shots = crate::story::shots(&s.project, s.active_sequence().unwrap()).unwrap().len();
    assert!(shots > 520, "{shots}");
    let shadow = s.execute("story.shadowCut", json!({})).unwrap()["sequence"].as_u64().unwrap();
    let r = s.execute("story.compare", json!({"with": shadow, "picture": false})).unwrap();
    // nothing differs, and nothing is reported gone because it lay past the five hundredth row
    assert_eq!((r["identical"].as_bool(), r["sameShots"].as_bool()), (Some(true), Some(true)));
    // (the forest is in the film twice, as the cutaway and as a shot of its own: each use is lined up with itself)
    assert_eq!(r["summary"], json!({"same": shots, "changed": 0, "new": 0, "onlyThere": 0}), "{}", r["summary"]);
    assert!(r["shotsOnlyThere"].as_array().unwrap().is_empty());
    assert_eq!((r["shots"].as_array().unwrap().len(), r["truncated"].as_bool()), (500, Some(true)));
    assert!(r["notes"].as_array().unwrap().iter().any(|n| n.as_str().unwrap().contains("more shots than are listed")));
}

#[test]
fn a_reversed_or_held_shot_that_did_not_change_is_the_same_shot() {
    let mut s = demo();
    let q = s.active_sequence().unwrap();
    let (reversed, held) = (q.video_tracks[0].items[3].id.0, q.video_tracks[0].items[4].id.0);
    s.execute("clip.speedDuration", json!({"clips": [reversed], "speed": 100, "reverse": true})).unwrap();
    s.execute("timeline.select", json!({"clips": [held]})).unwrap();
    s.execute("clip.frameHoldOptions", json!({"holdOn": "in"})).unwrap();
    let shadow = s.execute("story.shadowCut", json!({})).unwrap()["sequence"].as_u64().unwrap();
    let r = s.execute("story.compare", json!({"with": shadow, "picture": false})).unwrap();
    assert_eq!(r["summary"], json!({"same": 7, "changed": 0, "new": 0, "onlyThere": 0}), "{r}");
    assert_eq!(r["identical"], true);
    // played another way, it is another shot
    s.execute("timeline.slip", json!({"clip": reversed, "deltaFrames": 4})).unwrap();
    let r = s.execute("story.compare", json!({"with": shadow, "picture": false})).unwrap();
    let row = r["shots"].as_array().unwrap().iter().find(|x| x["clip"].as_u64() == Some(reversed)).unwrap().clone();
    // `what` is a list on every row; why a shot is new is said in `note`
    assert_eq!((row["status"].as_str(), row["what"].clone()), (Some("new"), json!([])), "{row}");
    assert!(row["note"].as_str().unwrap().contains("reversed or held"));
}

#[test]
fn versions_of_another_shape_are_said_not_to_be_compared_by_picture() {
    let mut s = demo();
    let shadow = s.execute("story.shadowCut", json!({})).unwrap()["sequence"].as_u64().unwrap();
    // the vertical version of the same cut
    s.edit("vertical", |pr, _| {
        let q = pr.sequence_mut(filmagic_project::ItemId(shadow)).ok_or(crate::EngineError::NoSequence)?;
        (q.settings.width, q.settings.height) = (1080, 1920);
        Ok(())
    })
    .unwrap();
    let r = s.execute("story.compare", json!({"with": shadow})).unwrap();
    let rows = r["shots"].as_array().unwrap();
    assert!(rows.iter().all(|x| x.get("picture").is_some_and(Value::is_null)), "asked for, and not compared: {rows:?}");
    assert!(r["notes"].as_array().unwrap().iter().any(|n| n.as_str().unwrap().contains("not the same shape")), "{r}");
    // not asked for: no field, no note
    let r = s.execute("story.compare", json!({"with": shadow, "picture": false})).unwrap();
    assert!(r["shots"].as_array().unwrap().iter().all(|x| x.get("picture").is_none()) && r["notes"].as_array().unwrap().is_empty());
}

#[test]
fn a_cut_to_black_or_between_even_frames_is_not_called_a_jump_cut() {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name": "dark", "width": 320, "height": 180, "fps": 24.0})).unwrap();
    let matte = |s: &mut Session, color: &str, at: f64| {
        let item = s.execute("file.newColorMatte", json!({"color": color, "width": 320, "height": 180, "seconds": 1.0})).unwrap()["item"].as_u64().unwrap();
        s.execute("timeline.place", json!({"item": item, "track": "V1", "seconds": at})).unwrap();
    };
    // near black, a second of nothing, all but black, then a shade lighter
    matte(&mut s, "#0a0a0a", 0.0);
    matte(&mut s, "#010101", 2.0);
    matte(&mut s, "#0f0f0f", 3.0);
    let r = s.execute("story.cuts", json!({})).unwrap();
    let cuts = r["cuts"].as_array().unwrap();
    assert_eq!(cuts.len(), 3, "{r}");
    let says = |c: &Value, what: &str| c["issues"].as_array().unwrap().iter().any(|i| i.as_str().unwrap().contains(what));
    // into black: the amount is a jump cut's, and there is no second picture to jump to
    let c = &cuts[0];
    assert_eq!((c["change"]["kind"].as_str(), c["change"]["likeness"].is_null(), c["to"]["clip"].is_null()), (Some("jump"), true, true), "{c}");
    assert!(c["issues"].as_array().unwrap().is_empty(), "{c}");
    // Between two even frames nothing says it is the same picture shifted: it is not called a
    // jump cut for certain, and the two frames are left to be looked at.
    let c = &cuts[2];
    assert_eq!(c["change"]["kind"], "jump", "{c}");
    assert!(says(c, "Look at the two frames") && !says(c, "jump cut: "), "{c}");
}

#[test]
fn every_look_of_a_span_is_taken_at_any_frame_rate() {
    let second = Tick(254_016_000_000);
    // ten minutes at 25 fps, a look every third frame: five thousand looks, where eight a second were assumed
    assert_eq!(crate::story::looks_in(Tick(second.0 * 600), Tick(second.0 * 3 / 25)), 5000);
    assert_eq!(crate::story::looks_in(Tick(second.0 * 600), Tick(second.0 / 10)), 6000);
    assert_eq!(crate::story::looks_in(Tick(second.0 * 600), Tick(second.0 / 8)), 4800);
    // nothing to look at, and a step that is no step
    assert_eq!(
        (crate::story::looks_in(Tick::ZERO, second), crate::story::looks_in(Tick(-5), second), crate::story::looks_in(second, Tick::ZERO)),
        (0, 0, 144_000)
    );
}

#[test]
fn a_board_is_a_png_of_its_own_and_shows_frames_as_wide_as_they_are_seen() {
    let dir = tmp_dir("story-board-shape");
    let movie = dir.join("clip.png");
    // a 120×90 still whose pixels are twice as wide as they are tall: seen 240×90
    std::fs::write(&movie, filmagic_export::encode_png(vec![200u8; 120 * 90 * 4], 120, 90).unwrap()).unwrap();
    let mut s = Session::default();
    let item = s.execute("file.import", json!({"paths": [movie.to_string_lossy()]})).unwrap()["items"][0].as_u64().unwrap();
    s.execute("file.newSequence", json!({"name": "wide", "width": 240, "height": 90, "fps": 24.0})).unwrap();
    let path = dir.join("board.png").to_string_lossy().into_owned();
    let square = s.execute("story.board", json!({"item": item, "count": 1, "width": 120, "path": path})).unwrap();
    assert_eq!((square["width"].as_u64(), square["height"].as_u64()), (Some(120 + 12), Some(90 + 12)));
    s.execute("clip.interpretFootage", json!({"items": [item], "pixelAspect": [2, 1]})).unwrap();
    let wide = s.execute("story.board", json!({"item": item, "count": 1, "width": 120, "path": path})).unwrap();
    assert_eq!((wide["width"].as_u64(), wide["height"].as_u64()), (Some(120 + 12), Some(45 + 12)), "half as tall for its width: {wide}");
    // it writes a PNG, and never over a file the project reads
    refuses(
        &mut s,
        "story.board",
        &[
            json!({"item": item, "path": dir.join("board.jpg").to_string_lossy()}),
            json!({"item": item, "path": dir.join("board").to_string_lossy()}),
            json!({"item": item, "path": movie.to_string_lossy()}),
        ],
    );
    assert_eq!(std::fs::read(&movie).unwrap(), filmagic_export::encode_png(vec![200u8; 120 * 90 * 4], 120, 90).unwrap(), "the media is as it was");
    let _ = std::fs::remove_dir_all(dir);
}
