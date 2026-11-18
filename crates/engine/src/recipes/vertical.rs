//! `vertical`: repurpose footage or a finished sequence for a phone (9:16), square (1:1) or
//! portrait (4:5) feed.
//!
//! A sequence (the active one, or `sequence`) gets a copy in the new shape. A project item or a
//! timeline clip gets a new sequence of that shape holding it. Where this build has
//! `clip.autoReframe` the picture follows the subject; elsewhere it fills the frame from the
//! centre, and the result says how to move the crop.

use filmagic_project::ItemId;
use filmagic_time::Tick;
use serde_json::{Map, Value, json};

use super::{RUN, Recipe, Steps, aspect_p, aspect_size, done, footage, text};
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

pub(super) fn run(s: &mut Session, p: &Value) -> Result<Value> {
    let aspect = aspect_p(p, "aspect", Some("9:16"), RUN)?.ok_or_else(|| bad(RUN, "`aspect` must be \"9:16\", \"1:1\" or \"4:5\" for this recipe"))?;
    let name = text(p, "name", 120, None, RUN)?;
    let given = |k: &str| p.get(k).is_some_and(|v| !v.is_null());
    if [given("sequence"), given("item"), given("clip")].iter().filter(|x| **x).count() > 1 {
        return Err(bad(RUN, "pass one of `sequence`, `item` or `clip`"));
    }
    let item_is_seq = p.get("item").and_then(Value::as_u64).is_some_and(|i| s.project.sequence(ItemId(i)).is_some());
    if given("item") && !item_is_seq || given("clip") {
        return media(s, p, aspect, name);
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
    let rate = s.project.sequence(seq).map(|q| q.settings.frame_rate).unwrap_or_default();
    let seq_name = s.project.item(seq).map(|i| i.name.clone()).unwrap_or_default();
    let name = name.unwrap_or_else(|| format!("{seq_name} {aspect}"));
    s.edit_group(LABEL, |s| {
        let mut st = Steps::default();
        if s.state.active_sequence != Some(seq) {
            st.must(s, "the sequence to repurpose", "sequence.open", json!({"item": seq.0}))?;
        }
        let shaped = super::reframe(s, &mut st, seq, aspect, &name, rate)?;
        let mut extra = Map::new();
        extra.insert("from".into(), json!({"sequence": seq.0}));
        extra.insert("aspect".into(), json!(aspect));
        extra.insert("shaped".into(), json!(shaped.map(|x| x.0)));
        st.next("layout.check", json!({}), "titles and captions inside the new safe areas");
        st.next("story.board", json!({"path": "vertical-board.png"}), "look at the new framing, one frame per shot");
        Ok(done(&RECIPE, s, st, extra, LABEL))
    })
}

/// An item or a clip's media in a new sequence of the shape.
fn media(s: &mut Session, p: &Value, aspect: &str, name: Option<String>) -> Result<Value> {
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
    s.edit_group(LABEL, |s| {
        let mut st = Steps { notes: f.notes.clone(), ..Default::default() };
        st.must(s, "a sequence of the new shape", "file.newSequence", json!({"name": name, "fromItem": f.item.0, "place": false}))?;
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
        let followed = crate::commands::find("clip.autoReframe").is_some()
            && st.may(s, "follow the subject", "clip.autoReframe", json!({"clips": clips, "output": "keyframes", "aspect": aspect, "wait": true})).is_some();
        if !followed {
            if crate::commands::find("clip.autoReframe").is_none() {
                st.skip(
                    "clip.autoReframe",
                    json!({"clips": clips, "aspect": aspect}),
                    "follow the subject",
                    "this build does not have the command yet: a centre crop instead",
                );
            }
            st.must(s, "fill the frame from the centre", "clip.fillFrame", json!({"clips": clips}))?;
            st.next(
                "effects.setParam",
                json!({"clip": first, "effect": "motion", "param": "position", "value": [w / 2, h / 2]}),
                "move the crop sideways onto the subject (x)",
            );
        }
        let mut extra = Map::new();
        extra.insert("from".into(), f.target.clone());
        extra.insert("aspect".into(), json!(aspect));
        extra.insert("clips".into(), json!(clips));
        st.next("layout.check", json!({}), "titles and captions inside the new safe areas");
        Ok(done(&RECIPE, s, st, extra, LABEL))
    })
}
