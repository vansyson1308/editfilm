//! Recipes: one command to a first draft, which the agent then adjusts (#22).
//!
//! There are some 750 commands, and agents get lost in them. A recipe runs the existing commands
//! for one goal ("a highlight reel from a long rally recording", "a punchy meme edit") through the
//! normal [`Session::execute`] path, and reports every step it took, with the command id and the
//! exact parameters, so the agent can tune or undo any of them. The guide (`guide.find`, in
//! [`guide`]) answers the smaller question: which commands, with which parameters, do a goal.
//!
//! - `recipes.list` lists the recipes; `recipes.describe {recipe}` shows one: its parameters, the
//!   commands it runs, the optional ones this build lacks, and what to tune afterwards.
//! - `recipes.run {recipe, …}` builds the draft as **one undo step** ([`Session::edit_group`]): a
//!   required step that fails rolls the whole draft back; an optional one that fails is a warning.
//!   A step whose command this build does not have yet (they arrive with later work) is skipped
//!   with a note, so the same recipe gains steps when they land.
//! - Analysis (`story.highlights`) runs as a job. `wait:true` answers when the draft is built;
//!   without it the reply names the job and a `resultCommand` (`recipes.run` with `job`) that
//!   builds the draft once the job has finished. Over MCP a waiting call is run that way (the
//!   automation crate's `LONG_COMMANDS`), so it never holds up the app.
//!
//! Recipes are deterministic: the same footage and parameters give the same draft. They ship no
//! media: music, sound effects and meme overlays come from asset packs the user installed
//! (`assets.search`), and a recipe says what to add when none is installed.

mod follow;
mod guide;
mod meme;
mod sports;
mod vertical;

#[cfg(test)]
mod tests;

use filmagic_project::{ClipId, ItemId, TrackItem, TrackKind};
use filmagic_time::Tick;
use serde_json::{Map, Value, json};

use crate::commands::{CommandSpec, NamedKeys, always, bad};
use crate::{EngineError, Result, Session};

const RUN: &str = "recipes.run";
/// Most steps one recipe records (every recipe is bounded far below this; a guard, not a limit).
const MAX_STEPS: usize = 400;

/// What a recipe is, for `recipes.list` / `recipes.describe` and the registry test.
pub(crate) struct Recipe {
    pub id: &'static str,
    pub title: &'static str,
    pub summary: &'static str,
    /// Parameter doc, in the style of a command's.
    pub params: &'static str,
    /// The commands it runs, and what each is for.
    pub steps: &'static [(&'static str, &'static str)],
    /// Optional commands that are not in every build yet, and what they add when present.
    pub later: &'static [(&'static str, &'static str)],
    /// Every parameter key it accepts (any other is refused, so a typo is not silently ignored).
    pub keys: &'static [&'static str],
    /// Whether it analyses the footage first (`story.highlights`, a job).
    pub analyses: bool,
}

pub(crate) const RECIPES: &[&Recipe] = &[&sports::RECIPE, &meme::RECIPE, &vertical::RECIPE];

fn recipe(id: &str) -> Option<&'static Recipe> {
    let id = id.trim().to_lowercase().replace([' ', '_'], "-");
    RECIPES.iter().copied().find(|r| r.id == id)
}

fn known_ids() -> String {
    RECIPES.iter().map(|r| format!("\"{}\"", r.id)).collect::<Vec<_>>().join(", ")
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    type Run = fn(&mut Session, &Value) -> Result<Value>;
    let spec = |id: &'static str, label: &'static str, params: &'static str, run: Run, journal: bool| CommandSpec {
        id,
        label,
        menu: &[],
        shortcut: None,
        params,
        enabled: always,
        run,
        journal,
        named: NamedKeys::NONE,
    };
    vec![
        spec("recipes.list", "List Recipes", "{} -> [{recipe,title,summary,analyses,missing:[command]}]", list, false),
        spec("recipes.describe", "Describe Recipe", r#"{"recipe":"sports-highlight|comedy-meme|vertical"}"#, describe, false),
        spec(
            RUN,
            "Run Recipe",
            r#"{"recipe":"sports-highlight|comedy-meme|vertical", …the recipe's own parameters (recipes.describe),"wait":bool=false,"job":id? (build from a finished analysis job: the resultCommand of a call that did not wait),"follow":key? (continue a draft after its follow-on job: the resultCommand of a running draft; takes only recipe and wait)} -> {status:"done",sequence,steps:[{step,command,params,status,why}],warnings,notes,pictureChange,nextSteps,undo} (one undo step) or {status:"running",job,resultCommand}"#,
            run,
            true,
        ),
        spec(
            "guide.find",
            "Find Commands for a Goal",
            r#"{"goal":str? (what you want done, in plain words; without it: every topic),"limit":n=3} -> {topics:[{topic,goal,steps:[{command,params,note}],tips}],recipes,commands (other commands whose words match)}"#,
            guide::find,
            false,
        ),
    ]
}

/// The session's drafts: those whose follow-on jobs run, and finished ones for a repeated call.
pub type State = follow::Drafts;

fn missing(r: &Recipe) -> Vec<&'static str> {
    r.later.iter().map(|l| l.0).chain(r.steps.iter().map(|x| x.0)).filter(|id| crate::commands::find(id).is_none()).collect()
}

fn list(_: &mut Session, p: &Value) -> Result<Value> {
    if !p.is_null() && !p.as_object().is_some_and(Map::is_empty) {
        return Err(bad("recipes.list", "takes no parameters; use recipes.describe {\"recipe\": id} for one recipe"));
    }
    Ok(json!({
        "recipes": RECIPES.iter().map(|r| json!({
            "recipe": r.id,
            "title": r.title,
            "summary": r.summary,
            "analyses": r.analyses,
            "missing": missing(r),
        })).collect::<Vec<_>>(),
        "run": "recipes.run {\"recipe\": id, …} builds a first draft in one undo step; recipes.describe {\"recipe\": id} lists its parameters",
        "guide": "guide.find {\"goal\": \"…\"} names the commands for a smaller goal",
    }))
}

fn recipe_p(p: &Value, cmd: &str) -> Result<&'static Recipe> {
    let id = match p.get("recipe") {
        Some(Value::String(s)) => s.as_str(),
        None | Some(Value::Null) => return Err(bad(cmd, format!("need `recipe`: one of {}", known_ids()))),
        Some(_) => return Err(bad(cmd, "`recipe` must be a recipe id (a string)")),
    };
    recipe(id).ok_or_else(|| bad(cmd, format!("no recipe `{}`; the recipes are {} (recipes.list)", clip_text(id, 40), known_ids())))
}

fn describe(_: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "recipes.describe";
    if !p.is_object() {
        return Err(bad(cmd, format!("params must be an object with `recipe`: one of {}", known_ids())));
    }
    let r = recipe_p(p, cmd)?;
    let have = |id: &str| crate::commands::find(id).is_some();
    Ok(json!({
        "recipe": r.id,
        "title": r.title,
        "summary": r.summary,
        "params": r.params,
        "analyses": r.analyses,
        "steps": r.steps.iter().map(|(c, why)| json!({"command": c, "why": why, "available": have(c)})).collect::<Vec<_>>(),
        "optional": r.later.iter().map(|(c, why)| json!({"command": c, "why": why, "available": have(c)})).collect::<Vec<_>>(),
        "undo": "the whole draft is one undo step (edit.undo)",
        "run": format!("recipes.run {{\"recipe\": \"{}\", …}}", r.id),
    }))
}

/// A string cut to at most `n` characters (for an error that quotes what the caller sent).
fn clip_text(s: &str, n: usize) -> String {
    let mut out: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        out.push('…');
    }
    out
}

fn run(s: &mut Session, p: &Value) -> Result<Value> {
    let Some(obj) = p.as_object() else {
        return Err(bad(RUN, format!("params must be an object with `recipe`: one of {}", known_ids())));
    };
    let r = recipe_p(p, RUN)?;
    if let Some(f) = p.get("follow").filter(|v| !v.is_null()) {
        let key = f.as_u64().ok_or_else(|| bad(RUN, "`follow` must be the key a resultCommand names"))?;
        if let Some(k) = obj.keys().find(|k| !["recipe", "follow", "wait"].contains(&k.as_str())) {
            return Err(bad(
                RUN,
                format!("`{}` cannot go with `follow`: a follow-on takes only `recipe` and `follow` (the draft keeps its own parameters)", clip_text(k, 40)),
            ));
        }
        return follow::resume(s, r, key, flag(p, "wait", false, RUN)?);
    }
    for k in obj.keys() {
        if !["recipe", "wait", "job"].contains(&k.as_str()) && !r.keys.contains(&k.as_str()) {
            return Err(bad(RUN, format!("`{}` is not a parameter of recipe `{}`; it takes {} (recipes.describe)", clip_text(k, 40), r.id, r.keys.join(", "))));
        }
    }
    let wait = flag(p, "wait", false, RUN)?;
    let job = match p.get("job") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_u64().ok_or_else(|| bad(RUN, "`job` must be the job id a recipes.run without `wait` answered"))?),
    };
    if job.is_some() && !r.analyses {
        return Err(bad(RUN, format!("recipe `{}` analyses nothing, so it has no `job`: run it without one", r.id)));
    }
    if let Some(j) = job
        && let Some(out) = follow::built_from(s, j, p)?
    {
        return Ok(out);
    }
    match r.id {
        "sports-highlight" => sports::run(s, p, wait, job),
        "comedy-meme" => meme::run(s, p, wait, job),
        _ => vertical::run(s, p, wait),
    }
}

// ---------- parameters ----------

pub(crate) fn flag(p: &Value, key: &str, default: bool, cmd: &str) -> Result<bool> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(bad(cmd, format!("`{key}` must be true or false"))),
    }
}

/// A number in `lo..=hi`, `default` when absent.
pub(crate) fn number(p: &Value, key: &str, lo: f64, hi: f64, default: f64, cmd: &str) -> Result<f64> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v.as_f64().filter(|x| x.is_finite() && (lo..=hi).contains(x)).ok_or_else(|| bad(cmd, format!("`{key}` must be a number from {lo} to {hi}"))),
    }
}

pub(crate) fn whole(p: &Value, key: &str, lo: u64, hi: u64, default: u64, cmd: &str) -> Result<u64> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v.as_u64().filter(|n| (lo..=hi).contains(n)).ok_or_else(|| bad(cmd, format!("`{key}` must be a whole number from {lo} to {hi}"))),
    }
}

/// Text of at most `max` characters, `None` when absent; `false` or `""` means none too.
pub(crate) fn text(p: &Value, key: &str, max: usize, default: Option<&str>, cmd: &str) -> Result<Option<String>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(default.map(str::to_string)),
        Some(Value::Bool(false)) => Ok(None),
        Some(Value::String(t)) if t.trim().is_empty() => Ok(None),
        Some(Value::String(t)) if t.chars().count() <= max && !t.chars().any(|c| c.is_control() && c != '\n') => Ok(Some(t.clone())),
        Some(Value::String(_)) => Err(bad(cmd, format!("`{key}` must be at most {max} characters, without control characters"))),
        Some(_) => Err(bad(cmd, format!("`{key}` must be text, or false for none"))),
    }
}

/// An aspect for a reframed copy: `None` keeps the source's shape.
pub(crate) fn aspect_p(p: &Value, key: &str, default: Option<&'static str>, cmd: &str) -> Result<Option<&'static str>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::String(a)) if a == "source" => Ok(None),
        Some(Value::String(a)) => match a.as_str() {
            "9:16" => Ok(Some("9:16")),
            "1:1" => Ok(Some("1:1")),
            "4:5" => Ok(Some("4:5")),
            _ => Err(bad(cmd, format!("`{key}` must be \"source\", \"9:16\", \"1:1\" or \"4:5\""))),
        },
        Some(_) => Err(bad(cmd, format!("`{key}` must be \"source\", \"9:16\", \"1:1\" or \"4:5\""))),
    }
}

/// The frame size of a social deliverable of `aspect`.
pub(crate) fn aspect_size(aspect: &str) -> (u32, u32) {
    match aspect {
        "1:1" => (1080, 1080),
        "4:5" => (1080, 1350),
        _ => (1080, 1920),
    }
}

/// An asset choice: `Auto` (search the installed packs), `Off`, or a named `pack/item`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Pick {
    Auto,
    Off,
    Asset(String),
}

pub(crate) fn pick_p(p: &Value, key: &str, default: Pick, cmd: &str) -> Result<Pick> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(true)) => Ok(Pick::Auto),
        Some(Value::Bool(false)) => Ok(Pick::Off),
        Some(Value::String(a)) if a.contains('/') && a.len() <= 200 && !a.chars().any(char::is_control) => Ok(Pick::Asset(a.clone())),
        Some(_) => Err(bad(cmd, format!("`{key}` must be true (search the installed packs), false, or an asset id \"pack/item\" (assets.search)"))),
    }
}

// ---------- footage ----------

/// What a recipe cuts from: a project item's media, or a timeline clip of the active sequence.
pub(crate) struct Footage {
    /// The item placed in the draft.
    pub item: ItemId,
    pub name: String,
    /// The target as `story.highlights` takes it (`{"clip": id}` or `{"item": id}`).
    pub target: Value,
    /// For a clip: the clip that maps the analysis's sequence time to the item's media time.
    pub clip: Option<TrackItem>,
    pub video: bool,
    pub audio: bool,
    /// What the analysis can cover, in its time (sequence time for a clip, media time for an item).
    pub span: (Tick, Tick),
    pub notes: Vec<String>,
}

impl Footage {
    /// Media time of the item at analysis time `t` (sequence time for a clip, media time for an item).
    pub fn media_at(&self, t: Tick) -> Tick {
        match &self.clip {
            Some(c) => c.source_time_at(t.clamp(c.start, c.end())),
            None => t,
        }
    }
}

pub(crate) fn footage(s: &Session, p: &Value, cmd: &str) -> Result<Footage> {
    let id = |k: &str| -> Result<Option<u64>> {
        match p.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v.as_u64().map(Some).ok_or_else(|| bad(cmd, format!("`{k}` must be an id"))),
        }
    };
    let (clip, item) = (id("clip")?, id("item")?);
    let mut notes = Vec::new();
    let (item, picked) = match (clip, item) {
        (Some(c), None) => {
            let q = s.active_sequence().ok_or_else(|| bad(cmd, "`clip` names a clip of the active sequence, and no sequence is open; pass `item` instead"))?;
            let Some((track, it)) = q.find_item(ClipId(c)) else {
                return Err(bad(cmd, format!("no clip {c} in the active sequence; list clips with sequence.clips, or pass a project `item`")));
            };
            let on_video = q.track(track).is_some_and(|t| t.kind == TrackKind::Video);
            // the picture: the clip itself, or the video clip linked to a sound clip
            let pic = if on_video {
                it.clone()
            } else {
                it.link.and_then(|l| q.video_tracks.iter().flat_map(|t| t.items.iter()).find(|x| x.link == Some(l))).cloned().unwrap_or_else(|| it.clone())
            };
            if pic.reverse || pic.frame_hold.is_some() {
                return Err(bad(cmd, format!("clip {} plays its media backwards or is a freeze frame: pass its project `item` instead", pic.id.0)));
            }
            if (pic.speed - 1.0).abs() > 1e-9 {
                notes.push(format!(
                    "clip {} plays at {:.0} %: the draft cuts its media at normal speed (speed it again with clip.speedDuration)",
                    pic.id.0,
                    pic.speed * 100.0
                ));
            }
            (pic.item, Some(pic))
        }
        (None, Some(i)) => (ItemId(i), None),
        (Some(_), Some(_)) => return Err(bad(cmd, "pass a timeline `clip` or a project `item`, not both")),
        (None, None) => return Err(bad(cmd, "need the footage: a project `item` (file.import answers its id) or a timeline `clip`")),
    };
    let pi = s.project.item(item).ok_or_else(|| bad(cmd, format!("no project item {}; list items with project.inspect", item.0)))?;
    if s.project.sequence(item).is_some() {
        return Err(bad(cmd, format!("item {} is a sequence: pass a media item or a clip (the `vertical` recipe takes a sequence)", item.0)));
    }
    let src =
        s.source(item).ok_or_else(|| bad(cmd, format!("`{}` has no media to read (offline, or not a media file); media.findMissing relinks it", pi.name)))?;
    let info = src.info();
    let span = match &picked {
        Some(c) if clip.is_some() => (c.start, c.end()),
        _ => match s.project.resolve_media(item) {
            Some((_, _, Some(range))) => (range.start, range.end()),
            _ => (Tick::ZERO, crate::commands::full_duration(s, pi)),
        },
    };
    Ok(Footage {
        span,
        item,
        name: pi.name.clone(),
        target: match (&picked, clip) {
            (Some(_), Some(c)) => json!({"clip": c}),
            _ => json!({"item": item.0}),
        },
        clip: picked,
        video: info.video.is_some(),
        audio: info.has_audio(),
        notes,
    })
}

// ---------- analysis ----------

pub(crate) enum Analysis {
    Ready(Value),
    /// The job's start reply.
    Running(u64),
}

/// `story.highlights` for a recipe: the finished result of `job`, or a new analysis with `params`.
pub(crate) fn analyse(s: &mut Session, footage: &Footage, mut params: Value, wait: bool, job: Option<u64>) -> Result<Analysis> {
    let r = match job {
        Some(j) => s.execute("story.highlights", json!({"job": j}))?,
        None => {
            if let (Some(o), Some(t)) = (params.as_object_mut(), footage.target.as_object()) {
                o.extend(t.clone());
                o.insert("wait".into(), json!(wait));
            }
            s.execute("story.highlights", params).map_err(|e| EngineError::Other(format!("{RUN}: analysing the footage (story.highlights): {e}")))?
        }
    };
    match r["status"].as_str() {
        Some("running") => Ok(Analysis::Running(r["job"].as_u64().or(job).unwrap_or(0))),
        Some("done") => {
            if job.is_some() && r["of"] != footage.target {
                return Err(bad(
                    RUN,
                    format!("job {} analysed {} and this call names {}: pass the same `clip` or `item`", job.unwrap_or(0), r["of"], footage.target),
                ));
            }
            Ok(Analysis::Ready(r))
        }
        Some(st) => {
            Err(EngineError::Other(format!("{RUN}: the analysis {st}: {}; nothing was changed", r["error"].as_str().unwrap_or("run the recipe again"))))
        }
        None => Err(EngineError::Other(format!("{RUN}: the analysis gave no result; nothing was changed"))),
    }
}

/// The reply to a call that started an analysis job: how to follow it and how to get the draft.
pub(crate) fn running(recipe: &str, p: &Value, job: u64) -> Value {
    let mut params = p.clone();
    if let Some(o) = params.as_object_mut() {
        o.remove("wait");
        o.insert("job".into(), json!(job));
    }
    json!({
        "recipe": recipe,
        "status": "running",
        "job": job,
        "poll": "jobs.list",
        "cancel": "jobs.cancel",
        "result": format!("{RUN} with \"job\": {job} (and the same parameters) builds the draft once the analysis has finished; over MCP, wait:true does this for you"),
        "resultCommand": {"command": RUN, "params": params},
    })
}

/// [`running`] for job `poll`, with `set` put into the resultCommand's parameters (`null` removes).
pub(crate) fn running_with(recipe: &str, p: &Value, poll: u64, set: &[(&str, Value)]) -> Value {
    let mut out = running(recipe, p, poll);
    if let Some(o) = out["resultCommand"]["params"].as_object_mut() {
        o.remove("job");
        for (k, v) in set {
            if v.is_null() {
                o.remove(*k);
            } else {
                o.insert((*k).to_string(), v.clone());
            }
        }
    }
    out["result"] = json!(format!("run the resultCommand once job {poll} has finished; over MCP, wait:true does this for you"));
    out
}

/// Seconds of an analysis time value (`{"seconds", "ticks", "frame"}`) as ticks.
pub(crate) fn ticks_of(v: &Value) -> Option<Tick> {
    v.get("ticks").and_then(Value::as_i64).map(Tick).or_else(|| v.as_f64().filter(|x| x.is_finite() && *x >= 0.0).map(Tick::from_seconds_f64))
}

// ---------- steps ----------

/// The steps of one draft, as the result reports them.
#[derive(Default)]
pub(crate) struct Steps {
    pub list: Vec<Value>,
    pub warnings: Vec<String>,
    pub notes: Vec<String>,
    pub next: Vec<Value>,
}

/// What of a step's result goes in the report: the ids it made and its warnings, not its whole
/// answer (an inspection can be large).
fn summary(v: &Value) -> Value {
    let mut out = Map::new();
    for k in ["clip", "clips", "sequence", "transition", "marker", "item", "items", "keyframes", "laterClipsMoved", "asset", "track", "layer", "duration"] {
        if let Some(x) = v.get(k) {
            out.insert(k.into(), x.clone());
        }
    }
    Value::Object(out)
}

impl Steps {
    fn record(&mut self, id: &str, params: &Value, why: &str, status: &str) -> usize {
        let n = self.list.len() + 1;
        if self.list.len() < MAX_STEPS {
            self.list.push(json!({"step": n, "command": id, "params": params, "why": why, "status": status}));
        }
        n
    }

    fn absorb(&mut self, id: &str, v: &Value) {
        if let Some(w) = v.get("warnings").and_then(Value::as_array) {
            for x in w.iter().filter_map(Value::as_str).take(4) {
                self.warnings.push(format!("{id}: {x}"));
            }
        }
    }

    /// A step the draft needs: its failure stops the recipe, and the whole draft is rolled back.
    pub fn must(&mut self, s: &mut Session, why: &str, id: &str, params: Value) -> Result<Value> {
        #[cfg(test)]
        let r = if tests::inject(id) { Err(EngineError::Other("injected failure".into())) } else { s.execute(id, params.clone()) };
        #[cfg(not(test))]
        let r = s.execute(id, params.clone());
        match r {
            Ok(v) => {
                let n = self.record(id, &params, why, "done");
                if let Some(x) = self.list.get_mut(n - 1) {
                    x["result"] = summary(&v);
                }
                self.absorb(id, &v);
                Ok(v)
            }
            Err(e) => {
                let n = self.list.len() + 1;
                Err(EngineError::Other(format!(
                    "{RUN}: step {n} (`{id}`, {why}) failed: {e}. Nothing was changed: the draft is one undo step and was rolled back"
                )))
            }
        }
    }

    /// A step that improves the draft: a failure is a warning, and a command this build does not
    /// have is skipped with a note.
    pub fn may(&mut self, s: &mut Session, why: &str, id: &str, params: Value) -> Option<Value> {
        if crate::commands::find(id).is_none() {
            self.skip(id, params, why, "this build does not have the command yet");
            return None;
        }
        match s.execute(id, params.clone()) {
            Ok(v) => {
                let n = self.record(id, &params, why, "done");
                if let Some(x) = self.list.get_mut(n - 1) {
                    x["result"] = summary(&v);
                }
                self.absorb(id, &v);
                Some(v)
            }
            Err(e) => {
                let n = self.record(id, &params, why, "failed");
                if let Some(x) = self.list.get_mut(n - 1) {
                    x["error"] = json!(e.to_string());
                }
                self.warnings.push(format!("step {n} (`{id}`, {why}) did not apply: {e}"));
                None
            }
        }
    }

    /// A step that did not run, and why (a note says so).
    pub fn skip(&mut self, id: &str, params: Value, why: &str, reason: &str) {
        let n = self.record(id, &params, why, "skipped");
        if let Some(x) = self.list.get_mut(n - 1) {
            x["reason"] = json!(reason);
        }
        self.notes.push(format!("skipped `{id}` ({why}): {reason}"));
    }

    /// A command the agent might run next to tune the draft.
    pub fn next(&mut self, command: &str, params: Value, why: &str) {
        self.next.push(json!({"command": command, "params": params, "why": why}));
    }
}

/// The video clips of `clips` (a `timeline.place` answer lists the picture and its sound), in the
/// active sequence.
pub(crate) fn video_clips(s: &Session, placed: &Value) -> Vec<ClipId> {
    let Some(q) = s.active_sequence() else { return Vec::new() };
    placed["clips"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_u64)
        .map(ClipId)
        .filter(|c| q.find_item(*c).and_then(|(t, _)| q.track(t)).is_some_and(|t| t.kind == TrackKind::Video))
        .collect()
}

pub(crate) fn audio_clips(s: &Session, placed: &Value) -> Vec<ClipId> {
    let Some(q) = s.active_sequence() else { return Vec::new() };
    placed["clips"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_u64)
        .map(ClipId)
        .filter(|c| q.find_item(*c).and_then(|(t, _)| q.track(t)).is_some_and(|t| t.kind == TrackKind::Audio))
        .collect()
}

/// A clip of the active sequence.
pub(crate) fn clip_of(s: &Session, c: ClipId) -> Option<TrackItem> {
    s.active_sequence().and_then(|q| q.find_item(c)).map(|(_, it)| it.clone())
}

pub(crate) fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

pub(crate) fn time_json(t: Tick) -> Value {
    json!({"seconds": round3(t.seconds()), "ticks": t.0})
}

/// The first installed asset of `kind` matching one of `queries` (then any of that kind), as
/// `assets.search` finds it: deterministic (its ranking is), and only files already on disk.
pub(crate) fn find_asset(s: &mut Session, steps: &mut Steps, kind: Value, queries: &[&str], why: &str) -> Option<String> {
    let mut tried: Vec<Value> = Vec::new();
    for q in queries.iter().map(|q| Some(*q)).chain([None]) {
        let mut params = json!({"kind": kind, "limit": 5});
        if let Some(q) = q {
            params["query"] = json!(q);
        }
        let r = match s.execute("assets.search", params.clone()) {
            Ok(r) => r,
            Err(e) => {
                steps.skip("assets.search", params, why, &format!("the packs cannot be searched: {e}"));
                return None;
            }
        };
        tried.push(params.clone());
        let hit = r["items"].as_array().into_iter().flatten().find(|i| i["downloaded"] != json!(false)).and_then(|i| i["asset"].as_str()).map(str::to_string);
        if let Some(a) = hit {
            steps.record("assets.search", &params, why, "done");
            return Some(a);
        }
        if r["total"] == 0 && r["notes"].as_array().is_some_and(|n| n.iter().any(|x| x.as_str().is_some_and(|t| t.starts_with("no packs installed")))) {
            break;
        }
    }
    steps.record("assets.search", tried.last().unwrap_or(&Value::Null), why, "nothing found");
    None
}

/// Keep graphic `clip` inside the title-safe area of the active sequence: while `layout.check`
/// finds it outside, make its layers smaller (each layer's Scale, its position drawn toward the
/// frame's centre) and anchor it again. Says so when it still does not fit.
pub(crate) fn fit(s: &mut Session, st: &mut Steps, clip: u64, anchor: &str) -> bool {
    let frame = s.active_sequence().map_or((1920.0, 1080.0), |q| (f64::from(q.settings.width), f64::from(q.settings.height)));
    let mut scale: Option<f64> = None;
    for _ in 0..8 {
        let Ok(r) = s.execute("layout.check", json!({"clip": clip})) else { return false };
        let outside = r["items"].as_array().into_iter().flatten().find(|i| i["clip"] == clip).map(|i| i["outside"].clone()).unwrap_or(Value::Null);
        if outside.is_null() {
            return true;
        }
        let Ok(list) = s.execute("graphics.list", json!({"clip": clip})) else { return false };
        let canvas = list["canvas"].as_array().and_then(|c| Some((c.first()?.as_f64()?, c.get(1)?.as_f64()?))).unwrap_or(frame);
        // a graphic drawn for another frame shape (a copy in another aspect) is first scaled to fit
        // the frame whole; the safe-area anchor then no longer applies (it reads the canvas)
        let other_shape = (canvas.0 - frame.0).abs() > 0.5 || (canvas.1 - frame.1).abs() > 0.5;
        if other_shape {
            // shrink the whole graphic about the frame's centre (Motion scale), which keeps a
            // centred title centred
            scale = match scale {
                None => st
                    .may(s, "the graphic, drawn for the other shape, fits the frame whole", "clip.fitToFrame", json!({"clips": [clip]}))
                    .and_then(|r| r["clips"].get(0)?["scale"].as_f64()),
                Some(pct) => {
                    let pct = round3(pct * 0.8);
                    st.may(s, "make it fit the frame", "effects.setParam", json!({"clip": clip, "effect": "motion", "param": "scale", "value": pct}))
                        .map(|_| pct)
                }
            };
            if scale.is_none() {
                break;
            }
            continue;
        }
        let layers: Vec<(u64, f64, [f64; 2])> = list["layers"]
            .as_array()
            .into_iter()
            .flatten()
            .take(32)
            .filter_map(|l| {
                let scale = l["scale"].as_f64().filter(|x| x.is_finite() && *x > 0.0)?;
                Some((l["layer"].as_u64()?, scale, [l["position"].get(0)?.as_f64()?, l["position"].get(1)?.as_f64()?]))
            })
            .collect();
        if layers.is_empty() {
            break;
        }
        let (cx, cy) = (canvas.0 / 2.0, canvas.1 / 2.0);
        for (layer, scale, [x, y]) in layers {
            let pos = [round3(cx + (x - cx) * 0.8), round3(cy + (y - cy) * 0.8)];
            st.may(
                s,
                "make it fit the frame",
                "graphics.set",
                json!({"clip": clip, "layer": layer, "props": {"scale": round3(scale * 0.8), "position": pos}, "pictureChange": false}),
            );
        }
        if !other_shape {
            st.may(s, "keep it in the title-safe area", "graphics.anchor", json!({"clip": clip, "anchor": anchor, "safe": "title"}));
        }
    }
    st.warnings.push(format!("graphic clip {clip} still leaves the safe area after shrinking: make its text shorter (layout.check)"));
    false
}

/// How much the styling changed the picture of `[a, b)`, from the looks taken before it.
pub(crate) fn picture_change(s: &Session, before: &[Option<crate::story::Frame>], a: Tick, b: Tick) -> Value {
    if b <= a {
        return Value::Null;
    }
    crate::story::change_json(before, &crate::story::looks_across(s, a, b))
}

/// Where a draft starts and ends in the active sequence (its picture and sound).
pub(crate) fn extent(s: &Session) -> (Tick, Tick) {
    s.active_sequence()
        .map(|q| {
            let items = || q.video_tracks.iter().chain(q.audio_tracks.iter()).flat_map(|t| t.items.iter());
            (items().map(|i| i.start).min().unwrap_or(Tick::ZERO), items().map(TrackItem::end).max().unwrap_or(Tick::ZERO))
        })
        .unwrap_or((Tick::ZERO, Tick::ZERO))
}

/// The finished result of a recipe.
pub(crate) fn done(recipe: &Recipe, s: &Session, steps: Steps, mut extra: Map<String, Value>, label: &str) -> Value {
    let (_, end) = extent(s);
    let mut out = Map::new();
    out.insert("recipe".into(), json!(recipe.id));
    out.insert("status".into(), json!("done"));
    out.insert("sequence".into(), json!(s.state.active_sequence.map(|q| q.0)));
    out.insert("sequenceName".into(), json!(s.state.active_sequence.and_then(|q| s.project.item(q)).map(|i| i.name.clone())));
    out.insert("duration".into(), time_json(end));
    out.append(&mut extra);
    out.insert("steps".into(), json!(steps.list));
    out.insert("warnings".into(), json!(steps.warnings));
    out.insert("notes".into(), json!(steps.notes));
    out.insert("nextSteps".into(), json!(steps.next));
    out.insert("undo".into(), json!(format!("edit.undo takes back the whole draft (one step: \"{label}\")")));
    Value::Object(out)
}

/// One cut of the footage placed in the draft.
#[derive(Clone, Debug)]
pub(crate) struct Placed {
    pub video: Option<ClipId>,
    pub audio: Vec<ClipId>,
    /// Media time of the item at the cut's first frame.
    pub media_in: Tick,
    pub media_out: Tick,
}

/// Place `cuts` (analysis time, in order) back to back from the start of the active sequence:
/// each a stretch of the footage's media, on V1 with its sound (A1 for sound-only footage).
pub(crate) fn place_cuts(s: &mut Session, steps: &mut Steps, f: &Footage, cuts: &[(Tick, Tick)], why: &str) -> Result<Vec<Placed>> {
    let rate = s.sequence_rate();
    let frame = rate.frame_duration();
    let mut at = Tick::ZERO;
    let mut out = Vec::new();
    for (k, (a, b)) in cuts.iter().enumerate() {
        let (ma, mb) = (f.media_at(*a), f.media_at(*b));
        let dur = rate.snap(mb - ma);
        if dur < frame {
            continue;
        }
        let params = json!({
            "item": f.item.0,
            "track": if f.video { "V1" } else { "A1" },
            "time": at.0,
            "sourceIn": ma.0,
            "duration": dur.0,
            "video": f.video,
            "audio": f.audio,
        });
        let placed = steps.must(s, &format!("{why} {}", k + 1), "timeline.place", params)?;
        out.push(Placed { video: video_clips(s, &placed).first().copied(), audio: audio_clips(s, &placed), media_in: ma, media_out: ma + dur });
        at += dur;
    }
    if out.is_empty() {
        return Err(bad(RUN, "every chosen stretch is shorter than a frame: nothing to place; nothing was changed"));
    }
    Ok(out)
}
