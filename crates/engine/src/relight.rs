//! `color.relight`: grade a shot toward the light of another hour of the day.
//!
//! `{"clip":id,"time":"18:15"|hours,"toTime":…?,"strength":0..1=1,"light":[x,y]?,"lightStrength":0..1=0.5}`
//!
//! Two ordinary, editable effects carry it. **Color Grading** takes the colour and weight of the
//! hour: white balance, tint, exposure, contrast, saturation, highlights and shadows, read from a
//! table of the day ([`DAY`]: blue and dark at night, pink at dawn, neutral and hard at noon, gold
//! before sunset, blue again after). **Lighting Effects** adds one soft light where the sun (or
//! the moon) stands at that hour, or at `light`. With `toTime` both are keyframed from the clip's
//! first frame to its last, through every hour of the table in between, so a day can pass during
//! a shot.
//!
//! This is a grade. It assumes the shot was photographed in neutral daylight and moves its colour
//! and exposure; it does not know the shapes in the scene, so it casts no new shadows and lights no
//! windows. For those, the picture has to be shot or painted that way.

use filmcraft_geom::Vec2;
use filmcraft_project::{ClipId, EffectInstance, Param, ParamValue, TrackItem, TrackKind};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, NamedKeys, bad, clip_p, has_seq};
use crate::{EngineError, Result, Session};

/// The grade of one hour: (hour, temperature, tint, exposure, contrast, saturation, highlights, shadows).
type Hour = (f64, [f64; 7]);

/// Color Grading parameters, in the order of an [`Hour`]'s values.
const GRADE: [&str; 7] = ["temperature", "tint", "exposure", "contrast", "saturation", "highlights", "shadows"];
/// The values that change nothing.
const NEUTRAL: [f64; 7] = [0.0, 0.0, 0.0, 0.0, 100.0, 0.0, 0.0];

/// The day, hour by hour.
pub const DAY: [Hour; 13] = [
    (0.0, [-45.0, 0.0, -1.7, 12.0, 60.0, -20.0, -25.0]),
    (5.0, [-40.0, 4.0, -1.4, 10.0, 65.0, -15.0, -20.0]),
    (6.5, [20.0, 10.0, -0.5, -5.0, 95.0, -5.0, 5.0]),
    (8.0, [18.0, 4.0, -0.1, 0.0, 105.0, 0.0, 0.0]),
    (10.0, [6.0, 0.0, 0.0, 4.0, 100.0, 0.0, 0.0]),
    (12.0, [0.0, 0.0, 0.1, 10.0, 100.0, 0.0, -5.0]),
    (15.0, [6.0, 0.0, 0.0, 5.0, 100.0, 0.0, 0.0]),
    (17.0, [28.0, 4.0, -0.1, -4.0, 110.0, -8.0, 5.0]),
    (18.5, [48.0, 10.0, -0.35, -8.0, 115.0, -15.0, 8.0]),
    (19.5, [30.0, 16.0, -0.8, -5.0, 100.0, -10.0, 0.0]),
    (20.5, [-30.0, 8.0, -1.2, 5.0, 75.0, -15.0, -15.0]),
    (22.0, [-45.0, 0.0, -1.7, 12.0, 60.0, -20.0, -25.0]),
    (24.0, [-45.0, 0.0, -1.7, 12.0, 60.0, -20.0, -25.0]),
];

fn lerp(a: f64, b: f64, k: f64) -> f64 {
    a + (b - a) * k
}

/// The grade at `hour` (0…24), at `strength` (0 = unchanged … 1 = the table's).
pub fn grade_at(hour: f64, strength: f64) -> [f64; 7] {
    let h = hour.clamp(0.0, 24.0);
    let mut out = NEUTRAL;
    for w in DAY.windows(2) {
        let (Some((h0, a)), Some((h1, b))) = (w.first(), w.get(1)) else { continue };
        if h >= *h0 && h <= *h1 {
            let k = if h1 > h0 { (h - h0) / (h1 - h0) } else { 0.0 };
            for i in 0..7 {
                out[i] = lerp(NEUTRAL[i], lerp(a[i], b[i], k), strength.clamp(0.0, 1.0));
            }
            break;
        }
    }
    out
}

/// The key light of an hour: where it stands in the frame (fractions), its colour, and how strong
/// it is against the sun at noon.
pub fn sun_at(hour: f64) -> (Vec2, [f32; 3], f64) {
    let h = hour.clamp(0.0, 24.0);
    if !(6.0..=18.5).contains(&h) {
        // the moon: high, to one side, cool and weak
        return (Vec2::new(0.75, 0.18), [0.62, 0.72, 1.0], 0.4);
    }
    let day = ((h - 6.0) / 12.5).clamp(0.0, 1.0);
    let height = (day * std::f64::consts::PI).sin();
    let pos = Vec2::new(lerp(0.12, 0.88, day), 0.42 - 0.3 * height);
    // white overhead, gold near the horizon
    let low = (1.0 - height) as f32;
    (pos, [1.0, 0.98 - 0.26 * low, 0.94 - 0.54 * low], 0.8 + 0.4 * f64::from(low))
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "color.relight",
        label: "Relight to a Time of Day",
        menu: &[],
        shortcut: None,
        params: r#"{"clip":id,"time":"HH:MM"|hours,"toTime":"HH:MM"|hours? (keyframed across the clip),"strength":0..1=1,"light":[x,y]? (fractions of the frame; default: where the sun stands),"lightStrength":0..1=0.5}"#,
        enabled: has_seq,
        run: relight,
        journal: true,
        named: NamedKeys::NONE,
    }]
}

/// An hour of the day from `"HH:MM"` or a number of hours.
fn hour_p(p: &Value, key: &str, cmd: &str) -> Result<Option<f64>> {
    let wrong = || bad(cmd, format!("`{key}` must be a time of day: \"HH:MM\" or hours from 0 to 24"));
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => {
            let (h, m) = text.trim().split_once(':').ok_or_else(wrong)?;
            let (h, m) = (h.parse::<u32>().map_err(|_| wrong())?, m.parse::<u32>().map_err(|_| wrong())?);
            if h > 24 || m > 59 || (h == 24 && m > 0) {
                return Err(wrong());
            }
            Ok(Some(f64::from(h) + f64::from(m) / 60.0))
        }
        Some(v) => v.as_f64().filter(|x| (0.0..=24.0).contains(x)).map(Some).ok_or_else(wrong),
    }
}

fn unit_p(p: &Value, key: &str, default: f64, cmd: &str) -> Result<f64> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v.as_f64().filter(|x| (0.0..=1.0).contains(x)).ok_or_else(|| bad(cmd, format!("`{key}` must be from 0 to 1"))),
    }
}

fn clock(hour: f64) -> String {
    let minutes = (hour * 60.0).round() as u32;
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

/// How many hours pass from `from` to `to`. The clock only runs forward: an end earlier than the
/// start is on the next day, so 22:00 to 02:00 is four hours through midnight, not twenty back
/// through noon.
fn hours_between(from: f64, to: f64) -> f64 {
    let d = (to - from).rem_euclid(24.0);
    if (1e-9..=24.0 - 1e-9).contains(&d) { d } else { 0.0 }
}

/// The hours a move from `from` to `to` passes: both ends and every hour of the table between
/// them, with how far through the move each lies (0…1).
fn stops(from: f64, to: f64) -> Vec<(f64, f64)> {
    let mut out = vec![(0.0, from)];
    let total = hours_between(from, to);
    if total > 0.0 {
        // each hour of the table by how long after `from` it comes (midnight is in the table twice, as 0 and as 24)
        let mut mid: Vec<(f64, f64)> =
            DAY.iter().map(|d| ((d.0 - from).rem_euclid(24.0), d.0)).filter(|(after, _)| *after > 1e-6 && *after < total - 1e-6).collect();
        mid.sort_by(|a, b| a.0.total_cmp(&b.0));
        mid.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-6);
        out.extend(mid.into_iter().map(|(after, h)| (after / total, h)));
        out.push((1.0, to));
    }
    out
}

/// Set parameter `id` of `e` to the values of `keys` (media time, value): static for one, keyframed for more.
fn write(e: &mut EffectInstance, id: &str, keys: &[(Tick, ParamValue)]) {
    let Some((_, first)) = keys.first() else { return };
    let prm = e.params.entry(id.to_string()).or_insert_with(|| Param::new(first.clone()));
    prm.keyframes.clear();
    prm.value = first.clone();
    if keys.len() > 1 {
        for (t, v) in keys {
            prm.put_keyframe(*t, v.clone());
        }
    }
}

/// What a relight asks for.
struct Ask {
    from: f64,
    to: f64,
    strength: f64,
    light: Option<Vec2>,
    light_strength: f64,
}

/// Write the grade and the light of `ask` into `it`'s Color Grading and Lighting Effects.
fn apply(it: &mut TrackItem, size: (u32, u32), ask: &Ask) -> usize {
    let mut stops = stops(ask.from, ask.to);
    let last = (it.end() - Tick(1)).max(it.start);
    let at = |k: f64| it.effect_time_at(it.start + Tick(((last - it.start).0 as f64 * k) as i64));
    // A clip that shows one moment of its media (a frame hold, a single frame) has no time for the
    // light to change in: every stop would land on one keyframe and the last would win. It gets
    // the light of `time`, still.
    if stops.first().zip(stops.last()).is_some_and(|(a, b)| at(a.0) == at(b.0)) {
        stops.truncate(1);
    }
    let when: Vec<Tick> = stops.iter().map(|(k, _)| at(*k)).collect();
    if let Some(e) = it.effects.iter_mut().find(|e| e.effect == "lumetri") {
        e.enabled = true;
        write(e, "basic_on", &[(Tick::ZERO, ParamValue::Bool(true))]);
        for (i, id) in GRADE.iter().enumerate() {
            let keys: Vec<(Tick, ParamValue)> = stops.iter().zip(&when).map(|((_, h), t)| (*t, ParamValue::Float(grade_at(*h, ask.strength)[i]))).collect();
            write(e, id, &keys);
        }
    }
    if let Some(e) = it.effects.iter_mut().find(|e| e.effect == "lighting_effects") {
        let (w, h) = (f64::from(size.0), f64::from(size.1));
        let on = ask.light_strength > 0.0;
        e.enabled = on;
        let lights: Vec<(Vec2, [f32; 3], f64)> = stops.iter().map(|(_, hr)| sun_at(*hr)).collect();
        let keys =
            |f: &dyn Fn(&(Vec2, [f32; 3], f64)) -> ParamValue| -> Vec<(Tick, ParamValue)> { lights.iter().zip(&when).map(|(l, t)| (*t, f(l))).collect() };
        // one soft omni light over the picture as it is: the rest of the frame keeps nearly all its own light
        write(e, "l1_type", &[(Tick::ZERO, ParamValue::Choice(2))]);
        write(e, "l1_major", &[(Tick::ZERO, ParamValue::Float(60.0))]);
        write(e, "ambient", &[(Tick::ZERO, ParamValue::Float(100.0 - 12.0 * ask.light_strength))]);
        write(e, "l1_center", &keys(&|l| ParamValue::Vec2(ask.light.map_or(Vec2::new(l.0.x * w, l.0.y * h), |p| Vec2::new(p.x * w, p.y * h)))));
        write(e, "l1_color", &keys(&|l| ParamValue::Color([l.1[0], l.1[1], l.1[2], 1.0])));
        write(e, "l1_intensity", &keys(&|l| ParamValue::Float((18.0 * ask.light_strength * l.2).clamp(0.0, 100.0))));
    }
    if stops.len() > 1 { stops.len() } else { 0 }
}

fn relight(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "color.relight";
    let clip: ClipId = clip_p(p, "clip").ok_or_else(|| bad(CMD, "need `clip`: the id of a video clip in the timeline"))?;
    let from = hour_p(p, "time", CMD)?.ok_or_else(|| bad(CMD, "need `time`: \"HH:MM\" or hours from 0 to 24"))?;
    let to = hour_p(p, "toTime", CMD)?.unwrap_or(from);
    let light = match p.get("light") {
        None | Some(Value::Null) => None,
        Some(v) => {
            let xy = v.as_array().filter(|a| a.len() == 2).ok_or_else(|| bad(CMD, "`light` is [x, y] in fractions of the frame"))?;
            let get = |i: usize| {
                xy.get(i).and_then(Value::as_f64).filter(|x| (-1.0..=2.0).contains(x)).ok_or_else(|| bad(CMD, "`light` is [x, y] in fractions of the frame"))
            };
            Some(Vec2::new(get(0)?, get(1)?))
        }
    };
    let ask = Ask { from, to, strength: unit_p(p, "strength", 1.0, CMD)?, light, light_strength: unit_p(p, "lightStrength", 0.5, CMD)? };
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (track, it) = q.find_item(clip).ok_or_else(|| bad(CMD, format!("no clip {} in the active sequence", clip.0)))?;
    let tr = q.track(track).filter(|t| t.kind == TrackKind::Video).ok_or_else(|| bad(CMD, "the clip is a sound clip; relighting needs a picture"))?;
    if tr.locked {
        return Err(bad(CMD, format!("track {} is locked", tr.name)));
    }
    let size = filmcraft_render::source_size(&s.project, it.item).filter(|(w, h)| *w > 0 && *h > 0).ok_or_else(|| bad(CMD, "the clip has no picture"))?;
    let has = |id: &str| it.effects.iter().any(|e| e.effect == id);
    let (need_grade, need_light) = (!has("lumetri"), !has("lighting_effects") && ask.light_strength > 0.0);
    // What is already on the clip and is about to be written over: said, not done in silence.
    let mut warnings: Vec<String> = Vec::new();
    if let Some(e) = it.effects.iter().find(|e| e.effect == "lumetri") {
        let set = GRADE.iter().zip(NEUTRAL).any(|(id, neutral)| {
            e.params.get(*id).is_some_and(|prm| !prm.keyframes.is_empty() || !matches!(prm.value, ParamValue::Float(v) if (v - neutral).abs() < 1e-9))
        });
        if set || !e.enabled {
            warnings.push(
                "the clip already had Color Grading with Basic Correction set: its white balance, tint, exposure, contrast, saturation, highlights and shadows were replaced (the other sections are untouched). Undo brings the old grade back".into(),
            );
        }
    }
    if it.effects.iter().any(|e| e.effect == "lighting_effects") && ask.light_strength > 0.0 {
        warnings.push("the clip already had Lighting Effects: its first light and ambience were replaced".into());
    }
    let hours = hours_between(from, to);
    if to < from && hours > 0.0 {
        warnings.push(format!("`toTime` is earlier in the day than `time`: the light runs forward {hours:.1} hours through midnight"));
    }
    let apply_effect = crate::commands::find("effects.apply").ok_or_else(|| EngineError::UnknownCommand("effects.apply".into()))?.run;
    let (start, end) = (it.start, it.end());
    let before = crate::story::looks_across(s, start, end);
    let keyframes = s.edit_group("Relight", |s| {
        for (need, id) in [(need_grade, "lumetri"), (need_light, "lighting_effects")] {
            if need {
                apply_effect(s, &json!({"clips": [clip.0], "effect": id}))?;
            }
        }
        s.edit_sequence("Relight", |q, _, _| {
            let (_, it) = q.find_item_mut(clip).ok_or(filmcraft_edit::EditError::NoItem(clip))?;
            Ok(apply(it, size, &ask))
        })
    })?;
    let change = crate::story::change_json(&before, &crate::story::looks_across(s, start, end));
    let g = grade_at(from, ask.strength);
    let (pos, color, _) = sun_at(from);
    let pos = ask.light.unwrap_or(pos);
    Ok(json!({
        "clip": clip.0,
        "time": clock(from),
        "toTime": (hours > 0.0).then(|| clock(to)),
        "hours": (hours * 100.0).round() / 100.0,
        "keyframes": keyframes,
        "grade": GRADE.iter().zip(g).map(|(k, v)| ((*k).to_string(), json!((v * 100.0).round() / 100.0))).collect::<serde_json::Map<_, _>>(),
        "light": {"at": [(pos.x * 1000.0).round() / 1000.0, (pos.y * 1000.0).round() / 1000.0], "color": color, "strength": ask.light_strength},
        "effects": ["lumetri", "lighting_effects"],
        "pictureChange": change,
        "warnings": warnings,
        "note": "a grade toward the light of that hour, made of ordinary Color Grading and Lighting Effects parameters you can go on editing. It moves colour and exposure; it casts no new shadows",
    }))
}
