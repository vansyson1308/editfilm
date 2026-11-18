//! `vertical`: repurpose footage or a finished sequence for a phone (9:16), square (1:1) or
//! portrait (4:5) feed.
//!
//! A sequence (the active one, or `sequence`) gets a copy in the new shape. A project item or a
//! timeline clip gets a new sequence of that shape holding it. Where this build has
//! `clip.autoReframe` the picture follows the subject; elsewhere it fills the frame from the
//! centre, and the result says how to move the crop. The reframe runs as a follow-on job folded
//! into the recipe's undo step; titles and text keep their place and are made smaller until
//! `layout.check` finds them inside the new frame.

use filmagic_project::ItemId;
use filmagic_time::Tick;
use serde_json::{Value, json};

use super::follow::{Follow, Pending};
use super::{RUN, Recipe, Steps, aspect_p, aspect_size, footage, text};
use crate::commands::bad;
use crate::{EngineError, Result, Session};

pub(super) const RECIPE: Recipe = Recipe {
    id: "vertical",
    title: "Vertical repurpose",
    summary: "A copy of a sequence, a clip or a project item in 9:16 (or 1:1, 4:5) for phone feeds: following the subject where this build has auto-reframe, else a centre crop",
    params: r#"{"sequence":id?|"item":id?|"clip":id? (what to repurpose; default: the active sequence),"aspect":"9:16|1:1|4:5"="9:16","name":str?}"#,
    steps: &[
        ("sequence.open", "the sequence to repurpose"),
        ("file.newSequence", "a sequence of the new shape"),
        ("sequence.settings", "its frame size"),
        ("timeline.place", "an item or a clip's media in it"),
        ("clip.fillFrame", "without auto-reframe: fill the frame from the centre"),
    ],
    later: &[("clip.autoReframe", "follow the subject instead of a centre crop")],
    keys: &["sequence", "item", "clip", "aspect", "name"],
    analyses: false,
};

const LABEL: &str = "Recipe: Vertical";

pub(super) fn run(s: &mut Session, p: &Value, wait: bool) -> Result<Value> {
    let aspect = aspect_p(p, "aspect", Some("9:16"), RUN)?.ok_or_else(|| bad(RUN, "`aspect` must be \"9:16\", \"1:1\" or \"4:5\" for this recipe"))?;
    let name = text(p, "name", 120, None, RUN)?;
    let given = |k: &str| p.get(k).is_some_and(|v| !v.is_null());
    if [given("sequence"), given("item"), given("clip")].iter().filter(|x| **x).count() > 1 {
        return Err(bad(RUN, "pass one of `sequence`, `item` or `clip`"));
    }
    let item_is_seq = p.get("item").and_then(Value::as_u64).is_some_and(|i| s.project.sequence(ItemId(i)).is_some());
    if given("item") && !item_is_seq || given("clip") {
        return media(s, p, aspect, name, wait);
    }
    let seq = match p.get("sequence").or_else(|| p.get("item")).filter(|v| !v.is_null()) {
        Some(v) => {
            let id = v.as_u64().map(ItemId).ok_or_else(|| bad(RUN, "`sequence` must be a sequence's project item id"))?;
            if s.project.sequence(id).is_none() {
                return Err(bad(RUN, format!("{} is no sequence of this project; project.inspect lists them", id.0)));
            }
            id
        }
        None => s.state.active_sequence.ok_or_else(|| bad(RUN, "no sequence is open: pass `sequence`, `item` or `clip`"))?,
    };
    if s.project.sequence(seq).is_none_or(|q| q.video_tracks.iter().chain(q.audio_tracks.iter()).all(|t| t.items.is_empty())) {
        return Err(bad(RUN, format!("sequence {} is empty: there is nothing to repurpose; place clips in it first, or pass an `item`", seq.0)));
    }
    let seq_name = s.project.item(seq).map(|i| i.name.clone()).unwrap_or_default();
    let name = name.unwrap_or_else(|| format!("{seq_name} {aspect}"));
    let mut d = Pending::new(s, &RECIPE, LABEL, finish);
    super::follow::open_seq(s, seq);
    d.extra.insert("from".into(), json!({"sequence": seq.0}));
    d.extra.insert("aspect".into(), json!(aspect));
    if crate::commands::find("clip.autoReframe").is_some() {
        // graphics keep their place and are fitted to the new frame after (not cropped with a clip)
        d.then(Follow::Reframe { seq, aspect, name, graphics: "keep", clips: None });
    } else {
        d.steps.skip(
            "clip.autoReframe",
            json!({"aspect": aspect}),
            "a copy that follows the subject",
            "this build does not have the command: a centre crop instead",
        );
        let st = &mut d.steps;
        d.copy = s.edit_group(LABEL, |s| Ok(super::follow::centre_crop(s, st, seq, aspect, &name)))?;
    }
    d.steps.next("layout.check", json!({}), "titles and captions inside the new safe areas");
    d.steps.next("story.board", json!({"path": "vertical-board.png"}), "look at the new framing, one frame per shot");
    super::follow::advance(s, d, wait)
}

/// After the reframe: the copy is the active sequence, and its graphics fit its frame.
fn finish(s: &mut Session, d: &mut Pending) -> Result<()> {
    if let Some(copy) = d.copy {
        super::follow::open_seq(s, copy);
        d.extra.insert("shaped".into(), json!(copy.0));
        refit(s, &mut d.steps);
    }
    Ok(())
}

/// Every graphic of the active sequence that leaves its safe area, made smaller and anchored
/// where it was (top, middle or bottom) until it fits.
fn refit(s: &mut Session, st: &mut Steps) {
    let Ok(r) = s.execute("layout.check", json!({})) else { return };
    let h = s.active_sequence().map_or(1080.0, |q| f64::from(q.settings.height));
    let out: Vec<(u64, &'static str)> = r["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|i| !i["outside"].is_null())
        .filter_map(|i| {
            let clip = i["clip"].as_u64()?;
            let b = i["samples"].get(0).map(|x| &x["box"]);
            let y = b.and_then(|b| Some(b.get(1)?.as_f64()? + b.get(3)?.as_f64()? / 2.0)).unwrap_or(h / 2.0);
            Some((
                clip,
                if y < h / 3.0 {
                    "top"
                } else if y > h * 2.0 / 3.0 {
                    "bottom"
                } else {
                    "center"
                },
            ))
        })
        .take(64)
        .collect();
    for (clip, anchor) in out {
        super::fit(s, st, clip, anchor);
    }
}

/// An item or a clip's media in a new sequence of the shape.
fn media(s: &mut Session, p: &Value, aspect: &'static str, name: Option<String>, wait: bool) -> Result<Value> {
    let f = footage(s, p, RUN)?;
    if !f.video {
        return Err(bad(RUN, "there is no picture to reframe: this footage has only sound"));
    }
    let (w, h) = aspect_size(aspect);
    let name = name.unwrap_or_else(|| format!("{} {aspect}", f.name));
    // what is shown: the clip's stretch of media, or the whole item
    let (source_in, duration) = match &f.clip {
        Some(c) => (c.source_in, c.source_time_at(c.end()) - c.source_in),
        None => (Tick::ZERO, f.span.1 - f.span.0),
    };
    let mut d = Pending::new(s, &RECIPE, LABEL, finish);
    let have = crate::commands::find("clip.autoReframe").is_some();
    s.edit_group(LABEL, |s| {
        let st = &mut d.steps;
        st.notes = f.notes.clone();
        let made = st.must(s, "a sequence of the new shape", "file.newSequence", json!({"name": name, "fromItem": f.item.0, "place": false}))?;
        st.must(s, "its frame size", "sequence.settings", json!({"width": w, "height": h}))?;
        let rate = s.sequence_rate();
        let dur = rate.snap(duration).max(rate.frame_duration());
        let placed = st.must(
            s,
            "the footage in it",
            "timeline.place",
            json!({"item": f.item.0, "track": "V1", "time": 0, "sourceIn": source_in.0, "duration": dur.0, "audio": f.audio}),
        )?;
        let clips: Vec<u64> = super::video_clips(s, &placed).into_iter().map(|c| c.0).collect();
        let first = *clips.first().ok_or_else(|| EngineError::Other(format!("{RUN}: timeline.place placed no picture")))?;
        let seq = made["sequence"].as_u64().map(ItemId).ok_or_else(|| bad(RUN, "file.newSequence answered no sequence"))?;
        if have {
            d.then(Follow::Keys { seq, aspect, clips: clips.clone() });
        } else {
            st.skip(
                "clip.autoReframe",
                json!({"clips": clips, "aspect": aspect}),
                "follow the subject",
                "this build does not have the command: a centre crop instead",
            );
            st.must(s, "fill the frame from the centre", "clip.fillFrame", json!({"clips": clips}))?;
            st.next(
                "effects.setParam",
                json!({"clip": first, "effect": "motion", "param": "position", "value": [w / 2, h / 2]}),
                "move the crop sideways onto the subject (x)",
            );
        }
        d.extra.insert("from".into(), f.target.clone());
        d.extra.insert("aspect".into(), json!(aspect));
        d.extra.insert("clips".into(), json!(clips));
        d.steps.next("layout.check", json!({}), "titles and captions inside the new safe areas");
        Ok(())
    })?;
    super::follow::advance(s, d, wait)
}
