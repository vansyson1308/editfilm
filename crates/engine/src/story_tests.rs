//! `story.*`: cutting rhythm, cut analysis, blink points, the board, split edits, shadow cuts.

use filmcraft_project::ClipId;
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::Session;
use crate::media_test_util::tmp_dir;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
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
    std::fs::write(&path, filmcraft_media::wav::write_wav16(&samples, 1, rate)).unwrap();
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
    assert_eq!(s.project.sequence(filmcraft_project::ItemId(shadow)), s.project.sequence(active), "the copy is exact");
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
    assert_eq!(crate::story::shots(&s.project, s.project.sequence(filmcraft_project::ItemId(shadow)).unwrap()).unwrap().len(), 7);

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
