//! The picture of nested sequences: a nest shows its sequence at that sequence's own size and
//! frame rate, through the clip's speed, holds and effects, with the sequence's captions.
//! Premiere's behaviour was observed in Premiere Pro 26.5.2.

use super::*;
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{DemoScene, Generator, MediaSource};
use filmcraft_project::{Caption, CaptionFormat, CaptionTrack, ClipId, Label, MediaClip, MediaRef, ParamValue, SequenceSettings, TrackId, TrackKind};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, TimeRange};

const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];

struct Rig {
    p: Project,
    map: SourceMap,
}

impl Rig {
    fn new() -> Rig {
        Rig { p: Project::new("nest"), map: SourceMap::default() }
    }

    fn add(&mut self, g: GeneratorSource) -> ItemId {
        let info = g.info().clone();
        let id = self.p.add_item(
            &info.name.clone(),
            Label::Iris,
            ItemKind::Media(Box::new(MediaClip {
                media: MediaRef::Generator(g.generator.clone()),
                info,
                interpret: Default::default(),
                mark_in: None,
                mark_out: None,
                markers: vec![],
                offline: false,
                proxy: None,
                identity: None,
            })),
            None,
        );
        self.map.0.insert(id, Arc::new(g) as SharedSource);
        id
    }

    fn matte(&mut self, color: [f32; 4], w: u32, h: u32) -> ItemId {
        self.add(GeneratorSource::new(Generator::ColorMatte { color }, w, h, FrameRate::FPS_24, Tick(10 * TICKS_PER_SECOND)))
    }

    fn seq(&mut self, name: &str, w: u32, h: u32, rate: FrameRate) -> ItemId {
        self.p.new_sequence(name, SequenceSettings { width: w, height: h, frame_rate: rate, ..Default::default() }, 2, 1, None)
    }

    /// `frames` frames of `item` from its start, at the start of V1 of `seq`.
    fn put(&mut self, seq: ItemId, item: ItemId, frames: i64) -> ClipId {
        let rate = self.p.sequence(seq).unwrap().settings.frame_rate;
        let ti = self.p.make_track_item(item, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.tick_of(frames)), rate).unwrap();
        let id = ti.id;
        self.p.sequence_mut(seq).unwrap().video_tracks[0].items.push(ti);
        id
    }

    fn clip(&mut self, seq: ItemId, clip: ClipId) -> &mut TrackItem {
        self.p.sequence_mut(seq).unwrap().find_item_mut(clip).unwrap().1
    }

    fn frame(&self, seq: ItemId, t: Tick) -> Image {
        render_sequence(&self.p, seq, t, RenderOptions::default(), &self.map)
    }

    /// The same frame through the frame plan (what the GPU compositor is given).
    fn planned(&self, seq: ItemId, t: Tick) -> Image {
        plan::execute_cpu(&plan::plan_frame(&self.p, seq, t, RenderOptions::default(), plan::DeviceCaps::ALL, &self.map))
    }
}

fn close(a: [f32; 4], b: [f32; 4]) -> bool {
    a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 0.02)
}

/// The largest channel difference between two images of the same size.
fn worst(a: &Image, b: &Image) -> f32 {
    assert_eq!((a.w, a.h), (b.w, b.h));
    a.px.iter().zip(&b.px).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

#[test]
fn a_smaller_nest_is_shown_at_its_own_size_in_the_middle() {
    // Premiere: a 1280x720 sequence in a 1920x1080 one comes in centred at 100%
    let mut r = Rig::new();
    let red = r.matte(RED, 320, 180);
    let inner = r.seq("inner", 320, 180, FrameRate::FPS_24);
    r.put(inner, red, 48);
    let outer = r.seq("outer", 640, 360, FrameRate::FPS_24);
    let nest = r.put(outer, inner, 48);
    for img in [r.frame(outer, Tick(1000)), r.planned(outer, Tick(1000))] {
        assert_eq!((img.w, img.h), (640, 360));
        // the nest covers x 160..480, y 90..270
        assert!(close(img.get(320, 180), RED) && close(img.get(170, 100), RED) && close(img.get(470, 260), RED));
        assert!(img.get(150, 180)[3] < 0.02 && img.get(320, 80)[3] < 0.02 && img.get(10, 10)[3] < 0.02, "empty around it");
    }
    // Scale to Frame Size fills the frame without touching Motion ▸ Scale
    r.clip(outer, nest).scale_to_frame = true;
    let img = r.frame(outer, Tick(1000));
    assert!(close(img.get(10, 10), RED) && close(img.get(630, 350), RED));
    // Motion ▸ Scale 200% (what Fit to frame sets here) does the same
    let c = r.clip(outer, nest);
    c.scale_to_frame = false;
    c.effect_mut("motion").unwrap().params.get_mut("scale").unwrap().value = ParamValue::Float(200.0);
    let img = r.frame(outer, Tick(1000));
    assert!(close(img.get(10, 10), RED) && close(img.get(630, 350), RED));
}

#[test]
fn a_larger_nest_is_cropped_by_the_frame() {
    let mut r = Rig::new();
    let ocean = r.add(GeneratorSource::demo(DemoScene::OceanSunset));
    let (w, h) = {
        let v = r.map.0[&ocean].info().video.clone().unwrap();
        (v.width, v.height)
    };
    let inner = r.seq("inner", w, h, FrameRate::FPS_24);
    r.put(inner, ocean, 48);
    let outer = r.seq("outer", w / 2, h / 2, FrameRate::FPS_24);
    r.put(outer, inner, 48);
    let (big, small) = (r.frame(inner, Tick(1000)), r.frame(outer, Tick(1000)));
    assert_eq!((small.w as u32, small.h as u32), (w / 2, h / 2));
    // the outer frame is the middle of the inner one, pixel for pixel
    let (ox, oy) = (big.w / 4, big.h / 4);
    for (x, y) in [(0, 0), (small.w / 2, small.h / 2), (small.w - 1, small.h - 1), (7, small.h - 3)] {
        assert!(close(small.get(x, y), big.get(x + ox, y + oy)), "({x}, {y})");
    }
}

#[test]
fn a_nest_at_another_frame_rate_shows_its_sequence_at_the_same_moment() {
    let mut r = Rig::new();
    let ocean = r.add(GeneratorSource::demo(DemoScene::OceanSunset));
    let (w, h) = {
        let v = r.map.0[&ocean].info().video.clone().unwrap();
        (v.width, v.height)
    };
    let inner = r.seq("inner", w, h, FrameRate::FPS_30);
    r.put(inner, ocean, 90);
    let outer = r.seq("outer", w, h, FrameRate::FPS_24);
    r.put(outer, inner, 72);
    // 1.5 s into the 24 fps sequence is 1.5 s into the 30 fps one
    let t = FrameRate::FPS_24.tick_of(36);
    assert!(worst(&r.frame(outer, t), &r.frame(inner, t)) < 0.01);
    assert!(worst(&r.frame(inner, t), &r.frame(inner, Tick::ZERO)) > 0.05, "the footage moves, so the comparison means something");
}

#[test]
fn speed_reverse_and_frame_hold_on_a_nest_pick_the_frame_of_its_sequence() {
    let mut r = Rig::new();
    let ocean = r.add(GeneratorSource::demo(DemoScene::OceanSunset));
    let (w, h) = {
        let v = r.map.0[&ocean].info().video.clone().unwrap();
        (v.width, v.height)
    };
    let rate = FrameRate::FPS_24;
    let inner = r.seq("inner", w, h, rate);
    r.put(inner, ocean, 96);
    let outer = r.seq("outer", w, h, rate);
    let nest = r.put(outer, inner, 48);
    let f = |n: i64| rate.tick_of(n);
    // double speed: frame 10 of the nest is frame 20 of its sequence
    r.clip(outer, nest).speed = 2.0;
    assert!(worst(&r.frame(outer, f(10)), &r.frame(inner, f(20))) < 0.01);
    // reversed (48 frames at normal speed): 10 frames in is 10 frames back from the end of what it
    // covers, a tick short of frame 38
    let c = r.clip(outer, nest);
    c.speed = 1.0;
    c.reverse = true;
    assert!(worst(&r.frame(outer, f(10)), &r.frame(inner, f(38) - Tick(1))) < 0.01);
    // a frame hold shows one frame of the sequence throughout
    let c = r.clip(outer, nest);
    c.reverse = false;
    c.frame_hold = Some(f(30));
    assert!(worst(&r.frame(outer, f(5)), &r.frame(inner, f(30))) < 0.01);
    assert!(worst(&r.frame(outer, f(40)), &r.frame(inner, f(30))) < 0.01);
}

#[test]
fn opacity_and_effects_on_a_nest_apply_to_its_whole_picture() {
    let mut r = Rig::new();
    let red = r.matte(RED, 320, 180);
    let inner = r.seq("inner", 320, 180, FrameRate::FPS_24);
    r.put(inner, red, 48);
    let outer = r.seq("outer", 320, 180, FrameRate::FPS_24);
    let nest = r.put(outer, inner, 48);
    r.clip(outer, nest).effect_mut("opacity").unwrap().params.get_mut("opacity").unwrap().value = ParamValue::Float(50.0);
    for img in [r.frame(outer, Tick(1000)), r.planned(outer, Tick(1000))] {
        let c = img.get(160, 90);
        assert!((c[3] - 0.5).abs() < 0.02, "half transparent: {c:?}");
    }
    // a crop on the nest cuts its picture like any clip's
    let c = r.clip(outer, nest);
    c.effect_mut("opacity").unwrap().params.get_mut("opacity").unwrap().value = ParamValue::Float(100.0);
    let mut crop = filmcraft_project::effect::find_effect("crop").unwrap().instance();
    crop.params.get_mut("left").unwrap().value = ParamValue::Float(50.0);
    c.effects.push(crop);
    let img = r.frame(outer, Tick(1000));
    assert!(img.get(40, 90)[3] < 0.02 && close(img.get(280, 90), RED));
}

#[test]
fn captions_inside_a_nest_are_part_of_its_picture() {
    // Premiere draws a nested sequence's captions in the sequence it is nested in
    let mut r = Rig::new();
    let red = r.matte(RED, 320, 180);
    let inner = r.seq("inner", 320, 180, FrameRate::FPS_24);
    r.put(inner, red, 48);
    let mut track = CaptionTrack::new(TrackId(9001), "Subtitles".into(), CaptionFormat::Subtitle);
    track.captions.push(Caption {
        id: ClipId(9002),
        start: Tick::ZERO,
        duration: Tick(TICKS_PER_SECOND),
        text: "NESTED".into(),
        speaker: None,
        cue_id: None,
        settings: String::new(),
    });
    r.p.sequence_mut(inner).unwrap().caption_tracks.push(track);
    let outer = r.seq("outer", 640, 360, FrameRate::FPS_24);
    r.put(outer, inner, 48);
    let t = Tick(1000);
    let with = r.frame(outer, t);
    r.p.sequence_mut(inner).unwrap().caption_tracks[0].enabled = false;
    let without = r.frame(outer, t);
    r.p.sequence_mut(inner).unwrap().caption_tracks[0].enabled = true;
    // the caption changes pixels, all of them inside the nest's part of the frame (x 160..480, y 90..270)
    let mut changed = 0;
    for y in 0..with.h {
        for x in 0..with.w {
            if !close(with.get(x, y), without.get(x, y)) {
                changed += 1;
                assert!((160..480).contains(&x) && (90..270).contains(&y), "caption pixel outside the nest at ({x}, {y})");
            }
        }
    }
    assert!(changed > 20, "the caption is drawn ({changed} pixels)");
    // after the caption ends there is nothing to draw
    assert!(worst(&r.frame(outer, Tick(TICKS_PER_SECOND + 1000)), &without) < 0.01);
    // the frame plan gives the same picture
    assert!(worst(&r.planned(outer, t), &with) < 0.02);
    // and the outer sequence's own captions are still its own business: with captions off for
    // the outer render, the nested ones show all the same
    let mut outer_track = CaptionTrack::new(TrackId(9003), "Outer".into(), CaptionFormat::Subtitle);
    outer_track.captions.push(Caption {
        id: ClipId(9004),
        start: Tick::ZERO,
        duration: Tick(TICKS_PER_SECOND),
        text: "OUTER".into(),
        speaker: None,
        cue_id: None,
        settings: String::new(),
    });
    r.p.sequence_mut(outer).unwrap().caption_tracks.push(outer_track);
    assert!(worst(&r.frame(outer, t), &with) < 0.01, "outer captions are only drawn when asked for");
    let shown = render_sequence(&r.p, outer, t, RenderOptions { captions: true, ..Default::default() }, &r.map);
    assert!(worst(&shown, &with) > 0.1, "and are drawn when asked for");
}

// ---- the frame plan draws a plain nest's layers itself

/// How many layers the plan of `seq` at `t` has (None when it fell back to one CPU image).
fn layer_count(r: &Rig, seq: ItemId, t: Tick) -> Option<usize> {
    match plan::plan_frame(&r.p, seq, t, RenderOptions::default(), plan::DeviceCaps::ALL, &r.map) {
        plan::FramePlan::Layers { layers, .. } => Some(layers.len()),
        _ => None,
    }
}

/// An inner sequence with the ocean on V1 and a half-size, half-transparent red matte over it on
/// V2, nested alone in an outer sequence of the same size. Returns (inner, outer, nest clip).
fn two_layer_nest(r: &mut Rig) -> (ItemId, ItemId, ClipId) {
    let ocean = r.add(GeneratorSource::demo(DemoScene::OceanSunset));
    let (w, h) = {
        let v = r.map.0[&ocean].info().video.clone().unwrap();
        (v.width, v.height)
    };
    let red = r.matte(RED, w / 2, h / 2);
    let inner = r.seq("inner", w, h, FrameRate::FPS_24);
    r.put(inner, ocean, 96);
    let rate = FrameRate::FPS_24;
    let mut top = r.p.make_track_item(red, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.tick_of(96)), rate).unwrap();
    top.effect_mut("opacity").unwrap().params.get_mut("opacity").unwrap().value = ParamValue::Float(50.0);
    r.p.sequence_mut(inner).unwrap().video_tracks[1].items.push(top);
    let outer = r.seq("outer", w, h, FrameRate::FPS_24);
    let nest = r.put(outer, inner, 96);
    (inner, outer, nest)
}

#[test]
fn a_plain_nest_is_planned_as_its_own_layers_and_looks_the_same() {
    let mut r = Rig::new();
    let (inner, outer, _) = two_layer_nest(&mut r);
    let t = FrameRate::FPS_24.tick_of(20);
    // two layers (the nest's clips), not one image of the nest
    assert_eq!(layer_count(&r, outer, t), Some(2));
    assert!(worst(&r.planned(outer, t), &r.frame(outer, t)) < 0.01);
    assert!(worst(&r.planned(outer, t), &r.frame(inner, t)) < 0.01, "and it is the nested sequence's picture");
    // a nest of the nest still plans down to the two clips
    let outermost = r.seq("outermost", r.p.sequence(outer).unwrap().settings.width, r.p.sequence(outer).unwrap().settings.height, FrameRate::FPS_24);
    r.put(outermost, outer, 96);
    assert_eq!(layer_count(&r, outermost, t), Some(2));
    assert!(worst(&r.planned(outermost, t), &r.frame(outermost, t)) < 0.01);
    // with something under the nest in the outer sequence too
    let under = r.matte([0.0, 0.0, 1.0, 1.0], 64, 64);
    let rate = FrameRate::FPS_24;
    let nest_clip = r.p.sequence_mut(outermost).unwrap().video_tracks[0].items.remove(0);
    let mut below = r.p.make_track_item(under, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.tick_of(96)), rate).unwrap();
    below.scale_to_frame = true;
    let s = r.p.sequence_mut(outermost).unwrap();
    s.video_tracks[0].items.push(below);
    s.video_tracks[1].items.push(nest_clip);
    assert_eq!(layer_count(&r, outermost, t), Some(3));
    assert!(worst(&r.planned(outermost, t), &r.frame(outermost, t)) < 0.01);
}

#[test]
fn a_nest_that_is_not_plain_is_still_drawn_right() {
    let t = FrameRate::FPS_24.tick_of(20);
    // each of these keeps the nest one layer (rendered on the CPU), and the plan matches the reference
    let cases: [(&str, fn(&mut Rig, ItemId, ItemId, ClipId)); 5] = [
        ("half-transparent nest", |r, _, outer, nest| {
            r.clip(outer, nest).effect_mut("opacity").unwrap().params.get_mut("opacity").unwrap().value = ParamValue::Float(50.0);
        }),
        ("moved nest", |r, _, outer, nest| {
            r.clip(outer, nest).effect_mut("motion").unwrap().params.get_mut("scale").unwrap().value = ParamValue::Float(60.0);
        }),
        ("nest with an effect", |r, _, outer, nest| {
            let mut crop = filmcraft_project::effect::find_effect("crop").unwrap().instance();
            crop.params.get_mut("left").unwrap().value = ParamValue::Float(30.0);
            r.clip(outer, nest).effects.push(crop);
        }),
        ("a blend mode inside the nest", |r, inner, _, _| {
            let top = &mut r.p.sequence_mut(inner).unwrap().video_tracks[1].items[0];
            top.effect_mut("opacity").unwrap().params.get_mut("blend").unwrap().value = ParamValue::Choice(3);
        }),
        ("a nest of another size", |r, inner, _, _| {
            r.p.sequence_mut(inner).unwrap().settings.width /= 2;
        }),
    ];
    for (what, change) in cases {
        let mut r = Rig::new();
        let (inner, outer, nest) = two_layer_nest(&mut r);
        // under the nest, so that blending the nest as a whole or layer by layer would differ
        let under = r.matte([0.0, 1.0, 0.0, 1.0], 64, 64);
        let rate = FrameRate::FPS_24;
        let nest_clip = r.p.sequence_mut(outer).unwrap().video_tracks[0].items.remove(0);
        let mut below = r.p.make_track_item(under, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.tick_of(96)), rate).unwrap();
        below.scale_to_frame = true;
        let s = r.p.sequence_mut(outer).unwrap();
        s.video_tracks[0].items.push(below);
        s.video_tracks[1].items.push(nest_clip);
        change(&mut r, inner, outer, nest);
        assert_eq!(layer_count(&r, outer, t), Some(2), "{what}: the matte and one image of the nest");
        let w = worst(&r.planned(outer, t), &r.frame(outer, t));
        assert!(w < 0.01, "{what}: plan and reference differ by {w}");
    }
}

#[test]
fn transformed_plain_nests_preserve_the_group_picture() {
    let t = FrameRate::FPS_24.tick_of(20);
    for (scale, rotation) in [(100.0, 0.0), (60.0, 0.0), (130.0, 19.0)] {
        let mut r = Rig::new();
        let (inner, outer, nest) = two_layer_nest(&mut r);
        let sequence = r.p.sequence(inner).unwrap();
        let child = sequence.video_tracks[0].items[0].id;
        let position = filmcraft_geom::Vec2::new(sequence.settings.width as f64 * 0.75, sequence.settings.height as f64 * 0.5);
        r.clip(inner, child).effect_mut("motion").unwrap().param_mut("position").unwrap().value = ParamValue::Vec2(position);
        let motion = r.clip(outer, nest).effect_mut("motion").unwrap();
        motion.param_mut("position").unwrap().value = ParamValue::Vec2(filmcraft_geom::Vec2::new(175.0, 75.0));
        motion.param_mut("scale").unwrap().value = ParamValue::Float(scale);
        motion.param_mut("rotation").unwrap().value = ParamValue::Float(rotation);
        for preview in [1.0, 0.5] {
            let opts = RenderOptions { scale: preview, ..Default::default() };
            let reference = render_sequence(&r.p, outer, t, opts, &r.map);
            let frame_plan = plan::plan_frame(&r.p, outer, t, opts, plan::DeviceCaps::ALL, &r.map);
            let planned = plan::execute_cpu(&frame_plan);
            let difference = worst(&planned, &reference);
            assert!(difference < 0.001, "scale {scale}, rotation {rotation}, preview {preview}: {difference}");
        }
    }
}

#[test]
fn a_transformed_nest_keeps_captions_at_its_held_source_time() {
    let mut r = Rig::new();
    let (inner, outer, nest) = two_layer_nest(&mut r);
    let mut captions = CaptionTrack::new(TrackId(9901), "Nested".into(), CaptionFormat::Subtitle);
    captions.captions.push(Caption {
        id: ClipId(9902),
        start: Tick::ZERO,
        duration: Tick(TICKS_PER_SECOND),
        text: "HELD".into(),
        speaker: None,
        cue_id: None,
        settings: String::new(),
    });
    r.p.sequence_mut(inner).unwrap().caption_tracks.push(captions);
    let clip = r.clip(outer, nest);
    clip.frame_hold = Some(FrameRate::FPS_24.tick_of(12));
    clip.effect_mut("motion").unwrap().param_mut("scale").unwrap().value = ParamValue::Float(75.0);
    let t = FrameRate::FPS_24.tick_of(30);
    let with = r.frame(outer, t);
    assert!(worst(&r.planned(outer, t), &with) < 0.001);
    r.p.sequence_mut(inner).unwrap().caption_tracks[0].enabled = false;
    let without = r.frame(outer, t);
    assert!(worst(&with, &without) > 0.1, "held caption must be visible after its outer-time end");
    assert!(worst(&r.planned(outer, t), &without) < 0.001);
}

/// A project that stores a media width far from the file's (here 1 against 4096) used to scale
/// every effect size by the ratio. The median filter's cost is its radius squared per pixel.
#[test]
fn a_wrong_stored_media_width_does_not_blow_up_effect_sizes() {
    let mut r = Rig::new();
    let m = r.matte(RED, 4096, 16);
    if let Some(v) = r.p.item_mut(m).and_then(|i| i.as_media_mut()).and_then(|c| c.info.video.as_mut()) {
        v.width = 1;
    }
    let s = r.seq("S", 4096, 16, FrameRate::FPS_24);
    let c = r.put(s, m, 10);
    let mut e = filmcraft_project::find_effect("median").unwrap().instance();
    e.params.get_mut("radius").unwrap().value = ParamValue::Float(1.0);
    r.clip(s, c).effects.push(e);
    let t = std::time::Instant::now();
    let _ = r.frame(s, Tick::ZERO);
    assert!(t.elapsed() < std::time::Duration::from_secs(2), "{:?}", t.elapsed());
}

// ---- Frame Blending / Optical Flow on a slowed nest

/// A 24 fps movie whose frame `k` is one flat grey, a step brighter than frame `k - 1`. It counts
/// the frames it hands out.
struct Steps {
    info: filmcraft_media::MediaInfo,
    fetched: Arc<std::sync::atomic::AtomicUsize>,
}

impl Steps {
    /// The grey (sRGB code) of frame `k`.
    fn code(k: i64) -> u8 {
        (40 + k.clamp(0, 20) * 10) as u8
    }
}

impl MediaSource for Steps {
    fn info(&self) -> &filmcraft_media::MediaInfo {
        &self.info
    }
    fn video_frame(&self, req: FrameRequest) -> filmcraft_media::Result<Arc<VideoFrame>> {
        self.fetched.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (w, h) = self.info.video.as_ref().map_or((1, 1), |v| (v.width, v.height));
        let v = Steps::code(FrameRate::FPS_24.frame_at(req.time));
        Ok(Arc::new(VideoFrame::rgba8(w, h, [v, v, v, 255].repeat((w * h) as usize))))
    }
    fn audio(&self, _start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<filmcraft_frame::AudioBuffer> {
        Ok(filmcraft_frame::AudioBuffer::silence(sample_rate, 2, frames))
    }
}

/// `inner` holds 48 frames of [`Steps`]; `outer` holds 48 frames of `inner` at a third of its
/// speed (16 frames of the nest). Returns (inner, outer, nest clip, frames fetched).
fn slowed_steps(r: &mut Rig) -> (ItemId, ItemId, ClipId, Arc<std::sync::atomic::AtomicUsize>) {
    let steps = r.matte(RED, 64, 36);
    let fetched = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let info = r.map.0[&steps].info().clone();
    r.map.0.insert(steps, Arc::new(Steps { info, fetched: fetched.clone() }) as SharedSource);
    let inner = r.seq("inner", 64, 36, FrameRate::FPS_24);
    r.put(inner, steps, 48);
    let outer = r.seq("outer", 64, 36, FrameRate::FPS_24);
    let nest = r.put(outer, inner, 48);
    r.clip(outer, nest).speed = 1.0 / 3.0;
    (inner, outer, nest, fetched)
}

#[test]
fn frame_blending_on_a_slowed_nest_mixes_the_two_neighbouring_frames_of_its_sequence() {
    use filmcraft_project::TimeInterpolation;
    let mut r = Rig::new();
    let (inner, outer, nest, _) = slowed_steps(&mut r);
    let f = |n: i64| FrameRate::FPS_24.tick_of(n);
    let grey = |img: &Image| img.get(32, 18)[0];
    let (one, two) = (grey(&r.frame(inner, f(1))), grey(&r.frame(inner, f(2))));
    assert!(two - one > 0.01, "the nest's frames differ: {one} {two}");
    // programme frames 3, 4 and 5 show the nest 1, 1⅓ and 1⅔ frames in
    // Frame Sampling repeats frame 1 of the nest
    for n in [3, 4, 5] {
        assert!((grey(&r.frame(outer, f(n))) - one).abs() < 1e-4, "frame sampling, frame {n}");
    }
    for mode in [TimeInterpolation::FrameBlending, TimeInterpolation::OpticalFlow] {
        r.clip(outer, nest).time_interpolation = mode;
        for (n, w) in [(4, 1.0 / 3.0), (5, 2.0 / 3.0)] {
            let t = f(n);
            let got = r.frame(outer, t);
            let want = one + (two - one) * w;
            assert!(grey(&got) > one + 0.003 && grey(&got) < two - 0.003, "{mode:?}, frame {n}: {} is not between {one} and {two}", grey(&got));
            assert!((grey(&got) - want).abs() < 2e-3, "{mode:?}, frame {n}: {} for {want}", grey(&got));
            // every pixel is that mix, opaque
            assert!(got.px.as_chunks::<4>().0.iter().all(|p| (p[0] - want).abs() < 2e-3 && (p[3] - 1.0).abs() < 1e-4), "{mode:?}, frame {n}");
            // and the frame plan (what the GPU compositor is given) draws the same picture
            assert!(worst(&r.planned(outer, t), &got) < 1e-3, "{mode:?}, frame {n}: the plan differs");
        }
        // on a frame of the nest there is nothing to mix
        assert!((grey(&r.frame(outer, f(3))) - one).abs() < 1e-4, "{mode:?}: frame 3 is frame 1 of the nest");
        assert!(worst(&r.planned(outer, f(3)), &r.frame(inner, f(1))) < 1e-3, "{mode:?}: planned frame 3");
        // the nest's last frame (15) is shown alone: frame 16 was trimmed away
        let last = grey(&r.frame(inner, f(15)));
        for n in [45, 46, 47] {
            assert!((grey(&r.frame(outer, f(n))) - last).abs() < 1e-4, "{mode:?}, frame {n}: the last frame alone");
        }
    }
    // reversed, the same pair is mixed: 1⅓ frames back from frame 16 lies between frames 14 and 15
    let c = r.clip(outer, nest);
    c.time_interpolation = TimeInterpolation::FrameBlending;
    c.reverse = true;
    let (a, b) = (grey(&r.frame(inner, f(14))), grey(&r.frame(inner, f(15))));
    let got = grey(&r.frame(outer, f(4)));
    assert!((got - (a + (b - a) * 2.0 / 3.0)).abs() < 2e-3, "reversed: {got} between {a} and {b}");
    // a frame hold shows its one frame whatever the interpolation
    let c = r.clip(outer, nest);
    c.reverse = false;
    c.frame_hold = Some(f(7));
    let held = grey(&r.frame(inner, f(7)));
    for n in [4, 5, 20] {
        assert!((grey(&r.frame(outer, f(n))) - held).abs() < 1e-4, "held, frame {n}");
        assert!(worst(&r.planned(outer, f(n)), &r.frame(inner, f(7))) < 1e-3, "held, planned frame {n}");
    }
}

#[test]
fn frame_blending_mixes_whole_frames_of_a_nest_that_animates_between_them() {
    // Opacity keyframes inside the nest are evaluated at any time asked for, so the two pictures
    // mixed must be the nest's frames, not the nest at the in-between time and its next frame.
    // The fade here is over a quarter of the way from frame 1 to frame 2: opaque on frame 1, 40 %
    // from 1¼ frames on. At 1⅓ frames the nest itself is at 40 %, the mix of its frames at 80 %.
    let mut r = Rig::new();
    let (inner, outer, nest, _) = slowed_steps(&mut r);
    let f = |n: i64| FrameRate::FPS_24.tick_of(n);
    let inside = r.p.sequence(inner).unwrap().video_tracks[0].items[0].id;
    let opacity = r.clip(inner, inside).effect_mut("opacity").unwrap().param_mut("opacity").unwrap();
    let key = |t: Tick, v: f64| filmcraft_project::Keyframe::new(t, ParamValue::Float(v));
    opacity.keyframes = vec![key(f(1), 100.0), key(Tick(f(1).0 * 5 / 4), 40.0), key(f(2), 40.0)];
    let alpha = |img: &Image| img.get(32, 18)[3];
    let (one, two) = (alpha(&r.frame(inner, f(1))), alpha(&r.frame(inner, f(2))));
    assert!((one - 1.0).abs() < 1e-3 && (two - 0.4).abs() < 1e-3, "the nest fades: {one} {two}");
    assert!((alpha(&r.frame(inner, Tick(f(1).0 * 4 / 3))) - 0.4).abs() < 1e-3, "and has faded a third of a frame in");
    r.clip(outer, nest).time_interpolation = filmcraft_project::TimeInterpolation::FrameBlending;
    for (n, want) in [(4, 0.8), (5, 0.6)] {
        let got = alpha(&r.frame(outer, f(n)));
        assert!((got - want).abs() < 2e-3, "frame {n}: {got} for {want}, between {one} and {two}");
    }
}

#[test]
fn a_reversed_blended_nest_renders_one_picture_on_a_frame_of_its_sequence() {
    // A reversed clip's time is a tick short of its mirrored position, so on a frame of the nest
    // the later frame of the pair weighs everything: that frame alone is rendered, not two
    // pictures with the first thrown away.
    let mut r = Rig::new();
    let (inner, outer, nest, fetched) = slowed_steps(&mut r);
    let f = |n: i64| FrameRate::FPS_24.tick_of(n);
    let grey = |img: &Image| img.get(32, 18)[0];
    let c = r.clip(outer, nest);
    c.time_interpolation = filmcraft_project::TimeInterpolation::FrameBlending;
    c.reverse = true;
    let fifteen = grey(&r.frame(inner, f(15)));
    let count = |r: &Rig, n: i64| {
        fetched.store(0, std::sync::atomic::Ordering::Relaxed);
        let img = r.frame(outer, f(n));
        (grey(&img), fetched.load(std::sync::atomic::Ordering::Relaxed))
    };
    // programme frame 3 is one frame back from frame 16 of the nest: frame 15, rendered once
    let (got, n) = count(&r, 3);
    assert!((got - fifteen).abs() < 1e-4, "frame 15 of the nest: {got} for {fifteen}");
    assert_eq!(n, 1, "one picture of the nest");
    // and the plan draws that frame's layers itself (no image of the nest)
    assert_eq!(layer_count(&r, outer, f(3)), Some(1));
    assert!(worst(&r.planned(outer, f(3)), &r.frame(inner, f(15))) < 1e-3);
    // a third of a frame further back, two frames of the nest are mixed
    let (got, n) = count(&r, 4);
    assert!(got < fifteen - 0.003, "between frames 14 and 15: {got}");
    assert_eq!(n, 2, "two pictures of the nest");
}

#[test]
fn frame_blending_on_a_nest_follows_its_speed_and_its_frame_rate() {
    use filmcraft_project::TimeInterpolation;
    let f = |n: i64| FrameRate::FPS_24.tick_of(n);
    let grey = |img: &Image| img.get(32, 18)[0];
    // 150 %: programme frame 1 shows the nest 1½ frames in
    let mut r = Rig::new();
    let (inner, outer, nest, _) = slowed_steps(&mut r);
    let (one, two, three) = (grey(&r.frame(inner, f(1))), grey(&r.frame(inner, f(2))), grey(&r.frame(inner, f(3))));
    let c = r.clip(outer, nest);
    c.speed = 1.5;
    c.duration = f(20);
    assert!((grey(&r.frame(outer, f(1))) - one).abs() < 1e-4, "frame sampling at 150 %");
    r.clip(outer, nest).time_interpolation = TimeInterpolation::FrameBlending;
    let got = r.frame(outer, f(1));
    assert!((grey(&got) - (one + two) / 2.0).abs() < 2e-3, "150 %: {} half-way between {one} and {two}", grey(&got));
    assert!(worst(&r.planned(outer, f(1)), &got) < 1e-3, "150 %: the plan differs");
    assert!((grey(&r.frame(outer, f(2))) - three).abs() < 1e-4, "150 %: frame 2 is frame 3 of the nest");
    // 100 % in a 30 fps sequence: its frame 1 is 0.8 of a frame into the 24 fps nest
    let mut r = Rig::new();
    let (inner, _, _, _) = slowed_steps(&mut r);
    let (zero, one) = (grey(&r.frame(inner, f(0))), grey(&r.frame(inner, f(1))));
    let outer = r.seq("thirty", 64, 36, FrameRate::FPS_30);
    let nest = r.put(outer, inner, 30);
    let t = FrameRate::FPS_30.tick_of(1);
    assert!((grey(&r.frame(outer, t)) - zero).abs() < 1e-4, "frame sampling at another rate");
    r.clip(outer, nest).time_interpolation = TimeInterpolation::FrameBlending;
    let got = r.frame(outer, t);
    assert!((grey(&got) - (zero + (one - zero) * 0.8)).abs() < 2e-3, "another rate: {} at 0.8 between {zero} and {one}", grey(&got));
    assert!(worst(&r.planned(outer, t), &got) < 1e-3, "another rate: the plan differs");
    // frame 5 of the 30 fps sequence is frame 4 of the nest
    assert!(worst(&r.frame(outer, FrameRate::FPS_30.tick_of(5)), &r.frame(inner, f(4))) < 1e-4);
}

#[test]
fn a_blended_nest_too_deep_to_render_twice_shows_the_frame_of_frame_sampling() {
    let mut r = Rig::new();
    let (inner, outer, nest, fetched) = slowed_steps(&mut r);
    let f = |n: i64| FrameRate::FPS_24.tick_of(n);
    let grey = |img: &Image| img.get(32, 18)[0];
    let (one, two) = (grey(&r.frame(inner, f(1))), grey(&r.frame(inner, f(2))));
    r.clip(outer, nest).time_interpolation = filmcraft_project::TimeInterpolation::FrameBlending;
    let at = |depth: u32| {
        fetched.store(0, std::sync::atomic::Ordering::Relaxed);
        let img = render_sequence(&r.p, outer, f(4), RenderOptions { depth, ..Default::default() }, &r.map);
        (grey(&img), fetched.load(std::sync::atomic::Ordering::Relaxed))
    };
    // with two levels left under it the nest is mixed
    let (got, n) = at(MAX_NEST_DEPTH - 2);
    assert!(got > one + 0.003 && got < two - 0.003 && n == 2, "mixed: {got} from {n} pictures");
    // with one level left it shows frame 1 of its sequence, as Frame Sampling does
    let (got, n) = at(MAX_NEST_DEPTH - 1);
    assert!((got - one).abs() < 1e-4 && n == 1, "sampled: {got} for {one} from {n} pictures");
}

#[test]
fn blended_nests_in_a_sequence_that_contains_itself_stay_bounded() {
    // Only a damaged project has a sequence nested in itself. Each blended nest renders two frames
    // of its sequence, so the renders would double at every level down to `MAX_NEST_DEPTH`.
    let mut r = Rig::new();
    let (inner, _, _, fetched) = slowed_steps(&mut r);
    let rate = FrameRate::FPS_24;
    let mut own = r.p.make_track_item(inner, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.tick_of(48)), rate).unwrap();
    own.speed = 1.0 / 3.0;
    own.time_interpolation = filmcraft_project::TimeInterpolation::FrameBlending;
    // a third of a frame in, so that frames 0 and 1 both fall between two frames of the nest
    own.source_in = Tick(rate.tick_of(1).0 / 3);
    own.effect_mut("opacity").unwrap().params.get_mut("opacity").unwrap().value = ParamValue::Float(50.0);
    r.p.sequence_mut(inner).unwrap().video_tracks[1].items.push(own);
    fetched.store(0, std::sync::atomic::Ordering::Relaxed);
    let img = r.frame(inner, Tick::ZERO);
    assert_eq!((img.w, img.h), (64, 36));
    let n = fetched.load(std::sync::atomic::Ordering::Relaxed);
    assert!((2..=40).contains(&n), "{n} frames fetched for one frame");
}

#[test]
fn frame_blending_on_a_slowed_multicam_clip_mixes_two_frames_of_its_angle() {
    let mut r = Rig::new();
    let (inner, outer, nest, _) = slowed_steps(&mut r);
    // a brighter matte on the angle that is not shown
    let other = r.matte([1.0, 1.0, 1.0, 1.0], 64, 36);
    let rate = FrameRate::FPS_24;
    let top = r.p.make_track_item(other, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.tick_of(48)), rate).unwrap();
    r.p.sequence_mut(inner).unwrap().video_tracks[1].items.push(top);
    let f = |n: i64| rate.tick_of(n);
    let grey = |img: &Image| img.get(32, 18)[0];
    let c = r.clip(outer, nest);
    c.multicam = Some(filmcraft_project::MulticamSel { enabled: true, angle: 0 });
    let (one, two) = (grey(&r.frame(outer, f(3))), grey(&r.frame(outer, f(6))));
    assert!(two - one > 0.01 && two < 0.5, "the angle's frames 1 and 2: {one} {two}");
    assert!((grey(&r.frame(outer, f(4))) - one).abs() < 1e-4, "frame sampling repeats frame 1");
    r.clip(outer, nest).time_interpolation = filmcraft_project::TimeInterpolation::FrameBlending;
    let got = r.frame(outer, f(4));
    assert!((grey(&got) - (one + (two - one) / 3.0)).abs() < 2e-3, "{} between {one} and {two}", grey(&got));
    assert!(worst(&r.planned(outer, f(4)), &got) < 1e-3, "the plan differs");
}
