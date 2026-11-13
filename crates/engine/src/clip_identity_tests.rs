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
