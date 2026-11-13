//! `sequence.inspect` filters and compact mode, `effects.list {effect}` (#16).

use serde_json::{Value, json};

use crate::Session;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

fn first_clip(s: &Session) -> u64 {
    s.active_sequence().unwrap().video_tracks[0].items[0].id.0
}

fn compact_clips(v: &Value) -> Vec<Value> {
    v["tracks"].as_array().unwrap().iter().flat_map(|t| t["clips"].as_array().unwrap().iter().cloned()).collect()
}

fn items(v: &Value, key: &str) -> Vec<u64> {
    v[key].as_array().unwrap().iter().flat_map(|t| t["items"].as_array().unwrap().iter().map(|i| i["clip"].as_u64().unwrap())).collect()
}

#[test]
fn compact_is_short_and_lists_what_changed() {
    let mut s = demo();
    let c = first_clip(&s);
    s.execute("effects.apply", json!({"clip": c, "effect": "Gaussian Blur"})).unwrap();
    let full = s.execute("sequence.inspect", json!({})).unwrap().to_string();
    let compact = s.execute("sequence.inspect", json!({"compact": true})).unwrap();
    assert!(compact.to_string().len() * 3 < full.len(), "compact {} vs full {}", compact.to_string().len(), full.len());
    let n: usize = s.active_sequence().unwrap().all_tracks().map(|t| t.items.len()).sum();
    assert_eq!(compact_clips(&compact).len(), n);
    let one = compact_clips(&compact).into_iter().find(|x| x["clip"] == c).unwrap();
    assert!(one["start"].is_number(), "{one}");
    let fx: Vec<&str> = one["effects"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert!(fx.iter().any(|e| e.contains("blur")), "{one}");
    assert!(!one["changed"].as_array().into_iter().flatten().any(|e| e["effect"] == "opacity"), "an untouched Opacity is left out: {one}");
    // a changed value is listed, with its value
    s.execute("effects.setParam", json!({"clip": c, "effect": "opacity", "param": "opacity", "value": 50.0})).unwrap();
    let compact = s.execute("sequence.inspect", json!({"compact": true, "clip": c})).unwrap();
    let changed = compact_clips(&compact)[0]["changed"].clone();
    let opacity = changed.as_array().unwrap().iter().find(|e| e["effect"] == "opacity").unwrap().clone();
    assert_eq!(opacity["params"]["opacity"], 50.0, "{compact}");
}

#[test]
fn filters_keep_only_the_clip_track_or_effect_asked_for() {
    let mut s = demo();
    let c = first_clip(&s);
    let r = s.execute("sequence.inspect", json!({"clip": c})).unwrap();
    assert_eq!(items(&r, "video"), vec![c]);
    assert!(items(&r, "audio").iter().all(|a| *a == c), "{r}");
    let r = s.execute("sequence.inspect", json!({"track": "v1"})).unwrap();
    assert_eq!(r["video"].as_array().unwrap().len(), 1);
    assert!(r["audio"].as_array().unwrap().is_empty());
    assert_eq!(r["video"][0]["track"], "V1");
    let r = s.execute("sequence.inspect", json!({"effect": "Opacity"})).unwrap();
    for t in r["video"].as_array().unwrap() {
        for i in t["items"].as_array().unwrap() {
            assert!(i["effects"].as_array().unwrap().iter().all(|e| e["effect"] == "opacity"), "{i}");
        }
    }
    let r = s.execute("sequence.inspect", json!({"compact": true, "clip": [c], "effect": "opacity"})).unwrap();
    assert_eq!(compact_clips(&r).len(), 1, "{r}");
    // no filter: as before
    let r = s.execute("sequence.inspect", json!({})).unwrap();
    assert!(r.get("filter").is_none() && r.get("compact").is_none());
}

#[test]
fn hostile_inspect_filters_are_refused() {
    let mut s = demo();
    for p in [
        json!({"clip": "x"}),
        json!({"clip": [1, "y"]}),
        json!({"clip": -1}),
        json!({"track": "Z9"}),
        json!({"track": true}),
        json!({"effect": 5}),
        json!({"effect": " "}),
        json!({"compact": "yes"}),
    ] {
        assert!(s.execute("sequence.inspect", p.clone()).is_err(), "{p}");
    }
    // an unknown clip id is simply not there
    let r = s.execute("sequence.inspect", json!({"compact": true, "clip": 987_654_321})).unwrap();
    assert!(compact_clips(&r).is_empty());
}

#[test]
fn effects_list_gives_one_effect_with_its_parameters() {
    let mut s = Session::default();
    let r = s.execute("effects.list", json!({"effect": "Gaussian Blur"})).unwrap();
    let a = r.as_array().unwrap();
    assert_eq!(a.len(), 1, "{r}");
    let info = a[0]["paramInfo"].as_array().unwrap();
    assert!(info.iter().any(|p| p["type"] == "float" && p["min"].is_number() && p["max"].is_number() && !p["default"].is_null()), "{r}");
    let e = s.execute("effects.list", json!({"effect": "No Such Effect"})).unwrap_err().to_string();
    assert!(e.contains("lists every id"), "{e}");
    assert!(s.execute("effects.list", json!({"effect": 3})).is_err());
}

#[test]
fn compact_keeps_two_of_the_same_effect_apart_and_leaves_automatic_motion_out() {
    let mut s = demo();
    let c = first_clip(&s);
    s.execute("effects.apply", json!({"clip": c, "effect": "Gaussian Blur"})).unwrap();
    s.execute("effects.apply", json!({"clip": c, "effect": "Gaussian Blur"})).unwrap();
    let blurs: Vec<usize> = s
        .active_sequence()
        .unwrap()
        .find_item(filmagic_project::ClipId(c))
        .unwrap()
        .1
        .effects
        .iter()
        .enumerate()
        .filter(|(_, e)| e.effect != "motion" && e.effect != "opacity" && e.effect != "time_remap")
        .map(|(i, _)| i)
        .collect();
    for (k, i) in blurs.iter().enumerate() {
        let id = s.active_sequence().unwrap().find_item(filmagic_project::ClipId(c)).unwrap().1.effects[*i].effect.clone();
        let def = filmagic_project::find_effect(&id).unwrap();
        let param = def.params.iter().find(|p| matches!(p.kind, filmagic_project::ParamKind::Float { .. })).unwrap().id;
        s.execute("effects.setParam", json!({"clip": c, "effect": i, "param": param, "value": 10.0 + k as f64})).unwrap();
    }
    let r = s.execute("sequence.inspect", json!({"compact": true, "clip": c})).unwrap();
    let one = &compact_clips(&r)[0];
    let changed = one["changed"].as_array().unwrap();
    let indices: Vec<u64> = changed.iter().map(|e| e["index"].as_u64().unwrap()).collect();
    assert_eq!(indices, blurs.iter().map(|i| *i as u64).collect::<Vec<_>>(), "both blurs, each with its index: {one}");
    // Motion left at its automatic position and anchor is not a change
    assert!(!changed.iter().any(|e| e["effect"] == "motion"), "{one}");
    // the demo's picture in a picture is scaled (a real change) but sits at its automatic
    // position and anchor, which are not changes
    for x in compact_clips(&s.execute("sequence.inspect", json!({"compact": true})).unwrap()) {
        for e in x["changed"].as_array().into_iter().flatten().filter(|e| e["effect"] == "motion") {
            assert!(e["params"].get("position").is_none() && e["params"].get("anchor_point").is_none() && e["params"].get("anchor").is_none(), "{x}");
        }
    }
}

#[test]
fn compact_finds_a_clip_past_the_first_five_hundred_and_takes_track_names() {
    let mut s = demo();
    let seq = s.state.active_sequence.unwrap();
    let mut p = (*s.project).clone();
    let mut next = p.next_id;
    let q = p.sequence_mut(seq).unwrap();
    let template = q.video_tracks[0].items[0].clone();
    let end = q.duration();
    let mut last = 0;
    for k in 0..600 {
        let mut it = template.clone();
        next += 1;
        it.id = filmagic_project::ClipId(next);
        it.link = None;
        it.start = end + filmagic_time::Tick(template.duration.0 * k);
        last = next;
        q.video_tracks[0].items.push(it);
    }
    p.next_id = next + 1;
    s.project = std::sync::Arc::new(p);
    let r = s.execute("sequence.inspect", json!({"compact": true, "clip": last})).unwrap();
    assert_eq!(compact_clips(&r).iter().map(|x| x["clip"].as_u64().unwrap()).collect::<Vec<_>>(), vec![last], "{r}");
    // the track's own name works as in the full form
    let name = s.active_sequence().unwrap().video_tracks[0].name.clone();
    let full = s.execute("sequence.inspect", json!({"track": name})).unwrap();
    let compact = s.execute("sequence.inspect", json!({"track": name, "compact": true})).unwrap();
    assert_eq!(full["video"].as_array().map(Vec::len), Some(1));
    assert_eq!(compact["tracks"].as_array().map(Vec::len), Some(1), "{}", compact["tracks"]);
}
