//! Story analysis: the measurable side of an editor's judgement about a cut.
//!
//! An editor weighs a cut on what the audience feels, whether it advances the story, whether it
//! comes at a rhythmically right moment, where the eye is looking, and whether screen direction and
//! space survive it. The first two are judgements nobody can compute. The rest leave traces in the
//! pictures and the sound, and this module measures those traces so that a person (or an agent
//! driving the editor) can decide with evidence instead of by guessing:
//!
//! - [`attention`]: where the eye most likely rests in a frame (a saliency centroid), so the
//!   distance the eye has to travel across a cut can be reported;
//! - [`drift`]: the dominant on-screen motion between two frames, so a reversal of screen direction
//!   across a cut can be reported;
//! - [`Change`]: how much the picture changes at a cut. A change that is neither small (the same
//!   context continues) nor large (a new context is read at once) is the jarring middle band of the
//!   jump cut;
//! - [`rhythm`] and [`Pace`]: cut rate and shot lengths, set against the range of human blink rates
//!   (roughly 4 to 40 a minute) that cutting rates fall into, from quiet dialogue to action;
//! - [`pauses`]: the ends of phrases in a sound track. A listener blinks, and a cut sits well,
//!   where a thought completes;
//! - [`settles`]: where a movement in the picture completes;
//! - [`medoid`] and [`contact_sheet`]: one representative frame per shot, laid out to be read like
//!   a page.
//!
//! Everything here is deterministic and works on reduced pictures; nothing learns or guesses.

use crate::Image;
use crate::scene::{Signature, difference};

/// Width of a [`Glance`] in cells.
pub const GW: usize = 64;
/// Height of a [`Glance`] in cells.
pub const GH: usize = 36;

/// A frame reduced to a `GW`×`GH` grid of display-referred luma (0…1).
#[derive(Clone, Debug, PartialEq)]
pub struct Glance {
    luma: Vec<f32>,
}

impl Glance {
    /// Reduce an RGBA8 frame (`w`×`h`, row-major, 4 bytes per pixel). A frame that is empty or
    /// shorter than its stated size gives a black glance.
    pub fn from_rgba(rgba: &[u8], w: usize, h: usize) -> Glance {
        let mut luma = vec![0f32; GW * GH];
        let needed = w.checked_mul(h).and_then(|n| n.checked_mul(4));
        if w == 0 || h == 0 || needed.is_none_or(|n| rgba.len() < n) {
            return Glance { luma };
        }
        let mut count = vec![0u32; GW * GH];
        // at most ~256×144 samples: plenty for a 64×36 grid, cheap for 4K frames
        let (step_x, step_y) = ((w / 256).max(1), (h / 144).max(1));
        for y in (0..h).step_by(step_y) {
            let gy = y * GH / h;
            for x in (0..w).step_by(step_x) {
                let Some(p) = rgba.get((y * w + x) * 4..(y * w + x) * 4 + 3) else { continue };
                let gi = gy * GW + x * GW / w;
                if let (Some(l), Some(c)) = (luma.get_mut(gi), count.get_mut(gi)) {
                    *l += 0.2126 * f32::from(p[0]) + 0.7152 * f32::from(p[1]) + 0.0722 * f32::from(p[2]);
                    *c += 1;
                }
            }
        }
        for (l, c) in luma.iter_mut().zip(&count) {
            *l = if *c > 0 { *l / *c as f32 / 255.0 } else { 0.0 };
        }
        // a frame smaller than the grid leaves cells unsampled: fill each from its nearest sampled pixel
        if w < GW || h < GH {
            for gy in 0..GH {
                for gx in 0..GW {
                    if count[gy * GW + gx] == 0 {
                        let (sx, sy) = ((gx * w / GW).min(w - 1), (gy * h / GH).min(h - 1));
                        let src = (sy * GH / h) * GW + sx * GW / w;
                        luma[gy * GW + gx] = luma.get(src).copied().unwrap_or(0.0);
                    }
                }
            }
        }
        Glance { luma }
    }

    /// Luma of cell (`x`, `y`), clamped to the grid.
    fn at(&self, x: isize, y: isize) -> f32 {
        let (x, y) = (x.clamp(0, GW as isize - 1) as usize, y.clamp(0, GH as isize - 1) as usize);
        self.luma.get(y * GW + x).copied().unwrap_or(0.0)
    }

    /// Mean luma, 0…1.
    pub fn mean(&self) -> f32 {
        self.luma.iter().sum::<f32>() / (GW * GH) as f32
    }

    /// Standard deviation of luma: a plain measure of how contrasty the frame is.
    pub fn contrast(&self) -> f32 {
        let m = self.mean();
        (self.luma.iter().map(|l| (l - m) * (l - m)).sum::<f32>() / (GW * GH) as f32).sqrt()
    }
}

/// Where the eye most likely rests in a frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Attention {
    /// Horizontal position, 0 (left edge) … 1 (right edge).
    pub x: f32,
    /// Vertical position, 0 (top) … 1 (bottom).
    pub y: f32,
    /// How concentrated the salient area is, 0 (one point) … ~0.5 (spread over the whole frame).
    pub spread: f32,
    /// 0 when the frame gives the eye nothing to hold (flat or black), growing with how strongly
    /// one area stands out.
    pub strength: f32,
}

/// How much more a change between frames counts than the same difference standing still.
const MOTION_PULL: f32 = 10.0;

/// The saliency centroid of `cur`: cells that stand out from their surroundings (centre–surround
/// luma difference and local gradient) and, when the previous frame is given, cells that move.
/// Motion counts most: the eye follows what moves. A frame with nothing salient reports its
/// centre with strength 0.
pub fn attention(cur: &Glance, prev: Option<&Glance>) -> Attention {
    let mut sal = vec![0f32; GW * GH];
    for y in 0..GH as isize {
        for x in 0..GW as isize {
            let c = cur.at(x, y);
            // surround: the mean of a sparse ring three cells out (cheap centre–surround)
            let ring = [(-3, 0), (3, 0), (0, -3), (0, 3), (-2, -2), (2, -2), (-2, 2), (2, 2)];
            let surround = ring.iter().map(|(dx, dy)| cur.at(x + dx, y + dy)).sum::<f32>() / ring.len() as f32;
            let gx = cur.at(x + 1, y) - cur.at(x - 1, y);
            let gy = cur.at(x, y + 1) - cur.at(x, y - 1);
            let mut s = (c - surround).abs() + 0.5 * (gx * gx + gy * gy).sqrt();
            if let Some(p) = prev {
                s += MOTION_PULL * (c - p.at(x, y)).abs();
            }
            sal[y as usize * GW + x as usize] = s;
        }
    }
    let mean = sal.iter().sum::<f32>() / (GW * GH) as f32;
    // only what stands out above the frame's own average holds the eye; squared, so peaks lead
    let (mut sw, mut sx, mut sy) = (0f64, 0f64, 0f64);
    for (i, s) in sal.iter().enumerate() {
        let w = f64::from((s - mean).max(0.0));
        let w = w * w;
        sw += w;
        sx += w * ((i % GW) as f64 + 0.5) / GW as f64;
        sy += w * ((i / GW) as f64 + 0.5) / GH as f64;
    }
    if sw <= 1e-9 || !sw.is_finite() {
        return Attention { x: 0.5, y: 0.5, spread: 0.5, strength: 0.0 };
    }
    let (cx, cy) = (sx / sw, sy / sw);
    let mut var = 0f64;
    for (i, s) in sal.iter().enumerate() {
        let w = f64::from((s - mean).max(0.0));
        let (dx, dy) = (((i % GW) as f64 + 0.5) / GW as f64 - cx, ((i / GW) as f64 + 0.5) / GH as f64 - cy);
        var += w * w * (dx * dx + dy * dy);
    }
    let strength = (sw / (GW * GH) as f64).sqrt();
    Attention { x: cx as f32, y: cy as f32, spread: (var / sw).sqrt() as f32, strength: strength as f32 }
}

/// How far the eye travels between two attention points, 0 … 1 (1 = corner to corner).
pub fn eye_travel(a: Attention, b: Attention) -> f32 {
    let (dx, dy) = (a.x - b.x, a.y - b.y);
    ((dx * dx + dy * dy).sqrt() / std::f32::consts::SQRT_2).clamp(0.0, 1.0)
}

/// The dominant on-screen motion between two frames.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Drift {
    /// Horizontal motion as a fraction of the frame width (positive = content moves right).
    pub dx: f32,
    /// Vertical motion as a fraction of the frame height (positive = content moves down).
    pub dy: f32,
    /// 0 (no better than standing still) … 1 (the shift explains the change completely).
    pub confidence: f32,
}

/// Best shift of profile `b` against `a` (in cells, positive = `b` is `a` moved toward higher
/// indices), and how much better it fits than no shift (0…1).
fn profile_shift(a: &[f32], b: &[f32], max: isize) -> (isize, f32) {
    let n = a.len().min(b.len()) as isize;
    let err = |s: isize| -> Option<f32> {
        let (mut e, mut k) = (0f32, 0u32);
        for i in 0..n {
            let j = i + s;
            if j < 0 || j >= n {
                continue;
            }
            if let (Some(x), Some(y)) = (a.get(i as usize), b.get(j as usize)) {
                e += (x - y).abs();
                k += 1;
            }
        }
        // a shift is trusted only when most of the profile still overlaps
        (k as isize * 2 >= n && k > 0).then(|| e / k as f32)
    };
    let Some(still) = err(0) else { return (0, 0.0) };
    let (mut best, mut best_e) = (0isize, still);
    for s in 1..=max {
        for cand in [s, -s] {
            if let Some(e) = err(cand)
                && e < best_e - 1e-6
            {
                best = cand;
                best_e = e;
            }
        }
    }
    let confidence = if still > 1e-4 { (1.0 - best_e / still).clamp(0.0, 1.0) } else { 0.0 };
    (best, confidence)
}

/// The dominant motion from `a` to `b`, found by matching the frames' column and row luma
/// profiles (a pan or a large moving subject shifts the profile; noise and small movements do
/// not). Shifts up to a third of the frame are searched.
pub fn drift(a: &Glance, b: &Glance) -> Drift {
    let cols = |g: &Glance| -> Vec<f32> { (0..GW).map(|x| (0..GH).map(|y| g.at(x as isize, y as isize)).sum::<f32>() / GH as f32).collect() };
    let rows = |g: &Glance| -> Vec<f32> { (0..GH).map(|y| (0..GW).map(|x| g.at(x as isize, y as isize)).sum::<f32>() / GW as f32).collect() };
    let (sx, cx) = profile_shift(&cols(a), &cols(b), (GW / 3) as isize);
    let (sy, cy) = profile_shift(&rows(a), &rows(b), (GH / 3) as isize);
    Drift { dx: sx as f32 / GW as f32, dy: sy as f32 / GH as f32, confidence: if sx != 0 { cx } else { cy } }
}

/// Horizontal screen direction of a drift: −1 (leftward), +1 (rightward), 0 (none worth naming).
pub fn screen_direction(d: Drift) -> i8 {
    if d.confidence < 0.25 || d.dx.abs() < 1.0 / GW as f32 {
        0
    } else if d.dx > 0.0 {
        1
    } else {
        -1
    }
}

/// How much the picture changes across a cut ([`crate::scene::difference`] of the last outgoing
/// and the first incoming frame).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// Almost the same picture: the eye reads one continuing context (a match cut, or a cut
    /// inside one static setup that may not need to be a cut at all).
    Match,
    /// Something changed, but not enough to read as a new context: the jarring middle band (a
    /// slightly tighter framing of the same subject, a subject that hops within the same setup).
    Jump,
    /// A clearly new picture: the eye re-evaluates the context at once.
    Clean,
}

impl Change {
    /// Below this the two frames read as the same picture.
    pub const MATCH_BELOW: f32 = 0.035;
    /// From here up the two frames read as different pictures.
    pub const CLEAN_FROM: f32 = 0.13;

    pub fn of(diff: f32) -> Change {
        if !diff.is_finite() || diff < Self::MATCH_BELOW {
            Change::Match
        } else if diff < Self::CLEAN_FROM {
            Change::Jump
        } else {
            Change::Clean
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Change::Match => "match",
            Change::Jump => "jump",
            Change::Clean => "clean",
        }
    }
}

/// Cut rate and shot lengths of a stretch of film.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rhythm {
    pub shots: usize,
    pub cuts: usize,
    /// Total length, seconds.
    pub seconds: f64,
    pub cuts_per_minute: f64,
    /// Shot lengths, seconds.
    pub mean: f64,
    pub median: f64,
    pub shortest: f64,
    pub longest: f64,
    /// Standard deviation over mean of the shot lengths: 0 = every shot the same length (a
    /// metronome), around 1 = lengths vary as much as they average.
    pub variation: f64,
}

/// Rhythm of shots of the given lengths (seconds; non-finite and non-positive lengths are skipped).
pub fn rhythm(shot_seconds: &[f64]) -> Rhythm {
    let mut v: Vec<f64> = shot_seconds.iter().copied().filter(|s| s.is_finite() && *s > 0.0).collect();
    if v.is_empty() {
        return Rhythm { shots: 0, cuts: 0, seconds: 0.0, cuts_per_minute: 0.0, mean: 0.0, median: 0.0, shortest: 0.0, longest: 0.0, variation: 0.0 };
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    let seconds: f64 = v.iter().sum();
    let mean = seconds / n as f64;
    let median = if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 };
    let var = v.iter().map(|s| (s - mean) * (s - mean)).sum::<f64>() / n as f64;
    let cuts = n - 1;
    Rhythm {
        shots: n,
        cuts,
        seconds,
        cuts_per_minute: if seconds > 0.0 { cuts as f64 * 60.0 / seconds } else { 0.0 },
        mean,
        median,
        shortest: v[0],
        longest: v[n - 1],
        variation: if mean > 0.0 { var.sqrt() / mean } else { 0.0 },
    }
}

/// Where a cut rate sits against the range of human blink rates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pace {
    /// Under 4 cuts a minute: slower than a calm blink rate; long takes.
    Contemplative,
    /// 4 to 10: the pace of a conversation (around 6 a minute).
    Dialogue,
    /// 10 to 18: brisk.
    Brisk,
    /// 18 to 40: the pace of action (around 25 a minute).
    Action,
    /// Over 40: faster than people blink even under stress; the audience cannot re-read each shot.
    Frantic,
}

impl Pace {
    pub fn of(cuts_per_minute: f64) -> Pace {
        if !cuts_per_minute.is_finite() || cuts_per_minute < 4.0 {
            Pace::Contemplative
        } else if cuts_per_minute < 10.0 {
            Pace::Dialogue
        } else if cuts_per_minute < 18.0 {
            Pace::Brisk
        } else if cuts_per_minute <= 40.0 {
            Pace::Action
        } else {
            Pace::Frantic
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Pace::Contemplative => "contemplative",
            Pace::Dialogue => "dialogue",
            Pace::Brisk => "brisk",
            Pace::Action => "action",
            Pace::Frantic => "frantic",
        }
    }

    pub fn note(self) -> &'static str {
        match self {
            Pace::Contemplative => "under 4 cuts a minute: slower than a calm blink rate; the audience is left to look around inside each shot",
            Pace::Dialogue => "4 to 10 cuts a minute: the pace of a conversation (around 6 a minute)",
            Pace::Brisk => "10 to 18 cuts a minute: brisker than conversation, short of action",
            Pace::Action => "18 to 40 cuts a minute: the pace of action (around 25 a minute)",
            Pace::Frantic => "over 40 cuts a minute: beyond the range of blink rates; shots change faster than they can be read",
        }
    }
}

/// A pause after a phrase in a sound track.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pause {
    /// Where the phrase ends, seconds from the start of the samples.
    pub start: f64,
    /// Where sound resumes (the end of the samples for a final pause).
    pub end: f64,
    /// How far the level falls from the phrase before it, dB.
    pub drop_db: f32,
}

/// Most 10 ms steps [`pauses`] analyses (four hours of sound).
pub const MAX_PAUSE_STEPS: usize = 4 * 3600 * 100;

/// Samples in one level step (10 ms) at `rate` Hz.
pub fn level_step(rate: u32) -> usize {
    (rate / 100).max(1) as usize
}

/// The level of mono samples in dB, one value per whole 10 ms step ([`level_step`] samples; a
/// trailing partial step is dropped). Non-finite samples count as silence. Sound can be fed in
/// pieces whose lengths are multiples of the step and the results concatenated.
pub fn levels_db(mono: &[f32], rate: u32) -> Vec<f32> {
    if rate == 0 {
        return Vec::new();
    }
    mono.chunks_exact(level_step(rate))
        .map(|w| {
            let ms = w.iter().map(|s| if s.is_finite() { f64::from(*s) * f64::from(*s) } else { 0.0 }).sum::<f64>() / w.len().max(1) as f64;
            (10.0 * ms.max(1e-10).log10()) as f32
        })
        .collect()
}

/// The pauses that follow phrases in mono samples at `rate` Hz: [`pauses_in_levels`] of
/// [`levels_db`].
pub fn pauses(mono: &[f32], rate: u32, min_pause: f64, min_phrase: f64) -> Vec<Pause> {
    if rate == 0 {
        return Vec::new();
    }
    pauses_in_levels(&levels_db(mono, rate), level_step(rate) as f64 / f64::from(rate), min_pause, min_phrase)
}

/// The pauses that follow phrases in a series of levels (dB, one every `step_s` seconds):
/// stretches of at least `min_pause` seconds below the track's own speech level that come after
/// at least `min_phrase` seconds of sound. The threshold adapts to the track (its quiet floor and
/// its loud level), so a noisy location track and a clean voice-over are read alike. Sound with
/// less than 9 dB between its quiet and its loud moments has no phrasing to find and returns
/// nothing. At most [`MAX_PAUSE_STEPS`] levels are read.
pub fn pauses_in_levels(levels: &[f32], step_s: f64, min_pause: f64, min_phrase: f64) -> Vec<Pause> {
    let steps = levels.len().min(MAX_PAUSE_STEPS);
    if steps < 20 || !step_s.is_finite() || step_s <= 0.0 {
        return Vec::new();
    }
    let db: Vec<f32> = levels.iter().take(steps).map(|d| if d.is_finite() { *d } else { -100.0 }).collect();
    let mut sorted = db.clone();
    sorted.sort_by(f32::total_cmp);
    let pick = |q: f64| sorted.get(((steps - 1) as f64 * q) as usize).copied().unwrap_or(-100.0);
    let (floor, loud) = (pick(0.10), pick(0.95));
    if loud - floor < 9.0 {
        return Vec::new();
    }
    let threshold = floor + 0.35 * (loud - floor);
    let mut voiced: Vec<bool> = db.iter().map(|d| *d > threshold).collect();
    // close gaps shorter than 60 ms inside a phrase (stop consonants, breaths between syllables)
    let bridge = (0.06 / step_s).round().max(1.0) as usize;
    let mut i = 0;
    while i < steps {
        if !voiced[i] {
            let j = (i..steps).find(|k| voiced[*k]).unwrap_or(steps);
            if i > 0 && j < steps && j - i <= bridge {
                voiced[i..j].iter_mut().for_each(|v| *v = true);
            }
            i = j;
        } else {
            i += 1;
        }
    }
    // `as usize` saturates: an infinite minimum is longer than any run, a NaN one is zero
    let need = |seconds: f64| ((seconds.max(0.0) / step_s).ceil() as usize).max(1);
    let (need_pause, need_phrase) = (need(min_pause), need(min_phrase));
    let mut out = Vec::new();
    let mut i = 0;
    while i < steps {
        // a phrase [i, j) …
        if !voiced[i] {
            i += 1;
            continue;
        }
        let j = (i..steps).find(|k| !voiced[*k]).unwrap_or(steps);
        // … and the pause [j, k) after it
        let k = (j..steps).find(|m| voiced[*m]).unwrap_or(steps);
        if j - i >= need_phrase && k - j >= need_pause {
            let mean = |a: usize, b: usize| db.get(a..b).filter(|s| !s.is_empty()).map_or(-100.0, |s| s.iter().sum::<f32>() / s.len() as f32);
            out.push(Pause { start: j as f64 * step_s, end: k as f64 * step_s, drop_db: mean(i, j) - mean(j, k) });
        }
        i = k.max(j + 1);
    }
    out
}

/// Where movement completes in a series of frame-to-frame activity values (e.g.
/// [`crate::scene::difference`] of consecutive frames): after each burst of activity, the first
/// sample where it has died down to about a third of the burst and stopped falling. Returns
/// (index, strength 0…1), the strength relative to the strongest burst in the series.
pub fn settles(activity: &[f32]) -> Vec<(usize, f32)> {
    let n = activity.len();
    if n < 5 {
        return Vec::new();
    }
    let clean = |i: usize| activity.get(i).copied().filter(|v| v.is_finite()).unwrap_or(0.0).max(0.0);
    let a: Vec<f32> = (0..n).map(|i| (clean(i.saturating_sub(1)) + 2.0 * clean(i) + clean((i + 1).min(n - 1))) / 4.0).collect();
    let mut sorted = a.clone();
    sorted.sort_by(f32::total_cmp);
    let median = sorted[n / 2];
    let top = sorted[n - 1];
    let floor = (2.5 * median).max(0.004);
    if top < floor {
        return Vec::new();
    }
    let mut out: Vec<(usize, f32)> = Vec::new();
    let mut i = 1;
    while i + 1 < n {
        let peak = a[i] >= floor && a[i] >= a[i - 1] && a[i] > a[i + 1];
        if !peak {
            i += 1;
            continue;
        }
        let p = a[i];
        let rest = (i + 1..n).find(|j| a[*j] <= 0.35 * p && a.get(j + 1).is_none_or(|next| *next >= a[*j] - 1e-6));
        match rest {
            Some(j) => {
                // a later, larger burst before the movement settled belongs to the same movement
                if !out.last().is_some_and(|l| l.0 == j) {
                    out.push((j, (p / top).clamp(0.0, 1.0)));
                } else if let Some(l) = out.last_mut() {
                    l.1 = l.1.max((p / top).clamp(0.0, 1.0));
                }
                i = j.max(i + 1);
            }
            None => break,
        }
    }
    out
}

/// The most representative of a set of frames: the one with the smallest total difference to all
/// the others. `None` for an empty set.
pub fn medoid(sigs: &[Signature]) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    for (i, a) in sigs.iter().enumerate() {
        let total: f32 = sigs.iter().map(|b| difference(a, b)).sum();
        if best.is_none_or(|(_, t)| total < t) {
            best = Some((i, total));
        }
    }
    best.map(|(i, _)| i)
}

/// Layout of a [`contact_sheet`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sheet {
    pub columns: usize,
    pub cell_w: usize,
    pub cell_h: usize,
    /// Space between cells and around the sheet, pixels.
    pub gap: usize,
}

/// Most cells one sheet holds.
pub const MAX_SHEET_CELLS: usize = 400;
/// Most pixels one sheet has.
pub const MAX_SHEET_PIXELS: usize = 16_000_000;

/// Lay frames out left to right, top to bottom, like the lines of a page, each fitted into its
/// cell with its label burned into the top-left corner. `None` when there is nothing to lay out
/// or the sheet would exceed [`MAX_SHEET_CELLS`] / [`MAX_SHEET_PIXELS`].
pub fn contact_sheet(cells: &[(Image, String)], s: Sheet) -> Option<Image> {
    if cells.is_empty() || cells.len() > MAX_SHEET_CELLS || s.columns == 0 || s.cell_w < 8 || s.cell_h < 8 {
        return None;
    }
    let rows = cells.len().div_ceil(s.columns);
    let w = s.columns.checked_mul(s.cell_w)?.checked_add(s.gap.checked_mul(s.columns + 1)?)?;
    let h = rows.checked_mul(s.cell_h)?.checked_add(s.gap.checked_mul(rows + 1)?)?;
    if w.checked_mul(h)? > MAX_SHEET_PIXELS {
        return None;
    }
    // a dark neutral board (display ~12 % grey), opaque
    let mut out = Image::filled(w, h, [0.013, 0.013, 0.013, 1.0]);
    if out.w != w || out.h != h {
        return None;
    }
    for (i, (img, label)) in cells.iter().enumerate() {
        let (x0, y0) = (s.gap + (i % s.columns) * (s.cell_w + s.gap), s.gap + (i / s.columns) * (s.cell_h + s.gap));
        if img.w > 0 && img.h > 0 {
            // fit inside the cell, centred, keeping the frame's shape
            let k = (s.cell_w as f32 / img.w as f32).min(s.cell_h as f32 / img.h as f32);
            let (dw, dh) = (((img.w as f32 * k).round() as usize).clamp(1, s.cell_w), ((img.h as f32 * k).round() as usize).clamp(1, s.cell_h));
            let (ox, oy) = (x0 + (s.cell_w - dw) / 2, y0 + (s.cell_h - dh) / 2);
            for y in 0..dh {
                for x in 0..dw {
                    let p = img.sample_bilinear_clamped((x as f32 + 0.5) / k - 0.5, (y as f32 + 0.5) / k - 0.5);
                    // over the board (the frame may be transparent where nothing was drawn)
                    let i4 = ((oy + y) * w + ox + x) * 4;
                    if let Some(d) = out.px.get_mut(i4..i4 + 4) {
                        let a = p[3].clamp(0.0, 1.0);
                        for c in 0..3 {
                            d[c] = p[c] + d[c] * (1.0 - a);
                        }
                    }
                }
            }
        }
        if !label.is_empty() {
            let px = (s.cell_h as f32 / 9.0).clamp(9.0, 28.0);
            let look = crate::vfx::TextLook {
                family: "Inter",
                pos: filmcraft_geom::Vec2::new(x0 as f64 + f64::from(px) * 0.5, y0 as f64 + f64::from(px) * 0.8),
                px,
                align: 0,
                color: [1.0, 1.0, 1.0, 1.0],
                box_alpha: 0.65,
            };
            crate::vfx::draw_text(&mut out, label, look);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests;
