//! Mask tracking: frame-to-frame motion of the picture inside a mask.
//!
//! The method (all textbook, implemented from the papers' equations):
//!
//! 1. **Features** — Shi–Tomasi "good features to track" (Shi & Tomasi, CVPR 1994): the smaller
//!    eigenvalue of the 2×2 gradient structure tensor summed over a 7×7 window, kept above 1 % of
//!    the strongest response inside the mask (shrunk by the window radius), with non-maximum
//!    suppression and a minimum spacing; at most [`MAX_FEATURES`].
//! 2. **Tracking** — pyramidal Lucas–Kanade (Bouguet's formulation of Lucas & Kanade 1981):
//!    a 4-level image pyramid (5-tap binomial blur + 2× decimation), a 15×15 window, Gauss–Newton
//!    iterations per level with bilinear sampling, the estimate doubled down the pyramid.
//!    Each feature is tracked forward and then backward; it is kept only when the backward track
//!    returns within 0.5 px of where it started (forward–backward error, Kalal et al. 2010).
//! 3. **Fit** — RANSAC (Fischler & Bolles 1981) over minimal samples (1 point for Position,
//!    2 for Position & Rotation and Position, Scale & Rotation) with a 1 px inlier threshold and
//!    a fixed-seed generator (deterministic), then a least-squares refit on the inliers:
//!    translation = mean displacement, similarity = the closed-form 2D Umeyama / Procrustes
//!    solution, rigid = the same with the scale normalised away.
//!
//! The result is the transform mapping frame A's pixels to frame B's; the engine applies it to
//! the mask path to write the next Mask Path keyframe.
//!
//! Each step fits the dominant motion inside the mask, whatever moves that way. [`Appearance`]
//! is the check that it is still the subject: what set the inside of the mask apart from its
//! surroundings in the first frame has to stay inside it ([`LOST_BELOW`]).

use filmcraft_geom::{Affine, Vec2};
use filmcraft_project::TrackMethod;
use rayon::prelude::*;

/// Most features tracked per frame.
pub const MAX_FEATURES: usize = 160;
const WIN: i32 = 7;
const LEVELS: usize = 4;
const FB_MAX: f32 = 0.5;
const RANSAC_THRESHOLD: f64 = 1.0;
const RANSAC_ITERS: usize = 300;

/// A single-channel f32 image (0..1, display-referred luma).
#[derive(Clone, Debug, PartialEq)]
pub struct Gray {
    pub w: usize,
    pub h: usize,
    pub px: Vec<f32>,
}

impl Gray {
    pub fn new(w: usize, h: usize, px: Vec<f32>) -> Self {
        assert_eq!(px.len(), w * h);
        Self { w, h, px }
    }
    /// Luma of 8-bit RGBA (Rec. 709 weights on the encoded values).
    pub fn from_rgba8(w: usize, h: usize, rgba: &[u8]) -> Self {
        let px = rgba.as_chunks::<4>().0.iter().map(|p| (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) / 255.0).collect();
        Self { w, h, px }
    }
    #[inline]
    fn at(&self, x: i32, y: i32) -> f32 {
        let x = x.clamp(0, self.w as i32 - 1) as usize;
        let y = y.clamp(0, self.h as i32 - 1) as usize;
        self.px[y * self.w + x]
    }
    /// Bilinear sample at continuous coordinates (pixel centres at integers), clamped.
    #[inline]
    pub fn sample(&self, x: f32, y: f32) -> f32 {
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (x0, y0) = (x0 as i32, y0 as i32);
        let a = self.at(x0, y0);
        let b = self.at(x0 + 1, y0);
        let c = self.at(x0, y0 + 1);
        let d = self.at(x0 + 1, y0 + 1);
        let top = a + (b - a) * fx;
        let bot = c + (d - c) * fx;
        top + (bot - top) * fy
    }
    /// 5-tap binomial blur then 2× decimation.
    pub(crate) fn down(&self) -> Gray {
        let k = [1.0, 4.0, 6.0, 4.0, 1.0].map(|v: f32| v / 16.0);
        let (w, h) = (self.w, self.h);
        let mut tmp = vec![0.0f32; w * h];
        tmp.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            for (x, o) in row.iter_mut().enumerate() {
                *o = (0..5).map(|i| k[i] * self.at(x as i32 + i as i32 - 2, y as i32)).sum();
            }
        });
        let t = Gray { w, h, px: tmp };
        let (nw, nh) = (w.div_ceil(2).max(1), h.div_ceil(2).max(1));
        let mut px = vec![0.0f32; nw * nh];
        px.par_chunks_mut(nw).enumerate().for_each(|(y, row)| {
            for (x, o) in row.iter_mut().enumerate() {
                *o = (0..5).map(|i| k[i] * t.at(2 * x as i32, 2 * y as i32 + i as i32 - 2)).sum();
            }
        });
        Gray { w: nw, h: nh, px }
    }
    fn pyramid(&self) -> Vec<Gray> {
        let mut v = vec![self.clone()];
        while v.len() < LEVELS && v.last().is_some_and(|g| g.w >= 32 && g.h >= 32) {
            let Some(n) = v.last().map(Gray::down) else { break };
            v.push(n);
        }
        v
    }
    /// Central-difference gradient.
    #[inline]
    fn grad(&self, x: i32, y: i32) -> (f32, f32) {
        ((self.at(x + 1, y) - self.at(x - 1, y)) * 0.5, (self.at(x, y + 1) - self.at(x, y - 1)) * 0.5)
    }
}

fn point_in(poly: &[Vec2], x: f64, y: f64) -> bool {
    let n = poly.len();
    let mut w = 0;
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        if (a.y <= y) != (b.y <= y) && a.x + (b.x - a.x) * (y - a.y) / (b.y - a.y) < x {
            w += if b.y > a.y { 1 } else { -1 };
        }
    }
    w != 0
}

fn dist_to_poly(poly: &[Vec2], p: Vec2) -> f64 {
    let n = poly.len();
    (0..n)
        .map(|i| {
            let (a, b) = (poly[i], poly[(i + 1) % n]);
            let d = b - a;
            let l2 = d.x * d.x + d.y * d.y;
            let t = if l2 > 0.0 { (((p - a).x * d.x + (p - a).y * d.y) / l2).clamp(0.0, 1.0) } else { 0.0 };
            (p - (a + d * t)).length()
        })
        .fold(f64::INFINITY, f64::min)
}

/// Shi–Tomasi corners inside `region` (a polygon in `g`'s pixels), strongest first.
pub fn features(g: &Gray, region: &[Vec2], max: usize, min_dist: f32) -> Vec<[f32; 2]> {
    if region.len() < 3 {
        return Vec::new();
    }
    let lo = region.iter().fold(Vec2::new(f64::INFINITY, f64::INFINITY), |a, p| Vec2::new(a.x.min(p.x), a.y.min(p.y)));
    let hi = region.iter().fold(Vec2::new(f64::NEG_INFINITY, f64::NEG_INFINITY), |a, p| Vec2::new(a.x.max(p.x), a.y.max(p.y)));
    let margin = WIN + 2;
    let x0 = (lo.x.floor() as i32).max(margin);
    let y0 = (lo.y.floor() as i32).max(margin);
    let x1 = (hi.x.ceil() as i32).min(g.w as i32 - 1 - margin);
    let y1 = (hi.y.ceil() as i32).min(g.h as i32 - 1 - margin);
    if x1 <= x0 || y1 <= y0 {
        return Vec::new();
    }
    let (bw, bh) = ((x1 - x0 + 1) as usize, (y1 - y0 + 1) as usize);
    // structure tensor response (min eigenvalue) over a 7×7 window, at pixels inside the region
    let r = 3;
    let resp: Vec<f32> = (0..bw * bh)
        .into_par_iter()
        .map(|i| {
            let (x, y) = (x0 + (i % bw) as i32, y0 + (i / bw) as i32);
            let p = Vec2::new(x as f64, y as f64);
            if !point_in(region, p.x, p.y) || dist_to_poly(region, p) < r as f64 {
                return 0.0;
            }
            let (mut a, mut b, mut c) = (0.0f32, 0.0f32, 0.0f32);
            for dy in -r..=r {
                for dx in -r..=r {
                    let (gx, gy) = g.grad(x + dx, y + dy);
                    a += gx * gx;
                    b += gx * gy;
                    c += gy * gy;
                }
            }
            let tr = (a + c) * 0.5;
            tr - ((a - c) * (a - c) * 0.25 + b * b).sqrt()
        })
        .collect();
    let best = resp.iter().copied().fold(0.0f32, f32::max);
    if best <= 1e-9 {
        return Vec::new();
    }
    let thr = best * 0.01;
    // 3×3 non-maximum suppression
    let mut cand: Vec<(f32, i32, i32)> = Vec::new();
    for yy in 1..bh.saturating_sub(1) {
        for xx in 1..bw.saturating_sub(1) {
            let v = resp[yy * bw + xx];
            if v < thr {
                continue;
            }
            let mut is_max = true;
            'n: for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    if (dx != 0 || dy != 0) && resp[(yy as i32 + dy) as usize * bw + (xx as i32 + dx) as usize] > v {
                        is_max = false;
                        break 'n;
                    }
                }
            }
            if is_max {
                cand.push((v, x0 + xx as i32, y0 + yy as i32));
            }
        }
    }
    cand.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    let mut out: Vec<[f32; 2]> = Vec::new();
    let md2 = min_dist * min_dist;
    for (_, x, y) in cand {
        let p = [x as f32, y as f32];
        if out.iter().all(|q| (q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2) >= md2) {
            out.push(p);
            if out.len() >= max {
                break;
            }
        }
    }
    out
}

/// Pyramidal Lucas–Kanade: where `p` in `a` moved to in `b` (pyramids from [`Gray::pyramid`]).
fn lk(pa: &[Gray], pb: &[Gray], p: [f32; 2]) -> Option<[f32; 2]> {
    let levels = pa.len().min(pb.len());
    let mut d = [0.0f32; 2];
    for lvl in (0..levels).rev() {
        let s = (1u32 << lvl) as f32;
        let (a, b) = (&pa[lvl], &pb[lvl]);
        let (px, py) = (p[0] / s, p[1] / s);
        // spatial gradient matrix and template over the window (bilinear at sub-pixel p)
        let n = (2 * WIN + 1) as usize;
        let mut tpl = Vec::with_capacity(n * n);
        let mut grads = Vec::with_capacity(n * n);
        let (mut gxx, mut gxy, mut gyy) = (0.0f32, 0.0f32, 0.0f32);
        for wy in -WIN..=WIN {
            for wx in -WIN..=WIN {
                let (x, y) = (px + wx as f32, py + wy as f32);
                let ix = (a.sample(x + 1.0, y) - a.sample(x - 1.0, y)) * 0.5;
                let iy = (a.sample(x, y + 1.0) - a.sample(x, y - 1.0)) * 0.5;
                gxx += ix * ix;
                gxy += ix * iy;
                gyy += iy * iy;
                tpl.push(a.sample(x, y));
                grads.push((ix, iy));
            }
        }
        let det = gxx * gyy - gxy * gxy;
        if det < 1e-7 {
            return None;
        }
        let mut v = [0.0f32; 2];
        for _ in 0..30 {
            let (mut bx, mut by) = (0.0f32, 0.0f32);
            let mut k = 0;
            for wy in -WIN..=WIN {
                for wx in -WIN..=WIN {
                    let (x, y) = (px + wx as f32 + d[0] + v[0], py + wy as f32 + d[1] + v[1]);
                    let diff = tpl[k] - b.sample(x, y);
                    let (ix, iy) = grads[k];
                    bx += diff * ix;
                    by += diff * iy;
                    k += 1;
                }
            }
            let ex = (gyy * bx - gxy * by) / det;
            let ey = (gxx * by - gxy * bx) / det;
            v[0] += ex;
            v[1] += ey;
            if ex * ex + ey * ey < 1e-6 {
                break;
            }
        }
        d = [d[0] + v[0], d[1] + v[1]];
        if lvl > 0 {
            d = [d[0] * 2.0, d[1] * 2.0];
        }
    }
    let q = [p[0] + d[0], p[1] + d[1]];
    let (w, h) = (pa[0].w as f32, pa[0].h as f32);
    (q[0].is_finite() && q[1].is_finite() && q[0] >= 0.0 && q[1] >= 0.0 && q[0] < w && q[1] < h).then_some(q)
}

/// Least-squares fit of `method` mapping `src` → `dst`.
pub fn fit_ls(method: TrackMethod, src: &[Vec2], dst: &[Vec2]) -> Option<Affine> {
    let n = src.len();
    if n == 0 || (method != TrackMethod::Position && n < 2) {
        return None;
    }
    let inv = 1.0 / n as f64;
    let cs = src.iter().fold(Vec2::ZERO, |a, p| a + *p) * inv;
    let cd = dst.iter().fold(Vec2::ZERO, |a, p| a + *p) * inv;
    if method == TrackMethod::Position {
        let t = cd - cs;
        return Some(Affine::translate(t.x, t.y));
    }
    // 2D Procrustes: [a −b; b a] minimising Σ |R s + t − d|²
    let (mut sa, mut sb, mut ss) = (0.0, 0.0, 0.0);
    for (s, d) in src.iter().zip(dst) {
        let (x, y) = (s.x - cs.x, s.y - cs.y);
        let (u, v) = (d.x - cd.x, d.y - cd.y);
        sa += x * u + y * v;
        sb += x * v - y * u;
        ss += x * x + y * y;
    }
    if ss < 1e-12 {
        return None;
    }
    let (mut a, mut b) = (sa / ss, sb / ss);
    if method == TrackMethod::PositionRotation {
        let l = (a * a + b * b).sqrt();
        if l < 1e-12 {
            return None;
        }
        a /= l;
        b /= l;
    }
    let lin = Affine { a, b, c: -b, d: a, e: 0.0, f: 0.0 };
    let t = cd - lin.apply(cs);
    Some(Affine { e: t.x, f: t.y, ..lin })
}

/// Deterministic xorshift for RANSAC sampling.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// RANSAC + least-squares refit. Returns (transform, inlier count).
pub fn fit_robust(method: TrackMethod, src: &[Vec2], dst: &[Vec2]) -> Option<(Affine, usize)> {
    let n = src.len();
    let k = if method == TrackMethod::Position { 1 } else { 2 };
    if n < k {
        return None;
    }
    let inliers_of = |m: &Affine| -> Vec<usize> { (0..n).filter(|&i| (m.apply(src[i]) - dst[i]).length() < RANSAC_THRESHOLD).collect() };
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ n as u64);
    let mut best: Vec<usize> = Vec::new();
    for _ in 0..RANSAC_ITERS {
        let i = rng.below(n);
        let mut idx = vec![i];
        if k == 2 {
            let mut j = rng.below(n);
            let mut guard = 0;
            while (j == i || (src[j] - src[i]).length() < 4.0) && guard < 16 {
                j = rng.below(n);
                guard += 1;
            }
            if j == i {
                continue;
            }
            idx.push(j);
        }
        let (s, d): (Vec<Vec2>, Vec<Vec2>) = idx.iter().map(|&q| (src[q], dst[q])).unzip();
        let Some(m) = fit_ls(method, &s, &d) else { continue };
        let inl = inliers_of(&m);
        if inl.len() > best.len() {
            best = inl;
            if best.len() == n {
                break;
            }
        }
    }
    if best.len() < k {
        return None;
    }
    // refit on inliers, then once more on the refit's inliers
    for _ in 0..2 {
        let (s, d): (Vec<Vec2>, Vec<Vec2>) = best.iter().map(|&q| (src[q], dst[q])).unzip();
        let m = fit_ls(method, &s, &d)?;
        let inl = inliers_of(&m);
        if inl.len() < best.len() {
            break;
        }
        best = inl;
    }
    let (s, d): (Vec<Vec2>, Vec<Vec2>) = best.iter().map(|&q| (src[q], dst[q])).unzip();
    fit_ls(method, &s, &d).map(|m| (m, best.len()))
}

/// One tracking step's result.
#[derive(Clone, Debug)]
pub struct Step {
    /// Maps frame A pixels to frame B pixels.
    pub transform: Affine,
    pub features: usize,
    pub tracked: usize,
    pub inliers: usize,
}

/// A frame prepared for tracking (its pyramid).
pub struct Prepared {
    levels: Vec<Gray>,
}

impl Prepared {
    pub fn new(g: Gray) -> Self {
        Self { levels: g.pyramid() }
    }
    pub fn base(&self) -> &Gray {
        &self.levels[0]
    }
}

/// Track the picture inside `region` (polygon in frame pixels) from `a` to `b`.
pub fn track_step(a: &Prepared, b: &Prepared, region: &[Vec2], method: TrackMethod) -> Option<Step> {
    let feats = features(a.base(), region, MAX_FEATURES, 5.0);
    if feats.len() < 3 {
        return None;
    }
    let pairs: Vec<(Vec2, Vec2)> = feats
        .par_iter()
        .filter_map(|&p| {
            let q = lk(&a.levels, &b.levels, p)?;
            let back = lk(&b.levels, &a.levels, q)?;
            let fb = ((back[0] - p[0]).powi(2) + (back[1] - p[1]).powi(2)).sqrt();
            (fb < FB_MAX).then(|| (Vec2::new(p[0] as f64, p[1] as f64), Vec2::new(q[0] as f64, q[1] as f64)))
        })
        .collect();
    let (src, dst): (Vec<Vec2>, Vec<Vec2>) = pairs.into_iter().unzip();
    let (transform, inliers) = fit_robust(method, &src, &dst)?;
    if inliers < 3 {
        return None;
    }
    Some(Step { transform, features: feats.len(), tracked: src.len(), inliers })
}

// ---------------------------------------------------------------- appearance

/// Most sample points per side of a region's bounding box (a larger region is sampled evenly, so
/// the work and the memory do not grow with the frame).
const LOOK_SAMPLES: usize = 128;
/// Shortest bounding-box side, in pixels, there is enough picture in to compare.
const LOOK_MIN_SIDE: f64 = 8.0;
/// Histogram nodes along lightness and along each of the two colour axes. Lightness is coarse
/// and logarithmic (a node every [`LOOK_LIGHT_STEP`] halvings of the encoded value), so a change
/// of exposure moves every colour by the same small part of a node; colour, which exposure
/// leaves alone, is finer.
const LOOK_LIGHT: usize = 4;
const LOOK_LIGHT_STEP: f32 = 1.5;
const LOOK_COLOUR: usize = 13;
/// Colour axes run from −`LOOK_COLOUR_RANGE` to +`LOOK_COLOUR_RANGE` (stronger colours are clamped).
const LOOK_COLOUR_RANGE: f32 = 0.6;
/// Least share of the region's picture that must be its own (colours its surroundings have less
/// of) for there to be something to recognise later.
const LOOK_MIN_OWN: f32 = 0.1;

/// [`Appearance::kept`] under which the picture inside a tracked region is no longer what
/// tracking started on: less than this share of what set the region apart is still inside it.
/// In the tests a mask sliding off a plain subject passes it while part of the subject is still
/// under the mask, and a mask that stays on its subject keeps over 0.9 while the subject turns or
/// grows under it, and stays above it through a stop of exposure either way.
pub const LOST_BELOW: f32 = 0.5;
/// [`Appearance::kept`] from which a tracked region is plainly still on what it started on.
/// Between [`LOST_BELOW`] and this it may be on its way off: in the tests a mask sliding off a
/// plain subject has over 70 % of the subject under it on every frame at or above this, and
/// never comes back above it. Tracking holds the frames in between back until one says which
/// way it went.
pub const SURE_ABOVE: f32 = 0.8;

#[derive(Clone, Copy)]
struct LookSample {
    x: f32,
    y: f32,
    weight: f32,
}

/// What set the picture inside a region apart from its surroundings in one frame, kept so a
/// later frame can be asked whether tracking still follows it.
///
/// Frame-to-frame motion alone cannot tell: on a small, plain subject in front of a busy
/// background most features lie on the background, the dominant motion is the background's, and
/// every step is a good fit while the mask slides off the subject.
///
/// The look is a colour histogram of the region, weighted towards its middle (the rim is mostly
/// background), less the histogram of a band around the region: what is left is the colours the
/// region has more of than its surroundings, which is the subject. A histogram does not care
/// where in the region a colour is, so a subject that turns or bends under the mask keeps its
/// look; one the mask has slid off does not.
///
/// A later frame is read the same way, less only the colours the band had then and has now: a
/// subject that shares a colour with what was behind it (a white shirt before a white wall) keeps
/// that colour as its own when it walks in front of something else, and one that walks in front
/// of something of its own colour is not taken for background.
pub struct Appearance {
    samples: Vec<LookSample>,
    /// Sample points of the band around the region.
    band: Vec<LookSample>,
    /// Histogram of the band (sums to 1, or all zero when the region fills the frame).
    around: Vec<f32>,
    /// The region's histogram less `around`, negative bins dropped.
    own: Vec<f32>,
    own_total: f32,
}

/// The pixel nearest (`x`, `y`) of a `w`×`h` 8-bit RGBA frame, 0..1 per channel.
fn rgb_at(w: usize, h: usize, rgba: &[u8], x: f64, y: f64) -> Option<[f32; 3]> {
    if !(x >= -0.5 && y >= -0.5) {
        return None;
    }
    let (xi, yi) = (x.round() as usize, y.round() as usize);
    if xi >= w || yi >= h {
        return None;
    }
    let i = yi.checked_mul(w)?.checked_add(xi)?.checked_mul(4)?;
    let [r, g, b] = rgba.get(i..i.checked_add(3)?)? else { return None };
    Some([*r as f32 / 255.0, *g as f32 / 255.0, *b as f32 / 255.0])
}

/// Histogram of weighted colours, normalised to sum to 1, and the weight it was made of. Each
/// colour is spread over the nodes around it (trilinear), so a small change moves a little weight
/// to a neighbouring node instead of all of it across a bin edge.
fn look_histogram(colours: impl Iterator<Item = ([f32; 3], f32)>) -> (Vec<f32>, f32) {
    let mut bins = vec![0.0f32; LOOK_LIGHT * LOOK_COLOUR * LOOK_COLOUR];
    let mut total = 0.0f32;
    // node below and the fraction towards the next one, for a position 0..1 along an axis of `n` nodes
    let node = |v: f32, n: usize| {
        let f = v.clamp(0.0, 1.0) * (n - 1) as f32;
        let i = (f as usize).min(n - 2);
        (i, f - i as f32)
    };
    for ([r, g, b], weight) in colours {
        let sum = r + g + b;
        // lightness, and two colour axes (red against green, yellow against blue) as a share of
        // the lightness: exposure scales r, g and b alike and leaves them where they are. The
        // 0.1 keeps the noise of near-black pixels from reading as colour.
        let axis = |c: f32| (c / (sum + 0.1) + LOOK_COLOUR_RANGE) / (2.0 * LOOK_COLOUR_RANGE);
        let (l, fl) = node(-(sum / 3.0).max(1e-3).log2() / (LOOK_LIGHT_STEP * (LOOK_LIGHT - 1) as f32), LOOK_LIGHT);
        let (u, fu) = node(axis(r - g), LOOK_COLOUR);
        let (v, fv) = node(axis(0.5 * (r + g) - b), LOOK_COLOUR);
        for (dl, wl) in [(0, 1.0 - fl), (1, fl)] {
            for (du, wu) in [(0, 1.0 - fu), (1, fu)] {
                for (dv, wv) in [(0, 1.0 - fv), (1, fv)] {
                    if let Some(bin) = bins.get_mut(((l + dl) * LOOK_COLOUR + u + du) * LOOK_COLOUR + v + dv) {
                        *bin += weight * wl * wu * wv;
                    }
                }
            }
        }
        total += weight;
    }
    if total > 0.0 {
        bins.iter_mut().for_each(|b| *b /= total);
    }
    (bins, total)
}

impl Appearance {
    /// The look of the picture inside `region` (a polygon in the frame's pixels) of a `w`×`h`
    /// 8-bit RGBA frame. `None` when there is nothing to recognise later: a region that is
    /// degenerate, smaller than a few pixels or outside the frame, or a picture inside it that
    /// has the colours of what is around it.
    pub fn capture(w: usize, h: usize, rgba: &[u8], region: &[Vec2]) -> Option<Self> {
        if region.len() < 3 || w == 0 || h == 0 || region.iter().any(|p| !(p.x.is_finite() && p.y.is_finite())) {
            return None;
        }
        // the region's bounding box, inside the frame
        let lo = region.iter().fold(Vec2::new(f64::INFINITY, f64::INFINITY), |a, p| Vec2::new(a.x.min(p.x), a.y.min(p.y)));
        let hi = region.iter().fold(Vec2::new(f64::NEG_INFINITY, f64::NEG_INFINITY), |a, p| Vec2::new(a.x.max(p.x), a.y.max(p.y)));
        let (x0, y0) = (lo.x.max(0.0), lo.y.max(0.0));
        let (bw, bh) = (hi.x.min(w as f64 - 1.0) - x0, hi.y.min(h as f64 - 1.0) - y0);
        if !(bw >= LOOK_MIN_SIDE && bh >= LOOK_MIN_SIDE) {
            return None;
        }
        // one sample per pixel up to LOOK_SAMPLES a side
        let (nx, ny) = ((bw as usize).clamp(1, LOOK_SAMPLES), (bh as usize).clamp(1, LOOK_SAMPLES));
        // full weight a quarter of the narrow side in from the rim, falling to nothing at the rim
        let taper = 0.25 * bw.min(bh);
        let mut samples = Vec::with_capacity(nx * ny);
        // the surroundings: the bounding box grown by half its size on every side, outside the region
        let mut band = Vec::with_capacity(nx * ny);
        for iy in 0..ny {
            for ix in 0..nx {
                let (fx, fy) = ((ix as f64 + 0.5) / nx as f64, (iy as f64 + 0.5) / ny as f64);
                let p = Vec2::new(x0 + fx * bw, y0 + fy * bh);
                if point_in(region, p.x, p.y) {
                    let weight = (dist_to_poly(region, p) / taper).min(1.0) as f32;
                    if weight > 0.0 {
                        samples.push(LookSample { x: p.x as f32, y: p.y as f32, weight });
                    }
                }
                let q = Vec2::new(x0 + (2.0 * fx - 0.5) * bw, y0 + (2.0 * fy - 0.5) * bh);
                if !point_in(region, q.x, q.y) && rgb_at(w, h, rgba, q.x, q.y).is_some() {
                    band.push(LookSample { x: q.x as f32, y: q.y as f32, weight: 1.0 });
                }
            }
        }
        let (inside, weight) = look_at(&samples, w, h, rgba, &Affine::IDENTITY);
        if weight <= 0.0 {
            return None;
        }
        let (around, _) = look_at(&band, w, h, rgba, &Affine::IDENTITY);
        let own: Vec<f32> = inside.iter().zip(&around).map(|(i, a)| (i - a).max(0.0)).collect();
        let own_total: f32 = own.iter().sum();
        (own_total >= LOOK_MIN_OWN).then_some(Self { samples, band, around, own, own_total })
    }

    /// The share of what set the region apart that is still inside it, in a `w`×`h` 8-bit RGBA
    /// frame where the captured frame's pixel `p` is now at `to_frame.apply(p)`: 1 while the
    /// region is on what it was captured on, 0 when it holds only what was around it. `None`
    /// when it cannot be told: more than half of the region has left the frame.
    pub fn kept(&self, w: usize, h: usize, rgba: &[u8], to_frame: &Affine) -> Option<f32> {
        let all: f32 = self.samples.iter().map(|s| s.weight).sum();
        let (now, seen) = look_at(&self.samples, w, h, rgba, to_frame);
        if !(seen > 0.0 && seen >= 0.5 * all && self.own_total > 0.0) {
            return None;
        }
        // the band where it is now (all zero where it has left the frame: nothing is discounted)
        let (around_now, _) = look_at(&self.band, w, h, rgba, to_frame);
        let kept: f32 = now.iter().zip(self.around.iter().zip(&around_now)).zip(&self.own).map(|((n, (a0, a1)), o)| (n - a0.min(*a1)).max(0.0).min(*o)).sum();
        Some((kept / self.own_total).clamp(0.0, 1.0))
    }
}

/// Histogram ([`look_histogram`]) of `samples` read from a `w`×`h` 8-bit RGBA frame in which
/// each has moved to `to_frame.apply(sample)`, and the weight that landed inside the frame.
fn look_at(samples: &[LookSample], w: usize, h: usize, rgba: &[u8], to_frame: &Affine) -> (Vec<f32>, f32) {
    look_histogram(samples.iter().filter_map(|s| {
        let q = to_frame.apply(Vec2::new(s.x as f64, s.y as f64));
        Some((rgb_at(w, h, rgba, q.x, q.y)?, s.weight))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic texture: smooth blobs + fine detail.
    fn texture(u: f64, v: f64) -> f32 {
        let s = (u * 0.21).sin() * (v * 0.17).cos() + 0.5 * ((u + v) * 0.43).sin() + 0.35 * ((u * 0.9 - v * 0.6).sin() * (v * 0.75).cos());
        let h = {
            let (iu, iv) = ((u / 6.0).floor() as i64, (v / 6.0).floor() as i64);
            let mut x = (iu.wrapping_mul(73_856_093) ^ iv.wrapping_mul(19_349_663)) as u64;
            x ^= x >> 13;
            x = x.wrapping_mul(0x5bd1_e995);
            (x >> 40) as f64 / (1u64 << 24) as f64
        };
        (0.5 + 0.22 * s + 0.12 * h).clamp(0.0, 1.0) as f32
    }

    /// Frame with a textured disc (radius 70, object coords centred) under `pose`, over a static
    /// differently-textured background.
    fn frame(w: usize, h: usize, pose: &Affine) -> Gray {
        let inv = pose.inverse().unwrap();
        let px = (0..w * h)
            .into_par_iter()
            .map(|i| {
                // 2×2 supersampling
                let mut acc = 0.0;
                for (ox, oy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                    let p = Vec2::new((i % w) as f64 + ox - 0.5, (i / w) as f64 + oy - 0.5);
                    let o = inv.apply(p);
                    acc += if o.length() < 70.0 { texture(o.x + 200.0, o.y + 300.0) } else { 0.25 + 0.1 * texture(p.x * 0.5 + 900.0, p.y * 0.5) };
                }
                acc / 4.0
            })
            .collect();
        Gray::new(w, h, px)
    }

    fn pose(t: f64, rot_per: f64, scale_per: f64) -> Affine {
        Affine::motion(Vec2::new(150.0 + 3.2 * t, 120.0 - 1.7 * t), Vec2::new(1.0 + scale_per * t, 1.0 + scale_per * t), rot_per * t, Vec2::ZERO)
    }

    fn circle(c: Vec2, r: f64) -> Vec<Vec2> {
        (0..48).map(|i| c + Vec2::new((i as f64 / 48.0 * std::f64::consts::TAU).cos() * r, (i as f64 / 48.0 * std::f64::consts::TAU).sin() * r)).collect()
    }

    fn run(method: TrackMethod, rot: f64, scale: f64, frames: usize) -> f64 {
        let (w, h) = (320, 240);
        let mut region = circle(Vec2::new(150.0, 120.0), 50.0);
        let first = frame(w, h, &pose(0.0, rot, scale));
        let look = Appearance::capture(w, h, &gray_rgba(&first), &region).expect("a textured disc has a look");
        let mut from_start = Affine::IDENTITY;
        let mut prev = Prepared::new(first);
        let mut worst = 0.0f64;
        let probe = [Vec2::new(30.0, 0.0), Vec2::new(-20.0, 25.0), Vec2::new(0.0, -40.0)];
        let mut tracked: Vec<Vec2> = probe.iter().map(|o| pose(0.0, rot, scale).apply(*o)).collect();
        for k in 1..=frames {
            let g = frame(w, h, &pose(k as f64, rot, scale));
            let px = gray_rgba(&g);
            let next = Prepared::new(g);
            let st = track_step(&prev, &next, &region, method).expect("tracked");
            region = region.iter().map(|p| st.transform.apply(*p)).collect();
            tracked = tracked.iter().map(|p| st.transform.apply(*p)).collect();
            // a subject that is followed keeps its look
            from_start = st.transform.then_apply(&from_start);
            let kept = look.kept(w, h, &px, &from_start).expect("in frame");
            assert!(kept > 0.95, "frame {k}: kept {kept}");
            for (o, t) in probe.iter().zip(&tracked) {
                let truth = pose(k as f64, rot, scale).apply(*o);
                worst = worst.max((truth - *t).length());
            }
            prev = next;
        }
        worst
    }

    #[test]
    fn tracks_translation() {
        let e = run(TrackMethod::Position, 0.0, 0.0, 10);
        assert!(e < 0.5, "max drift {e} px");
    }

    #[test]
    fn tracks_rotation() {
        let e = run(TrackMethod::PositionRotation, 2.0, 0.0, 10);
        assert!(e < 0.75, "max drift {e} px");
    }

    #[test]
    fn tracks_similarity() {
        let e = run(TrackMethod::PositionScaleRotation, -1.5, 0.012, 10);
        assert!(e < 0.75, "max drift {e} px");
    }

    /// 8-bit RGBA of a frame whose pixel (x, y) has the colour `f(x, y)` (0..1 per channel).
    fn rgba(w: usize, h: usize, f: impl Fn(f64, f64) -> [f32; 3] + Sync) -> Vec<u8> {
        let mut px = vec![255u8; w * h * 4];
        px.par_chunks_mut(4).enumerate().for_each(|(i, o)| {
            let c = f((i % w) as f64, (i / w) as f64);
            for (o, c) in o.iter_mut().zip(c) {
                *o = (c.clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        });
        px
    }

    fn rect(c: Vec2, w: f64, h: f64) -> Vec<Vec2> {
        vec![c + Vec2::new(-w / 2.0, -h / 2.0), c + Vec2::new(w / 2.0, -h / 2.0), c + Vec2::new(w / 2.0, h / 2.0), c + Vec2::new(-w / 2.0, h / 2.0)]
    }

    /// A plain 26×68 block (a figure in a plain shirt) standing still in front of water that
    /// drifts 2.5 px right and 1 px down a frame: frame `k` of a 320×240 shot. `gain` scales
    /// the encoded values (exposure).
    fn plain_on_water(k: f64, gain: f32) -> Vec<u8> {
        rgba(320, 240, |x, y| {
            let shirt = (x - 160.0).abs() < 13.0 && (y - 120.0).abs() < 34.0;
            let c = if shirt {
                [0.78, 0.72, 0.6]
            } else {
                let v = 0.45 + 1.1 * texture(x - 2.5 * k + 40.0, y - 1.0 * k + 700.0);
                [0.22 * v, 0.42 * v, 0.6 * v]
            };
            c.map(|v| v * gain)
        })
    }

    fn gray_rgba(g: &Gray) -> Vec<u8> {
        g.px.iter().flat_map(|v| [(v.clamp(0.0, 1.0) * 255.0).round() as u8; 3].into_iter().chain([255])).collect()
    }

    /// The mask slides off a small plain subject onto the moving background behind it, every
    /// step a good fit: the tracker alone reports nothing, the look notices.
    #[test]
    fn a_mask_that_slides_off_a_plain_subject_is_noticed() {
        let (w, h) = (320usize, 240usize);
        let centre = Vec2::new(160.0, 120.0);
        // the share of the 26×68 subject (it stands still at `centre`) under a mask of mw×mh moved by `slid`
        let under = |slid: Vec2, mw: f64, mh: f64| {
            let along = |d: f64, mask: f64, subject: f64| ((mask + subject) / 2.0 - d.abs()).clamp(0.0, subject) / subject;
            along(slid.x, mw, 26.0) * along(slid.y, mh, 68.0)
        };
        for (mw, mh, method) in [(60.0, 100.0, TrackMethod::Position), (34.0, 76.0, TrackMethod::Position), (60.0, 100.0, TrackMethod::PositionScaleRotation)] {
            let mut region = rect(centre, mw, mh);
            let first = plain_on_water(0.0, 1.0);
            let look = Appearance::capture(w, h, &first, &region).expect("a plain subject on water has a look");
            let mut prev = Prepared::new(Gray::from_rgba8(w, h, &first));
            let mut from_start = Affine::IDENTITY;
            let (mut unsure_at, mut lost_at) = (None, None);
            for k in 1..=30 {
                let f = plain_on_water(k as f64, 1.0);
                let next = Prepared::new(Gray::from_rgba8(w, h, &f));
                let st = track_step(&prev, &next, &region, method).expect("every step fits: the tracker alone sees nothing wrong");
                region = region.iter().map(|p| st.transform.apply(*p)).collect();
                from_start = st.transform.then_apply(&from_start);
                let slid = from_start.apply(centre) - centre;
                let kept = look.kept(w, h, &f, &from_start).expect("the region is in the frame");
                let what = format!("{mw}×{mh} {method:?}, frame {k}: kept {kept}, {:.0} % of the subject under the mask", 100.0 * under(slid, mw, mh));
                if kept >= SURE_ABOVE {
                    // a frame taken as plainly on the subject has most of the subject under the mask
                    assert!(unsure_at.is_none(), "sure again after doubt: {what}");
                    assert!(under(slid, mw, mh) > 0.7, "{what}");
                } else if unsure_at.is_none() {
                    unsure_at = Some(k);
                }
                if kept < LOST_BELOW && lost_at.is_none() {
                    // noticed before the mask is wholly off the subject
                    assert!(under(slid, mw, mh) > 0.1, "{what}");
                    lost_at = Some(k);
                }
                if lost_at.is_some() {
                    assert!(kept < LOST_BELOW, "lost stays lost: {what}");
                }
                prev = next;
            }
            // the subject stands still, so how far the mask moved is how far it is off
            let slid = (from_start.apply(centre) - centre).length();
            assert!(slid > 60.0, "{mw}×{mh} {method:?}: the mask follows the water ({slid:.1} px)");
            assert!(lost_at.is_some(), "{mw}×{mh} {method:?}: never noticed");
        }
    }

    /// A subject the mask stays on keeps its look when it turns or changes size under a mask that
    /// does not follow that, and through a stop of exposure either way.
    #[test]
    fn a_subject_the_mask_is_on_keeps_its_look() {
        let (w, h) = (320usize, 240usize);
        for (method, rot, scale) in [(TrackMethod::Position, 2.0, 0.0), (TrackMethod::Position, 0.0, 0.012), (TrackMethod::Position, 0.0, -0.006)] {
            let mut region = circle(Vec2::new(150.0, 120.0), 50.0);
            let g0 = frame(w, h, &pose(0.0, rot, scale));
            let look = Appearance::capture(w, h, &gray_rgba(&g0), &region).expect("a textured disc has a look");
            let mut prev = Prepared::new(g0);
            let mut from_start = Affine::IDENTITY;
            for k in 1..=30 {
                let g = frame(w, h, &pose(k as f64, rot, scale));
                let px = gray_rgba(&g);
                let next = Prepared::new(g);
                let st = track_step(&prev, &next, &region, method).expect("tracked");
                region = region.iter().map(|p| st.transform.apply(*p)).collect();
                from_start = st.transform.then_apply(&from_start);
                let kept = look.kept(w, h, &px, &from_start).expect("in frame");
                assert!(kept > 0.9, "rotation {rot}°, scale {scale} per frame, frame {k}: kept {kept}");
                prev = next;
            }
        }
        let region = rect(Vec2::new(160.0, 120.0), 60.0, 100.0);
        let look = Appearance::capture(w, h, &plain_on_water(0.0, 1.0), &region).unwrap();
        for stops in [-1.0f32, -0.5, 0.5, 1.0] {
            // a stop doubles the light; the encoded value goes with its 1/2.2 power
            let kept = look.kept(w, h, &plain_on_water(17.0, 2f32.powf(stops / 2.2)), &Affine::IDENTITY).unwrap();
            assert!(kept > LOST_BELOW, "{stops} stops: kept {kept}");
        }
    }

    /// A 28×80 figure in a white shirt and beige trousers, its middle at (`x`, 120) of a 320×240
    /// shot: a white wall on the left, a green hedge on the right, a dark floor under both.
    fn figure_by_wall_and_hedge(x: f64) -> Vec<u8> {
        rgba(320, 240, |px, py| {
            if (px - x).abs() < 14.0 && (py - 120.0).abs() < 40.0 {
                if py < 120.0 { [0.95, 0.95, 0.94] } else { [0.76, 0.66, 0.5] }
            } else if py >= 150.0 {
                [0.15, 0.14, 0.13]
            } else if px < 140.0 {
                [0.92, 0.92, 0.9]
            } else {
                let v = 0.6 + 0.8 * texture(px + 300.0, py + 50.0);
                [0.2 * v, 0.45 * v, 0.2 * v]
            }
        })
    }

    /// A subject that shares a colour with what is behind it keeps its look when it walks in
    /// front of something else, either way, under a mask that stays exactly on it and has
    /// background inside it.
    #[test]
    fn a_followed_subject_keeps_its_look_in_front_of_another_background() {
        let (w, h) = (320usize, 240usize);
        for (from, step) in [(60.0, 4.0), (260.0, -4.0)] {
            let look = Appearance::capture(w, h, &figure_by_wall_and_hedge(from), &rect(Vec2::new(from, 120.0), 60.0, 110.0)).expect("the figure has a look");
            for k in 1..=50 {
                let moved = step * k as f64;
                let kept = look.kept(w, h, &figure_by_wall_and_hedge(from + moved), &Affine::translate(moved, 0.0)).expect("in frame");
                assert!(kept > 0.8, "from x = {from}, frame {k}: kept {kept}");
            }
        }
    }

    #[test]
    fn a_look_needs_something_to_recognise_and_survives_hostile_input() {
        let (w, h) = (320usize, 240usize);
        let f = plain_on_water(0.0, 1.0);
        let on_subject = rect(Vec2::new(160.0, 120.0), 60.0, 100.0);
        // water in a band of water: nothing of its own
        for (c, mw, mh) in [
            (Vec2::new(60.0, 60.0), 60.0, 100.0),
            (Vec2::new(250.0, 170.0), 34.0, 76.0),
            (Vec2::new(80.0, 150.0), 110.0, 130.0),
            (Vec2::new(240.0, 60.0), 24.0, 24.0),
        ] {
            assert!(Appearance::capture(w, h, &f, &rect(c, mw, mh)).is_none(), "water only, {mw}×{mh}");
        }
        let r = std::panic::catch_unwind(|| {
            let f = plain_on_water(0.0, 1.0);
            let on_subject = rect(Vec2::new(160.0, 120.0), 60.0, 100.0);
            let nan = [Vec2::new(f64::NAN, 0.0), Vec2::new(10.0, 0.0), Vec2::new(10.0, 10.0)];
            let far = rect(Vec2::new(1e12, -1e12), 60.0, 100.0);
            let vast = rect(Vec2::new(160.0, 120.0), 1e300, 1e300);
            let inf = rect(Vec2::new(160.0, 120.0), f64::INFINITY, 40.0);
            for region in [&on_subject[..2], &nan[..], &far[..], &inf[..], &rect(Vec2::new(160.0, 120.0), 3.0, 3.0)[..], &[][..]] {
                assert!(Appearance::capture(w, h, &f, region).is_none(), "{region:?}");
            }
            // a region far larger than the frame is sampled where the frame is, in bounded memory
            let _ = Appearance::capture(w, h, &f, &vast);
            // sizes that do not match the buffer
            for (w, h, px) in [(0, 0, &f[..]), (w, h, &f[..100]), (w, h, &[][..]), (usize::MAX, usize::MAX, &f[..]), (w * 2, h * 2, &f[..])] {
                let _ = Appearance::capture(w, h, px, &on_subject);
            }
            let look = Appearance::capture(w, h, &f, &on_subject).unwrap();
            for m in [
                Affine::translate(f64::NAN, 0.0),
                Affine::translate(f64::INFINITY, f64::NEG_INFINITY),
                Affine::scale(1e300, 1e300),
                Affine::scale(0.0, 0.0),
                Affine::translate(1e6, 0.0),
                Affine::translate(-200.0, 0.0),
            ] {
                assert!(look.kept(w, h, &f, &m).is_none_or(|k| (0.0..=1.0).contains(&k)), "{m:?}");
                let _ = look.kept(usize::MAX, usize::MAX, &f, &m);
                let _ = look.kept(w, h, &f[..7], &m);
            }
            // most of the region out of the frame: it cannot be told
            assert!(look.kept(w, h, &f, &Affine::translate(-200.0, 0.0)).is_none());
            assert!(look.kept(w, h, &f, &Affine::translate(f64::NAN, 0.0)).is_none());
        });
        assert!(r.is_ok(), "panicked on hostile input");
        let look = Appearance::capture(w, h, &f, &on_subject).unwrap();
        assert!(look.kept(w, h, &f, &Affine::IDENTITY).unwrap() > 0.99);
    }

    #[test]
    fn ransac_rejects_outliers() {
        let truth = Affine::motion(Vec2::new(5.0, -3.0), Vec2::new(1.1, 1.1), 7.0, Vec2::ZERO);
        let mut src = Vec::new();
        let mut dst = Vec::new();
        for i in 0..40 {
            let p = Vec2::new((i * 37 % 100) as f64, (i * 61 % 90) as f64);
            src.push(p);
            dst.push(if i % 4 == 0 { p + Vec2::new(20.0, -13.0) } else { truth.apply(p) });
        }
        let (m, inl) = fit_robust(TrackMethod::PositionScaleRotation, &src, &dst).unwrap();
        assert_eq!(inl, 30);
        for p in &src {
            assert!((m.apply(*p) - truth.apply(*p)).length() < 1e-6);
        }
    }
}
