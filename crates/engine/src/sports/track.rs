//! Analysis for the sports tools: a template tracker for small objects and a motion-saliency
//! subject finder. Both work on small grey frames ([`Luma`]) decoded at reduced size.
//!
//! Methods (textbook, implemented from their definitions):
//!
//! - **Template tracking** — normalised cross-correlation (NCC) of a template against the frame
//!   in a search window around a constant-velocity prediction, with a parabolic sub-pixel fit of
//!   the correlation peak. The template is sampled on a grid of at most [`TEMPLATE_SIDE`]² points
//!   whatever the object's size, so the cost per frame is bounded. A match below [`MIN_SCORE`] is
//!   a lost frame: the position is predicted and the window grows until the object is found again.
//!   The template is refreshed slowly (only from confident matches) so lighting changes are
//!   followed without drifting off the object. This complements the feature tracker behind
//!   `masks.track` (Shi–Tomasi + Lucas–Kanade), which needs detail inside the region: a ball is
//!   a few plain pixels.
//! - **Motion saliency** — the absolute difference of two frames, less a noise floor taken from
//!   its median, gives a motion-energy map; its weighted centroid, refined by a few mean-shift
//!   iterations (Fukunaga & Hostetler 1975) in a window around it, is where the action is. A
//!   heuristic: it follows whatever moves, and a moving camera moves everything.

use filmagic_geom::Vec2;

/// Most template sample points per side.
pub const TEMPLATE_SIDE: usize = 24;
/// Least NCC score that counts as finding the object.
pub const MIN_SCORE: f32 = 0.6;
/// Score from which a match is trusted enough to refresh the template.
const REFRESH_SCORE: f32 = 0.85;
/// Largest search radius, in analysis pixels.
pub const MAX_RADIUS: i64 = 128;
/// Least standard deviation (0..1 luma) inside a template for it to be trackable.
const MIN_TEMPLATE_STD: f32 = 0.02;

/// A grey frame, 0..1, row-major.
#[derive(Clone, Debug, PartialEq)]
pub struct Luma {
    pub w: usize,
    pub h: usize,
    pub px: Vec<f32>,
}

impl Luma {
    /// From RGBA8 (Rec. 709 luma of the encoded values). `None` when the buffer is too short or
    /// the frame is empty.
    pub fn from_rgba8(w: usize, h: usize, rgba: &[u8]) -> Option<Luma> {
        let n = w.checked_mul(h)?;
        if n == 0 || rgba.len() < n.checked_mul(4)? {
            return None;
        }
        let px =
            rgba.as_chunks::<4>().0.iter().take(n).map(|p| (0.2126 * f32::from(p[0]) + 0.7152 * f32::from(p[1]) + 0.0722 * f32::from(p[2])) / 255.0).collect();
        Some(Luma { w, h, px })
    }

    /// The value at (x, y), the nearest edge pixel outside the frame.
    fn at(&self, x: i64, y: i64) -> f32 {
        let xi = x.clamp(0, self.w as i64 - 1) as usize;
        let yi = y.clamp(0, self.h as i64 - 1) as usize;
        self.px.get(yi * self.w + xi).copied().unwrap_or(0.0)
    }
}

/// A zero-mean, unit-norm template sampled at integer offsets from its centre.
#[derive(Clone, Debug)]
pub struct Template {
    offs: Vec<(i64, i64)>,
    vals: Vec<f32>,
    /// Half size in pixels (sets the search grid: a coarse step must not skip a small peak).
    half: (i64, i64),
    /// Standard deviation of the captured patch: a candidate with much less contrast (plain
    /// floor whose faint grain happens to have the template's shape) is not the object.
    std: f32,
}

impl Template {
    /// The template of the `2·half.0+1` × `2·half.1+1` box around (cx, cy). `None` when the box
    /// is plain (nothing to recognise).
    pub fn capture(img: &Luma, cx: f64, cy: f64, half: (i64, i64)) -> Option<Template> {
        let (hx, hy) = (half.0.clamp(1, 4096), half.1.clamp(1, 4096));
        let sx = ((2 * hx + 1) as usize).div_ceil(TEMPLATE_SIDE).max(1) as i64;
        let sy = ((2 * hy + 1) as usize).div_ceil(TEMPLATE_SIDE).max(1) as i64;
        let mut offs = Vec::new();
        let mut y = -hy;
        while y <= hy {
            let mut x = -hx;
            while x <= hx {
                offs.push((x, y));
                x += sx;
            }
            y += sy;
        }
        let (ix, iy) = (cx.round() as i64, cy.round() as i64);
        let vals: Vec<f32> = offs.iter().map(|&(dx, dy)| img.at(ix + dx, iy + dy)).collect();
        let t = Template { offs, vals, half: (hx, hy), std: 0.0 };
        t.normalised(MIN_TEMPLATE_STD)
    }

    fn normalised(mut self, min_std: f32) -> Option<Template> {
        let n = self.vals.len().max(1) as f32;
        let mean = self.vals.iter().sum::<f32>() / n;
        for v in &mut self.vals {
            *v -= mean;
        }
        let norm = self.vals.iter().map(|v| v * v).sum::<f32>().sqrt();
        if !norm.is_finite() || norm / n.sqrt() < min_std {
            return None;
        }
        if min_std > 0.0 {
            self.std = norm / n.sqrt();
        }
        for v in &mut self.vals {
            *v /= norm;
        }
        Some(self)
    }

    /// NCC of the template centred at (x, y) of `img`, -1..1 (0 for a plain patch).
    pub fn score(&self, img: &Luma, x: i64, y: i64) -> f32 {
        let n = self.vals.len().max(1) as f32;
        let mut sum = 0.0f32;
        let mut sum2 = 0.0f32;
        let mut dot = 0.0f32;
        for (&(dx, dy), &t) in self.offs.iter().zip(&self.vals) {
            let v = img.at(x + dx, y + dy);
            sum += v;
            sum2 += v * v;
            dot += t * v;
        }
        // Σ t = 0, so Σ t·(v − mean) = Σ t·v
        let var = sum2 - sum * sum / n;
        if var <= 1e-9 || !var.is_finite() {
            return 0.0;
        }
        let ncc = (dot / var.sqrt()).clamp(-1.0, 1.0);
        // a patch with far less contrast than the object is not it, whatever its shape
        let contrast = ((var / n).sqrt() / (0.5 * self.std).max(1e-6)).min(1.0);
        ncc * contrast
    }

    /// Blend `other` (same offsets) into this one with weight `k`.
    fn blend(&self, other: &Template, k: f32) -> Template {
        let vals = self.vals.iter().zip(&other.vals).map(|(a, b)| a * (1.0 - k) + b * k).collect();
        let t = Template { offs: self.offs.clone(), vals, half: self.half, std: self.std * (1.0 - k) + other.std * k };
        t.normalised(0.0).unwrap_or_else(|| self.clone())
    }

    fn resample(&self, img: &Luma, cx: f64, cy: f64) -> Option<Template> {
        let (ix, iy) = (cx.round() as i64, cy.round() as i64);
        let vals = self.offs.iter().map(|&(dx, dy)| img.at(ix + dx, iy + dy)).collect();
        Template { offs: self.offs.clone(), vals, half: self.half, std: 0.0 }.normalised(MIN_TEMPLATE_STD)
    }
}

/// The best match of `t` within `radius` of `around`: (position with sub-pixel refinement, score).
pub fn search(img: &Luma, t: &Template, around: Vec2, radius: i64) -> Option<(Vec2, f32)> {
    if !(around.x.is_finite() && around.y.is_finite()) {
        return None;
    }
    let r = radius.clamp(1, MAX_RADIUS);
    let cx = (around.x.round() as i64).clamp(0, img.w as i64 - 1);
    let cy = (around.y.round() as i64).clamp(0, img.h as i64 - 1);
    // the correlation peak is about as wide as the object: a coarser grid could step over it
    let fine = (t.half.0.min(t.half.1) / 2).clamp(1, 3);
    let step = if r > 24 {
        3
    } else if r > 12 {
        2
    } else {
        1
    }
    .min(fine);
    let mut best = (cx, cy, f32::MIN);
    let (x0, x1) = ((cx - r).max(0), (cx + r).min(img.w as i64 - 1));
    let (y0, y1) = ((cy - r).max(0), (cy + r).min(img.h as i64 - 1));
    let mut y = y0;
    while y <= y1 {
        let mut x = x0;
        while x <= x1 {
            let s = t.score(img, x, y);
            if s > best.2 {
                best = (x, y, s);
            }
            x += step;
        }
        y += step;
    }
    // refine around the coarse best at single-pixel steps
    let (bx, by) = (best.0, best.1);
    for y in (by - step + 1).max(0)..=(by + step - 1).min(img.h as i64 - 1) {
        for x in (bx - step + 1).max(0)..=(bx + step - 1).min(img.w as i64 - 1) {
            let s = t.score(img, x, y);
            if s > best.2 {
                best = (x, y, s);
            }
        }
    }
    let (bx, by, bs) = best;
    if !bs.is_finite() || bs == f32::MIN {
        return None;
    }
    // parabolic sub-pixel peak along each axis
    let sub = |m: f32, c: f32, p: f32| -> f64 {
        let d = m - 2.0 * c + p;
        if d.abs() < 1e-6 { 0.0 } else { f64::from((0.5 * (m - p) / d).clamp(-0.5, 0.5)) }
    };
    let dx = sub(t.score(img, bx - 1, by), bs, t.score(img, bx + 1, by));
    let dy = sub(t.score(img, bx, by - 1), bs, t.score(img, bx, by + 1));
    Some((Vec2::new(bx as f64 + dx, by as f64 + dy), bs))
}

/// Follows one object from frame to frame.
pub struct PointTracker {
    tmpl: Template,
    pos: Vec2,
    vel: Vec2,
    lost: u32,
    base_radius: i64,
}

impl PointTracker {
    /// Start on the `half`-sized box around `at` in `first`. `None` when the box is plain.
    pub fn new(first: &Luma, at: Vec2, half: (i64, i64)) -> Option<PointTracker> {
        if !(at.x.is_finite() && at.y.is_finite()) {
            return None;
        }
        let tmpl = Template::capture(first, at.x, at.y, half)?;
        let base_radius = (half.0.max(half.1) * 2).clamp(8, MAX_RADIUS / 2);
        Some(PointTracker { tmpl, pos: at, vel: Vec2::ZERO, lost: 0, base_radius })
    }

    /// Where the object is in `img` (the next frame), or `None` when it was not found there.
    pub fn step(&mut self, img: &Luma) -> Option<(Vec2, f32)> {
        let pred = self.pos + self.vel;
        let grow = i64::from(self.lost.min(6)) + 1;
        let r = ((self.base_radius as f64 + self.vel.length()).min(MAX_RADIUS as f64) as i64).saturating_mul(grow).min(MAX_RADIUS);
        let found = search(img, &self.tmpl, pred, r).filter(|(_, s)| *s >= MIN_SCORE);
        match found {
            Some((p, score)) => {
                let moved = (p - self.pos) * (1.0 / f64::from(self.lost + 1));
                self.vel = self.vel * 0.3 + moved * 0.7;
                self.pos = p;
                self.lost = 0;
                if score >= REFRESH_SCORE
                    && let Some(now) = self.tmpl.resample(img, p.x, p.y)
                {
                    self.tmpl = self.tmpl.blend(&now, 0.15);
                }
                Some((p, score))
            }
            None => {
                self.pos = Vec2::new(pred.x.clamp(0.0, img.w as f64), pred.y.clamp(0.0, img.h as f64));
                self.vel = self.vel * 0.8;
                self.lost = self.lost.saturating_add(1);
                None
            }
        }
    }
}

/// Where things move between `prev` and `cur`: the centre of the main motion, or `None` when
/// almost nothing moves (or the frames differ in size).
pub fn motion_centroid(prev: &Luma, cur: &Luma) -> Option<Vec2> {
    if prev.w != cur.w || prev.h != cur.h || cur.px.len() != cur.w * cur.h || prev.px.len() != cur.px.len() || cur.px.is_empty() {
        return None;
    }
    let (w, h) = (cur.w, cur.h);
    let d: Vec<f32> = prev.px.iter().zip(&cur.px).map(|(a, b)| (a - b).abs()).collect();
    let mut sorted = d.clone();
    let mid = sorted.len() / 2;
    let median = *sorted.select_nth_unstable_by(mid, f32::total_cmp).1;
    let floor = (2.5 * median).max(0.03);
    let wts: Vec<f32> = d.iter().map(|v| if v.is_finite() { (v - floor).max(0.0) } else { 0.0 }).collect();
    let total: f64 = wts.iter().map(|v| f64::from(*v)).sum();
    if total < (w * h) as f64 * 0.0004 {
        return None;
    }
    let centroid = |c: Option<(Vec2, f64)>| -> Option<Vec2> {
        let (mut sx, mut sy, mut sw) = (0.0f64, 0.0f64, 0.0f64);
        for (i, &v) in wts.iter().enumerate() {
            if v <= 0.0 {
                continue;
            }
            let (x, y) = ((i % w) as f64 + 0.5, (i / w) as f64 + 0.5);
            if let Some((c, r)) = c
                && ((x - c.x).powi(2) + (y - c.y).powi(2)) > r * r
            {
                continue;
            }
            sx += f64::from(v) * x;
            sy += f64::from(v) * y;
            sw += f64::from(v);
        }
        (sw > 0.0).then(|| Vec2::new(sx / sw, sy / sw))
    };
    let mut c = centroid(None)?;
    let r = 0.2 * w.max(h) as f64;
    for _ in 0..4 {
        match centroid(Some((c, r))) {
            Some(n) => c = n,
            None => break,
        }
    }
    Some(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: usize, h: usize, dots: &[(f64, f64, f64)]) -> Luma {
        let mut px = vec![0.2f32; w * h];
        for (i, v) in px.iter_mut().enumerate() {
            let (x, y) = ((i % w) as f64 + 0.5, (i / w) as f64 + 0.5);
            // a faint fixed grain so the background is not perfectly plain
            *v += 0.02 * (((x * 0.37).sin() * (y * 0.23).cos()) as f32);
            for &(cx, cy, r) in dots {
                if (x - cx).powi(2) + (y - cy).powi(2) <= r * r {
                    *v = 0.95;
                }
            }
        }
        Luma { w, h, px }
    }

    #[test]
    fn follows_a_small_fast_dot() {
        let at = |k: f64| (20.0 + 7.0 * k, 60.0 + 3.0 * k - 0.08 * k * k);
        let first = frame(240, 135, &[(at(0.0).0, at(0.0).1, 3.0)]);
        let mut t = PointTracker::new(&first, Vec2::new(at(0.0).0, at(0.0).1), (5, 5)).unwrap();
        for k in 1..28 {
            let (x, y) = at(k as f64);
            let (p, s) = t.step(&frame(240, 135, &[(x, y, 3.0)])).unwrap_or_else(|| panic!("lost at frame {k}"));
            assert!((p.x - x).abs() < 1.0 && (p.y - y).abs() < 1.0, "frame {k}: {p:?} vs ({x}, {y}), score {s}");
        }
    }

    #[test]
    fn reports_a_lost_object_and_finds_it_again() {
        let first = frame(200, 100, &[(50.0, 50.0, 3.0)]);
        let mut t = PointTracker::new(&first, Vec2::new(50.0, 50.0), (5, 5)).unwrap();
        assert!(t.step(&frame(200, 100, &[(54.0, 50.0, 3.0)])).is_some());
        // hidden for three frames
        for _ in 0..3 {
            assert!(t.step(&frame(200, 100, &[])).is_none());
        }
        let back = t.step(&frame(200, 100, &[(70.0, 50.0, 3.0)])).map(|(p, _)| p);
        assert!(back.is_some_and(|p| (p.x - 70.0).abs() < 1.0), "{back:?}");
    }

    #[test]
    fn a_plain_box_is_not_trackable() {
        let flat = Luma { w: 64, h: 64, px: vec![0.5; 64 * 64] };
        assert!(PointTracker::new(&flat, Vec2::new(32.0, 32.0), (6, 6)).is_none());
        assert!(PointTracker::new(&flat, Vec2::new(f64::NAN, 3.0), (6, 6)).is_none());
    }

    #[test]
    fn hostile_frames_do_not_panic() {
        assert!(Luma::from_rgba8(10, 10, &[0; 12]).is_none());
        assert!(Luma::from_rgba8(0, 10, &[]).is_none());
        assert!(Luma::from_rgba8(usize::MAX, 3, &[]).is_none());
        let one = Luma { w: 1, h: 1, px: vec![0.3] };
        let t = Template { offs: vec![(0, 0), (1, 0)], vals: vec![0.7, -0.7], half: (1, 1), std: 0.1 };
        let _ = search(&one, &t, Vec2::new(1e300, -1e300), i64::MAX);
        let _ = search(&one, &t, Vec2::new(f64::NAN, 0.0), 5);
        assert!(motion_centroid(&one, &Luma { w: 2, h: 1, px: vec![0.0, 1.0] }).is_none());
        let _ = Template::capture(&one, 0.0, 0.0, (i64::MAX, i64::MIN));
    }

    #[test]
    fn motion_centroid_finds_the_mover_and_ignores_a_still_scene() {
        let a = frame(160, 90, &[(40.0, 45.0, 8.0)]);
        let b = frame(160, 90, &[(46.0, 45.0, 8.0)]);
        let c = motion_centroid(&a, &b).unwrap();
        assert!((c.x - 43.0).abs() < 3.0 && (c.y - 45.0).abs() < 2.0, "{c:?}");
        assert!(motion_centroid(&a, &a).is_none());
    }
}
