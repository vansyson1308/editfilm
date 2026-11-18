//! #13: every time-valued parameter takes ticks or `{"seconds"|"frame"|"timecode"}`, keyframes take
//! timeline time, and a graphic past the end of the picture is reported.

use filmagic_project::{ClipId, Interpolation};
use filmagic_time::{FrameRate, TICKS_PER_SECOND, Tick};
use serde_json::{Value, json};

use crate::Session;
use crate::commands::command_specs;
use crate::time_params::{Base, Clock, TimeKey, keys_of, normalize, registry, value_ticks};

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

fn sec(x: f64) -> i64 {
    Tick::from_seconds_f64(x).0
}

fn first_clip(s: &Session) -> filmagic_project::TrackItem {
    s.active_sequence().unwrap().video_tracks[0].items[0].clone()
}

/// Params holding `leaf` under `k`, inside the containers of its path.
fn params_with(k: &TimeKey, leaf: Value) -> Value {
    let leaf = if k.positional { json!([[leaf, 0.5]]) } else { leaf };
    let mut v = json!({ k.key.clone(): leaf });
    for (name, array) in k.path.iter().rev() {
        v = if *array { json!({ name.clone(): [v] }) } else { json!({ name.clone(): v }) };
    }
    v
}

fn leaf<'a>(k: &TimeKey, v: &'a Value) -> &'a Value {
    let mut v = v;
    for (name, array) in &k.path {
        v = &v[name.as_str()];
        if *array {
            v = &v[0];
        }
    }
    let v = &v[k.key.as_str()];
    if k.positional { &v[0][0] } else { v }
}

/// The registry-wide guard: every command whose params doc mentions ticks declares its time keys
/// in a form the reader finds, and every one of those keys takes `{"seconds": …}`. A new command
/// with a ticks parameter is covered without a line here.
#[test]
fn every_ticks_parameter_of_every_command_accepts_seconds() {
    let s = demo();
    let mut checked = 0;
    for c in command_specs() {
        if !c.params.contains("tick") && !c.params.contains("Tick") {
            continue;
        }
        let keys = registry().get(c.id).cloned().unwrap_or_default();
        // every `:ticks`-like type in the doc, however it is spelled (`: ticks`, `:Ticks`, `:[[ticks`),
        // must be a key the reader found: a misspelt doc key would otherwise go unchecked
        let declared = declared_tick_types(c.params);
        assert_eq!(declared, keys.len(), "{}: the doc types {declared} parameters as ticks, the time reader found {keys:?}: {}", c.id, c.params);
        for k in &keys {
            let p = params_with(k, json!({"seconds": 1.5}));
            let n = normalize(&s, c.id, p.clone()).unwrap_or_else(|e| panic!("{} `{}`: {e} ({p})", c.id, k.key));
            assert_eq!(leaf(k, &n), &json!(sec(1.5)), "{} `{}` takes seconds: {p} -> {n}", c.id, k.key);
            // and integer ticks are left as they are
            let p = params_with(k, json!(12345));
            assert_eq!(normalize(&s, c.id, p.clone()).unwrap(), p, "{} `{}`: ticks unchanged", c.id, k.key);
            checked += 1;
        }
    }
    assert!(checked >= 100, "only {checked} time parameters found: the doc reader lost some");
}

/// How many values a params doc types as ticks: a `tick` word right after a `:` (spaces and `[`
/// allowed between), in any letter case.
fn declared_tick_types(doc: &str) -> usize {
    let lower = doc.to_ascii_lowercase();
    lower
        .match_indices("tick")
        .filter(|(i, _)| {
            // the start of the word holding `tick` (`mediaticks`, `timelineticks`)
            let word = lower[..*i].trim_end_matches(|c: char| c.is_ascii_alphabetic());
            word.trim_end_matches([' ', '[']).ends_with(':')
        })
        .count()
}

#[test]
fn the_commands_the_issue_names_have_their_time_keys_and_bases() {
    let has = |id: &str, key: &str, base: Base| {
        let keys = registry().get(id).unwrap_or_else(|| panic!("{id} has no time keys"));
        assert!(keys.iter().any(|k| k.key == key && k.base == base), "{id} `{key}` ({base:?}) in {keys:?}");
    };
    has("timeline.razor", "time", Base::Sequence);
    has("graphics.template.apply", "time", Base::Sequence);
    has("effects.setParam", "time", Base::Sequence);
    has("effects.setInterpolation", "mediaTime", Base::Source);
    has("effects.setInterpolation", "time", Base::Source); // `keyframes[].time`
    has("effects.setInterpolation", "time", Base::Sequence);
    has("effects.setKeyframe", "time", Base::Sequence);
    has("effects.moveKeyframes", "delta", Base::Offset);
    has("markers.add", "time", Base::Sequence);
    has("markers.addRange", "duration", Base::Length);
    has("clip.frameHold", "time", Base::Sequence);
    has("captions.add", "time", Base::Sequence);
    has("captions.trim", "delta", Base::Offset);
    has("project.setMarks", "in", Base::Source);
    has("transcript.set", "start", Base::Source);
    has("graphics.setRoll", "preroll", Base::Length);
    has("sequence.setTransition", "start", Base::Sequence);
    let points = &registry()["mixer.writeAutomation"];
    assert!(points.iter().any(|k| k.key == "points" && k.positional), "{points:?}");
    // keys with their own object form are normalized too, so they get the same checks
    has("timeline.place", "sourceIn", Base::Source);
    has("timeline.place", "duration", Base::Length);
    has("timeline.move", "time", Base::Sequence);
    has("source.setPlayhead", "time", Base::Source);
}

#[test]
fn the_doc_reader_follows_nesting() {
    let k = keys_of(r#"{"keyframes":[{"clip":id,"time":mediaTicks}],"delta":timelineTicks}"#);
    assert_eq!(k.len(), 2, "{k:?}");
    assert_eq!((k[0].path.clone(), k[0].key.as_str(), k[0].base), (vec![("keyframes".to_string(), true)], "time", Base::Source));
    assert_eq!((k[1].path.len(), k[1].base), (0, Base::Offset));
    let k = keys_of(r#"{"item":id,"transcript":{"words":[{"text":str,"start":mediaTicks,"end":mediaTicks}]}}"#);
    assert_eq!(k[0].path, vec![("transcript".to_string(), false), ("words".to_string(), true)]);
    assert_eq!(keys_of(r#"{"time":ticks|{"seconds":f}}"#).len(), 1, "a documented object form is normalized too");
    assert_eq!(keys_of(r#"{"in":ticks?|null}"#).len(), 1);
    // the guard in the registry test sees a misspelt key the reader misses
    for typo in [r#"{"time": ticks}"#, r#"{"time":Ticks}"#, r#"{"time": [[ ticks,value]]}"#] {
        assert_eq!((declared_tick_types(typo), keys_of(typo).len()), (1, 0), "{typo}");
    }
    assert_eq!(declared_tick_types(r#"{"a":mediaTicks,"b":timelineTicks,"p":[[ticks,v]]} (or ticks as intro)"#), 3);
}

#[test]
fn seconds_frames_and_timecode_convert_at_the_right_rate() {
    let rate = FrameRate::new(30, 1);
    let clock = || Some(Clock::new(rate, false, Tick::ZERO));
    let t = |v: Value| value_ticks("x", "time", &v, Base::Sequence, clock);
    assert_eq!(t(json!({"seconds": 52.5})).unwrap(), Some(sec(52.5)));
    assert_eq!(t(json!({"frame": 30})).unwrap(), Some(TICKS_PER_SECOND));
    assert_eq!(t(json!({"frames": 15.0})).unwrap(), Some(TICKS_PER_SECOND / 2));
    assert_eq!(t(json!({"timecode": "00:00:52:15"})).unwrap(), Some(sec(52.5)));
    assert_eq!(t(json!({"ticks": 7})).unwrap(), Some(7));
    assert_eq!(t(json!(7)).unwrap(), Some(7), "ticks as before");
    assert_eq!(t(json!(-7)).unwrap(), Some(-7), "an integer tick is the command's to judge, as before");
    assert_eq!(t(json!(3.0)).unwrap(), Some(3), "a whole float is that many ticks");
    assert!(t(json!(2.6)).unwrap_err().to_string().contains("whole number of ticks"), "a fraction of a tick is refused");
    assert_eq!(t(Value::Null).unwrap(), None);
    // a relative timecode counts from the playhead
    let at = |v: Value| value_ticks("x", "time", &v, Base::Sequence, || Some(Clock::new(rate, false, Tick::from_seconds_f64(10.0))));
    assert_eq!(at(json!({"timecode": "+30"})).unwrap(), Some(sec(11.0)));
    // an offset may be negative
    assert_eq!(value_ticks("x", "delta", &json!({"seconds": -0.5}), Base::Offset, clock).unwrap(), Some(sec(-0.5)));
}

#[test]
fn hostile_time_values_are_refused_with_a_reason() {
    let rate = FrameRate::new(25, 1);
    let clock = || Some(Clock::new(rate, false, Tick::ZERO));
    let err = |v: Value, base: Base| value_ticks("cmd", "time", &v, base, clock).unwrap_err().to_string();
    for (v, why) in [
        (json!({"seconds": 1e300}), "beyond the time range"),
        (json!({"seconds": -1e300}), "beyond the time range"),
        (json!({"seconds": 4.0e7}), "beyond the time range"),
        (json!({"seconds": "NaN"}), "must be a number"),
        (json!({"seconds": -0.5}), "must not be negative"),
        (json!({"frame": -1}), "must not be negative"),
        (json!({"frame": 1.5}), "whole number"),
        (json!({"frame": i64::MAX}), "beyond the time range"),
        (json!({"frame": 1e30}), "whole number"),
        (json!({"timecode": "ab:cd"}), "not a timecode"),
        (json!({"timecode": ""}), "not a timecode"),
        (json!({"timecode": "1".repeat(65)}), "longer than 64"),
        (json!({"timecode": 12}), "must be text"),
        (json!({"seconds": 1, "frame": 2}), "exactly one"),
        (json!({}), "exactly one"),
        (json!({"second": 1}), "unknown unit"),
        (json!({"ticks": 1e300}), "integer in the time range"),
        (json!({"ticks": 0.5}), "integer in the time range"),
        (json!(u64::MAX), "beyond the time range"),
        (json!(1e300), "beyond the time range"),
        (json!(-1.5), "whole number of ticks"),
        (json!(-2.0), "must not be negative"),
        (json!("00:00:01:00"), "must be ticks"),
        (json!(true), "must be ticks"),
        (json!([1]), "must be ticks"),
    ] {
        let e = err(v.clone(), Base::Sequence);
        assert!(e.contains(why), "{v}: {e}");
        assert!(e.contains("`time`"), "the error names the parameter: {e}");
    }
    // a frame rate of zero, or no rate at all, cannot count frames: seconds still work
    for clock in [Some(Clock::new(FrameRate { num: 0, den: 1 }, false, Tick::ZERO)), Some(Clock::new(FrameRate { num: 25, den: 0 }, false, Tick::ZERO)), None] {
        let e = value_ticks("cmd", "time", &json!({"frame": 3}), Base::Sequence, || clock).unwrap_err().to_string();
        assert!(e.contains("seconds"), "{e}");
        assert!(value_ticks("cmd", "time", &json!({"timecode": "00:00:01:00"}), Base::Sequence, || clock).is_err());
        assert_eq!(value_ticks("cmd", "time", &json!({"seconds": 2}), Base::Sequence, || clock).unwrap(), Some(sec(2.0)));
    }
}

#[test]
fn hostile_time_values_fail_the_command_without_changing_the_project() {
    let mut s = demo();
    let before = s.project.clone();
    for (cmd, p) in [
        ("timeline.razor", json!({"time": {"seconds": f64::MAX}})),
        ("timeline.razor", json!({"time": {"seconds": -2.0}})),
        ("markers.add", json!({"time": {"timecode": "nope"}})),
        ("markers.addRange", json!({"duration": {"seconds": -1.0}})),
        ("clip.frameHold", json!({"time": "5 s"})),
        ("captions.trim", json!({"caption": 1, "edge": "out", "delta": {"frame": 0.5}})),
        ("effects.setParam", json!({"clip": first_clip(&s).id.0, "param": "scale", "value": 50.0, "time": {"seconds": 1e20}})),
        ("mixer.writeAutomation", json!({"strip": "A1", "points": [[{"seconds": -3.0}, 0.5]]})),
    ] {
        let e = s.execute(cmd, p.clone()).unwrap_err().to_string();
        assert!(e.contains(cmd), "{cmd} {p}: {e}");
        assert!(std::sync::Arc::ptr_eq(&before, &s.project) || *before == *s.project, "{cmd}: nothing changed");
    }
}

/// The issue's case: a tag meant for 52.5 s lands at 52.5 s, however the time is written.
#[test]
fn markers_land_where_seconds_frames_or_timecode_say() {
    let mut s = demo();
    let rate = s.sequence_rate();
    let at = |s: &Session, id: &Value| s.active_sequence().unwrap().markers.iter().find(|m| json!(m.id.0) == *id).unwrap().start;
    let a = s.execute("markers.add", json!({"time": {"seconds": 52.5}, "name": "SLOW-MO"})).unwrap()["marker"].clone();
    assert_eq!(at(&s, &a), Tick::from_seconds_f64(52.5));
    let b = s.execute("markers.add", json!({"time": {"frame": 40}})).unwrap()["marker"].clone();
    assert_eq!(at(&s, &b), rate.tick_of(40));
    let tc = filmagic_time::format_timecode_frames(72, rate, false);
    let c = s.execute("markers.add", json!({"time": {"timecode": tc}})).unwrap()["marker"].clone();
    assert_eq!(at(&s, &c), rate.tick_of(72));
    // the old integer form is unchanged
    let d = s.execute("markers.add", json!({"time": rate.tick_of(12).0})).unwrap()["marker"].clone();
    assert_eq!(at(&s, &d), rate.tick_of(12));
    // a range marker's length in seconds
    let r = s.execute("markers.addRange", json!({"time": {"seconds": 1.0}, "duration": {"seconds": 2.0}})).unwrap()["marker"].clone();
    let m = s.active_sequence().unwrap().markers.iter().find(|m| json!(m.id.0) == r).unwrap().clone();
    assert_eq!(m.duration, Tick::from_seconds_f64(2.0));
}

#[test]
fn razor_frame_hold_and_captions_take_seconds() {
    let mut s = demo();
    let c = first_clip(&s);
    let cut = c.start + Tick(c.duration.0 / 2);
    let cut = s.sequence_rate().snap(cut);
    let r = s.execute("timeline.razor", json!({"clip": c.id.0, "time": {"seconds": cut.seconds()}})).unwrap();
    assert!(r["cuts"].as_u64().unwrap() >= 1, "{r}");
    let q = s.active_sequence().unwrap();
    assert!(q.video_tracks[0].items.iter().any(|i| i.start == cut || i.end() == cut), "the cut is at {cut:?}");

    let mut s = demo();
    let c = first_clip(&s);
    let at = c.start + s.sequence_rate().tick_of(5);
    let r = s.execute("clip.frameHold", json!({"clips": [c.id.0], "time": {"seconds": at.seconds()}})).unwrap();
    let held_clip = ClipId(r["clips"][0].as_u64().unwrap());
    let (_, held_item) = s.active_sequence().unwrap().find_item(held_clip).unwrap();
    assert_eq!(held_item.start, at, "the hold starts at the time asked for");
    let held = held_item.frame_hold;
    assert_eq!(held, Some(c.moving_source_time_at(at)), "held at the frame asked for, not the playhead");

    let mut s = demo();
    let id = s.execute("captions.add", json!({"time": {"seconds": 2.0}, "text": "hi"})).unwrap()["caption"].as_u64().unwrap();
    let cap = |s: &Session| s.active_sequence().unwrap().caption_tracks[0].captions.iter().find(|c| c.id.0 == id).unwrap().clone();
    assert_eq!(cap(&s).start, s.sequence_rate().snap(Tick::from_seconds_f64(2.0)));
    let d0 = cap(&s).duration;
    s.execute("captions.trim", json!({"caption": id, "edge": "out", "delta": {"seconds": -1.0}})).unwrap();
    assert_eq!(cap(&s).duration, d0 - Tick::from_seconds_f64(1.0));
}

#[test]
fn a_template_applies_at_a_time_in_seconds() {
    let mut s = demo();
    let id = s.execute("graphics.template.list", json!({"source": "builtin"})).unwrap()[0]["id"].as_str().unwrap().to_string();
    let r = s.execute("graphics.template.apply", json!({"template": id, "time": {"seconds": 4.0}})).unwrap();
    let clip = ClipId(r["clip"].as_u64().unwrap());
    assert_eq!(s.active_sequence().unwrap().find_item(clip).unwrap().1.start, s.sequence_rate().snap(Tick::from_seconds_f64(4.0)));
}

/// Keyframes by timeline time: the engine finds the clip's media time through its speed.
#[test]
fn keyframes_set_and_eased_by_timeline_seconds_on_a_sped_up_clip() {
    let mut s = demo();
    let c = first_clip(&s);
    s.execute("clip.speedDuration", json!({"clips": [c.id.0], "speed": 200})).unwrap();
    let it = s.active_sequence().unwrap().find_item(c.id).unwrap().1.clone();
    assert_eq!(it.speed, 2.0);
    let t1 = it.start.seconds() + 0.5;
    let t2 = it.start.seconds() + 1.0;
    // one call: the value, its keyframe and its easing
    let r = s
        .execute("effects.setParam", json!({"clip": c.id.0, "param": "scale", "value": 100.0, "keyframe": true, "time": {"seconds": t1}, "ease": "easeOut"}))
        .unwrap();
    assert_eq!(r["ease"], "easeOut", "{r}");
    let mt1 = it.effect_time_at(Tick::from_seconds_f64(t1));
    assert_eq!(r["mediaTime"], mt1.0, "timeline time went through the speed");
    s.execute("effects.setParam", json!({"clip": c.id.0, "param": "scale", "value": 150.0, "keyframe": true, "time": {"seconds": t2}})).unwrap();
    let keys = |s: &Session| s.active_sequence().unwrap().find_item(c.id).unwrap().1.effect("motion").unwrap().params["scale"].keyframes.clone();
    assert_eq!(keys(&s)[0].interp, Interpolation::EaseOut);
    // ease the second one by its timeline time, a little off the frame (nearest, within half a frame)
    let off = t2 + 0.2 / s.sequence_rate().as_f64();
    let r = s.execute("effects.setInterpolation", json!({"clip": c.id.0, "param": "scale", "time": {"seconds": off}, "interpolation": "easeIn"})).unwrap();
    assert_eq!(r["mediaTime"], keys(&s)[1].time.0, "{r}");
    assert_eq!(keys(&s)[1].interp, Interpolation::EaseIn);
    // edit its value by timeline time too
    s.execute("effects.setKeyframe", json!({"clip": c.id.0, "param": "scale", "time": {"seconds": t2}, "value": 175.0, "interpolation": "hold"})).unwrap();
    assert_eq!(keys(&s)[1].value, filmagic_project::ParamValue::Float(175.0));
    assert_eq!(keys(&s)[1].interp, Interpolation::Hold);
    // and delete it by timeline time
    let r = s.execute("effects.deleteKeyframe", json!({"clip": c.id.0, "param": "scale", "time": {"seconds": t2}})).unwrap();
    assert_eq!(r["removed"], true, "{r}");
    assert_eq!(keys(&s).len(), 1);
    // media time still works exactly, as before
    let m = keys(&s)[0].time.0;
    s.execute("effects.setInterpolation", json!({"clip": c.id.0, "param": "scale", "mediaTime": m, "interpolation": "linear"})).unwrap();
    assert_eq!(keys(&s)[0].interp, Interpolation::Linear);
}

#[test]
fn ease_and_interpolation_names_are_checked() {
    let mut s = demo();
    let c = first_clip(&s).id.0;
    let e = s.execute("effects.setParam", json!({"clip": c, "param": "scale", "value": 90.0, "ease": "wobbly", "keyframe": true})).unwrap_err().to_string();
    assert!(e.contains("easeIn"), "lists the easings: {e}");
    let e = s.execute("effects.setParam", json!({"clip": c, "param": "scale", "value": 90.0, "ease": 3, "keyframe": true})).unwrap_err().to_string();
    assert!(e.contains("`ease`"), "{e}");
    // ease on a static value needs a keyframe, and nothing changes
    let before = s.project.clone();
    let e = s.execute("effects.setParam", json!({"clip": c, "param": "scale", "value": 90.0, "ease": "easeIn"})).unwrap_err().to_string();
    assert!(e.contains("\"keyframe\": true"), "{e}");
    assert_eq!(*before, *s.project);
    s.execute("effects.setParam", json!({"clip": c, "param": "scale", "value": 90.0, "keyframe": true})).unwrap();
    let e = s.execute("effects.setInterpolation", json!({"clip": c, "param": "scale", "interpolation": "bouncy"})).unwrap_err().to_string();
    assert!(e.contains("`interpolation` must be one of"), "{e}");
    // a keyframe that is not there is said so, with where the keyframes are
    let e = s.execute("effects.setKeyframe", json!({"clip": c, "param": "scale", "time": {"seconds": 0.0}, "value": 1.0})).unwrap_err().to_string();
    assert!(e.contains("no keyframe") && e.contains("effects.list"), "{e}");
    let e = s.execute("effects.deleteKeyframe", json!({"clip": c, "param": "scale"})).unwrap_err().to_string();
    assert!(e.contains("`mediaTime`") && e.contains("`time`"), "{e}");
}

/// The issue's end title: 81.5 s in an 80.5 s video is reported, with the trim that fixes it.
#[test]
fn a_graphic_past_the_last_picture_frame_is_reported() {
    let mut s = demo();
    let q = s.active_sequence().unwrap();
    let rate = q.settings.frame_rate;
    let end = q.video_tracks.iter().flat_map(|t| &t.items).map(|i| i.end()).max().unwrap();
    let start = end - Tick::from_seconds_f64(4.0);
    let r = s.execute("graphics.newText", json!({"text": "THE END", "time": start.0, "seconds": 5.0})).unwrap();
    let over = &r["pastSequenceEnd"][0];
    let placed = s.active_sequence().unwrap().find_item(ClipId(r["clip"].as_u64().unwrap())).unwrap().1.end();
    assert_eq!(over["over"], (placed - end).0, "{r}");
    assert!((over["overSeconds"].as_f64().unwrap() - 1.0).abs() < 0.05, "about the second asked for: {r}");
    assert_eq!(over["overFrames"], rate.frame_at(placed - end + rate.frame_duration() - Tick(1)), "{r}");
    assert_eq!(over["pictureEnd"], end.0);
    let w = r["warnings"][0].as_str().unwrap();
    assert!(w.contains("after the last picture frame") && w.contains("timeline.trim"), "{w}");
    // the fix the warning gives works as written
    let clip = r["clip"].as_u64().unwrap();
    s.execute("timeline.trim", json!({"clip": clip, "edge": "out", "mode": "regular", "delta": -over["over"].as_i64().unwrap()})).unwrap();
    assert_eq!(s.active_sequence().unwrap().find_item(ClipId(clip)).unwrap().1.end(), end);

    // within the picture: no warning
    let r = s.execute("graphics.newText", json!({"text": "inside", "time": {"seconds": 1.0}, "seconds": 1.0})).unwrap();
    assert!(r.get("pastSequenceEnd").is_none() && r.get("warnings").is_none(), "{r}");

    // a caption too
    let r = s.execute("captions.add", json!({"time": (end - Tick::from_seconds_f64(1.0)).0, "durationSeconds": 3.0})).unwrap();
    assert_eq!(r["pastSequenceEnd"][0]["kind"], "caption", "{r}");
    assert!(r["warnings"][0].as_str().unwrap().contains("captions.trim"), "{r}");
    assert!(r["caption"].is_u64(), "the result keeps its fields");
}

#[test]
fn project_inspect_reports_durations_in_seconds_and_frames() {
    let s = demo();
    let v = crate::commands::inspect_project(&s);
    fn items(v: &Value, out: &mut Vec<Value>) {
        for c in v["children"].as_array().into_iter().flatten() {
            if c.get("item").is_some() {
                out.push(c.clone());
            }
            items(c, out);
        }
    }
    let mut all = Vec::new();
    items(&v["root"], &mut all);
    let media = all.iter().find(|i| i["duration"].as_i64().unwrap_or(0) > 0 && i["durationFrames"].is_i64()).expect("an item with a duration");
    let ticks = media["duration"].as_i64().unwrap();
    assert!((media["durationSeconds"].as_f64().unwrap() - Tick(ticks).seconds()).abs() < 1e-6, "{media}");
}

/// A media item with a picture, and a fresh active sequence at 30 fps (the demo media is not
/// 30 fps), so a frame counted at the wrong rate shows.
fn media_at_another_rate() -> (Session, filmagic_project::ItemId, FrameRate) {
    let mut s = demo();
    let item = s.project.items.values().find(|i| i.has_video() && i.as_media().is_some()).map(|i| i.id).unwrap();
    let media_rate = s.project.item(item).unwrap().frame_rate();
    s.execute("file.newSequence", json!({"name": "thirty", "fps": 30})).unwrap();
    assert_ne!(s.sequence_rate(), media_rate, "the test needs two rates");
    (s, item, media_rate)
}

/// Review finding: source times counted frames at the sequence rate.
#[test]
fn source_times_count_frames_at_the_media_rate() {
    let (mut s, item, media_rate) = media_at_another_rate();
    s.execute("source.open", json!({"item": item.0})).unwrap();
    let flat = s.execute("source.setPlayhead", json!({"frame": 48})).unwrap()["time"].clone();
    let object = s.execute("source.setPlayhead", json!({"time": {"frame": 48}})).unwrap()["time"].clone();
    assert_eq!(object, flat, "the object form counts at the media rate, as the flat key does");
    assert_eq!(object, json!(media_rate.tick_of(48).0));
    // with no sequence open, the item still has a rate
    s.state.active_sequence = None;
    let none = s.execute("source.setPlayhead", json!({"time": {"frame": 48}})).unwrap()["time"].clone();
    assert_eq!(none, flat);
}

/// Review finding: a frame hold on a source timecode, given as a time object, counted at the
/// sequence rate and ignored the media's start timecode.
#[test]
fn a_frame_hold_on_source_timecode_counts_in_the_media_own_timecode() {
    let (mut s, item, media_rate) = media_at_another_rate();
    s.execute("clip.modifyTimecode", json!({"item": item.0, "timecode": "01:00:00:00"})).unwrap();
    let start = s.project.item(item).unwrap().as_media().unwrap().info.start_timecode.unwrap();
    assert!(start > 0);
    let placed = s.execute("timeline.place", json!({"item": item.0, "time": 0, "audio": false})).unwrap();
    let clip = s.active_sequence().unwrap().video_tracks[0].items[0].id.0;
    let _ = placed;
    let hold = |s: &mut Session, extra: Value| {
        let mut p = json!({"clips": [clip], "holdOn": "sourceTimecode"});
        for (k, v) in extra.as_object().unwrap() {
            p[k] = v.clone();
        }
        s.execute("clip.frameHoldOptions", p).unwrap();
        s.active_sequence().unwrap().find_item(ClipId(clip)).unwrap().1.frame_hold
    };
    let tc = filmagic_time::format_timecode_frames(start + 48, media_rate, false);
    let by_key = hold(&mut s, json!({"timecode": tc}));
    assert_eq!(by_key, Some(media_rate.tick_of(48)));
    assert_eq!(hold(&mut s, json!({"time": {"timecode": tc}})), by_key, "the time object subtracts the start timecode too");
    assert_eq!(hold(&mut s, json!({"time": {"frame": 48}})), by_key, "frames count at the media rate");
    assert_eq!(hold(&mut s, json!({"time": {"seconds": media_rate.tick_of(48).seconds()}})), by_key);
    // a timecode before the media's first frame is refused, not wrapped
    let e =
        s.execute("clip.frameHoldOptions", json!({"clips": [clip], "holdOn": "sourceTimecode", "time": {"timecode": "00:00:01:00"}})).unwrap_err().to_string();
    assert!(e.contains("must not be negative"), "{e}");
    // in sequence time `time` counts at the sequence rate
    let seq_rate = s.sequence_rate();
    let r = s.execute("clip.frameHoldOptions", json!({"clips": [clip], "holdOn": "sequenceTime", "time": {"frame": 15}}));
    assert!(r.is_ok(), "{r:?}");
    let it = s.active_sequence().unwrap().find_item(ClipId(clip)).unwrap().1.clone();
    assert_eq!(it.frame_hold, Some(media_rate.snap(it.moving_source_time_at(seq_rate.tick_of(15)))));
}

/// Review finding: keys whose commands read their own object form skipped the hostile checks.
#[test]
fn keys_with_their_own_object_form_get_the_hostile_checks() {
    let mut s = demo();
    let c = first_clip(&s);
    let item = c.item.0;
    for (cmd, p, why) in [
        ("timeline.move", json!({"moves": [{"clip": c.id.0, "track": "V1", "time": {"seconds": -5.0}}]}), "must not be negative"),
        ("timeline.place", json!({"item": item, "time": 0, "sourceIn": {"seconds": 1e300}}), "beyond the time range"),
        ("timeline.place", json!({"item": item, "time": 0, "duration": {"seconds": 1e300}}), "beyond the time range"),
        ("timeline.place", json!({"item": item, "time": 0, "duration": {"seconds": -1.0}}), "must not be negative"),
    ] {
        let e = s.execute(cmd, p.clone()).unwrap_err().to_string();
        assert!(e.contains(why), "{cmd} {p}: {e}");
    }
    // and their valid object forms still work as before
    let r = s.execute("timeline.place", json!({"item": item, "track": "new", "time": {"seconds": 1.0}, "duration": {"seconds": 2.0}, "audio": false})).unwrap();
    assert!(r.is_object(), "{r}");
}

/// Review findings: the trim advice for a title wholly past the end, and titles past the end
/// hiding each other.
#[test]
fn past_end_advice_deletes_what_lies_wholly_past_and_titles_do_not_count_as_picture() {
    let mut s = demo();
    let end = s.active_sequence().unwrap().video_tracks.iter().flat_map(|t| &t.items).map(|i| i.end()).max().unwrap();
    // A: from inside the picture to 4 s past its end
    let a = s.execute("graphics.newText", json!({"text": "A", "time": (end - Tick::from_seconds_f64(1.0)).0, "seconds": 5.0})).unwrap();
    assert_eq!(a["pastSequenceEnd"][0]["whollyPast"], false, "{a}");
    // B: wholly past the end; A must not count as picture and hide it
    let b = s.execute("graphics.newText", json!({"text": "B", "time": end.0, "seconds": 2.0})).unwrap();
    let over = &b["pastSequenceEnd"][0];
    assert_eq!(over["pictureEnd"], end.0, "the picture end is the footage's: {b}");
    assert_eq!(over["whollyPast"], true, "{b}");
    let fix = over["fix"].as_str().unwrap();
    assert!(fix.starts_with("timeline.delete"), "{fix}");
    assert!(b["warnings"][0].as_str().unwrap().contains("wholly past the picture"), "{b}");
    // the advice works as given
    let clip = b["clip"].as_u64().unwrap();
    s.execute("timeline.delete", json!({"clips": [clip]})).unwrap();
    assert!(s.active_sequence().unwrap().find_item(ClipId(clip)).is_none());
    // a caption wholly past the end: delete it
    let r = s.execute("captions.add", json!({"time": (end + Tick::from_seconds_f64(1.0)).0, "durationSeconds": 2.0})).unwrap();
    assert!(r["pastSequenceEnd"][0]["fix"].as_str().unwrap().starts_with("captions.delete"), "{r}");
}

/// Review finding: `project.inspect` gave sound-only items and subclips the default frame rate.
#[test]
fn project_inspect_rates_follow_the_picture() {
    let mut s = demo();
    let video = s.project.items.values().find(|i| i.has_video() && i.as_media().is_some()).map(|i| (i.id, i.frame_rate())).unwrap();
    s.execute("source.open", json!({"item": video.0.0})).unwrap();
    let sub = s.execute("clip.makeSubclip", json!({"item": video.0.0, "name": "sub"})).unwrap();
    let sub_id = sub["item"].as_u64().or_else(|| sub["subclip"].as_u64()).expect("the subclip's id");
    let v = crate::commands::inspect_project(&s);
    let mut all = Vec::new();
    fn items(v: &Value, out: &mut Vec<Value>) {
        for c in v["children"].as_array().into_iter().flatten() {
            if c.get("item").is_some() {
                out.push(c.clone());
            }
            items(c, out);
        }
    }
    items(&v["root"], &mut all);
    let row = |id: u64| all.iter().find(|r| r["item"] == id).cloned().unwrap();
    assert_eq!(row(sub_id)["frameRate"], json!(video.1.as_f64()), "a subclip plays at its media's rate");
    for r in &all {
        let id = filmagic_project::ItemId(r["item"].as_u64().unwrap());
        let it = s.project.item(id).unwrap();
        if it.as_media().is_some_and(|m| !m.info.has_video()) {
            assert!(r["durationFrames"].is_null() && r["frameRate"].is_null(), "sound alone has no frames: {r}");
        }
    }
}

/// Follow-up review finding: a source time object given for several clips was converted with the
/// first clip's media, and that result applied to every clip.
#[test]
fn a_source_time_object_for_clips_with_different_media_clocks_is_refused() {
    let mut s = demo();
    let items: Vec<_> = s.project.items.values().filter(|i| i.has_video() && i.as_media().is_some()).map(|i| i.id).take(2).collect();
    assert_eq!(items.len(), 2, "the demo has two pictures");
    s.execute("file.newSequence", json!({"name": "two"})).unwrap();
    for (item, tc) in items.iter().zip(["01:00:00:00", "02:00:00:00"]) {
        s.execute("clip.modifyTimecode", json!({"item": item.0, "timecode": tc})).unwrap();
    }
    s.execute("timeline.place", json!({"item": items[0].0, "time": 0, "audio": false})).unwrap();
    let first_end = s.active_sequence().unwrap().video_tracks[0].items[0].end();
    s.execute("timeline.place", json!({"item": items[1].0, "time": first_end.0, "audio": false})).unwrap();
    let clips: Vec<u64> = s.active_sequence().unwrap().video_tracks[0].items.iter().map(|i| i.id.0).collect();
    assert_eq!(clips.len(), 2);
    let before = s.project.clone();
    for time in [json!({"timecode": "01:00:02:00"}), json!({"frame": 48})] {
        let e = s.execute("clip.frameHoldOptions", json!({"clips": clips, "holdOn": "sourceTimecode", "time": time})).unwrap_err().to_string();
        assert!(e.contains("flat `timecode`") && e.contains("one call per clip"), "{e}");
        assert_eq!(*before, *s.project, "nothing changed");
    }
    // seconds mean the same media time in every clip
    s.execute("clip.frameHoldOptions", json!({"clips": clips, "holdOn": "sourceTimecode", "time": {"seconds": 1.0}})).unwrap();
    // with one clock for all, the object form is per clip too
    s.execute("clip.modifyTimecode", json!({"item": items[1].0, "timecode": "01:00:00:00"})).unwrap();
    let rates: Vec<_> = items.iter().map(|i| s.project.item(*i).unwrap().frame_rate()).collect();
    let r = s.execute("clip.frameHoldOptions", json!({"clips": clips, "holdOn": "sourceTimecode", "time": {"timecode": "01:00:02:00"}}));
    if rates[0] == rates[1] {
        r.unwrap();
        let held: Vec<_> = clips.iter().map(|c| s.active_sequence().unwrap().find_item(ClipId(*c)).unwrap().1.frame_hold).collect();
        assert_eq!(held[0], held[1], "{held:?}");
    } else {
        assert!(r.is_err(), "different rates are refused too");
    }
}
