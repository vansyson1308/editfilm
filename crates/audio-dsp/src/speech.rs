//! A light speech detector that needs no model: where in a signal someone is speaking.
//!
//! Clean room, from textbook methods:
//!
//! * **Band limiting and decimation.** The input goes through a 70 Hz high-pass and two 3.4 kHz
//!   low-passes (RBJ biquads; the telephone speech band) and is decimated to about 8 kHz.
//! * **Frame features** (10 ms hop, 30 ms window): the frame level in dBFS and its *voicing*, the
//!   peak of the normalised autocorrelation `r(τ) = Σx[n]x[n+τ] / √(Σx[n]² Σx[n+τ]²)` over lags
//!   for a pitch of 70–400 Hz (the classic voiced/unvoiced decision of short-time autocorrelation
//!   analysis, Rabiner & Schafer, *Digital Processing of Speech Signals*, 1978, ch. 4).
//! * **An adaptive level threshold**, as in energy-based endpoint detection (Rabiner & Sambur,
//!   1975): set from the signal's own noise floor (10th percentile of the frame levels, digital
//!   silence counted as −100 dBFS) and speech level (95th percentile), 30 % of the way up and at least 6 dB above the
//!   floor, and never below −60 dBFS.
//! * **Segments.** A frame that is loud and voiced is a speech core. Cores grow over the loud
//!   frames next to them (unvoiced consonants, up to 200 ms) and above a low threshold 3 dB over
//!   the floor; gaps shorter than [`SpeechParams::bridge_s`] (the pauses between words) are
//!   bridged, and a segment needs at least [`SpeechParams::min_voiced_s`] of voiced frames.
//! * **Syllabic modulation.** Speech rises and falls with its syllables, 3–8 times a second
//!   (the 4 Hz peak of the speech modulation spectrum, Houtgast & Steeneken). A voiced segment
//!   whose level hardly moves (spread of its frame levels under 2 dB over 300 ms or more) is a
//!   steady tone, a hum or a held note, not speech, and is dropped.
//!
//! A car door, a footstep or a noise burst is loud but not voiced; a steady tone or hum is voiced
//! but not modulated: neither is speech. Music with a sung or strongly rhythmic melody can still
//! read as speech; a clip's Essential Sound type is the better guide there.
//!
//! [`SpeechSegment::confidence`] (0…1) is the mean voicing of the segment's voiced frames times
//! how strongly its level is modulated (full at a 4 dB spread).
//!
//! Hostile input is harmless: non-finite samples count as silence, any sample rate works (below
//! 16 kHz nothing is decimated), and analysis stops after [`MAX_FRAMES`] frames (six hours).

use crate::biquad::{Biquad, Coeffs, FilterType};

/// Hop between analysis frames (seconds).
pub const HOP_S: f64 = 0.01;
/// Analysis window (seconds).
const WINDOW_S: f64 = 0.03;
/// Pitch range searched for voicing (Hz).
const F0_MIN: f64 = 70.0;
const F0_MAX: f64 = 400.0;
/// Frames analysed at most (six hours at 10 ms); later audio is ignored and flagged.
pub const MAX_FRAMES: usize = 6 * 3600 * 100;
/// Frames of unvoiced but loud audio a core may grow over, on each side (200 ms).
const GROW_FRAMES: usize = 20;
/// A voiced segment of at least this many frames whose level spread is under
/// [`STEADY_SPREAD_DB`] is a steady tone.
const STEADY_FRAMES: usize = 30;
const STEADY_SPREAD_DB: f32 = 2.0;
/// Level spread at which the modulation factor of the confidence is full.
const FULL_SPREAD_DB: f32 = 4.0;

/// Detector settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpeechParams {
    /// Peak normalised autocorrelation for a frame to count as voiced (0…1).
    pub voicing: f32,
    /// Pauses shorter than this (seconds) stay inside one segment: the gaps between words.
    pub bridge_s: f64,
    /// Voiced audio a segment needs (seconds).
    pub min_voiced_s: f64,
    /// Frames quieter than this (dBFS) are never speech.
    pub min_level_db: f32,
}

impl Default for SpeechParams {
    fn default() -> Self {
        SpeechParams { voicing: 0.6, bridge_s: 0.25, min_voiced_s: 0.12, min_level_db: -60.0 }
    }
}

/// One stretch of speech, in seconds from the start of the analysed signal.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpeechSegment {
    pub start_s: f64,
    pub end_s: f64,
    /// 0…1: how voiced and how syllabically modulated the segment is.
    pub confidence: f32,
}

/// The result of an analysis.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SpeechAnalysis {
    pub segments: Vec<SpeechSegment>,
    /// Seconds analysed.
    pub duration_s: f64,
    /// The level threshold used (dBFS).
    pub threshold_db: f32,
    /// The noise floor found (dBFS).
    pub floor_db: f32,
    /// Analysis stopped at [`MAX_FRAMES`]; audio after `duration_s` was not looked at.
    pub truncated: bool,
}

impl SpeechAnalysis {
    /// Total seconds of speech.
    pub fn speech_s(&self) -> f64 {
        self.segments.iter().map(|s| s.end_s - s.start_s).sum()
    }
    /// The segment containing `t` (seconds), if any.
    pub fn segment_at(&self, t: f64) -> Option<&SpeechSegment> {
        self.segments.iter().find(|s| s.start_s < t && t < s.end_s)
    }
}

#[derive(Clone, Copy, Debug)]
struct Frame {
    db: f32,
    voicing: f32,
}

/// Streaming detector: feed mono audio with [`SpeechDetector::process`], then
/// [`SpeechDetector::finish`].
pub struct SpeechDetector {
    params: SpeechParams,
    /// Decimation factor and the decimated rate.
    decim: usize,
    rate: f64,
    phase: usize,
    hp: (Coeffs, Biquad),
    lp: (Coeffs, Biquad, Biquad),
    /// Decimated samples still needed by a frame; the next frame starts at `buf[next]`.
    buf: Vec<f32>,
    next: usize,
    hop: usize,
    win: usize,
    lag_min: usize,
    lag_max: usize,
    /// Decimated samples received.
    received: u64,
    frames: Vec<Frame>,
    truncated: bool,
}

impl SpeechDetector {
    pub fn new(sample_rate: f64, params: SpeechParams) -> Self {
        let sr = if sample_rate.is_finite() && sample_rate >= 1000.0 { sample_rate.min(1.0e6) } else { 8000.0 };
        let decim = ((sr / 8000.0).floor() as usize).max(1);
        let rate = sr / decim as f64;
        let hop = ((HOP_S * rate).round() as usize).max(1);
        let win = ((WINDOW_S * rate).round() as usize).max(8);
        let lag_min = ((rate / F0_MAX).floor() as usize).max(2);
        let lag_max = ((rate / F0_MIN).ceil() as usize).max(lag_min + 1);
        let q = std::f64::consts::FRAC_1_SQRT_2;
        let p = SpeechParams {
            voicing: if params.voicing.is_finite() { params.voicing.clamp(0.0, 1.0) } else { 0.6 },
            bridge_s: if params.bridge_s.is_finite() { params.bridge_s.clamp(0.0, 5.0) } else { 0.25 },
            min_voiced_s: if params.min_voiced_s.is_finite() { params.min_voiced_s.clamp(0.0, 10.0) } else { 0.12 },
            min_level_db: if params.min_level_db.is_finite() { params.min_level_db.clamp(-120.0, 0.0) } else { -60.0 },
        };
        SpeechDetector {
            params: p,
            decim,
            rate,
            phase: 0,
            hp: (Coeffs::design(FilterType::HighPass, sr, F0_MIN, q, 0.0), Biquad::new()),
            lp: (Coeffs::design(FilterType::LowPass, sr, 3400.0, q, 0.0), Biquad::new(), Biquad::new()),
            buf: Vec::new(),
            next: 0,
            hop,
            win,
            lag_min,
            lag_max,
            received: 0,
            frames: Vec::new(),
            truncated: false,
        }
    }

    /// Feed mono samples.
    pub fn process(&mut self, mono: &[f32]) {
        if self.truncated {
            return;
        }
        for &x in mono {
            let x = if x.is_finite() { x as f64 } else { 0.0 };
            let y = self.hp.1.tick(&self.hp.0, x);
            let y = self.lp.1.tick(&self.lp.0, y);
            let y = self.lp.2.tick(&self.lp.0, y);
            self.phase += 1;
            if self.phase >= self.decim {
                self.phase = 0;
                self.buf.push(y as f32);
                self.received += 1;
            }
        }
        self.run_frames();
    }

    /// Feed a stereo (or wider) block as its channel average.
    pub fn process_planar(&mut self, channels: &[&[f32]]) {
        let n = channels.iter().map(|c| c.len()).min().unwrap_or(0);
        if n == 0 {
            return;
        }
        let k = channels.len() as f32;
        let mono: Vec<f32> = (0..n).map(|i| channels.iter().map(|c| c.get(i).copied().unwrap_or(0.0)).sum::<f32>() / k).collect();
        self.process(&mono);
    }

    fn run_frames(&mut self) {
        let span = self.win + self.lag_max;
        while self.next + span <= self.buf.len() {
            if self.frames.len() >= MAX_FRAMES {
                self.truncated = true;
                self.buf.clear();
                self.next = 0;
                return;
            }
            let f = self.frame(self.next);
            self.frames.push(f);
            self.next += self.hop;
        }
        if self.next > 8192 {
            self.buf.drain(..self.next);
            self.next = 0;
        }
    }

    fn frame(&self, at: usize) -> Frame {
        let x = self.buf.get(at..at + self.win + self.lag_max).unwrap_or(&[]);
        let w = self.win.min(x.len());
        let e0: f64 = x.iter().take(w).map(|v| (*v as f64) * (*v as f64)).sum();
        let db = (10.0 * (e0 / w.max(1) as f64 + 1e-12).log10()) as f32;
        if e0 <= 1e-14 || x.len() < w + self.lag_max {
            return Frame { db, voicing: 0.0 };
        }
        // energy of the lagged window, slid one sample per lag
        let mut el: f64 = x[self.lag_min..self.lag_min + w].iter().map(|v| (*v as f64) * (*v as f64)).sum();
        let mut best = 0.0f64;
        for tau in self.lag_min..=self.lag_max {
            if tau > self.lag_min {
                let out = x[tau - 1] as f64;
                let inn = x[tau + w - 1] as f64;
                el = (el - out * out + inn * inn).max(0.0);
            }
            if el <= 1e-14 {
                continue;
            }
            let num: f64 = x[..w].iter().zip(&x[tau..tau + w]).map(|(a, b)| *a as f64 * *b as f64).sum();
            let r = num / (e0 * el).sqrt();
            if r > best {
                best = r;
            }
        }
        Frame { db, voicing: best.clamp(0.0, 1.0) as f32 }
    }

    /// Finish the analysis and return the speech segments.
    pub fn finish(mut self) -> SpeechAnalysis {
        let total = self.received;
        if !self.truncated {
            // flush the filters' tail and the last partial frames
            let pad = self.win + self.lag_max;
            self.buf.extend(std::iter::repeat_n(0.0, pad));
            let want = (total as f64 / self.hop as f64).ceil() as usize;
            let span = self.win + self.lag_max;
            while self.frames.len() < want && self.next + span <= self.buf.len() && self.frames.len() < MAX_FRAMES {
                let f = self.frame(self.next);
                self.frames.push(f);
                self.next += self.hop;
            }
        }
        let duration_s = if self.truncated { self.frames.len() as f64 * self.hop as f64 / self.rate } else { total as f64 / self.rate };
        segments(&self.frames, self.hop as f64 / self.rate, self.win as f64 / self.rate, duration_s, &self.params, self.truncated)
    }
}

fn percentile(sorted: &[f32], p: f32) -> f32 {
    if sorted.is_empty() {
        return -120.0;
    }
    let i = ((sorted.len() - 1) as f32 * p) as usize;
    sorted.get(i).copied().unwrap_or(-120.0)
}

fn spread_db(levels: &[f32]) -> f32 {
    if levels.len() < 2 {
        return 0.0;
    }
    let n = levels.len() as f32;
    let mean = levels.iter().sum::<f32>() / n;
    (levels.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / n).sqrt()
}

fn segments(frames: &[Frame], hop_s: f64, win_s: f64, duration_s: f64, p: &SpeechParams, truncated: bool) -> SpeechAnalysis {
    // digital silence counts as a −100 dBFS floor: a clean recording with real pauses gets the
    // absolute gate, a noisy one a threshold above its own noise
    let mut lv: Vec<f32> = frames.iter().map(|f| f.db.max(-100.0)).collect();
    lv.sort_by(f32::total_cmp);
    let floor = percentile(&lv, 0.10);
    let top = percentile(&lv, 0.95);
    let thr = (floor + 0.3 * (top - floor)).max(floor + 6.0).max(p.min_level_db);
    let low = (floor + 3.0).max(p.min_level_db);
    let mut out = SpeechAnalysis { segments: Vec::new(), duration_s, threshold_db: thr, floor_db: floor, truncated };
    if top <= -100.0 {
        return out;
    }
    let core: Vec<bool> = frames.iter().map(|f| f.db > thr && f.voicing >= p.voicing).collect();
    // grow cores over neighbouring loud frames (consonants, word edges)
    let n = frames.len();
    let mut on = core.clone();
    for k in 0..n {
        if !core[k] {
            continue;
        }
        let mut j = k;
        while j > 0 && k - (j - 1) <= GROW_FRAMES && !core[j - 1] && frames[j - 1].db > low {
            j -= 1;
            on[j] = true;
        }
        let mut j = k + 1;
        while j < n && j - k <= GROW_FRAMES && !core[j] && frames[j].db > low {
            on[j] = true;
            j += 1;
        }
    }
    // runs of frames
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut start = None;
    for (k, &v) in on.iter().enumerate() {
        match (v, start) {
            (true, None) => start = Some(k),
            (false, Some(s)) => {
                runs.push((s, k));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        runs.push((s, n));
    }
    let bridge = (p.bridge_s / hop_s).round() as usize;
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for r in runs {
        match merged.last_mut() {
            Some(last) if r.0 - last.1 < bridge => last.1 = r.1,
            _ => merged.push(r),
        }
    }
    let min_voiced = (p.min_voiced_s / hop_s).round().max(1.0) as usize;
    // frame k is centred on k·hop + win/2
    let centre = win_s / 2.0;
    for (a, b) in merged {
        let voiced: Vec<usize> = (a..b).filter(|k| core[*k]).collect();
        if voiced.len() < min_voiced {
            continue;
        }
        // the spread of the voiced frames' levels, without the window-long ramps at the edges
        let edge = (win_s / hop_s).ceil() as usize;
        let inner = voiced.get(edge..voiced.len().saturating_sub(edge)).filter(|v| !v.is_empty()).unwrap_or(&voiced);
        let levels: Vec<f32> = inner.iter().map(|k| frames[*k].db).collect();
        let spread = spread_db(&levels);
        if b - a >= STEADY_FRAMES && spread < STEADY_SPREAD_DB {
            continue;
        }
        let mean_v = voiced.iter().map(|k| frames[*k].voicing).sum::<f32>() / voiced.len() as f32;
        let confidence = (mean_v * (spread / FULL_SPREAD_DB).min(1.0)).clamp(0.0, 1.0);
        let start_s = (a as f64 * hop_s + centre - hop_s / 2.0).max(0.0);
        let end_s = (b as f64 * hop_s + centre - hop_s / 2.0).min(duration_s).max(start_s);
        out.segments.push(SpeechSegment { start_s, end_s, confidence });
    }
    out
}

/// Analyse a whole mono signal.
pub fn detect(mono: &[f32], sample_rate: f64, params: SpeechParams) -> SpeechAnalysis {
    let mut d = SpeechDetector::new(sample_rate, params);
    d.process(mono);
    d.finish()
}

/// Synthetic test signals shared by the tests of this crate and of dependent crates.
#[cfg(any(test, feature = "test-util"))]
pub mod test_signals {
    /// Deterministic xorshift noise in [-1, 1).
    pub struct Rng(pub u64);
    impl Rng {
        pub fn sample(&mut self) -> f32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            ((self.0 >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
        }
    }

    /// A speech-like signal: a voiced source (gliding f0 ≈ 110–160 Hz, harmonics falling 6 dB/oct
    /// up to 4 kHz) under a syllable envelope (~4.5 syllables/s), only where `active(t)`.
    pub fn speech(secs: f64, sr: u32, amp: f32, active: impl Fn(f64) -> bool) -> Vec<f32> {
        let n = (secs * sr as f64) as usize;
        let mut phase = 0.0f64;
        (0..n)
            .map(|i| {
                let t = i as f64 / sr as f64;
                if !active(t) {
                    return 0.0;
                }
                let f0 = 135.0 + 25.0 * (2.0 * std::f64::consts::PI * 0.7 * t).sin();
                phase += f0 / sr as f64;
                let mut v = 0.0;
                let mut k = 1;
                while k as f64 * f0 < 4000.0 {
                    v += (2.0 * std::f64::consts::PI * phase * k as f64).sin() / k as f64;
                    k += 1;
                }
                let syl = 0.6 - 0.4 * (2.0 * std::f64::consts::PI * 4.5 * t).cos();
                (v * syl * 0.5) as f32 * amp
            })
            .collect()
    }

    /// Decaying broadband noise bursts (a car door, a footstep) of `len_s` at each time in `at`.
    pub fn bursts(secs: f64, sr: u32, amp: f32, at: &[f64], len_s: f64) -> Vec<f32> {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let n = (secs * sr as f64) as usize;
        (0..n)
            .map(|i| {
                let t = i as f64 / sr as f64;
                let noise = rng.sample();
                match at.iter().find(|a| t >= **a && t < **a + len_s) {
                    Some(a) => noise * amp * (-(t - a) * 12.0).exp() as f32,
                    None => 0.0,
                }
            })
            .collect()
    }

    /// Steady noise at `amp` RMS-ish (room tone, traffic).
    pub fn noise(secs: f64, sr: u32, amp: f32, seed: u64) -> Vec<f32> {
        let mut rng = Rng(seed.max(1));
        (0..(secs * sr as f64) as usize).map(|_| rng.sample() * amp).collect()
    }

    /// A sine at `f` Hz.
    pub fn tone(secs: f64, sr: u32, f: f64, amp: f32) -> Vec<f32> {
        (0..(secs * sr as f64) as usize).map(|i| (amp as f64 * (2.0 * std::f64::consts::PI * f * i as f64 / sr as f64).sin()) as f32).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::test_signals::*;
    use super::*;

    const SR: u32 = 48_000;

    fn near(segs: &[SpeechSegment], want: &[(f64, f64)], tol: f64) {
        assert_eq!(segs.len(), want.len(), "segments {segs:?}, expected {want:?}");
        for (s, (a, b)) in segs.iter().zip(want) {
            assert!((s.start_s - a).abs() <= tol && (s.end_s - b).abs() <= tol, "segment {s:?}, expected [{a}, {b}]");
        }
    }

    fn mix(a: &[f32], b: &[f32]) -> Vec<f32> {
        a.iter().zip(b).map(|(x, y)| x + y).collect()
    }

    #[test]
    fn speech_with_pauses_is_found_where_it_is() {
        let on = |t: f64| (1.0..3.2).contains(&t) || (4.0..6.5).contains(&t) || (8.0..8.9).contains(&t);
        for (sr, amp) in [(SR, 0.4f32), (44_100, 0.05), (16_000, 0.8), (8_000, 0.2)] {
            let x = speech(10.0, sr, amp, on);
            let a = detect(&x, sr as f64, SpeechParams::default());
            println!("{sr} Hz, amp {amp}: {:?}", a.segments);
            near(&a.segments, &[(1.0, 3.2), (4.0, 6.5), (8.0, 8.9)], 0.06);
            assert!(a.segments.iter().all(|s| s.confidence > 0.5), "{:?}", a.segments);
            assert!((a.duration_s - 10.0).abs() < 0.01);
        }
    }

    #[test]
    fn speech_over_steady_noise_is_still_found() {
        let on = |t: f64| (2.0..4.0).contains(&t) || (6.0..7.5).contains(&t);
        let x = mix(&speech(10.0, SR, 0.4, on), &noise(10.0, SR, 0.02, 7));
        let a = detect(&x, SR as f64, SpeechParams::default());
        println!("speech over noise: {:?} (floor {} dB, threshold {} dB)", a.segments, a.floor_db, a.threshold_db);
        near(&a.segments, &[(2.0, 4.0), (6.0, 7.5)], 0.08);
    }

    #[test]
    fn noise_bursts_tones_hum_and_silence_are_not_speech() {
        let cases: Vec<(&str, Vec<f32>)> = vec![
            ("car-door bursts", bursts(8.0, SR, 0.9, &[1.0, 3.5, 6.0], 0.4)),
            ("steady noise", noise(8.0, SR, 0.2, 3)),
            ("steady 440 Hz tone", tone(8.0, SR, 440.0, 0.3)),
            ("hum 120 Hz + harmonics", mix(&tone(8.0, SR, 120.0, 0.2), &tone(8.0, SR, 240.0, 0.1))),
            ("1 kHz beeps with pauses", {
                let t = tone(8.0, SR, 1000.0, 0.3);
                t.iter().enumerate().map(|(i, v)| if (i as f64 / SR as f64) % 1.5 < 1.0 { *v } else { 0.0 }).collect()
            }),
            ("silence", vec![0.0; 8 * SR as usize]),
        ];
        for (name, x) in cases {
            let a = detect(&x, SR as f64, SpeechParams::default());
            println!("{name}: {:?}", a.segments);
            assert!(a.segments.is_empty(), "{name}: {:?}", a.segments);
        }
    }

    #[test]
    fn hostile_input_does_not_panic() {
        for sr in [0.0, -1.0, f64::NAN, f64::INFINITY, 1.0, 1e12] {
            let a = detect(&[0.5, f32::NAN, f32::INFINITY, -1.0], sr, SpeechParams::default());
            assert!(a.segments.is_empty());
        }
        let bad = SpeechParams { voicing: f32::NAN, bridge_s: -1.0, min_voiced_s: f64::INFINITY, min_level_db: f32::NEG_INFINITY };
        let x: Vec<f32> = speech(2.0, SR, 0.4, |_| true).into_iter().enumerate().map(|(i, v)| if i % 97 == 0 { f32::NAN } else { v }).collect();
        let _ = detect(&x, SR as f64, bad);
        assert!(detect(&[], SR as f64, SpeechParams::default()).segments.is_empty());
    }

    #[test]
    fn block_size_does_not_change_the_result() {
        let x = speech(6.0, SR, 0.3, |t| (0.5..2.0).contains(&t) || (3.0..5.0).contains(&t));
        let whole = detect(&x, SR as f64, SpeechParams::default());
        let mut d = SpeechDetector::new(SR as f64, SpeechParams::default());
        for c in x.chunks(1234) {
            d.process(c);
        }
        assert_eq!(d.finish(), whole);
    }
}
