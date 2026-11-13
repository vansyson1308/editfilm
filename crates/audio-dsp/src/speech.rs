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
//!   silence counted as −100 dBFS) and speech level (95th percentile), 30 % of the way up and at
//!   least 6 dB above the floor, and never below −60 dBFS. A clip with no pause at all (trimmed
//!   tight to the words) spans less than 20 dB: its floor is then put 30 dB under the speech.
//! * **Segments.** A frame that is loud and voiced is a speech core. Cores grow over the loud
//!   frames next to them (unvoiced consonants, up to 200 ms) and above a low threshold 3 dB over
//!   the floor; gaps shorter than [`SpeechParams::bridge_s`] (the pauses between words) are
//!   bridged, and a segment needs at least [`SpeechParams::min_voiced_s`] of voiced frames.
//! * **The level envelope's shape.** Speech rises and falls with its syllables, about 1–12
//!   times a second (the 4 Hz peak of the speech modulation spectrum, Houtgast & Steeneken), and
//!   its vowels swell in and out. Peaks are counted with a 3 dB hysteresis. A run is not speech
//!   when it is steady (level spread under 2 dB over 300 ms or more: a tone, hum or held note),
//!   when it dies away from a struck start (a car door's thunk, a footstep), or when its peaks
//!   fall more than 3.5 times slower than they rise (plucked or struck notes). Runs longer than
//!   1.5 s are also judged in 1 s windows, so music before and after the words drops out; with
//!   music under the speech, a segment's edges can stretch by up to about a second.
//!
//! Noise bursts are loud but not voiced; tones and hum are voiced but steady; thunks and plucks
//! are voiced but struck: none of them is speech. A sung melody can still read as speech; a
//! clip's Essential Sound type is the better guide there.
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
        // a frame at or under the absolute gate is never a speech core (the threshold is never
        // below it), so its voicing is not needed: silence and quiet room tone cost no search
        if e0 <= 1e-14 || x.len() < w + self.lag_max || db <= self.params.min_level_db {
            return Frame { db, voicing: 0.0 };
        }
        // prefix sums of x², so the lagged window's energy is O(1) per lag
        let mut pre = Vec::with_capacity(x.len() + 1);
        pre.push(0.0f64);
        for v in x {
            let last = pre.last().copied().unwrap_or(0.0);
            pre.push(last + (*v as f64) * (*v as f64));
        }
        let r_at = |tau: usize| -> f64 {
            let (Some(hi), Some(lo), Some(b)) = (pre.get(tau + w), pre.get(tau), x.get(tau..tau + w)) else { return 0.0 };
            let el = (hi - lo).max(0.0);
            if el <= 1e-14 {
                return 0.0;
            }
            let num: f64 = x[..w].iter().zip(b).map(|(a, b)| *a as f64 * *b as f64).sum();
            num / (e0 * el).sqrt()
        };
        // every other lag, then the neighbours of the best: the autocorrelation peak of a voiced
        // frame is several samples wide at ~8 kHz, so this finds the same peak for half the work
        let (mut best, mut at) = (0.0f64, self.lag_min);
        let mut tau = self.lag_min;
        while tau <= self.lag_max {
            let r = r_at(tau);
            if r > best {
                (best, at) = (r, tau);
            }
            tau += 2;
        }
        for t in [at.saturating_sub(1), at + 1] {
            if (self.lag_min..=self.lag_max).contains(&t) {
                best = best.max(r_at(t));
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
    let mut floor = percentile(&lv, 0.10);
    let top = percentile(&lv, 0.95);
    // A clip trimmed tight to the words, or continuous speech, has no pauses: its 10th
    // percentile is the troughs between syllables, not a noise floor. When the levels span less
    // than a speech-to-pause contrast, the floor is put that far under the speech level instead.
    if top - floor < UNIMODAL_SPAN_DB {
        floor = top - UNIMODAL_SPAN_DB - 10.0;
    }
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
    // Long runs are looked at in 1 s windows as well: a window that is clearly not speech (a
    // row of plucked notes, a held note, a decay) is cut out, so music before and after the
    // words under the same level does not join the segment.
    let mut parts: Vec<(usize, usize)> = Vec::new();
    for (a, b) in merged {
        if b - a <= SPLIT_FRAMES {
            parts.push((a, b));
            continue;
        }
        let mut keep = vec![false; b - a];
        let mut w0 = a;
        loop {
            let w1 = (w0 + WINDOW_FRAMES).min(b);
            let env: Vec<f32> = (w0..w1).map(|k| frames[k].db.max(low)).collect();
            let sh = Shape::of(&env);
            if !sh.clearly_not_speech(hop_s) {
                keep[w0 - a..w1 - a].iter_mut().for_each(|x| *x = true);
            }
            if w1 >= b {
                break;
            }
            w0 += WINDOW_HOP_FRAMES;
        }
        let mut start = None;
        for (k, on) in keep.iter().chain(std::iter::once(&false)).enumerate() {
            match (*on, start) {
                (true, None) => start = Some(a + k),
                (false, Some(s0)) => {
                    parts.push((s0, a + k));
                    start = None;
                }
                _ => {}
            }
        }
    }
    for (a, b) in parts {
        let voiced: Vec<usize> = (a..b).filter(|k| core[*k]).collect();
        if voiced.len() < min_voiced {
            continue;
        }
        // the level envelope of the whole run (floored at `low`)
        let env: Vec<f32> = (a..b).map(|k| frames[k].db.max(low)).collect();
        let shape = Shape::of(&env);
        if b - a >= STEADY_FRAMES && shape.spread < STEADY_SPREAD_DB {
            continue; // a steady tone, hum or held note
        }
        if !shape.speech_like(hop_s) {
            continue;
        }
        let spread = shape.spread;
        let mean_v = voiced.iter().map(|k| frames[*k].voicing).sum::<f32>() / voiced.len() as f32;
        let syllables = if shape.peaks >= 2 { 1.0 } else { 0.7 };
        let confidence = (mean_v * (spread / FULL_SPREAD_DB).min(1.0) * syllables).clamp(0.0, 1.0);
        let start_s = (a as f64 * hop_s + centre - hop_s / 2.0).max(0.0);
        let end_s = (b as f64 * hop_s + centre - hop_s / 2.0).min(duration_s).max(start_s);
        out.segments.push(SpeechSegment { start_s, end_s, confidence });
    }
    out
}

/// How a run's level envelope moves: what tells speech from a struck or held sound.
struct Shape {
    /// Standard deviation of the levels (dB).
    spread: f32,
    /// Syllable-like peaks: rises of at least [`PEAK_HYSTERESIS_DB`] after a fall of as much.
    peaks: usize,
    /// Frames from the start to the loudest frame, and from it to the end.
    to_max: usize,
    after_max: usize,
    /// Whether the level never rises again by [`PEAK_HYSTERESIS_DB`] after its maximum.
    decays_from_max: bool,
    /// Median over the peaks of (frames falling to the next trough) / (frames rising from the
    /// previous trough). A struck or plucked note jumps up and dies away slowly (a large ratio);
    /// a syllable swells in and out.
    fall_rise: f32,
}

/// Hysteresis of the syllable-peak count (dB).
const PEAK_HYSTERESIS_DB: f32 = 3.0;
/// A median fall/rise ratio above this is a run of struck or plucked notes.
const STRUCK_FALL_RISE: f32 = 3.5;
/// Runs longer than this (frames) are also judged in windows of [`WINDOW_FRAMES`].
const SPLIT_FRAMES: usize = 150;
const WINDOW_FRAMES: usize = 100;
const WINDOW_HOP_FRAMES: usize = 25;
/// Below this span between the 10th and 95th percentile levels there is no pause in the clip.
const UNIMODAL_SPAN_DB: f32 = 20.0;

impl Shape {
    fn of(env: &[f32]) -> Shape {
        let n = env.len();
        // 3-frame mean
        let sm: Vec<f32> = (0..n)
            .map(|k| {
                let (lo, hi) = (k.saturating_sub(1), (k + 2).min(n));
                env[lo..hi].iter().sum::<f32>() / (hi - lo).max(1) as f32
            })
            .collect();
        // alternating troughs and peaks with a 3 dB hysteresis
        let mut extrema: Vec<(usize, bool)> = vec![(0, false)]; // (frame, is_peak)
        let (mut rising, mut lo_i, mut hi_i) = (true, 0usize, 0usize);
        for (k, &v) in sm.iter().enumerate() {
            let (lo, hi) = (sm.get(lo_i).copied().unwrap_or(v), sm.get(hi_i).copied().unwrap_or(v));
            if rising {
                if v > hi {
                    hi_i = k;
                }
                if v < sm.get(hi_i).copied().unwrap_or(v) - PEAK_HYSTERESIS_DB {
                    extrema.push((hi_i, true));
                    rising = false;
                    lo_i = k;
                }
            } else {
                if v < lo {
                    lo_i = k;
                }
                if v > sm.get(lo_i).copied().unwrap_or(v) + PEAK_HYSTERESIS_DB {
                    extrema.push((lo_i, false));
                    rising = true;
                    hi_i = k;
                }
            }
            let _ = hi;
        }
        if rising {
            extrema.push((hi_i, true));
        }
        extrema.push((n.saturating_sub(1), false));
        let mut ratios: Vec<f32> = Vec::new();
        for w in extrema.windows(3) {
            if let [(a, false), (p, true), (b, false)] = w {
                let rise = p.saturating_sub(*a).max(1) as f32;
                let fall = b.saturating_sub(*p).max(1) as f32;
                ratios.push(fall / rise);
            }
        }
        let peaks = extrema.iter().filter(|e| e.1).count();
        ratios.sort_by(f32::total_cmp);
        let fall_rise = ratios.get(ratios.len() / 2).copied().unwrap_or(1.0);
        let m = sm.iter().enumerate().fold((0, f32::NEG_INFINITY), |acc, (k, v)| if *v > acc.1 { (k, *v) } else { acc }).0;
        let mut min_after = f32::INFINITY;
        let mut decays = true;
        for &v in sm.iter().skip(m) {
            min_after = min_after.min(v);
            if v > min_after + PEAK_HYSTERESIS_DB {
                decays = false;
                break;
            }
        }
        Shape { spread: spread_db(env), peaks, to_max: m, after_max: n.saturating_sub(m), decays_from_max: decays, fall_rise }
    }

    /// Positive evidence that a window is not speech: steady, dying away from a struck start,
    /// or a row of struck notes. (A window with one long vowel is not evidence either way.)
    fn clearly_not_speech(&self, hop_s: f64) -> bool {
        let n = self.to_max + self.after_max;
        (n >= STEADY_FRAMES && self.spread < STEADY_SPREAD_DB)
            || (self.decays_from_max && self.after_max as f64 * hop_s >= 0.2 && self.to_max * 3 <= self.after_max)
            || (self.peaks >= 2 && self.fall_rise > STRUCK_FALL_RISE)
    }

    /// Speech rises and falls with its syllables, about 1–12 times a second, and its vowels swell
    /// in and out. A struck sound (a car-door thunk, a footstep) jumps to its maximum and dies
    /// away; a run of plucked notes is a row of such jumps.
    fn speech_like(&self, hop_s: f64) -> bool {
        let secs = (self.to_max + self.after_max) as f64 * hop_s;
        // dies away from an early maximum: a thunk or a single struck note
        if self.decays_from_max && self.after_max as f64 * hop_s >= 0.2 && self.to_max * 3 <= self.after_max {
            return false;
        }
        if self.peaks >= 2 && self.fall_rise > STRUCK_FALL_RISE {
            return false;
        }
        if self.peaks >= 2 {
            let rate = self.peaks as f64 / secs.max(1e-3);
            return (1.0..=12.0).contains(&rate);
        }
        // one syllable: a short word
        secs <= 0.6
    }
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

    /// A car-door thunk at `at` seconds: a noise burst over a resonance at `f` Hz, both dying away.
    pub fn thunk(secs: f64, sr: u32, at: f64, f: f64, amp: f32) -> Vec<f32> {
        let mut r = Rng(5);
        (0..(secs * sr as f64) as usize)
            .map(|i| {
                let t = i as f64 / sr as f64 - at;
                if t < 0.0 {
                    return 0.0;
                }
                let n = r.sample() * 0.5 * (-t * 30.0).exp() as f32;
                let s = (2.0 * std::f64::consts::PI * f * t).sin() as f32 * (-t * 6.0).exp() as f32;
                (n + s) * amp
            })
            .collect()
    }

    /// Plucked notes (harmonics dying away, the higher faster), `rate` notes a second, each
    /// struck at once and decaying by `decay` (1/s, fundamental).
    pub fn plucked(secs: f64, sr: u32, rate: f64, decay: f64, amp: f32) -> Vec<f32> {
        let notes = [110.0, 147.0, 165.0, 196.0, 247.0, 220.0];
        (0..(secs * sr as f64) as usize)
            .map(|i| {
                let t = i as f64 / sr as f64;
                let k = (t * rate) as usize;
                let tt = t - k as f64 / rate;
                let f = notes[k % notes.len()];
                let v: f64 = (1..10).map(|h| (2.0 * std::f64::consts::PI * f * h as f64 * tt).sin() / h as f64 * (-tt * decay * h as f64).exp()).sum();
                (v * amp as f64) as f32
            })
            .collect()
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

    /// Speech with no pause at all (a clip trimmed tight to the words) has no noise floor to set
    /// the threshold from: it is still speech, with and without room tone under it.
    #[test]
    fn continuous_speech_without_pauses_is_found() {
        for secs in [0.5, 3.0, 8.0] {
            let x = speech(secs, SR, 0.3, |_| true);
            let toned = mix(&x, &noise(secs, SR, 0.0045, 3));
            for (name, sig) in [("clean", x), ("room tone", toned)] {
                let a = detect(&sig, SR as f64, SpeechParams::default());
                println!("{secs} s {name}: {:?}", a.segments);
                near(&a.segments, &[(0.0, secs)], 0.03);
                assert!(a.segments[0].confidence > 0.8, "{:?}", a.segments);
            }
        }
        // one short pause: two confident segments
        let a = detect(&speech(4.0, SR, 0.3, |t| !(2.0..2.4).contains(&t)), SR as f64, SpeechParams::default());
        near(&a.segments, &[(0.0, 2.0), (2.4, 4.0)], 0.03);
        assert!(a.segments.iter().all(|g| g.confidence > 0.8), "{:?}", a.segments);
    }

    /// The #15 case: a car door with a low thunk (a decaying 90 or 120 Hz resonance under a noise
    /// burst) is voiced but dies away from a struck start; plucked notes are a row of such
    /// starts. Neither is speech.
    #[test]
    fn thunks_and_plucked_notes_are_not_speech() {
        for (f, amp) in [(90.0, 0.2), (120.0, 0.2), (90.0, 0.9)] {
            let a = detect(&thunk(3.0, SR, 1.0, f, amp), SR as f64, SpeechParams::default());
            assert!(a.segments.is_empty(), "thunk {f} Hz: {:?}", a.segments);
        }
        for (rate, decay) in [(2.0, 1.0), (3.0, 0.7), (4.0, 1.5), (6.0, 2.0), (4.0, 4.0)] {
            let a = detect(&plucked(6.0, SR, rate, decay, 0.3), SR as f64, SpeechParams::default());
            assert!(a.segments.is_empty(), "plucked {rate}/s, decay {decay}: {:?}", a.segments);
        }
        // speech over plucked music: found, and the music before and after it is mostly left out
        // (segment edges are judged in 1 s windows, so they can stretch up to about a second)
        for rate in [2.0, 4.0, 6.0] {
            let m: Vec<f32> = plucked(8.0, SR, rate, 1.5, 0.1).iter().zip(speech(8.0, SR, 0.3, |t| (2.0..5.0).contains(&t))).map(|(a, b)| a + b).collect();
            let a = detect(&m, SR as f64, SpeechParams::default());
            println!("speech over plucked {rate}/s: {:?}", a.segments);
            assert_eq!(a.segments.len(), 1, "{:?}", a.segments);
            let g = a.segments[0];
            assert!(g.start_s <= 2.05 && g.end_s >= 4.95, "the speech is inside {g:?}");
            assert!(g.start_s >= 1.0 && g.end_s <= 6.0, "the music is mostly outside {g:?}");
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
