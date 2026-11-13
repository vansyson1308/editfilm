//! What an id a caller passed as a clip id really is (#14).
//!
//! Clips, links, groups, tracks, transitions, markers and project items all take their ids from
//! the one counter of the project (`Project::alloc_id`), so a link id (the `link` field of a clip
//! in `sequence.inspect`) reads like a clip id: `[52, 53]` where 53 is the link of clip 52 and its
//! sound 54. A command told to act on such an id used to skip it silently or fail with an
//! unrelated reason ("track is locked"). [`check_named_clips`] runs before every command that
//! documents `clips` / `clip` and refuses an id that is no clip of the active sequence, saying
//! what it is and which clip ids to pass instead.

use filmagic_project::{ClipId, ItemId, Sequence, TrackKind};
use serde_json::Value;

use crate::commands::{CommandSpec, NamedKeys};
use crate::{EngineError, Result, Session};

/// `V2` / `A1` for a track of `q`.
pub(crate) fn track_tag(q: &Sequence, t: filmagic_project::TrackId) -> Option<String> {
    let find = |kind: TrackKind, p: &str| q.tracks(kind).iter().position(|x| x.id == t).map(|i| format!("{p}{}", i + 1));
    find(TrackKind::Video, "V").or_else(|| find(TrackKind::Audio, "A"))
}

/// "52 (video, V1) and 54 (audio, A1)" for the clips of `q` that `pick` selects.
fn list_members(q: &Sequence, pick: impl Fn(&filmagic_project::TrackItem) -> bool) -> Vec<String> {
    let mut out = Vec::new();
    for (kind, word, p) in [(TrackKind::Video, "video", "V"), (TrackKind::Audio, "audio", "A")] {
        for (i, t) in q.tracks(kind).iter().enumerate() {
            for it in t.items.iter().filter(|it| pick(it)) {
                out.push(format!("{} ({word}, {p}{})", it.id.0, i + 1));
            }
        }
    }
    out
}

fn join(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// What `id` is, for a caller that passed it as a clip id of `q` (the active sequence) and it is
/// none. Always one sentence that ends with what to pass instead.
pub fn describe_non_clip(s: &Session, q: &Sequence, id: u64) -> String {
    const HINT: &str = "clip ids are the `clip` numbers of the items in sequence.inspect";
    let links = list_members(q, |it| it.link == Some(id));
    if !links.is_empty() {
        return format!("{id} is a link id, not a clip id; its linked clips are {}. Pass those clip ids", join(&links));
    }
    let group = list_members(q, |it| it.group == Some(id));
    if !group.is_empty() {
        return format!("{id} is a group id, not a clip id; its grouped clips are {}. Pass those clip ids", join(&group));
    }
    if let Some(tag) = track_tag(q, filmagic_project::TrackId(id)) {
        return format!("{id} is the id of track {tag}, not of a clip; pass the ids of its clips ({HINT})");
    }
    for t in q.all_tracks() {
        if let Some(x) = t.transitions.iter().find(|x| x.id.0 == id) {
            let tag = track_tag(q, t.id).unwrap_or_default();
            let clips: Vec<String> = x.from.iter().chain(&x.to).map(|c| c.0.to_string()).collect();
            return format!("{id} is a transition on {tag} (between clips {}), not a clip; use the transition.* commands for it", clips.join(" and "));
        }
        if let Some(it) = t.items.iter().find(|it| it.markers.iter().any(|m| m.id.0 == id)) {
            return format!("{id} is a marker of clip {}, not a clip; pass {}", it.id.0, it.id.0);
        }
    }
    if q.markers.iter().any(|m| m.id.0 == id) {
        return format!("{id} is a sequence marker, not a clip ({HINT})");
    }
    if let Some(i) = s.project.item(ItemId(id)) {
        let placed = list_members(q, |it| it.item == ItemId(id));
        return if placed.is_empty() {
            format!("{id} is the project item \"{}\" (an `item` id), not a timeline clip, and it is not in the active sequence", i.name)
        } else {
            format!("{id} is the project item \"{}\" (an `item` id), not a timeline clip; its clips in the active sequence are {}", i.name, join(&placed))
        };
    }
    let elsewhere = s.project.sequences().find(|i| i.as_sequence().is_some_and(|o| o.find_item(ClipId(id)).is_some()));
    match elsewhere {
        Some(i) => format!("clip {id} is in sequence \"{}\", not the active one: open it first with sequence.open {{\"item\":{}}}", i.name, i.id.0),
        None => format!("no clip {id} in the active sequence ({HINT})"),
    }
}

/// The clip ids a caller named under the `clips` / `clip` keys `spec` documents, raw.
fn named_values<'a>(spec: &CommandSpec, p: &'a Value) -> Vec<(&'static str, &'a Value)> {
    let mut out = Vec::new();
    if spec.named.contains(NamedKeys::CLIPS)
        && let Some(a) = p.get("clips").and_then(Value::as_array)
    {
        out.extend(a.iter().map(|v| ("clips", v)));
    }
    if spec.named.contains(NamedKeys::CLIP)
        && let Some(v) = p.get("clip").filter(|v| !v.is_null())
    {
        out.push(("clip", v));
    }
    out
}

/// Refuse a call that names, under a `clips` / `clip` key its command documents, a value that is
/// no clip (or caption) of the active sequence: a link, group, track, transition, marker or item
/// id, a clip of another sequence, a number of nothing, or a value that is not an id. The error
/// says which and what to pass instead. Commands used to skip such ids, so `[clip, link]` changed
/// only the clip, and some failed with an unrelated reason. Without an open sequence the command
/// decides (most then say none is open).
pub fn check_named_clips(s: &Session, spec: &CommandSpec, p: &Value) -> Result<()> {
    let named = named_values(spec, p);
    if named.is_empty() {
        return Ok(());
    }
    let Some(q) = s.active_sequence() else { return Ok(()) };
    let mut why = Vec::new();
    for (key, v) in named {
        let Some(id) = v.as_u64() else {
            why.push(format!("`{key}` holds {v}, which is not a clip id (clip ids are the `clip` numbers of the items in sequence.inspect)"));
            continue;
        };
        if q.find_item(ClipId(id)).is_some() || q.find_caption(ClipId(id)).is_some() {
            continue;
        }
        why.push(describe_non_clip(s, q, id));
    }
    why.dedup();
    if why.is_empty() {
        return Ok(());
    }
    Err(EngineError::BadParams { cmd: spec.id.to_string(), msg: format!("{}. Nothing was changed", why.join("; ")) })
}
