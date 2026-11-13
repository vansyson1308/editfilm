//! Audio safety checks for agents: where people speak (`audio.speechSegments`), whether a cut
//! lands inside a sentence (warnings on `timeline.razor`, `sequence.addEdit` and `timeline.trim`),
//! and whether the mix is balanced (`mix.check`).
//!
//! Speech detection is [`filmagic_audio_dsp::speech`]: a light, model-free detector (voicing,
//! an adaptive level threshold and syllabic modulation), so none of this needs the transcript
//! model. Loudness is the BS.1770 meter the export and Auto-Match use
//! ([`filmagic_audio_dsp::LoudnessMeter`]).
//!
//! | command | does |
//! |---|---|
//! | `audio.speechSegments` | speech segments (seconds, confidence) of a clip, a project item or a sequence range |
//! | `mix.check` | loudness (integrated, max short-term, true peak, LRA) per track and per Essential Sound role, dialogue measured only where speech is, and the music-to-dialogue difference against a target range |

use std::collections::HashSet;

use serde_json::{Value, json};

use filmagic_audio_dsp::LoudnessMeter;
use filmagic_audio_dsp::speech::{SpeechAnalysis, SpeechDetector, SpeechParams, SpeechSegment};
use filmagic_project::{AudioType, ClipId, Sequence, TrackItem};
use filmagic_render::SourceProvider;
use filmagic_time::Tick;

use crate::commands::{CommandSpec, bad, clip_p, has_seq, item_p, track_p};
use crate::{EngineError, Result, Session};

/// Seconds of audio looked at on each side of a cut.
const CUT_WINDOW_S: f64 = 8.0;
/// A cut this close (seconds) to the edge of a speech segment is at the pause, not inside it.
const CUT_EDGE_S: f64 = 0.03;
/// Analysis chunk (samples at the sequence rate) for streaming renders.
const CHUNK: i64 = 48_000;
/// Default music-to-dialogue target range (dB the music sits under the dialogue).
pub const MUSIC_UNDER_DIALOGUE_DB: (f64, f64) = (15.0, 20.0);
/// Longest range `mix.check` and range `audio.speechSegments` analyse (seconds): six hours.
const MAX_RANGE_S: f64 = 6.0 * 3600.0;

fn segment_json(s: &SpeechSegment, offset: f64) -> Value {
    json!({"start": round3(s.start_s + offset), "end": round3(s.end_s + offset), "confidence": (s.confidence as f64 * 1000.0).round() / 1000.0})
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

fn finite_or_null(v: f64) -> Value {
    if v.is_finite() { json!((v * 100.0).round() / 100.0) } else { Value::Null }
}

/// The detector settings a command asks for (`bridgeSeconds`), validated.
fn params_p(p: &Value, cmd: &str) -> Result<SpeechParams> {
    let mut sp = SpeechParams::default();
    if let Some(v) = p.get("bridgeSeconds").filter(|v| !v.is_null()) {
        let b = v
            .as_f64()
            .filter(|b| b.is_finite() && (0.0..=2.0).contains(b))
            .ok_or_else(|| bad(cmd, "`bridgeSeconds` must be a number of seconds from 0 to 2"))?;
        sp.bridge_s = b;
    }
    Ok(sp)
}

/// A copy of `item` that plays the same source over timeline `[a0, a1)` (ticks), so the audio on
/// both sides of a cut or trim edge can be analysed, including material the clip no longer uses.
/// Reversed, frame-held and nonsensical-speed clips are analysed over their own range only.
fn widened(item: &TrackItem, a0: Tick, a1: Tick) -> TrackItem {
    let mut w = item.clone();
    if item.reverse || item.frame_hold.is_some() || !item.speed.is_finite() || item.speed <= 0.0 || a1 <= a0 {
        return w;
    }
    let mut start = a0;
    // never before the media's start
    let back = Tick(((item.start - a0).0 as f64 * item.speed).round() as i64);
    if item.source_in - back < Tick::ZERO {
        start = item.start - Tick((item.source_in.0 as f64 / item.speed).floor() as i64);
    }
    let shift = Tick(((item.start - start).0 as f64 * item.speed).round() as i64);
    w.start = start;
    w.source_in = (item.source_in - shift).max(Tick::ZERO);
    w.duration = (a1 - start).max(Tick(1));
    w
}

/// Speech in a clip's own signal (clip gain and clip effects) over timeline samples `[a0, a1)` at
/// `sr`, with segment times in sequence seconds. `None` when the clip has no audio.
pub fn clip_speech(item: &TrackItem, a0: i64, a1: i64, sr: u32, sources: &dyn SourceProvider, params: SpeechParams) -> Option<SpeechAnalysis> {
    let mut d = SpeechDetector::new(sr as f64, params);
    let mut pos = a0;
    let mut any = false;
    while pos < a1 {
        let n = (a1 - pos).min(CHUNK) as usize;
        let [l, r] = filmagic_render::audio::clip_signal(item, pos, n, sr, sources)?;
        any = true;
        d.process_planar(&[&l, &r]);
        pos += n as i64;
    }
    if !any {
        return None;
    }
    let mut a = d.finish();
    let off = a0 as f64 / sr as f64;
    for s in &mut a.segments {
        s.start_s += off;
        s.end_s += off;
    }
    Some(a)
}

/// Speech over a clip's whole range (Auto-Match, ducking).
pub fn clip_speech_whole(item: &TrackItem, sr: u32, sources: &dyn SourceProvider) -> Option<SpeechAnalysis> {
    let a0 = item.start.to_units_floor(sr as i64);
    let a1 = item.end().to_units_floor(sr as i64);
    clip_speech(item, a0, a1, sr, sources, SpeechParams::default())
}

/// Integrated loudness (LUFS) of a clip's own signal over its speech segments only, measured with
/// the channels it has (as [`crate::essential_sound::clip_loudness`] does). `None` when silent.
pub fn clip_speech_loudness(item: &TrackItem, segments: &[SpeechSegment], sr: u32, sources: &dyn SourceProvider) -> Option<f64> {
    let mono = filmagic_render::audio::clip_is_mono(item, sources)?;
    let mut meter = LoudnessMeter::new(sr as f64, if mono { 1 } else { 2 });
    for s in segments {
        let a0 = (s.start_s * sr as f64).floor() as i64;
        let a1 = (s.end_s * sr as f64).ceil() as i64;
        let mut pos = a0;
        while pos < a1 {
            let n = (a1 - pos).min(CHUNK) as usize;
            let [l, r] = filmagic_render::audio::clip_signal(item, pos, n, sr, sources)?;
            if mono {
                let m: Vec<f32> = l.iter().zip(&r).map(|(a, b)| 0.5 * (a + b)).collect();
                meter.process(&[&m]);
            } else {
                meter.process(&[&l, &r]);
            }
            pos += n as i64;
        }
    }
    Some(meter.integrated()).filter(|v| v.is_finite())
}

/// Whether a clip's speech is worth checking at a cut: dialogue and untyped clips (music, effects
/// and ambience are not sentences).
fn speaks(item: &TrackItem) -> bool {
    item.essential.as_ref().is_none_or(|e| e.kind == AudioType::Dialogue)
}

/// For each `(clip, cut)` — a clip as it is (before a razor, after a trim) and a timeline time —
/// whether the cut lands inside speech, with the nearest pauses before and after it.
/// Returns (warnings, speechCuts).
pub(crate) fn cut_warnings(s: &Session, cuts: &[(TrackItem, Tick)]) -> (Vec<String>, Vec<Value>) {
    let Some(seq) = s.active_sequence() else { return (Vec::new(), Vec::new()) };
    let sr = seq.settings.sample_rate.max(1);
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let mut warnings = Vec::new();
    let mut found = Vec::new();
    let mut seen: HashSet<(u64, i64)> = HashSet::new();
    for (it, t) in cuts {
        if !speaks(it) || !seen.insert((it.id.0, t.0)) {
            continue;
        }
        let w = Tick::from_seconds_f64(CUT_WINDOW_S);
        let (lo, hi) = (*t - w, *t + w);
        let wide = widened(it, lo.max(Tick::ZERO), hi);
        let a0 = wide.start.to_units_floor(sr as i64);
        let a1 = wide.end().to_units_floor(sr as i64);
        let Some(a) = clip_speech(&wide, a0, a1, sr, &provider, SpeechParams::default()) else { continue };
        let ts = t.seconds();
        let Some(seg) = a.segments.iter().find(|g| g.start_s + CUT_EDGE_S < ts && ts < g.end_s - CUT_EDGE_S) else { continue };
        // the window may have cut the segment short: then the pause lies beyond what was looked at
        let before = (seg.start_s > a0 as f64 / sr as f64 + 0.01).then_some(seg.start_s);
        let after = (seg.end_s < a1 as f64 / sr as f64 - 0.01).then_some(seg.end_s);
        let fmt = |v: Option<f64>| v.map(|x| format!("{x:.2} s")).unwrap_or_else(|| format!("more than {CUT_WINDOW_S:.0} s away"));
        warnings.push(format!(
            "the cut at {ts:.2} s on clip {} (\"{}\") lands inside speech ({:.2}–{:.2} s): the nearest pauses are at {} before and {} after. Undo (edit.undo) and cut at a pause, or check with audio.speechSegments",
            it.id.0,
            it.name,
            seg.start_s,
            seg.end_s,
            fmt(before),
            fmt(after)
        ));
        found.push(json!({
            "clip": it.id.0,
            "seconds": round3(ts),
            "segment": [round3(seg.start_s), round3(seg.end_s)],
            "confidence": (seg.confidence as f64 * 1000.0).round() / 1000.0,
            "pauseBefore": before.map(round3),
            "pauseAfter": after.map(round3),
        }));
    }
    (warnings, found)
}

/// Add the speech warnings of `cuts` to a razor / trim result.
pub(crate) fn annotate_cuts(s: &Session, out: &mut Value, cuts: &[(TrackItem, Tick)]) {
    if cuts.is_empty() {
        return;
    }
    let (warnings, found) = cut_warnings(s, cuts);
    if !found.is_empty() {
        out["warnings"] = json!(warnings);
        out["speechCuts"] = json!(found);
    }
}

/// Audio clips of the active sequence that span `t` (strictly inside), among `only` when given.
pub(crate) fn audio_clips_at(s: &Session, t: Tick, only: Option<&[ClipId]>) -> Vec<TrackItem> {
    let Some(seq) = s.active_sequence() else { return Vec::new() };
    seq.audio_tracks.iter().flat_map(|tr| tr.items.iter()).filter(|i| i.start < t && t < i.end() && only.is_none_or(|o| o.contains(&i.id))).cloned().collect()
}

/// The clips of `before` (audio clips spanning `t` before a razor) that the razor did cut: they
/// now end at `t`. Each is returned as it was before the cut, with the cut time.
pub(crate) fn cut_at(s: &Session, before: Vec<TrackItem>, t: Tick) -> Vec<(TrackItem, Tick)> {
    let Some(seq) = s.active_sequence() else { return Vec::new() };
    before.into_iter().filter(|b| seq.find_item(b.id).is_some_and(|(_, now)| now.end() == t && now.start == b.start)).map(|b| (b, t)).collect()
}

/// The audio clips among `clips` with the trimmed edge (their Out edge when `out`, else In), after
/// a trim.
pub(crate) fn trim_edges(s: &Session, clips: &[ClipId], out: bool) -> Vec<(TrackItem, Tick)> {
    let Some(seq) = s.active_sequence() else { return Vec::new() };
    seq.audio_tracks.iter().flat_map(|t| t.items.iter()).filter(|i| clips.contains(&i.id)).map(|i| (i.clone(), if out { i.end() } else { i.start })).collect()
}

// --------------------------------------------------------------------------- audio.speechSegments

fn sequence_range_p(s: &Session, p: &Value, cmd: &str) -> Result<(f64, f64)> {
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let end_all = seq.duration().seconds();
    let get = |k: &str| -> Result<Option<f64>> {
        match p.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => {
                v.as_f64().filter(|x| x.is_finite() && *x >= 0.0).map(Some).ok_or_else(|| bad(cmd, format!("`{k}` must be a number of seconds, 0 or more")))
            }
        }
    };
    let a = get("startSeconds")?.unwrap_or(0.0);
    let b = get("endSeconds")?.unwrap_or(end_all).min(end_all.max(a));
    if b <= a {
        return Err(bad(cmd, format!("the range {a:.3}–{b:.3} s is empty (the sequence is {end_all:.3} s long): pass `startSeconds` < `endSeconds`")));
    }
    if b - a > MAX_RANGE_S {
        return Err(bad(cmd, format!("the range is {:.0} s; analyse at most {MAX_RANGE_S:.0} s at a time", b - a)));
    }
    Ok((a, b))
}

/// A copy of `seq` that keeps only the audio items `keep` says yes to (video untouched).
fn filtered(seq: &Sequence, keep: impl Fn(usize, &TrackItem) -> bool) -> Sequence {
    let mut q = seq.clone();
    for (k, t) in q.audio_tracks.iter_mut().enumerate() {
        t.items.retain(|i| keep(k, i));
    }
    q
}

/// Stream the mix of `seq` over sequence seconds `[a, b)` into `f` (planar stereo, sequence rate).
fn render_mix(s: &Session, seq: &Sequence, a: f64, b: f64, sources: &dyn SourceProvider, mut f: impl FnMut(&[Vec<f32>])) {
    let sr = seq.settings.sample_rate.max(1) as f64;
    let (a0, a1) = ((a * sr).floor() as i64, (b * sr).ceil() as i64);
    let mut pos = a0;
    while pos < a1 {
        let n = (a1 - pos).min(CHUNK) as usize;
        let buf = filmagic_render::audio::mix_sequence(&s.project, seq, pos, n, sources);
        f(&buf.channels);
        pos += n as i64;
    }
}

fn speech_segments(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "audio.speechSegments";
    let params = params_p(p, cmd)?;
    let min_conf = match p.get("minConfidence") {
        None | Some(Value::Null) => 0.0,
        Some(v) => v.as_f64().filter(|c| c.is_finite() && (0.0..=1.0).contains(c)).ok_or_else(|| bad(cmd, "`minConfidence` must be a number from 0 to 1"))?,
    };
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let mut notes: Vec<String> = Vec::new();
    let (analysis, offset, base, target) = if p.get("clip").is_some_and(|v| !v.is_null()) {
        let c = clip_p(p, "clip").ok_or_else(|| bad(cmd, "`clip` must be a clip id"))?;
        let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
        let (_, it) = seq.find_item(c).ok_or_else(|| bad(cmd, format!("no clip {} in the active sequence; list clips with timeline.inspect", c.0)))?;
        if !seq.audio_tracks.iter().any(|t| t.items.iter().any(|i| i.id == c)) {
            return Err(bad(cmd, format!("clip {} is a video clip; pass its linked audio clip (timeline.inspect lists links)", c.0)));
        }
        let sr = seq.settings.sample_rate.max(1);
        let a = clip_speech(it, it.start.to_units_floor(sr as i64), it.end().to_units_floor(sr as i64), sr, &provider, params)
            .ok_or_else(|| bad(cmd, format!("clip {} has no audio to analyse (a nested sequence or a missing file)", c.0)))?;
        if it.essential.as_ref().is_some_and(|e| e.kind != AudioType::Dialogue) {
            notes.push("this clip's Essential Sound type is not Dialogue: music and effects can read as speech".into());
        }
        (a, 0.0, "sequence", json!({"clip": c.0}))
    } else if p.get("item").is_some_and(|v| !v.is_null()) {
        let id = item_p(p, "item").ok_or_else(|| bad(cmd, "`item` must be a project item id"))?;
        if s.project.item(id).is_none() {
            return Err(bad(cmd, format!("no project item {}; list items with project.inspect", id.0)));
        }
        let src = provider.source(id).ok_or_else(|| bad(cmd, format!("project item {} has no media to read (offline, or a sequence)", id.0)))?;
        let info = src.info();
        if !info.has_audio() {
            return Err(bad(cmd, format!("project item {} has no audio", id.0)));
        }
        let stream = match p.get("stream") {
            None | Some(Value::Null) => 0,
            Some(v) => v
                .as_u64()
                .and_then(|x| usize::try_from(x).ok())
                .filter(|x| *x < info.audio_streams.len())
                .ok_or_else(|| bad(cmd, format!("`stream` must be an audio stream index below {}", info.audio_streams.len())))?,
        };
        let dur = info.duration.seconds();
        if !dur.is_finite() || dur <= 0.0 {
            return Err(bad(cmd, format!("project item {} has no duration", id.0)));
        }
        if dur > MAX_RANGE_S {
            notes.push(format!("only the first {MAX_RANGE_S:.0} s were analysed"));
        }
        const SR: u32 = 16_000;
        let total = (dur.min(MAX_RANGE_S) * SR as f64).ceil() as i64;
        let mut d = SpeechDetector::new(SR as f64, params);
        let mut pos = 0i64;
        while pos < total {
            let n = (total - pos).min(SR as i64) as usize;
            let buf = src.audio_stream(stream, pos, n, SR).map_err(|e| EngineError::Other(format!("{cmd}: reading item {}: {e}", id.0)))?;
            let refs: Vec<&[f32]> = buf.channels.iter().map(Vec::as_slice).collect();
            d.process_planar(&refs);
            pos += n as i64;
        }
        (d.finish(), 0.0, "source", json!({"item": id.0, "stream": stream}))
    } else {
        let (a, b) = sequence_range_p(s, p, cmd)?;
        let tr = track_p(s, p, "track", cmd)?;
        let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
        let sr = seq.settings.sample_rate.max(1);
        let q = match tr {
            Some(t) => {
                let k = seq.audio_tracks.iter().position(|x| x.id == t).ok_or_else(|| bad(cmd, "`track` must be an audio track (A1, A2…)"))?;
                filtered(seq, |i, _| i == k)
            }
            None => seq.clone(),
        };
        let mut d = SpeechDetector::new(sr as f64, params);
        render_mix(s, &q, a, b, &provider, |ch| {
            let refs: Vec<&[f32]> = ch.iter().map(Vec::as_slice).collect();
            d.process_planar(&refs);
        });
        notes.push(match tr {
            Some(_) => "measured on that track as it plays (volume, mute and effects included)".into(),
            None => "measured on the full mix: music under the dialogue can read as speech; pass `track` or a `clip` for one voice".into(),
        });
        (d.finish(), a, "sequence", json!({"startSeconds": a, "endSeconds": b, "track": p.get("track").cloned().unwrap_or(Value::Null)}))
    };
    if analysis.truncated {
        notes.push(format!("analysis stopped after {:.0} s", analysis.duration_s));
    }
    let segs: Vec<&SpeechSegment> = analysis.segments.iter().filter(|g| g.confidence as f64 >= min_conf).collect();
    let speech: f64 = segs.iter().map(|g| g.end_s - g.start_s).sum();
    let pauses: Vec<Value> = segs.windows(2).map(|w| json!([round3(w[0].end_s + offset), round3(w[1].start_s + offset)])).collect();
    Ok(json!({
        "of": target,
        "timeBase": base,
        "segments": segs.iter().map(|g| segment_json(g, offset)).collect::<Vec<_>>(),
        "pauses": pauses,
        "speechSeconds": round3(speech),
        "analysedSeconds": round3(analysis.duration_s),
        "notes": notes,
    }))
}

// ------------------------------------------------------------------------------------ mix.check

/// Loudness of one stem.
#[derive(Default)]
struct Stem {
    integrated: f64,
    max_short_term: f64,
    true_peak: f64,
    lra: f64,
}

impl Stem {
    fn json(&self) -> Value {
        json!({
            "integratedLufs": finite_or_null(self.integrated),
            "maxShortTermLufs": finite_or_null(self.max_short_term),
            "truePeakDbtp": finite_or_null(self.true_peak),
            "lraLu": finite_or_null(self.lra),
        })
    }
}

fn measure(s: &Session, seq: &Sequence, a: f64, b: f64, sources: &dyn SourceProvider) -> Stem {
    let sr = seq.settings.sample_rate.max(1) as f64;
    let mut m = LoudnessMeter::new(sr, 2);
    render_mix(s, seq, a, b, sources, |ch| {
        let refs: Vec<&[f32]> = ch.iter().map(Vec::as_slice).collect();
        m.process(&refs);
    });
    Stem { integrated: m.integrated(), max_short_term: m.max_short_term(), true_peak: m.true_peak_dbtp(), lra: m.loudness_range() }
}

/// Loudness of `seq` over the parts of `[a, b)` inside `segments` (sequence seconds).
fn measure_in(s: &Session, seq: &Sequence, segments: &[(f64, f64)], sources: &dyn SourceProvider) -> Stem {
    let sr = seq.settings.sample_rate.max(1) as f64;
    let mut m = LoudnessMeter::new(sr, 2);
    for (x, y) in segments {
        render_mix(s, seq, *x, *y, sources, |ch| {
            let refs: Vec<&[f32]> = ch.iter().map(Vec::as_slice).collect();
            m.process(&refs);
        });
    }
    Stem { integrated: m.integrated(), max_short_term: m.max_short_term(), true_peak: m.true_peak_dbtp(), lra: m.loudness_range() }
}

fn range_pair_p(p: &Value, cmd: &str) -> Result<(f64, f64)> {
    let Some(v) = p.get("targetDb").filter(|v| !v.is_null()) else { return Ok(MUSIC_UNDER_DIALOGUE_DB) };
    let pair = v.as_array().filter(|a| a.len() == 2).and_then(|a| Some((a[0].as_f64()?, a[1].as_f64()?)));
    match pair {
        Some((lo, hi)) if lo.is_finite() && hi.is_finite() && (0.0..=60.0).contains(&lo) && (0.0..=60.0).contains(&hi) && lo <= hi => Ok((lo, hi)),
        _ => Err(bad(cmd, "`targetDb` must be [min, max]: how many dB the music sits under the dialogue, 0–60, min ≤ max (default [15, 20])")),
    }
}

fn mix_check(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "mix.check";
    let (lo, hi) = range_pair_p(p, cmd)?;
    let (a, b) = sequence_range_p(s, p, cmd)?;
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?.clone();
    let sr = seq.settings.sample_rate.max(1);
    let mut warnings: Vec<String> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    let mix = measure(s, &seq, a, b, &provider);
    // tracks: each track's own items, through the whole mixer (its fader, mute, the master)
    let mut tracks = Vec::new();
    for (k, t) in seq.audio_tracks.iter().enumerate() {
        if t.items.is_empty() {
            continue;
        }
        let st = measure(s, &filtered(&seq, |i, _| i == k), a, b, &provider);
        tracks.push(json!({"track": format!("A{}", k + 1), "id": t.id.0, "name": t.name, "muted": t.muted, "loudness": st.json()}));
    }
    // roles: the clips of each Essential Sound type
    let role_of = |i: &TrackItem| i.essential.as_ref().map(|e| e.kind);
    let in_range = |i: &TrackItem| i.start.seconds() < b && i.end().seconds() > a && i.enabled;
    let mut roles = serde_json::Map::new();
    let mut speech: Vec<(f64, f64)> = Vec::new();
    let mut dialogue_speech = Stem { integrated: f64::NEG_INFINITY, ..Default::default() };
    let mut music_under = Stem { integrated: f64::NEG_INFINITY, ..Default::default() };
    let mut music_all = f64::NEG_INFINITY;
    let present = |t: AudioType| seq.audio_tracks.iter().flat_map(|x| x.items.iter()).any(|i| role_of(i) == Some(t) && in_range(i));
    for t in AudioType::ALL {
        if !present(t) {
            continue;
        }
        let q = filtered(&seq, |_, i| role_of(i) == Some(t));
        let all = measure(s, &q, a, b, &provider);
        let mut v = json!({"loudness": all.json()});
        if t == AudioType::Dialogue {
            let mut d = SpeechDetector::new(sr as f64, SpeechParams::default());
            render_mix(s, &q, a, b, &provider, |ch| {
                let refs: Vec<&[f32]> = ch.iter().map(Vec::as_slice).collect();
                d.process_planar(&refs);
            });
            let an = d.finish();
            speech = an.segments.iter().map(|g| (g.start_s + a, g.end_s + a)).collect();
            let secs: f64 = speech.iter().map(|(x, y)| y - x).sum();
            if speech.is_empty() {
                warnings.push("no speech was detected on the Dialogue clips: check their types (essentialSound.setType) or use audio.speechSegments".into());
            } else {
                dialogue_speech = measure_in(s, &q, &speech, &provider);
            }
            v["speech"] = json!({"seconds": round3(secs), "segments": speech.len(), "loudness": dialogue_speech.json()});
        }
        if t == AudioType::Music {
            music_all = all.integrated;
        }
        roles.insert(t.label().to_string(), v);
    }
    if present(AudioType::Music) && !speech.is_empty() {
        let q = filtered(&seq, |_, i| role_of(i) == Some(AudioType::Music));
        music_under = measure_in(s, &q, &speech, &provider);
        if let Some(m) = roles.get_mut(AudioType::Music.label()) {
            m["underSpeech"] = json!({"loudness": music_under.json()});
        }
    }
    let untyped = seq.audio_tracks.iter().flat_map(|x| x.items.iter()).filter(|i| in_range(i) && i.essential.is_none()).count();
    if untyped > 0 {
        notes.push(format!(
            "{untyped} audio clip(s) have no Essential Sound type and are in no role: set Dialogue / Music / SFX / Ambience with essentialSound.setType for a role-based check"
        ));
    }
    // the music-to-dialogue difference, measured where both play
    let music_ref = if music_under.integrated.is_finite() { music_under.integrated } else { music_all };
    let balance = if dialogue_speech.integrated.is_finite() && music_ref.is_finite() {
        let diff = dialogue_speech.integrated - music_ref;
        let (verdict, change) = if diff < lo {
            ("musicTooLoud", diff - lo)
        } else if diff > hi {
            ("musicTooQuiet", diff - hi)
        } else {
            ("ok", 0.0)
        };
        if verdict != "ok" {
            warnings.push(format!(
                "the music sits {diff:.1} dB under the dialogue (target {lo:.0}–{hi:.0} dB): change the music by {change:+.1} dB (its clip volume or track fader), or duck it with essentialSound.generateDucking {{\"depthDb\": {:.0}}}",
                (lo + hi) / 2.0
            ));
        }
        json!({
            "differenceDb": (diff * 100.0).round() / 100.0,
            "targetDb": [lo, hi],
            "verdict": verdict,
            "musicGainChangeDb": (change * 100.0).round() / 100.0,
            "measuredWhere": if music_under.integrated.is_finite() { "musicUnderSpeech" } else { "wholeRange" },
        })
    } else {
        let why = if !present(AudioType::Dialogue) {
            "no clips typed Dialogue"
        } else if !present(AudioType::Music) {
            "no clips typed Music"
        } else if music_ref.is_finite() {
            "no speech found on the Dialogue clips"
        } else {
            "the Music clips are silent here"
        };
        notes.push(format!("no music-to-dialogue difference: {why} (essentialSound.setType sets types)"));
        json!({"differenceDb": Value::Null, "targetDb": [lo, hi], "verdict": "unknown"})
    };
    if mix.true_peak.is_finite() && mix.true_peak > -1.0 {
        warnings.push(format!(
            "the mix peaks at {:.1} dBTP, above -1 dBTP: the export's true-peak limiter (on by default) will catch it, but consider lowering the loudest track",
            mix.true_peak
        ));
    }
    Ok(json!({
        "range": {"startSeconds": round3(a), "endSeconds": round3(b)},
        "mix": mix.json(),
        "tracks": tracks,
        "roles": roles,
        "musicUnderDialogue": balance,
        "warnings": warnings,
        "notes": notes,
    }))
}

fn spec(id: &'static str, label: &'static str, params: &'static str, run: fn(&mut Session, &Value) -> Result<Value>) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: has_seq, run, journal: false, named: crate::commands::NamedKeys::NONE }
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            enabled: crate::commands::always,
            ..spec(
                "audio.speechSegments",
                "Find Speech Segments",
                r#"{"clip":id?|"item":id?|"startSeconds":f64?,"endSeconds":f64?,"track":"A1"|id?,"stream":u32?,"minConfidence":0..1=0,"bridgeSeconds":0..2=0.25}"#,
                speech_segments,
            )
        },
        spec("mix.check", "Check Mix Loudness", r#"{"startSeconds":f64?,"endSeconds":f64?,"targetDb":[min,max]=[15,20]}"#, mix_check),
    ]
}
