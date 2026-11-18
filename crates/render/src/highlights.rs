//! Highlight cues: where the interesting moments of long single-camera footage are (#21).
//!
//! Pure analysis of a mono signal and a series of small frames, for `story.highlights`. Each cue is
//! a published, well-known method implemented from its description; none of them understands what
//! it hears or sees, so each is reported as the cue it is:
//!
//! - **Impacts** (a paddle or racket hit, a ball bounce, a clap): onsets of the log-compressed
//!   *spectral flux*, the half-wave rectified frame-to-frame increase of the magnitude spectrum
//!   (Bello et al., "A Tutorial on Onset Detection in Music Signals", IEEE TSAP 2005; Dixon,
//!   "Onset Detection Revisited", DAFx 2006), picked as local maxima above a moving mean plus a
//!   multiple of the moving deviation. An onset counts as an *impact* when its energy rises by at
//!   least 6 dB over the moment before and falls by as much within 80 ms: short percussive
//!   sounds do, a voice, a note or a sound that stops does not.
//!   It cannot tell a paddle from a door slam, a dropped ball or a clap.
//! - **Cheering / applause**: a sustained rise of the median level over a low-percentile
//!   baseline of the surrounding 20 s, with a high spectral flatness (Wiener entropy: the ratio
//!   of the geometric to the arithmetic mean of the power spectrum; noise-like sound is near
//!   0.5, tones and most speech far lower). It cannot tell a cheer from wind, traffic, rain or a
//!   crowd talking loudly, and misses a quiet crowd.
//! - **Motion**: the mean absolute difference of the luma of successive sampled frames on a
//!   128×72 grid. It measures how much of the picture changes, not what moves: a camera pan
//!   or a lighting change reads as motion, a small fast ball barely does.
//! - **Scoreboard changes (heuristic)**: a cell of the frame (one of 16×9) that holds still
//!   with fine detail for most of the footage (an overlay, a scoreboard, a clock) and then
//!   changes for a moment while most other still cells do not. It does not read digits, and
//!   misses scoreboards that move, fade or sit on moving video; a channel logo that animates
//!   reads as a change.
//!
//! - **Speech** ([`filmagic_audio_dsp::speech`]): where someone talks, so that a suggested In or
//!   Out does not cut a word (a player's call, a commentator) in half.
//!
//! [`find_highlights`] turns the cues into ranked segments: rallies (runs of impacts at a rally's
//! cadence) and peaks of the combined score, each with a suggested In and Out that does not cut
//! into a rally or a sentence and lands where the picture is quietest nearby.

use filmagic_audio_dsp::fft::Fft;
use filmagic_audio_dsp::speech::{SpeechDetector, SpeechParams};

/// Analysis sample rate of the sound, Hz (sounds are read already decimated to it).
pub const AUDIO_RATE: u32 = 16_000;
/// FFT size and hop, samples (32 ms frames, 16 ms apart).
const N: usize = 512;
const HOP: usize = 256;
/// Seconds between analysis frames.
pub const HOP_SECONDS: f64 = HOP as f64 / AUDIO_RATE as f64;
/// Luma grid of the picture analysis.
pub const GRID_W: usize = 128;
pub const GRID_H: usize = 72;
/// Scoreboard cells (each 8×8 of the grid).
const CELLS_X: usize = 16;
const CELLS_Y: usize = 9;
const CELL: usize = 8;
const CELLS: usize = CELLS_X * CELLS_Y;

/// One onset of the sound.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Onset {
    /// Seconds from the start of the analysed span.
    pub t: f64,
    /// How far above its local threshold the flux peak rose (1 = at the threshold).
    pub ratio: f32,
    /// Fall of the energy 80 ms after the onset, dB.
    pub decay_db: f32,
    /// A short percussive sound (decays by 6 dB or more within 80 ms).
    pub impact: bool,
    /// 0..1: how clear an impact it is.
    pub score: f32,
}

/// What the sound gave.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AudioCues {
    /// Level of each analysis frame, dBFS.
    pub level_db: Vec<f32>,
    /// Cheer strength of each analysis frame, 0..1.
    pub cheer: Vec<f32>,
    pub onsets: Vec<Onset>,
    /// Sustained cheering: (start, end, rise over the baseline in dB), seconds.
    pub cheers: Vec<(f64, f64, f32)>,
    /// Speech: (start, end), seconds.
    pub speech: Vec<(f64, f64)>,
}

/// A scoreboard-like change of the picture (heuristic).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoardChange {
    pub t: f64,
    /// Still, detailed cells that changed together.
    pub cells: usize,
}

/// What the picture gave.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PictureCues {
    /// Seconds of each sample.
    pub times: Vec<f64>,
    /// Motion from the previous sample, 0..1 of full-scale luma (0 for the first).
    pub motion: Vec<f32>,
    pub board: Vec<BoardChange>,
}

/// Every cue of one analysed span.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cues {
    /// Seconds analysed.
    pub span: f64,
    pub audio: Option<AudioCues>,
    pub picture: Option<PictureCues>,
}

// ------------------------------------------------------------------ sound

/// Streaming sound analysis: push mono samples at [`AUDIO_RATE`], then [`AudioAnalyser::finish`].
pub struct AudioAnalyser {
    fft: Fft,
    window: Vec<f32>,
    norm: f32,
    pending: Vec<f32>,
    prev: Vec<f32>,
    have_prev: bool,
    flux: Vec<f32>,
    level_db: Vec<f32>,
    flatness: Vec<f32>,
    re: Vec<f32>,
    im: Vec<f32>,
    /// Frames analysed at most (the caller's cap on the span); later samples are ignored.
    max_frames: usize,
    speech: SpeechDetector,
}

/// Bins of the band read (150 Hz to 7 kHz).
fn band() -> (usize, usize) {
    let bin = AUDIO_RATE as f64 / N as f64;
    ((150.0 / bin).ceil() as usize, ((7000.0 / bin).floor() as usize).min(N / 2))
}

impl AudioAnalyser {
    /// An analyser for at most `max_seconds` of sound.
    pub fn new(max_seconds: f64) -> AudioAnalyser {
        let window: Vec<f32> = (0..N).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / N as f32).cos()).collect();
        let norm = window.iter().sum::<f32>() / 2.0;
        let max_frames = if max_seconds.is_finite() && max_seconds > 0.0 { (max_seconds / HOP_SECONDS).ceil().min(1e9) as usize + 1 } else { 0 };
        AudioAnalyser {
            fft: Fft::new(N),
            window,
            norm,
            pending: Vec::with_capacity(N * 2),
            prev: vec![0.0; N / 2 + 1],
            have_prev: false,
            flux: Vec::new(),
            level_db: Vec::new(),
            flatness: Vec::new(),
            re: vec![0.0; N],
            im: vec![0.0; N],
            max_frames,
            speech: SpeechDetector::new(f64::from(AUDIO_RATE), SpeechParams::default()),
        }
    }

    pub fn push(&mut self, samples: &[f32]) {
        for chunk in samples.chunks(HOP) {
            if self.flux.len() >= self.max_frames {
                return;
            }
            let start = self.pending.len();
            self.pending.extend(chunk.iter().map(|x| if x.is_finite() { x.clamp(-4.0, 4.0) } else { 0.0 }));
            if let Some(new) = self.pending.get(start..) {
                self.speech.process(new);
            }
            while self.pending.len() >= N && self.flux.len() < self.max_frames {
                self.frame();
                self.pending.drain(..HOP);
            }
        }
    }

    fn frame(&mut self) {
        let (lo, hi) = band();
        let mut energy = 0.0f64;
        for i in 0..N {
            let x = self.pending.get(i).copied().unwrap_or(0.0);
            energy += f64::from(x) * f64::from(x);
            self.re[i] = x * self.window[i];
            self.im[i] = 0.0;
        }
        self.fft.forward(&mut self.re, &mut self.im);
        let mut flux = 0.0f32;
        let (mut log_sum, mut lin_sum) = (0.0f64, 0.0f64);
        for k in 0..=N / 2 {
            let mag = (self.re[k] * self.re[k] + self.im[k] * self.im[k]).sqrt() / self.norm;
            let l = (1.0 + 100.0 * mag).ln();
            if k >= lo && k <= hi {
                if self.have_prev {
                    flux += (l - self.prev[k]).max(0.0);
                }
                let p = f64::from(mag) * f64::from(mag) + 1e-12;
                log_sum += p.ln();
                lin_sum += p;
            }
            self.prev[k] = l;
        }
        let nb = (hi + 1 - lo).max(1) as f64;
        self.have_prev = true;
        self.flux.push(flux / nb as f32);
        self.level_db.push((10.0 * (energy / N as f64 + 1e-12).log10()) as f32);
        self.flatness.push(((log_sum / nb).exp() / (lin_sum / nb)).clamp(0.0, 1.0) as f32);
    }

    pub fn finish(self) -> AudioCues {
        let onsets = pick_onsets(&self.flux, &self.level_db);
        let cheer = cheer_curve(&self.level_db, &self.flatness);
        let cheers = cheer_events(&cheer, &self.level_db);
        let speech = self.speech.finish().segments.iter().map(|g| (g.start_s, g.end_s)).collect();
        AudioCues { level_db: self.level_db, cheer, onsets, cheers, speech }
    }
}

/// Moving mean and deviation of `x` over `[i - before, i + after]`, from prefix sums.
fn moving_stats(x: &[f32], before: usize, after: usize) -> Vec<(f32, f32)> {
    let mut s1 = vec![0.0f64; x.len() + 1];
    let mut s2 = vec![0.0f64; x.len() + 1];
    for (i, v) in x.iter().enumerate() {
        s1[i + 1] = s1[i] + f64::from(*v);
        s2[i + 1] = s2[i] + f64::from(*v) * f64::from(*v);
    }
    (0..x.len())
        .map(|i| {
            let a = i.saturating_sub(before);
            let b = (i + after + 1).min(x.len());
            let n = (b - a).max(1) as f64;
            let m = (s1[b] - s1[a]) / n;
            let var = ((s2[b] - s2[a]) / n - m * m).max(0.0);
            (m as f32, var.sqrt() as f32)
        })
        .collect()
}

fn pick_onsets(flux: &[f32], level: &[f32]) -> Vec<Onset> {
    // a second either side for the threshold, ±48 ms for the local maximum, 80 ms apart at least
    let w = (1.0 / HOP_SECONDS).round() as usize;
    let stats = moving_stats(flux, w, w);
    let local = 3usize;
    let decay_frames = (0.08 / HOP_SECONDS).round() as usize;
    let mut out: Vec<Onset> = Vec::new();
    for i in 1..flux.len() {
        let f = flux[i];
        let (m, sd) = stats[i];
        let thr = m + 1.5 * sd + 0.02;
        if f < thr || level.get(i).is_none_or(|l| *l < -60.0) {
            continue;
        }
        let a = i.saturating_sub(local);
        let b = (i + local + 1).min(flux.len());
        if flux[a..b].iter().any(|v| *v > f) || flux[a..i].contains(&f) {
            continue;
        }
        // the peak level just after the onset, against the level 80 ms on
        let peak = level.get(i..(i + 3).min(level.len())).map_or(-120.0, |s| s.iter().copied().fold(-120.0f32, f32::max));
        let later = level.get(i + decay_frames..(i + decay_frames + 3).min(level.len())).filter(|s| !s.is_empty());
        let decay_db = later.map_or(24.0, |s| peak - s.iter().sum::<f32>() / s.len() as f32);
        // and against the level just before it: a sound that stops is no hit
        let pre = level.get(i.saturating_sub(5)..i.saturating_sub(1)).filter(|s| !s.is_empty()).map_or(-120.0, |s| s.iter().sum::<f32>() / s.len() as f32);
        let ratio = f / thr.max(1e-6);
        let impact = decay_db >= 6.0 && peak - pre >= 6.0;
        let score = if impact { (0.5 * ((ratio - 1.0) / 2.0).clamp(0.0, 1.0) + 0.5 * (decay_db / 18.0).clamp(0.0, 1.0)).clamp(0.05, 1.0) } else { 0.0 };
        let t = i as f64 * HOP_SECONDS;
        match out.last_mut() {
            Some(prev) if t - prev.t < 0.08 => {
                if ratio > prev.ratio {
                    *prev = Onset { t, ratio, decay_db, impact, score };
                }
            }
            _ => out.push(Onset { t, ratio, decay_db, impact, score }),
        }
    }
    out
}

/// The `q` quantile (0..1) of `v` (sorted in place).
fn quantile(v: &mut [f32], q: f64) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f32::total_cmp);
    let i = ((v.len() - 1) as f64 * q.clamp(0.0, 1.0)).round() as usize;
    v.get(i).copied().unwrap_or(0.0)
}

fn cheer_curve(level: &[f32], flat: &[f32]) -> Vec<f32> {
    let n = level.len();
    if n == 0 {
        return Vec::new();
    }
    let half = (0.25 / HOP_SECONDS).round() as usize;
    // the median level of half a second: a click or a hit does not lift it, a crowd does
    let step = (0.1 / HOP_SECONDS).round().max(1.0) as usize;
    let mut med = vec![0.0f32; n];
    let mut flat_m = vec![0.0f32; n];
    let mut buf: Vec<f32> = Vec::with_capacity(2 * half + 1);
    let mut i = 0;
    while i < n {
        let (a, b) = (i.saturating_sub(half), (i + half + 1).min(n));
        buf.clear();
        buf.extend_from_slice(&level[a..b]);
        let m = quantile(&mut buf, 0.5);
        buf.clear();
        buf.extend_from_slice(&flat[a..b]);
        let fm = quantile(&mut buf, 0.5);
        for j in i..(i + step).min(n) {
            med[j] = m;
            flat_m[j] = fm;
        }
        i += step;
    }
    // the baseline: a low percentile of the median level over ±10 s, sampled every half second
    let long = (10.0 / HOP_SECONDS).round() as usize;
    let bstep = (0.5 / HOP_SECONDS).round().max(1.0) as usize;
    let mut base = vec![-120.0f32; n];
    let mut i = 0;
    while i < n {
        let (a, b) = (i.saturating_sub(long), (i + long + 1).min(n));
        buf.clear();
        buf.extend(med[a..b].iter().step_by(4));
        let q = quantile(&mut buf, 0.2);
        for j in i..(i + bstep).min(n) {
            base[j] = q;
        }
        i += bstep;
    }
    (0..n)
        .map(|j| {
            let rise = med[j] - base[j];
            let loud = ((rise - 6.0) / 12.0).clamp(0.0, 1.0);
            let noisy = ((flat_m[j] - 0.2) / 0.25).clamp(0.0, 1.0);
            if med[j] < -60.0 { 0.0 } else { loud * noisy }
        })
        .collect()
}

fn cheer_events(cheer: &[f32], level: &[f32]) -> Vec<(f64, f64, f32)> {
    let min_len = (0.6 / HOP_SECONDS).round() as usize;
    let mut out = Vec::new();
    let mut i = 0;
    while i < cheer.len() {
        if cheer[i] < 0.2 {
            i += 1;
            continue;
        }
        let a = i;
        while i < cheer.len() && cheer[i] >= 0.2 {
            i += 1;
        }
        if i - a >= min_len {
            // the rise, read back from the strength: 6 dB at 0, 18 dB at full
            let peak = cheer[a..i].iter().copied().fold(0.0f32, f32::max);
            let _ = level;
            out.push((a as f64 * HOP_SECONDS, i as f64 * HOP_SECONDS, 6.0 + 12.0 * peak));
        }
    }
    out
}

// ---------------------------------------------------------------- picture

/// The luma of an RGBA8 frame averaged onto the [`GRID_W`]×[`GRID_H`] grid (0..255).
pub fn luma_grid(rgba: &[u8], w: usize, h: usize) -> Option<Vec<u8>> {
    let px = w.checked_mul(h)?;
    if px == 0 || rgba.len() < px.checked_mul(4)? {
        return None;
    }
    let mut sum = vec![0u32; GRID_W * GRID_H];
    let mut cnt = vec![0u32; GRID_W * GRID_H];
    // at most about 4 samples per grid cell in each direction: big frames are read sparsely
    let sx = (w / (GRID_W * 4)).max(1);
    let sy = (h / (GRID_H * 4)).max(1);
    for y in (0..h).step_by(sy) {
        let gy = (y * GRID_H / h).min(GRID_H - 1);
        for x in (0..w).step_by(sx) {
            let gx = (x * GRID_W / w).min(GRID_W - 1);
            let o = (y * w + x) * 4;
            let (r, g, b) = (u32::from(rgba[o]), u32::from(rgba[o + 1]), u32::from(rgba[o + 2]));
            let i = gy * GRID_W + gx;
            sum[i] += (54 * r + 183 * g + 19 * b) >> 8;
            cnt[i] += 1;
        }
    }
    Some(sum.iter().zip(&cnt).map(|(s, c)| (s / (*c).max(1)).min(255) as u8).collect())
}

/// Streaming picture analysis: push sampled frames in time order, then [`PictureAnalyser::finish`].
#[derive(Default)]
pub struct PictureAnalyser {
    prev: Option<Vec<u8>>,
    times: Vec<f64>,
    motion: Vec<f32>,
    /// Per sample and cell: change from the previous sample and detail, luma units (0..255).
    change: Vec<[u8; CELLS]>,
    detail: Vec<[u8; CELLS]>,
}

impl PictureAnalyser {
    pub fn new() -> PictureAnalyser {
        PictureAnalyser::default()
    }

    /// A frame at `t` seconds; a frame that cannot be read is `None` (it repeats the last one).
    pub fn push(&mut self, t: f64, grid: Option<Vec<u8>>) {
        let Some(g) = grid.filter(|g| g.len() == GRID_W * GRID_H).or_else(|| self.prev.clone()) else {
            return;
        };
        let mut change = [0u8; CELLS];
        let mut detail = [0u8; CELLS];
        let mut total = 0u64;
        for cy in 0..CELLS_Y {
            for cx in 0..CELLS_X {
                let (mut d, mut s1, mut s2) = (0u32, 0u32, 0u32);
                for y in 0..CELL {
                    for x in 0..CELL {
                        let i = (cy * CELL + y) * GRID_W + cx * CELL + x;
                        let v = u32::from(g[i]);
                        s1 += v;
                        s2 += v * v;
                        if let Some(p) = &self.prev {
                            d += v.abs_diff(u32::from(p[i]));
                        }
                    }
                }
                let n = (CELL * CELL) as u32;
                let mean = s1 / n;
                let var = (s2 / n).saturating_sub(mean * mean);
                let c = cy * CELLS_X + cx;
                change[c] = (d / n).min(255) as u8;
                detail[c] = ((var as f64).sqrt() as u32).min(255) as u8;
                total += u64::from(d);
            }
        }
        let motion = if self.prev.is_some() { (total as f64 / (GRID_W * GRID_H) as f64 / 255.0) as f32 } else { 0.0 };
        self.times.push(t);
        self.motion.push(motion);
        self.change.push(change);
        self.detail.push(detail);
        self.prev = Some(g);
    }

    pub fn finish(self) -> PictureCues {
        let board = board_changes(&self.times, &self.change, &self.detail);
        PictureCues { times: self.times, motion: self.motion, board }
    }
}

fn board_changes(times: &[f64], change: &[[u8; CELLS]], detail: &[[u8; CELLS]]) -> Vec<BoardChange> {
    let n = times.len();
    if n < 20 {
        return Vec::new();
    }
    // cells that hold still with fine detail for most of the footage
    let still: Vec<usize> = (0..CELLS)
        .filter(|&c| {
            let calm = change.iter().skip(1).filter(|ch| ch[c] <= 1).count();
            let fine = detail.iter().filter(|d| d[c] >= 12).count();
            calm * 10 >= (n - 1) * 8 && fine * 10 >= n * 8
        })
        .collect();
    if still.is_empty() {
        return Vec::new();
    }
    let rate = (n as f64 - 1.0) / (times[n - 1] - times[0]).max(1e-6);
    let calm_span = (rate * 1.0).ceil().max(2.0) as usize;
    let mut out: Vec<BoardChange> = Vec::new();
    for i in 1..n {
        let changed: Vec<usize> = still.iter().copied().filter(|&c| change[i][c] >= 6).collect();
        // a cut or a flash changes everything at once: not a scoreboard
        if changed.is_empty() || changed.len() * 2 > still.len().max(2) && still.len() > 3 {
            continue;
        }
        // calm before and after, apart from this sample and the next
        let calm = |c: usize, r: std::ops::Range<usize>| r.filter_map(|j| change.get(j)).all(|ch| ch[c] <= 2);
        let real: usize = changed.iter().filter(|&&c| calm(c, i.saturating_sub(calm_span)..i) && calm(c, i + 2..(i + 2 + calm_span).min(n))).count();
        if real == 0 || i + 2 + calm_span > n || i < calm_span {
            continue;
        }
        match out.last_mut() {
            Some(prev) if times[i] - prev.t < 1.0 => prev.cells = prev.cells.max(real),
            _ => out.push(BoardChange { t: times[i], cells: real }),
        }
    }
    out
}

// ------------------------------------------------------------ highlights

/// How [`find_highlights`] reads the cues.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Options {
    /// Rallies only, peaks only, or both (rallies first).
    pub mode: Mode,
    /// Gaps between the impacts of a rally, seconds.
    pub cadence: (f64, f64),
    /// Impacts a rally has at least.
    pub min_hits: usize,
    /// Segment length, seconds.
    pub min_len: f64,
    pub max_len: f64,
    /// Lead before a rally's first impact and tail after its last, seconds.
    pub lead: f64,
    pub tail: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Auto,
    Rally,
    Peaks,
}

impl Default for Options {
    fn default() -> Self {
        Options { mode: Mode::Auto, cadence: (0.3, 2.5), min_hits: 3, min_len: 2.0, max_len: 45.0, lead: 1.0, tail: 1.5 }
    }
}

/// A found moment (seconds from the start of the analysed span).
#[derive(Clone, Debug, PartialEq)]
pub struct Highlight {
    pub start: f64,
    pub end: f64,
    /// Suggested In and Out: before the first impact and after the last (and after the cheering),
    /// at the quietest picture nearby.
    pub suggested: (f64, f64),
    /// 0..1.
    pub score: f64,
    pub rally: bool,
    pub impacts: usize,
    /// Median gap between the impacts, seconds.
    pub cadence: Option<f64>,
    /// Rise of the cheering over the baseline, dB.
    pub cheer_db: Option<f32>,
    /// Mean motion 0..1 (of the footage's busiest stretch).
    pub motion: Option<f64>,
    /// A scoreboard-like change during the moment or just after (heuristic).
    pub board: bool,
    /// Someone speaks during the moment; its suggested In and Out do not cut a word.
    pub speech: bool,
}

impl Highlight {
    /// The cues that fired.
    pub fn cues(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.impacts > 0 {
            v.push("impacts");
        }
        if self.rally {
            v.push("rally");
        }
        if self.cheer_db.is_some() {
            v.push("cheer");
        }
        if self.motion.is_some_and(|m| m >= 0.5) {
            v.push("motion");
        }
        if self.board {
            v.push("scoreboard (heuristic)");
        }
        if self.speech {
            v.push("speech");
        }
        v
    }
}

/// Motion normalised to the footage's busiest stretch (its 95th percentile), 0..1.
fn motion_norm(p: &PictureCues) -> Vec<f32> {
    let mut v: Vec<f32> = p.motion.iter().copied().skip(1).collect();
    let top = quantile(&mut v, 0.95).max(1e-3);
    p.motion.iter().map(|m| (m / top).clamp(0.0, 1.0)).collect()
}

fn mean_in(times: &[f64], vals: &[f32], a: f64, b: f64) -> Option<f64> {
    let (mut s, mut n) = (0.0, 0usize);
    for (t, v) in times.iter().zip(vals) {
        if *t >= a && *t <= b {
            s += f64::from(*v);
            n += 1;
        }
    }
    (n > 0).then(|| s / n as f64)
}

/// The time near `t` (within `reach` seconds, on the side `dir`: -1 earlier, +1 later) where the
/// picture moves least, never crossing `limit`.
fn quiet_near(p: Option<&(Vec<f64>, Vec<f32>)>, t: f64, reach: f64, dir: f64, limit: f64) -> f64 {
    let Some((times, motion)) = p else { return t };
    let (lo, hi) = if dir < 0.0 { ((t - reach).max(limit), t) } else { (t, (t + reach).min(limit)) };
    let mut best = (t, f32::INFINITY);
    for (tt, m) in times.iter().zip(motion) {
        if *tt >= lo && *tt <= hi && *m < best.1 - 1e-3 {
            best = (*tt, *m);
        }
    }
    best.0
}

/// Ranked moments of `cues` (best first).
pub fn find_highlights(cues: &Cues, o: &Options) -> Vec<Highlight> {
    let span = cues.span.max(0.0);
    let impacts: Vec<Onset> = cues.audio.as_ref().map(|a| a.onsets.iter().copied().filter(|x| x.impact).collect()).unwrap_or_default();
    let cheers: Vec<(f64, f64, f32)> = cues.audio.as_ref().map(|a| a.cheers.clone()).unwrap_or_default();
    let motion = cues.picture.as_ref().map(|p| (p.times.clone(), motion_norm(p)));
    let board: Vec<f64> = cues.picture.as_ref().map(|p| p.board.iter().map(|b| b.t).collect()).unwrap_or_default();
    let speech: Vec<(f64, f64)> = cues.audio.as_ref().map(|a| a.speech.clone()).unwrap_or_default();
    let mut found: Vec<Highlight> = Vec::new();

    let describe = |start: f64, end: f64, rally: bool, hits: &[Onset]| -> Highlight {
        let cheer = cheers.iter().filter(|c| c.0 < end + 3.0 && c.1 > start).map(|c| c.2).fold(None, |a: Option<f32>, b| Some(a.map_or(b, |x| x.max(b))));
        let mot = motion.as_ref().and_then(|(t, m)| mean_in(t, m, start, end));
        let has_board = board.iter().any(|b| *b >= start && *b <= end + 5.0);
        let gaps: Vec<f64> = hits.windows(2).map(|w| w[1].t - w[0].t).collect();
        let cadence = (!gaps.is_empty()).then(|| {
            let mut g: Vec<f32> = gaps.iter().map(|x| *x as f32).collect();
            f64::from(quantile(&mut g, 0.5))
        });
        let hit_score = hits.iter().map(|h| f64::from(h.score)).sum::<f64>();
        let mut score = 0.0f64;
        let mut weight = 0.0f64;
        if cues.audio.is_some() {
            score += 0.45 * (hit_score / 8.0).min(1.0) + 0.3 * cheer.map_or(0.0, |c| f64::from((c - 6.0) / 12.0).clamp(0.2, 1.0));
            weight += 0.75;
        }
        if let Some(m) = mot {
            score += 0.2 * m;
            weight += 0.2;
        }
        if has_board {
            score += 0.1;
        }
        weight += 0.05;
        let score = (score / weight.max(1e-9)).clamp(0.0, 1.0);
        Highlight {
            start,
            end,
            suggested: (start, end),
            score,
            rally,
            impacts: hits.len(),
            cadence,
            cheer_db: cheer,
            motion: mot,
            board: has_board,
            speech: false,
        }
    };

    // rallies: runs of impacts at a rally's cadence
    if o.mode != Mode::Peaks {
        let mut i = 0;
        while i < impacts.len() {
            let mut j = i;
            while j + 1 < impacts.len() {
                let gap = impacts[j + 1].t - impacts[j].t;
                if gap < o.cadence.0 || gap > o.cadence.1 {
                    break;
                }
                j += 1;
            }
            let run = &impacts[i..=j];
            if run.len() >= o.min_hits.max(2) {
                let (first, last) = (run[0].t, run[run.len() - 1].t);
                let mut end = last + o.tail;
                // the cheering after a rally belongs to it
                if let Some(c) = cheers.iter().find(|c| c.0 <= end + 1.0 && c.1 > last) {
                    end = end.max(c.1.min(last + 8.0));
                }
                found.push(describe((first - o.lead).max(0.0), end.min(span), true, run));
            }
            i = j + 1;
        }
    }

    // peaks of cheering or of a burst of impacts that is no rally
    if o.mode != Mode::Rally {
        for c in &cheers {
            if found.iter().any(|h| h.start < c.1 && h.end > c.0) {
                continue;
            }
            let (a, b) = ((c.0 - 3.0).max(0.0), (c.1 + 1.0).min(span));
            let hits: Vec<Onset> = impacts.iter().copied().filter(|x| x.t >= a && x.t <= b).collect();
            found.push(describe(a, b, false, &hits));
        }
        if let Some((times, m)) = &motion
            && (o.mode == Mode::Peaks || found.is_empty())
        {
            // busy stretches of picture where nothing was heard
            let mut k = 0;
            while k < m.len() {
                if m[k] < 0.7 {
                    k += 1;
                    continue;
                }
                let a = k;
                while k < m.len() && m[k] >= 0.5 {
                    k += 1;
                }
                let (ta, tb) = (times[a], times.get(k.min(m.len() - 1)).copied().unwrap_or(span));
                if tb - ta >= 1.0 && !found.iter().any(|h| h.start < tb && h.end > ta) {
                    let hits: Vec<Onset> = impacts.iter().copied().filter(|x| x.t >= ta && x.t <= tb).collect();
                    found.push(describe((ta - 0.5).max(0.0), (tb + 0.5).min(span), false, &hits));
                }
            }
        }
    }

    // lengths, In and Out
    let rallies: Vec<(f64, f64)> = found.iter().filter(|h| h.rally).map(|h| (h.start + o.lead, h.end)).collect();
    for h in &mut found {
        if h.end - h.start < o.min_len {
            let pad = (o.min_len - (h.end - h.start)) / 2.0;
            h.start = (h.start - pad).max(0.0);
            h.end = (h.end + pad).min(span);
        }
        if h.end - h.start > o.max_len {
            h.end = h.start + o.max_len;
        }
        // never into another rally: the previous one's end and the next one's first impact bound the reach
        let before = rallies.iter().map(|r| r.1).filter(|e| *e <= h.start + 1e-9).fold(0.0f64, f64::max);
        let after = rallies.iter().map(|r| r.0).filter(|s| *s >= h.end - 1e-9).fold(span, f64::min);
        let mut i = quiet_near(motion.as_ref(), h.start, 1.5, -1.0, before);
        let mut out = quiet_near(motion.as_ref(), h.end, 1.5, 1.0, after);
        // a cut inside a sentence moves out to its edge, up to 3 s, never into another rally
        if let Some(g) = speech.iter().find(|g| g.0 < i && i < g.1) {
            i = if i - g.0 <= 3.0 && g.0 >= before { g.0 } else { i };
        }
        if let Some(g) = speech.iter().find(|g| g.0 < out && out < g.1) {
            out = if g.1 - out <= 3.0 && g.1 <= after { g.1 } else { out };
        }
        h.speech = speech.iter().any(|g| g.0 < out && g.1 > i);
        h.suggested = (i.max(0.0).min(h.start), out.min(span).max(h.end));
    }
    found.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.start.total_cmp(&b.start)));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `seconds` of quiet noise at -50 dBFS with a click (a 3 ms burst) at each time of `clicks`,
    /// and a noise swell (a crowd) of +30 dB between `swell`.
    fn signal(seconds: f64, clicks: &[f64], swell: Option<(f64, f64)>) -> Vec<f32> {
        let n = (seconds * AUDIO_RATE as f64) as usize;
        let mut seed = 0x1234_5678u32;
        let mut noise = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed as f32 / u32::MAX as f32) * 2.0 - 1.0
        };
        let mut x: Vec<f32> = (0..n).map(|_| 0.003 * noise()).collect();
        for &c in clicks {
            let a = (c * AUDIO_RATE as f64) as usize;
            for (k, v) in x.iter_mut().skip(a).take(48).enumerate() {
                *v += 0.6 * (-(k as f32) / 10.0).exp() * if k % 2 == 0 { 1.0 } else { -1.0 };
            }
        }
        if let Some((a, b)) = swell {
            for (i, v) in x.iter_mut().enumerate() {
                let t = i as f64 / AUDIO_RATE as f64;
                if t >= a && t <= b {
                    let env = ((t - a).min(b - t) / 0.3).min(1.0) as f32;
                    *v += 0.1 * env * noise();
                }
            }
        }
        x
    }

    fn analyse(x: &[f32]) -> AudioCues {
        let mut a = AudioAnalyser::new(x.len() as f64 / AUDIO_RATE as f64 + 1.0);
        for chunk in x.chunks(4000) {
            a.push(chunk);
        }
        a.finish()
    }

    #[test]
    fn clicks_at_a_cadence_are_impacts() {
        let clicks: Vec<f64> = (0..8).map(|i| 2.0 + 0.9 * i as f64).collect();
        let a = analyse(&signal(12.0, &clicks, None));
        let hits: Vec<f64> = a.onsets.iter().filter(|o| o.impact).map(|o| o.t).collect();
        assert_eq!(hits.len(), clicks.len(), "{hits:?}");
        for (h, c) in hits.iter().zip(&clicks) {
            assert!((h - c).abs() < 0.04, "{h} vs {c}");
        }
        assert!(a.cheers.is_empty(), "clicks are no crowd: {:?}", a.cheers);
    }

    #[test]
    fn a_noise_swell_is_cheering_and_not_impacts() {
        let a = analyse(&signal(20.0, &[], Some((8.0, 12.0))));
        assert_eq!(a.cheers.len(), 1, "{:?}", a.cheers);
        let (s, e, rise) = a.cheers[0];
        assert!((7.6..=8.8).contains(&s) && (11.2..=12.4).contains(&e), "{s}..{e}");
        assert!(rise > 10.0);
        assert!(a.onsets.iter().filter(|o| o.impact).count() <= 1, "{:?}", a.onsets);
    }

    #[test]
    fn a_tone_is_neither() {
        let x: Vec<f32> =
            (0..AUDIO_RATE as usize * 6).map(|i| if (2.0..4.0).contains(&(i as f64 / 16000.0)) { 0.3 * (i as f32 * 0.2).sin() } else { 0.0 }).collect();
        let a = analyse(&x);
        assert!(a.cheers.is_empty(), "{:?}", a.cheers);
        assert!(a.onsets.iter().filter(|o| o.impact).count() == 0, "a held note does not decay: {:?}", a.onsets);
    }

    fn frame(t: f64, moving: bool, board: u8) -> Vec<u8> {
        let (w, h) = (256usize, 144usize);
        let mut px = vec![60u8; w * h * 4];
        // a detailed still overlay in the top-left corner with a "digit" that changes
        for y in 4..20 {
            for x in 4..40 {
                let v = if (x / 2 + y / 2) % 2 == 0 { 230 } else { 20 };
                let o = (y * w + x) * 4;
                px[o..o + 3].fill(v);
            }
        }
        for y in 6..18 {
            for x in 8..14 {
                let o = (y * w + x) * 4;
                px[o..o + 3].fill(board);
            }
        }
        if moving {
            let cx = (40.0 + 150.0 * (0.5 + 0.5 * (t * 3.0).sin())) as usize;
            for y in 60..110 {
                for x in cx..(cx + 40).min(w) {
                    let o = (y * w + x) * 4;
                    px[o..o + 3].fill(240);
                }
            }
        }
        px
    }

    #[test]
    fn moving_shapes_are_motion_and_a_scoreboard_change_is_found() {
        let mut p = PictureAnalyser::new();
        for i in 0..80 {
            let t = i as f64 * 0.25;
            let moving = (6.0..12.0).contains(&t);
            let board = if t < 15.0 { 230 } else { 20 };
            p.push(t, luma_grid(&frame(t, moving, board), 256, 144));
        }
        let c = p.finish();
        let busy = mean_in(&c.times, &c.motion, 6.5, 11.5).unwrap();
        let calm = mean_in(&c.times, &c.motion, 0.5, 5.5).unwrap();
        assert!(busy > 10.0 * calm.max(1e-4), "{busy} vs {calm}");
        assert_eq!(c.board.len(), 1, "{:?}", c.board);
        assert!((c.board[0].t - 15.0).abs() < 0.3, "{:?}", c.board);
    }

    #[test]
    fn rallies_are_ranked_with_their_cheering_and_suggested_cuts() {
        // a 5-hit rally at 0.8 s with cheering after, then two hits far apart, then a 3-hit rally
        let mut clicks: Vec<f64> = (0..5).map(|i| 4.0 + 0.8 * i as f64).collect();
        clicks.extend([14.0, 19.0]);
        clicks.extend((0..3).map(|i| 25.0 + 1.0 * i as f64));
        let audio = analyse(&signal(32.0, &clicks, Some((7.8, 10.5))));
        let cues = Cues { span: 32.0, audio: Some(audio), picture: None };
        let h = find_highlights(&cues, &Options::default());
        let rallies: Vec<&Highlight> = h.iter().filter(|x| x.rally).collect();
        assert_eq!(rallies.len(), 2, "{h:#?}");
        assert_eq!((h[0].impacts, h[0].rally), (5, true), "the long rally with cheering ranks first: {h:#?}");
        assert!(h[0].cheer_db.is_some() && h[0].cues().contains(&"cheer"));
        assert!((h[0].cadence.unwrap() - 0.8).abs() < 0.05);
        assert!(h[0].suggested.0 <= 4.0 - 0.5 && h[0].suggested.1 >= 10.0, "{:?}", h[0].suggested);
        let second = rallies.iter().find(|x| x.impacts == 3).unwrap();
        assert!(second.suggested.0 < 25.0 && second.suggested.1 > 27.0);
        // rally mode: the lone hits are no moment
        let only = find_highlights(&cues, &Options { mode: Mode::Rally, ..Options::default() });
        assert!(only.iter().all(|x| x.rally));
    }

    /// A voice-like sound: harmonics of 140 Hz swelling at a syllable rate of 4 Hz.
    fn voice(x: &mut [f32], a: f64, b: f64) {
        for (i, v) in x.iter_mut().enumerate() {
            let t = i as f64 / AUDIO_RATE as f64;
            if t >= a && t <= b {
                let env = 0.5 + 0.5 * (2.0 * std::f64::consts::PI * 4.0 * t).sin();
                let tone: f64 = (1..8).map(|k| (2.0 * std::f64::consts::PI * 140.0 * k as f64 * t).sin() / k as f64).sum();
                *v += (0.12 * env * tone) as f32;
            }
        }
    }

    #[test]
    fn a_cut_does_not_split_a_sentence() {
        // a rally from 4 s to 7.2 s; someone speaks from 7.6 s to 10 s, over where the Out would be
        let clicks: Vec<f64> = (0..5).map(|i| 4.0 + 0.8 * i as f64).collect();
        let mut x = signal(16.0, &clicks, None);
        voice(&mut x, 7.6, 10.0);
        let audio = analyse(&x);
        assert!(audio.speech.iter().any(|g| g.0 < 8.8 && g.1 > 9.5), "{:?}", audio.speech);
        let h = find_highlights(&Cues { span: 16.0, audio: Some(audio), picture: None }, &Options::default());
        let r = h.iter().find(|x| x.rally).unwrap();
        assert!(r.speech && r.cues().contains(&"speech"), "{r:?}");
        assert!(r.suggested.1 >= 9.7, "the Out waits for the end of the sentence: {:?}", r.suggested);
    }

    #[test]
    fn hostile_input_never_panics() {
        let mut a = AudioAnalyser::new(f64::NAN);
        a.push(&[f32::NAN; 3000]);
        assert!(a.finish().onsets.is_empty());
        let mut a = AudioAnalyser::new(2.0);
        a.push(&[f32::INFINITY, f32::NEG_INFINITY, 1e30, -1e30]);
        a.push(&vec![1.0; 100_000]);
        let c = a.finish();
        assert!(c.level_db.len() <= (2.0 / HOP_SECONDS) as usize + 2);
        assert_eq!(luma_grid(&[1, 2, 3], 10, 10), None);
        assert_eq!(luma_grid(&[], 0, 0), None);
        assert_eq!(luma_grid(&[], usize::MAX, 3), None);
        let mut p = PictureAnalyser::new();
        p.push(0.0, None);
        p.push(f64::NAN, Some(vec![1, 2]));
        let _ = p.finish();
        let _ = find_highlights(&Cues { span: f64::NAN, audio: None, picture: None }, &Options::default());
        let _ = find_highlights(&Cues::default(), &Options { cadence: (5.0, 0.1), min_hits: 0, min_len: f64::INFINITY, max_len: -1.0, ..Options::default() });
    }
}
