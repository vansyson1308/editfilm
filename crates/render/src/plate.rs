//! Clean plates: the background of a locked-off shot with whatever moved through it taken out.
//!
//! [`temporal_median`] takes the per-pixel median of frames sampled across a shot. Where the camera
//! does not move and a subject passes through, each background pixel is uncovered in most of the
//! samples, so the median is the background. It is a statistic, not an invention: a pixel the
//! subject covers in half the samples or more keeps the subject, and a moving camera gives a smear.
//! [`Stillness`] says how much of the picture the samples agree on, so the caller can warn.

use rayon::prelude::*;

/// Most frames one plate is made from.
pub const MAX_PLATE_SAMPLES: usize = 31;

/// How well the samples of a plate agree.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stillness {
    /// Fraction of pixels where at least three quarters of the samples lie close to the median
    /// (0…1). Near 1: a locked-off shot. Low: the camera moves, and the plate is not usable.
    pub settled: f32,
    /// Fraction of pixels where some sample differs clearly from the median (0…1): how much of
    /// the frame something moved through.
    pub disturbed: f32,
}

/// The per-pixel, per-channel median of RGBA8 frames of `w`×`h` pixels, and how well they agreed.
/// `None` when fewer than three frames are given, more than [`MAX_PLATE_SAMPLES`], or a frame is
/// not `w * h * 4` bytes.
pub fn temporal_median(frames: &[Vec<u8>], w: usize, h: usize) -> Option<(Vec<u8>, Stillness)> {
    let n = frames.len();
    let len = w.checked_mul(h)?.checked_mul(4)?;
    if !(3..=MAX_PLATE_SAMPLES).contains(&n) || len == 0 || frames.iter().any(|f| f.len() != len) {
        return None;
    }
    let mut out = Vec::new();
    out.try_reserve_exact(len).ok()?;
    out.resize(len, 0u8);
    // close: within 8 of 255, what a locked-off camera's noise and compression stay inside
    const CLOSE: u8 = 8;
    let counts: (usize, usize) = out
        .par_chunks_mut(4)
        .enumerate()
        .map(|(i, o)| {
            let mut near = n;
            let mut far = false;
            for c in 0..4 {
                let mut v = [0u8; MAX_PLATE_SAMPLES];
                for (slot, f) in v.iter_mut().zip(frames) {
                    *slot = f.get(i * 4 + c).copied().unwrap_or(0);
                }
                let s = &mut v[..n];
                s.sort_unstable();
                let m = if n % 2 == 1 { s[n / 2] } else { ((u16::from(s[n / 2 - 1]) + u16::from(s[n / 2])) / 2) as u8 };
                o[c] = m;
                if c < 3 {
                    let close = s.iter().filter(|x| x.abs_diff(m) <= CLOSE).count();
                    near = near.min(close);
                    far |= close < n;
                }
            }
            (usize::from(near * 4 >= n * 3), usize::from(far))
        })
        .reduce(|| (0, 0), |a, b| (a.0 + b.0, a.1 + b.1));
    let total = (w * h).max(1) as f32;
    Some((out, Stillness { settled: counts.0 as f32 / total, disturbed: counts.1 as f32 / total }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: usize, h: usize, bg: u8, blob: Option<(usize, usize)>) -> Vec<u8> {
        let mut px = vec![bg; w * h * 4];
        px.chunks_mut(4).for_each(|p| p[3] = 255);
        if let Some((bx, by)) = blob {
            for y in by..(by + 4).min(h) {
                for x in bx..(bx + 4).min(w) {
                    px[(y * w + x) * 4..(y * w + x) * 4 + 3].copy_from_slice(&[255, 0, 0]);
                }
            }
        }
        px
    }

    #[test]
    fn a_subject_crossing_a_locked_off_shot_is_taken_out() {
        // a red square moves across a grey frame: it covers any pixel in one sample of seven
        let frames: Vec<Vec<u8>> = (0..7).map(|i| frame(32, 16, 60, Some((i * 4, 6)))).collect();
        let (plate, still) = temporal_median(&frames, 32, 16).unwrap();
        assert_eq!(plate, frame(32, 16, 60, None), "the plate is the empty background");
        assert!(still.settled > 0.99, "{still:?}");
        assert!(still.disturbed > 0.1 && still.disturbed < 0.4, "the square's path is what moved: {still:?}");
    }

    #[test]
    fn a_subject_that_stays_put_stays_in_the_plate() {
        let frames: Vec<Vec<u8>> = (0..5).map(|_| frame(16, 16, 60, Some((6, 6)))).collect();
        let (plate, still) = temporal_median(&frames, 16, 16).unwrap();
        assert_eq!(plate, frame(16, 16, 60, Some((6, 6))));
        assert_eq!(still.disturbed, 0.0);
    }

    #[test]
    fn camera_noise_is_not_movement_but_a_changing_picture_is() {
        let noisy: Vec<Vec<u8>> = (0..6).map(|i| frame(8, 8, 100 + i as u8, None)).collect();
        let (_, still) = temporal_median(&noisy, 8, 8).unwrap();
        assert_eq!((still.settled, still.disturbed), (1.0, 0.0), "a few levels of noise are the same picture");
        let moving: Vec<Vec<u8>> = (0..6).map(|i| frame(8, 8, i as u8 * 40, None)).collect();
        let (_, still) = temporal_median(&moving, 8, 8).unwrap();
        assert!(still.settled < 0.01 && still.disturbed > 0.99, "{still:?}");
    }

    #[test]
    fn an_even_number_of_samples_takes_the_middle_pair() {
        let frames = vec![frame(2, 2, 10, None), frame(2, 2, 20, None), frame(2, 2, 30, None), frame(2, 2, 250, None)];
        let (plate, _) = temporal_median(&frames, 2, 2).unwrap();
        assert_eq!(plate[0], 25);
        assert_eq!(plate[3], 255, "alpha is carried through");
    }

    #[test]
    fn hostile_inputs_are_refused_not_indexed() {
        assert!(temporal_median(&[], 4, 4).is_none());
        assert!(temporal_median(&[frame(4, 4, 9, None), frame(4, 4, 9, None)], 4, 4).is_none(), "two frames have no median worth the name");
        assert!(temporal_median(&[frame(4, 4, 9, None), frame(4, 4, 9, None), frame(5, 4, 9, None)], 4, 4).is_none(), "sizes differ");
        let many: Vec<Vec<u8>> = (0..MAX_PLATE_SAMPLES + 1).map(|_| frame(2, 2, 9, None)).collect();
        assert!(temporal_median(&many, 2, 2).is_none());
        let three = vec![frame(4, 4, 9, None); 3];
        assert!(temporal_median(&three, 0, 4).is_none());
        assert!(temporal_median(&three, 4, 5).is_none(), "the stated size is not the buffers' size");
        assert!(temporal_median(&three, usize::MAX, usize::MAX).is_none(), "a size whose byte count overflows");
    }
}
