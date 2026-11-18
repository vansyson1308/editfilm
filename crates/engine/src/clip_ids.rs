//! What an id a caller passed as a clip id really is (#14).
//!
//! Clips, links, groups, tracks, transitions, markers and project items all take their ids from
//! the one counter of the project (`Project::alloc_id`), so a link id (the `link` field of a clip
//! in `sequence.inspect`) reads like a clip id: `[52, 53]` where 53 is the link of clip 52 and its
//! sound 54. A command told to act on such an id used to skip it silently or fail with an
//! unrelated reason ("track is locked"). [`check_named_clips`] runs before every command that
//! documents `clips` / `clip` as clip ids and refuses an id that is no clip of the sequence the
//! command works on, saying what it is and which clip ids to pass instead.
//!
//! What is checked, and against which sequence:
//! - the keys a command's params doc declares as `"clips":[id]` or `"clip":id`, at the outermost
//!   call only; `clip` holds a list only where the doc says `"clip":id|[id]` (elsewhere a list is
//!   refused, since those commands read one id and would ignore it) (commands that run other commands pass ids they already hold);
//!   keys a doc does not declare, and ids nested in other values (`moves[].clip`), are the
//!   command's own business;
//! - against the sequence named by the command's `sequence` where its doc declares one
//!   (`edit.label` from the custom-colour dialog), or by the `item` of a command whose `item` is
//!   the sequence it inspects ([`INSPECTS_ITEM`]: `sequence.inspect`), else the active one;
//! - not at all when the command takes `item` *instead of* `clip` ([`ITEM_OR_CLIP`]) and an
//!   `item` is given, since the clip is then ignored;
//! - caption ids count only for commands that act on captions ([`takes_captions`]): today none of
//!   the commands that document `clips` / `clip` does (captions have their own `captions` key).

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

/// Commands whose `item` and `clip` are alternatives and `item` wins: with `item` given, `clip` is
/// not read. (`source.open` and `clip.makeSubclip` read `clip` first; `story.blinkPoints` refuses
/// both together: their `clip` is checked.)
const ITEM_OR_CLIP: &[&str] = &["clip.rename", "metadata.get", "metadata.set", "tts.inspect"];

/// Commands whose `item` is the sequence they look at, so their clip ids belong to it.
const INSPECTS_ITEM: &[&str] = &["sequence.inspect"];

/// The doc form of a `clip` that takes one id or a list.
const CLIP_OR_LIST: &str = r#""clip":id|[id]"#;

/// Whether `spec` acts on captions named under `clips` / `clip`, so a caption id is one of its
/// targets. Commands on captions are `captions.*`; none of them documents `clips` / `clip` today.
pub fn takes_captions(spec: &CommandSpec) -> bool {
    spec.id.starts_with("captions.")
}

/// The sequence a command's clip ids belong to: its documented `sequence`, or the `item` of an
/// [`INSPECTS_ITEM`] command when that is a sequence, else the active one. `None` when there is none (the command then decides), and also
/// when `sequence` names no sequence (the command reports that itself).
fn target_sequence<'a>(s: &'a Session, spec: &CommandSpec, p: &Value) -> Option<(&'a Sequence, Option<&'a str>)> {
    let named = |k: &str| p.get(k).and_then(Value::as_u64).map(ItemId);
    if spec.params.contains(r#""sequence":id"#) && p.get("sequence").is_some_and(|v| !v.is_null()) {
        let id = named("sequence")?;
        let q = s.project.sequence(id)?;
        let name = s.project.item(id).map(|i| i.name.as_str());
        return Some((q, if s.state.active_sequence == Some(id) { None } else { name }));
    }
    // `sequence.inspect`'s `item` is the sequence it looks at; elsewhere `item` is a source, a
    // replacement (`clip.replaceFromBin` can put a sequence in place of clips) or an alternative
    // the command reads after `clip` (`audio.speechSegments`)
    if INSPECTS_ITEM.contains(&spec.id)
        && let Some(id) = named("item")
        && let Some(q) = s.project.sequence(id)
    {
        let name = s.project.item(id).map(|i| i.name.as_str());
        return Some((q, if s.state.active_sequence == Some(id) { None } else { name }));
    }
    s.active_sequence().map(|q| (q, None))
}

/// What `id` is, for a caller that passed it as a clip id of `q` and it is none. `other` names
/// `q` when it is not the active sequence. Always one sentence that ends with what to pass instead.
pub fn describe_non_clip(s: &Session, q: &Sequence, other: Option<&str>, id: u64) -> String {
    const HINT: &str = "clip ids are the `clip` numbers of the items in sequence.inspect";
    let here = match other {
        Some(n) => format!("sequence \"{n}\""),
        None => "the active sequence".to_string(),
    };
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
    if let Some(k) = q.caption_tracks.iter().position(|t| t.id.0 == id) {
        return format!("{id} is the id of caption track C{}, not of a clip; captions.* commands take caption tracks", k + 1);
    }
    if let Some((tid, _)) = q.find_caption(ClipId(id)) {
        let k = q.caption_tracks.iter().position(|t| t.id == tid).map_or(0, |k| k + 1);
        return format!("{id} is a caption on caption track C{k}, not a clip; caption commands (captions.*, graphics.upgradeCaption) take it under `captions`");
    }
    for t in q.all_tracks() {
        if let Some(x) = t.transitions.iter().find(|x| x.id.0 == id) {
            let tag = track_tag(q, t.id).unwrap_or_default();
            let at = match (x.from, x.to) {
                (Some(a), Some(b)) => format!("between clips {} and {}", a.0, b.0),
                (Some(a), None) => format!("at the end of clip {}", a.0),
                (None, Some(b)) => format!("at the start of clip {}", b.0),
                (None, None) => "on no clip".to_string(),
            };
            return format!("{id} is a transition on {tag} ({at}), not a clip; use the transition.* commands for it");
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
            format!("{id} is the project item \"{}\" (an `item` id), not a timeline clip, and it is not in {here}", i.name)
        } else {
            format!("{id} is the project item \"{}\" (an `item` id), not a timeline clip; its clips in {here} are {}", i.name, join(&placed))
        };
    }
    let elsewhere = s.project.sequences().find(|i| i.as_sequence().is_some_and(|o| !std::ptr::eq(o, q) && o.find_item(ClipId(id)).is_some()));
    match (elsewhere, other) {
        (Some(i), None) => {
            format!("clip {id} is in sequence \"{}\", not the active one: open it first with sequence.open {{\"item\":{}}}", i.name, i.id.0)
        }
        (Some(i), Some(_)) => format!("clip {id} is in sequence \"{}\", not in {here}", i.name),
        (None, _) => format!("no clip {id} in {here} ({HINT})"),
    }
}

/// A `clip` list sent to a command that reads one id: refused, with what to send instead.
fn clip_list_refused(spec: &CommandSpec, p: &Value) -> Option<String> {
    if !spec.named.contains(NamedKeys::CLIP) || spec.params.contains(CLIP_OR_LIST) {
        return None;
    }
    if ITEM_OR_CLIP.contains(&spec.id) && p.get("item").is_some_and(|v| !v.is_null()) {
        return None;
    }
    let a = p.get("clip")?.as_array()?;
    let one = match a.first().and_then(Value::as_u64) {
        Some(id) => format!("pass {id}"),
        None => "pass one clip id".to_string(),
    };
    let many = if spec.named.contains(NamedKeys::CLIPS) { ", or use `clips:[...]` for several" } else { "" };
    Some(format!("`clip` takes one id here, not a list; {one}{many}"))
}

/// The clip ids a caller named under the `clips` / `clip` keys `spec` documents, raw. `clip` may
/// hold a list where the doc says so ([`CLIP_OR_LIST`]).
fn named_values<'a>(spec: &CommandSpec, p: &'a Value) -> Vec<(&'static str, &'a Value)> {
    let mut out = Vec::new();
    if spec.named.contains(NamedKeys::CLIPS)
        && let Some(a) = p.get("clips").and_then(Value::as_array)
    {
        out.extend(a.iter().map(|v| ("clips", v)));
    }
    let item_instead = ITEM_OR_CLIP.contains(&spec.id) && p.get("item").is_some_and(|v| !v.is_null());
    if spec.named.contains(NamedKeys::CLIP)
        && !item_instead
        && let Some(v) = p.get("clip").filter(|v| !v.is_null())
    {
        match v.as_array() {
            Some(a) => out.extend(a.iter().map(|v| ("clip", v))),
            None => out.push(("clip", v)),
        }
    }
    out
}

/// Why `v` is not a clip id, when it is not a whole non-negative number.
fn not_an_id(key: &str, v: &Value) -> String {
    match v.as_f64() {
        // 29.0 reads as a float, and the commands read ids as integers: passing it on would make
        // them skip it, so it is refused with the integer to send
        Some(f) if f.fract() == 0.0 && (0.0..9.0e15).contains(&f) => {
            format!("`{key}` holds {v}, a decimal number; clip ids are whole numbers written without a decimal point: pass {}", f as u64)
        }
        _ => format!("`{key}` holds {v}, which is not a clip id (clip ids are the `clip` numbers of the items in sequence.inspect)"),
    }
}

/// Refuse a call that names, under a `clips` / `clip` key its command documents, a value that is
/// no clip of the sequence it works on (see the module doc): a link, group, track, transition,
/// marker, caption or item id, a clip of another sequence, a number of nothing, or a value that is
/// not an id. The error says which and what to pass instead. Commands used to skip such ids, so
/// `[clip, link]` changed only the clip, and some failed with an unrelated reason. Without a
/// sequence to check against the command decides (most then say none is open).
pub fn check_named_clips(s: &Session, spec: &CommandSpec, p: &Value) -> Result<()> {
    if let Some(why) = clip_list_refused(spec, p) {
        let tail = if spec.journal { ". Nothing was changed" } else { "" };
        return Err(EngineError::BadParams { cmd: spec.id.to_string(), msg: format!("{why}{tail}") });
    }
    let named = named_values(spec, p);
    if named.is_empty() {
        return Ok(());
    }
    let Some((q, other)) = target_sequence(s, spec, p) else { return Ok(()) };
    let captions = takes_captions(spec);
    let usable = |id: u64| q.find_item(ClipId(id)).is_some() || (captions && q.find_caption(ClipId(id)).is_some());
    // a hostile list of a million wrong ids gets a short answer: the first few, then a count
    const SHOWN: usize = 8;
    let mut why = Vec::new();
    let mut more = 0usize;
    for (key, v) in named {
        if why.len() >= SHOWN {
            more += usize::from(!v.as_u64().is_some_and(usable));
            continue;
        }
        let Some(id) = v.as_u64() else {
            why.push(not_an_id(key, v));
            continue;
        };
        if !usable(id) {
            why.push(describe_non_clip(s, q, other, id));
        }
    }
    why.dedup();
    if why.is_empty() {
        return Ok(());
    }
    if more > 0 {
        why.push(format!("and {more} more ids that are no clips"));
    }
    // a query changes nothing either way; an edit is refused before it starts
    let tail = if spec.journal { ". Nothing was changed" } else { "" };
    Err(EngineError::BadParams { cmd: spec.id.to_string(), msg: format!("{}{tail}", why.join("; ")) })
}
