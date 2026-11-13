use std::sync::Arc;

use serde_json::json;

use crate::Session;
use filmagic_project::{ItemId, Transcript, Word};
use filmagic_speech::FixedTranscriber;
use filmagic_time::Tick;

/// The demo project with a fake transcriber whose transcript fits the first A1 clip's media:
/// "Hello um world." (Speaker 1), a 1.2 s pause, "Second speaker here." (Speaker 2).
fn session() -> (Session, ItemId, Tick) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    let a = &q.audio_tracks[0].items[0];
    let (item, sin) = (a.item, a.source_in);
    let sec = |x: f64| sin + Tick::from_seconds_f64(x);
    let mut t = Transcript { language: "en".into(), ..Default::default() };
    for (text, a, b, sp) in
        [("Hello", 0.2, 0.5, 0), ("um", 0.6, 0.9, 0), ("world.", 1.0, 1.4, 0), ("Second", 2.6, 3.0, 1), ("speaker", 3.0, 3.5, 1), ("here.", 3.5, 4.0, 1)]
    {
        let mut w = Word::new(text, sec(a), sec(b));
        w.speaker = Some(sp);
        t.words.push(w);
    }
    t.normalize();
    s.transcriber = Some(Arc::new(FixedTranscriber { transcript: t, id: "fixed".into() }));
    let start = a_start(&s);
    (s, item, start)
}

fn a_start(s: &Session) -> Tick {
    s.active_sequence().unwrap().audio_tracks[0].items[0].start
}

fn words(s: &mut Session) -> Vec<String> {
    let r = s.execute("transcript.inspect", json!({})).unwrap();
    r["words"].as_array().unwrap().iter().map(|w| w["text"].as_str().unwrap().to_string()).collect()
}

#[test]
fn generate_inspect_search_and_rename() {
    let (mut s, item, start) = session();
    assert!(s.execute("transcript.select", json!({"from": 0})).is_err(), "disabled before transcribing");
    let r = s.execute("transcript.generate", json!({"items": [item.0]})).unwrap();
    assert_eq!(r["items"][0]["words"], 6, "{r}");
    assert_eq!(r["items"][0]["source"], "fixed");
    assert_eq!(s.project.transcripts[&item].words.len(), 6);

    let r = s.execute("transcript.inspect", json!({})).unwrap();
    assert_eq!(words(&mut s), ["Hello", "um", "world.", "Second", "speaker", "here."]);
    assert_eq!(r["words"][0]["start"].as_i64().unwrap(), (start + Tick::from_seconds_f64(0.2)).0, "mapped to sequence time");
    assert_eq!(r["paragraphs"].as_array().unwrap().len(), 2, "speaker change splits paragraphs: {}", r["paragraphs"]);
    assert_eq!(r["speakers"], json!(["Speaker 1", "Speaker 2"]));

    let r = s.execute("transcript.search", json!({"query": "second spea"})).unwrap();
    assert_eq!(r["matches"], json!([{"from": 3, "to": 4, "start": r["matches"][0]["start"], "end": r["matches"][0]["end"]}]));

    s.execute("transcript.renameSpeaker", json!({"speaker": "Speaker 2", "name": "Ann"})).unwrap();
    let r = s.execute("transcript.inspect", json!({})).unwrap();
    assert_eq!(r["words"][3]["speaker"], "Ann");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.transcripts[&item].speakers[1].name, "Speaker 2");
    s.execute("transcript.renameSpeaker", json!({"speaker": 0, "item": item.0, "name": "Bo"})).unwrap();
    assert_eq!(s.project.transcripts[&item].speakers[0].name, "Bo");
    assert!(s.execute("transcript.renameSpeaker", json!({"speaker": "Nobody", "name": "X"})).is_err());

    // transcribing is one undo step
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.project.transcripts.is_empty());
}

#[test]
fn select_extract_and_lift_by_words() {
    let (mut s, item, _) = session();
    s.execute("transcript.generate", json!({"items": [item.0]})).unwrap();
    let rate = s.sequence_rate();
    let before = s.active_sequence().unwrap().duration();

    let r = s.execute("transcript.select", json!({"from": 3, "to": 5})).unwrap();
    let (a, b) = (Tick(r["start"].as_i64().unwrap()), Tick(r["end"].as_i64().unwrap()));
    let q = s.active_sequence().unwrap();
    assert_eq!(q.mark_in, Some(a));
    assert_eq!(q.mark_out, Some(b - rate.frame_duration()), "Out is the last frame inside");
    assert_eq!(rate.snap(a), a, "frame aligned");
    assert_eq!(s.playhead(), a);

    // Extract "um": the sequence gets shorter by the word's frames and the word is gone
    let r = s.execute("transcript.extract", json!({"from": 1})).unwrap();
    let cut = Tick(r["end"].as_i64().unwrap()) - Tick(r["start"].as_i64().unwrap());
    assert!(cut > Tick::ZERO);
    assert_eq!(s.active_sequence().unwrap().duration(), before - cut);
    assert!(!words(&mut s).contains(&"um".to_string()), "{:?}", words(&mut s));
    s.active_sequence().unwrap().check().unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active_sequence().unwrap().duration(), before);

    // Lift leaves a gap: same duration, word gone
    s.execute("transcript.lift", json!({"from": 1, "to": 1})).unwrap();
    assert_eq!(s.active_sequence().unwrap().duration(), before);
    assert_eq!(words(&mut s), ["Hello", "world.", "Second", "speaker", "here."]);
    assert!(s.execute("transcript.extract", json!({"from": 99})).is_err());
}

#[test]
fn remove_fillers_pauses_and_create_captions() {
    let (mut s, item, _) = session();
    s.execute("transcript.generate", json!({"items": [item.0]})).unwrap();
    let before = s.active_sequence().unwrap().duration();
    let r = s.execute("transcript.removeFillers", json!({})).unwrap();
    assert_eq!(r["removed"], 1, "{r}");
    assert!(!words(&mut s).contains(&"um".to_string()));
    let r = s.execute("transcript.removePauses", json!({"minSeconds": 1.0, "keepSeconds": 0.1})).unwrap();
    assert_eq!(r["removed"], 1, "{r}");
    let after = s.active_sequence().unwrap().duration();
    assert!(after < before - Tick::from_seconds_f64(1.0), "the pause and the filler are gone");
    assert_eq!(words(&mut s).len(), 5);
    s.active_sequence().unwrap().check().unwrap();

    let r = s.execute("transcript.createCaptions", json!({"maxChars": 32})).unwrap();
    assert_eq!(r["captions"], 2, "one caption per speaker: {r}");
    let q = s.active_sequence().unwrap();
    let tr = &q.caption_tracks[0];
    assert_eq!(tr.captions[0].text, "Hello world.");
    assert_eq!(tr.captions[1].speaker.as_deref(), Some("Speaker 2"));
    q.check().unwrap();
}

#[test]
fn set_delete_and_models() {
    let (mut s, item, _) = session();
    let t = json!({"language": "en", "words": [
        {"text": "b", "start": 2000, "end": 3000, "speaker": 1},
        {"text": "a", "start": 0, "end": 1000},
    ]});
    let r = s.execute("transcript.set", json!({"item": item.0, "transcript": t})).unwrap();
    assert_eq!(r["words"], 2);
    let tr = &s.project.transcripts[&item];
    assert_eq!(tr.words[0].text, "a", "normalized: sorted");
    assert_eq!(tr.speakers.len(), 2, "missing speaker labels added");
    assert_eq!(tr.source, "imported");
    assert!(s.execute("transcript.set", json!({"item": 999_999, "transcript": {}})).is_err());
    assert!(s.execute("transcript.set", json!({"item": item.0, "transcript": {"words": 3}})).is_err());

    // survives save/load
    let bytes = filmagic_format::encode(&s.project, true);
    let back = filmagic_format::decode(&bytes).unwrap();
    assert_eq!(back.project.transcripts[&item].words.len(), 2);

    s.execute("transcript.delete", json!({"items": [item.0]})).unwrap();
    assert!(s.project.transcripts.is_empty());
    assert!(s.execute("transcript.delete", json!({})).is_err());

    let m = s.execute("transcript.models", json!({})).unwrap();
    assert_eq!(m["available"], filmagic_speech::available());
    assert!(m["models"].as_array().unwrap().iter().any(|x| x["id"] == "whisper-base"));
}

#[test]
fn generate_without_a_transcriber() {
    let (mut s, item, _) = session();
    s.transcriber = None;
    let e = s.execute("transcript.generate", json!({"items": [item.0], "model": "nope"})).unwrap_err().to_string();
    // a build without speech-to-text says that first: the command is disabled (#97)
    let why = if filmagic_speech::available() { "unknown speech model" } else { "not available in this build" };
    assert!(e.contains(why), "{e}");
    if !filmagic_speech::available() {
        assert!(!s.is_enabled("sequence.transcribe"), "Transcribe Sequence follows transcript.generate");
        let e = s.execute("transcript.generate", json!({"items": [item.0]})).unwrap_err().to_string();
        assert!(e.contains("whisper") && e.contains("not available"), "{e}");
        #[cfg(not(feature = "speech-download"))]
        assert!(s.execute("transcript.downloadModel", json!({})).unwrap_err().to_string().contains("not available"));
    }
    // defaults to the media of the open sequence's audio clips
    s.transcriber = Some(Arc::new(FixedTranscriber { transcript: Transcript::default(), id: "empty".into() }));
    assert!(s.is_enabled("sequence.transcribe"), "an installed recogniser enables Transcribe Sequence");
    let r = s.execute("transcript.generate", json!({})).unwrap();
    assert!(r["items"].as_array().unwrap().len() >= 2, "{r}");
}

/// Records the language each transcription asked for.
struct LanguageProbe {
    seen: std::sync::Mutex<Vec<Option<String>>>,
}

impl filmagic_speech::Transcriber for LanguageProbe {
    fn id(&self) -> String {
        "probe".into()
    }
    fn transcribe(&self, _: &[f32], opts: &filmagic_speech::Options, _: filmagic_speech::ProgressFn) -> Result<Transcript, filmagic_speech::SpeechError> {
        self.seen.lock().unwrap().push(opts.language.clone());
        let mut t = Transcript { language: opts.language.clone().unwrap_or_else(|| "vi".into()), ..Default::default() };
        t.words.push(Word::new("Xin", Tick::ZERO, Tick::from_seconds_f64(0.3)));
        Ok(t)
    }
}

#[test]
fn transcription_takes_vietnamese_and_refuses_unknown_languages() {
    let (mut s, item, _) = session();
    let probe = Arc::new(LanguageProbe { seen: Default::default() });
    s.transcriber = Some(probe.clone());
    for (given, want) in [
        (json!("vi"), Some("vi")),
        (json!("vi-VN"), Some("vi")),
        (json!(" VI "), Some("vi")),
        (json!("auto"), None),
        (json!(""), None),
        (json!("yue"), Some("yue")),
    ] {
        s.execute("transcript.generate", json!({"items": [item.0], "language": given})).unwrap_or_else(|e| panic!("{given}: {e}"));
        assert_eq!(probe.seen.lock().unwrap().last().cloned().flatten().as_deref(), want, "{given}");
    }
    // the setting: Vietnamese as the default language, written either way
    for setting in ["vi", "vi-VN"] {
        s.prefs.media_analysis.default_language = setting.into();
        s.execute("transcript.generate", json!({"items": [item.0]})).unwrap();
        assert_eq!(probe.seen.lock().unwrap().last().cloned().flatten().as_deref(), Some("vi"), "{setting}");
    }
    let calls = probe.seen.lock().unwrap().len();
    for bad in [json!("xx"), json!("klingon"), json!("vi\u{0}"), json!(5), json!(true), json!(["vi"]), json!({"code": "vi"}), json!("a".repeat(100_000))] {
        let e = s.execute("transcript.generate", json!({"items": [item.0], "language": bad})).unwrap_err().to_string();
        assert!(e.contains("language"), "{e}");
        assert!(e.len() < 400, "the input is not echoed in full: {} bytes", e.len());
    }
    assert_eq!(probe.seen.lock().unwrap().len(), calls, "refused before transcribing");
}

#[test]
fn models_list_the_languages_and_a_model_for_vietnamese() {
    let mut s = Session::default();
    let r = s.execute("transcript.models", json!({})).unwrap();
    let codes: Vec<&str> = r["languages"].as_array().unwrap().iter().map(|l| l["code"].as_str().unwrap()).collect();
    assert!(codes.contains(&"vi") && codes.contains(&"en") && codes.len() >= 99, "{codes:?}");
    let ids: Vec<&str> = r["models"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&"whisper-large-v3-turbo"), "{ids:?}");
    // the Settings choice offers it too, and Vietnamese as a default language
    s.execute("prefs.set", json!({"key": "mediaAnalysis.whisperModel", "value": "whisper-large-v3-turbo"})).unwrap();
    s.execute("prefs.set", json!({"key": "mediaAnalysis.defaultLanguage", "value": "vi"})).unwrap();
    assert_eq!(s.prefs.media_analysis.default_language, "vi");
}
