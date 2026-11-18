//! #14: which clips and tracks a command targets. Link ids and other ids that are no clip are
//! refused with what they are; `effects.setParam` takes several clips as one undo step; the
//! commands an agent could not find are found by what they do; templates and graphics take
//! "V2" / "auto"; `timeline.place` `track:"new"` follows the media; `file.newSequence` can take
//! only a clip's settings.

use std::sync::Arc;

use filmagic_project::{ClipId, TrackItem};
use filmagic_time::Tick;
use serde_json::{Value, json};

use crate::{EngineError, Session};

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.state.selection.clear();
    s
}

fn refused(r: crate::Result<Value>) -> String {
    match r {
        Err(EngineError::BadParams { msg, .. }) => msg,
        other => panic!("expected refused parameters, got {other:?}"),
    }
}

/// The first video clip of V1 with a linked audio clip, and that audio clip.
fn linked_pair(s: &Session) -> (TrackItem, TrackItem) {
    let q = s.active_sequence().unwrap();
    for v in &q.video_tracks[0].items {
        if let Some(a) = q.audio_tracks.iter().flat_map(|t| &t.items).find(|a| v.link.is_some() && a.link == v.link) {
            return (v.clone(), a.clone());
        }
    }
    panic!("the demo has a linked clip on V1");
}

fn clip(s: &Session, id: ClipId) -> TrackItem {
    s.active_sequence().unwrap().find_item(id).unwrap().1.clone()
}

// ---------------------------------------------------------------- link ids and other non-clips

/// The session's `clip.speedDuration [52, 53]`, where 53 was the link of clip 52: it said "track
/// is locked". It now says that 53 is a link, which clips it links, and changes nothing.
#[test]
fn a_link_id_named_as_a_clip_says_what_it_is() {
    let mut s = demo();
    let (v, a) = linked_pair(&s);
    let link = v.link.unwrap();
    let before = s.project.clone();
    let undo = s.history.undo.len();
    let why = refused(s.execute("clip.speedDuration", json!({"clips": [v.id.0, link], "speed": 50})));
    assert!(why.starts_with(&format!("{link} is a link id, not a clip id; its linked clips are ")), "{why}");
    assert!(why.contains(&format!("{} (video, V1)", v.id.0)) && why.contains(&format!("{} (audio, A", a.id.0)), "{why}");
    assert!(!why.contains("locked"), "{why}");
    assert!(Arc::ptr_eq(&before, &s.project) && s.history.undo.len() == undo, "nothing changed");
    // the clip ids it names work
    s.execute("clip.speedDuration", json!({"clips": [v.id.0, a.id.0], "speed": 200})).unwrap();
    assert!(clip(&s, v.id).duration < v.duration && clip(&s, a.id).duration < a.duration);
}

/// `essentialSound.set` given a link id beside a clip changed the clip and silently skipped the
/// link; given only the link it said to select audio clips.
#[test]
fn essential_sound_refuses_a_link_id_instead_of_skipping_it() {
    let mut s = demo();
    let (v, a) = linked_pair(&s);
    let link = v.link.unwrap();
    s.execute("essentialSound.setType", json!({"clips": [a.id.0], "type": "dialogue"})).unwrap();
    let before = s.project.clone();
    for clips in [json!([a.id.0, link]), json!([link])] {
        let why = refused(s.execute("essentialSound.set", json!({"clips": clips, "key": "repair.noise.on", "value": true})));
        assert!(why.contains(&format!("{link} is a link id")) && why.contains(&a.id.0.to_string()), "{clips}: {why}");
        assert!(Arc::ptr_eq(&before, &s.project), "{clips}: nothing changed");
    }
    // the video clip of the pair stands for its linked audio (Linked Selection is on by default)
    s.execute("essentialSound.set", json!({"clips": [v.id.0], "key": "repair.noise.on", "value": true})).unwrap();
    assert!(!Arc::ptr_eq(&before, &s.project));
    let r = s.execute("essentialSound.inspect", json!({"clips": [a.id.0]})).unwrap();
    assert_eq!(r["clips"][0]["settings"]["repair"]["noise"]["on"], true, "{r}");
}

/// With Linked Selection off, a video clip has no audio to stand for: the error says what to pass.
#[test]
fn essential_sound_on_a_lone_video_clip_says_what_to_pass() {
    let mut s = demo();
    s.execute("sequence.linkedSelection", json!({"on": false})).unwrap();
    let (v, _) = linked_pair(&s);
    let why = refused(s.execute("essentialSound.setType", json!({"clips": [v.id.0], "type": "music"})));
    assert!(why.contains("no audio clip to change") && why.contains("Linked Selection"), "{why}");
}

/// Every kind of id that is no clip is named as what it is, for any command that takes clips.
#[test]
fn ids_of_other_things_are_named_for_what_they_are() {
    let mut s = demo();
    let (v, a) = linked_pair(&s);
    s.execute("timeline.select", json!({"clips": [v.id.0, a.id.0]})).unwrap();
    s.execute("clip.group", json!({})).unwrap();
    s.state.selection.clear();
    let group = clip(&s, v.id).group.unwrap();
    let q = s.active_sequence().unwrap();
    let v2 = q.video_tracks[1].id.0;
    let item = v.item.0;
    let before = s.project.clone();
    for (cmd, params) in [
        ("clip.enable", json!({"clips": [group]})),
        ("edit.clear", json!({"clips": [v2]})),
        ("clip.speedDuration", json!({"clips": [item], "speed": 50})),
        ("clip.audioGain", json!({"clips": [u64::MAX], "db": -3})),
        ("effects.setParam", json!({"clip": "52", "param": "scale", "value": 50})),
        ("edit.rippleDelete", json!({"clips": [-1, 1.5, null, {"clip": 1}]})),
    ] {
        let why = refused(s.execute(cmd, params.clone()));
        assert!(why.ends_with("Nothing was changed"), "{cmd} {params}: {why}");
        assert!(Arc::ptr_eq(&before, &s.project), "{cmd} {params}: nothing changed");
    }
    assert!(refused(s.execute("clip.enable", json!({"clips": [group]}))).starts_with(&format!("{group} is a group id")));
    assert!(refused(s.execute("edit.clear", json!({"clips": [v2]}))).starts_with(&format!("{v2} is the id of track V2")));
    let why = refused(s.execute("clip.speedDuration", json!({"clips": [item], "speed": 50})));
    assert!(why.starts_with(&format!("{item} is the project item")) && why.contains(&format!("{} (video, V1)", v.id.0)), "{why}");
    assert!(refused(s.execute("clip.audioGain", json!({"clips": [u64::MAX]}))).starts_with(&format!("no clip {}", u64::MAX)));
}

/// A huge list of wrong ids is answered with the first few and a count, quickly.
#[test]
fn a_huge_list_of_wrong_ids_gets_a_short_answer() {
    let mut s = demo();
    let ids: Vec<u64> = (0..200_000u64).map(|i| u64::MAX - i).collect();
    let started = std::time::Instant::now();
    let why = refused(s.execute("clip.enable", json!({"clips": ids})));
    assert!(why.contains("and 199992 more ids that are no clips"), "{}", &why[..why.len().min(400)]);
    assert!(why.len() < 2000, "{} bytes", why.len());
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
}

// ---------------------------------------------------------------- effects.setParam on many clips

#[test]
fn set_param_takes_many_clips_as_one_undo_step() {
    let mut s = demo();
    let clips: Vec<TrackItem> = s.active_sequence().unwrap().video_tracks[0].items.iter().take(3).cloned().collect();
    assert_eq!(clips.len(), 3);
    let ids: Vec<u64> = clips.iter().map(|c| c.id.0).collect();
    let undo = s.history.undo.len();
    let r = s.execute("effects.setParam", json!({"clips": ids, "effect": "opacity", "param": "opacity", "value": 40.0})).unwrap();
    assert_eq!(r["clips"].as_array().unwrap().len(), 3, "{r}");
    assert_eq!(s.history.undo.len(), undo + 1, "one undo step");
    let opacity = |s: &Session, c: ClipId| clip(s, c).effect("opacity").unwrap().f64_at("opacity", Tick::ZERO);
    for c in &clips {
        assert_eq!(opacity(&s, c.id), 40.0);
    }
    s.execute("edit.undo", json!({})).unwrap();
    for c in &clips {
        assert_eq!(opacity(&s, c.id), 100.0, "undo restores every clip");
    }
    // one clip as before, with the old result
    let r = s.execute("effects.setParam", json!({"clip": ids[0], "effect": "opacity", "param": "opacity", "value": 50.0})).unwrap();
    assert!(r.get("clips").is_none() && r["value"] == json!(50.0), "{r}");
}

/// With #13's time values and easing: one call keys several clips at a time in seconds.
#[test]
fn set_param_on_many_clips_takes_time_objects_and_ease() {
    let mut s = demo();
    let q = s.active_sequence().unwrap();
    let (a, b) = (q.video_tracks[0].items[0].clone(), q.video_tracks[0].items[1].clone());
    let at = |c: &TrackItem| json!({"seconds": (c.start.0 as f64 / filmagic_time::TICKS_PER_SECOND as f64) + 0.5});
    for c in [&a, &b] {
        let r = s
            .execute(
                "effects.setParam",
                json!({"clips": [c.id.0], "effect": "opacity", "param": "opacity", "value": 50.0, "keyframe": true, "time": at(c), "ease": "easeOut"}),
            )
            .unwrap();
        assert_eq!(r["keyframes"], 1, "{r}");
        assert_eq!(r["ease"], "easeOut", "{r}");
    }
    let undo = s.history.undo.len();
    let r = s
        .execute("effects.setParam", json!({"clips": [a.id.0, b.id.0], "effect": "opacity", "param": "opacity", "value": 20.0, "time": {"seconds": 0}}))
        .unwrap();
    assert_eq!(r["clips"].as_array().unwrap().len(), 2, "{r}");
    assert_eq!(s.history.undo.len(), undo + 1, "one undo step");
    // a link id is refused after the time object is read, not mistaken for a bad time
    let link = linked_pair(&s).0.link.unwrap();
    let why = refused(s.execute("effects.setParam", json!({"clips": [a.id.0, link], "param": "opacity", "value": 1, "time": {"seconds": 1}})));
    assert!(why.contains(&format!("{link} is a link id")), "{why}");
}

/// A list that fails on one clip changes none of them, and malformed lists are refused.
#[test]
fn set_param_on_many_clips_is_all_or_nothing_and_checks_its_list() {
    let mut s = demo();
    let q = s.active_sequence().unwrap();
    let (v0, v1) = (q.video_tracks[0].items[0].id.0, q.video_tracks[0].items[1].id.0);
    s.execute("effects.apply", json!({"clips": [v0], "effect": "Gaussian Blur"})).unwrap();
    let before = s.project.clone();
    let p = |clips: Value| json!({"clips": clips, "effect": "gaussian_blur", "param": "blurriness", "value": 20.0});
    let why = refused(s.execute("effects.setParam", p(json!([v0, v1]))));
    assert!(why.contains(&format!("clip {v1} has no effect")), "{why}");
    assert!(Arc::ptr_eq(&before, &s.project), "the clip that has the effect kept its value");
    for bad in [json!([]), json!("all"), json!([v0, v0]), json!([v0, f64::MAX])] {
        assert!(s.execute("effects.setParam", p(bad.clone())).is_err(), "{bad}");
    }
    assert!(s.execute("effects.setParam", json!({"clip": v0, "clips": [v0], "effect": "gaussian_blur", "param": "blurriness", "value": 1})).is_err());
    // out of range on the second clip: the first keeps its value
    s.execute("effects.apply", json!({"clips": [v1], "effect": "Gaussian Blur"})).unwrap();
    let before2 = s.project.clone();
    assert!(s.execute("effects.setParam", json!({"clips": [v0, v1], "effect": "gaussian_blur", "param": "blurriness", "value": 1e300})).is_err());
    assert!(Arc::ptr_eq(&before2, &s.project));
    assert!(!Arc::ptr_eq(&before, &s.project));
}

// ---------------------------------------------------------------- findability

#[test]
fn command_list_matches_words_in_labels_params_and_keywords() {
    let mut s = demo();
    let ids = |s: &mut Session, f: &str| -> Vec<String> {
        s.execute("command.list", json!({"filter": f})).unwrap().as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap().to_string()).collect()
    };
    // words in any order, from the label
    assert!(ids(&mut s, "attributes paste").contains(&"edit.pasteAttributes".to_string()));
    // what it does, from its keywords
    for (f, want) in [
        ("many clips colour", "edit.pasteAttributes"),
        ("grade several clips", "file.newAdjustmentLayer"),
        ("adjustment layer", "file.newAdjustmentLayer"),
        ("remove effects many clips", "edit.removeAttributes"),
    ] {
        assert!(ids(&mut s, f).contains(&want.to_string()), "{f}: {:?}", ids(&mut s, f));
    }
    // from the params doc
    assert!(ids(&mut s, "timeRemapping").contains(&"edit.pasteAttributes".to_string()));
    // the old single substring still works, and an empty filter lists everything
    assert!(ids(&mut s, "razor").contains(&"timeline.razor".to_string()));
    assert_eq!(ids(&mut s, "").len(), crate::commands::command_specs().len());
    assert!(ids(&mut s, "zzzz-no-such-word").is_empty());
    let r = s.execute("command.list", json!({"filter": "paste attributes"})).unwrap();
    let pa = r.as_array().unwrap().iter().find(|c| c["id"] == "edit.pasteAttributes").unwrap();
    assert!(pa["keywords"].as_str().unwrap().contains("many clips"), "{pa}");
}

// ---------------------------------------------------------------- track targeting

fn graphics_session() -> Session {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name": "Tracks", "fps": 24, "width": 128, "height": 128, "video": 3, "audio": 1})).unwrap();
    s
}

fn track_index(s: &Session, c: &Value) -> usize {
    let q = s.active_sequence().unwrap();
    let (t, _) = q.find_item(ClipId(c["clip"].as_u64().unwrap())).unwrap();
    q.video_tracks.iter().position(|x| x.id == t).unwrap()
}

#[test]
fn graphics_take_a_track_name_and_auto() {
    let mut s = graphics_session();
    // a clip on V2 from 1 s to 2 s; nothing at the playhead (0)
    s.execute("graphics.newRectangle", json!({"track": 1, "frame": 24, "seconds": 1})).unwrap();
    let r = s.execute("graphics.newText", json!({"text": "a", "track": "V3", "frame": 0, "seconds": 2})).unwrap();
    assert_eq!(track_index(&s, &r), 2, "V3 is the third video track");
    s.execute("edit.undo", json!({})).unwrap();
    // the default looks at the playhead only: V1 is free there and over the range
    let r = s.execute("graphics.newText", json!({"text": "b", "frame": 0, "seconds": 2})).unwrap();
    assert_eq!(track_index(&s, &r), 0);
    s.execute("edit.undo", json!({})).unwrap();
    // auto: above every clip the range overlaps, so nothing covers it
    let r = s.execute("graphics.newText", json!({"text": "c", "track": "auto", "frame": 0, "seconds": 2})).unwrap();
    assert_eq!(track_index(&s, &r), 2);
    // auto again over the same range: V3 is taken now, so a new track is added
    let r = s.execute("graphics.newText", json!({"text": "d", "track": "AUTO", "frame": 0, "seconds": 2})).unwrap();
    assert_eq!(track_index(&s, &r), 3);
    assert_eq!(s.active_sequence().unwrap().video_tracks.len(), 4);
}

#[test]
fn graphics_track_names_that_cannot_be_used_say_what_to_do() {
    let mut s = graphics_session();
    s.execute("graphics.newRectangle", json!({"track": "V2", "frame": 0, "seconds": 1})).unwrap();
    let before = s.project.clone();
    for (track, says) in [
        (json!("V2"), "V2 is not free here"),
        (json!("V9"), "no track V9: this sequence has V1–V3"),
        (json!("A1"), "a graphic goes on a video track"),
        (json!("V0"), "`track` takes"),
        (json!("top"), "`track` takes"),
        (json!(-1), "`track` takes"),
        (json!(1.5), "`track` takes"),
        (json!([1]), "`track` takes"),
    ] {
        let e = s.execute("graphics.newText", json!({"text": "x", "track": track, "frame": 0, "seconds": 1})).unwrap_err().to_string();
        assert!(e.contains(says), "{track}: {e}");
        assert!(Arc::ptr_eq(&before, &s.project), "{track}: nothing changed");
    }
    assert!(s.execute("graphics.newText", json!({"text": "x", "track": "V2", "frame": 0})).unwrap_err().to_string().contains("\"auto\""));
    s.execute("timeline.setTrack", json!({"track": "V3", "locked": true})).unwrap();
    let e = s.execute("graphics.newText", json!({"text": "x", "track": "V3", "frame": 0, "seconds": 1})).unwrap_err().to_string();
    assert!(e.contains("V3 is locked"), "{e}");
}

#[test]
fn a_template_takes_a_track_name_and_auto() {
    let mut s = graphics_session();
    let list = s.execute("graphics.template.list", json!({})).unwrap();
    let t = list.as_array().unwrap().first().unwrap()["id"].clone();
    let r = s.execute("graphics.template.apply", json!({"template": t, "track": "V2", "frame": 0})).unwrap();
    assert_eq!(track_index(&s, &r), 1);
    let r = s.execute("graphics.template.apply", json!({"template": t, "track": "auto", "frame": 0})).unwrap();
    assert_eq!(track_index(&s, &r), 2);
    let e = s.execute("graphics.template.apply", json!({"template": t, "track": "V2", "frame": 0})).unwrap_err().to_string();
    assert!(e.contains("V2 is not free here") && e.contains("graphics.template.apply"), "{e}");
}

/// Item 11 of the demo has sound only: `track:"new"` adds an audio track for it.
#[test]
fn place_on_a_new_track_follows_the_media_kind() {
    let mut s = demo();
    s.execute("file.newSequence", json!({"name": "Sound", "fps": 24, "video": 1, "audio": 1})).unwrap();
    let (nv, na) = (1, 1);
    let undo = s.history.undo.len();
    s.execute("timeline.place", json!({"item": 11, "track": "new", "time": 0})).unwrap();
    let q = s.active_sequence().unwrap();
    assert_eq!((q.video_tracks.len(), q.audio_tracks.len()), (nv, na + 1), "an audio track was added, no video track");
    assert_eq!(q.audio_tracks[1].items.len(), 1, "the sound is on the new track");
    assert!(q.audio_tracks[0].items.is_empty() && q.video_tracks[0].items.is_empty());
    assert_eq!(s.history.undo.len(), undo + 1, "one undo step");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active_sequence().unwrap().audio_tracks.len(), na);
    // an explicit `audioTrack` still says where the sound goes
    s.execute("timeline.place", json!({"item": 11, "track": "V1", "audioTrack": "A1", "time": 0})).unwrap();
    assert_eq!(s.active_sequence().unwrap().audio_tracks.len(), na);
}

#[test]
fn new_sequence_from_a_clip_can_take_only_its_settings() {
    let mut s = demo();
    let src = s.project.item(filmagic_project::ItemId(5)).unwrap().clone();
    let want = crate::commands::default_seq_settings_for(src.as_media().unwrap());
    let r = s.execute("file.newSequence", json!({"name": "Settings only", "fromItem": 5, "place": false})).unwrap();
    let q = s.project.sequence(filmagic_project::ItemId(r["sequence"].as_u64().unwrap())).unwrap();
    assert!(q.all_tracks().all(|t| t.items.is_empty()), "nothing placed");
    assert_eq!((q.settings.width, q.settings.height, q.settings.frame_rate), (want.width, want.height, want.frame_rate));
    // the default still places the clip
    let r = s.execute("file.newSequence", json!({"name": "With clip", "fromItem": 5})).unwrap();
    let q = s.project.sequence(filmagic_project::ItemId(r["sequence"].as_u64().unwrap())).unwrap();
    assert!(q.all_tracks().any(|t| !t.items.is_empty()));
    // hostile values change nothing
    let before = s.project.clone();
    for p in [
        json!({"fromItem": 5, "place": "no"}),
        json!({"fromItem": 5, "place": 0}),
        json!({"fromItem": 999_999_999u64, "place": false}),
        json!({"fromItem": r["sequence"], "place": false}),
    ] {
        assert!(s.execute("file.newSequence", p.clone()).is_err(), "{p}");
        assert!(Arc::ptr_eq(&before, &s.project), "{p}");
    }
}

// ---------------------------------------------------------------- review of #24 (checklist #33)

fn video_clips(s: &Session) -> Vec<TrackItem> {
    s.active_sequence().unwrap().video_tracks[0].items.clone()
}

/// The custom-colour dialog stores `{clips, sequence}` and runs `edit.label` later, maybe after
/// another sequence was opened: the clips are checked against that `sequence`.
#[test]
fn edit_label_checks_its_clips_against_the_sequence_it_names() {
    let mut s = demo();
    let seq = s.state.active_sequence.unwrap();
    let c = video_clips(&s)[0].id;
    s.execute("file.newSequence", json!({"name": "Other"})).unwrap();
    assert_ne!(s.state.active_sequence, Some(seq));
    s.execute("edit.label", json!({"label": "#336699", "clips": [c.0], "sequence": seq.0})).unwrap();
    let q = s.project.sequence(seq).unwrap();
    assert!(q.find_item(c).unwrap().1.custom_label_color.is_some(), "the clip of the named sequence got the colour");
    // an id that is no clip of that sequence is still refused, and named against it
    let why = refused(s.execute("edit.label", json!({"label": "#336699", "clips": [987_654_321u64], "sequence": seq.0})));
    assert!(why.contains("no clip 987654321 in sequence"), "{why}");
}

/// A caption id is no clip: commands on clips refuse it instead of skipping it or failing with
/// "track is locked".
#[test]
fn a_caption_id_is_refused_by_commands_on_clips() {
    let mut s = demo();
    let (v, a) = linked_pair(&s);
    let cap = s.execute("captions.add", json!({"time": {"seconds": 1.0}, "text": "hi"})).unwrap()["caption"].as_u64().unwrap();
    let before = s.project.clone();
    let why = refused(s.execute("clip.speedDuration", json!({"clips": [v.id.0, cap], "speed": 200})));
    assert!(why.contains(&format!("{cap} is a caption on caption track C1")) && !why.contains("locked"), "{why}");
    s.execute("essentialSound.setType", json!({"clips": [a.id.0], "type": "dialogue"})).unwrap();
    let typed = s.project.clone();
    let why = refused(s.execute("essentialSound.set", json!({"clips": [a.id.0, cap], "key": "repair.noise.on", "value": true})));
    assert!(why.contains("is a caption"), "{why}");
    assert!(Arc::ptr_eq(&typed, &s.project), "nothing changed");
    assert!(!Arc::ptr_eq(&before, &typed));
    // and a caption track's id is named as one
    let track = s.active_sequence().unwrap().caption_tracks[0].id.0;
    let why = refused(s.execute("clip.enable", json!({"clips": [track]})));
    assert!(why.starts_with(&format!("{track} is the id of caption track C1")), "{why}");
}

/// Run by a command that calls another (no id check at that depth), a missing clip is reported
/// as missing, not as on a locked track.
#[test]
fn speed_duration_reports_a_missing_clip_as_missing() {
    let mut s = demo();
    let v = video_clips(&s).last().unwrap().id;
    let run = crate::commands::find("clip.speedDuration").unwrap().run;
    let e = run(&mut s, &json!({"clips": [v.0, 987_654_321u64], "speed": 200})).unwrap_err().to_string();
    assert!(!e.contains("locked"), "{e}");
    assert!(e.contains("987654321"), "{e}");
}

/// The id check runs before the time values are read, so a link id named as the clip of a
/// source-time parameter gets the link message, not one about the clock it was read on.
#[test]
fn the_clip_check_runs_before_the_time_values_are_read() {
    let mut s = demo();
    let (v, _) = linked_pair(&s);
    let link = v.link.unwrap();
    let why =
        refused(s.execute("effects.deleteKeyframe", json!({"clip": link, "effect": "opacity", "param": "opacity", "mediaTime": {"timecode": "00:00:00:05"}})));
    assert!(why.starts_with(&format!("{link} is a link id")), "{why}");
}

/// Each `clips[]` entry says where its own keyframe went; the `ease` error names the clip.
#[test]
fn set_param_on_many_clips_reports_each_clips_media_time() {
    let mut s = demo();
    let clips = video_clips(&s);
    let (a, b) = (&clips[0], &clips[1]);
    let r =
        s.execute("effects.setParam", json!({"clips": [a.id.0, b.id.0], "effect": "opacity", "param": "opacity", "value": 40.0, "keyframe": true})).unwrap();
    let entries = r["clips"].as_array().unwrap();
    let mt = |e: &Value| e["mediaTime"].as_i64().unwrap();
    // the playhead is pinned to each clip: their media times differ
    let want = |c: &TrackItem| c.effect_time_at(s.playhead().clamp(c.start, (c.end() - Tick(1)).max(c.start))).0;
    assert_eq!((mt(&entries[0]), mt(&entries[1])), (want(a), want(b)), "{r}");
    assert!(entries.iter().all(|e| e["mediaSeconds"].is_number()), "{r}");
    let why =
        refused(s.execute("effects.setParam", json!({"clips": [a.id.0, b.id.0], "effect": "motion", "param": "scale", "value": 50.0, "ease": "easeOut"})));
    assert!(why.contains(&format!("of clip {} is not animated", a.id.0)), "{why}");
}

/// Sound only, `"track":"new"` and an existing `audioTrack`: the sound goes on that track.
#[test]
fn place_sound_on_an_existing_audio_track_with_track_new() {
    let mut s = demo();
    s.execute("file.newSequence", json!({"name": "Sound", "fps": 24, "video": 1, "audio": 2})).unwrap();
    s.execute("timeline.place", json!({"item": 11, "track": "new", "audioTrack": "A2", "time": 0})).unwrap();
    let q = s.active_sequence().unwrap();
    assert_eq!((q.video_tracks.len(), q.audio_tracks.len()), (1, 2), "no track added");
    assert_eq!(q.audio_tracks[1].items.len(), 1, "the sound is on A2");
}

/// The shape #16 gives `sequence.inspect` (`"item":id?,"clip":id|[id]?`, a query): the clips are
/// checked against the sequence `item` names, `clip` takes a list, and a query's refusal does not
/// say "Nothing was changed".
#[test]
fn a_query_checks_clips_against_the_sequence_it_inspects() {
    use crate::commands::{CommandSpec, NamedKeys};
    let mut s = demo();
    let seq = s.state.active_sequence.unwrap();
    let c = video_clips(&s)[0].id;
    s.execute("file.newSequence", json!({"name": "Other"})).unwrap();
    let spec = CommandSpec {
        id: "sequence.inspect",
        label: "Inspect Sequence",
        menu: &[],
        shortcut: None,
        params: r#"{"item":id?,"clip":id|[id]?}"#,
        enabled: |_| Ok(()),
        run: |_, _| Ok(Value::Null),
        journal: false,
        named: NamedKeys::CLIP,
    };
    let check = |s: &Session, p: Value| crate::clip_ids::check_named_clips(s, &spec, &p);
    check(&s, json!({"item": seq.0, "clip": c.0})).unwrap();
    check(&s, json!({"item": seq.0, "clip": [c.0]})).unwrap();
    let e = check(&s, json!({"item": seq.0, "clip": [c.0, 987_654_321u64]})).unwrap_err().to_string();
    assert!(e.contains("no clip 987654321 in sequence") && !e.contains("Nothing was changed"), "{e}");
    // without `item`, the active sequence: the clip is elsewhere
    let e = check(&s, json!({"clip": [c.0]})).unwrap_err().to_string();
    assert!(e.contains("not the active one"), "{e}");
}

/// `metadata.*` reads `item` instead of `clip`: with an `item`, a `clip` is not checked.
#[test]
fn item_instead_of_clip_is_not_checked() {
    let mut s = demo();
    let item = video_clips(&s)[0].item.0;
    s.execute("metadata.set", json!({"item": item, "clip": 0, "field": "Scene", "value": "12"})).unwrap();
    assert!(refused(s.execute("metadata.set", json!({"clip": 0, "field": "Scene", "value": "12"}))).starts_with("no clip 0"));
}

#[test]
fn small_wordings() {
    let mut s = demo();
    let clips = video_clips(&s);
    // a whole number written as a float is refused with the integer to send
    let c = clips[0].id.0;
    let why = refused(s.execute("clip.enable", json!({"clips": [c as f64]})));
    assert!(why.contains(&format!("pass {c}")), "{why}");
    // a transition at a clip's edge names that edge
    let last = clips.last().unwrap().id;
    s.execute("sequence.applyVideoTransition", json!({"clip": last.0, "edge": "out", "frames": 10})).unwrap();
    let tr = s.active_sequence().unwrap().video_tracks[0].transitions.iter().find(|t| t.from == Some(last) && t.to.is_none()).unwrap().id.0;
    let why = refused(s.execute("clip.enable", json!({"clips": [tr]})));
    assert!(why.contains(&format!("at the end of clip {}", last.0)), "{why}");
    // a sequence with no video tracks says so
    s.execute("file.newSequence", json!({"name": "Sound only", "video": 0, "audio": 1})).unwrap();
    let e = s.execute("graphics.newText", json!({"text": "x", "track": "V1"})).unwrap_err().to_string();
    assert!(e.contains("this sequence has no video tracks"), "{e}");
    // a track index written as a float
    let e = s.execute("graphics.newText", json!({"text": "x", "track": 1.0})).unwrap_err().to_string();
    assert!(e.contains("pass 1 (or \"V2\")"), "{e}");
}

#[test]
fn command_list_ranks_matches_and_refuses_a_filter_that_is_no_text() {
    let mut s = demo();
    let ids = |s: &mut Session, f: &str| -> Vec<String> {
        s.execute("command.list", json!({"filter": f})).unwrap().as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap().to_string()).collect()
    };
    let hits = ids(&mut s, "paste attributes");
    assert_eq!(hits.first().map(String::as_str), Some("edit.pasteAttributes"), "{hits:?}");
    // commands named for the word come before ones that only mention it in their params
    let clip = ids(&mut s, "speed");
    let first_param_only = clip.iter().position(|id| !id.to_lowercase().contains("speed")).unwrap_or(clip.len());
    assert!(clip[..first_param_only].iter().all(|id| id.to_lowercase().contains("speed")), "{clip:?}");
    assert!(clip[first_param_only..].iter().all(|id| !id.to_lowercase().contains("speed")), "{clip:?}");
    for bad in [json!(3), json!(["a"]), json!({"a": 1})] {
        assert!(refused(s.execute("command.list", json!({"filter": bad}))).contains("`filter` must be a string"), "{bad}");
    }
}

/// `place:false` takes a subclip's settings, and refuses `fromItems`.
#[test]
fn new_sequence_settings_only_takes_a_subclip_and_one_item() {
    let mut s = demo();
    let sub = s.execute("clip.makeSubclip", json!({"item": 5, "startFrame": 10, "endFrame": 60})).unwrap()["item"].as_u64().unwrap();
    let r = s.execute("file.newSequence", json!({"name": "From subclip", "fromItem": sub, "place": false})).unwrap();
    let q = s.project.sequence(filmagic_project::ItemId(r["sequence"].as_u64().unwrap())).unwrap();
    let src = s.project.item(filmagic_project::ItemId(5)).unwrap();
    let want = crate::commands::default_seq_settings_for(src.as_media().unwrap());
    assert_eq!((q.settings.width, q.settings.height, q.settings.frame_rate), (want.width, want.height, want.frame_rate));
    assert!(q.all_tracks().all(|t| t.items.is_empty()));
    let why = refused(s.execute("file.newSequence", json!({"fromItem": 5, "fromItems": [5], "place": false})));
    assert!(why.contains("without `fromItems`"), "{why}");
}

// ---------------------------------------------------------------- re-review of #24 (d4cf229)

/// A list under `clip` for a command that reads one id would be ignored (a razor without its
/// clip cuts every track): it is refused with the id to send.
#[test]
fn a_list_under_clip_is_refused_where_clip_takes_one_id() {
    let mut s = demo();
    let (v, _) = linked_pair(&s);
    let before = s.project.clone();
    let at = json!({"seconds": (v.start.0 as f64 / filmagic_time::TICKS_PER_SECOND as f64) + 0.5});
    let why = refused(s.execute("timeline.razor", json!({"time": at, "clip": [v.id.0]})));
    assert!(why.starts_with(&format!("`clip` takes one id here, not a list; pass {}", v.id.0)), "{why}");
    assert!(Arc::ptr_eq(&before, &s.project), "nothing was cut");
    let why = refused(s.execute("clip.revealInProject", json!({"clip": [v.id.0]})));
    assert!(why.contains("`clip` takes one id here"), "{why}");
    // the single id still works
    s.execute("timeline.razor", json!({"time": at, "clip": v.id.0})).unwrap();
    assert!(!Arc::ptr_eq(&before, &s.project));
}

/// `item` moves the check to another sequence only for `sequence.inspect`; `sequence` only for a
/// command that documents it; `source.open` reads `clip` before `item`, so its `clip` is checked.
#[test]
fn item_and_sequence_move_the_check_only_where_the_command_says_so() {
    let mut s = demo();
    let c = video_clips(&s)[0].id.0;
    let (v, _) = linked_pair(&s);
    let link = v.link.unwrap();
    let seq = s.state.active_sequence.unwrap();
    let other = s.execute("file.newSequence", json!({"name": "Other"})).unwrap()["sequence"].as_u64().unwrap();
    s.execute("sequence.open", json!({"item": seq.0})).unwrap();
    let check = |s: &Session, id: &str, p: Value| crate::clip_ids::check_named_clips(s, crate::commands::find(id).unwrap(), &p);
    // speechSegments reads `clip` first: a clip of the active sequence passes beside another sequence's item
    check(&s, "audio.speechSegments", json!({"clip": c, "item": other})).unwrap();
    // `sequence` on a command that does not document it does not move the check
    check(&s, "clip.enable", json!({"clips": [c], "sequence": other})).unwrap();
    // source.open reads `clip` first: a link id there is refused even with an `item`
    let e = check(&s, "source.open", json!({"clip": link, "item": v.item.0})).unwrap_err().to_string();
    assert!(e.contains(&format!("{link} is a link id")), "{e}");
}

/// One timeline `time` for several clips: each clip not under it is named in `warnings`.
#[test]
fn set_param_warns_for_clips_outside_the_time() {
    let mut s = demo();
    let clips = video_clips(&s);
    let (a, b) = (&clips[0], &clips[1]);
    let inside_a = a.start + Tick((a.duration.0 / 2).max(1));
    let r = s
        .execute(
            "effects.setParam",
            json!({"clips": [a.id.0, b.id.0], "effect": "opacity", "param": "opacity", "value": 40.0, "keyframe": true, "time": inside_a.0}),
        )
        .unwrap();
    let w = r["warnings"].as_array().unwrap();
    assert_eq!(w.len(), 1, "{r}");
    assert!(w[0].as_str().unwrap().contains(&format!("is outside clip {}", b.id.0)), "{r}");
    // every clip under the time: no warning
    let r = s
        .execute("effects.setParam", json!({"clips": [a.id.0], "effect": "opacity", "param": "opacity", "value": 50.0, "keyframe": true, "time": inside_a.0}))
        .unwrap();
    assert!(r.get("warnings").is_none(), "{r}");
}
