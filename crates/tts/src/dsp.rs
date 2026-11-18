//! What voices without their own pace or pitch control need afterwards: a time stretch that keeps
//! the pitch (WSOLA, `filmagic_audio_dsp::stretch`), a pitch shift that keeps the length, and
//! headroom so a narration never clips.

use filmagic_audio_dsp::AudioEffect;
use filmagic_audio_dsp::stretch::Wsola;

/// `len` samples of `x` from `start` (may be negative or past the end: silence there).
fn window(x: &[f32], start: i64, len: usize) -> Vec<f32> {
    (0..len)
        .map(|t| {
            let i = start.saturating_add(t as i64);
            if i < 0 { 0.0 } else { usize::try_from(i).ok().and_then(|i| x.get(i)).copied().unwrap_or(0.0) }
        })
        .collect()
}

/// Play `x` at `speed` (2 = twice as fast, half as long) keeping its pitch.
pub fn stretch(x: &[f32], speed: f64, rate: u32) -> Vec<f32> {
    if x.is_empty() || !speed.is_finite() || speed <= 0.0 || (speed - 1.0).abs() < 1e-9 {
        return x.to_vec();
    }
    let ws = Wsola::new(rate);
    let h = ws.hop();
    let out_len = (x.len() as f64 / speed).round() as usize;
    let hops = out_len.div_ceil(h.max(1));
    let mut out = vec![0.0f32; hops.saturating_mul(h)];
    // the grain before the first: its falling half is the first hop of the input
    let mut prev = -(h as i64);
    for (k, chunk) in out.chunks_mut(h).enumerate() {
        let nominal = (k as f64 * h as f64 * speed).round() as i64;
        let pos = ws.align(x, prev + h as i64, nominal);
        let falling = window(x, prev + h as i64, h);
        let rising = window(x, pos, h);
        ws.overlap(&falling, &rising, chunk);
        prev = pos;
    }
    out.truncate(out_len);
    out
}

/// Shift the pitch of `x` by `semitones`, keeping its length (latency compensated).
pub fn pitch_shift(mut x: Vec<f32>, semitones: f64, rate: u32) -> Vec<f32> {
    let mut ps = filmagic_audio_dsp::effects::PitchShifter::new(rate as f32, 1);
    ps.set_param("semitones", semitones as f32);
    ps.reset();
    let lat = ps.latency();
    let n = x.len();
    x.resize(n + lat, 0.0);
    for block in x.chunks_mut(4096) {
        ps.process(&mut [block]);
    }
    x.drain(..lat.min(x.len()));
    x.truncate(n);
    x
}

/// Scale down so the peak is at most 0.98.
pub fn headroom(x: &mut [f32]) {
    let peak = x.iter().fold(0f32, |m, v| m.max(v.abs()));
    if peak > 0.98 {
        let g = 0.98 / peak;
        x.iter_mut().for_each(|v| *v *= g);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(n: usize, rate: u32) -> Vec<f32> {
        (0..n).map(|i| (i as f32 * 220.0 * std::f32::consts::TAU / rate as f32).sin() * 0.5).collect()
    }

    #[test]
    fn stretch_changes_the_length_not_the_level() {
        let rate = 24_000;
        let x = tone(rate as usize * 2, rate);
        for speed in [0.5, 0.8, 1.25, 2.0] {
            let y = stretch(&x, speed, rate);
            let want = (x.len() as f64 / speed).round() as usize;
            assert_eq!(y.len(), want, "{speed}");
            let rms = |v: &[f32]| (v.iter().map(|s| s * s).sum::<f32>() / v.len().max(1) as f32).sqrt();
            let (a, b) = (rms(&x), rms(y.get(2000..y.len() - 2000).unwrap_or(&y)));
            assert!((b / a - 1.0).abs() < 0.15, "{speed}: {a} vs {b}");
            assert!(y.iter().all(|s| s.is_finite()));
        }
        for bad in [f64::NAN, 0.0, -1.0, f64::INFINITY] {
            assert_eq!(stretch(&x, bad, rate), x);
        }
        assert!(stretch(&[], 2.0, rate).is_empty());
        assert_eq!(stretch(&[0.5], 0.5, rate).len(), 2);
    }

    #[test]
    fn headroom_never_clips() {
        let mut x = vec![0.1, -3.0, 2.0];
        headroom(&mut x);
        assert!(x.iter().all(|s| s.abs() <= 0.98 + 1e-6));
        let mut quiet = vec![0.5, -0.5];
        headroom(&mut quiet);
        assert_eq!(quiet, vec![0.5, -0.5]);
    }

    #[test]
    fn pitch_shift_keeps_the_length() {
        let x = tone(10_000, 24_000);
        assert_eq!(pitch_shift(x.clone(), 3.0, 24_000).len(), x.len());
        assert!(pitch_shift(Vec::new(), 3.0, 24_000).is_empty());
    }
}
