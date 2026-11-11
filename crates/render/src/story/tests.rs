use super::*;
use crate::scene::signature;

/// A `w`×`h` RGBA8 frame of grey `bg` with `rects` (x, y, w, h, grey) painted on it.
fn frame(w: usize, h: usize, bg: u8, rects: &[(usize, usize, usize, usize, u8)]) -> Vec<u8> {
    let mut px = vec![bg; w * h * 4];
    px.chunks_mut(4).for_each(|p| p[3] = 255);
    for &(rx, ry, rw, rh, v) in rects {
        for y in ry..(ry + rh).min(h) {
            for x in rx..(rx + rw).min(w) {
                px[(y * w + x) * 4..(y * w + x) * 4 + 3].fill(v);
            }
        }
    }
    px
}

/// A frame with vertical structure everywhere (so a horizontal shift is measurable), moved `shift` pixels right.
fn textured(w: usize, h: usize, shift: isize) -> Vec<u8> {
    let mut px = vec![255u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let u = (x as isize - shift).rem_euclid(w as isize) as f32 / w as f32;
            // two incommensurate waves: no shift but the true one lines the profile up
            let v = 0.5 + 0.3 * (u * 6.0 * std::f32::consts::TAU).sin() + 0.2 * (u * 2.3 * std::f32::consts::TAU + 1.0).sin();
            px[(y * w + x) * 4..(y * w + x) * 4 + 3].fill((v.clamp(0.0, 1.0) * 255.0) as u8);
        }
    }
    px
}

fn glance(px: &[u8], w: usize, h: usize) -> Glance {
    Glance::from_rgba(px, w, h)
}

#[test]
fn a_glance_survives_hostile_frames() {
    let black = Glance::from_rgba(&[], 0, 0);
    assert_eq!(black.mean(), 0.0);
    // the stated size is larger than the buffer
    assert_eq!(Glance::from_rgba(&[255; 16], 100, 100).mean(), 0.0);
    // a size whose byte count overflows
    assert_eq!(Glance::from_rgba(&[255; 16], usize::MAX, usize::MAX).mean(), 0.0);
    // a frame smaller than the grid fills every cell from its nearest pixel
    let tiny = Glance::from_rgba(&frame(2, 2, 255, &[]), 2, 2);
    assert!((tiny.mean() - 1.0).abs() < 1e-4, "{}", tiny.mean());
    let one = Glance::from_rgba(&frame(1, 1, 128, &[]), 1, 1);
    assert!((one.mean() - 128.0 / 255.0).abs() < 1e-3);
}

#[test]
fn mean_and_contrast_read_the_picture() {
    let flat = glance(&frame(128, 72, 100, &[]), 128, 72);
    assert!((flat.mean() - 100.0 / 255.0).abs() < 1e-3);
    assert!(flat.contrast() < 1e-4);
    let split = glance(&frame(128, 72, 0, &[(0, 0, 64, 72, 255)]), 128, 72);
    assert!((split.mean() - 0.5).abs() < 0.02);
    assert!((split.contrast() - 0.5).abs() < 0.02);
}

#[test]
fn attention_rests_on_what_stands_out() {
    let left = glance(&frame(256, 144, 40, &[(20, 60, 24, 24, 255)]), 256, 144);
    let a = attention(&left, None);
    assert!(a.x < 0.25 && (a.y - 0.5).abs() < 0.15, "{a:?}");
    assert!(a.strength > 0.0 && a.spread < 0.2, "{a:?}");
    let right = glance(&frame(256, 144, 40, &[(212, 60, 24, 24, 255)]), 256, 144);
    let b = attention(&right, None);
    assert!(b.x > 0.75, "{b:?}");
    assert!(eye_travel(a, b) > 0.4, "the eye crosses the frame: {}", eye_travel(a, b));
    assert!(eye_travel(a, a) < 1e-6);
}

#[test]
fn a_flat_frame_gives_the_eye_nothing_and_says_so() {
    let a = attention(&glance(&frame(64, 36, 90, &[]), 64, 36), None);
    assert_eq!((a.x, a.y, a.strength), (0.5, 0.5, 0.0));
    let black = attention(&Glance::from_rgba(&[], 0, 0), None);
    assert_eq!(black.strength, 0.0);
}

#[test]
fn the_eye_follows_what_moves_over_what_merely_stands_out() {
    // a big bright static shape on the right, a small dim one that moves on the left
    let prev = glance(&frame(256, 144, 40, &[(180, 40, 60, 60, 255), (30, 70, 12, 12, 110)]), 256, 144);
    let cur = glance(&frame(256, 144, 40, &[(180, 40, 60, 60, 255), (48, 70, 12, 12, 110)]), 256, 144);
    let still = attention(&cur, None);
    let moving = attention(&cur, Some(&prev));
    assert!(still.x > 0.6, "without motion the bright shape leads: {still:?}");
    assert!(moving.x < still.x - 0.15, "with motion the eye is pulled toward the mover: {moving:?} vs {still:?}");
}

#[test]
fn drift_measures_a_pan_and_its_direction() {
    let (w, h) = (256, 144);
    let a = glance(&textured(w, h, 0), w, h);
    let right = glance(&textured(w, h, 16), w, h);
    let left = glance(&textured(w, h, -16), w, h);
    let d = drift(&a, &right);
    assert!((d.dx - 16.0 / 256.0).abs() <= 1.0 / GW as f32, "{d:?}");
    assert!(d.confidence > 0.5, "{d:?}");
    assert_eq!(screen_direction(d), 1);
    assert_eq!(screen_direction(drift(&a, &left)), -1);
    let none = drift(&a, &a);
    assert_eq!((none.dx, none.dy), (0.0, 0.0));
    assert_eq!(screen_direction(none), 0);
    // a flat frame has no direction, whatever it is compared with
    let flat = glance(&frame(w, h, 90, &[]), w, h);
    assert_eq!(screen_direction(drift(&flat, &flat)), 0);
}

#[test]
fn a_change_is_a_match_a_jump_or_a_clean_cut() {
    let (w, h) = (256, 144);
    let wide = frame(w, h, 60, &[(100, 40, 50, 80, 220)]);
    let same = wide.clone();
    // the same setup with the subject hopped sideways: the jump cut
    let hop = frame(w, h, 60, &[(128, 40, 50, 80, 220)]);
    // another picture altogether
    let other = frame(w, h, 200, &[(0, 0, 256, 50, 20), (10, 90, 200, 40, 90)]);
    let d = |a: &[u8], b: &[u8]| crate::scene::difference(&signature(a, w, h), &signature(b, w, h));
    assert_eq!(Change::of(d(&wide, &same)), Change::Match);
    assert_eq!(Change::of(d(&wide, &hop)), Change::Jump, "difference {}", d(&wide, &hop));
    assert_eq!(Change::of(d(&wide, &other)), Change::Clean, "difference {}", d(&wide, &other));
    assert_eq!(Change::of(f32::NAN), Change::Match, "an unmeasurable difference claims nothing");
    assert_eq!((Change::Match.label(), Change::Jump.label(), Change::Clean.label()), ("match", "jump", "clean"));
}

#[test]
fn rhythm_counts_cuts_and_reads_shot_lengths() {
    let r = rhythm(&[2.0, 4.0, 6.0, 8.0]);
    assert_eq!((r.shots, r.cuts), (4, 3));
    assert!((r.seconds - 20.0).abs() < 1e-9 && (r.cuts_per_minute - 9.0).abs() < 1e-9, "{r:?}");
    assert_eq!((r.mean, r.median, r.shortest, r.longest), (5.0, 5.0, 2.0, 8.0));
    assert!((r.variation - (5.0f64).sqrt() / 5.0).abs() < 1e-9);
    // a metronome has no variation
    assert_eq!(rhythm(&[1.0; 12]).variation, 0.0);
    // junk lengths are skipped, not summed
    let r = rhythm(&[f64::NAN, -1.0, 0.0, f64::INFINITY, 3.0]);
    assert_eq!((r.shots, r.cuts, r.seconds), (1, 0, 3.0));
    let none = rhythm(&[]);
    assert_eq!((none.shots, none.cuts_per_minute), (0, 0.0));
}

#[test]
fn pace_follows_the_blink_range() {
    for (cpm, pace) in [
        (0.0, Pace::Contemplative),
        (3.9, Pace::Contemplative),
        (6.0, Pace::Dialogue),
        (12.0, Pace::Brisk),
        (25.0, Pace::Action),
        (40.0, Pace::Action),
        (41.0, Pace::Frantic),
    ] {
        assert_eq!(Pace::of(cpm), pace, "{cpm}");
        assert!(!Pace::of(cpm).note().is_empty() && !Pace::of(cpm).label().is_empty());
    }
    assert_eq!(Pace::of(f64::NAN), Pace::Contemplative);
}

/// `phrases` of (sound seconds, silence seconds) as a 220 Hz tone over a faint hiss, at `rate`.
fn speech(rate: u32, phrases: &[(f64, f64)]) -> Vec<f32> {
    let mut out = Vec::new();
    let mut seed = 1u32;
    let mut noise = move || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5
    };
    for &(on, off) in phrases {
        for i in 0..(on * f64::from(rate)) as usize {
            out.push(0.3 * (i as f32 * 220.0 * std::f32::consts::TAU / rate as f32).sin() + 0.002 * noise());
        }
        for _ in 0..(off * f64::from(rate)) as usize {
            out.push(0.002 * noise());
        }
    }
    out
}

#[test]
fn pauses_are_found_where_phrases_end() {
    let rate = 16_000;
    let found = pauses(&speech(rate, &[(1.0, 0.5), (0.8, 0.3), (1.2, 0.6)]), rate, 0.2, 0.4);
    assert_eq!(found.len(), 3, "{found:?}");
    for (p, (start, end)) in found.iter().zip([(1.0, 1.5), (2.3, 2.6), (3.8, 4.4)]) {
        assert!((p.start - start).abs() < 0.04 && (p.end - end).abs() < 0.04, "{p:?} vs {start}..{end}");
        assert!(p.drop_db > 20.0, "{p:?}");
    }
}

#[test]
fn short_gaps_and_short_sounds_are_not_phrasing() {
    let rate = 16_000;
    // a 40 ms gap inside a phrase is a consonant, not a pause
    assert!(pauses(&speech(rate, &[(0.6, 0.04), (0.6, 0.0)]), rate, 0.2, 0.4).is_empty());
    // a pause shorter than asked for
    assert!(pauses(&speech(rate, &[(1.0, 0.15), (1.0, 0.0)]), rate, 0.2, 0.4).is_empty());
    // a click is not a phrase: the silence after it is not the end of a thought
    assert!(pauses(&speech(rate, &[(0.1, 0.6), (0.1, 0.6)]), rate, 0.2, 0.4).is_empty());
    // leading silence comes before any phrase
    let lead = pauses(&speech(rate, &[(0.0, 1.0), (1.0, 0.5)]), rate, 0.2, 0.4);
    assert_eq!(lead.len(), 1, "{lead:?}");
    assert!((lead[0].start - 2.0).abs() < 0.04);
}

#[test]
fn sound_without_phrasing_and_hostile_samples_return_nothing() {
    let rate = 16_000;
    // a constant tone: no quiet moments to tell phrases apart
    assert!(pauses(&speech(rate, &[(3.0, 0.0)]), rate, 0.2, 0.4).is_empty());
    assert!(pauses(&vec![0.0; 48_000], 48_000, 0.2, 0.4).is_empty(), "digital silence");
    assert!(pauses(&[], 48_000, 0.2, 0.4).is_empty());
    assert!(pauses(&[0.5; 100], 0, 0.2, 0.4).is_empty(), "a zero sample rate divides nothing");
    assert!(pauses(&[0.5; 100], 48_000, 0.2, 0.4).is_empty(), "too short to analyse");
    // NaN and infinite samples are read as silence
    let mut junk = speech(rate, &[(1.0, 0.5), (1.0, 0.5)]);
    junk[100] = f32::NAN;
    junk[200] = f32::INFINITY;
    assert_eq!(pauses(&junk, rate, 0.2, 0.4).len(), 2);
    // absurd minimums find nothing and loop nowhere
    assert!(pauses(&junk, rate, f64::INFINITY, 0.4).is_empty());
    assert_eq!(pauses(&junk, rate, f64::NAN, f64::NAN).len(), pauses(&junk, rate, 0.0, 0.0).len());
    // a rate so high that a step is longer than the samples
    assert!(pauses(&junk, u32::MAX, 0.2, 0.4).is_empty());
}

#[test]
fn a_movement_settles_after_its_burst() {
    // calm, a burst of movement at 10..14, calm again
    let mut a = vec![0.002f32; 40];
    for (i, v) in [0.03, 0.08, 0.09, 0.05].into_iter().enumerate() {
        a[10 + i] = v;
    }
    let s = settles(&a, 20);
    assert_eq!(s.len(), 1, "{s:?}");
    assert!((14..=17).contains(&s[0].0), "settles just after the burst: {s:?}");
    assert!((s[0].1 - 1.0).abs() < 1e-6);
    // a second, weaker burst gets a lower strength
    for (i, v) in [0.02, 0.04, 0.02].into_iter().enumerate() {
        a[25 + i] = v;
    }
    let s = settles(&a, 20);
    assert_eq!(s.len(), 2, "{s:?}");
    assert!(s[1].1 < s[0].1 && s[1].0 > 27, "{s:?}");
}

#[test]
fn stillness_and_junk_settle_nowhere() {
    assert!(settles(&[], 20).is_empty());
    assert!(settles(&[0.5, 0.5], 20).is_empty());
    assert!(settles(&[0.001; 50], 20).is_empty(), "nothing moved");
    assert!(settles(&[f32::NAN; 50], 20).is_empty());
    // movement that never dies down has no settle point
    let rising: Vec<f32> = (0..50).map(|i| 0.01 + i as f32 * 0.01).collect();
    assert!(settles(&rising, 20).is_empty());
}

#[test]
fn the_medoid_is_the_frame_most_like_the_others() {
    let (w, h) = (64, 36);
    let sigs: Vec<_> = [10u8, 120, 130, 140, 250].iter().map(|v| signature(&frame(w, h, *v, &[]), w, h)).collect();
    assert_eq!(medoid(&sigs), Some(2));
    assert_eq!(medoid(&sigs[..1]), Some(0));
    assert_eq!(medoid(&[]), None);
}

fn cell(v: f32) -> Image {
    Image::filled(32, 18, [v, v, v, 1.0])
}

#[test]
fn a_contact_sheet_reads_like_a_page() {
    let cells: Vec<(Image, String)> = (0..5).map(|i| (cell(0.1 + i as f32 * 0.2), String::new())).collect();
    let s = Sheet { columns: 3, cell_w: 32, cell_h: 18, gap: 4 };
    let sheet = contact_sheet(&cells, s).unwrap();
    assert_eq!((sheet.w, sheet.h), (3 * 32 + 4 * 4, 2 * 18 + 3 * 4));
    let at = |col: usize, row: usize| sheet.get(4 + col * 36 + 16, 4 + row * 22 + 9)[0];
    // left to right, then the next line
    for (i, (col, row)) in [(0, 0), (1, 0), (2, 0), (0, 1), (1, 1)].into_iter().enumerate() {
        assert!((at(col, row) - (0.1 + i as f32 * 0.2)).abs() < 1e-4, "cell {i}: {}", at(col, row));
    }
    // the sixth place is empty board
    assert!((at(2, 1) - 0.013).abs() < 1e-4);
    assert!(sheet.px.chunks(4).all(|p| p[3] == 1.0), "the sheet is opaque");
}

#[test]
fn a_frame_keeps_its_shape_inside_its_cell() {
    // a tall frame in a wide cell is pillarboxed, not stretched
    let tall = Image::filled(10, 40, [0.8, 0.8, 0.8, 1.0]);
    let sheet = contact_sheet(&[(tall, String::new())], Sheet { columns: 1, cell_w: 80, cell_h: 40, gap: 0 }).unwrap();
    assert!((sheet.get(40, 20)[0] - 0.8).abs() < 1e-4, "the frame is in the middle");
    assert!((sheet.get(5, 20)[0] - 0.013).abs() < 1e-4 && (sheet.get(75, 20)[0] - 0.013).abs() < 1e-4, "board at the sides");
    // a transparent frame shows the board through
    let clear = Image::new(8, 8);
    let sheet = contact_sheet(&[(clear, String::new())], Sheet { columns: 1, cell_w: 8, cell_h: 8, gap: 0 }).unwrap();
    assert!((sheet.get(4, 4)[0] - 0.013).abs() < 1e-4);
    // an empty frame leaves its cell blank
    assert!(contact_sheet(&[(Image::new(0, 0), String::new())], Sheet { columns: 1, cell_w: 8, cell_h: 8, gap: 0 }).is_some());
}

#[test]
fn labels_are_burned_into_their_cells() {
    let plain = contact_sheet(&[(cell(0.2), String::new())], Sheet { columns: 1, cell_w: 160, cell_h: 90, gap: 0 }).unwrap();
    let labelled = contact_sheet(&[(cell(0.2), "12".into())], Sheet { columns: 1, cell_w: 160, cell_h: 90, gap: 0 }).unwrap();
    assert_ne!(plain, labelled);
    // only the top-left corner changed
    assert_eq!(plain.get(150, 80), labelled.get(150, 80));
}

#[test]
fn oversized_and_empty_sheets_are_refused() {
    let s = Sheet { columns: 4, cell_w: 32, cell_h: 18, gap: 2 };
    assert!(contact_sheet(&[], s).is_none());
    assert!(contact_sheet(&[(cell(0.1), String::new())], Sheet { columns: 0, ..s }).is_none());
    assert!(contact_sheet(&[(cell(0.1), String::new())], Sheet { cell_w: 2, ..s }).is_none());
    assert!(contact_sheet(&[(cell(0.1), String::new())], Sheet { cell_w: usize::MAX / 2, ..s }).is_none(), "overflowing width");
    assert!(contact_sheet(&[(cell(0.1), String::new())], Sheet { cell_w: 10_000, cell_h: 10_000, ..s }).is_none(), "over the pixel budget");
    assert!(contact_sheet(&[(cell(0.1), String::new())], Sheet { gap: usize::MAX, ..s }).is_none());
    let many: Vec<(Image, String)> = (0..MAX_SHEET_CELLS + 1).map(|_| (Image::new(0, 0), String::new())).collect();
    assert!(contact_sheet(&many, s).is_none());
}

#[test]
fn movement_all_over_the_frame_leads_the_eye_nowhere() {
    let (w, h) = (256, 144);
    // one bright post in the middle of rippling water: every cell of the water flickers a little
    let water = |seed: u32| {
        let mut px = frame(w, h, 90, &[(120, 40, 14, 70, 235)]);
        let mut state = seed;
        for (i, p) in px.chunks_mut(4).enumerate() {
            let (x, y) = (i % w, i / w);
            if (120..134).contains(&x) && (40..110).contains(&y) {
                continue;
            }
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            // ripples are coarse: the same nudge for each 4×4 block
            let block = ((x / 4) as u32).wrapping_mul(73_856_093) ^ ((y / 4) as u32).wrapping_mul(19_349_663) ^ seed;
            let nudge = (block.wrapping_mul(2_654_435_761) >> 27) as u8;
            p[..3].fill(74 + nudge);
        }
        px
    };
    let (a, b) = (glance(&water(1), w, h), glance(&water(2), w, h));
    let still = attention(&b, None);
    let moving = attention(&b, Some(&a));
    assert_eq!(moving.motion, 0.0, "ripples everywhere are not a subject: {moving:?}");
    assert!((moving.x - still.x).abs() < 0.02 && (moving.y - still.y).abs() < 0.02, "the eye stays on the post: {moving:?} vs {still:?}");
    assert!((moving.x - 0.5).abs() < 0.1 && (moving.y - 0.52).abs() < 0.12, "{moving:?}");
    // a small thing that moves in a still frame counts in full
    let prev = glance(&frame(w, h, 40, &[(30, 70, 12, 12, 110)]), w, h);
    let cur = glance(&frame(w, h, 40, &[(48, 70, 12, 12, 110)]), w, h);
    assert_eq!(attention(&cur, Some(&prev)).motion, 1.0);
    // nothing moved at all
    assert_eq!(attention(&cur, Some(&cur)).motion, 0.0);
    assert_eq!(attention(&cur, None).motion, 0.0);
}

/// Frame-to-frame activity of a hand-held close shot, measured eight times a second on real
/// footage: the camera never holds still, a head turns up at sample 34 and back down around 47.
const HAND_HELD: [f32; 94] = [
    0.065, 0.050, 0.046, 0.041, 0.051, 0.050, 0.046, 0.039, 0.047, 0.019, 0.024, 0.025, 0.027, 0.045, 0.024, 0.031, 0.021, 0.021, 0.023, 0.036, 0.032, 0.036,
    0.020, 0.025, 0.033, 0.033, 0.031, 0.025, 0.022, 0.024, 0.033, 0.035, 0.031, 0.024, 0.048, 0.057, 0.041, 0.033, 0.041, 0.040, 0.040, 0.040, 0.033, 0.025,
    0.032, 0.045, 0.054, 0.055, 0.072, 0.060, 0.046, 0.048, 0.035, 0.036, 0.034, 0.022, 0.023, 0.020, 0.021, 0.018, 0.034, 0.023, 0.027, 0.025, 0.030, 0.035,
    0.044, 0.039, 0.028, 0.027, 0.027, 0.027, 0.023, 0.016, 0.029, 0.031, 0.023, 0.020, 0.032, 0.041, 0.045, 0.052, 0.048, 0.044, 0.044, 0.052, 0.043, 0.027,
    0.025, 0.022, 0.019, 0.024, 0.027, 0.030,
];

#[test]
fn a_turn_of_the_head_stands_out_of_a_hand_held_shot() {
    let found = settles(&HAND_HELD, 21);
    let at: Vec<usize> = found.iter().map(|f| f.0).collect();
    // the head has turned (the burst at 34–35 is over by 37), and has come back down (48 is over by 55–58)
    assert!(at.iter().any(|i| (36..=39).contains(i)), "the look up: {found:?}");
    assert!(at.iter().any(|i| (53..=58).contains(i)), "the look back down: {found:?}");
    // a handful of movements in twelve seconds, not one at every wobble of the camera
    assert!((3..=6).contains(&found.len()), "{found:?}");
    assert!(found.iter().all(|f| f.1 > 0.0 && f.1 <= 1.0));
    // against the median of the whole series alone, the look up would not have counted
    let whole = {
        let mut v = HAND_HELD.to_vec();
        v.sort_by(f32::total_cmp);
        v[v.len() / 2]
    };
    assert!(0.057 < 2.5 * whole, "the burst is under 2.5 times the series' median of {whole}");
}

#[test]
fn a_move_longer_than_the_window_still_settles() {
    // a slow three-second tilt in a shot that is otherwise calm: the local median sits inside the move
    let mut a = vec![0.004f32; 120];
    for v in a.iter_mut().take(60).skip(20) {
        *v = 0.05;
    }
    let found = settles(&a, 8);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!((60..=63).contains(&found[0].0), "{found:?}");
    // absurd windows are clamped, not indexed
    assert_eq!(settles(&a, 0).len(), settles(&a, 2).len());
    assert!(!settles(&a, usize::MAX).is_empty());
}

#[test]
fn a_voice_is_heard_over_an_engine() {
    let rate = 16_000u32;
    // three phrases of a voice (400 and 1200 Hz) with half-second pauses, under a loud 70 Hz engine
    let mut track = Vec::new();
    let voiced = [(0.0, 1.0), (1.5, 2.4), (2.9, 4.0)];
    for i in 0..(4.5 * f64::from(rate)) as usize {
        let t = i as f64 / f64::from(rate);
        let engine = 0.5 * (t * 70.0 * std::f64::consts::TAU).sin();
        let speaking = voiced.iter().any(|(a, b)| t >= *a && t < *b);
        let voice = if speaking { 0.06 * (t * 400.0 * std::f64::consts::TAU).sin() + 0.04 * (t * 1200.0 * std::f64::consts::TAU).sin() } else { 0.0 };
        track.push((engine + voice) as f32);
    }
    assert!(pauses(&track, rate, 0.25, 0.4).is_empty(), "the engine hides the pauses in the whole sound");
    // fed in pieces, as a long clip is
    let mut band = VoiceBand::new(rate);
    let levels: Vec<f32> = track.chunks(level_step(rate) * 37).flat_map(|piece| band.levels_db(piece)).collect();
    let found = pauses_in_levels(&levels, 0.01, 0.25, 0.4);
    assert_eq!(found.len(), 3, "{found:?}");
    for (p, end) in found.iter().zip([1.0, 2.4, 4.0]) {
        assert!((p.start - end).abs() < 0.06, "{p:?} vs {end}");
    }
    // hostile samples and rates do not disturb it
    let mut junk = VoiceBand::new(0);
    assert!(junk.levels_db(&[f32::NAN, f32::INFINITY, 0.5]).is_empty());
    let mut band = VoiceBand::new(rate);
    assert!(band.levels_db(&vec![f32::NAN; 1600]).iter().all(|v| v.is_finite()));
}

#[test]
fn a_change_is_measured_where_it_is_strongest() {
    let (w, h) = (256, 144);
    let base = frame(w, h, 100, &[]);
    assert_eq!(picture_change(&base, &base, w, h).map(|c| (c.mean, c.strongest, c.reads())), Some((0.0, 0.0, "none")));
    // a small region lifted by 30 levels: the mean barely moves, the region says it
    let lifted = frame(w, h, 100, &[(100, 60, 40, 30, 130)]);
    let c = picture_change(&base, &lifted, w, h).unwrap();
    assert!(c.mean < 0.01 && (c.strongest - 30.0 / 255.0).abs() < 0.01, "{c:?}");
    assert_eq!(c.reads(), "visible");
    // the whole frame lifted by 4 levels: there, but only side by side
    assert_eq!(picture_change(&base, &frame(w, h, 104, &[]), w, h).unwrap().reads(), "subtle");
    assert_eq!(picture_change(&base, &frame(w, h, 101, &[]), w, h).unwrap().reads(), "none");
    assert_eq!(picture_change(&base, &frame(w, h, 200, &[]), w, h).unwrap().reads(), "strong");
    // grain averages out: every pixel off by ±3 at random is no change of the picture
    let mut grain = base.clone();
    let mut seed = 5u32;
    for p in grain.chunks_mut(4) {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let d = (seed >> 29) as i16 - 3;
        p[..3].iter_mut().for_each(|v| *v = (i16::from(*v) + d).clamp(0, 255) as u8);
    }
    assert_eq!(picture_change(&base, &grain, w, h).unwrap().reads(), "none");
    // sizes that do not match their buffers are refused
    assert!(picture_change(&base, &lifted, w, h + 1).is_none());
    assert!(picture_change(&base, &base[..8], w, h).is_none());
    assert!(picture_change(&[], &[], 0, 0).is_none());
    assert!(picture_change(&base, &base, usize::MAX, usize::MAX).is_none());
}
