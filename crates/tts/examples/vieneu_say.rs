//! `cargo run -p filmagic-tts --example vieneu_say -- <engine dir> <models dir> <voice name> <script.txt> <out.wav> [pace]`
//!
//! Speaks a script file with a VieNeu-TTS voice through Filmagic's bridge (the same path as
//! `tts.create`): finds a Python with `vieneu` (`<engine dir>/venv`, `FILMAGIC_VIENEU_PYTHON` or
//! the PATH), prepares the model and voice list the first time (downloading into
//! `<models dir>/vieneu/cache`), and writes a mono 32-bit float WAV. Pass `-` as the voice name to
//! list the voices.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    use filmagic_tts::vieneu;
    let a: Vec<String> = std::env::args().collect();
    let (Some(engine), Some(models), Some(name), Some(script), Some(out)) = (a.get(1), a.get(2), a.get(3), a.get(4), a.get(5)) else {
        eprintln!("usage: vieneu_say <engine dir> <models dir> <voice name|-> <script.txt> <out.wav> [pace]");
        std::process::exit(2);
    };
    let dirs = vieneu::Dirs { engine: engine.into(), models: models.into() };
    let st = vieneu::detect(&dirs, None);
    let Some(ready) = st.ready else {
        eprintln!("VieNeu-TTS not found; tried: {:?}", st.tried);
        for s in vieneu::install_steps(&st, &dirs) {
            eprintln!("  {s}");
        }
        std::process::exit(1);
    };
    println!("using {} (Python {}, vieneu {:?})", ready.python.display(), ready.python_version, ready.vieneu);
    if vieneu::cached_voices(&dirs.models).is_none() || name == "-" {
        let t0 = std::time::Instant::now();
        let voices = vieneu::prepare(&ready.python, &dirs, None)?;
        println!("prepared in {:.1} s: {}", t0.elapsed().as_secs_f64(), voices.iter().map(|v| v.name.as_str()).collect::<Vec<_>>().join(", "));
        if name == "-" {
            return Ok(());
        }
    }
    let text = std::fs::read_to_string(script)?;
    let pace = a.get(6).and_then(|p| p.parse().ok()).unwrap_or(1.0);
    let req = vieneu::Request { text: &text, voice: Some(name), reference: None, params: filmagic_tts::Params { pace, semitones: 0.0 } };
    let t0 = std::time::Instant::now();
    let audio = vieneu::synthesize(&ready.python, &dirs, &req, None)?;
    let rate = audio.sample_rate;
    let data = (audio.samples.len() * 4) as u32;
    let mut w = Vec::with_capacity(44 + data as usize);
    for part in [
        &b"RIFF"[..],
        &(36 + data).to_le_bytes(),
        b"WAVEfmt ",
        &16u32.to_le_bytes(),
        &3u16.to_le_bytes(),
        &1u16.to_le_bytes(),
        &rate.to_le_bytes(),
        &(rate * 4).to_le_bytes(),
        &4u16.to_le_bytes(),
        &32u16.to_le_bytes(),
        b"data",
        &data.to_le_bytes(),
    ] {
        w.extend_from_slice(part);
    }
    for s in &audio.samples {
        w.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(out, w)?;
    println!("{:.2} s of speech at {rate} Hz in {:.1} s -> {out}", audio.seconds(), t0.elapsed().as_secs_f64());
    Ok(())
}
