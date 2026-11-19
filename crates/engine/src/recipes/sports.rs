//! `sports-highlight`: from a long rally recording, a short highlight reel.
//!
//! `story.highlights` scores the footage (rallies of impacts at a cadence, cheering, motion, a
//! scoreboard that changes). The best moments are taken by score until the reel reaches its
//! target length, cut from their suggested In and Out (which do not cut into a rally and keep the
//! cheering), and placed in time order in a new sequence. The best one ends in slow motion, cuts
//! are softened, each moment gets a marker, and the reel opens on a title. Music is laid under it
//! and ducked when a music pack is installed. A ball trail (with a `ball` seed) and a reframed
//! copy (`aspect`) come from the sports commands where this build has them, as follow-on
//! jobs folded into the same undo step.

use filmagic_time::Tick;
use serde_json::{Map, Value, json};

use filmagic_project::ItemId;

use super::follow::{Follow, Pending};
use super::{
    Analysis, Footage, Pick, Placed, RUN, Recipe, Steps, analyse, aspect_p, clip_of, extent, find_asset, flag, footage, number, pick_p, picture_change,
    place_cuts, round3, running, text, ticks_of, time_json, whole,
};
use crate::commands::bad;
use crate::{Result, Session};

pub(super) const RECIPE: Recipe = Recipe {
    id: "sports-highlight",
    title: "Sports highlight reel",
    summary: "From a long rally recording, a short highlight reel: the best rallies by sound and picture, in order, with slow motion on the best, soft cuts, markers, a title, and music when a pack is installed",
    params: r#"{"item":id|"clip":id (the recording: a project item, or a timeline clip),"fromSeconds":f64?,"toSeconds":f64? (the part to look in),"targetSeconds":5..300=30 (reel length aimed at),"maxClips":1..20=6,"order":"time|score"="time","mode":"rally|peaks|auto"="rally","cadence":[min,max]?,"minHits":n?,"picture":bool=true,"fps":n? (story.highlights' own),"name":str? (the reel's name),"title":str|false="HIGHLIGHTS","slowMo":bool=true (the best moment ends at 50 %),"transitionFrames":0..60=8 (0: straight cuts),"markers":bool=true,"music":true|false|"pack/item"=true (true: the first music in the installed packs, ducked),"aspect":"source|9:16|1:1|4:5"="source" (also a copy in that shape, made last: it follows the action with clip.autoReframe, and the title and ball trail follow into it),"vertical":bool=false (the same as "aspect":"9:16"),"ball":{"point":[x,y],"at":seconds,"size":px=24}? (the ball at `at` in the recording, in its pixels: a trail behind it),"wait":bool=false,"job":id?}"#,
    steps: &[
        ("story.highlights", "find and score the moments (a job)"),
        ("file.newSequence", "the reel, with the recording's frame size and rate"),
        ("timeline.place", "each moment, back to back"),
        ("clip.speedRamp", "slow motion at the end of the best moment"),
        ("sequence.applyVideoTransition", "soften each cut"),
        ("sequence.applyAudioTransition", "soften each cut's sound"),
        ("markers.add", "a marker on each moment, with its score and cues"),
        ("graphics.template.apply", "an opening title"),
        ("assets.search", "music in the installed packs"),
        ("assets.place", "the music under the reel, ducked"),
        ("file.newSequence", "with `aspect` and no auto-reframe: a sequence of that shape holding the reel"),
        ("clip.fillFrame", "with `aspect` and no auto-reframe: a centre crop"),
    ],
    later: &[
        ("clip.autoReframe", "with `aspect`: a copy in that shape that follows the action"),
        ("sports.ballTrail", "with `ball`: a fading trail behind the ball"),
    ],
    keys: &[
        "item",
        "clip",
        "fromSeconds",
        "toSeconds",
        "targetSeconds",
        "maxClips",
        "order",
        "mode",
        "cadence",
        "minHits",
        "picture",
        "fps",
        "name",
        "title",
        "slowMo",
        "transitionFrames",
        "markers",
        "music",
        "aspect",
        "vertical",
        "ball",
    ],
    analyses: true,
};

const LABEL: &str = "Recipe: Sports Highlight";
/// A moment shorter than this is not worth a cut of its own.
const MIN_CUT_SECONDS: f64 = 1.5;

/// A moment `story.highlights` found, in analysis time.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Seg {
    pub rank: u64,
    pub score: f64,
    pub a: Tick,
    pub b: Tick,
    pub cues: Value,
    pub kind: Value,
}

pub(super) fn segments(r: &Value) -> Vec<Seg> {
    r["segments"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| {
            let a = ticks_of(&v["suggestedIn"]).or_else(|| ticks_of(&v["start"]))?;
            let b = ticks_of(&v["suggestedOut"]).or_else(|| ticks_of(&v["end"]))?;
            (b > a).then(|| Seg {
                rank: v["rank"].as_u64().unwrap_or(0),
                score: v["score"].as_f64().filter(|x| x.is_finite()).unwrap_or(0.0),
                a,
                b,
                cues: v["cues"].clone(),
                kind: v["kind"].clone(),
            })
        })
        .collect()
}

/// The moments of the reel: by score, until `target` is reached (up to a fifth more) or `max`
/// are taken; one that would run long is skipped unless nothing is taken yet, when it is cut to
/// `target` keeping its end (the cheering). Overlapping moments are taken once. Then in time
/// order, or by score.
pub(super) fn choose(segs: &[Seg], target: Tick, max: usize, by_time: bool) -> Vec<Seg> {
    let mut ranked: Vec<Seg> = segs.to_vec();
    ranked.sort_by(|x, y| y.score.total_cmp(&x.score).then(x.rank.cmp(&y.rank)).then(x.a.cmp(&y.a)));
    let roof = target + Tick(target.0 / 5);
    let shortest = Tick::from_seconds_f64(MIN_CUT_SECONDS);
    let mut out: Vec<Seg> = Vec::new();
    let mut total = Tick::ZERO;
    for mut g in ranked {
        if out.len() >= max || total >= target {
            break;
        }
        if out.iter().any(|o| g.a < o.b && o.a < g.b) {
            continue;
        }
        let len = g.b - g.a;
        if total + len > roof {
            // cut from its start to what is left, keeping its end (the cheering), when enough is
            // left to be worth a cut; the first moment is always cut to the target
            let left = if out.is_empty() { target } else { target - total };
            if left < shortest.max(Tick(len.0 / 2)) && !out.is_empty() {
                continue;
            }
            g.a = g.b - left;
        }
        if g.b - g.a < shortest && !out.is_empty() {
            continue;
        }
        total += g.b - g.a;
        out.push(g);
    }
    if by_time {
        out.sort_by_key(|g| g.a);
    }
    out
}

/// The `ball` seed: where the ball is at a moment of the recording (its pixels, media seconds).
struct Ball {
    point: [f64; 2],
    size: f64,
    at: Tick,
}

fn ball_p(p: &Value) -> Result<Option<Ball>> {
    let Some(b) = p.get("ball").filter(|v| !v.is_null()) else { return Ok(None) };
    if let Some(k) = b.as_object().and_then(|o| o.keys().find(|k| !["point", "at", "size"].contains(&k.as_str()))) {
        return Err(bad(RUN, format!("`ball.{}` is not a ball parameter: point, at, size", super::clip_text(k, 40))));
    }
    let wrong =
        || bad(RUN, "`ball` must be {\"point\":[x,y] (the ball's centre, the recording's pixels),\"at\":seconds (when, in the recording),\"size\":px?}");
    let point = b
        .get("point")
        .and_then(Value::as_array)
        .filter(|a| a.len() == 2)
        .and_then(|a| Some([a.first()?.as_f64()?, a.get(1)?.as_f64()?]))
        .filter(|[x, y]| x.is_finite() && y.is_finite() && (0.0..=100_000.0).contains(x) && (0.0..=100_000.0).contains(y))
        .ok_or_else(wrong)?;
    let at = b.get("at").and_then(Value::as_f64).filter(|x| x.is_finite() && (0.0..=1e7).contains(x)).ok_or_else(wrong)?;
    let size = number(b, "size", 2.0, 400.0, 24.0, RUN)?;
    Ok(Some(Ball { point, size, at: Tick::from_seconds_f64(at) }))
}

pub(super) fn run(s: &mut Session, p: &Value, wait: bool, job: Option<u64>) -> Result<Value> {
    let f = footage(s, p, RUN)?;
    if !f.video && !f.audio {
        return Err(bad(RUN, "the footage has neither picture nor sound"));
    }
    let target = Tick::from_seconds_f64(number(p, "targetSeconds", 5.0, 300.0, 30.0, RUN)?);
    let max = whole(p, "maxClips", 1, 20, 6, RUN)? as usize;
    let by_time = match p.get("order") {
        None | Some(Value::Null) => true,
        Some(Value::String(o)) if o == "time" => true,
        Some(Value::String(o)) if o == "score" => false,
        Some(_) => return Err(bad(RUN, "`order` must be \"time\" or \"score\"")),
    };
    let title = text(p, "title", 80, Some("HIGHLIGHTS"), RUN)?;
    let name = text(p, "name", 120, None, RUN)?.unwrap_or_else(|| format!("{} Highlights", f.name));
    let slow = flag(p, "slowMo", true, RUN)?;
    let frames = whole(p, "transitionFrames", 0, 60, 8, RUN)?;
    let markers = flag(p, "markers", true, RUN)?;
    let music = pick_p(p, "music", Pick::Auto, RUN)?;
    let aspect = match (aspect_p(p, "aspect", None, RUN)?, flag(p, "vertical", false, RUN)?) {
        (Some(a), true) if a != "9:16" => return Err(bad(RUN, "`vertical` is 9:16 and `aspect` names another: pass one of them")),
        (_, true) => Some("9:16"),
        (a, false) => a,
    };
    let ball = ball_p(p)?;
    // story.highlights checks its own parameters before it starts
    let mut hp = Map::new();
    for k in ["fromSeconds", "toSeconds", "cadence", "minHits", "picture", "fps"] {
        if let Some(v) = p.get(k).filter(|v| !v.is_null()) {
            hp.insert(k.into(), v.clone());
        }
    }
    hp.insert("mode".into(), p.get("mode").filter(|v| !v.is_null()).cloned().unwrap_or(json!("rally")));
    hp.insert("limit".into(), json!(20));
    hp.insert("maxSeconds".into(), json!(round3(target.seconds().min(45.0))));
    let r = match analyse(s, &f, Value::Object(hp), wait, job)? {
        Analysis::Running(j) => return Ok(running(RECIPE.id, p, j)),
        Analysis::Ready(r) => r,
    };
    let chosen = choose(&segments(&r), target, max, by_time);
    if chosen.is_empty() {
        return Err(bad(
            RUN,
            "story.highlights found no moment that stands out (no run of impacts at the cadence, no cheering, no burst of motion): try \"mode\":\"peaks\", a wider `cadence` or a lower `minHits`. Nothing was changed",
        ));
    }
    let mut d = Pending::new(s, &RECIPE, LABEL, finish);
    d.cache = job.map(|j| (j, super::follow::essence(p)));
    let o = Opts { title, name, slow, frames, markers, music, aspect, ball };
    s.edit_group(LABEL, |s| build(s, &mut d, &f, &r, &chosen, o))?;
    super::follow::advance(s, d, wait)
}

struct Opts {
    title: Option<String>,
    name: String,
    slow: bool,
    frames: u64,
    markers: bool,
    music: Pick,
    aspect: Option<&'static str>,
    ball: Option<Ball>,
}

/// The reel: everything but what needs a job (the ball trail, the reframed copy) and what goes on
/// last (the title, in each sequence, once the copy is made).
fn build(s: &mut Session, d: &mut Pending, f: &Footage, r: &Value, chosen: &[Seg], o: Opts) -> Result<()> {
    let st = &mut d.steps;
    st.notes = f.notes.clone();
    st.record("story.highlights", &f.target, "find and score the moments", "done");
    let made =
        st.must(s, "the reel, with the recording's frame size and rate", "file.newSequence", json!({"name": o.name, "fromItem": f.item.0, "place": false}))?;
    let reel = made["sequence"].as_u64().map(ItemId).ok_or_else(|| bad(RUN, "file.newSequence answered no sequence"))?;
    let cuts: Vec<(Tick, Tick)> = chosen.iter().map(|g| (g.a, g.b)).collect();
    let placed = place_cuts(s, st, f, &cuts, "moment")?;
    let rate = s.sequence_rate();
    // seconds of the slow motion left without the moment's sound (it ends at its Out)
    let mut silent: Option<f64> = None;
    // slow motion on the best moment's last stretch: the reel grows, later moments move along
    let best = chosen.iter().zip(&placed).max_by(|x, y| x.0.score.total_cmp(&y.0.score).then(y.0.a.cmp(&x.0.a))).map(|(_, pl)| pl.clone());
    if o.slow
        && let Some(pl) = &best
        && let Some(v) = pl.video
        && let Some(it) = clip_of(s, v)
    {
        let tail = Tick::from_seconds_f64(1.5).min(Tick(it.duration.0 / 3));
        let at = rate.snap(it.end() - tail);
        let ramped = st.may(
            s,
            "slow motion at the end of the best moment",
            "clip.speedRamp",
            json!({"clip": v.0, "from": 100, "to": 50, "at": at.0, "duration": Tick::from_seconds_f64(0.4).min(tail).0, "ripple": true, "audio": "keep"}),
        );
        st.next("clip.speedRamp", json!({"clip": v.0, "clear": true}), "remove the slow motion, or call again with another `to` (percent)");
        // the sound plays on at its own speed: end it with the moment, faded, not past its Out
        if ramped.is_some()
            && let Some(a) = pl.audio.first()
            && let Some(ai) = clip_of(s, *a)
        {
            let keep = rate.snap(pl.media_out - pl.media_in);
            if ai.duration > keep + rate.frame_duration() {
                silent = Some((ai.duration - keep).seconds());
                st.may(
                    s,
                    "end the moment's sound at its Out (it plays at its own speed under the slow motion)",
                    "timeline.trim",
                    json!({"clip": a.0, "edge": "out", "mode": "regular", "delta": (keep - ai.duration).0, "linked": false}),
                );
                st.may(s, "fade it out", "sequence.applyAudioTransition", json!({"clip": a.0, "edge": "out", "frames": 8, "align": "end"}));
            }
        }
    }
    if o.frames > 0 {
        for pl in placed.iter().skip(1) {
            if let Some(v) = pl.video {
                st.may(s, "soften the cut", "sequence.applyVideoTransition", json!({"clip": v.0, "edge": "in", "frames": o.frames, "align": "center"}));
            }
            if let Some(a) = pl.audio.first() {
                st.may(s, "soften the cut's sound", "sequence.applyAudioTransition", json!({"clip": a.0, "edge": "in", "frames": o.frames, "align": "center"}));
            }
        }
    }
    // the cuts as placed (the slow motion moved the later ones)
    let mut moments = Vec::new();
    for (g, pl) in chosen.iter().zip(&placed) {
        let Some(it) = pl.video.or(pl.audio.first().copied()).and_then(|c| clip_of(s, c)) else { continue };
        moments.push(json!({
            "rank": g.rank,
            "score": g.score,
            "kind": g.kind,
            "cues": g.cues,
            "clip": it.id.0,
            "reel": {"start": time_json(it.start), "end": time_json(it.end())},
            "media": {"from": round3(pl.media_in.seconds()), "to": round3(pl.media_out.seconds())},
        }));
        if o.markers {
            let cues: Vec<&str> = g.cues.as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            st.may(
                s,
                "a marker on the moment",
                "markers.add",
                json!({"target": "program", "time": it.start.0, "name": format!("Highlight {}", g.rank), "comment": format!("score {:.2}: {}", g.score, cues.join(", ")), "durationFrames": rate.frame_at(it.duration).max(1)}),
            );
        }
    }
    let (a, b) = extent(s);
    d.looks = Some((crate::story::looks_across(s, a, b), a, b));
    let scored = music_step(s, &mut d.steps, &o.music);
    let st = &mut d.steps;
    if let Some(secs) = silent.filter(|_| !scored) {
        // the ducked music, once placed, plays on at full level under it
        st.notes.push(format!(
            "the last {secs:.1} s of the slow motion has no sound (the moment's sound ends at its Out): add music (assets.place with \"duck\": true), or lengthen that audio clip (timeline.trim)"
        ));
    }
    if let Some(ball) = &o.ball {
        match ball_seed(s, &placed, ball) {
            Ok(f) => d.then(f(reel)),
            Err(why) => st.skip("sports.ballTrail", json!({"point": ball.point, "size": ball.size}), "a trail behind the ball", &why),
        }
    } else if let Some(v) = placed.first().and_then(|x| x.video) {
        st.next(
            "sports.ballTrail",
            json!({"clip": v.0, "point": [0, 0], "size": 24, "at": 0, "wait": true}),
            "a trail behind the ball: give its centre (sequence pixels) at `at`",
        );
    }
    if let Some(asp) = o.aspect {
        d.then(Follow::Reframe { seq: reel, aspect: asp, name: format!("{} {asp}", o.name), graphics: "follow", clips: None });
    }
    d.extra.insert("reel".into(), json!(reel.0));
    d.extra.insert("footage".into(), f.target.clone());
    d.extra.insert("moments".into(), json!(moments));
    d.extra.insert(
        "analysis".into(),
        json!({"found": r["found"], "impacts": r["impacts"], "span": r["span"], "analysed": r["analysed"], "warnings": r["warnings"]}),
    );
    d.ctx = json!({"reel": reel.0, "title": o.title, "subtitle": f.name});
    let st = &mut d.steps;
    let mut hp = f.target.clone();
    hp["limit"] = json!(20);
    hp["wait"] = json!(true);
    st.next("story.highlights", hp, "every moment found, with scores: swap one in with timeline.place");
    if let Some(first) = moments.first() {
        st.next("timeline.trim", json!({"clip": first["clip"], "edge": "in", "mode": "ripple", "deltaFrames": 12}), "start a moment later (negative: earlier)");
    }
    st.next("story.board", json!({"path": "highlights-board.png"}), "look at the reel, one frame per shot");
    st.next("mix.check", json!({}), "check the loudness of the reel");
    st.next("file.exportMedia", json!({"path": "highlights.mp4", "draft": true, "wait": true}), "a quick review copy");
    Ok(())
}

/// After the jobs: the title in the reel, and in the copy (fitted to its frame).
fn finish(s: &mut Session, d: &mut Pending) -> Result<()> {
    let reel = d.ctx["reel"].as_u64().map(ItemId);
    let title = d.ctx["title"].as_str().map(str::to_string);
    let subtitle = d.ctx["subtitle"].clone();
    if let Some(reel) = reel {
        super::follow::open_seq(s, reel);
    }
    let place_title = |s: &mut Session, st: &mut Steps, why: &str| -> Option<u64> {
        let t = title.as_ref()?;
        let v = st.may(
            s,
            why,
            "graphics.template.apply",
            json!({"template": "builtin:title-centered", "values": {"title": t, "subtitle": subtitle}, "time": 0, "track": "auto"}),
        )?;
        let clip = v["clip"].as_u64()?;
        super::fit(s, st, clip, "center");
        Some(clip)
    };
    if let Some(c) = place_title(s, &mut d.steps, "an opening title") {
        d.steps.next("graphics.template.set", json!({"clip": c, "values": {"title": title, "subtitle": ""}}), "change the title's words");
    }
    if let Some((before, a, b)) = d.looks.take() {
        d.extra.insert("pictureChange".into(), picture_change(s, &before, a, b));
    }
    if let Some(copy) = d.copy {
        super::follow::open_seq(s, copy);
        place_title(s, &mut d.steps, "the title in the copy, fitted to its frame");
        let aspect = s.project.sequence(copy).map(|q| (q.settings.width, q.settings.height));
        d.extra.insert("shaped".into(), json!({"sequence": copy.0, "size": aspect}));
        d.steps.notes.push(format!("the active sequence is the reframed copy; the reel itself is sequence {}", reel.map_or(0, |r| r.0)));
    }
    Ok(())
}

/// Music under the whole reel, ducked under speech, from an installed pack.
/// Music under the reel from the packs; whether it was placed.
pub(super) fn music_step(s: &mut Session, st: &mut Steps, music: &Pick) -> bool {
    let asset = match music {
        Pick::Off => return false,
        Pick::Asset(a) => Some(a.clone()),
        Pick::Auto => find_asset(s, st, json!("music"), &["sport", "energetic", "upbeat"], "music in the installed packs"),
    };
    match asset {
        Some(a) => {
            if let Some(v) =
                st.may(s, "music under the reel, ducked", "assets.place", json!({"asset": a, "time": 0, "untilEnd": true, "audioTrack": "new", "duck": true}))
            {
                st.next("clip.volumeDownMany", json!({"clips": v["clips"]}), "the music a little quieter");
                return true;
            }
            false
        }
        None => {
            st.notes.push(
                "no music: no music bed ships with Filmagic and no installed pack has one. Install a pack you hold a licence for (assets.install, or assets.buildPack from a folder of your own files), then assets.place {\"asset\": \"pack/item\", \"time\": 0, \"untilEnd\": true, \"duck\": true}"
                    .into(),
            );
            st.next("assets.search", json!({"kind": "music"}), "music in the installed packs, once one is installed");
            false
        }
    }
}

/// Where the ball trail is seeded: the moment showing `ball.at`, and the reel time that shows it
/// (through the slow motion's remap).
fn ball_seed(s: &Session, placed: &[Placed], ball: &Ball) -> std::result::Result<impl FnOnce(ItemId) -> Follow, String> {
    let v = placed
        .iter()
        .find(|pl| pl.media_in <= ball.at && ball.at < pl.media_out)
        .and_then(|pl| pl.video)
        .ok_or_else(|| "`ball.at` is in none of the chosen moments: give a time inside one (`moments[].media`)".to_string())?;
    let it = clip_of(s, v).ok_or_else(|| "the moment's clip is gone".to_string())?;
    let at = crate::sequence_tools::timeline_time_of(&it, ball.at).ok_or_else(|| "the reel does not show `ball.at` (it falls in a cut)".to_string())?;
    let at = s.sequence_rate().snap(at);
    let (point, size) = (ball.point, ball.size);
    Ok(move |seq| Follow::Trail { seq, clip: v.0, point, size, at })
}
