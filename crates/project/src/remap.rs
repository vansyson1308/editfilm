//! Time Remapping: a clip whose Speed is keyframed plays its media at a varying rate.
//!
//! The intrinsic `time_remap` effect's `speed` parameter (per cent) is keyframed in the same clock
//! as every other clip keyframe: the clip's media time at its constant speed (the *linear* media
//! time, [`TrackItem::linear_source_time_at`]). The remap turns that linear media time `b` into
//! the media time actually shown:
//!
//! ```text
//! Φ(b) = b₀ + ∫ from b₀ to b of r(β)/100 dβ
//! ```
//!
//! where `r` is the keyframed speed and `b₀` is the time of the first keyframe in play order (the
//! last in media time for a reversed clip), the anchor where shown and linear media time agree. So the clip's absolute playback speed at a moment is its clip
//! speed times `r/100`. Before the first keyframe the speed is the first keyframe's; after the
//! last, the last one's. A speed of 0 holds the frame.
//!
//! Anchoring at a keyframe (not at the clip's In point) keeps a ramp in place when the clip is cut
//! or its head trimmed: those edits keep the linear media time of every remaining moment, so both
//! pieces of a razored ramp show exactly the frames the whole clip showed.
//!
//! A constant (unkeyed) remap speed has no anchor and changes nothing: a constant speed is the
//! clip's own Speed (Speed/Duration), which every importer maps a constant remap to.
//!
//! The integral is exact (closed form) on linear and hold segments, up to f64 rounding (tick
//! counts are exact below 2^53, about 410 days). Eased (Bezier) segments use Simpson's rule on
//! 64 panels of a curve whose ease is solved to a residual of 1e-7: the error is then of the
//! order of 1e-7 of the segment's length (about 60 ns on a 1 s eased segment), well under a
//! frame at any rate.

use filmagic_time::Tick;

use crate::keyframe::{Interpolation, Keyframe, Param, ParamValue, segment_eases, segment_value};
use crate::track::TrackItem;

/// Keyframes a remap curve is read from at most. A project file may hold any number; the curve
/// is evaluated a few times a frame, so a hostile one cannot make every frame slow. A ramp made by
/// `clip.speedRamp` has far fewer.
pub const MAX_REMAP_KEYS: usize = 256;
/// Highest remap speed honoured, per cent (1000×). Larger values are read as this.
pub const MAX_REMAP_PERCENT: f64 = 100_000.0;
/// Simpson panels (even) on an eased segment.
const PANELS: usize = 64;

/// The Time Remapping curve of one clip (see the module docs).
#[derive(Clone, Debug)]
pub struct Remap<'a> {
    keys: &'a [Keyframe],
    /// The clip plays its media backwards: its first keyframe in play order is the last in time.
    reverse: bool,
}

/// Speed factor (1.0 = the clip's own speed) of a stored remap value. Hostile values: NaN and
/// anything that is not a number read as 100 % (1.0), negative values and -∞ as a hold (0), and
/// values over the cap (+∞ included) as the cap.
fn factor(v: &ParamValue) -> f64 {
    let p = v.as_f64().unwrap_or(100.0);
    if p.is_nan() {
        return 1.0;
    }
    p.clamp(0.0, MAX_REMAP_PERCENT) / 100.0
}

impl<'a> Remap<'a> {
    /// The remap curve of `param` when it is keyframed with times in order, for a clip playing
    /// its media forward or (`reverse`) backward.
    pub fn of_param(param: &'a Param, reverse: bool) -> Option<Remap<'a>> {
        if param.keyframes.is_empty() {
            return None;
        }
        let keys = param.keyframes.get(..param.keyframes.len().min(MAX_REMAP_KEYS))?;
        if !keys.windows(2).all(|w| w[0].time < w[1].time) {
            // a hand-edited file out of order: not a curve we can integrate
            return None;
        }
        // keys that all say 100 % change nothing (and must not turn on frame blending)
        if keys.iter().all(|k| factor(&k.value) == 1.0) {
            return None;
        }
        Some(Remap { keys, reverse })
    }

    /// The anchor: linear media time where shown media time equals it, the first keyframe in
    /// play order.
    pub fn anchor(&self) -> Tick {
        let k = if self.reverse { self.keys.last() } else { self.keys.first() };
        k.map_or(Tick::ZERO, |k| k.time)
    }

    /// Speed factor at linear media time `b` (1.0 = the clip's own speed).
    pub fn factor_at(&self, b: Tick) -> f64 {
        let k = self.keys;
        let (Some(first), Some(last)) = (k.first(), k.last()) else { return 1.0 };
        if b <= first.time {
            return factor(&first.value);
        }
        if b >= last.time {
            return factor(&last.value);
        }
        let i = k.partition_point(|x| x.time <= b).saturating_sub(1);
        match (k.get(i), k.get(i + 1)) {
            (Some(a), Some(c)) => {
                let u = (b - a.time).0 as f64 / (c.time - a.time).0.max(1) as f64;
                seg_factor(a, c, u)
            }
            _ => factor(&last.value),
        }
    }

    /// Integral of the factor over `[from, to]` (linear media ticks; negative when `to < from`).
    fn integral(&self, from: Tick, to: Tick) -> f64 {
        if to < from {
            return -self.integral(to, from);
        }
        let k = self.keys;
        let (Some(first), Some(last)) = (k.first(), k.last()) else { return (to - from).0 as f64 };
        let mut sum = 0.0;
        // before the first key and after the last: constant
        if from < first.time {
            let end = to.min(first.time);
            sum += (end - from).0 as f64 * factor(&first.value);
        }
        if to > last.time {
            let start = from.max(last.time);
            sum += (to - start).0 as f64 * factor(&last.value);
        }
        let (lo, hi) = (from.max(first.time), to.min(last.time));
        if lo >= hi {
            return sum;
        }
        let start = k.partition_point(|x| x.time <= lo).saturating_sub(1);
        for w in k.get(start..).unwrap_or(&[]).windows(2) {
            let (a, c) = (&w[0], &w[1]);
            if a.time >= hi {
                break;
            }
            let (s0, s1) = (lo.max(a.time), hi.min(c.time));
            if s1 > s0 {
                sum += seg_integral(a, c, s0, s1);
            }
        }
        sum
    }

    /// Shown media time at linear media time `b`.
    pub fn shown(&self, b: Tick) -> Tick {
        let a = self.anchor();
        let v = a.0 as f64 + self.integral(a, b);
        to_tick(v)
    }

    /// The smallest linear media time whose shown media time reaches `target`, searched upward
    /// from `from` (or downward when `down`), or `None` when the curve never gets there (it ends
    /// in a hold) within the tick range.
    pub fn linear_reaching(&self, from: Tick, target: Tick, down: bool) -> Option<Tick> {
        let reached = |b: Tick| if down { self.shown(b) <= target } else { self.shown(b) >= target };
        if reached(from) {
            return Some(from);
        }
        // grow a bracket, then bisect
        let mut step: i64 = 1 << 20;
        let mut lo = from;
        let mut hi;
        loop {
            let next = if down { lo.0.checked_sub(step) } else { lo.0.checked_add(step) }?;
            hi = Tick(next);
            if hi.0.unsigned_abs() > (Tick::MAX.0 / 4) as u64 {
                return None;
            }
            if reached(hi) {
                break;
            }
            lo = hi;
            step = step.checked_mul(2)?;
        }
        // invariant: !reached(lo), reached(hi)
        for _ in 0..80 {
            let gap = hi.0.abs_diff(lo.0);
            if gap <= 1 {
                break;
            }
            let mid = Tick(lo.0 + (hi.0 - lo.0) / 2);
            if reached(mid) { hi = mid } else { lo = mid }
        }
        Some(hi)
    }
}

/// Speed factor at `u` (0..1) of the segment from `a` to `c`.
fn seg_factor(a: &Keyframe, c: &Keyframe, u: f64) -> f64 {
    let fa = factor(&a.value);
    if a.interp == Interpolation::Hold {
        return fa;
    }
    let p = segment_value(a, c, u.clamp(0.0, 1.0), a.value.as_f64().unwrap_or(100.0), c.value.as_f64().unwrap_or(100.0));
    if p.is_nan() {
        return fa;
    }
    p.clamp(0.0, MAX_REMAP_PERCENT) / 100.0
}

/// Integral of the factor over `[s0, s1]` inside the segment from `a` to `c`.
fn seg_integral(a: &Keyframe, c: &Keyframe, s0: Tick, s1: Tick) -> f64 {
    let span = (c.time - a.time).0.max(1) as f64;
    let (u0, u1) = ((s0 - a.time).0 as f64 / span, (s1 - a.time).0 as f64 / span);
    let len = (s1 - s0).0 as f64;
    if a.interp == Interpolation::Hold {
        return len * factor(&a.value);
    }
    let (fa, fc) = (factor(&a.value), factor(&c.value));
    let clamped = |x: f64| x.clamp(0.0, MAX_REMAP_PERCENT / 100.0);
    if !segment_eases(a, c) {
        // linear: the trapezoid is exact, unless the line leaves [0, cap] inside the segment
        let raw = |u: f64| fa + (fc - fa) * u;
        let (v0, v1) = (raw(u0), raw(u1));
        if v0 == clamped(v0) && v1 == clamped(v1) {
            return len * (v0 + v1) / 2.0;
        }
    }
    // Simpson's rule on PANELS panels
    let h = (u1 - u0) / PANELS as f64;
    let mut acc = seg_factor(a, c, u0) + seg_factor(a, c, u1);
    for i in 1..PANELS {
        let w = if i % 2 == 1 { 4.0 } else { 2.0 };
        acc += w * seg_factor(a, c, u0 + h * i as f64);
    }
    len * acc / (3.0 * PANELS as f64)
}

fn to_tick(v: f64) -> Tick {
    if v.is_nan() {
        return Tick::ZERO;
    }
    let cap = (Tick::MAX.0 / 2) as f64;
    Tick(v.clamp(-cap, cap).round() as i64)
}

impl TrackItem {
    /// The clip's Time Remapping curve when its Speed is keyframed (see [`crate::remap`]).
    pub fn remap(&self) -> Option<Remap<'_>> {
        let e = self.effect("time_remap").filter(|e| e.enabled)?;
        Remap::of_param(e.param("speed")?, self.reverse)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::intrinsic_video;
    use filmagic_time::TICKS_PER_SECOND;

    const S: i64 = TICKS_PER_SECOND;

    fn secs(x: f64) -> Tick {
        Tick((x * S as f64).round() as i64)
    }

    fn clip(start: f64, dur: f64, src_in: f64, speed: f64, reverse: bool) -> TrackItem {
        TrackItem {
            custom_label_color: None,
            id: crate::ClipId(1),
            item: crate::ItemId(2),
            name: "c".into(),
            label: crate::Label::Iris,
            start: secs(start),
            duration: secs(dur),
            source_in: secs(src_in),
            speed,
            reverse,
            enabled: true,
            link: None,
            group: None,
            effects: intrinsic_video(),
            markers: vec![],
            gain_db: 0.0,
            frame_hold: None,
            scale_to_frame: false,
            essential: None,
            multicam: None,
            time_interpolation: Default::default(),
            maintain_pitch: false,
            hold_filters: false,
            field_options: None,
            source_channels: Vec::new(),
            audio_stream: 0,
            graphic: None,
        }
    }

    /// Keyframes (linear media seconds, per cent, interpolation) on the clip's remap speed.
    fn keys(it: &mut TrackItem, k: &[(f64, f64, Interpolation)]) {
        let p = it.effect_mut("time_remap").and_then(|e| e.param_mut("speed")).unwrap();
        p.keyframes = k
            .iter()
            .map(|&(t, v, interp)| {
                let mut kf = Keyframe::new(secs(t), ParamValue::Float(v));
                kf.interp = interp;
                kf
            })
            .collect();
    }

    fn close(a: Tick, b: Tick, tol: i64) -> bool {
        (a.0 - b.0).abs() <= tol
    }

    #[test]
    fn unkeyed_remap_changes_nothing() {
        let mut it = clip(10.0, 4.0, 2.0, 1.0, false);
        it.effect_mut("time_remap").and_then(|e| e.param_mut("speed")).unwrap().value = ParamValue::Float(40.0);
        assert!(!it.has_speed_ramp());
        assert_eq!(it.source_time_at(secs(11.0)), secs(3.0));
        assert_eq!(it.source_out(), secs(6.0));
    }

    #[test]
    fn linear_ramp_is_the_hand_computed_integral() {
        // 100 % for 1 s, linear down to 40 % over the next second, then 40 %
        let mut it = clip(0.0, 4.0, 0.0, 1.0, false);
        keys(&mut it, &[(0.0, 100.0, Interpolation::Linear), (1.0, 100.0, Interpolation::Linear), (2.0, 40.0, Interpolation::Linear)]);
        assert!(it.has_speed_ramp());
        assert_eq!(it.source_time_at(secs(0.5)), secs(0.5));
        assert_eq!(it.source_time_at(secs(1.0)), secs(1.0));
        // 1 + ∫0..0.5 (1 - 0.6u) du = 1 + 0.5 - 0.075
        assert!(close(it.source_time_at(secs(1.5)), secs(1.425), 1));
        assert!(close(it.source_time_at(secs(2.0)), secs(1.7), 1));
        assert!(close(it.source_time_at(secs(3.0)), secs(2.1), 1));
        assert!(close(it.source_out(), secs(2.5), 1));
    }

    #[test]
    fn eased_ramp_averages_its_ends_and_eases() {
        let mut it = clip(0.0, 4.0, 0.0, 1.0, false);
        keys(&mut it, &[(0.0, 100.0, Interpolation::Linear), (1.0, 100.0, Interpolation::AutoBezier), (2.0, 40.0, Interpolation::EaseIn)]);
        // a symmetric S-curve: the mean speed is the mean of its ends
        let tol = S / 100_000; // 10 µs
        assert!(close(it.source_time_at(secs(2.0)), secs(1.7), tol), "{:?}", it.source_time_at(secs(2.0)).seconds());
        assert!(close(it.source_time_at(secs(3.0)), secs(2.1), tol));
        // the speed leaves 100 % gently: just after the key it is still near 100 %
        let r = it.remap().unwrap();
        assert!(r.factor_at(secs(1.05)) > 0.98, "{}", r.factor_at(secs(1.05)));
        assert!((r.factor_at(secs(1.5)) - 0.7).abs() < 1e-9);
    }

    #[test]
    fn a_zero_speed_holds_the_frame() {
        let mut it = clip(0.0, 4.0, 0.0, 1.0, false);
        keys(&mut it, &[(0.0, 100.0, Interpolation::Hold), (1.0, 0.0, Interpolation::Hold), (2.0, 100.0, Interpolation::Linear)]);
        assert_eq!(it.source_time_at(secs(1.0)), secs(1.0));
        assert_eq!(it.source_time_at(secs(1.5)), secs(1.0));
        assert_eq!(it.source_time_at(secs(1.999)), secs(1.0));
        assert_eq!(it.source_time_at(secs(2.5)), secs(1.5));
    }

    #[test]
    fn clip_speed_scales_the_curve() {
        // a 200 % clip with a 50 % remap plays at 100 %
        let mut it = clip(0.0, 2.0, 0.0, 2.0, false);
        keys(&mut it, &[(0.0, 50.0, Interpolation::Linear)]);
        assert_eq!(it.source_time_at(secs(1.0)), secs(1.0));
        assert_eq!(it.source_out(), secs(2.0));
    }

    #[test]
    fn razoring_keeps_every_frame() {
        // both pieces of a cut clip keep the linear media time, and so the ramp
        let mut it = clip(5.0, 4.0, 3.0, 1.0, false);
        keys(&mut it, &[(3.0, 100.0, Interpolation::AutoBezier), (4.0, 30.0, Interpolation::AutoBezier), (5.5, 150.0, Interpolation::Linear)]);
        let cut = secs(6.3);
        let (mut left, mut right) = (it.clone(), it.clone());
        left.duration = cut - it.start;
        right.start = cut;
        right.duration = it.end() - cut;
        right.source_in += Tick(((cut - it.start).0 as f64 * it.speed).round() as i64);
        for k in 0..400 {
            let t = it.start + Tick(it.duration.0 * k / 400);
            let piece = if t < cut { &left } else { &right };
            assert_eq!(piece.source_time_at(t), it.source_time_at(t), "at {}", t.seconds());
        }
    }

    #[test]
    fn reversed_clip_anchors_at_its_start() {
        // a reversed 4 s clip of media 0..4 s, ramped from 100 % at its start to 50 % after 1 s
        let mut it = clip(0.0, 4.0, 0.0, 1.0, true);
        let top = it.linear_source_time_at(it.start);
        let one = it.linear_source_time_at(secs(1.0));
        keys(&mut it, &[(one.seconds(), 50.0, Interpolation::Linear), (top.seconds(), 100.0, Interpolation::Linear)]);
        assert_eq!(it.source_time_at(it.start), top, "the clip starts on the frame it started on");
        // 1 s at a mean of 75 %, then 50 %: 0.75 + 0.5 s of media back from the top
        assert!(close(it.source_time_at(secs(2.0)), top - secs(1.25), 2));
        assert!(it.source_time_at(secs(3.0)) < it.source_time_at(secs(2.0)));
    }

    #[test]
    fn hostile_curves_never_panic() {
        let mut it = clip(0.0, 4.0, 0.0, 1.0, false);
        for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -5.0, 1e300, 0.0] {
            keys(&mut it, &[(0.0, v, Interpolation::Bezier), (1e-9, v, Interpolation::Linear), (9e6, -v, Interpolation::Hold)]);
            let _ = (it.source_time_at(secs(3.0)), it.source_out());
        }
        // out of order keys are not a curve
        keys(&mut it, &[(2.0, 50.0, Interpolation::Linear), (1.0, 50.0, Interpolation::Linear)]);
        assert!(!it.has_speed_ramp());
        // a text value
        let p = it.effect_mut("time_remap").and_then(|e| e.param_mut("speed")).unwrap();
        p.keyframes = vec![Keyframe::new(Tick(0), ParamValue::Text("x".into())), Keyframe::new(Tick(9), ParamValue::Bool(true))];
        let _ = it.source_time_at(secs(1.0));
        // many keys are read up to the cap
        let p = it.effect_mut("time_remap").and_then(|e| e.param_mut("speed")).unwrap();
        p.keyframes = (0..10_000).map(|i| Keyframe::new(Tick(i * 1000), ParamValue::Float(50.0))).collect();
        assert!(it.remap().is_some_and(|r| r.keys.len() == MAX_REMAP_KEYS));
        // a curve that ends in a hold never reaches a later media time
        keys(&mut it, &[(0.0, 100.0, Interpolation::Linear), (1.0, 0.0, Interpolation::Linear)]);
        assert_eq!(it.remap().unwrap().linear_reaching(Tick::ZERO, secs(5.0), false), None);
        // shown(b) = b - b²/2 reaches 0.25 s at b = 1 - √0.5
        assert!(close(it.remap().unwrap().linear_reaching(Tick::ZERO, secs(0.25), false).unwrap(), secs(1.0 - 0.5f64.sqrt()), 2));
    }
}
