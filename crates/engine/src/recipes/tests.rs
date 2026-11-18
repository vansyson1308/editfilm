//! Recipes and the guide (#22): every command they name exists, the pure planning is right, and
//! each recipe builds its draft end to end on synthetic footage made here (a rally of clicks and
//! cheering over a moving square; a voice-like sound with a burst of laughter), with one undo
//! step, the same draft for the same input, and hostile parameters refused without a change.

use std::path::{Path, PathBuf};

use filmagic_time::Tick;
use serde_json::{Value, json};

use super::guide::TOPICS;
use super::meme::plan;
use super::sports::{Seg, choose};
use super::{RECIPES, Recipe};
use crate::Session;
use crate::media_test_util::tmp_dir;

/// Commands a recipe or the guide may name although this build does not have them yet; they are
/// skipped with a note (recipes) or left out (the guide's `prefer`) until they land.
const LATER: &[&str] = &["clip.autoReframe", "sports.ballTrail", "sports.trackPoint"];

const FPS: u32 = 25;
const SR: u32 = 48_000;
const W: u32 = 64;
const H: u32 = 36;

fn exists(id: &str) -> bool {
    crate::commands::find(id).is_some()
}

#[test]
fn every_command_the_recipes_and_the_guide_name_exists() {
    let mut seen = std::collections::HashSet::new();
    for r in RECIPES {
        assert!(seen.insert(r.id), "two recipes are called {}", r.id);
        for (c, _) in r.steps {
            assert!(exists(c), "recipe {} runs `{c}`, which is not in the registry", r.id);
        }
        for (c, _) in r.later {
            assert!(exists(c) || LATER.contains(c), "recipe {} names `{c}`, which is neither in the registry nor expected later", r.id);
        }
        assert!(!r.keys.contains(&"recipe") && !r.keys.contains(&"job") && !r.keys.contains(&"wait"));
    }
    let mut topics = std::collections::HashSet::new();
    for t in TOPICS {
        assert!(topics.insert(t.id), "two guide topics are called {}", t.id);
        for (c, _, _) in t.steps {
            assert!(exists(c), "guide topic {} names `{c}`, which is not in the registry", t.id);
        }
        if let Some((c, _, _)) = t.prefer {
            assert!(exists(c) || LATER.contains(&c), "guide topic {} prefers `{c}`, which is neither in the registry nor expected later", t.id);
        }
    }
    // the recipes' and the guide's own commands are registered
    for c in ["recipes.list", "recipes.describe", "recipes.run", "guide.find"] {
        assert!(exists(c), "{c}");
    }
    // a command the guide names inside its example parameters is in the registry too
    let spaces: std::collections::HashSet<&str> = crate::commands::command_specs().iter().filter_map(|c| c.id.split('.').next()).collect();
    for t in TOPICS {
        for (_, p, n) in t.steps {
            for w in format!("{p} {n}").split(|c: char| !(c.is_alphanumeric() || c == '.')) {
                let looks_like_id = w.contains('.') && w.split('.').count() == 2 && w.chars().next().is_some_and(char::is_lowercase) && !w.ends_with('.');
                let is_file = [".png", ".mp4", ".gif", ".srt"].iter().any(|x| w.ends_with(&x[1..]) && w.contains('.'));
                let in_space = w.split('.').next().is_some_and(|x| spaces.contains(x));
                if looks_like_id && in_space && !is_file && w.split('.').all(|x| !x.is_empty() && x.chars().all(char::is_alphanumeric)) {
                    assert!(exists(w) || LATER.contains(&w), "guide topic {} mentions `{w}`", t.id);
                }
            }
        }
    }
}

fn guide(s: &mut Session, goal: &str) -> Value {
    s.execute("guide.find", json!({"goal": goal})).unwrap()
}

fn named(v: &Value) -> Vec<String> {
    v["topics"].as_array().unwrap().iter().flat_map(|t| t["steps"].as_array().unwrap().iter().map(|x| x["command"].as_str().unwrap().to_string())).collect()
}

#[test]
fn the_guide_leads_to_the_commands_agents_missed() {
    let mut s = Session::default();
    let r = guide(&mut s, "apply the same grade to many clips");
    assert_eq!(r["topics"][0]["topic"], "same-look-many-clips", "{r}");
    assert!(named(&r).contains(&"edit.pasteAttributes".to_string()), "{r}");
    let r = guide(&mut s, "add an adjustment layer");
    assert_eq!(r["topics"][0]["topic"], "adjustment-layer", "{r}");
    assert_eq!(r["topics"][0]["steps"][0]["command"], "file.newAdjustmentLayer");
    let r = guide(&mut s, "duck music under speech");
    assert_eq!(r["topics"][0]["topic"], "duck-music", "{r}");
    assert!(named(&r).contains(&"essentialSound.generateDucking".to_string()));
    let r = guide(&mut s, "reframe to 9:16");
    assert_eq!(r["topics"][0]["topic"], "reframe-vertical", "{r}");
    // the preferred command shows only where this build has it
    assert_eq!(named(&r).contains(&"clip.autoReframe".to_string()), exists("clip.autoReframe"));
    let r = guide(&mut s, "a highlight reel of the best rallies");
    assert_eq!(r["topics"][0]["topic"], "highlight-reel", "{r}");
    assert!(r["recipes"].as_array().unwrap().iter().any(|x| x["recipe"] == "sports-highlight"), "{r}");
    let r = guide(&mut s, "funny meme");
    assert!(r["recipes"].as_array().unwrap().iter().any(|x| x["recipe"] == "comedy-meme"), "{r}");
    // every topic without a goal; nothing for gibberish, with a note
    let all = s.execute("guide.find", json!({})).unwrap();
    assert_eq!(all["topics"].as_array().unwrap().len(), TOPICS.len());
    let none = guide(&mut s, "qqqq zzzz");
    assert!(none["topics"].as_array().unwrap().is_empty() && none["notes"].is_array(), "{none}");
    // hostile
    for bad in [json!({"goal": 5}), json!({"goal": "x".repeat(501)}), json!({"limit": 0}), json!({"limit": "3"}), json!([1])] {
        assert!(s.execute("guide.find", bad.clone()).is_err(), "{bad}");
    }
    // the deterministic order
    assert_eq!(guide(&mut s, "slow motion freeze"), guide(&mut s, "slow motion freeze"));
}

fn seg(rank: u64, score: f64, a: f64, b: f64) -> Seg {
    Seg { rank, score, a: Tick::from_seconds_f64(a), b: Tick::from_seconds_f64(b), cues: json!([]), kind: json!("rally") }
}

#[test]
fn choose_takes_the_best_moments_up_to_the_target_in_time_order() {
    let segs = [seg(1, 0.9, 50.0, 60.0), seg(2, 0.8, 10.0, 18.0), seg(3, 0.7, 30.0, 52.0), seg(4, 0.5, 70.0, 75.0), seg(5, 0.4, 80.0, 81.0)];
    let target = Tick::from_seconds_f64(20.0);
    let c = choose(&segs, target, 6, true);
    // 50–60 and 10–18 (18 s); 30–52 overlaps the first; 70–75 still fits (23 s of at most 24)
    let got: Vec<(f64, f64)> = c.iter().map(|g| (g.a.seconds(), g.b.seconds())).collect();
    assert_eq!(got, vec![(10.0, 18.0), (50.0, 60.0), (70.0, 75.0)], "{c:?}");
    // a moment that would run long is cut from its start to what is left, keeping its end
    let c = choose(&segs, Tick::from_seconds_f64(14.0), 6, true);
    let got: Vec<(f64, f64)> = c.iter().map(|g| (g.a.seconds(), g.b.seconds())).collect();
    assert_eq!(got, vec![(14.0, 18.0), (50.0, 60.0)], "{c:?}");
    // by score
    let c = choose(&segs, target, 6, false);
    assert_eq!(c[0].rank, 1);
    // at most `max`
    assert_eq!(choose(&segs, Tick::from_seconds_f64(300.0), 2, true).len(), 2);
    // one long moment is cut to the target, keeping its end
    let c = choose(&[seg(1, 0.9, 0.0, 100.0)], target, 6, true);
    assert_eq!((c[0].a.seconds(), c[0].b.seconds()), (80.0, 100.0));
    assert!(choose(&[], target, 6, true).is_empty());
}

fn t(x: f64) -> Tick {
    Tick::from_seconds_f64(x)
}

#[test]
fn meme_plan_cuts_pauses_and_ends_on_the_payoff() {
    let speech = [(t(1.0), t(2.0)), (t(3.0), t(4.0)), (t(4.2), t(5.0)), (t(8.0), t(9.0))];
    let p = plan((t(0.0), t(20.0)), t(10.0), &speech, t(20.0), t(0.35), true);
    let s: Vec<(f64, f64)> = p.iter().map(|(a, b)| ((a.seconds() * 100.0).round() / 100.0, (b.seconds() * 100.0).round() / 100.0)).collect();
    // phrases padded by 0.15 s; 4.0–4.2 is too short a pause to cut; the payoff runs from 0.6 s
    // before the moment to 2.5 s after it, merged with the phrase before it
    assert_eq!(s, vec![(0.85, 2.15), (2.85, 5.15), (7.85, 12.5)], "{s:?}");
    // the total is capped from the front
    let p = plan((t(0.0), t(20.0)), t(10.0), &speech, t(5.0), t(0.35), true);
    let total: f64 = p.iter().map(|(a, b)| (*b - *a).seconds()).sum();
    assert!((total - 5.0).abs() < 1e-6, "{p:?}");
    assert_eq!(p.last().map(|x| x.1), Some(t(12.5)));
    // without speech, or with cutPauses false: one stretch before the payoff
    let p = plan((t(0.0), t(20.0)), t(10.0), &[], t(6.0), t(0.35), true);
    assert_eq!(p, vec![(t(6.5), t(12.5))]);
    let p = plan((t(0.0), t(20.0)), t(10.0), &speech, t(6.0), t(0.35), false);
    assert_eq!(p, vec![(t(6.5), t(12.5))]);
    // a moment at the very end, or outside the span, stays inside it
    let p = plan((t(0.0), t(3.0)), t(50.0), &[], t(20.0), t(0.35), true);
    assert_eq!(p, vec![(t(0.0), t(3.0))]);
    // a flood of phrases is merged down to at most 40 pieces
    let many: Vec<(Tick, Tick)> = (0..500).map(|k| (t(f64::from(k) * 0.8), t(f64::from(k) * 0.8 + 0.3))).collect();
    let p = plan((t(0.0), t(400.0)), t(399.0), &many, t(120.0), t(0.35), true);
    assert!(p.len() <= 40, "{}", p.len());
}

// ---------------------------------------------------------------------------- synthetic media

fn noise(seed: &mut u32) -> f32 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 17;
    *seed ^= *seed << 5;
    (*seed as f32 / u32::MAX as f32) * 2.0 - 1.0
}

/// Quiet noise, with a click at each of `clicks` and crowd-like noise over each of `swells`.
fn rally_sound(seconds: f64, clicks: &[f64], swells: &[(f64, f64)]) -> Vec<f32> {
    let n = (seconds * f64::from(SR)) as usize;
    let mut seed = 0x9e37_79b9u32;
    let mut x: Vec<f32> = (0..n).map(|_| 0.003 * noise(&mut seed)).collect();
    for &c in clicks {
        let a = (c * f64::from(SR)) as usize;
        for (k, v) in x.iter_mut().skip(a).take((SR / 300) as usize).enumerate() {
            *v += 0.6 * (-(k as f32) / (SR as f32 / 1600.0)).exp() * if k % 2 == 0 { 1.0 } else { -1.0 };
        }
    }
    for &(a, b) in swells {
        for (i, v) in x.iter_mut().enumerate() {
            let t = i as f64 / f64::from(SR);
            if t >= a && t <= b {
                *v += 0.1 * ((t - a).min(b - t) / 0.3).min(1.0) as f32 * noise(&mut seed);
            }
        }
    }
    x
}

/// A voice-like sound over `[a, b)`: harmonics of 140 Hz swelling at a syllable rate of 4 Hz.
fn voice(x: &mut [f32], a: f64, b: f64) {
    for (i, v) in x.iter_mut().enumerate() {
        let t = i as f64 / f64::from(SR);
        if t >= a && t < b {
            let env = 0.5 + 0.5 * (2.0 * std::f64::consts::PI * 4.0 * t).sin();
            let tone: f64 = (1..8).map(|k| (2.0 * std::f64::consts::PI * 140.0 * f64::from(k) * t).sin() / f64::from(k)).sum();
            *v += (0.12 * env * tone) as f32;
        }
    }
}

fn wav(path: &Path, x: &[f32]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, filmagic_media::wav::write_wav16(x, 1, SR)).unwrap();
}

/// A movie (ProRes + PCM, written by our own encoder) of `sound`, with a white square crossing a
/// dark frame during each of `moving` (seconds).
fn movie(dir: &Path, name: &str, sound: &[f32], moving: &[(f64, f64)]) -> PathBuf {
    let seconds = sound.len() as f64 / f64::from(SR);
    let frames = (seconds * f64::from(FPS)).floor() as u32;
    let pics = dir.join(format!("{name}-frames"));
    std::fs::create_dir_all(&pics).unwrap();
    let mut first = String::new();
    for n in 0..frames {
        let t = f64::from(n) / f64::from(FPS);
        let mut img = image::RgbaImage::from_pixel(W, H, image::Rgba([40, 40, 40, 255]));
        if let Some((a, _)) = moving.iter().find(|(a, b)| t >= *a && t < *b) {
            let x0 = (((t - a) * 40.0) as u32) % (W - 12);
            for y in 10..26 {
                for x in x0..x0 + 12 {
                    img.put_pixel(x, y, image::Rgba([250, 250, 250, 255]));
                }
            }
        }
        let p = pics.join(format!("f{n:05}.png"));
        img.save(&p).unwrap();
        if n == 0 {
            first = p.to_string_lossy().to_string();
        }
    }
    let snd = dir.join(format!("{name}.wav"));
    wav(&snd, sound);
    let mut s = Session::default();
    s.execute("prefs.set", json!({"key": "media.indeterminateTimebase", "value": "25"})).unwrap();
    let img = s.execute("file.import", json!({"paths": [first], "imageSequence": true})).unwrap()["items"][0].as_u64().unwrap();
    let aud = s.execute("file.import", json!({"paths": [snd.to_string_lossy()]})).unwrap()["items"][0].as_u64().unwrap();
    s.execute("file.newSequence", json!({"name": "src", "width": W, "height": H, "fps": FPS, "video": 1, "audio": 1})).unwrap();
    s.execute("timeline.place", json!({"item": img, "track": "V1", "time": 0, "audio": false})).unwrap();
    s.execute("timeline.place", json!({"item": aud, "track": "A1", "time": 0})).unwrap();
    let out = dir.join(format!("{name}.mov"));
    s.execute("file.exportMedia", json!({"path": out.to_string_lossy(), "format": "prores", "wait": true})).unwrap();
    assert!(out.is_file());
    out
}

/// 24 s of a match: a 6-hit rally from 4 s with cheering after it, a lone hit at 13 s, a 5-hit
/// rally from 16 s with cheering; the square moves during the rallies.
fn rally_movie(dir: &Path) -> PathBuf {
    let mut clicks: Vec<f64> = (0..6).map(|i| 4.0 + 0.8 * f64::from(i)).collect();
    clicks.push(13.0);
    clicks.extend((0..5).map(|i| 16.0 + 0.9 * f64::from(i)));
    let x = rally_sound(24.0, &clicks, &[(8.8, 11.0), (20.0, 22.0)]);
    movie(dir, "rally", &x, &[(4.0, 9.0), (16.0, 21.0)])
}

fn session(data: &Path) -> Session {
    let mut s = Session::default();
    s.packs.set_dir(data);
    s
}

fn import(s: &mut Session, p: &Path) -> u64 {
    s.execute("file.import", json!({"paths": [p.to_string_lossy()]})).unwrap()["items"][0].as_u64().unwrap()
}

fn recipe(s: &mut Session, p: Value) -> Value {
    s.execute("recipes.run", p.clone()).unwrap_or_else(|e| panic!("{p}: {e}"))
}

fn steps_of<'a>(r: &'a Value, cmd: &str) -> Vec<&'a Value> {
    r["steps"].as_array().unwrap().iter().filter(|x| x["command"] == cmd).collect()
}

fn notes(r: &Value) -> String {
    format!("{} {}", r["notes"], r["warnings"])
}

/// The draft without its ids: what must be the same for the same input.
fn shape(r: &Value) -> Value {
    let steps: Vec<Value> = r["steps"].as_array().unwrap().iter().map(|x| json!([x["command"], x["status"], x["why"]])).collect();
    let moments: Vec<Value> = r["moments"].as_array().map(|m| m.iter().map(|x| json!([x["media"], x["reel"], x["score"]])).collect()).unwrap_or_default();
    json!({"steps": steps, "moments": moments, "pieces": r["pieces"].as_array().map(|p| p.iter().map(|x| x["media"].clone()).collect::<Vec<_>>()), "duration": r["duration"]})
}

#[test]
fn a_sports_highlight_reel_from_a_rally_recording() {
    let dir = tmp_dir("recipe-sports");
    let path = rally_movie(&dir);
    let mut s = session(&dir.join("data"));
    let item = import(&mut s, &path);
    let items_before = s.project.items.len();
    let undo_before = s.history.undo.len();
    let r = recipe(&mut s, json!({"recipe": "sports-highlight", "item": item, "targetSeconds": 20, "wait": true}));
    assert_eq!(r["status"], "done", "{r}");
    assert!(r.get("job").is_none(), "a finished draft names no job: MCP would wait for it");
    // the two rallies, in time order, each holding its hits
    let m = r["moments"].as_array().unwrap();
    assert_eq!(m.len(), 2, "{r}");
    let (a0, b0) = (m[0]["media"]["from"].as_f64().unwrap(), m[0]["media"]["to"].as_f64().unwrap());
    let (a1, b1) = (m[1]["media"]["from"].as_f64().unwrap(), m[1]["media"]["to"].as_f64().unwrap());
    assert!(a0 <= 4.0 && b0 >= 8.0 && b0 < a1 && a1 <= 16.0 && b1 >= 19.6, "{m:?}");
    // the reel: a new sequence of the recording's size and rate, the active one
    let seq = r["sequence"].as_u64().unwrap();
    assert_eq!(s.state.active_sequence.map(|x| x.0), Some(seq));
    let q = s.active_sequence().unwrap();
    assert_eq!((q.settings.width, q.settings.height, q.settings.frame_rate.as_f64()), (W, H, 25.0));
    let v1 = &q.video_tracks[0].items;
    assert_eq!(v1.len(), 2, "{r}");
    assert_eq!(v1[0].start, Tick::ZERO);
    assert_eq!(v1[0].end(), v1[1].start, "back to back");
    // every step is reported with its command and params; the optional ones applied here
    assert_eq!(steps_of(&r, "timeline.place").len(), 2);
    for c in ["clip.speedRamp", "sequence.applyVideoTransition", "sequence.applyAudioTransition", "graphics.template.apply"] {
        assert!(steps_of(&r, c).iter().all(|x| x["status"] == "done") && !steps_of(&r, c).is_empty(), "{c}: {r}");
    }
    assert!(steps_of(&r, "timeline.place")[0]["params"]["sourceIn"].is_i64());
    assert_eq!(q.markers.len(), 2, "a marker on each moment");
    assert!(q.markers[0].name.starts_with("Highlight"));
    assert!(steps_of(&r, "clip.speedRamp")[0]["result"]["keyframes"].as_u64().is_some_and(|k| k >= 2), "slow motion: {r}");
    // no music pack installed: said so, with what to add
    assert!(notes(&r).contains("no music"), "{r}");
    assert!(r["nextSteps"].as_array().unwrap().iter().any(|x| x["command"] == "story.highlights"));
    assert!(r["pictureChange"].is_object(), "{r}");
    // one undo step takes the whole draft back
    assert_eq!(s.history.undo.len(), undo_before + 1);
    assert_eq!(s.history.undo.last().unwrap().0, "Recipe: Sports Highlight");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.items.len(), items_before);
    // the same input gives the same draft
    let again = recipe(&mut s, json!({"recipe": "sports-highlight", "item": item, "targetSeconds": 20, "wait": true}));
    assert_eq!(shape(&again), shape(&r));
    // a timeline clip is read in sequence time: placed 10 s in, the same media is cut
    s.execute("file.newSequence", json!({"name": "M", "fromItem": item})).unwrap();
    s.execute("timeline.place", json!({"item": item, "track": "V2", "seconds": 10.0})).unwrap();
    let clip = s.active_sequence().unwrap().video_tracks[1].items[0].id.0;
    let c = recipe(&mut s, json!({"recipe": "sports-highlight", "clip": clip, "targetSeconds": 20, "wait": true, "title": false, "markers": false}));
    let cm: Vec<Value> = c["moments"].as_array().unwrap().iter().map(|x| x["media"].clone()).collect();
    let rm: Vec<Value> = r["moments"].as_array().unwrap().iter().map(|x| x["media"].clone()).collect();
    assert_eq!(cm.len(), rm.len(), "{c}");
    for (x, y) in cm.iter().zip(&rm) {
        assert!((x["from"].as_f64().unwrap() - y["from"].as_f64().unwrap()).abs() < 0.1, "{x} vs {y}");
    }
    assert!(steps_of(&c, "graphics.template.apply").is_empty() && steps_of(&c, "markers.add").is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_sports_recipe_without_wait_runs_as_a_job_then_builds_once() {
    let dir = tmp_dir("recipe-job");
    let path = rally_movie(&dir);
    let mut s = session(&dir.join("data"));
    let item = import(&mut s, &path);
    let undo = s.history.undo.len();
    let start = recipe(&mut s, json!({"recipe": "sports-highlight", "item": item, "targetSeconds": 20, "picture": false}));
    assert_eq!(start["status"], "running", "{start}");
    let job = start["job"].as_u64().unwrap();
    let rc = &start["resultCommand"];
    assert_eq!(rc["command"], "recipes.run");
    assert_eq!(rc["params"]["job"], job);
    assert!(rc["params"].get("wait").is_none());
    let items = s.project.items.len();
    assert_eq!(s.history.undo.len(), undo, "starting the analysis changes nothing");
    let mut r = Value::Null;
    for _ in 0..4000 {
        r = recipe(&mut s, rc["params"].clone());
        if r["status"] != "running" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(r["status"], "done", "{r}");
    assert_eq!(s.history.undo.len(), undo + 1, "one draft");
    let built = s.project.items.len();
    assert!(built > items);
    // asked again: the same draft, not a second reel
    let again = recipe(&mut s, rc["params"].clone());
    assert_eq!(again["alreadyBuilt"], true);
    assert_eq!(again["sequence"], r["sequence"]);
    assert_eq!((s.project.items.len(), s.history.undo.len()), (built, undo + 1));
    // a job of other footage is refused
    let other = import(&mut s, &dir.join("rally.wav"));
    let e = s.execute("recipes.run", json!({"recipe": "sports-highlight", "item": other, "job": job + 100})).unwrap_err().to_string();
    assert!(e.contains("job"), "{e}");
    let _ = std::fs::remove_dir_all(dir);
}

/// A pack of one's own files: a music bed, a boom, a meme.
fn own_pack(s: &mut Session, root: &Path) {
    let f = root.join("Mine");
    let mut seed = 7u32;
    let bed: Vec<f32> = (0..(50 * SR) as usize).map(|i| 0.1 * (i as f32 * 0.01).sin() + 0.01 * noise(&mut seed)).collect();
    wav(&f.join("music/upbeat bed.wav"), &bed);
    let boom: Vec<f32> = (0..(SR / 2) as usize).map(|i| 0.5 * (-(i as f32) / 4000.0).exp() * noise(&mut seed)).collect();
    wav(&f.join("sfx/boom.wav"), &boom);
    std::fs::create_dir_all(f.join("memes")).unwrap();
    image::RgbaImage::from_pixel(32, 32, image::Rgba([0, 0, 0, 0])).save(f.join("memes/reaction cat.png")).unwrap();
    let r = s.execute("assets.buildPack", json!({"from": f.to_string_lossy(), "id": "mine", "license": "CC0-1.0"})).unwrap();
    assert_eq!(r["built"]["items"], 3, "{r}");
}

#[test]
fn a_sports_reel_with_music_and_a_vertical_copy() {
    let dir = tmp_dir("recipe-sports-music");
    let path = rally_movie(&dir);
    let mut s = session(&dir.join("data"));
    own_pack(&mut s, &dir);
    let item = import(&mut s, &path);
    let undo = s.history.undo.len();
    let r = recipe(
        &mut s,
        json!({"recipe": "sports-highlight", "item": item, "targetSeconds": 20, "aspect": "9:16", "ball": {"point": [20, 18], "at": 5.0}, "wait": true}),
    );
    assert_eq!(r["status"], "done", "{r}");
    let music = steps_of(&r, "assets.place");
    assert_eq!(music.len(), 1, "{r}");
    assert_eq!(music[0]["status"], "done", "{r}");
    assert_eq!(music[0]["params"]["asset"], "mine/music-upbeat-bed");
    assert_eq!(music[0]["params"]["duck"], true);
    // the 9:16 copy is the active sequence, holding the reel
    let shaped = r["shaped"]["sequence"].as_u64().unwrap();
    assert_eq!(s.state.active_sequence.map(|x| x.0), Some(shaped));
    let q = s.active_sequence().unwrap();
    assert_eq!((q.settings.width, q.settings.height), (1080, 1920));
    assert_eq!(q.video_tracks[0].items[0].item.0, r["reel"].as_u64().unwrap());
    // the steps a later build adds are skipped with a note until then
    for c in ["clip.autoReframe", "sports.ballTrail"] {
        let st = steps_of(&r, c);
        assert_eq!(st.len(), 1, "{c}: {r}");
        assert_eq!(st[0]["status"], if exists(c) { "done" } else { "skipped" }, "{c}: {r}");
    }
    assert_eq!(s.history.undo.len(), undo + 1, "one undo step, with the copy");
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.project.sequence(filmagic_project::ItemId(shaped)).is_none());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_comedy_meme_from_a_talking_clip() {
    let dir = tmp_dir("recipe-meme");
    // three phrases with pauses, then a burst of laughter (loud noise) with the square moving
    let mut x = rally_sound(16.0, &[], &[(9.0, 11.0)]);
    for (a, b) in [(1.0, 2.6), (3.6, 5.2), (6.2, 8.4)] {
        voice(&mut x, a, b);
    }
    let path = movie(&dir, "talk", &x, &[(9.0, 11.0)]);
    let mut s = session(&dir.join("data"));
    own_pack(&mut s, &dir);
    let item = import(&mut s, &path);
    let undo = s.history.undo.len();
    let r = recipe(
        &mut s,
        json!({"recipe": "comedy-meme", "item": item, "maxSeconds": 12, "punchText": "BRUH", "voiceover": {"text": "So this happened."}, "wait": true}),
    );
    assert_eq!(r["status"], "done", "{r}");
    let moment = r["moment"]["source"].as_f64().unwrap();
    assert!((8.5..=10.0).contains(&moment), "the laugh: {moment} {r}");
    // dead air cut: several pieces, each in order, the last ending after the moment
    let pieces = r["pieces"].as_array().unwrap();
    assert!(pieces.len() >= 2, "{r}");
    let last = pieces.last().unwrap()["media"]["to"].as_f64().unwrap();
    assert!(last > moment + 1.5, "{r}");
    let total: f64 = pieces.iter().map(|p| p["media"]["to"].as_f64().unwrap() - p["media"]["from"].as_f64().unwrap()).sum();
    assert!(total <= 12.0 + 1e-6, "{total}");
    // the moment freezes and shakes; text; a sound and the meme from the pack; the voice-over
    for c in ["clip.insertFrameHoldSegment", "graphics.newText", "graphics.set", "graphics.anchor", "assets.place", "tts.create"] {
        let st = steps_of(&r, c);
        assert!(!st.is_empty() && st.iter().all(|x| x["status"] == "done"), "{c}: {r}");
    }
    let shake = steps_of(&r, "effects.apply");
    assert_eq!(shake[0]["params"]["effect"], "camera_shake");
    let placed: Vec<&str> = steps_of(&r, "assets.place").iter().map(|x| x["params"]["asset"].as_str().unwrap()).collect();
    assert_eq!(placed, vec!["mine/sfx-boom", "mine/memes-reaction-cat"], "{r}");
    let q = s.active_sequence().unwrap();
    assert!(q.video_tracks[0].items.iter().any(|c| c.frame_hold.is_some()), "a freeze");
    assert_eq!(q.settings.width, W);
    assert_eq!(s.history.undo.len(), undo + 1);
    assert_eq!(s.history.undo.last().unwrap().0, "Recipe: Comedy Meme");
    // deterministic
    s.execute("edit.undo", json!({})).unwrap();
    let again = recipe(
        &mut s,
        json!({"recipe": "comedy-meme", "item": item, "maxSeconds": 12, "punchText": "BRUH", "voiceover": {"text": "So this happened."}, "wait": true}),
    );
    assert_eq!(shape(&again), shape(&r));
    // without packs, cuts or a freeze: said what to add, and the edit is one stretch
    s.execute("edit.undo", json!({})).unwrap();
    let mut bare = Session::default();
    let item = import(&mut bare, &path);
    let r = recipe(&mut bare, json!({"recipe": "comedy-meme", "item": item, "cutPauses": false, "freeze": 0, "topText": false, "wait": true}));
    assert_eq!(r["pieces"].as_array().unwrap().len(), 1, "{r}");
    assert!(steps_of(&r, "clip.insertFrameHoldSegment").is_empty() && steps_of(&r, "graphics.newText").is_empty());
    assert!(notes(&r).contains("meme") && notes(&r).contains("sound"), "{r}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn vertical_recipe_for_a_sequence_an_item_and_a_clip() {
    let dir = tmp_dir("recipe-vertical");
    let path = rally_movie(&dir);
    let mut s = Session::default();
    let item = import(&mut s, &path);
    // an item: a 9:16 sequence holding it, filled from the centre
    let r = recipe(&mut s, json!({"recipe": "vertical", "item": item}));
    assert_eq!(r["status"], "done", "{r}");
    let q = s.active_sequence().unwrap();
    assert_eq!((q.settings.width, q.settings.height), (1080, 1920));
    let clip = &q.video_tracks[0].items[0];
    assert_eq!(clip.item.0, item);
    assert!(steps_of(&r, "clip.fillFrame").len() == 1 || steps_of(&r, "clip.autoReframe").iter().any(|x| x["status"] == "done"), "{r}");
    // a sequence (the active one): a square copy that nests it
    let seq = r["sequence"].as_u64().unwrap();
    let r = recipe(&mut s, json!({"recipe": "vertical", "aspect": "1:1"}));
    let q = s.active_sequence().unwrap();
    assert_eq!((q.settings.width, q.settings.height), (1080, 1080));
    assert_eq!(r["from"]["sequence"], seq);
    // a clip: its own stretch of media
    s.execute("file.newSequence", json!({"name": "M", "fromItem": item})).unwrap();
    let c = s.active_sequence().unwrap().video_tracks[0].items[0].id;
    s.execute("timeline.trim", json!({"clip": c.0, "edge": "in", "mode": "regular", "deltaFrames": 50})).unwrap();
    let r = recipe(&mut s, json!({"recipe": "vertical", "clip": c.0, "aspect": "4:5"}));
    let q = s.active_sequence().unwrap();
    assert_eq!((q.settings.width, q.settings.height), (1080, 1350));
    let it = &q.video_tracks[0].items[0];
    assert_eq!((it.source_in.seconds(), it.start), (2.0, Tick::ZERO), "{r}");
    assert!((it.duration.seconds() - 22.0).abs() < 0.05, "{}", it.duration.seconds());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn list_and_describe_show_every_recipe() {
    let mut s = Session::default();
    let l = s.execute("recipes.list", json!({})).unwrap();
    let ids: Vec<&str> = l["recipes"].as_array().unwrap().iter().map(|r| r["recipe"].as_str().unwrap()).collect();
    assert_eq!(ids, vec!["sports-highlight", "comedy-meme", "vertical"]);
    for r in RECIPES {
        let d = s.execute("recipes.describe", json!({"recipe": r.id})).unwrap();
        assert_eq!(d["steps"].as_array().unwrap().len(), r.steps.len());
        let missing: Vec<&str> = l["recipes"].as_array().unwrap().iter().find(|x| x["recipe"] == r.id).unwrap()["missing"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect();
        assert!(missing.iter().all(|c| LATER.contains(c)), "{missing:?}");
        let _: &Recipe = r;
    }
    // a near spelling is accepted; another word is not
    assert!(s.execute("recipes.describe", json!({"recipe": "Sports Highlight"})).is_ok());
    assert!(s.execute("recipes.describe", json!({"recipe": "sport"})).is_err());
}

#[test]
fn hostile_parameters_are_refused_and_change_nothing() {
    let dir = tmp_dir("recipe-hostile");
    let mut x = rally_sound(6.0, &[1.0, 1.8, 2.6, 3.4], &[]);
    voice(&mut x, 4.0, 5.0);
    let path = movie(&dir, "short", &x, &[(1.0, 3.0)]);
    let mut s = Session::default();
    let item = import(&mut s, &path);
    let wav_item = import(&mut s, &dir.join("short.wav"));
    s.execute("file.newSequence", json!({"name": "S", "fromItem": item})).unwrap();
    let seq = s.state.active_sequence.unwrap().0;
    let long = "x".repeat(5000);
    let cases = [
        json!(null),
        json!([1, 2]),
        json!({}),
        json!({"recipe": 3}),
        json!({"recipe": "no-such"}),
        json!({"recipe": long}),
        json!({"recipe": "sports-highlight"}),
        json!({"recipe": "sports-highlight", "item": item, "clip": 1}),
        json!({"recipe": "sports-highlight", "item": 999_999}),
        json!({"recipe": "sports-highlight", "item": -1}),
        json!({"recipe": "sports-highlight", "item": "1"}),
        json!({"recipe": "sports-highlight", "item": seq}),
        json!({"recipe": "sports-highlight", "clip": 999_999}),
        json!({"recipe": "sports-highlight", "item": item, "targetSeconds": 0}),
        json!({"recipe": "sports-highlight", "item": item, "targetSeconds": 1e300}),
        json!({"recipe": "sports-highlight", "item": item, "targetSeconds": "30"}),
        json!({"recipe": "sports-highlight", "item": item, "maxClips": 1000}),
        json!({"recipe": "sports-highlight", "item": item, "order": "random"}),
        json!({"recipe": "sports-highlight", "item": item, "title": long}),
        json!({"recipe": "sports-highlight", "item": item, "title": "a\u{0007}b"}),
        json!({"recipe": "sports-highlight", "item": item, "music": "nope"}),
        json!({"recipe": "sports-highlight", "item": item, "music": 3}),
        json!({"recipe": "sports-highlight", "item": item, "aspect": "2:1"}),
        json!({"recipe": "sports-highlight", "item": item, "ball": {"point": [1]}}),
        json!({"recipe": "sports-highlight", "item": item, "ball": {"point": [1, 2], "at": -1}}),
        json!({"recipe": "sports-highlight", "item": item, "ball": {"point": [f64::MAX, 2], "at": 1}}),
        json!({"recipe": "sports-highlight", "item": item, "ball": {"point": [1, 2], "at": 1, "size": 0}}),
        json!({"recipe": "sports-highlight", "item": item, "transitionFrames": -2}),
        json!({"recipe": "sports-highlight", "item": item, "mode": "everything"}),
        json!({"recipe": "sports-highlight", "item": item, "cadence": [5, 1]}),
        json!({"recipe": "sports-highlight", "item": item, "fromSeconds": 5, "toSeconds": 1}),
        json!({"recipe": "sports-highlight", "item": item, "typo": 1}),
        json!({"recipe": "sports-highlight", "item": item, "wait": "yes"}),
        json!({"recipe": "sports-highlight", "item": item, "job": "x"}),
        json!({"recipe": "sports-highlight", "item": item, "job": 123_456}),
        json!({"recipe": "comedy-meme", "item": wav_item}),
        json!({"recipe": "comedy-meme", "item": item, "maxSeconds": 0.5}),
        json!({"recipe": "comedy-meme", "item": item, "zoom": 500}),
        json!({"recipe": "comedy-meme", "item": item, "freeze": "1"}),
        json!({"recipe": "comedy-meme", "item": item, "voiceover": "hi"}),
        json!({"recipe": "comedy-meme", "item": item, "voiceover": {"voice": "x"}}),
        json!({"recipe": "comedy-meme", "item": item, "moment": -3}),
        json!({"recipe": "vertical", "aspect": "source"}),
        json!({"recipe": "vertical", "aspect": "16:9"}),
        json!({"recipe": "vertical", "sequence": item}),
        json!({"recipe": "vertical", "item": item, "clip": 3}),
        json!({"recipe": "vertical", "item": wav_item}),
        json!({"recipe": "vertical", "job": 1}),
    ];
    for p in cases {
        let (rev, undo, jobs, items) = (s.revision, s.history.undo.len(), s.jobs.len(), s.project.items.len());
        let e = s.execute("recipes.run", p.clone());
        assert!(e.is_err(), "{p} was accepted: {e:?}");
        let msg = e.unwrap_err().to_string();
        assert!(msg.len() < 1200, "{msg}");
        assert_eq!((s.revision, s.history.undo.len(), s.jobs.len(), s.project.items.len()), (rev, undo, jobs, items), "{p} changed something: {msg}");
    }
    for p in [json!({"recipe": "x"}), json!({"recipe": 1}), json!(null), json!({})] {
        assert!(s.execute("recipes.describe", p).is_err());
    }
    assert!(s.execute("recipes.list", json!({"x": 1})).is_err());
    let _ = std::fs::remove_dir_all(dir);
}
