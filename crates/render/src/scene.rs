//! Scene Edit Detection: find the shot changes (hard cuts) in a clip.
//!
//! Each frame is reduced to a [`Signature`]: a 32×18 grid of mean luma and a 16-bin histogram per
//! RGB channel. The difference of two consecutive frames ([`difference`]) mixes the histogram
//! distance (half the L1 distance of the normalised histograms, 0…1) with the mean absolute luma
//! difference of the grids (0…1). A hard cut changes both; camera or subject motion moves the grid
//! but keeps the histogram, and a lighting change shifts the histogram smoothly over many frames.
//!
//! [`detect_cuts`] marks frame *i* as the first frame of a new shot when its difference is above an
//! absolute threshold set by the sensitivity **and** clearly above the local activity (the median of
//! the differences around it), so busy footage needs a bigger jump than static footage. Shots
//! shorter than `min_shot_frames` are merged into the previous one (flashes, single bad frames).

/// Grid size of a signature.
const GW: usize = 32;
const GH: usize = 18;
const BINS: usize = 16;

/// The reduced picture of one frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Signature {
    grid: Vec<f32>,
    hist: [f32; 3 * BINS],
}

/// Reduce an RGBA8 frame (`w`×`h`, row-major, 4 bytes per pixel) to its signature.
pub fn signature(rgba: &[u8], w: usize, h: usize) -> Signature {
    let mut grid = vec![0f32; GW * GH];
    let mut count = vec![0u32; GW * GH];
    let mut hist = [0f32; 3 * BINS];
    if w == 0 || h == 0 || rgba.len() < w * h * 4 {
        return Signature { grid, hist };
    }
    // sample at most ~256×144 pixels: plenty for both measures and cheap for 4K frames
    let step_x = (w / 256).max(1);
    let step_y = (h / 144).max(1);
    let mut n = 0u32;
    for y in (0..h).step_by(step_y) {
        let gy = y * GH / h;
        for x in (0..w).step_by(step_x) {
            let i = (y * w + x) * 4;
            let (r, g, b) = (rgba[i] as f32, rgba[i + 1] as f32, rgba[i + 2] as f32);
            let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
            let gi = gy * GW + x * GW / w;
            grid[gi] += luma;
            count[gi] += 1;
            hist[rgba[i] as usize * BINS / 256] += 1.0;
            hist[BINS + rgba[i + 1] as usize * BINS / 256] += 1.0;
            hist[2 * BINS + rgba[i + 2] as usize * BINS / 256] += 1.0;
            n += 1;
        }
    }
    for (g, c) in grid.iter_mut().zip(&count) {
        *g = if *c > 0 { *g / *c as f32 / 255.0 } else { 0.0 };
    }
    let inv = 1.0 / n.max(1) as f32;
    hist.iter_mut().for_each(|v| *v *= inv);
    Signature { grid, hist }
}

/// How different two frames look, 0 (identical) … 1.
pub fn difference(a: &Signature, b: &Signature) -> f32 {
    // per channel: half the L1 distance of two distributions is in 0..1
    let h: f32 = a.hist.iter().zip(&b.hist).map(|(x, y)| (x - y).abs()).sum::<f32>() / 6.0;
    let g: f32 = a.grid.iter().zip(&b.grid).map(|(x, y)| (x - y).abs()).sum::<f32>() / (GW * GH) as f32;
    // the grid term rarely exceeds ~0.5 even between unrelated shots: weigh it up
    (0.5 * h + 0.5 * (2.0 * g).min(1.0)).clamp(0.0, 1.0)
}

/// How alike two frames are in what is where: the correlation of their brightness patterns, −1 … 1.
/// The same setup, a little changed, stays near 1 however the exposure moved; two different
/// pictures fall toward 0 even when both are dark and [`difference`] is small. `None` when either
/// frame is too even to have a pattern (black, fog, a plain wall).
pub fn likeness(a: &Signature, b: &Signature) -> Option<f32> {
    let n = a.grid.len().min(b.grid.len());
    if n == 0 {
        return None;
    }
    let mean = |g: &[f32]| g.iter().take(n).map(|v| f64::from(*v)).sum::<f64>() / n as f64;
    let (ma, mb) = (mean(&a.grid), mean(&b.grid));
    let (mut saa, mut sbb, mut sab) = (0.0f64, 0.0f64, 0.0f64);
    for (x, y) in a.grid.iter().zip(&b.grid) {
        let (dx, dy) = (f64::from(*x) - ma, f64::from(*y) - mb);
        saa += dx * dx;
        sbb += dy * dy;
        sab += dx * dy;
    }
    // a standard deviation under 1.5% of full scale is no pattern to compare
    let floor = 0.015f64 * 0.015 * n as f64;
    if !(saa.is_finite() && sbb.is_finite() && sab.is_finite()) || saa < floor || sbb < floor {
        return None;
    }
    Some((sab / (saa * sbb).sqrt()).clamp(-1.0, 1.0) as f32)
}

/// Detection settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneOptions {
    /// 0 (only the most obvious cuts) … 100 (also subtle ones). Default 50.
    pub sensitivity: f32,
    /// Shortest shot kept, in frames (a cut closer than this to the previous one is ignored).
    pub min_shot_frames: usize,
}

impl Default for SceneOptions {
    fn default() -> Self {
        SceneOptions { sensitivity: 50.0, min_shot_frames: 6 }
    }
}

impl SceneOptions {
    /// The absolute difference a cut must exceed.
    pub fn threshold(&self) -> f32 {
        let s = self.sensitivity.clamp(0.0, 100.0) / 100.0;
        0.45 - 0.33 * s
    }
}

/// Frames that start a new shot. `diffs[i]` is the difference between frame `i - 1` and frame
/// `i` (`diffs[0]` is ignored).
pub fn detect_cuts(diffs: &[f32], opt: &SceneOptions) -> Vec<usize> {
    const WINDOW: usize = 5;
    let thr = opt.threshold();
    let mut cuts: Vec<usize> = Vec::new();
    for i in 1..diffs.len() {
        let d = diffs[i];
        if d < thr {
            continue;
        }
        // local activity: median of the neighbours (the candidate itself excluded)
        let lo = i.saturating_sub(WINDOW).max(1);
        let hi = (i + WINDOW + 1).min(diffs.len());
        let mut around: Vec<f32> = (lo..hi).filter(|&j| j != i).map(|j| diffs[j]).collect();
        let med = if around.is_empty() {
            0.0
        } else {
            around.sort_by(f32::total_cmp);
            around[around.len() / 2]
        };
        if d < 2.5 * med + 0.05 {
            continue;
        }
        // a local maximum (two-frame dissolves report their larger step once)
        if diffs.get(i + 1).is_some_and(|n| *n > d) || (i > 1 && diffs[i - 1] > d && cuts.last() != Some(&(i - 1))) {
            continue;
        }
        let since = cuts.last().copied().unwrap_or(0);
        if i - since < opt.min_shot_frames.max(1) {
            continue;
        }
        cuts.push(i);
    }
    cuts
}

/// A frame that looks like the start of a new shot but did not reach the threshold of `opt`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NearMiss {
    /// Frame index (as in [`detect_cuts`]).
    pub frame: usize,
    /// Its difference to the frame before.
    pub difference: f32,
    /// The lowest sensitivity (0…100) at which it would have been a cut.
    pub sensitivity: f32,
}

/// Frames that [`detect_cuts`] left out only because their difference is under the sensitivity's
/// threshold, although it stands far above the activity around them (at least eight times the
/// local median): two shots that look alike, such as the same sea under the same sky. They are
/// reported, not cut, so the sensitivity keeps meaning what it says; the caller can raise it.
/// A difference too small for any sensitivity is not reported.
pub fn near_misses(diffs: &[f32], opt: &SceneOptions) -> Vec<NearMiss> {
    const WINDOW: usize = 5;
    // A difference this small is the same picture shown again: 25 fps footage in a 50 fps
    // sequence, or a clip slowed to half speed, repeats every frame.
    const REPEAT: f32 = 1e-5;
    let thr = opt.threshold();
    let lowest = SceneOptions { sensitivity: 100.0, ..*opt }.threshold();
    let cuts = detect_cuts(diffs, opt);
    let gap = opt.min_shot_frames.max(1);
    // The steps at which the picture changed at all. A repeated frame says nothing about how busy
    // the footage is: counted in, half the differences around a step would be zero, the "quiet
    // stretch" would be quiet by construction, and a flash would have its two steps further apart.
    let live: Vec<usize> = (1..diffs.len()).filter(|&i| diffs.get(i).is_some_and(|d| d.is_finite() && *d > REPEAT)).collect();
    let at = |k: usize| live.get(k).and_then(|&i| diffs.get(i)).copied().unwrap_or(0.0);
    let mut out: Vec<NearMiss> = Vec::new();
    for (k, &i) in live.iter().enumerate() {
        let d = at(k);
        if d >= thr || d < lowest {
            continue;
        }
        let (lo, hi) = (k.saturating_sub(WINDOW), (k + WINDOW + 1).min(live.len()));
        let mut around: Vec<f32> = (lo..hi).filter(|&j| j != k).map(at).collect();
        around.sort_by(f32::total_cmp);
        let med = around.get(around.len() / 2).copied().unwrap_or(0.0);
        if d < 8.0 * med + 0.05 {
            continue;
        }
        // A cut is one step: the picture changes and stays changed. A photographer's flash, a
        // lightning stroke or a dropped frame changes it and changes it back, which is two steps
        // a frame or two apart, and no cut.
        let alone = (k.saturating_sub(2)..(k + 3).min(live.len())).filter(|&j| j != k).all(|j| at(j) < 0.5 * d);
        if !alone {
            continue;
        }
        if cuts.iter().any(|c| c.abs_diff(i) < gap) || out.last().is_some_and(|m| i - m.frame < gap) {
            continue;
        }
        out.push(NearMiss { frame: i, difference: d, sensitivity: ((0.45 - d) / 0.33 * 100.0).ceil().clamp(0.0, 100.0) });
    }
    out
}

/// The cuts of [`detect_cuts`] together with those between shots that look alike
/// ([`near_misses`]), in order.
pub fn cuts_with_look_alikes(diffs: &[f32], opt: &SceneOptions) -> Vec<usize> {
    let mut at = detect_cuts(diffs, opt);
    // already kept clear of the threshold's cuts and of each other by the shortest shot
    at.extend(near_misses(diffs, opt).into_iter().map(|m| m.frame));
    at.sort_unstable();
    at.dedup();
    at
}

/// Incremental detector: push frames in order, read the cuts at the end.
#[derive(Clone, Debug, Default)]
pub struct SceneDetector {
    prev: Option<Signature>,
    diffs: Vec<f32>,
}

impl SceneDetector {
    pub fn new() -> Self {
        Self::default()
    }
    /// Add the next frame; returns its difference to the previous one.
    pub fn push_rgba(&mut self, rgba: &[u8], w: usize, h: usize) -> f32 {
        self.push(signature(rgba, w, h))
    }
    pub fn push(&mut self, sig: Signature) -> f32 {
        let d = self.prev.as_ref().map(|p| difference(p, &sig)).unwrap_or(0.0);
        self.diffs.push(d);
        self.prev = Some(sig);
        d
    }
    pub fn diffs(&self) -> &[f32] {
        &self.diffs
    }
    pub fn cuts(&self, opt: &SceneOptions) -> Vec<usize> {
        detect_cuts(&self.diffs, opt)
    }
    /// Likely cuts just under the threshold of `opt` ([`near_misses`]).
    pub fn near_misses(&self, opt: &SceneOptions) -> Vec<NearMiss> {
        near_misses(&self.diffs, opt)
    }
    /// [`cuts_with_look_alikes`] of the frames pushed so far.
    pub fn cuts_with_look_alikes(&self, opt: &SceneOptions) -> Vec<usize> {
        cuts_with_look_alikes(&self.diffs, opt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic shot: a pattern chosen by `shot`, moving with time `t` (pan + noise).
    fn frame(shot: u32, t: u32, w: usize, h: usize) -> Vec<u8> {
        let mut v = vec![0u8; w * h * 4];
        let mut seed = shot.wrapping_mul(2_654_435_761) ^ t.wrapping_mul(40_503);
        for y in 0..h {
            for x in 0..w {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                let noise = ((seed >> 16) % 9) as i32 - 4;
                let xx = (x as u32 + 2 * t) as f32;
                let (r, g, b) = match shot % 4 {
                    0 => (40.0 + 150.0 * (xx * 0.05).sin().abs(), 90.0, 160.0),
                    1 => (200.0, 180.0 - (y as f32) * 0.5, 40.0 + (xx * 0.1).cos().abs() * 60.0),
                    2 => (if (xx as usize / 8 + y / 8).is_multiple_of(2) { 230.0 } else { 20.0 }, 120.0, 60.0),
                    _ => (30.0, 30.0 + (y as f32 * 1.5).min(200.0), 30.0 + (xx * 0.03).sin().abs() * 200.0),
                };
                let i = (y * w + x) * 4;
                v[i] = (r as i32 + noise).clamp(0, 255) as u8;
                v[i + 1] = (g as i32 + noise).clamp(0, 255) as u8;
                v[i + 2] = (b as i32 + noise).clamp(0, 255) as u8;
                v[i + 3] = 255;
            }
        }
        v
    }

    fn run(shots: &[(u32, u32)], opt: &SceneOptions) -> Vec<usize> {
        let mut d = SceneDetector::new();
        let mut t = 0;
        for &(shot, len) in shots {
            for _ in 0..len {
                d.push_rgba(&frame(shot, t, 96, 54), 96, 54);
                t += 1;
            }
        }
        d.cuts(opt)
    }

    #[test]
    fn finds_hard_cuts_and_ignores_motion() {
        let cuts = run(&[(0, 20), (1, 15), (2, 25), (3, 12), (0, 10)], &SceneOptions::default());
        assert_eq!(cuts, vec![20, 35, 60, 72]);
        // one continuous moving shot: no cuts at any sensitivity
        for s in [0.0, 50.0, 100.0] {
            assert!(run(&[(2, 60)], &SceneOptions { sensitivity: s, ..Default::default() }).is_empty(), "sensitivity {s}");
        }
    }

    #[test]
    fn min_shot_length_merges_flashes() {
        // a 2-frame flash of another shot inside shot 0: one cut into the flash is kept, the cut
        // back out (2 frames later) is closer than the minimum shot length
        let cuts = run(&[(0, 20), (1, 2), (0, 20)], &SceneOptions::default());
        assert_eq!(cuts, vec![20]);
        let cuts = run(&[(0, 20), (1, 2), (0, 20)], &SceneOptions { min_shot_frames: 1, ..Default::default() });
        assert_eq!(cuts, vec![20, 22]);
    }

    #[test]
    fn sensitivity_controls_subtle_cuts() {
        // a subtle cut: the same pattern with a small global brightness change
        let mk = |k: i32| {
            let mut f = frame(0, 0, 64, 36);
            f.chunks_mut(4).for_each(|p| (0..3).for_each(|c| p[c] = (p[c] as i32 + k).clamp(0, 255) as u8));
            signature(&f, 64, 36)
        };
        let mut d = SceneDetector::new();
        for i in 0..30 {
            d.push(mk(if i < 15 { 0 } else { 12 }));
        }
        let diff = d.diffs()[15];
        assert!(diff > 0.1 && diff < 0.4, "subtle diff {diff}");
        assert!(d.cuts(&SceneOptions { sensitivity: 0.0, ..Default::default() }).is_empty());
        assert_eq!(d.cuts(&SceneOptions { sensitivity: 100.0, ..Default::default() }), vec![15]);
        assert!(SceneOptions { sensitivity: 100.0, ..Default::default() }.threshold() < SceneOptions::default().threshold());
    }

    #[test]
    fn a_cut_between_look_alike_shots_is_reported_as_a_near_miss() {
        // a quiet shot, then a jump of 0.2 (the same sea under the same sky from another boat)
        let mut diffs = vec![0.008f32; 60];
        diffs[30] = 0.20;
        let opt = SceneOptions::default();
        assert!(detect_cuts(&diffs, &opt).is_empty(), "under the default threshold of {}", opt.threshold());
        let near = near_misses(&diffs, &opt);
        assert_eq!(near.len(), 1);
        assert_eq!((near[0].frame, near[0].difference), (30, 0.20));
        // the sensitivity it names does find it, and one below does not
        let s = near[0].sensitivity;
        assert_eq!(detect_cuts(&diffs, &SceneOptions { sensitivity: s, ..opt }), vec![30]);
        assert!(detect_cuts(&diffs, &SceneOptions { sensitivity: s - 2.0, ..opt }).is_empty());
        assert!(near_misses(&diffs, &SceneOptions { sensitivity: s, ..opt }).is_empty(), "a found cut is not also a near miss");
        // movement is not a near miss: the jump has to stand far above what goes on around it
        let mut busy = vec![0.03f32; 60];
        busy[30] = 0.20;
        assert!(near_misses(&busy, &opt).is_empty());
        // too small for any sensitivity, right beside a real cut, or junk: nothing
        let mut tiny = vec![0.001f32; 60];
        tiny[30] = 0.10;
        assert!(near_misses(&tiny, &opt).is_empty());
        let mut beside = vec![0.008f32; 60];
        beside[30] = 0.6;
        beside[32] = 0.2;
        assert!(near_misses(&beside, &opt).is_empty());
        assert!(near_misses(&[], &opt).is_empty());
        assert!(near_misses(&[f32::NAN; 20], &opt).is_empty());
    }

    #[test]
    fn a_flash_is_not_a_cut_between_look_alike_shots() {
        let opt = SceneOptions::default();
        // a photographer's flash: the picture jumps and jumps back on the next frame
        let mut flash = vec![0.008f32; 60];
        (flash[30], flash[31]) = (0.20, 0.19);
        assert!(near_misses(&flash, &opt).is_empty());
        // or two frames later (a flash that lasts two frames)
        let mut long = vec![0.008f32; 60];
        (long[30], long[32]) = (0.20, 0.18);
        assert!(near_misses(&long, &opt).is_empty());
        // a cut is one step, and stays
        let mut cut = vec![0.008f32; 60];
        cut[30] = 0.20;
        assert_eq!(near_misses(&cut, &opt).len(), 1);
    }

    #[test]
    fn repeated_frames_do_not_make_busy_footage_look_quiet() {
        let opt = SceneOptions::default();
        // hand-held footage: every frame differs by 0.03, and one step of 0.13 is movement, not a cut
        let mut busy = vec![0.03f32; 60];
        busy[30] = 0.13;
        assert!(near_misses(&busy, &opt).is_empty());
        // The same footage at half speed, or in a sequence of twice its frame rate: every frame
        // is shown twice. It is no quieter for that.
        let doubled: Vec<f32> = busy.iter().flat_map(|d| [*d, 0.0]).collect();
        assert!(near_misses(&doubled, &opt).is_empty(), "{:?}", near_misses(&doubled, &opt));
        // a flash in doubled footage is still two steps, now four frames apart
        let mut flash = vec![0.008f32; 60];
        (flash[30], flash[31]) = (0.20, 0.19);
        let flash: Vec<f32> = flash.iter().flat_map(|d| [*d, 0.0, 0.0, 0.0]).collect();
        assert!(near_misses(&flash, &opt).is_empty());
        // and a look-alike cut in doubled footage is still found, at its own frame
        let mut cut = vec![0.008f32; 60];
        cut[30] = 0.20;
        let cut: Vec<f32> = cut.iter().flat_map(|d| [*d, 0.0]).collect();
        assert_eq!(near_misses(&cut, &opt).iter().map(|m| m.frame).collect::<Vec<_>>(), vec![60]);
        // a picture that never changes and then does: nothing around it to be busy
        let mut still = vec![0.0f32; 60];
        still[30] = 0.20;
        assert_eq!(near_misses(&still, &opt).len(), 1);
    }

    #[test]
    fn look_alike_cuts_join_the_cuts_in_order() {
        let opt = SceneOptions::default();
        let mut diffs = vec![0.008f32; 120];
        (diffs[20], diffs[50], diffs[90]) = (0.6, 0.2, 0.7);
        assert_eq!(detect_cuts(&diffs, &opt), vec![20, 90]);
        assert_eq!(cuts_with_look_alikes(&diffs, &opt), vec![20, 50, 90]);
        // nothing new where every cut is already over the threshold, and nothing at all in nothing
        assert_eq!(cuts_with_look_alikes(&diffs, &SceneOptions { sensitivity: 100.0, ..opt }), vec![20, 50, 90]);
        assert!(cuts_with_look_alikes(&[], &opt).is_empty());
    }

    /// A 64×36 frame of `base` grey with blocks (x, y, w, h, grey) on it.
    fn blocks(base: u8, rects: &[(usize, usize, usize, usize, u8)]) -> Vec<u8> {
        let (w, h) = (64usize, 36usize);
        let mut px = vec![base; w * h * 4];
        for &(x0, y0, bw, bh, v) in rects {
            for y in y0..(y0 + bh).min(h) {
                for x in x0..(x0 + bw).min(w) {
                    px[(y * w + x) * 4..(y * w + x) * 4 + 3].fill(v);
                }
            }
        }
        px.iter_mut().skip(3).step_by(4).for_each(|a| *a = 255);
        px
    }

    #[test]
    fn likeness_tells_a_new_picture_from_the_same_one_shifted() {
        let sig = |base, rects: &[(usize, usize, usize, usize, u8)]| signature(&blocks(base, rects), 64, 36);
        // a night street: a lit window on the left; and another: a street lamp on the right
        let (street, lamp) = (sig(8, &[(6, 8, 14, 12, 60)]), sig(8, &[(44, 4, 6, 26, 55)]));
        let d = difference(&street, &lamp);
        assert!(d < 0.13, "two dark pictures differ little in sum: {d}");
        assert!(likeness(&street, &lamp).unwrap() < 0.2, "and are not alike: {:?}", likeness(&street, &lamp));
        // the same street with the window a little to the right: the same picture, shifted
        let nudged = sig(8, &[(8, 8, 14, 12, 60)]);
        assert!(likeness(&street, &nudged).unwrap() > 0.8, "{:?}", likeness(&street, &nudged));
        // the same picture a stop brighter is still the same picture
        let brighter = sig(20, &[(6, 8, 14, 12, 120)]);
        assert!(likeness(&street, &brighter).unwrap() > 0.95);
        // nothing to compare in black, fog or a plain wall
        let plain = sig(40, &[]);
        assert_eq!((likeness(&plain, &street), likeness(&street, &plain), likeness(&plain, &plain)), (None, None, None));
        assert_eq!(likeness(&signature(&[], 0, 0), &street), None);
    }

    #[test]
    fn degenerate_inputs() {
        assert!(detect_cuts(&[], &SceneOptions::default()).is_empty());
        assert!(detect_cuts(&[0.0], &SceneOptions::default()).is_empty());
        let s = signature(&[], 0, 0);
        assert_eq!(difference(&s, &s), 0.0);
    }
}
