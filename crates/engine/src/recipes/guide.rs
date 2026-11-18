//! `guide.find {goal}`: which commands, with which parameters, do a goal.
//!
//! Agents searched the command list for "same colour on 8 clips" and never found
//! `edit.pasteAttributes` or `file.newAdjustmentLayer`. The guide is a table of goals, each with
//! the words an agent might use for it and the exact commands, in order, with example
//! parameters. A test checks that every command it names is in the registry (or is one the
//! tests list as arriving later: a `prefer` step, shown only when this build has it).

use serde_json::{Value, json};

use crate::commands::bad;
use crate::{Result, Session};

/// One goal: how to do it.
pub(crate) struct Topic {
    pub id: &'static str,
    pub goal: &'static str,
    /// Other words for it (matched with the goal).
    pub words: &'static str,
    /// The commands, in order: (command, example params, what it does here).
    pub steps: &'static [(&'static str, &'static str, &'static str)],
    /// A better way where this build has the command: (command, example params, why).
    pub prefer: Option<(&'static str, &'static str, &'static str)>,
    pub tips: &'static str,
}

pub(crate) const TOPICS: &[Topic] = &[
    Topic {
        id: "same-look-many-clips",
        goal: "apply the same colour grade, effects or look to many clips",
        words: "copy paste attributes grade color colour lumetri effect settings several all clips match batch same",
        steps: &[
            ("edit.copy", r#"{} (after timeline.select {"clips":[source clip]})"#, "copy the clip that has the look"),
            (
                "edit.pasteAttributes",
                r#"{"clips":[2,3,4],"effects":true,"motion":false}"#,
                "paste its effects (and optionally motion, opacity, volume) onto the others in one step",
            ),
            ("lumetri.applyPreset", r#"{"name":"…","clips":[2,3,4]}"#, "or: a colour preset on all of them (lumetri.presets lists them)"),
            ("effects.setParam", r#"{"clips":[2,3,4],"effect":"lumetri","param":"…","value":…}"#, "or: one parameter on many clips at once"),
        ],
        prefer: None,
        tips: "one effect over everything below it is an adjustment layer (topic adjustment-layer)",
    },
    Topic {
        id: "adjustment-layer",
        goal: "add an adjustment layer (one effect over several clips)",
        words: "adjustment layer grade over everything below track above global effect",
        steps: &[
            ("file.newAdjustmentLayer", r#"{"seconds":30}"#, "a new adjustment layer item"),
            ("timeline.place", r#"{"item":<layer>,"track":"V2","seconds":0}"#, "on a track above the clips it should change"),
            ("effects.apply", r#"{"clips":[<layer clip>],"effect":"lumetri"}"#, "the effect, on the layer"),
            ("effects.setParam", r#"{"clip":<layer clip>,"effect":"lumetri","param":"…","value":…}"#, "tune it once for everything below"),
        ],
        prefer: None,
        tips: "trim the layer's clip to change where it applies; Opacity on the layer fades the effect",
    },
    Topic {
        id: "duck-music",
        goal: "duck the music under speech",
        words: "duck ducking lower music voice dialogue speech talk background quieter sidechain",
        steps: &[
            ("essentialSound.setType", r#"{"clips":[<voice clips>],"type":"dialogue"}"#, "mark the speech"),
            ("essentialSound.setType", r#"{"clips":[<music clips>],"type":"music"}"#, "mark the music"),
            (
                "essentialSound.generateDucking",
                r#"{"clips":[<music clips>],"by":"speech","depthDb":12}"#,
                "volume keyframes that dip the music under the speech",
            ),
            ("mix.check", r#"{}"#, "check the loudness afterwards"),
        ],
        prefer: None,
        tips: "assets.place with \"duck\":true places music already ducked",
    },
    Topic {
        id: "reframe-vertical",
        goal: "reframe to 9:16 vertical (or square, 4:5) for phone, Reels, Shorts, TikTok",
        words: "vertical portrait 9:16 916 tiktok reels shorts phone square 1:1 4:5 reframe crop aspect",
        steps: &[
            ("recipes.run", r#"{"recipe":"vertical","aspect":"9:16"}"#, "a 9:16 copy of the active sequence (or pass item / clip)"),
            ("sequence.settings", r#"{"width":1080,"height":1920}"#, "or by hand: change a sequence's frame size"),
            ("clip.fillFrame", r#"{"clips":[…]}"#, "then fill the new frame (a centre crop)"),
            ("effects.setParam", r#"{"clip":…,"effect":"motion","param":"position","value":[540,960]}"#, "move the crop onto the subject"),
        ],
        prefer: Some(("clip.autoReframe", r#"{"output":"sequence","aspect":"9:16","wait":true}"#, "a copy that follows the subject, with overlays kept on it")),
        tips: "layout.check afterwards: titles may now leave the title-safe area",
    },
    Topic {
        id: "highlight-reel",
        goal: "make a highlight reel from a long sports or rally recording",
        words: "highlight highlights reel best moments rally sport sports match game pickleball tennis",
        steps: &[
            ("recipes.run", r#"{"recipe":"sports-highlight","item":<recording>,"targetSeconds":30,"wait":true}"#, "a first draft in one step"),
            ("story.highlights", r#"{"item":<recording>,"wait":true}"#, "or: just the ranked moments, with suggested In and Out"),
            ("timeline.place", r#"{"item":<recording>,"sourceIn":…,"duration":…}"#, "place a moment yourself"),
        ],
        prefer: None,
        tips: "recipes.describe {\"recipe\":\"sports-highlight\"} lists every option (slow motion, music, a 9:16 copy, a ball trail)",
    },
    Topic {
        id: "meme-edit",
        goal: "make a funny meme or comedy edit of a clip",
        words: "meme funny comedy joke punchy jump cut reaction tiktok viral short",
        steps: &[
            ("recipes.run", r#"{"recipe":"comedy-meme","item":<clip>,"wait":true}"#, "a first draft in one step"),
            ("assets.search", r#"{"kind":["meme-gif","meme-video","meme-image"]}"#, "meme overlays in the installed packs"),
            ("assets.buildPack", r#"{"from":"<folder>","license":"…","install":true}"#, "a pack from your own memes"),
        ],
        prefer: None,
        tips: "recipes.describe {\"recipe\":\"comedy-meme\"} lists every option",
    },
    Topic {
        id: "slow-motion",
        goal: "slow motion or a speed ramp",
        words: "slow motion slowmo slow-mo speed ramp time remapping faster speed up 50% fast",
        steps: &[
            (
                "clip.speedRamp",
                r#"{"clip":…,"from":100,"to":40,"at":<ticks>,"duration":<ticks>,"ripple":true}"#,
                "a smooth ramp down (and back with `segments`)",
            ),
            ("clip.speedDuration", r#"{"clips":[…],"speed":50,"ripple":true}"#, "a constant speed for the whole clip"),
        ],
        prefer: None,
        tips: "speedRamp mutes the linked sound by default (\"audio\":\"keep\" keeps it)",
    },
    Topic {
        id: "freeze-frame",
        goal: "freeze frame",
        words: "freeze frame hold still pause picture stop",
        steps: &[
            ("clip.insertFrameHoldSegment", r#"{"clip":…,"time":<ticks>,"seconds":2}"#, "insert a held frame there (later clips move)"),
            ("clip.frameHold", r#"{"clips":[…],"time":<ticks>}"#, "or: hold the whole clip on one frame"),
        ],
        prefer: None,
        tips: "",
    },
    Topic {
        id: "title-text",
        goal: "add a title or text on screen",
        words: "title text caption lower third name words heading overlay label font",
        steps: &[
            (
                "graphics.template.apply",
                r#"{"template":"builtin:title-centered","values":{"title":"…"},"time":0,"track":"auto"}"#,
                "a designed title (graphics.template.list lists them)",
            ),
            ("graphics.newText", r#"{"text":"…","seconds":3,"track":"auto"}"#, "or: plain text"),
            ("graphics.set", r##"{"clip":…,"props":{"size":120,"fill_color":"#ffcc00","stroke":true}}"##, "style it"),
            ("graphics.anchor", r#"{"clip":…,"anchor":"bottom"}"#, "put it in a corner of the safe area"),
            ("layout.check", r#"{}"#, "check it is readable and inside the safe area"),
        ],
        prefer: None,
        tips: "",
    },
    Topic {
        id: "captions",
        goal: "add subtitles or captions from the speech",
        words: "subtitles captions srt transcript transcribe speech to text words",
        steps: &[
            ("sequence.transcribe", r#"{"language":"auto"}"#, "transcribe the sequence's speech"),
            ("transcript.createCaptions", r#"{"maxChars":32,"lines":2}"#, "captions from the transcript"),
            ("captions.setStyle", r#"{"track":"C1","size":60,"outline":4}"#, "their look"),
            ("captions.export", r#"{"path":"captions.srt"}"#, "or burn them in with file.exportMedia {\"burnCaptions\":true}"),
        ],
        prefer: None,
        tips: "",
    },
    Topic {
        id: "remove-silence",
        goal: "remove silences, pauses or dead air (jump cuts)",
        words: "silence silences pause pauses dead air jump cut cuts gaps umm filler fillers tighten",
        steps: &[
            ("audio.speechSegments", r#"{"clip":…}"#, "where the speech and the pauses are"),
            ("timeline.razor", r#"{"time":<ticks>,"clip":…}"#, "cut at each end of a pause"),
            ("edit.rippleDelete", r#"{"clips":[<pause pieces>]}"#, "take the pauses out and close the gaps"),
            ("transcript.removeFillers", r#"{}"#, "or, with a transcript: remove um and uh"),
        ],
        prefer: None,
        tips: "the comedy-meme recipe cuts pauses for you",
    },
    Topic {
        id: "loudness",
        goal: "make the sound loud enough, even, and safe for social media",
        words: "loudness loud quiet normalize normalise lufs levels volume even match limiter clipping",
        steps: &[
            ("mix.check", r#"{}"#, "measure the mix (LUFS, true peak)"),
            ("essentialSound.autoMatch", r#"{"clips":[…]}"#, "bring clips to their type's loudness"),
            ("file.exportMedia", r#"{"path":"out.mp4","loudnessLufs":-14}"#, "or normalise on export (the true-peak limiter is on)"),
        ],
        prefer: None,
        tips: "",
    },
    Topic {
        id: "transitions",
        goal: "add transitions between clips (dissolve, fade)",
        words: "transition transitions dissolve fade crossfade cross wipe soften cut cuts",
        steps: &[
            ("sequence.applyVideoTransition", r#"{"clip":…,"edge":"in","frames":12}"#, "a cross dissolve at a clip's start"),
            ("sequence.applyAudioTransition", r#"{"clip":…,"edge":"in","frames":12}"#, "a crossfade of the sound"),
            ("trim.applyDefaultTransition", r#"{"clips":[…]}"#, "the default transition on many clips"),
            ("effects.list", r#"{"kind":"VideoTransition"}"#, "the transitions there are"),
        ],
        prefer: None,
        tips: "",
    },
    Topic {
        id: "music-sfx",
        goal: "add music or sound effects",
        words: "music song soundtrack bed background sound effect effects sfx whoosh boom ambience",
        steps: &[
            ("assets.search", r#"{"kind":"music"}"#, "music and sounds in the installed packs (kind sfx, ambience…)"),
            ("assets.place", r#"{"asset":"pack/item","time":0,"untilEnd":true,"duck":true}"#, "place one (ducked under speech)"),
            ("assets.catalog", r#"{}"#, "packs that can be installed"),
            ("assets.buildPack", r#"{"from":"<folder>","license":"…","install":true}"#, "a pack from your own files"),
        ],
        prefer: None,
        tips: "no music ships with Filmagic: install a pack you hold a licence for",
    },
    Topic {
        id: "export",
        goal: "export or render the video",
        words: "export render save mp4 file output deliver upload draft review",
        steps: &[
            ("file.exportMedia", r#"{"path":"review.mp4","draft":true,"wait":true}"#, "a quick review copy"),
            ("file.exportMedia", r#"{"path":"final.mp4","smartRender":true,"wait":true}"#, "the final; re-exports re-encode only what changed"),
            ("jobs.list", r#"{}"#, "progress of exports that did not wait"),
        ],
        prefer: None,
        tips: "",
    },
    Topic {
        id: "look-at-it",
        goal: "see what the edit looks like",
        words: "see look view preview watch check screenshot frames board thumbnail inspect",
        steps: &[
            ("story.board", r#"{"path":"board.png"}"#, "one frame per shot, in one picture"),
            ("preview.motion", r#"{"path":"clip.gif","fromSeconds":0,"toSeconds":3}"#, "a short moving preview"),
            ("sequence.inspect", r#"{"compact":true}"#, "the clips and what was changed on them"),
            ("layout.check", r#"{}"#, "what overlaps or leaves the safe area"),
        ],
        prefer: None,
        tips: "",
    },
    Topic {
        id: "scene-cuts",
        goal: "split footage at its scene changes",
        words: "scene detection cuts shots split detect edit points shot changes",
        steps: &[
            ("clip.sceneEditDetection", r#"{"clips":[…],"wait":true}"#, "cut a clip at its shot changes"),
            ("media.sourceCuts", r#"{"item":…,"wait":true}"#, "or: just list where the cuts are"),
        ],
        prefer: None,
        tips: "",
    },
    Topic {
        id: "zoom",
        goal: "zoom in, punch in, or a slow push (Ken Burns)",
        words: "zoom punch in push ken burns scale bigger closer move pan keyframe animate",
        steps: &[
            ("effects.setParam", r#"{"clip":…,"effect":"motion","param":"scale","value":115}"#, "a punch-in for the whole clip"),
            (
                "effects.setParam",
                r#"{"clip":…,"effect":"motion","param":"scale","value":100,"time":<start>,"keyframe":true}"#,
                "an animated push: a keyframe at the start…",
            ),
            ("effects.setParam", r#"{"clip":…,"effect":"motion","param":"scale","value":120,"time":<end>,"keyframe":true}"#, "…and one at the end"),
        ],
        prefer: None,
        tips: "",
    },
    Topic {
        id: "shake",
        goal: "camera shake or an impact effect",
        words: "shake camera impact hit earthquake wobble",
        steps: &[("effects.apply", r#"{"clips":[…],"effect":"camera_shake"}"#, "Camera Shake on the clip")],
        prefer: None,
        tips: "effects.list {\"effect\":\"camera_shake\",\"detail\":true} lists its parameters",
    },
    Topic {
        id: "green-screen",
        goal: "remove a green screen (chroma key)",
        words: "green screen chroma key keying blue screen background remove transparent",
        steps: &[
            ("effects.apply", r#"{"clips":[…],"effect":"ultra_key"}"#, "Ultra Key on the clip"),
            ("effects.setParam", r##"{"clip":…,"effect":"ultra_key","param":"key_color","value":"#00ff00"}"##, "the colour to remove"),
        ],
        prefer: None,
        tips: "assets.place {\"key\":true} keys a green-screen meme as it places it",
    },
    Topic {
        id: "markers",
        goal: "mark moments with markers",
        words: "marker markers mark note bookmark chapter",
        steps: &[
            ("markers.add", r#"{"target":"program","time":<ticks>,"name":"…"}"#, "a marker on the sequence"),
            ("markers.addRange", r#"{"time":<ticks>,"durationFrames":48,"name":"…"}"#, "a marker over a stretch"),
        ],
        prefer: None,
        tips: "story.highlights {\"markers\":true} marks the moments it finds",
    },
    Topic {
        id: "voice-over",
        goal: "add a voice-over or narration (text to speech, or recorded)",
        words: "voice voiceover voice-over narration narrator tts text to speech speak say read vietnamese tieng viet vieneu",
        steps: &[
            ("tts.voices", r#"{}"#, "the voices"),
            ("tts.create", r#"{"text":"…","voice":"…","time":0}"#, "speak a line onto the timeline"),
            ("tts.engines", r#"{}"#, "Vietnamese: whether VieNeu-TTS is set up (voices `vieneu:<name>`)"),
            ("tts.setupVieneu", r#"{"install":true,"wait":true}"#, "Vietnamese: install VieNeu-TTS"),
            ("tts.render", r#"{"text":"…","voice":"vieneu:<name>","wait":true}"#, "Vietnamese: synthesize first (a job), then tts.create"),
            ("audio.voiceover.start", r#"{"track":"A2"}"#, "or record one from a microphone"),
        ],
        prefer: None,
        tips: "",
    },
    Topic {
        id: "noise",
        goal: "clean up noisy dialogue",
        words: "noise noisy hum hiss denoise clean dialogue clarity reverb echo",
        steps: &[
            ("essentialSound.setType", r#"{"clips":[…],"type":"dialogue"}"#, "mark it as dialogue"),
            ("essentialSound.set", r#"{"clips":[…],"values":{"repair.noise.on":true,"repair.noise.amount":5}}"#, "reduce the noise (repair.humHz for hum)"),
        ],
        prefer: None,
        tips: "",
    },
    Topic {
        id: "cut-out-subject",
        goal: "cut out a person or object (picture in picture, layers)",
        words: "cut out cutout subject person object layer extract mask isolate picture in picture pip",
        steps: &[
            (
                "layers.extract",
                r#"{"clip":…,"region":{"shape":"ellipse","x":0.4,"y":0.2,"w":0.2,"h":0.6},"follow":true}"#,
                "a layer of what is in the region, followed through the clip",
            ),
            ("layers.transform", r#"{"layer":…,"scale":60,"offset":[400,0]}"#, "move and scale it"),
        ],
        prefer: None,
        tips: "masks.add and masks.track for a mask you draw yourself",
    },
    Topic {
        id: "undo",
        goal: "undo, or see what was done",
        words: "undo redo revert back history mistake",
        steps: &[("edit.undo", r#"{}"#, "take back the last step (a recipe is one)"), ("history.list", r#"{}"#, "the steps, newest last")],
        prefer: None,
        tips: "",
    },
];

/// Words that say nothing about a goal.
const STOP: &[&str] = &[
    "a", "an", "the", "to", "of", "in", "on", "for", "and", "or", "my", "i", "want", "how", "do", "make", "add", "with", "it", "is", "from", "all", "can",
    "me", "please", "video", "clip", "clips",
];

fn words(t: &str) -> Vec<String> {
    t.to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != ':' && c != '-')
        .map(|w| w.trim_matches(['-', ':']).to_string())
        .filter(|w| !w.is_empty() && !STOP.contains(&w.as_str()))
        .take(40)
        .collect()
}

/// How many of `goal`'s words the topic holds (a word matches a topic word it starts, or that
/// starts it, from three letters).
fn score(t: &Topic, goal: &[String]) -> usize {
    let own = words(&format!("{} {} {}", t.id.replace('-', " "), t.goal, t.words));
    goal.iter().filter(|g| own.iter().any(|w| w == *g || (g.len() >= 3 && w.len() >= 3 && (w.starts_with(g.as_str()) || g.starts_with(w.as_str()))))).count()
}

fn topic_json(t: &Topic) -> Value {
    let mut steps: Vec<Value> = Vec::new();
    if let Some((c, p, why)) = t.prefer.filter(|p| crate::commands::find(p.0).is_some()) {
        steps.push(json!({"command": c, "params": p, "note": format!("best: {why}")}));
    }
    steps.extend(t.steps.iter().map(|(c, p, n)| json!({"command": c, "params": p, "note": n})));
    json!({"topic": t.id, "goal": t.goal, "steps": steps, "tips": t.tips})
}

pub(super) fn find(_: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "guide.find";
    if !p.is_object() && !p.is_null() {
        return Err(bad(cmd, "params must be an object, e.g. {\"goal\": \"duck the music under speech\"}"));
    }
    let limit = super::whole(p, "limit", 1, 30, 3, cmd)? as usize;
    let goal = match p.get("goal") {
        None | Some(Value::Null) => None,
        Some(Value::String(g)) if g.chars().count() <= 500 => Some(g.clone()),
        Some(Value::String(_)) => return Err(bad(cmd, "`goal` must be at most 500 characters")),
        Some(_) => return Err(bad(cmd, "`goal` must be text: what you want done, in plain words")),
    };
    let Some(goal) = goal.filter(|g| !g.trim().is_empty()) else {
        return Ok(json!({
            "topics": TOPICS.iter().map(|t| json!({"topic": t.id, "goal": t.goal})).collect::<Vec<_>>(),
            "recipes": super::RECIPES.iter().map(|r| json!({"recipe": r.id, "summary": r.summary})).collect::<Vec<_>>(),
            "use": "guide.find {\"goal\": \"…\"} for the commands of one goal; recipes.run for a first draft",
        }));
    };
    let g = words(&goal);
    let mut ranked: Vec<(usize, usize)> = TOPICS.iter().enumerate().map(|(i, t)| (score(t, &g), i)).filter(|x| x.0 > 0).collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let topics: Vec<Value> = ranked.iter().take(limit).filter_map(|(_, i)| TOPICS.get(*i)).map(topic_json).collect();
    let recipes: Vec<Value> = super::RECIPES
        .iter()
        .filter(|r| {
            let own = words(&format!("{} {} {}", r.id.replace('-', " "), r.title, r.summary));
            g.iter().any(|w| w.len() >= 4 && own.iter().any(|o| o.starts_with(w.as_str())))
        })
        .map(|r| json!({"recipe": r.id, "summary": r.summary, "run": format!("recipes.run {{\"recipe\": \"{}\", …}}", r.id)}))
        .collect();
    // other commands whose words hold the goal's: what a topic does not cover
    let mut cmds: Vec<(usize, &str, &str)> = crate::commands::command_specs()
        .iter()
        .filter_map(|c| {
            let hay = format!("{} {} {}", c.id, c.label, crate::commands::command_keywords(c.id).unwrap_or_default()).to_lowercase();
            let n = g.iter().filter(|w| w.len() >= 3 && hay.contains(w.as_str())).count();
            (n > 0).then_some((n, c.id, c.label))
        })
        .collect();
    cmds.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
    let commands: Vec<Value> = cmds.iter().take(8).map(|(_, id, label)| json!({"command": id, "label": label})).collect();
    let mut out = json!({"goal": goal, "topics": topics, "recipes": recipes, "commands": commands});
    if topics_empty(&out) {
        out["notes"] = json!(["no guide topic matches these words: try other words, guide.find {} for every topic, or command.list with a filter"]);
    }
    Ok(out)
}

fn topics_empty(v: &Value) -> bool {
    v["topics"].as_array().is_none_or(Vec::is_empty)
}
