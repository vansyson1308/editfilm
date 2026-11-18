# filmagic-speech

Speech-to-text for Filmagic's Text panel ▸ Transcript (L2). User-facing behaviour, commands and
model handling: [docs/transcripts.md](../../docs/transcripts.md).

- `Transcriber` trait (mono 16 kHz in, word-timed `filmagic_project::Transcript` out) and
  `FixedTranscriber` for tests and agents.
- `models`: Whisper model catalogue (tiny, base, small and large-v3 turbo, all multilingual;
  pinned revisions, SHA-256, sizes, licences) and, with feature `download`, the verified downloader
  (ureq + rustls + RustCrypto). Weights are never bundled.
- `LANGUAGES`: the 100 languages Whisper recognises (code and name; `yue` needs a large-v3 model).
  Checked on 2026-11-13: Vietnamese speech made by VieNeu-TTS ("Xin chào các bạn. Hôm nay chúng ta
  học cách dựng video bằng Filmagic.") was transcribed by whisper-large-v3-turbo with `vi` as
  "Xin chào các bạn! Hôm nay chúng ta học cách dựng video bằng FuelMagic." The only error is the
  product name. The words had timestamps, 4.3 s of audio took 23.5 s on a loaded CPU, and the
  128-band mel front end needed no change.
- `whisper` (feature `whisper`): Whisper inference on candle (CPU, pure Rust).
- `mel`, `vad`, `diarize`: log-mel front end, word-bound tightening, MFCC-clustering speaker labels.

Both features are off by default and are never enabled for wasm (`cargo xtask wasm` checks the
crate without them).

## References

Implemented from the published description of the model: A. Radford et al., "Robust Speech
Recognition via Large-Scale Weak Supervision" (OpenAI, 2022), the model cards and configuration
files published with the weights (`config.json`, `generation_config.json`, `tokenizer.json`), and
the candle tensor library's public API. Weights: OpenAI Whisper, MIT licence.
