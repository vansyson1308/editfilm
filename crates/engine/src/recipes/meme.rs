//! `comedy-meme`: from a clip, a punchy short edit with meme overlays and sounds.
//!
//! `story.highlights` (in `peaks` mode) finds the moment: the strongest burst of sound or motion
//! (a laugh, a shout, a crash). The edit builds up to it: the dead air between phrases is cut out
//! (jump cuts from the speech it heard), every other cut punches in, and on the moment the
//! picture freezes, shakes and zooms, with a sound effect and a meme overlay from the installed
//! packs. Top text runs over the build-up, and an optional voice-over is spoken with
//! Text to Speech.

use filmagic_time::Tick;
use serde_json::{Map, Value, json};

use super::follow::{Follow, Pending};
use super::{
    Analysis, Footage, Pick, RUN, Recipe, Steps, analyse, aspect_p, clip_of, extent, find_asset, flag, footage, number, pick_p, picture_change, place_cuts,
    round3, text, ticks_of, time_json,
};
use crate::commands::bad;
use crate::{Result, Session};

pub(super) const RECIPE: Recipe = Recipe {
    id: "comedy-meme",
    title: "Comedy meme edit",
    summary: "From a clip, a punchy short edit that builds to its funniest moment: dead air cut out, punch-in zooms, a freeze with shake on the moment, top text, and a sound effect and meme overlay from the installed packs",
    params: r#"{"item":id|"clip":id (the clip: a project item, or a timeline clip),"fromSeconds":f64?,"toSeconds":f64? (the part to use; at most 600 s apart),"maxSeconds":3..120=20 (the edit's length at most),"moment":seconds? (the moment to build to: media time for an item, sequence time for a clip; default: the loudest laugh or cheer, else the strongest burst story.highlights finds),"cutPauses":bool=true (jump cuts: drop pauses in the speech),"minPause":0.1..3=0.35 (seconds of silence worth cutting),"zoom":100..200=115 (punch-in percent on every other cut; 100: none),"freeze":0..3=1 (seconds the moment freezes; 0: no freeze),"shake":bool=true,"topText":str|false="WAIT FOR IT...","punchText":str|false? (text on the moment),"sound":true|false|"pack/item"=true (a sound effect on the moment from the installed packs),"meme":true|false|"pack/item"=true (a meme overlay on the moment from the installed packs),"voiceover":{"text":str,"voice":str?,"pace":0.5..2?,"language":"vi"?}? (a Text to Speech line at the start; "vi" or a "vieneu:<name>" voice: Vietnamese with VieNeu-TTS, synthesized as a job first; skipped with how to install it when VieNeu is not set up),"voiceJob":id? (set by the resultCommand),"picture":bool=true,"fps":n?,"aspect":"source|9:16|1:1|4:5"="source" (also a copy in that shape),"name":str?,"wait":bool=false,"job":id?}"#,
    steps: &[
        ("story.highlights", "find the moment and the speech (a job)"),
        ("file.newSequence", "the edit, with the clip's frame size and rate"),
        ("timeline.place", "each piece that is kept, back to back"),
        ("effects.setParam", "punch in on every other cut (Motion Scale)"),
        ("clip.insertFrameHoldSegment", "freeze on the moment"),
        ("effects.apply", "shake the freeze (Camera Shake)"),
        ("graphics.newText", "top text over the build-up, and text on the moment"),
        ("graphics.set", "meme lettering: bold white with a black outline"),
        ("graphics.anchor", "the text at the top or bottom of the title-safe area"),
        ("assets.search", "a sound effect and a meme in the installed packs"),
        ("assets.place", "them on the moment"),
        ("tts.voices", "with a Vietnamese `voiceover`: the first VieNeu voice"),
        ("tts.render", "with a Vietnamese `voiceover`: synthesize it first (a job)"),
        ("tts.create", "with `voiceover`: the line, spoken"),
        ("file.newSequence", "with `aspect` and no auto-reframe: a sequence of that shape holding the edit"),
        ("clip.fillFrame", "with `aspect` and no auto-reframe: a centre crop"),
    ],
    later: &[("clip.autoReframe", "with `aspect`: a copy in that shape that follows the subject")],
    keys: &[
        "item",
        "clip",
        "fromSeconds",
        "toSeconds",
        "maxSeconds",
        "moment",
        "cutPauses",
        "minPause",
        "zoom",
        "freeze",
        "shake",
        "topText",
        "punchText",
        "sound",
        "meme",
        "voiceover",
        "voiceJob",
        "picture",
        "fps",
        "aspect",
        "name",
    ],
    analyses: true,
};

const LABEL: &str = "Recipe: Comedy Meme";
/// Longest stretch of footage a meme edit is cut from.
const MAX_SOURCE_SECONDS: f64 = 600.0;
/// Most pieces an edit is cut into.
const MAX_PIECES: usize = 40;
/// Kept after the moment (the payoff), and around each phrase.
const TAIL_SECONDS: f64 = 2.5;
const PAD_SECONDS: f64 = 0.15;
const LEAD_SECONDS: f64 = 0.6;

/// The pieces of the edit (analysis time, in order): up to `max` long, ending just after
/// `moment`. With `speech` (phrases, in order) and `cut`, pauses longer than `min_pause` are cut
/// out; the moment itself and its payoff are always kept whole.
pub(super) fn plan(span: (Tick, Tick), moment: Tick, speech: &[(Tick, Tick)], max: Tick, min_pause: Tick, cut: bool) -> Vec<(Tick, Tick)> {
    let (lo, hi) = span;
    let moment = moment.clamp(lo, hi);
    let end = (moment + Tick::from_seconds_f64(TAIL_SECONDS)).min(hi);
    // look back further than `max`: pauses come out
    let back = Tick(max.0.saturating_mul(3));
    let start = (end - back).max(lo);
    let pad = Tick::from_seconds_f64(PAD_SECONDS);
    let payoff = ((moment - Tick::from_seconds_f64(LEAD_SECONDS)).max(start), end);
    let mut pieces: Vec<(Tick, Tick)> = if cut && !speech.is_empty() {
        let mut v: Vec<(Tick, Tick)> = speech.iter().map(|(a, b)| ((*a - pad).max(start), (*b + pad).min(payoff.0))).filter(|(a, b)| b > a).collect();
        v.push(payoff);
        v.sort_by_key(|x| x.0);
        v
    } else {
        vec![(start, end)]
    };
    // merge what overlaps or is closer than a pause worth cutting
    let mut merged: Vec<(Tick, Tick)> = Vec::new();
    for (a, b) in pieces.drain(..) {
        match merged.last_mut() {
            Some(last) if a - last.1 < min_pause => last.1 = last.1.max(b),
            _ => merged.push((a, b)),
        }
    }
    // at most MAX_PIECES: close the smallest gaps first
    while merged.len() > MAX_PIECES {
        let Some(k) = merged.windows(2).enumerate().min_by_key(|(k, w)| (w[1].0 - w[0].1, *k)).map(|(k, _)| k) else { break };
        let next = merged.remove(k + 1);
        if let Some(m) = merged.get_mut(k) {
            m.1 = next.1;
        }
    }
    // at most `max` long: drop from the front
    let mut total: Tick = merged.iter().fold(Tick::ZERO, |t, (a, b)| t + (*b - *a));
    while total > max {
        let excess = total - max;
        let n = merged.len();
        let Some(first) = merged.first_mut() else { break };
        let (a, b) = *first;
        if b - a <= excess && n > 1 {
            merged.remove(0);
            total -= b - a;
        } else {
            // cut into the piece, but not mid-phrase: at the next phrase that starts in it
            let cut = (a + excess).min(b);
            let in_phrase = speech.iter().any(|g| g.0 - pad < cut && cut < g.1 + pad);
            let next = speech.iter().map(|g| (g.0 - pad).max(a)).filter(|x| *x >= cut && *x < b).min();
            match next.filter(|_| in_phrase) {
                Some(x) => {
                    first.0 = x;
                    total -= x - a;
                }
                // the cut is in the piece's last phrase: leave that phrase out whole
                None if in_phrase && n > 1 => {
                    merged.remove(0);
                    total -= b - a;
                }
                None => {
                    first.0 = cut;
                    total -= cut - a;
                }
            }
        }
    }
    merged.retain(|(a, b)| b > a);
    merged
}

fn seconds_list(v: &Value) -> Vec<(Tick, Tick)> {
    v.as_array()
        .into_iter()
        .flatten()
        .take(2000)
        .filter_map(|g| {
            let (a, b) = (g["start"].as_f64()?, g["end"].as_f64()?);
            (a.is_finite() && b.is_finite() && b > a && a >= 0.0).then(|| (Tick::from_seconds_f64(a), Tick::from_seconds_f64(b)))
        })
        .collect()
}

/// Where the loudest burst of crowd-like noise (a laugh, a cheer) starts: the moment a comedy
/// edit builds to, when there is one.
fn strongest_cheer(r: &Value) -> Option<Tick> {
    r["cheers"]
        .as_array()?
        .iter()
        .filter_map(|c| Some((c["riseDb"].as_f64().filter(|x| x.is_finite())?, c["start"].as_f64().filter(|x| x.is_finite() && *x >= 0.0)?)))
        .max_by(|a, b| a.0.total_cmp(&b.0).then(b.1.total_cmp(&a.1)))
        .map(|(_, t)| Tick::from_seconds_f64(t))
}

struct Voice {
    text: String,
    voice: Option<String>,
    pace: Option<f64>,
    /// `"vi"`: a Vietnamese voice (VieNeu-TTS), the first installed one unless `voice` names one.
    vietnamese: bool,
}

impl Voice {
    fn is_vieneu(&self) -> bool {
        self.vietnamese || self.voice.as_deref().is_some_and(|v| v.starts_with("vieneu"))
    }
}

/// How the voice-over can be spoken in the build.
enum Speak {
    /// `tts.create` speaks it (a built-in or natural voice, or a VieNeu take already synthesized).
    Create,
    /// It cannot be spoken here, and why.
    Skip(String),
}

/// What to say when VieNeu-TTS cannot speak.
const VIENEU_HELP: &str = "Vietnamese voices need VieNeu-TTS: tts.engines shows what is missing, and tts.setupVieneu {\"install\": true} installs it (a Python environment and the model, about 0.6 GB); then run the recipe again";

/// Get a VieNeu voice-over ready before the edit is built: pick the voice and start its synthesis
/// (`tts.render`, a job; inline with `wait`). `Ok(Some(job))` is the job to wait for.
fn prepare_voice(s: &mut Session, v: &mut Voice, wait: bool) -> std::result::Result<Option<u64>, String> {
    if v.voice.is_none() {
        let list = s.execute("tts.voices", json!({})).map_err(|e| e.to_string())?;
        let first = list["voices"].as_array().into_iter().flatten().filter_map(|x| x["id"].as_str()).find(|id| id.starts_with("vieneu:")).map(str::to_string);
        match first {
            Some(id) => v.voice = Some(id),
            None => return Err(format!("no Vietnamese voice is installed. {VIENEU_HELP}")),
        }
    }
    let mut rp = json!({"text": v.text, "voice": v.voice, "wait": wait});
    if let Some(x) = v.pace {
        rp["pace"] = json!(x);
    }
    let r = s.execute("tts.render", rp).map_err(|e| format!("{e}. {VIENEU_HELP}"))?;
    Ok(r["job"].as_u64())
}

/// Where the synthesis job `job` (one the recipe started) stands: still running (`None`), or how
/// the build speaks.
fn voice_state(s: &Session, job: u64) -> Option<Speak> {
    use std::sync::atomic::Ordering;
    // pruned: tts.create finds the take in its cache, or says to synthesize it
    let Some(j) = s.jobs.iter().find(|j| j.id == job) else { return Some(Speak::Create) };
    if !j.progress.finished.load(Ordering::Relaxed) {
        return None;
    }
    let failed = j.result.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref().and_then(|r| r.as_ref().err().cloned());
    Some(match failed {
        Some(e) => Speak::Skip(format!("the Vietnamese voice-over could not be synthesized: {e}. {VIENEU_HELP}")),
        None => Speak::Create,
    })
}

fn voice_p(p: &Value) -> Result<Option<Voice>> {
    let Some(v) = p.get("voiceover").filter(|v| !v.is_null() && **v != json!(false)) else { return Ok(None) };
    if !v.is_object() {
        return Err(bad(RUN, "`voiceover` must be {\"text\": str, \"voice\": str?, \"pace\": 0.5..2?} (tts.voices lists the voices)"));
    }
    let Some(text) = text(v, "text", 2000, None, RUN)? else {
        return Err(bad(RUN, "`voiceover` needs `text`: the line to speak, at most 2000 characters"));
    };
    let voice = super::text(v, "voice", 120, None, RUN)?;
    let pace = match v.get("pace") {
        None | Some(Value::Null) => None,
        Some(_) => Some(number(v, "pace", 0.5, 2.0, 1.0, RUN)?),
    };
    let vietnamese = match v.get("language") {
        None | Some(Value::Null) => false,
        Some(Value::String(l)) if l == "vi" || l == "vi-VN" => true,
        Some(Value::String(l)) if l.len() <= 16 && voice.as_deref().is_none_or(|x| !x.starts_with("vieneu")) => {
            return Err(bad(
                RUN,
                format!(
                    "`voiceover.language` \"{l}\": only \"vi\" (Vietnamese, VieNeu-TTS) is chosen this way; for another language name a `voice` (tts.voices)"
                ),
            ));
        }
        Some(_) => return Err(bad(RUN, "`voiceover.language` must be \"vi\", or left out")),
    };
    if let Some(k) = v.as_object().and_then(|o| o.keys().find(|k| !["text", "voice", "pace", "language"].contains(&k.as_str()))) {
        return Err(bad(RUN, format!("`voiceover.{}` is not a voice-over parameter: text, voice, pace, language", super::clip_text(k, 40))));
    }
    Ok(Some(Voice { text, voice, pace, vietnamese }))
}

/// What a resultCommand adds to the parameters: the analysis `job`, and the chosen Vietnamese
/// voice and its synthesis job.
fn voice_extra(o: &Opts, voice_job: Option<u64>, job: Option<u64>) -> Vec<(&'static str, Value)> {
    let mut out = vec![("job", job.map_or(Value::Null, |j| json!(j)))];
    if let Some(vj) = voice_job {
        out.push(("voiceJob", json!(vj)));
    }
    if let Some(v) = o.voice.as_ref().filter(|v| v.is_vieneu())
        && let Some(id) = &v.voice
    {
        let mut vo = json!({"text": v.text, "voice": id});
        if let Some(x) = v.pace {
            vo["pace"] = json!(x);
        }
        out.push(("voiceover", vo));
    }
    out
}

struct Opts {
    max: Tick,
    moment: Option<Tick>,
    cut: bool,
    min_pause: Tick,
    zoom: f64,
    freeze: f64,
    shake: bool,
    top: Option<String>,
    punch: Option<String>,
    sound: Pick,
    meme: Pick,
    voice: Option<Voice>,
    speak: Speak,
    aspect: Option<&'static str>,
    name: String,
}

pub(super) fn run(s: &mut Session, p: &Value, wait: bool, job: Option<u64>) -> Result<Value> {
    let f = footage(s, p, RUN)?;
    if !f.video {
        return Err(bad(RUN, "a meme edit needs a picture: this footage has only sound"));
    }
    let mut o = Opts {
        max: Tick::from_seconds_f64(number(p, "maxSeconds", 3.0, 120.0, 20.0, RUN)?),
        moment: match p.get("moment") {
            None | Some(Value::Null) => None,
            Some(_) => Some(Tick::from_seconds_f64(number(p, "moment", 0.0, 1e7, 0.0, RUN)?)),
        },
        cut: flag(p, "cutPauses", true, RUN)?,
        min_pause: Tick::from_seconds_f64(number(p, "minPause", 0.1, 3.0, 0.35, RUN)?),
        zoom: number(p, "zoom", 100.0, 200.0, 115.0, RUN)?,
        freeze: number(p, "freeze", 0.0, 3.0, 1.0, RUN)?,
        shake: flag(p, "shake", true, RUN)?,
        top: text(p, "topText", 120, Some("WAIT FOR IT..."), RUN)?,
        punch: text(p, "punchText", 120, None, RUN)?,
        sound: pick_p(p, "sound", Pick::Auto, RUN)?,
        meme: pick_p(p, "meme", Pick::Auto, RUN)?,
        voice: voice_p(p)?,
        speak: Speak::Create,
        aspect: aspect_p(p, "aspect", None, RUN)?,
        name: text(p, "name", 120, None, RUN)?.unwrap_or_else(|| format!("{} Meme", f.name)),
    };
    // bounded cost: a meme is cut from at most ten minutes
    let sec = |k: &str| p.get(k).and_then(Value::as_f64).filter(|x| x.is_finite());
    let from = sec("fromSeconds").map_or(f.span.0, Tick::from_seconds_f64).clamp(f.span.0, f.span.1);
    let to = sec("toSeconds").map_or(f.span.1, Tick::from_seconds_f64).clamp(f.span.0, f.span.1);
    if (to - from).seconds() > MAX_SOURCE_SECONDS {
        return Err(bad(
            RUN,
            format!(
                "a meme edit is cut from at most {MAX_SOURCE_SECONDS:.0} s and this footage is {:.0} s: pass `fromSeconds` and `toSeconds` around the part to use",
                (to - from).seconds()
            ),
        ));
    }
    let mut hp = Map::new();
    for k in ["fromSeconds", "toSeconds", "picture", "fps"] {
        if let Some(v) = p.get(k).filter(|v| !v.is_null()) {
            hp.insert(k.into(), v.clone());
        }
    }
    hp.insert("mode".into(), json!("peaks"));
    hp.insert("limit".into(), json!(5));
    hp.insert("minSeconds".into(), json!(0.3));
    hp.insert("maxSeconds".into(), json!(10));
    let mut voice_job = match p.get("voiceJob") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_u64().ok_or_else(|| bad(RUN, "`voiceJob` must be a job id (the resultCommand sets it)"))?),
    };
    if let Some(vj) = voice_job {
        if !o.voice.as_ref().is_some_and(Voice::is_vieneu) {
            return Err(bad(RUN, "`voiceJob` is only for a Vietnamese `voiceover`: leave it out"));
        }
        if !s.recipes.voice_jobs.contains(&vj) {
            return Err(bad(RUN, format!("`voiceJob` {vj} is no voice-over this recipe started: leave it out (the resultCommand sets it)")));
        }
    }
    // the analysis first: it checks its own parameters, and nothing is synthesized for a call it refuses
    let analysis = analyse(s, &f, Value::Object(hp), wait, job)?;
    // a Vietnamese voice-over is synthesized as a job beside the analysis
    if voice_job.is_none()
        && let Some(v) = o.voice.as_mut().filter(|v| v.is_vieneu())
    {
        match prepare_voice(s, v, wait) {
            Ok(j) => {
                voice_job = j;
                if let Some(j) = j {
                    s.recipes.voice_jobs.push_back(j);
                    while s.recipes.voice_jobs.len() > 32 {
                        s.recipes.voice_jobs.pop_front();
                    }
                }
            }
            Err(why) => o.speak = Speak::Skip(why),
        }
    }
    let r = match analysis {
        Analysis::Running(j) => return Ok(super::running_with(RECIPE.id, p, j, &voice_extra(&o, voice_job, Some(j)))),
        Analysis::Ready(r) => r,
    };
    if let Some(vj) = voice_job {
        match voice_state(s, vj) {
            Some(sp) => o.speak = sp,
            None => return Ok(super::running_with(RECIPE.id, p, vj, &voice_extra(&o, voice_job, job))),
        }
    }
    let mut d = Pending::new(s, &RECIPE, LABEL, finish);
    d.cache = job.map(|j| (j, super::follow::essence(p)));
    s.edit_group(LABEL, |s| build(s, &mut d, &f, &r, o))?;
    super::follow::advance(s, d, wait)
}

fn build(s: &mut Session, d: &mut Pending, f: &Footage, r: &Value, o: Opts) -> Result<()> {
    let mut st = Steps { notes: f.notes.clone(), ..Default::default() };
    st.record("story.highlights", &f.target, "find the moment and the speech", "done");
    let span = (r["span"]["from"].as_f64().map_or(f.span.0, Tick::from_seconds_f64), r["span"]["to"].as_f64().map_or(f.span.1, Tick::from_seconds_f64));
    let moment = match o.moment {
        Some(m) => m,
        None => match strongest_cheer(r).or_else(|| r["segments"].get(0).and_then(|g| ticks_of(&g["start"]))) {
            Some(m) => m,
            None => {
                st.notes.push(
                    "no moment stood out (no burst of sound or motion): the edit builds to two thirds of the way in; pass `moment` (seconds) to choose it"
                        .into(),
                );
                span.0 + Tick((span.1 - span.0).0 / 3 * 2)
            }
        },
    };
    let speech = seconds_list(&r["speech"]);
    if o.cut && speech.is_empty() {
        st.notes.push("no speech was heard, so no pauses were cut: the edit is one stretch before the moment".into());
    }
    let cuts = plan(span, moment, &speech, o.max, o.min_pause, o.cut);
    if cuts.is_empty() {
        return Err(bad(RUN, "nothing to cut: the footage is shorter than a frame around the moment. Nothing was changed"));
    }
    let made =
        st.must(s, "the edit, with the clip's frame size and rate", "file.newSequence", json!({"name": o.name, "fromItem": f.item.0, "place": false}))?;
    let edit_seq = made["sequence"].as_u64().map(filmagic_project::ItemId).ok_or_else(|| bad(RUN, "file.newSequence answered no sequence"))?;
    let placed = place_cuts(s, &mut st, f, &cuts, "piece")?;
    let rate = s.sequence_rate();
    let (a0, b0) = extent(s);
    d.looks = Some((crate::story::looks_across(s, a0, b0), a0, b0));
    // punch in on every other cut
    let zoomed: Vec<u64> = placed.iter().skip(1).step_by(2).filter_map(|pl| pl.video.map(|v| v.0)).collect();
    if o.zoom > 100.0 && !zoomed.is_empty() {
        st.may(s, "punch in on every other cut", "effects.setParam", json!({"clips": zoomed, "effect": "motion", "param": "scale", "value": o.zoom}));
    }
    // the moment in the edit: the piece holding it (the last, by the plan)
    let m_media = f.media_at(moment);
    let at_moment = placed.iter().rev().find(|pl| pl.media_in <= m_media && m_media < pl.media_out).or(placed.last()).and_then(|pl| {
        let it = clip_of(s, pl.video?)?;
        Some((it.id, rate.snap(it.start + (m_media.clamp(pl.media_in, pl.media_out) - pl.media_in))))
    });
    let mut hit_at = at_moment.map(|x| x.1).unwrap_or(Tick::ZERO);
    let mut hold_len = Tick::from_seconds_f64(1.0);
    if let Some((clip, t)) = at_moment {
        if o.freeze > 0.0 {
            if let Some(v) = st.may(s, "freeze on the moment", "clip.insertFrameHoldSegment", json!({"clip": clip.0, "time": t.0, "seconds": round3(o.freeze)}))
            {
                let hold = v["clip"].as_u64();
                hold_len = v["duration"].as_i64().map_or(hold_len, Tick);
                if let Some(hold) = hold {
                    if o.shake {
                        st.may(s, "shake the freeze", "effects.apply", json!({"clips": [hold], "effect": "camera_shake"}));
                    }
                    st.may(
                        s,
                        "zoom on the freeze",
                        "effects.setParam",
                        json!({"clip": hold, "effect": "motion", "param": "scale", "value": round3((o.zoom + 15.0).min(220.0))}),
                    );
                    st.next("edit.rippleDelete", json!({"clips": [hold]}), "no freeze");
                }
            }
        } else if o.shake {
            st.skip("effects.apply", json!({"effect": "camera_shake"}), "shake the freeze", "there is no freeze (`freeze` is 0)");
        }
        hit_at = t;
    }
    // sound and meme on the moment, from the installed packs
    let sound = match &o.sound {
        Pick::Off => None,
        Pick::Asset(a) => Some(a.clone()),
        Pick::Auto => find_asset(s, &mut st, json!("sfx"), &["boom", "impact", "hit", "scratch"], "a sound effect in the installed packs"),
    };
    match sound {
        Some(a) => {
            st.may(s, "a sound effect on the moment", "assets.place", json!({"asset": a, "time": hit_at.0, "audioTrack": "new"}));
        }
        None if o.sound != Pick::Off => {
            st.notes.push("no sound effect: no installed pack has one. assets.catalog lists the CC0 sound packs (assets.install {\"catalog\": id}); then assets.search {\"kind\": \"sfx\"}".into());
            st.next("assets.search", json!({"kind": "sfx", "query": "boom"}), "a sound effect for the moment, once a pack is installed");
        }
        None => {}
    }
    let meme = match &o.meme {
        Pick::Off => None,
        Pick::Asset(a) => Some(a.clone()),
        Pick::Auto => find_asset(s, &mut st, json!(["meme-gif", "meme-video", "meme-image"]), &["reaction"], "a meme in the installed packs"),
    };
    match meme {
        Some(a) => {
            st.may(
                s,
                "a meme overlay on the moment",
                "assets.place",
                json!({"asset": a, "time": hit_at.0, "duration": (hold_len + Tick::from_seconds_f64(0.5)).0, "track": "new"}),
            );
        }
        None if o.meme != Pick::Off => {
            st.notes.push(
                "no meme overlay: no meme pack is installed (none ships with Filmagic). Build one from a folder of memes you may use with assets.buildPack {\"from\": folder, \"license\": …}, then assets.search {\"kind\": \"meme-gif\"}"
                    .into(),
            );
            st.next(
                "assets.buildPack",
                json!({"from": "<folder of your memes>", "license": "<their licence>", "install": true}),
                "a meme pack from your own files",
            );
        }
        None => {}
    }
    if let (Some(v), Speak::Skip(why)) = (&o.voice, &o.speak) {
        st.skip("tts.create", json!({"text": v.text, "voice": v.voice}), "the voice-over, spoken", why);
        st.next("tts.engines", json!({}), "what the Vietnamese voices need");
        st.next("tts.setupVieneu", json!({"install": true, "wait": true}), "install VieNeu-TTS, then run the recipe again");
    } else if let Some(v) = &o.voice {
        let mut tp = json!({"text": v.text, "time": 0});
        if let Some(x) = &v.voice {
            tp["voice"] = json!(x);
        }
        if let Some(x) = v.pace {
            tp["pace"] = json!(x);
        }
        if st.may(s, "the voice-over, spoken", "tts.create", tp).is_none() {
            st.next("tts.voices", json!({}), "the voices this build has (tts.downloadVoices for the natural ones)");
        }
    }
    let extra = &mut d.extra;
    extra.insert("edit".into(), json!(edit_seq.0));
    extra.insert("footage".into(), f.target.clone());
    extra.insert("moment".into(), json!({"source": round3(moment.seconds()), "edit": time_json(hit_at)}));
    extra.insert(
        "pieces".into(),
        json!(
            placed
                .iter()
                .map(|pl| json!({"clip": pl.video.map(|c| c.0), "media": {"from": round3(pl.media_in.seconds()), "to": round3(pl.media_out.seconds())}}))
                .collect::<Vec<_>>()
        ),
    );
    if let Some(asp) = o.aspect {
        // the pieces are reframed (a freeze cannot be: it is made to fill the frame after)
        let clips: Vec<u64> = placed.iter().filter_map(|pl| pl.video.map(|c| c.0)).collect();
        d.then(Follow::Reframe { seq: edit_seq, aspect: asp, name: format!("{} {asp}", o.name), graphics: "follow", clips: Some(clips) });
    }
    d.ctx = json!({
        "edit": edit_seq.0,
        "top": o.top,
        "punch": o.punch,
        "hit": hit_at.0,
        "hold": hold_len.0,
        "zoom": o.zoom,
    });
    let mut again = f.target.clone();
    again["recipe"] = json!(RECIPE.id);
    again["moment"] = json!(round3(moment.seconds()));
    again["cutPauses"] = json!(false);
    st.next(super::RUN, again, "the same moment without jump cuts (edit.undo first)");
    st.next(
        "preview.motion",
        json!({"path": "meme.gif", "fromSeconds": round3((hit_at.seconds() - 2.0).max(0.0)), "toSeconds": round3(hit_at.seconds() + 2.0)}),
        "watch the moment",
    );
    st.next("layout.check", json!({}), "check the text stays readable and inside the safe area");
    st.next("file.exportMedia", json!({"path": "meme.mp4", "draft": true, "wait": true}), "a quick review copy");
    let notes = std::mem::take(&mut d.steps);
    d.steps = merge_steps(notes, st);
    Ok(())
}

/// Steps recorded before (`a`) and after (`b`), as one list.
fn merge_steps(a: Steps, b: Steps) -> Steps {
    let mut out = a;
    for mut x in b.list {
        x["step"] = json!(out.list.len() + 1);
        out.list.push(x);
    }
    out.warnings.extend(b.warnings);
    out.notes.extend(b.notes);
    out.next.extend(b.next);
    out
}

/// Meme lettering on the active sequence: bold white with a black outline, sized to the frame's
/// width, kept in the title-safe area.
fn lettering(s: &mut Session, st: &mut Steps, words: &str, from: Tick, len: Tick, anchor: &str, why: &str) -> Option<u64> {
    let rate = s.sequence_rate();
    let (w, h) = s.active_sequence().map(|q| (f64::from(q.settings.width), f64::from(q.settings.height)))?;
    let chars = words.chars().count().max(1) as f64;
    // about 0.6 em a character: the words fit the width with a margin
    let size = round3((h * 0.085).min(0.9 * w / (0.6 * chars)).clamp(12.0, 400.0));
    let len = len.max(rate.frame_duration());
    let v = st.may(
        s,
        why,
        "graphics.newText",
        json!({"text": words, "time": from.0, "seconds": round3(len.seconds()), "size": size, "track": "auto", "position": [w / 2.0, h / 2.0]}),
    )?;
    let clip = v["clip"].as_u64()?;
    st.may(
        s,
        "meme lettering: bold white with a black outline",
        "graphics.set",
        json!({"clip": clip, "props": {"font_style": "Bold", "align": "center", "fill_color": "#ffffff", "stroke": true, "stroke_width": round3((size / 12.0).max(3.0)), "shadow": false}, "pictureChange": false}),
    );
    st.may(s, "keep the text in the title-safe area", "graphics.anchor", json!({"clip": clip, "anchor": anchor, "safe": "title"}));
    super::fit(s, st, clip, anchor);
    Some(clip)
}

/// After the reframe job: in the copy the freeze fills the frame (it is not reframed), and the
/// lettering goes on in each sequence, sized to it.
fn finish(s: &mut Session, d: &mut Pending) -> Result<()> {
    let tick = |k: &str| Tick(d.ctx[k].as_i64().unwrap_or(0));
    let (hit, hold) = (tick("hit"), tick("hold"));
    let zoom = d.ctx["zoom"].as_f64().unwrap_or(115.0);
    let (top, punch) = (d.ctx["top"].as_str().map(str::to_string), d.ctx["punch"].as_str().map(str::to_string));
    let edit = d.ctx["edit"].as_u64().map(filmagic_project::ItemId);
    let mut seqs: Vec<filmagic_project::ItemId> = edit.into_iter().collect();
    if let Some(copy) = d.copy {
        seqs.push(copy);
    }
    for (k, seq) in seqs.iter().enumerate() {
        super::follow::open_seq(s, *seq);
        let st = &mut d.steps;
        if k > 0 {
            let holds: Vec<u64> = s
                .active_sequence()
                .map(|q| q.video_tracks.iter().flat_map(|t| t.items.iter()).filter(|i| i.frame_hold.is_some()).map(|i| i.id.0).collect())
                .unwrap_or_default();
            if !holds.is_empty() && st.may(s, "the freeze fills the new frame", "clip.fillFrame", json!({"clips": holds})).is_some() {
                for h in holds {
                    let scale = clip_of(s, filmagic_project::ClipId(h))
                        .and_then(|it| it.effect("motion").map(|m| m.f64_at("scale", it.start)))
                        .filter(|x| x.is_finite() && *x > 0.0)
                        .unwrap_or(100.0);
                    st.may(
                        s,
                        "zoom on the freeze, covering the frame",
                        "effects.setParam",
                        json!({"clip": h, "effect": "motion", "param": "scale", "value": round3(scale * (zoom + 15.0) / 100.0)}),
                    );
                }
            }
        }
        let why = |w: &'static str| if k == 0 { w } else { "the same text in the copy, sized to it" };
        if let Some(t) = &top
            && let Some(c) = lettering(s, st, t, Tick::ZERO, hit, "top", why("top text over the build-up"))
            && k == 0
        {
            st.next("graphics.setText", json!({"clip": c, "text": "…"}), "your own top text");
        }
        if let Some(t) = &punch {
            lettering(s, st, t, hit, hold + Tick::from_seconds_f64(0.5), "bottom", why("text on the moment"));
        }
        if k == 0
            && let Some((before, a, b)) = d.looks.take()
        {
            d.extra.insert("pictureChange".into(), picture_change(s, &before, a, b.max(extent(s).1)));
        }
    }
    if let Some(copy) = d.copy {
        d.extra.insert("shaped".into(), json!({"sequence": copy.0, "size": s.project.sequence(copy).map(|q| (q.settings.width, q.settings.height))}));
        d.steps.notes.push(format!("the active sequence is the reframed copy; the edit itself is sequence {}", edit.map_or(0, |e| e.0)));
    }
    Ok(())
}
