//! Paths for the sports tools: the camera path of Auto Reframe (gap filling, dead zone, a
//! zero-phase critically damped spring, a pan-speed limit), keyframe thinning, and the curves a
//! path line is drawn along.
//!
//! Methods:
//!
//! - **Dead zone** — the camera target moves only when the subject leaves a zone around it, so a
//!   subject that wanders a little gives a held frame, not a jittering one.
//! - **Critically damped spring** — the exact solution of x'' = −2ωx' − ω²(x − target) over a
//!   step with a constant target, run forward and then backward over the whole range. The second
//!   pass removes the first pass's lag (the camera may start moving before the subject does: the
//!   whole range is known), and both ease in and out. The ends are padded by odd reflection (as
//!   forward–backward filters are), so a subject still moving at an end is not lagged there.
//! - **Speed limit** — a forward and then a backward pass that bound each step; after the
//!   backward pass every step is within the bound.
//! - **Thinning** — Ramer–Douglas–Peucker over (time, value) with the error measured along the
//!   value, so the keyframes left reproduce every sample within a tolerance by linear
//!   interpolation.
//! - **Curves** — Catmull–Rom splines (centripetal-free, uniform) through the points, or a
//!   least-squares parabola (a ball's flight under gravity, seen from the side) fitted in the
//!   points' own principal direction.

use filmagic_geom::Vec2;

/// The samples with gaps (`None`) filled: linear between known neighbours, held before the first
/// and after the last. `None` when nothing is known.
pub fn fill_gaps(v: &[Option<f64>]) -> Option<Vec<f64>> {
    v.iter().position(Option::is_some)?;
    let mut out = vec![0.0; v.len()];
    let mut last: Option<(usize, f64)> = None;
    for (i, s) in v.iter().enumerate() {
        if let Some(x) = *s {
            match last {
                Some((j, y)) if i > j + 1 => {
                    for (k, o) in out.iter_mut().enumerate().take(i).skip(j + 1) {
                        let t = (k - j) as f64 / (i - j) as f64;
                        *o = y + (x - y) * t;
                    }
                }
                None => {
                    for o in out.iter_mut().take(i) {
                        *o = x;
                    }
                }
                _ => {}
            }
            out[i] = x;
            last = Some((i, x));
        }
    }
    if let Some((j, y)) = last {
        for o in out.iter_mut().skip(j + 1) {
            *o = y;
        }
    }
    Some(out)
}

/// The camera follows the target only once it is more than `zone` away.
pub fn dead_zone(v: &[f64], zone: f64) -> Vec<f64> {
    let zone = if zone.is_finite() { zone.max(0.0) } else { 0.0 };
    let mut out = Vec::with_capacity(v.len());
    let Some(&first) = v.first() else { return out };
    let mut cam = first;
    for &x in v {
        if x - cam > zone {
            cam = x - zone;
        } else if cam - x > zone {
            cam = x + zone;
        }
        out.push(cam);
    }
    out
}

/// One pass of a critically damped spring of angular frequency `omega` (1/s) chasing `v`.
fn spring_pass(v: &[f64], dt: f64, omega: f64) -> Vec<f64> {
    let mut out = Vec::with_capacity(v.len());
    let Some(&first) = v.first() else { return out };
    let (mut x, mut vel) = (first, 0.0f64);
    let e = (-omega * dt).exp();
    for &target in v {
        let d = x - target;
        let tmp = (vel + omega * d) * dt;
        vel = (vel - omega * tmp) * e;
        x = target + (d + tmp) * e;
        out.push(x);
    }
    out
}

/// Zero-phase spring smoothing: forward, then backward over the result.
pub fn spring_smooth(v: &[f64], dt: f64, tau: f64) -> Vec<f64> {
    if !(dt.is_finite() && dt > 0.0 && tau.is_finite() && tau > 1e-6) {
        return v.to_vec();
    }
    let n = v.len();
    if n < 2 {
        return v.to_vec();
    }
    let omega = 2.0 / tau;
    // odd reflection about each end, long enough for the spring to settle: a subject still
    // moving at an end is extrapolated, so the passes reach it there instead of lagging behind
    let pad = ((6.0 * tau / dt).ceil() as usize).clamp(1, 4096).min(n - 1);
    let mut ext = Vec::with_capacity(n + 2 * pad);
    ext.extend((1..=pad).rev().map(|k| 2.0 * v[0] - v[k]));
    ext.extend_from_slice(v);
    ext.extend((1..=pad).map(|k| 2.0 * v[n - 1] - v[n - 1 - k]));
    let fwd = spring_pass(&ext, dt, omega);
    let rev: Vec<f64> = fwd.into_iter().rev().collect();
    let mut back = spring_pass(&rev, dt, omega);
    back.reverse();
    back.into_iter().skip(pad).take(n).collect()
}

/// Bound each step to `max_step`.
pub fn limit_speed(v: &[f64], max_step: f64) -> Vec<f64> {
    if !(max_step.is_finite() && max_step > 0.0) {
        return v.to_vec();
    }
    let mut out = v.to_vec();
    for i in 1..out.len() {
        let p = out[i - 1];
        out[i] = out[i].clamp(p - max_step, p + max_step);
    }
    for i in (0..out.len().saturating_sub(1)).rev() {
        let n = out[i + 1];
        out[i] = out[i].clamp(n - max_step, n + max_step);
    }
    out
}

/// Clamp every value into `[lo, hi]` (the middle when the range is empty).
pub fn clamp_all(v: &mut [f64], lo: f64, hi: f64) {
    for x in v {
        *x = if lo <= hi { x.clamp(lo, hi) } else { 0.5 * (lo + hi) };
    }
}

/// Indices of the samples (at `times`, ascending) to keep so that linear interpolation between
/// them reproduces every sample within `tol` (in every component). Always keeps the first and
/// the last.
pub fn thin(times: &[f64], values: &[Vec<f64>], tol: f64) -> Vec<usize> {
    let n = values.len().min(times.len());
    if n <= 2 {
        return (0..n).collect();
    }
    let tol = if tol.is_finite() { tol.max(0.0) } else { 0.0 };
    let mut keep = vec![false; n];
    keep[0] = true;
    keep[n - 1] = true;
    // explicit stack: no recursion depth to bound
    let mut stack = vec![(0usize, n - 1)];
    while let Some((a, b)) = stack.pop() {
        if b <= a + 1 {
            continue;
        }
        let mut worst = (0usize, 0.0f64);
        for k in a + 1..b {
            let span = times[b] - times[a];
            let t = if span.abs() > 1e-12 { (times[k] - times[a]) / span } else { 0.0 };
            let err = values[k].iter().zip(values[a].iter().zip(&values[b])).map(|(v, (x, y))| (v - (x + (y - x) * t)).abs()).fold(0.0, f64::max);
            if err > worst.1 {
                worst = (k, err);
            }
        }
        if worst.1 > tol {
            keep[worst.0] = true;
            stack.push((a, worst.0));
            stack.push((worst.0, b));
        }
    }
    (0..n).filter(|&i| keep[i]).collect()
}

/// A uniform Catmull–Rom spline through `pts`, `per` samples per span.
pub fn catmull_rom(pts: &[Vec2], per: usize) -> Vec<Vec2> {
    if pts.len() < 3 {
        return pts.to_vec();
    }
    let per = per.clamp(1, 64);
    let get = |i: isize| pts[i.clamp(0, pts.len() as isize - 1) as usize];
    let mut out = Vec::with_capacity(pts.len() * per);
    for i in 0..pts.len() - 1 {
        let (p0, p1, p2, p3) = (get(i as isize - 1), get(i as isize), get(i as isize + 1), get(i as isize + 2));
        for k in 0..per {
            let t = k as f64 / per as f64;
            let (t2, t3) = (t * t, t * t * t);
            let f =
                |a: f64, b: f64, c: f64, d: f64| 0.5 * (2.0 * b + (-a + c) * t + (2.0 * a - 5.0 * b + 4.0 * c - d) * t2 + (-a + 3.0 * b - 3.0 * c + d) * t3);
            out.push(Vec2::new(f(p0.x, p1.x, p2.x, p3.x), f(p0.y, p1.y, p2.y, p3.y)));
        }
    }
    if let Some(&l) = pts.last() {
        out.push(l);
    }
    out
}

/// A least-squares parabola through `pts`, sampled at `n` points from the first point's
/// projection to the last's. The parabola is fitted along the direction from the first point to
/// the last (the flight's run), so a lob that goes up and comes down is one arc. `None` with
/// fewer than three distinct points.
pub fn parabola(pts: &[Vec2], n: usize) -> Option<Vec<Vec2>> {
    let (&a, &b) = (pts.first()?, pts.last()?);
    let dir = b - a;
    let len = dir.length();
    if pts.len() < 3 || !(len.is_finite() && len > 1e-6) {
        return None;
    }
    let u = dir * (1.0 / len);
    let nrm = Vec2::new(-u.y, u.x);
    // coordinates along the run (s) and across it (d)
    let sd: Vec<(f64, f64)> = pts.iter().map(|p| (dot(*p - a, u), dot(*p - a, nrm))).collect();
    // normal equations for d = c0 + c1·s + c2·s², scaled by the run length for conditioning
    let mut m = [[0.0f64; 3]; 3];
    let mut r = [0.0f64; 3];
    for &(s, d) in &sd {
        let x = s / len;
        let row = [1.0, x, x * x];
        for i in 0..3 {
            for j in 0..3 {
                m[i][j] += row[i] * row[j];
            }
            r[i] += row[i] * d;
        }
    }
    let c = solve3(m, r)?;
    let n = n.clamp(2, 512);
    let (s0, s1) = (sd.first()?.0, sd.last()?.0);
    Some(
        (0..n)
            .map(|k| {
                let s = s0 + (s1 - s0) * k as f64 / (n - 1) as f64;
                let x = s / len;
                let d = c[0] + c[1] * x + c[2] * x * x;
                a + u * s + nrm * d
            })
            .collect(),
    )
}

fn dot(a: Vec2, b: Vec2) -> f64 {
    a.x * b.x + a.y * b.y
}

/// Gaussian elimination with partial pivoting; `None` for a singular system.
fn solve3(mut m: [[f64; 3]; 3], mut r: [f64; 3]) -> Option<[f64; 3]> {
    for col in 0..3 {
        let piv = (col..3).max_by(|&i, &j| m[i][col].abs().total_cmp(&m[j][col].abs()))?;
        if m[piv][col].abs() < 1e-12 {
            return None;
        }
        m.swap(col, piv);
        r.swap(col, piv);
        for row in col + 1..3 {
            let f = m[row][col] / m[col][col];
            for k in col..3 {
                m[row][k] -= f * m[col][k];
            }
            r[row] -= f * r[col];
        }
    }
    let mut x = [0.0f64; 3];
    for i in (0..3).rev() {
        let s: f64 = (i + 1..3).map(|k| m[i][k] * x[k]).sum();
        x[i] = (r[i] - s) / m[i][i];
    }
    x.iter().all(|v| v.is_finite()).then_some(x)
}

/// Total length of a polyline.
pub fn length(pts: &[Vec2]) -> f64 {
    pts.windows(2).map(|w| (w[1] - w[0]).length()).sum()
}

/// `n + 1` points evenly spaced along the polyline `pts` by arc length (`None` when it has no
/// length).
pub fn resample(pts: &[Vec2], n: usize) -> Option<Vec<Vec2>> {
    let total = length(pts);
    if !(total.is_finite() && total > 1e-9) || n == 0 {
        return None;
    }
    let mut out = Vec::with_capacity(n + 1);
    let mut seg = 0usize;
    let mut acc = 0.0f64;
    for k in 0..=n {
        let want = total * k as f64 / n as f64;
        while seg + 1 < pts.len() - 1 && acc + (pts[seg + 1] - pts[seg]).length() < want {
            acc += (pts[seg + 1] - pts[seg]).length();
            seg += 1;
        }
        let (a, b) = (pts[seg], pts[(seg + 1).min(pts.len() - 1)]);
        let l = (b - a).length();
        let t = if l > 1e-12 { ((want - acc) / l).clamp(0.0, 1.0) } else { 0.0 };
        out.push(a + (b - a) * t);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gaps_are_filled_and_ends_held() {
        assert_eq!(fill_gaps(&[None, Some(2.0), None, None, Some(8.0), None]), Some(vec![2.0, 2.0, 4.0, 6.0, 8.0, 8.0]));
        assert_eq!(fill_gaps(&[None, None]), None);
        assert_eq!(fill_gaps(&[]), None);
    }

    #[test]
    fn a_jittery_still_subject_gives_a_still_camera() {
        let v: Vec<f64> = (0..300).map(|k| 500.0 + 6.0 * ((k as f64) * 1.7).sin()).collect();
        let cam = spring_smooth(&dead_zone(&v, 10.0), 0.1, 0.8);
        let spread = cam.iter().cloned().fold(f64::MIN, f64::max) - cam.iter().cloned().fold(f64::MAX, f64::min);
        assert!(spread < 1e-9, "{spread}");
    }

    #[test]
    fn smoothing_is_zero_phase_and_speed_is_bounded() {
        // a step from 0 to 100 at sample 50: the smoothed path crosses 50 at the step
        let v: Vec<f64> = (0..100).map(|k| if k < 50 { 0.0 } else { 100.0 }).collect();
        let s = spring_smooth(&v, 0.1, 0.5);
        let cross = s.iter().position(|x| *x >= 50.0).unwrap();
        assert!((48..=51).contains(&cross), "{cross}");
        let l = limit_speed(&s, 3.0);
        assert!(l.windows(2).all(|w| (w[1] - w[0]).abs() <= 3.0 + 1e-9));
        assert!(spring_smooth(&v, f64::NAN, 1.0) == v && limit_speed(&v, -1.0) == v);
        // a steady pan is followed to both ends
        let ramp: Vec<f64> = (0..50).map(|k| 10.0 * k as f64).collect();
        let r = spring_smooth(&ramp, 0.1, 0.8);
        assert!(r.iter().zip(&ramp).all(|(a, b)| (a - b).abs() < 0.5), "{r:?}");
        assert_eq!(spring_smooth(&[3.0], 0.1, 0.5), vec![3.0]);
    }

    #[test]
    fn thinning_keeps_the_shape() {
        let v: Vec<Vec<f64>> = (0..100).map(|k| vec![if k < 50 { k as f64 } else { 50.0 }, 3.0]).collect();
        let t: Vec<f64> = (0..100).map(f64::from).collect();
        assert_eq!(thin(&t, &v, 0.01), vec![0, 50, 99]);
        assert_eq!(thin(&[], &[], 1.0), Vec::<usize>::new());
    }

    #[test]
    fn parabola_fits_a_lob() {
        let pts: Vec<Vec2> = (0..9).map(|k| Vec2::new(100.0 + 50.0 * k as f64, 400.0 - 60.0 * k as f64 + 7.5 * (k * k) as f64)).collect();
        let arc = parabola(&pts, 9).unwrap();
        for (a, b) in arc.iter().zip(&pts) {
            assert!((*a - *b).length() < 1e-6, "{a:?} {b:?}");
        }
        assert!(parabola(&pts[..2], 5).is_none());
        assert!(parabola(&[Vec2::ZERO, Vec2::ZERO, Vec2::ZERO], 5).is_none());
        let even = resample(&[Vec2::ZERO, Vec2::new(10.0, 0.0), Vec2::new(10.0, 10.0)], 4).unwrap();
        assert!((even[2] - Vec2::new(10.0, 0.0)).length() < 1e-9, "{even:?}");
        assert!(resample(&[Vec2::ZERO, Vec2::ZERO], 4).is_none());
        assert!(catmull_rom(&pts, 4).len() == 8 * 4 + 1);
    }
}
