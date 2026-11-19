//! Follow-on steps of a draft: the heavy analyses a recipe runs after the draft is built (a ball
//! trail, an auto-reframe), as jobs.
//!
//! The draft itself is built in one call (one undo step). Each follow-on then runs as a job: the
//! reply says `"status": "running"` with the job and a `resultCommand` (`recipes.run {"recipe",
//! "follow"}`) that applies the next step once the job is done, so no call holds the session or
//! the app's UI thread for the analysis. With `wait:true` inside the engine (the CLI) the jobs run
//! in the call instead. Whatever runs, every change the follow-ons make is folded into the
//! draft's undo step: `edit.undo` takes the whole draft back. An edit made by someone else while a
//! job runs stops the folding (it would otherwise be undone with the draft), and the result says so.

use std::collections::VecDeque;
use std::sync::{Arc, Weak};

use filmagic_project::{ItemId, Project};
use filmagic_time::Tick;
use serde_json::{Map, Value, json};

use super::{RUN, Recipe, Steps, aspect_size, done};
use crate::commands::bad;
use crate::{EngineError, Result, Session};

/// Drafts whose follow-ons are still running, and finished ones (for a repeated resultCommand).
const KEEP: usize = 8;

/// A follow-on step.
#[derive(Clone, Debug)]
pub(crate) enum Follow {
    /// `sports.ballTrail` on `clip` of the sequence `seq`, seeded at sequence time `at`.
    Trail { seq: ItemId, clip: u64, point: [f64; 2], size: f64, at: Tick },
    /// `clip.autoReframe {output:"sequence"}` of `seq`: a copy in `aspect`.
    Reframe { seq: ItemId, aspect: &'static str, name: String, graphics: &'static str, clips: Option<Vec<u64>> },
    /// `clip.autoReframe {output:"keyframes"}` on `clips` of `seq`, a sequence already of `aspect`.
    Keys { seq: ItemId, aspect: &'static str, clips: Vec<u64> },
}

impl Follow {
    fn seq(&self) -> ItemId {
        match self {
            Follow::Trail { seq, .. } | Follow::Reframe { seq, .. } | Follow::Keys { seq, .. } => *seq,
        }
    }

    fn command(&self) -> &'static str {
        match self {
            Follow::Trail { .. } => "sports.ballTrail",
            Follow::Reframe { .. } | Follow::Keys { .. } => "clip.autoReframe",
        }
    }

    fn why(&self) -> &'static str {
        match self {
            Follow::Trail { .. } => "a trail behind the ball (a job)",
            Follow::Reframe { .. } => "a copy in the new shape that follows the action (a job)",
            Follow::Keys { .. } => "follow the subject in the new shape (a job)",
        }
    }

    fn params(&self, wait: bool) -> Value {
        match self {
            Follow::Trail { clip, point, size, at, .. } => json!({"clip": clip, "point": point, "size": size, "at": at.0, "wait": wait}),
            Follow::Reframe { aspect, name, graphics, clips, .. } => {
                let mut p = json!({"output": "sequence", "aspect": aspect, "name": name, "graphics": graphics, "wait": wait});
                if let Some(c) = clips {
                    p["clips"] = json!(c);
                }
                p
            }
            Follow::Keys { aspect, clips, .. } => json!({"clips": clips, "output": "keyframes", "aspect": aspect, "wait": wait}),
        }
    }
}

/// What the recipe does once the follow-ons are over (titles in each sequence, …).
pub(crate) type Finish = fn(&mut Session, &mut Pending) -> Result<()>;

/// A draft between its build and its last follow-on.
pub(crate) struct Pending {
    pub recipe: &'static Recipe,
    pub label: &'static str,
    key: u64,
    epoch: u64,
    /// The newest undo entry before the recipe (its snapshot), `None` for an empty history:
    /// every entry above it is the draft's, folded into the first one. `fold` false once someone
    /// else edited while a job ran.
    base: Option<Weak<Project>>,
    fold: bool,
    /// Entries above `base` after the last fold (0 before the draft's first edit, else 1).
    owned: usize,
    pub steps: Steps,
    pub extra: Map<String, Value>,
    queue: VecDeque<Follow>,
    /// The running follow-on: its job and the index of its step in `steps`.
    running: Option<(u64, Follow, usize)>,
    finish: Finish,
    pub ctx: Value,
    /// The copy a reframe made (or its centre-crop fallback).
    pub copy: Option<ItemId>,
    /// The analysis job and the parameters the draft was built from (`recipes.run {job}` answers it).
    pub cache: Option<(u64, Value)>,
    /// The programme across the draft before its styling, to measure `pictureChange` against.
    pub looks: Option<(Vec<Option<crate::story::Frame>>, Tick, Tick)>,
}

/// A finished draft, for a repeated `recipes.run`: built from which parameters, in which document,
/// and whether it is still there (its first undo entry and its sequence).
#[derive(Clone)]
pub(crate) struct Built {
    params: Value,
    epoch: u64,
    entry: Option<Weak<Project>>,
    sequence: Option<u64>,
    pub value: Value,
}

#[derive(Default)]
pub struct Drafts {
    pending: VecDeque<Pending>,
    /// By analysis job.
    built: VecDeque<(u64, Built)>,
    /// By follow key.
    followed: VecDeque<(u64, Built)>,
    next_key: u64,
    /// Narration jobs a comedy-meme started (a `voiceJob` must be one of them).
    pub voice_jobs: VecDeque<u64>,
    /// The undo entry each recently applied sports job made, by job ([`Drafts::applied`]): how a
    /// draft tells its own follow-on's entry from an edit of the same name made meanwhile.
    applied: VecDeque<(u64, Weak<Project>)>,
}

impl Drafts {
    /// `sports::poll` applied `job`'s result: its undo entry is the newest one.
    pub(crate) fn applied(&mut self, job: u64, undo: &[(String, Arc<Project>, Option<String>)]) {
        if let Some(u) = undo.last() {
            self.applied.retain(|a| a.0 != job);
            self.applied.push_back((job, Arc::downgrade(&u.1)));
            while self.applied.len() > KEEP * 4 {
                self.applied.pop_front();
            }
        }
    }
}

/// An undo entry, by its snapshot. A `Weak` keeps the snapshot's allocation (not the project), so
/// no later snapshot can take its address.
fn ptr(s: &Session, i: usize) -> Option<Weak<Project>> {
    s.history.undo.get(i).map(|u| Arc::downgrade(&u.1))
}

fn same(a: &Arc<Project>, b: &Weak<Project>) -> bool {
    std::ptr::eq(Arc::as_ptr(a), b.as_ptr())
}

fn top(s: &Session) -> Option<Weak<Project>> {
    s.history.undo.len().checked_sub(1).and_then(|i| ptr(s, i))
}

/// Index just above `base` in the undo history (0 for no base), if `base` is still there.
fn above(s: &Session, base: Option<&Weak<Project>>) -> Option<usize> {
    match base {
        None => Some(0),
        Some(b) => s.history.undo.iter().rposition(|u| same(&u.1, b)).map(|i| i + 1),
    }
}

/// The parameters that make a draft what it is (not how it was asked for).
pub(crate) fn essence(p: &Value) -> Value {
    let mut q = p.clone();
    if let Some(o) = q.as_object_mut() {
        for k in ["wait", "job", "voiceJob", "follow"] {
            o.remove(k);
        }
    }
    q
}

impl Pending {
    /// A draft about to be built: the undo history as it stands now is its base.
    pub fn new(s: &mut Session, recipe: &'static Recipe, label: &'static str, finish: Finish) -> Pending {
        let key = s.recipes.next_key;
        s.recipes.next_key = key.wrapping_add(1);
        Pending {
            recipe,
            label,
            key,
            epoch: s.document_epoch,
            base: top(s),
            fold: true,
            owned: 0,
            steps: Steps::default(),
            extra: Map::new(),
            queue: VecDeque::new(),
            running: None,
            finish,
            ctx: Value::Null,
            copy: None,
            cache: None,
            looks: None,
        }
    }

    pub fn then(&mut self, f: Follow) {
        self.queue.push_back(f);
    }

    /// Fold every undo entry above the base into the first one, named after the recipe.
    fn fold(&mut self, s: &mut Session) {
        if !self.fold {
            return;
        }
        let Some(i) = above(s, self.base.as_ref()) else {
            // the base left the history (its limit): nothing safe to fold into
            self.fold = false;
            return;
        };
        if s.history.undo.len() > i + 1 {
            s.history.undo.truncate(i + 1);
            s.history.merge_key = None;
        }
        if let Some(u) = s.history.undo.get_mut(i) {
            u.0 = self.label.to_string();
        }
        self.owned = s.history.undo.len().saturating_sub(i).min(1);
    }

    /// At a resultCommand: only the running job's own entry may have arrived since the last fold.
    fn check_foreign(&mut self, s: &Session) {
        if !self.fold {
            return;
        }
        let Some(i) = above(s, self.base.as_ref()) else {
            self.fold = false;
            return;
        };
        // the running job's entry is known by the token sports::poll left for it, not by its label
        // (an edit of the same name made meanwhile is someone else's)
        let new: Vec<&Arc<Project>> = s.history.undo.iter().skip(i + self.owned).map(|u| &u.1).collect();
        let job = self.running.as_ref().map(|r| r.0);
        let made = job.and_then(|j| s.recipes.applied.iter().find(|a| a.0 == j)).map(|a| &a.1);
        let ours = match (new.as_slice(), made) {
            ([], _) => true,
            ([one], Some(w)) => same(one, w),
            _ => false,
        };
        if !ours {
            self.fold = false;
            self.steps.warnings.push(
                "the project was edited while the recipe's jobs ran: the steps that follow are their own undo steps, so edit.undo does not take back your edit with the draft"
                    .into(),
            );
        }
    }
}

/// The reply while a follow-on job runs.
fn running_reply(d: &Pending, job: u64, cmd: &str) -> Value {
    json!({
        "recipe": d.recipe.id,
        "status": "running",
        "job": job,
        "step": cmd,
        "draft": d.extra.get("reel").or_else(|| d.extra.get("edit")).cloned().unwrap_or(Value::Null),
        "poll": "jobs.list",
        "cancel": "jobs.cancel",
        "result": format!("the draft is built; `{cmd}` runs as job {job}. Run the resultCommand once the job has finished (over MCP, wait:true does this for you)"),
        "resultCommand": {"command": RUN, "params": {"recipe": d.recipe.id, "follow": d.key}},
        // a failed or stopped follow-on falls back (a centre crop) or warns: see `failed`
        "resultOnFailure": true,
    })
}

fn summary(v: &Value) -> Value {
    let mut out = Map::new();
    for k in ["clip", "clips", "sequence", "coverage", "lost", "keyframes", "pictureChange"] {
        if let Some(x) = v.get(k) {
            out.insert(k.into(), x.clone());
        }
    }
    Value::Object(out)
}

pub(crate) fn open_seq(s: &mut Session, seq: ItemId) {
    if s.state.active_sequence != Some(seq) && s.project.sequence(seq).is_some() {
        s.execute("sequence.open", json!({"item": seq.0})).ok();
    }
}

/// A follow-on whose job finished: what it made.
fn applied(s: &mut Session, d: &mut Pending, f: &Follow, idx: usize, r: &Value) {
    if let Some(x) = d.steps.list.get_mut(idx) {
        x["status"] = json!("done");
        x["result"] = summary(r);
    }
    if let Some(w) = r.get("warnings").and_then(Value::as_array) {
        for x in w.iter().filter_map(Value::as_str).take(4) {
            d.steps.warnings.push(format!("{}: {x}", f.command()));
        }
    }
    if let Follow::Reframe { .. } = f
        && let Some(id) = r["sequence"]["id"].as_u64()
    {
        d.copy = Some(ItemId(id));
        open_seq(s, ItemId(id));
    }
}

/// A follow-on that could not run or was stopped: a warning, and the fallback where there is one.
fn failed(s: &mut Session, d: &mut Pending, f: &Follow, idx: usize, why: &str, cancelled: bool) {
    if let Some(x) = d.steps.list.get_mut(idx) {
        x["status"] = json!(if cancelled { "cancelled" } else { "failed" });
        x["error"] = json!(why);
    }
    if cancelled {
        d.queue.clear();
        d.steps.notes.push(format!("`{}` was stopped: the draft is kept as it was before it, and no later job ran", f.command()));
        return;
    }
    d.steps.warnings.push(format!("`{}` ({}) did not apply: {why}", f.command(), f.why()));
    match f {
        Follow::Trail { .. } => {
            d.steps.next(
                "sports.ballTrail",
                f.params(true),
                "seed the trail again: `point` is the ball's centre at `at` (sequence pixels); a larger `size`, or a `box`, if it is hard to see",
            );
        }
        Follow::Reframe { seq, aspect, name, .. } => {
            d.copy = centre_crop(s, &mut d.steps, *seq, aspect, name);
        }
        Follow::Keys { clips, .. } => {
            d.steps.may(s, "fill the frame from the centre", "clip.fillFrame", json!({"clips": clips}));
        }
    }
}

/// The reel nested in a sequence of `aspect`, filling its frame: a copy in that shape, cropped
/// from the centre. `None` when it could not be made.
pub(crate) fn centre_crop(s: &mut Session, st: &mut Steps, reel: ItemId, aspect: &str, name: &str) -> Option<ItemId> {
    let (w, h) = aspect_size(aspect);
    let rate = s.project.sequence(reel).map(|q| q.settings.frame_rate).unwrap_or_default();
    open_seq(s, reel);
    let made =
        st.may(s, "a sequence in the new shape, holding the reel", "file.newSequence", json!({"name": name, "fromItems": [reel.0], "fps": rate.as_f64()}))?;
    let seq = made["sequence"].as_u64().map(ItemId)?;
    st.may(s, "the new shape's frame size", "sequence.settings", json!({"width": w, "height": h}))?;
    let nested: Vec<u64> = s
        .active_sequence()
        .map(|q| q.video_tracks.iter().flat_map(|t| t.items.iter()).filter(|i| i.item == reel).map(|i| i.id.0).collect())
        .unwrap_or_default();
    st.may(s, "fill the frame: a centre crop of the reel", "clip.fillFrame", json!({"clips": nested}));
    if let Some(c) = nested.first() {
        st.next(
            "effects.setParam",
            json!({"clip": c, "effect": "motion", "param": "position", "value": [w / 2, h / 2]}),
            "move the crop sideways onto the action (x)",
        );
    }
    Some(seq)
}

/// Run what is left of a draft: its follow-ons (as jobs, or in this call with `wait`), then its
/// finish. Answers `running` while a job runs (the draft is kept for the resultCommand), else the
/// finished draft.
pub(crate) fn advance(s: &mut Session, mut d: Pending, wait: bool) -> Result<Value> {
    d.fold(s);
    loop {
        if let Some((job, f, idx)) = d.running.take() {
            let r = s.execute("sports.report", json!({"job": job}))?;
            match r["status"].as_str() {
                Some("running") => {
                    let out = running_reply(&d, job, f.command());
                    d.running = Some((job, f, idx));
                    keep(s, d);
                    return Ok(out);
                }
                Some("done") => applied(s, &mut d, &f, idx, &r),
                Some(st) => failed(s, &mut d, &f, idx, r["error"].as_str().unwrap_or(st), st == "cancelled"),
                None => failed(s, &mut d, &f, idx, "the job gave no report", false),
            }
            d.fold(s);
            continue;
        }
        if let Some(f) = d.queue.pop_front() {
            // the job works on the active sequence
            open_seq(s, f.seq());
            let params = f.params(wait);
            let idx = d.steps.list.len();
            d.steps.list.push(json!({"step": idx + 1, "command": f.command(), "params": params, "why": f.why(), "status": "running"}));
            match s.execute(f.command(), params) {
                Ok(v) if wait => applied(s, &mut d, &f, idx, &v),
                Ok(v) => match v["job"].as_u64() {
                    Some(job) => d.running = Some((job, f, idx)),
                    None => applied(s, &mut d, &f, idx, &v),
                },
                Err(e) => failed(s, &mut d, &f, idx, &e.to_string(), false),
            }
            d.fold(s);
            continue;
        }
        (d.finish)(s, &mut d)?;
        d.fold(s);
        let entry = above(s, d.base.as_ref()).filter(|_| d.fold).and_then(|i| ptr(s, i));
        let Pending { recipe, label, key, epoch, steps, extra, cache, .. } = d;
        let value = done(recipe, s, steps, extra, label);
        let built = Built { params: Value::Null, epoch, entry, sequence: value["sequence"].as_u64(), value: value.clone() };
        let r = &mut s.recipes;
        r.followed.retain(|b| b.0 != key);
        r.followed.push_back((key, built.clone()));
        if let Some((job, params)) = cache {
            r.built.retain(|b| b.0 != job);
            r.built.push_back((job, Built { params, ..built }));
        }
        while r.followed.len() > KEEP {
            r.followed.pop_front();
        }
        while r.built.len() > KEEP * 2 {
            r.built.pop_front();
        }
        return Ok(value);
    }
}

fn keep(s: &mut Session, d: Pending) {
    s.recipes.pending.retain(|p| p.key != d.key);
    s.recipes.pending.push_back(d);
    while s.recipes.pending.len() > KEEP {
        s.recipes.pending.pop_front();
    }
}

/// Whether a finished draft is still in the project: the same document, its undo entry still in
/// the history (not undone), its sequence still there.
fn alive(s: &Session, b: &Built) -> bool {
    b.epoch == s.document_epoch
        && b.entry.as_ref().is_none_or(|e| s.history.undo.iter().any(|u| same(&u.1, e)))
        && b.sequence.is_none_or(|q| s.project.sequence(ItemId(q)).is_some())
}

fn again(b: &Built, why: String) -> Value {
    let mut out = b.value.clone();
    out["alreadyBuilt"] = json!(true);
    if let Some(n) = out["notes"].as_array_mut() {
        n.push(json!(why));
    }
    out
}

/// `recipes.run {recipe, follow}`: the next step of a draft whose follow-on job ran.
pub(crate) fn resume(s: &mut Session, recipe: &Recipe, key: u64, wait: bool) -> Result<Value> {
    if let Some(i) = s.recipes.pending.iter().position(|p| p.key == key) {
        let Some(mut d) = s.recipes.pending.remove(i) else {
            return Err(bad(RUN, format!("no draft {key} is waiting")));
        };
        if d.recipe.id != recipe.id {
            let id = d.recipe.id;
            keep(s, d);
            return Err(bad(RUN, format!("draft {key} is a `{id}` draft, not `{}`", recipe.id)));
        }
        if d.epoch != s.document_epoch {
            if let Some((job, _, _)) = &d.running {
                s.execute("jobs.cancel", json!({"job": job})).ok();
            }
            return Err(EngineError::Other(format!("{RUN}: the project was closed or replaced while the draft's jobs ran; nothing more was done")));
        }
        d.check_foreign(s);
        return advance(s, d, wait);
    }
    if let Some(b) = s.recipes.followed.iter().find(|b| b.0 == key).map(|b| b.1.clone()) {
        if alive(s, &b) {
            return Ok(again(&b, format!("draft {key} was finished before; nothing more was done")));
        }
        return Err(bad(RUN, format!("draft {key} was finished and then undone or closed; run the recipe again for a new draft")));
    }
    Err(bad(RUN, format!("no draft {key}: a `follow` is the key a resultCommand names (drafts are kept for the last {KEEP})")))
}

/// For `recipes.run {job}`: the draft built from that analysis, if it is still there and was
/// built from the same parameters; a draft still in its jobs answers where it is.
pub(crate) fn built_from(s: &mut Session, job: u64, p: &Value) -> Result<Option<Value>> {
    let want = essence(p);
    if let Some(d) = s.recipes.pending.iter().find(|d| d.cache.as_ref().is_some_and(|c| c.0 == job)) {
        if d.cache.as_ref().is_some_and(|c| c.1 != want) {
            return Err(bad(
                RUN,
                format!("a draft from job {job} with other parameters is still running its jobs: follow its resultCommand, or run the recipe without `job`"),
            ));
        }
        if let Some((j, f, _)) = &d.running {
            return Ok(Some(running_reply(d, *j, f.command())));
        }
    }
    let Some(i) = s.recipes.built.iter().position(|b| b.0 == job) else { return Ok(None) };
    let Some(b) = s.recipes.built.get(i).map(|b| b.1.clone()) else { return Ok(None) };
    if b.params != want {
        return Err(bad(
            RUN,
            format!(
                "job {job} was already built into a draft with other parameters: run the recipe without `job` for a new draft (the analysis is cached, so it is quick)"
            ),
        ));
    }
    if alive(s, &b) {
        return Ok(Some(again(
            &b,
            format!(
                "this draft was built from job {job} before; it was not built again. Run the recipe without `job` for another draft (the analysis is cached)"
            ),
        )));
    }
    // undone, closed or replaced: build it again
    s.recipes.built.remove(i);
    Ok(None)
}
