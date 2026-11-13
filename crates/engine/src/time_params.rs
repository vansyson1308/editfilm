//! Time-valued command parameters (#13): one convention for every command.
//!
//! A parameter whose doc types it as ticks (`ticks`, `tick`, `mediaTicks`, `timelineTicks`, or
//! the `[[ticks,…]]` of a point list) takes either
//!
//! - an integer tick count (254 016 000 000 per second), exactly as before, or
//! - a time object: `{"seconds": f}`, `{"frame": n}`, `{"timecode": "HH:MM:SS:FF"}` or
//!   `{"ticks": n}`.
//!
//! [`normalize`] turns every time object into ticks before a command runs, so the commands keep
//! reading integers and old scripts keep working. The doc of each command says which of these
//! time bases a value is in, and the base decides the frame rate a `frame` or `timecode` counts
//! in:
//!
//! - **sequence time** (`ticks`): from the start of the sequence, at its frame rate. A relative
//!   timecode (`"+10"`, `"-00:00:01:00"`) counts from the playhead.
//! - **source time** (`mediaTicks`, and `mediaTime` / `sourceIn`, and `time` with
//!   `"target":"source"`): media time of the clip or item the command names (`clip`, `item`, or
//!   the Source Monitor's item), at that media's frame rate.
//! - **an offset** (`delta`, `timelineTicks`): a signed length of sequence time.
//! - **a length** (`duration` and the graphics roll and responsive timings): a length of sequence
//!   time, never negative.
//!
//! Hostile values (non-finite or out-of-range numbers, negative absolute times, fractional
//! frames, malformed or over-long timecode, a frame rate of zero) are refused with an error that
//! says what to send instead.

use std::collections::HashMap;
use std::sync::OnceLock;

use filmagic_project::ItemId;
use filmagic_time::{FrameRate, TICKS_PER_SECOND, Tick, parse_timecode};
use serde_json::{Map, Value};

use crate::commands::{bad, command_specs};
use crate::{Result, Session};

/// Longest timecode text accepted (a timecode has at most a dozen characters; this leaves room
/// for spaces and a sign).
pub(crate) const MAX_TIMECODE_LEN: usize = 64;

/// Largest magnitude of `seconds` that fits the tick range (about 420 days).
pub(crate) const MAX_SECONDS: f64 = i64::MAX as f64 / TICKS_PER_SECOND as f64;

/// The time base of a parameter: what its value is measured from, and in which frame rate a
/// `frame` or `timecode` counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Base {
    /// From the start of the sequence, at the sequence frame rate; never negative.
    Sequence,
    /// Media time of the named clip or item, at its frame rate; never negative.
    Source,
    /// A signed offset of sequence time.
    Offset,
    /// A length of sequence time; never negative.
    Length,
}

impl Base {
    fn name(self) -> &'static str {
        match self {
            Base::Sequence => "sequence time",
            Base::Source => "source (media) time",
            Base::Offset => "an offset of sequence time",
            Base::Length => "a length of sequence time",
        }
    }
    fn signed(self) -> bool {
        self == Base::Offset
    }
}

/// One time-valued parameter of a command, read from its params doc.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimeKey {
    /// Containers on the way to the key: `("keyframes", true)` is the array `keyframes`, whose
    /// objects hold the key.
    pub path: Vec<(String, bool)>,
    pub key: String,
    /// The doc's base for the key; `time` with `"target":"source"` is source time whatever this says.
    pub base: Base,
    /// `"points":[[ticks,value]]`: the time is the first element of each inner array.
    pub positional: bool,
}

const TICK_TYPES: [&str; 4] = ["ticks", "tick", "mediaTicks", "timelineTicks"];
/// Graphics roll and responsive timings, and other lengths documented as plain `ticks`.
const LENGTH_KEYS: [&str; 7] = ["duration", "preroll", "easeIn", "easeOut", "postroll", "intro", "outro"];

fn base_of(key: &str, ty: &str) -> Base {
    match (key, ty) {
        (_, "mediaTicks") | ("mediaTime" | "sourceIn", _) => Base::Source,
        (_, "timelineTicks") | ("delta", _) => Base::Offset,
        (k, _) if LENGTH_KEYS.contains(&k) => Base::Length,
        _ => Base::Sequence,
    }
}

/// The time-valued parameters a params doc declares. A key the doc already gives an object form
/// (`"sourceIn":ticks|{"frame"|…}`) is read by its command and left out.
pub fn keys_of(doc: &str) -> Vec<TimeKey> {
    let b = doc.as_bytes();
    let mut out: Vec<TimeKey> = Vec::new();
    // containers: (the key they are the value of, is an array)
    let mut stack: Vec<(String, bool)> = Vec::new();
    let mut last_key = String::new();
    let mut after_colon = false;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match c {
            b'"' => {
                let start = i + 1;
                let end = doc[start..].find('"').map_or(b.len(), |n| start + n);
                let text = doc.get(start..end).unwrap_or("");
                i = end + 1;
                after_colon = false;
                if b.get(i) == Some(&b':') {
                    last_key = text.to_string();
                    i += 1;
                    after_colon = true;
                    let rest = doc.get(i..).unwrap_or("");
                    let path: Vec<(String, bool)> = stack.iter().filter(|(k, _)| !k.is_empty()).cloned().collect();
                    if rest.starts_with("[[ticks") {
                        out.push(TimeKey { path, key: last_key.clone(), base: Base::Sequence, positional: true });
                    } else {
                        let ident: String = rest.chars().take_while(char::is_ascii_alphabetic).collect();
                        if TICK_TYPES.contains(&ident.as_str()) {
                            let tail = rest.get(ident.len()..).unwrap_or("");
                            let tail = tail.strip_prefix('?').unwrap_or(tail);
                            if !tail.starts_with("|{") {
                                out.push(TimeKey { path, key: last_key.clone(), base: base_of(&last_key, &ident), positional: false });
                            }
                        }
                    }
                }
                continue;
            }
            b'{' | b'[' => {
                let name = if after_colon { std::mem::take(&mut last_key) } else { String::new() };
                stack.push((name, c == b'['));
            }
            b'}' | b']' => {
                stack.pop();
            }
            _ => {}
        }
        if !c.is_ascii_whitespace() {
            after_colon = false;
        }
        i += 1;
    }
    out.dedup();
    out
}

/// Every command's time-valued parameters, by command id.
pub fn registry() -> &'static HashMap<&'static str, Vec<TimeKey>> {
    static KEYS: OnceLock<HashMap<&'static str, Vec<TimeKey>>> = OnceLock::new();
    KEYS.get_or_init(|| command_specs().iter().map(|c| (c.id, keys_of(c.params))).filter(|(_, k)| !k.is_empty()).collect())
}

/// What a `frame` or `timecode` counts in: frame rate, drop-frame, and the frame a relative
/// timecode counts from. `None` when there is nothing to count frames of.
pub type Clock = Option<(FrameRate, bool, Tick)>;

/// Ticks of one time value under `key` (in `base`), or `Ok(None)` for null / absent. `clock` is
/// asked for only when a `frame` or `timecode` needs it.
pub fn value_ticks(cmd: &str, key: &str, v: &Value, base: Base, clock: impl FnOnce() -> Clock) -> Result<Option<i64>> {
    let forms = "ticks (an integer), or {\"seconds\": s}, {\"frame\": n} or {\"timecode\": \"HH:MM:SS:FF\"}";
    let range = |what: String| bad(cmd, format!("`{key}` {what} is beyond the time range (±{MAX_SECONDS:.0} seconds)"));
    let t = match v {
        Value::Null => return Ok(None),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                // integer ticks: unchanged, whatever their sign (the command decides)
                return Ok(Some(i));
            }
            // u64 beyond i64, or a float: a float tick count is rounded; neither may leave the range
            let f = n.as_f64().filter(|f| f.is_finite()).ok_or_else(|| range(n.to_string()))?;
            if f.abs() >= i64::MAX as f64 {
                return Err(range(format!("{f} ticks")));
            }
            f.round() as i64
        }
        Value::Object(o) => object_ticks(cmd, key, o, base, clock)?,
        _ => return Err(bad(cmd, format!("`{key}` must be {forms}; it is in {}", base.name()))),
    };
    if t < 0 && !base.signed() {
        return Err(bad(cmd, format!("`{key}` must not be negative ({} is {:.3} s); it is {}", v, Tick(t).seconds(), base.name())));
    }
    Ok(Some(t))
}

fn object_ticks(cmd: &str, key: &str, o: &Map<String, Value>, base: Base, clock: impl FnOnce() -> Clock) -> Result<i64> {
    let forms = "one of {\"seconds\": s}, {\"frame\": n}, {\"timecode\": \"HH:MM:SS:FF\"} or {\"ticks\": n}";
    if o.len() != 1 {
        return Err(bad(cmd, format!("`{key}` must hold exactly {forms}, got {}", Value::Object(o.clone()))));
    }
    let Some((unit, v)) = o.iter().next() else { return Err(bad(cmd, format!("`{key}` must hold {forms}"))) };
    let range = || bad(cmd, format!("`{key}` {v} is beyond the time range (±{MAX_SECONDS:.0} seconds)"));
    let need_clock = |what: &str| -> Result<(FrameRate, bool, Tick)> {
        let Some((rate, df, current)) = clock() else {
            return Err(bad(cmd, format!("`{key}` in {what} needs a frame rate, and there is none here ({}): give {{\"seconds\": s}}", base.name())));
        };
        if rate.num <= 0 || rate.den <= 0 || rate.frame_duration() <= Tick::ZERO {
            return Err(bad(cmd, format!("`{key}`: the frame rate {}/{} is not usable for {what}; give {{\"seconds\": s}}", rate.num, rate.den)));
        }
        Ok((rate, df, current))
    };
    let frame_ticks = |rate: FrameRate, f: i64| -> Result<i64> {
        let t = rate.tick_of(f);
        // tick_of saturates: a frame that does not round-trip is out of range
        if rate.frame_at(t) != f { Err(range()) } else { Ok(t.0) }
    };
    match unit.as_str() {
        "seconds" => {
            let s = v.as_f64().ok_or_else(|| bad(cmd, format!("`{key}`.seconds must be a number, got {v}")))?;
            if !s.is_finite() || s.abs() >= MAX_SECONDS {
                return Err(range());
            }
            Ok(Tick::from_seconds_f64(s).0)
        }
        "frame" | "frames" => {
            let f = v
                .as_i64()
                .or_else(|| v.as_f64().filter(|f| f.is_finite() && f.fract() == 0.0 && f.abs() < i64::MAX as f64).map(|f| f as i64))
                .ok_or_else(|| bad(cmd, format!("`{key}`.frame must be a whole number of frames, got {v}")))?;
            let (rate, _, _) = need_clock("frames")?;
            frame_ticks(rate, f)
        }
        "timecode" => {
            let tc = v.as_str().ok_or_else(|| bad(cmd, format!("`{key}`.timecode must be text like \"00:00:52:12\", got {v}")))?;
            if tc.chars().count() > MAX_TIMECODE_LEN {
                return Err(bad(cmd, format!("`{key}`.timecode is longer than {MAX_TIMECODE_LEN} characters")));
            }
            let (rate, df, current) = need_clock("timecode")?;
            // a relative timecode counts from the playhead for an absolute time, from zero for a length
            let from = if matches!(base, Base::Sequence | Base::Source) { rate.frame_at(current) } else { 0 };
            let f = parse_timecode(tc, rate, df, from)
                .map_err(|e| bad(cmd, format!("`{key}`.timecode `{tc}` is not a timecode ({}): use HH:MM:SS:FF, e.g. \"00:00:52:12\"", e.0)))?;
            frame_ticks(rate, f)
        }
        "ticks" | "time" => v
            .as_i64()
            .or_else(|| v.as_f64().filter(|f| f.is_finite() && f.abs() < i64::MAX as f64).map(|f| f.round() as i64))
            .ok_or_else(|| bad(cmd, format!("`{key}`.ticks must be an integer in the time range, got {v}"))),
        other => Err(bad(cmd, format!("`{key}` has an unknown unit `{other}`: give {forms}"))),
    }
}

/// Sequence clock: the `sequence` the command names, else the active one.
fn sequence_clock(s: &Session, top: &Value) -> Clock {
    let id = top.get("sequence").and_then(Value::as_u64).map(ItemId).or(s.state.active_sequence)?;
    let q = s.project.sequence(id)?;
    let current = s.state.playheads.get(&id).copied().unwrap_or_else(|| if Some(id) == s.state.active_sequence { s.playhead() } else { Tick::ZERO });
    Some((q.settings.frame_rate, q.settings.drop_frame, current))
}

/// Source clock: the media of the `clip` or `item` named next to the key or at the top level,
/// else the Source Monitor's item.
fn source_clock(s: &Session, here: &Value, top: &Value) -> Clock {
    let item_of = |v: &Value| -> Option<ItemId> {
        if let Some(i) = v.get("item").and_then(Value::as_u64) {
            return Some(ItemId(i));
        }
        let c = filmagic_project::ClipId(v.get("clip").and_then(Value::as_u64)?);
        s.active_sequence().and_then(|q| q.find_item(c)).map(|(_, it)| it.item)
    };
    let item = item_of(here).or_else(|| item_of(top)).or(s.state.source_item)?;
    let rate = crate::clip_ops::source_view(s, item).map(|v| v.rate).or_else(|| s.project.item(item).map(|i| i.frame_rate()))?;
    let df = s.project.sequence(item).is_some_and(|q| q.settings.drop_frame);
    let current = if Some(item) == s.state.source_item { s.state.source_playhead } else { Tick::ZERO };
    Some((rate, df, current))
}

/// Turn every time object (and every float tick count) under the time-valued parameters of
/// command `id` into integer ticks. Integer ticks pass through untouched.
pub fn normalize(s: &Session, id: &str, mut params: Value) -> Result<Value> {
    let Some(keys) = registry().get(id) else { return Ok(params) };
    let top = params.clone();
    let to_source = top.get("target").and_then(Value::as_str) == Some("source");
    for k in keys {
        let base = if to_source && k.key == "time" { Base::Source } else { k.base };
        let path: Vec<&str> = k.path.iter().map(|(p, _)| p.as_str()).collect();
        apply(&mut params, &path, 0, &mut |obj| convert_in(s, id, k, base, obj, &top))?;
    }
    Ok(params)
}

/// Depth limit of the path walk: the docs nest two levels at most.
const MAX_DEPTH: usize = 8;

fn apply(v: &mut Value, path: &[&str], depth: usize, f: &mut dyn FnMut(&mut Value) -> Result<()>) -> Result<()> {
    if depth > MAX_DEPTH {
        return Ok(());
    }
    match v {
        Value::Array(items) => {
            for it in items {
                apply(it, path, depth + 1, f)?;
            }
            Ok(())
        }
        Value::Object(o) => match path.split_first() {
            None => f(v),
            Some((first, rest)) => match o.get_mut(*first) {
                Some(child) => apply(child, rest, depth + 1, f),
                None => Ok(()),
            },
        },
        _ => Ok(()),
    }
}

/// Convert `k.key` in the object `obj` (which holds it, with any `clip` / `item` beside it).
fn convert_in(s: &Session, id: &str, k: &TimeKey, base: Base, obj: &mut Value, top: &Value) -> Result<()> {
    let here = obj.clone();
    let clock = &|| match base {
        Base::Source => source_clock(s, &here, top),
        _ => sequence_clock(s, top),
    };
    let Some(slot) = obj.get_mut(&k.key) else { return Ok(()) };
    if k.positional {
        let Some(points) = slot.as_array_mut() else { return Ok(()) };
        for (n, p) in points.iter_mut().enumerate() {
            if let Some(first) = p.as_array_mut().and_then(|a| a.first_mut())
                && let Some(t) = value_ticks(id, &format!("{}[{n}][0]", k.key), first, base, clock)?
            {
                *first = Value::from(t);
            }
        }
        return Ok(());
    }
    if let Some(t) = value_ticks(id, &k.key, slot, base, clock)? {
        *slot = Value::from(t);
    }
    Ok(())
}

/// Seconds, rounded to the microsecond, for results that report ticks.
pub fn secs(t: Tick) -> f64 {
    (t.seconds() * 1e6).round() / 1e6
}

/// Commands that place or lengthen a graphic, title or caption: their result warns when it runs
/// past the sequence's last picture frame (#13).
pub(crate) const PLACES_GRAPHICS: [&str; 11] = [
    "graphics.newText",
    "graphics.newVerticalText",
    "graphics.newShape",
    "graphics.newRectangle",
    "graphics.newEllipse",
    "graphics.newPolygon",
    "graphics.newFromFile",
    "graphics.template.apply",
    "graphics.resetDuration",
    "captions.add",
    "timeline.place",
];

/// Ends of the active sequence's video clips and captions before a command, by id.
pub(crate) struct Ends(HashMap<filmagic_project::ClipId, Tick>);

pub(crate) fn ends(s: &Session) -> Ends {
    let mut m = HashMap::new();
    if let Some(q) = s.active_sequence() {
        for it in q.video_tracks.iter().flat_map(|t| &t.items) {
            m.insert(it.id, it.end());
        }
        for c in q.caption_tracks.iter().flat_map(|t| &t.captions) {
            m.insert(c.id, tadd(c.start, c.duration));
        }
    }
    Ends(m)
}

/// A clip whose picture is a graphic: a title, shape or template, or a still image.
fn graphic_like(s: &Session, item: ItemId) -> bool {
    match s.project.item(item).map(|i| &i.kind) {
        Some(filmagic_project::ItemKind::Graphic { .. }) => true,
        Some(filmagic_project::ItemKind::Media(m)) => matches!(m.info.kind, filmagic_media::MediaKind::Still),
        _ => false,
    }
}

/// Warn, in `result`, about each graphic or caption this command placed or lengthened that ends
/// after the last picture frame of the rest of the sequence, and say how to trim it. There is no
/// trim option: an end card may run on past the last shot on purpose, and the one-call trim the
/// warning gives is exact.
pub(crate) fn warn_past_end(s: &Session, before: &Ends, result: &mut Value) {
    let Some(q) = s.active_sequence() else { return };
    let rate = q.settings.frame_rate;
    let changed = |id: filmagic_project::ClipId, end: Tick| before.0.get(&id).is_none_or(|b| *b < end);
    let video = q.video_tracks.iter().flat_map(|t| &t.items);
    let placed: Vec<_> = video.clone().filter(|it| changed(it.id, it.end()) && graphic_like(s, it.item)).collect();
    let captions: Vec<_> = q.caption_tracks.iter().flat_map(|t| &t.captions).filter(|c| changed(c.id, tadd(c.start, c.duration))).collect();
    if placed.is_empty() && captions.is_empty() {
        return;
    }
    // the picture: every video clip but the ones just placed
    let Some(picture_end) = video.filter(|it| !placed.iter().any(|p| p.id == it.id)).map(|it| it.end()).max() else { return };
    let mut found = Vec::new();
    let mut warnings = Vec::new();
    let mut note = |kind: &str, id: u64, end: Tick, fix: String| {
        let over = tsub(end, picture_end);
        if over <= Tick::ZERO {
            return;
        }
        let frames = rate.frame_at(tsub(tadd(over, rate.frame_duration()), Tick(1)));
        warnings.push(Value::from(format!(
            "{kind} {id} ends at {:.3} s, {:.3} s ({frames} frames) after the last picture frame at {:.3} s. If it should end with the picture, run {fix}",
            end.seconds(),
            over.seconds(),
            picture_end.seconds()
        )));
        found.push(serde_json::json!({
            "kind": kind, "id": id, "end": end.0, "endSeconds": secs(end), "pictureEnd": picture_end.0,
            "pictureEndSeconds": secs(picture_end), "over": over.0, "overSeconds": secs(over), "overFrames": frames,
        }));
    };
    for it in &placed {
        let d = -(tsub(it.end(), picture_end)).0;
        note("graphic clip", it.id.0, it.end(), format!("timeline.trim {{\"clip\":{},\"edge\":\"out\",\"mode\":\"regular\",\"delta\":{d}}}", it.id.0));
    }
    for c in &captions {
        let end = tadd(c.start, c.duration);
        let d = -(tsub(end, picture_end)).0;
        note("caption", c.id.0, end, format!("captions.trim {{\"caption\":{},\"edge\":\"out\",\"delta\":{d}}}", c.id.0));
    }
    if found.is_empty() {
        return;
    }
    if !result.is_object() {
        *result = serde_json::json!({ "result": result.take() });
    }
    if let Some(o) = result.as_object_mut() {
        match o.get_mut("warnings").and_then(Value::as_array_mut) {
            Some(w) => w.extend(warnings),
            None => {
                o.insert("warnings".into(), Value::Array(warnings));
            }
        }
        o.insert("pastSequenceEnd".into(), Value::Array(found));
    }
}

fn tadd(a: Tick, b: Tick) -> Tick {
    Tick(a.0.saturating_add(b.0))
}
fn tsub(a: Tick, b: Tick) -> Tick {
    Tick(a.0.saturating_sub(b.0))
}
