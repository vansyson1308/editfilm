//! Text to Speech (narrations): write what the narrator says, pick a voice, and get an audio clip.
//!
//! | command | does |
//! |---|---|
//! | `tts.voices` | languages, voices, vocal pitches, pace limits and the pause marker |
//! | `tts.preview` | synthesize without changing the project (`sample: true` speaks the voice's sample sentence); the host plays [`Session::tts_preview`]; `path` also writes a WAV |
//! | `tts.create` | synthesize, write `Narration <n>.wav`, import it and place it at the playhead (one undo step) |
//! | `tts.edit` | change a narration clip's script or voice settings and regenerate it in place (one undo step) |
//! | `tts.inspect` | the narration settings of a clip or item |
//! | `tts.render` | synthesize in a background job into the cache, so the UI's create / edit / preview return at once |
//! | `tts.downloadVoices` | download the natural voices (Kokoro-82M and CMUdict, 331 MB shared, plus 0.5 MB per voice; SHA-256 checked) as a background job; `voices` picks which |
//! | `tts.engines` | which voice engines work here: built in, natural, and VieNeu-TTS (Python found, `vieneu` installed, voices known) with the exact install steps |
//! | `tts.setupVieneu` | get VieNeu-TTS ready in a background job: optionally create a Python environment and `pip install vieneu` (`install: true`), then download its model and list its voices |
//!
//! **Vietnamese voices** come from VieNeu-TTS, an optional external engine ([`filmagic_tts::vieneu`]):
//! a Python interpreter with the `vieneu` package runs as a subprocess, like ffmpeg, never linked
//! or bundled. Its voices are `vieneu:<name>` (listed by `tts.voices` once `tts.setupVieneu` has
//! run); `vieneu-clone` with `referenceAudio` speaks in the voice of a reference clip, and only
//! with `confirmAuthorizedVoice: true` (the speaker agreed). Downloads go to the models directory
//! ([`crate::models_store`]). VieNeu gives no word timings: run `transcript.generate` with
//! `language: "vi"` on the narration for captions.
//!
//! **Where a new narration goes.** At `time` (default: the playhead) on `track` if given (it must
//! be free there), else on the first targeted audio track that is free for the narration's length,
//! else the first free audio track, else on a new audio track. Existing clips are never overwritten.
//!
//! **Editing keeps the clip's length** (user decision, 2026-10-07). The clip keeps its start,
//! duration and speed. When the new speech is longer than the clip, the clip still ends where it
//! did but the file holds the whole speech, so trimming the clip's end out reveals the rest. When it
//! is shorter, the file is padded with silence to the clip's length. The clip's source In is reset
//! to the start of the new speech. Other clips (including other clips of the old narration) are not
//! changed. Each edit writes a new file; the old item and its narration stay, so undo is exact.
//!
//! Files are named `Narration <n>.wav` (mono 32-bit float at the voice's rate, 24 kHz) and saved
//! where voice-over recordings go (Scratch Disks ▸ Captured, else next to the project, else
//! `Narrations` in the data or temporary directory); an edit writes next to the previous file.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Value, json};

use filmagic_project::{ClipId, ItemId, ItemKind, MediaRef, Narration, TrackId, TrackKind, VocalPitch};
use filmagic_time::{Tick, TimeRange};
use filmagic_tts as tts;

use crate::commands::{CommandSpec, always, bad, has_seq, str_p, time_p, u64_p};
use crate::{EngineError, Result, Session};

/// Item name prefix of generated narrations.
const BASE_NAME: &str = "Narration";

/// The narration of a project item.
pub fn narration_of(s: &Session, item: ItemId) -> Option<&Narration> {
    s.project.narrations.get(&item)
}

/// The narration clip a command works on: `clip`, else the first selected clip whose item is a
/// narration.
pub fn target_clip(s: &Session, p: &Value) -> Option<ClipId> {
    let seq = s.active_sequence()?;
    let is_narration = |c: ClipId| seq.find_item(c).is_some_and(|(_, it)| s.project.narrations.contains_key(&it.item));
    match u64_p(p, "clip") {
        Some(c) => Some(ClipId(c)).filter(|c| is_narration(*c)),
        None => s.state.selection.iter().copied().find(|c| is_narration(*c)),
    }
}

fn has_narration_clip(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    target_clip(s, &Value::Null).map(|_| ()).ok_or_else(|| "select a narration clip".into())
}

/// The VieNeu preset a voice id names (`vieneu:<name>`).
fn vieneu_name(voice: &str) -> Option<&str> {
    voice.strip_prefix(tts::vieneu::VOICE_PREFIX)
}

/// A VieNeu voice: a preset or the cloned voice.
fn is_vieneu(voice: &str) -> bool {
    vieneu_name(voice).is_some() || voice == tts::vieneu::CLONE_VOICE
}

/// The sentence "Hear this voice" speaks: Vietnamese for the VieNeu voices.
fn sample_sentence(voice: &str) -> &'static str {
    if is_vieneu(voice) { tts::vieneu::SAMPLE_SENTENCE } else { tts::SAMPLE_SENTENCE }
}

/// What synthesis needs from the session, captured so background jobs run without it.
#[derive(Clone, Debug)]
struct VoiceEnv {
    models_dir: Option<PathBuf>,
    python: String,
}

impl VoiceEnv {
    fn of(s: &Session) -> Self {
        VoiceEnv { models_dir: s.models_dir(), python: s.prefs.media_analysis.vieneu_python.clone() }
    }
}

/// The voice (and for `vieneu-clone`, its reference clip) from params, checked; its language.
fn voice_setting(s: &Session, cmd: &str, p: &Value, voice: &str, base: Option<&Narration>) -> Result<(String, Option<String>)> {
    let reference_p = match p.get("referenceAudio") {
        None | Some(Value::Null) => None,
        Some(Value::String(r)) => Some(r.as_str()),
        Some(_) => return Err(bad(cmd, "`referenceAudio` must be a path to an audio file")),
    };
    if reference_p.is_some() && voice != tts::vieneu::CLONE_VOICE {
        return Err(bad(cmd, format!("`referenceAudio` only works with the voice `{}`", tts::vieneu::CLONE_VOICE)));
    }
    if voice == tts::vieneu::CLONE_VOICE {
        let reference = match reference_p {
            Some(r) => {
                // cloning someone's voice needs their consent, every time a new clip is given
                if p.get("confirmAuthorizedVoice").and_then(Value::as_bool) != Some(true) {
                    return Err(bad(
                        cmd,
                        "cloning a voice needs the speaker's consent: pass \"confirmAuthorizedVoice\": true only after the person whose voice is in the clip agreed",
                    ));
                }
                tts::vieneu::check_reference(Path::new(r)).map_err(|e| bad(cmd, e.to_string()))?;
                r.to_string()
            }
            None => base.filter(|b| b.voice == voice).and_then(|b| b.reference_audio.clone()).ok_or_else(|| {
                bad(cmd, "the voice `vieneu-clone` needs `referenceAudio` (a 3 to 30 second clip of the speaker) and \"confirmAuthorizedVoice\": true")
            })?,
        };
        return Ok((tts::vieneu::LANGUAGE.to_string(), Some(reference)));
    }
    if let Some(name) = vieneu_name(voice) {
        if name.trim().is_empty() || name.chars().count() > 120 || name.chars().any(char::is_control) {
            return Err(bad(cmd, format!("`{voice}` is not a VieNeu voice name (tts.voices lists them)")));
        }
        // the voice list is known once tts.setupVieneu has run; before that VieNeu itself checks
        if let Some(list) = s.models_dir().as_deref().and_then(tts::vieneu::cached_voices)
            && !list.iter().any(|v| v.name == name)
        {
            let ids: Vec<String> = list.iter().map(|v| v.id()).collect();
            return Err(bad(cmd, format!("unknown voice `{voice}`; VieNeu voices: {}", ids.join(", "))));
        }
        return Ok((tts::vieneu::LANGUAGE.to_string(), None));
    }
    let info = tts::voices_in(None).into_iter().find(|v| v.id == voice).ok_or_else(|| {
        bad(cmd, format!("unknown voice `{voice}` (tts.voices lists them; Vietnamese voices are `vieneu:<name>` once tts.setupVieneu has run)"))
    })?;
    Ok((info.language.to_string(), None))
}

/// Script and settings from params, on top of `base`.
fn settings(s: &Session, cmd: &str, p: &Value, base: Option<&Narration>) -> Result<Narration> {
    // a setting of the wrong type is an error, never silently replaced by a default
    for key in ["text", "voice", "pitch"] {
        if p.get(key).is_some_and(|v| !v.is_string()) {
            return Err(bad(cmd, format!("`{key}` must be a string")));
        }
    }
    let text = match str_p(p, "text") {
        Some(t) => t.to_string(),
        None => base.map(|b| b.text.clone()).ok_or_else(|| bad(cmd, "`text` is required"))?,
    };
    let voice = str_p(p, "voice").map(str::to_string).or_else(|| base.map(|b| b.voice.clone())).unwrap_or_else(|| tts::default_voice_id().to_string());
    let (language, reference_audio) = voice_setting(s, cmd, p, &voice, base)?;
    let pitch = match str_p(p, "pitch") {
        Some(v) => VocalPitch::from_id(v).ok_or_else(|| bad(cmd, format!("unknown pitch `{v}` (extraLow, low, default, high, extraHigh)")))?,
        None => base.map(|b| b.pitch).unwrap_or_default(),
    };
    let pace = match p.get("pace") {
        Some(v) => v.as_f64().ok_or_else(|| bad(cmd, "`pace` must be a number"))?,
        None => base.map_or(1.0, |b| b.pace),
    };
    let n = Narration { text, language, voice, pitch, pace, speech_duration: Tick::ZERO, reference_audio };
    n.check().map_err(|e| bad(cmd, e))?;
    Ok(n)
}

/// Synthesized narrations by settings, newest last (`tts.render` fills it in the background so
/// the UI's create / edit / preview don't block).
pub type SynthCache = Arc<Mutex<Vec<(String, Arc<tts::Audio>)>>>;
const CACHE_ENTRIES: usize = 6;

fn cache_key(n: &Narration) -> String {
    format!("{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}", n.voice, n.pitch.id(), n.pace.to_bits(), n.reference_audio.as_deref().unwrap_or_default(), n.text)
}

/// The error message an agent can act on.
fn tts_message(e: tts::TtsError) -> String {
    match e {
        tts::TtsError::NotInstalled => "the natural voices are not downloaded yet: Text to Speech > Download natural voices (tts.downloadVoices)".into(),
        tts::TtsError::VoiceNotInstalled(id) => format!("the natural voice `{id}` is not downloaded yet: run tts.downloadVoices {{\"voices\": [\"{id}\"]}}"),
        tts::TtsError::EngineMissing(e) => {
            format!("VieNeu-TTS is not installed ({e}). tts.engines lists the install steps; tts.setupVieneu {{\"install\": true}} does them")
        }
        e @ tts::TtsError::Engine(_) => format!("{e}. tts.engines shows which Python is used; tts.setupVieneu re-checks the setup"),
        e => e.to_string(),
    }
}

fn tts_err(cmd: &str, e: tts::TtsError) -> EngineError {
    bad(cmd, tts_message(e))
}

/// Synthesize `n` (no cache). `cancel` stops an external engine.
fn render(n: &Narration, env: &VoiceEnv, cancel: Option<&std::sync::atomic::AtomicBool>) -> std::result::Result<tts::Audio, tts::TtsError> {
    let params = tts::Params { semitones: n.pitch.semitones(), pace: n.pace };
    if is_vieneu(&n.voice) {
        let dir = env.models_dir.as_deref().ok_or_else(|| tts::TtsError::Engine("there is no data directory for models".into()))?;
        let st = tts::vieneu::detect(Some(&env.python), dir);
        let Some(ready) = st.ready else {
            let tried: Vec<String> = st.tried.iter().map(|(p, why)| format!("{p}: {why}")).collect();
            return Err(tts::TtsError::EngineMissing(if tried.is_empty() { "no Python found".into() } else { tried.join("; ") }));
        };
        let req = tts::vieneu::Request { text: &n.text, voice: vieneu_name(&n.voice), reference: n.reference_audio.as_deref().map(Path::new), params };
        return tts::vieneu::synthesize(&ready.python, dir, &req, cancel);
    }
    let voice = tts::voice_in(&n.voice, env.models_dir.as_deref())?;
    voice.synthesize(&n.text, &params)
}

fn synthesize(s: &Session, cmd: &str, n: &Narration) -> Result<Arc<tts::Audio>> {
    let key = cache_key(n);
    if let Some(a) = s.tts_cache.lock().unwrap_or_else(PoisonError::into_inner).iter().find(|(k, _)| *k == key).map(|(_, a)| a.clone()) {
        return Ok(a);
    }
    let a = Arc::new(render(n, &VoiceEnv::of(s), None).map_err(|e| tts_err(cmd, e))?);
    cache_put(&s.tts_cache, key, a.clone());
    Ok(a)
}

fn cache_put(c: &SynthCache, key: String, a: Arc<tts::Audio>) {
    let mut c = c.lock().unwrap_or_else(PoisonError::into_inner);
    c.retain(|(k, _)| *k != key);
    c.push((key, a));
    let extra = c.len().saturating_sub(CACHE_ENTRIES);
    c.drain(..extra);
}

/// Whether this build can download and run the natural voices (Kokoro-82M). Official packages
/// enable the `neural-voices` feature of the desktop app; a plain `cargo build` does not (#2318).
pub fn natural_voices_available() -> bool {
    cfg!(feature = "neural-voices")
}

fn package_json(dir: Option<&Path>) -> Value {
    let shared: u64 = tts::catalog::FILES.iter().filter(|f| tts::catalog::is_shared(f)).map(|f| f.size).sum();
    json!({
        "id": tts::catalog::PACKAGE_ID,
        "name": tts::catalog::PACKAGE_NAME,
        "installed": dir.is_some_and(tts::catalog::installed),
        "anyInstalled": dir.is_some_and(tts::catalog::any_installed),
        "size": tts::catalog::size(),
        "sharedBytes": shared,
        "missingBytes": dir.map_or(tts::catalog::size(), tts::catalog::missing_bytes),
        "perVoice": tts::catalog::VOICES.iter().map(|v| json!({
            "voice": v.info.id,
            "installed": dir.is_some_and(|d| tts::catalog::voice_installed(d, v)),
            "missingBytes": dir.map_or(tts::catalog::size(), |d| tts::catalog::missing_bytes_for(d, &[v])),
        })).collect::<Vec<_>>(),
        "license": tts::catalog::LICENSE,
        "source": tts::catalog::SOURCE,
        "available": natural_voices_available(),
    })
}

/// VieNeu-TTS as `tts.voices` reports it, from the voice cache (no Python is started).
fn vieneu_json(dir: Option<&Path>) -> (Value, Vec<Value>) {
    let cached = dir.and_then(tts::vieneu::cached_voices).unwrap_or_default();
    let mut list: Vec<Value> = cached.iter().map(tts::vieneu::VieneuVoice::info_json).collect();
    if !cached.is_empty() {
        list.push(json!({
            "id": tts::vieneu::CLONE_VOICE,
            "name": "Cloned voice (reference clip)",
            "language": tts::vieneu::LANGUAGE,
            "gender": null,
            "engine": "vieneu",
            "description": "Speaks in the voice of `referenceAudio` (3 to 30 s of one speaker); needs \"confirmAuthorizedVoice\": true.",
            "license": tts::vieneu::LICENSE,
            "installed": true,
        }));
    }
    let info = json!({
        "ready": !cached.is_empty(),
        "voices": cached.len(),
        "license": tts::vieneu::LICENSE,
        "source": tts::vieneu::SOURCE,
        "wordTimings": false,
        "setup": if cached.is_empty() { "Vietnamese voices need VieNeu-TTS: tts.engines shows whether it is installed and how; tts.setupVieneu gets it ready" } else { "ready" },
    });
    (info, list)
}

fn voices(s: &mut Session, _p: &Value) -> Result<Value> {
    let dir = s.models_dir();
    let mut list: Vec<Value> = tts::voices_in(dir.as_deref()).iter().map(|v| json!(v)).collect();
    let (vieneu, vlist) = vieneu_json(dir.as_deref());
    list.extend(vlist);
    let mut warnings = crate::models_store::elsewhere_warnings(s);
    if vieneu["ready"] != json!(true) {
        warnings.push("no Vietnamese voices yet: they need VieNeu-TTS (tts.engines, then tts.setupVieneu)".into());
    }
    Ok(json!({
        "package": package_json(dir.as_deref()),
        "vieneu": vieneu,
        "modelsDir": dir.as_ref().map(|d| d.to_string_lossy().into_owned()),
        "warnings": warnings,
        "languages": tts::LANGUAGES.iter().map(|(id, name)| json!({"id": id, "name": name})).collect::<Vec<_>>(),
        "voices": list,
        "defaultVoice": tts::default_voice_id(),
        "pitches": VocalPitch::ALL.iter().map(|p| json!({"id": p.id(), "label": p.label(), "semitones": p.semitones()})).collect::<Vec<_>>(),
        "pace": {"min": tts::MIN_PACE, "max": tts::MAX_PACE, "default": 1.0},
        "pauseMarker": tts::script::PAUSE_MARKER,
        "maxTextBytes": tts::MAX_TEXT_BYTES,
        "sampleSentence": tts::SAMPLE_SENTENCE,
    }))
}

fn preview(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "tts.preview";
    let base = target_clip(s, p).and_then(|c| s.active_sequence()?.find_item(c).map(|(_, it)| it.item)).and_then(|i| s.project.narrations.get(&i).cloned());
    let mut q = p.clone();
    if q.get("sample").and_then(Value::as_bool).unwrap_or(false) {
        q["text"] = json!(sample_sentence(str_p(&q, "voice").or(base.as_ref().map(|b| b.voice.as_str())).unwrap_or_default()));
    }
    let n = settings(s, cmd, &q, base.as_ref())?;
    let audio = synthesize(s, cmd, &n)?;
    let seconds = audio.seconds();
    let samples = audio.samples.len();
    let rate = audio.sample_rate;
    let mut path = Value::Null;
    if let Some(dest) = str_p(p, "path") {
        let bytes = crate::voiceover::write_wav_f32(&audio.samples, rate);
        s.services.write_file(dest, &bytes).map_err(|e| EngineError::Other(format!("{dest}: {e}")))?;
        path = json!(dest);
    }
    s.tts_preview = Some(audio);
    Ok(json!({"seconds": seconds, "samples": samples, "sampleRate": rate, "voice": n.voice, "path": path}))
}

/// Write `samples` as a new WAV and import it (its own undo step; callers collapse history).
/// `near`: an edit writes next to the narration's previous file unless `dir` is given.
fn write_and_import(s: &mut Session, p: &Value, samples: &[f32], rate: u32, near: Option<ItemId>) -> Result<(ItemId, String)> {
    let beside = near.filter(|_| p.get("dir").is_none()).and_then(|i| match &s.project.items.get(&i)?.kind {
        ItemKind::Media(m) => match &m.media {
            MediaRef::File { path } => std::path::Path::new(path).parent().filter(|d| !d.as_os_str().is_empty()).map(|d| d.to_string_lossy().into_owned()),
            MediaRef::Generator(_) => None,
        },
        _ => None,
    });
    let dir = beside.unwrap_or_else(|| crate::voiceover::media_dir(s, p, "Narrations"));
    let path = crate::voiceover::unique_wav_path(s, &dir, BASE_NAME)?;
    if !cfg!(target_arch = "wasm32") {
        std::fs::create_dir_all(&dir).map_err(|e| EngineError::Other(format!("{dir}: {e}")))?;
    }
    let bytes = crate::voiceover::write_wav_f32(samples, rate);
    s.services.write_file(&path, &bytes).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    let item = crate::commands::import_bytes(s, &path, bytes.into(), None)?;
    Ok((item, path))
}

fn track_is_free(s: &Session, track: TrackId, range: TimeRange) -> bool {
    s.active_sequence()
        .and_then(|q| q.audio_tracks.iter().find(|t| t.id == track))
        .is_some_and(|t| !t.locked && !t.items.iter().any(|it| it.start < range.end() && range.start < it.end()))
}

/// The track a new narration of `range` goes to (None: add a track).
fn placement(s: &Session, p: &Value, range: TimeRange) -> Result<Option<TrackId>> {
    let cmd = "tts.create";
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    if let Some(v) = p.get("track") {
        let id =
            crate::mixer::strip_ref(seq, v).filter(|id| seq.audio_tracks.iter().any(|t| t.id == *id)).ok_or_else(|| bad(cmd, format!("no audio track {v}")))?;
        if !track_is_free(s, id, range) {
            return Err(bad(cmd, format!("audio track {v} is locked or has clips there")));
        }
        return Ok(Some(id));
    }
    let targeted = s.targeting().targeted;
    let order = seq.audio_tracks.iter().filter(|t| targeted.contains(&t.id)).chain(seq.audio_tracks.iter().filter(|t| !targeted.contains(&t.id)));
    Ok(order.map(|t| t.id).find(|id| track_is_free(s, *id, range)))
}

fn create(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "tts.create";
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let mut n = settings(s, cmd, p, None)?;
    let at = time_p(s, p, "").unwrap_or(s.playhead());
    if at < Tick::ZERO {
        return Err(bad(cmd, "the start time cannot be negative"));
    }
    let audio = synthesize(s, cmd, &n)?;
    let rate = audio.sample_rate.max(1);
    let dur = Tick::from_units(audio.samples.len() as i64, i64::from(rate));
    n.speech_duration = dur;
    let range = TimeRange::new(at, dur);
    let track = placement(s, p, range)?;
    let n0 = s.history.undo.len();
    let result = (|| -> Result<Value> {
        let track = match track {
            Some(t) => t,
            None => {
                let before: Vec<TrackId> = s.active_sequence().map(|q| q.audio_tracks.iter().map(|t| t.id).collect()).unwrap_or_default();
                s.execute("sequence.addTracks", json!({"video": 0, "audio": 1, "audioAfter": "last"}))?;
                s.active_sequence()
                    .and_then(|q| q.audio_tracks.iter().map(|t| t.id).find(|id| !before.contains(id)))
                    .ok_or_else(|| EngineError::Other("could not add an audio track".into()))?
            }
        };
        let (item, path) = write_and_import(s, p, &audio.samples, rate, None)?;
        let seq_rate = s.project.sequence(seq_id).map(|q| q.settings.frame_rate).unwrap_or_default();
        let narration = n.clone();
        let clip = s.edit("New Narration", |pr, st| {
            let mut it = pr
                .make_track_item(item, TrackKind::Audio, at, TimeRange::new(Tick::ZERO, dur), seq_rate)
                .ok_or(EngineError::Other("the narration is not importable".into()))?;
            it.duration = dur;
            let id = it.id;
            let seq = pr.sequence_mut(seq_id).ok_or(EngineError::NoSequence)?;
            let t = seq.audio_tracks.iter_mut().find(|t| t.id == track).ok_or_else(|| bad(cmd, "the narration track was deleted"))?;
            let pos = t.items.partition_point(|x| x.start <= at);
            t.items.insert(pos, it);
            seq.check().map_err(EngineError::Other)?;
            pr.narrations.insert(item, narration);
            st.selection = vec![id];
            Ok(id)
        })?;
        let seq = s.project.sequence(seq_id).ok_or(EngineError::NoSequence)?;
        Ok(json!({
            "item": item.0,
            "clip": clip.0,
            "track": crate::mixer::strip_label(seq, track),
            "start": at.0,
            "duration": dur.0,
            "speechDuration": dur.0,
            "path": path,
            "sampleRate": rate,
        }))
    })();
    crate::clip_ops::collapse_history(s, n0, "New Narration");
    if result.is_err() && s.history.undo.len() > n0 {
        // a step failed after the track was added or the file imported: roll back to before
        let _ = s.undo();
        s.history.redo.clear();
    }
    result
}

/// Samples of media a clip of `duration` at `speed` plays, rounded up (exact for 100 %).
fn needed_samples(duration: Tick, speed: f64, rate: u32) -> usize {
    let rate = i64::from(rate.max(1));
    let media = if speed.is_finite() && speed != 0.0 && (speed.abs() - 1.0).abs() > 1e-12 {
        // speed is a ratio, not edit time: scale the exact tick count
        Tick::from_seconds_f64(duration.seconds() * speed.abs())
    } else {
        duration
    };
    let mut n = media.to_units_floor(rate);
    if Tick::from_units(n, rate) < media {
        n = n.saturating_add(1);
    }
    let cap = (tts::MAX_SECONDS * 2.0) as i64 * rate;
    n.clamp(0, cap) as usize
}

fn edit(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "tts.edit";
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let clip = target_clip(s, p).ok_or_else(|| bad(cmd, "select a narration clip (or pass `clip`)"))?;
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (track, it) = seq.find_item(clip).ok_or_else(|| bad(cmd, "no such clip"))?;
    if seq.audio_tracks.iter().any(|t| t.id == track && t.locked) {
        return Err(bad(cmd, "the clip's track is locked"));
    }
    let (old_item, clip_dur, speed) = (it.item, it.duration, it.speed);
    let base = s.project.narrations.get(&old_item).cloned().ok_or_else(|| bad(cmd, "the clip is not a narration"))?;
    let mut n = settings(s, cmd, p, Some(&base))?;
    let audio = synthesize(s, cmd, &n)?;
    let rate = audio.sample_rate.max(1);
    let speech = audio.samples.len();
    n.speech_duration = Tick::from_units(speech as i64, i64::from(rate));
    // the clip keeps its length: pad the file with silence to what the clip plays
    let needed = needed_samples(clip_dur, speed, rate);
    let mut samples = audio.samples.clone();
    if needed > samples.len() {
        samples.try_reserve(needed - samples.len()).map_err(|e| bad(cmd, format!("unable to allocate the narration: {e}")))?;
        samples.resize(needed, 0.0);
    }
    let file_len = samples.len();
    let n0 = s.history.undo.len();
    let result = (|| -> Result<Value> {
        let (item, path) = write_and_import(s, p, &samples, rate, Some(old_item))?;
        let name = s.project.items.get(&item).map(|i| i.name.clone()).unwrap_or_default();
        let narration = n.clone();
        s.edit("Edit Narration", |pr, _| {
            let seq = pr.sequence_mut(seq_id).ok_or(EngineError::NoSequence)?;
            let (_, it) = seq.find_item_mut(clip).ok_or_else(|| bad(cmd, "the clip was deleted"))?;
            it.item = item;
            it.name = name;
            it.source_in = Tick::ZERO;
            seq.check().map_err(EngineError::Other)?;
            pr.narrations.insert(item, narration);
            Ok(())
        })?;
        Ok(json!({
            "clip": clip.0,
            "item": item.0,
            "previousItem": old_item.0,
            "duration": clip_dur.0,
            "speechDuration": n.speech_duration.0,
            "fileDuration": Tick::from_units(file_len as i64, i64::from(rate)).0,
            "path": path,
            "sampleRate": rate,
        }))
    })();
    crate::clip_ops::collapse_history(s, n0, "Edit Narration");
    if result.is_err() && s.history.undo.len() > n0 {
        let _ = s.undo();
        s.history.redo.clear();
    }
    result
}

fn inspect(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "tts.inspect";
    let (item, clip) = match u64_p(p, "item") {
        Some(i) => (ItemId(i), None),
        None => {
            let c = target_clip(s, p).ok_or_else(|| bad(cmd, "pass `item` or `clip`, or select a narration clip"))?;
            let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
            (seq.find_item(c).map(|(_, it)| it.item).ok_or_else(|| bad(cmd, "no such clip"))?, Some(c.0))
        }
    };
    let n = s.project.narrations.get(&item).ok_or_else(|| bad(cmd, "not a narration"))?;
    Ok(json!({"item": item.0, "clip": clip, "narration": n}))
}

fn new_job(s: &mut Session, label: String) -> crate::Job {
    let id = s.jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1;
    crate::Job { id, label, progress: Default::default(), result: Default::default() }
}

fn spawn(s: &mut Session, job: crate::Job, wait: bool, name: &str, run: impl FnOnce() + Send + 'static) -> Result<u64> {
    let id = job.id;
    let run = crate::export_tools::guard_job(job.progress.clone(), job.result.clone(), run);
    s.jobs.push(job);
    if wait || cfg!(target_arch = "wasm32") {
        run();
    } else {
        std::thread::Builder::new().name(name.into()).spawn(run).map_err(|e| EngineError::Other(e.to_string()))?;
    }
    Ok(id)
}

type JobResult = std::sync::Mutex<Option<std::result::Result<filmagic_export::Report, String>>>;

fn finish(prog: &filmagic_export::Progress, res: &JobResult, r: std::result::Result<String, String>, t0: web_time::Instant) {
    use std::sync::atomic::Ordering;
    let secs = t0.elapsed().as_secs_f64();
    let r = match r {
        Ok(status) => {
            *prog.status.lock().unwrap_or_else(PoisonError::into_inner) = status;
            Ok(filmagic_export::Report { path: String::new(), frames: 0, seconds: secs, bytes: 0, render_fps: 0.0, extra_files: Vec::new() })
        }
        Err(e) => {
            *prog.status.lock().unwrap_or_else(PoisonError::into_inner) = e.clone();
            *prog.error.lock().unwrap_or_else(PoisonError::into_inner) = Some(e.clone());
            Err(e)
        }
    };
    prog.finished.store(true, Ordering::Relaxed);
    *res.lock().unwrap_or_else(PoisonError::into_inner) = Some(r);
}

/// Synthesize in the background into the cache, so `tts.create` / `tts.edit` / `tts.preview` with
/// the same settings return at once.
fn render_job(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "tts.render";
    let base = target_clip(s, p).and_then(|c| s.active_sequence()?.find_item(c).map(|(_, it)| it.item)).and_then(|i| s.project.narrations.get(&i).cloned());
    let mut q = p.clone();
    if q.get("sample").and_then(Value::as_bool).unwrap_or(false) {
        q["text"] = json!(sample_sentence(str_p(&q, "voice").or(base.as_ref().map(|b| b.voice.as_str())).unwrap_or_default()));
    }
    let n = settings(s, cmd, &q, base.as_ref())?;
    let key = cache_key(&n);
    if s.tts_cache.lock().unwrap_or_else(PoisonError::into_inner).iter().any(|(k, _)| *k == key) {
        return Ok(json!({"job": null, "cached": true}));
    }
    let job = new_job(s, "Synthesizing narration".into());
    let (prog, res, cache) = (job.progress.clone(), job.result.clone(), s.tts_cache.clone());
    let wait = p.get("wait").and_then(Value::as_bool).unwrap_or(false);
    let env = VoiceEnv::of(s);
    let id = spawn(s, job, wait, "filmagic-tts", move || {
        let t0 = web_time::Instant::now();
        *prog.status.lock().unwrap_or_else(PoisonError::into_inner) = "Synthesizing…".into();
        let r = match render(&n, &env, Some(&prog.cancel)) {
            Ok(a) => {
                let secs = a.seconds();
                cache_put(&cache, key, Arc::new(a));
                Ok(format!("{secs:.1} s of speech"))
            }
            Err(e) => Err(tts_message(e)),
        };
        finish(&prog, &res, r, t0);
    })?;
    Ok(json!({"job": id, "cached": false}))
}

/// The natural voices `voices` names (all of them when absent).
fn natural_voices_p(cmd: &str, p: &Value) -> Result<Vec<&'static tts::catalog::NeuralVoice>> {
    match p.get("voices") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(a)) => {
            let mut out: Vec<&'static tts::catalog::NeuralVoice> = Vec::new();
            for v in a {
                let id = v.as_str().ok_or_else(|| bad(cmd, "`voices` must be a list of natural voice ids (\"kokoro-heart\", …)"))?;
                let voice = tts::catalog::find(id).ok_or_else(|| {
                    let ids: Vec<&str> = tts::catalog::VOICES.iter().map(|v| v.info.id).collect();
                    bad(cmd, format!("`{id}` is not a natural voice; natural voices: {}", ids.join(", ")))
                })?;
                if !out.iter().any(|o| o.info.id == voice.info.id) {
                    out.push(voice);
                }
            }
            if out.is_empty() {
                return Err(bad(cmd, "`voices` is empty: name at least one voice, or leave it out for all of them"));
            }
            Ok(out)
        }
        Some(_) => Err(bad(cmd, "`voices` must be a list of natural voice ids (\"kokoro-heart\", …)")),
    }
}

/// Download the natural voices (Kokoro-82M and CMUdict, shared, and each voice's own file) in
/// the background: `voices` picks which, all of them by default.
fn download_voices(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "tts.downloadVoices";
    let voices = natural_voices_p(cmd, p)?;
    let dir = s.models_dir().ok_or_else(|| bad(cmd, "no data directory for voice models"))?;
    let missing = tts::catalog::missing_bytes_for(&dir, &voices);
    let ids: Vec<&str> = if voices.is_empty() { tts::catalog::VOICES.iter().map(|v| v.info.id).collect() } else { voices.iter().map(|v| v.info.id).collect() };
    if missing == 0 {
        return Ok(json!({"job": null, "installed": true, "voices": ids, "dir": tts::catalog::package_dir(&dir).to_string_lossy()}));
    }
    if !cfg!(feature = "neural-voices") {
        return Err(bad(cmd, "natural voices are not available in this build (built without the `neural-voices` feature)"));
    }
    let wait = p.get("wait").and_then(Value::as_bool).unwrap_or(false);
    let job = new_job(s, format!("Downloading {}", tts::catalog::PACKAGE_NAME));
    job.progress.total.store(missing, std::sync::atomic::Ordering::Relaxed);
    let (prog, res) = (job.progress.clone(), job.result.clone());
    let files: Vec<&'static tts::catalog::ModelFile> = tts::catalog::files_for(&voices);
    let target = tts::catalog::package_dir(&dir);
    let id = spawn(s, job, wait, "filmagic-tts-download", move || {
        let t0 = web_time::Instant::now();
        let r = download_package(&dir, &files, &prog);
        finish(&prog, &res, r.map(|()| "Natural voices installed".to_string()), t0);
    })?;
    Ok(json!({"job": id, "installed": false, "voices": ids, "bytes": missing, "dir": target.to_string_lossy()}))
}

#[cfg(feature = "neural-voices")]
fn download_package(dir: &std::path::Path, files: &[&tts::catalog::ModelFile], prog: &filmagic_export::Progress) -> std::result::Result<(), String> {
    use std::sync::atomic::Ordering;
    let files: Vec<filmagic_speech::models::ModelFile> =
        files.iter().map(|f| filmagic_speech::models::ModelFile { name: f.name, url: f.url, sha256: f.sha256, size: f.size }).collect();
    let mut progress = |done: u64, total: u64, file: &str| {
        prog.done.store(done, Ordering::Relaxed);
        prog.total.store(total, Ordering::Relaxed);
        *prog.status.lock().unwrap_or_else(PoisonError::into_inner) = format!("Downloading {file}");
        !prog.cancel.load(Ordering::Relaxed)
    };
    filmagic_speech::models::download_files(&tts::catalog::package_dir(dir), &files, &mut progress).map_err(|e| e.to_string())
}

#[cfg(not(feature = "neural-voices"))]
fn download_package(_: &std::path::Path, _: &[&tts::catalog::ModelFile], _: &filmagic_export::Progress) -> std::result::Result<(), String> {
    Err("natural voices are not available in this build".into())
}

fn probe_json(p: &tts::vieneu::Probe) -> Value {
    json!({"command": p.python.display(), "source": p.python.source, "pythonVersion": p.python_version, "vieneu": p.vieneu})
}

/// Which voice engines work here, and how to install VieNeu-TTS when it does not. Starts Python a
/// few times (to look), so it takes a second or two.
fn engines(s: &mut Session, _: &Value) -> Result<Value> {
    let cmd = "tts.engines";
    let dir = s.models_dir().ok_or_else(|| bad(cmd, "no data directory for models"))?;
    let st = tts::vieneu::detect(Some(&s.prefs.media_analysis.vieneu_python), &dir);
    let cached = tts::vieneu::cached_voices(&dir).map_or(0, |v| v.len());
    let next = match (&st.ready, cached) {
        (Some(_), 0) => "run tts.setupVieneu to download the VieNeu model (about 0.6 GB) and list its voices",
        (Some(_), _) => "ready: pick a `vieneu:<name>` voice from tts.voices",
        (None, _) if st.python.is_some() => {
            "run tts.setupVieneu {\"install\": true} to create a Python environment in the models directory and pip install vieneu there, or follow installSteps"
        }
        (None, _) => "install Python 3.10 or newer (installSteps), then run tts.setupVieneu {\"install\": true}",
    };
    Ok(json!({
        "modelsDir": dir.to_string_lossy(),
        "builtin": {"available": true, "voices": tts::voices().len()},
        "natural": package_json(Some(&dir)),
        "vieneu": {
            "available": st.available(),
            "ready": st.available() && cached > 0,
            "python": st.ready.as_ref().or(st.python.as_ref()).map(probe_json),
            "tried": st.tried.iter().map(|(p, why)| json!({"command": p, "problem": why})).collect::<Vec<_>>(),
            "voices": cached,
            "dir": tts::vieneu::dir(&dir).to_string_lossy(),
            "venv": tts::vieneu::venv_dir(&dir).to_string_lossy(),
            "cacheDir": tts::vieneu::cache_dir(&dir).to_string_lossy(),
            "installSteps": if st.available() { Vec::new() } else { tts::vieneu::install_steps(&st, &dir) },
            "pythonSetting": "mediaAnalysis.vieneuPython",
            "pythonEnv": tts::vieneu::PYTHON_ENV,
            "package": tts::vieneu::PACKAGE,
            "license": tts::vieneu::LICENSE,
            "source": tts::vieneu::SOURCE,
            "next": next,
        },
    }))
}

/// Get VieNeu-TTS ready: with `install: true`, create `<models>/vieneu/venv` and `pip install
/// vieneu` there when no interpreter has it; then load the model once (it downloads about 0.6 GB
/// into the models directory the first time) and cache the voice list. A background job.
fn setup_vieneu(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "tts.setupVieneu";
    let install = match p.get("install") {
        None | Some(Value::Null) => false,
        Some(v) => v.as_bool().ok_or_else(|| bad(cmd, "`install` must be true or false"))?,
    };
    let python = match p.get("python") {
        None | Some(Value::Null) => None,
        Some(Value::String(py)) if !py.trim().is_empty() && py.len() <= 4096 && !py.contains('\0') => Some(py.trim().to_string()),
        Some(_) => return Err(bad(cmd, "`python` must be the path of a Python interpreter")),
    };
    let dir = s.models_dir().ok_or_else(|| bad(cmd, "no data directory for models"))?;
    let configured = python.clone().unwrap_or_else(|| s.prefs.media_analysis.vieneu_python.clone());
    let st = tts::vieneu::detect(Some(&configured), &dir);
    if let (Some(py), Some(ready)) = (&python, &st.ready)
        && ready.python.source == "setting"
    {
        // the interpreter given works: remember it
        let mut next = s.prefs.clone();
        next.media_analysis.vieneu_python = py.clone();
        s.set_prefs(next).map_err(|e| EngineError::Other(format!("saving preferences: {e}")))?;
    }
    let (ready, base) = match (&st.ready, &st.python) {
        (Some(r), _) => (Some(r.python.clone()), None),
        (None, Some(b)) if install => (None, Some(b.python.clone())),
        (None, _) => {
            let steps = tts::vieneu::install_steps(&st, &dir);
            let tried: Vec<String> = st.tried.iter().map(|(p, why)| format!("{p}: {why}")).collect();
            let how = if st.python.is_some() && !install { " Or run tts.setupVieneu {\"install\": true} to do the first two steps." } else { "" };
            return Err(bad(cmd, format!("VieNeu-TTS is not installed (tried {}). Steps: {}.{how}", tried.join("; "), steps.join("; "))));
        }
    };
    let wait = p.get("wait").and_then(Value::as_bool).unwrap_or(false);
    let job = new_job(s, "Setting up VieNeu-TTS".into());
    let (prog, res) = (job.progress.clone(), job.result.clone());
    let models = dir.clone();
    let id = spawn(s, job, wait, "filmagic-vieneu-setup", move || {
        let t0 = web_time::Instant::now();
        let status = |m: &str| *prog.status.lock().unwrap_or_else(PoisonError::into_inner) = m.to_string();
        let r = (|| -> std::result::Result<String, String> {
            let py = match (ready, base) {
                (Some(py), _) => py,
                (None, Some(base)) => tts::vieneu::install(&base, &models, Some(&prog.cancel), &mut |m| status(m))?,
                (None, None) => return Err("no Python to install VieNeu-TTS with".into()),
            };
            status("Loading the VieNeu model (the first time it downloads about 0.6 GB)…");
            let voices = tts::vieneu::prepare(&py, &models, Some(&prog.cancel)).map_err(tts_message)?;
            Ok(format!("VieNeu-TTS ready with {} voices ({})", voices.len(), py.display()))
        })();
        finish(&prog, &res, r, t0);
    })?;
    Ok(json!({
        "job": id,
        "installing": st.ready.is_none(),
        "venv": tts::vieneu::venv_dir(&dir).to_string_lossy(),
        "cacheDir": tts::vieneu::cache_dir(&dir).to_string_lossy(),
        "notes": ["follow the job with jobs.list; when it is done, tts.voices lists the `vieneu:<name>` voices"],
    }))
}

fn spec(
    id: &'static str,
    label: &'static str,
    params: &'static str,
    enabled: fn(&Session) -> std::result::Result<(), String>,
    run: fn(&mut Session, &Value) -> Result<Value>,
    journal: bool,
) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled, run, journal, named: crate::commands::NamedKeys::NONE }
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec("tts.voices", "List Narration Voices", "{}", always, voices, false),
        spec(
            "tts.preview",
            "Preview Narration",
            r#"{"text":str?,"voice":str?,"pitch":"default"?,"pace":f64?,"sample":bool?,"clip":id?,"path":str?,"referenceAudio":path?,"confirmAuthorizedVoice":bool?}"#,
            always,
            preview,
            false,
        ),
        spec(
            "tts.create",
            "New Narration",
            r#"{"text":str,"voice":str? (tts.voices; "vieneu:<name>" for Vietnamese),"pitch":"extraLow|low|default|high|extraHigh"?,"pace":0.5..2?,"time":ticks?,"track":"A1"|id?,"dir":str?,"referenceAudio":path? (voice "vieneu-clone"),"confirmAuthorizedVoice":bool? (required with referenceAudio)}"#,
            has_seq,
            create,
            true,
        ),
        spec(
            "tts.edit",
            "Edit Narration",
            r#"{"clip":id?,"text":str?,"voice":str?,"pitch":str?,"pace":f64?,"dir":str?,"referenceAudio":path?,"confirmAuthorizedVoice":bool?}"#,
            has_narration_clip,
            edit,
            true,
        ),
        spec("tts.inspect", "Inspect Narration", r#"{"clip":id?,"item":id?}"#, always, inspect, false),
        spec(
            "tts.render",
            "Synthesize Narration",
            r#"{"text":str?,"voice":str?,"pitch":str?,"pace":f64?,"sample":bool?,"clip":id?,"wait":bool?,"referenceAudio":path?,"confirmAuthorizedVoice":bool?}"#,
            always,
            render_job,
            false,
        ),
        spec("tts.downloadVoices", "Download Natural Voices", r#"{"voices":["kokoro-heart",…]? (default all),"wait":bool?}"#, always, download_voices, false),
        spec("tts.engines", "Voice Engines", "{}", always, engines, false),
        spec(
            "tts.setupVieneu",
            "Set Up VieNeu-TTS",
            r#"{"install":bool? (create a Python environment and pip install vieneu),"python":path?,"wait":bool?}"#,
            always,
            setup_vieneu,
            false,
        ),
    ]
}
