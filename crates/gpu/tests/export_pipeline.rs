//! End-to-end GPU export parity: the registered `filmagic-gpu` frame renderer drives the full
//! export pipeline (`filmagic_export::export` → PNG sequence) and its frames must match the
//! `GpuRendering::Off` (CPU reference) frames within the GPU-vs-CPU parity tolerance — p99 of the
//! per-pixel max RGB difference over black ≤ 6 and mean < 1.5 (the golden-test criterion).

use filmagic_export::{ExportSettings, Format, FrameRenderer, GpuRenderStats, GpuRendering, Progress, export, register_frame_renderer};
use filmagic_media::generators::GeneratorSource;
use filmagic_media::{Generator, MediaSource};
use filmagic_project::{ItemKind, Label, MediaClip, MediaRef, SequenceSettings, TrackKind};
use filmagic_render::SourceMap;
use filmagic_time::{TICKS_PER_SECOND, TimeRange};
use std::sync::Arc;

/// The same wrapper the app and CLI register: `filmagic-gpu`'s renderer behind
/// `filmagic_export`'s trait.
struct GpuFrameRenderer(filmagic_gpu::ExportRenderer);

impl FrameRenderer for GpuFrameRenderer {
    fn render(
        &mut self,
        project: &filmagic_project::Project,
        seq: filmagic_project::ItemId,
        t: filmagic_time::Tick,
        opts: filmagic_render::RenderOptions,
        sources: &dyn filmagic_render::SourceProvider,
    ) -> Option<filmagic_render::Image> {
        self.0.render(project, seq, t, opts, sources)
    }

    fn caps(&self) -> filmagic_render::plan::DeviceCaps {
        self.0.caps()
    }
}

/// The colour matte on V1, and with `adjusted` an adjustment layer over it on V2 (Proc Amp and
/// Gaussian Blur, opaque, Normal, full frame: the kind the GPU runs as a `PlanStep::Adjust`).
fn project_with(width: u32, height: u32, adjusted: bool) -> (Arc<filmagic_project::Project>, filmagic_project::ItemId, SourceMap) {
    let mut p = filmagic_project::Project::new("x");
    let g = GeneratorSource::new(
        Generator::ColorMatte { color: [0.2, 0.5, 0.9, 1.0] },
        320,
        180,
        filmagic_time::FrameRate::FPS_24,
        filmagic_time::Tick(2 * TICKS_PER_SECOND),
    );
    let info = g.info().clone();
    let red = p.add_item(
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
    let tracks = if adjusted { 2 } else { 1 };
    let seq = p.new_sequence("s", SequenceSettings { width, height, frame_rate: filmagic_time::FrameRate::FPS_24, ..Default::default() }, tracks, 1, None);
    let v = p
        .make_track_item(
            red,
            TrackKind::Video,
            filmagic_time::Tick::ZERO,
            TimeRange::new(filmagic_time::Tick::ZERO, filmagic_time::FrameRate::FPS_24.tick_of(2)),
            filmagic_time::FrameRate::FPS_24,
        )
        .unwrap();
    p.sequence_mut(seq).unwrap().video_tracks[0].items.push(v);
    if adjusted {
        let rate = filmagic_time::FrameRate::FPS_24;
        let a = p.add_item("Adjustment Layer", Label::Lavender, ItemKind::AdjustmentLayer { width, height, rate, duration: rate.tick_of(2) }, None);
        let mut ti =
            p.make_track_item(a, TrackKind::Video, filmagic_time::Tick::ZERO, TimeRange::new(filmagic_time::Tick::ZERO, rate.tick_of(2)), rate).unwrap();
        for (id, param, v) in [("proc_amp", "brightness", 25.0), ("gaussian_blur", "blurriness", 6.0)] {
            let mut e = filmagic_project::find_effect(id).unwrap().instance();
            e.param_mut(param).unwrap().value = filmagic_project::ParamValue::Float(v);
            ti.effects.insert(0, e);
        }
        p.sequence_mut(seq).unwrap().video_tracks[1].items.push(ti);
    }
    let mut m = SourceMap::default();
    m.0.insert(red, Arc::new(g));
    (Arc::new(p), seq, m)
}

fn png(dir: &str, name: &str) -> Vec<u8> {
    // image_sequence_path flattens the path stem: <stem><index>.png in the temp dir itself.
    let index: u64 = name.trim_start_matches("img").trim_end_matches(".png").parse().unwrap_or(0);
    std::fs::read(std::env::temp_dir().join(format!("fc-gpu-export-{dir}{index:03}.png"))).unwrap()
}

fn export_png(dir: &str, gpu: GpuRendering, size: (u32, u32)) {
    export_project_png(dir, gpu, size, false);
}

fn export_project_png(dir: &str, gpu: GpuRendering, (w, h): (u32, u32), adjusted: bool) {
    let (p, seq, m) = project_with(w, h, adjusted);
    let mut s = ExportSettings {
        format: Format::PngSequence,
        path: std::env::temp_dir().join(format!("fc-gpu-export-{dir}")).to_string_lossy().into_owned(),
        ..Default::default()
    };
    s.range = Some(TimeRange::new(filmagic_time::Tick::ZERO, filmagic_time::FrameRate::FPS_24.tick_of(2)));
    s.gpu_rendering = gpu;
    export(&p, seq, &s, &m, &Progress::default()).unwrap();
}

fn assert_parity(cpu_png: &[u8], gpu_png: &[u8], what: &str) {
    let a = image::load_from_memory(cpu_png).unwrap().to_rgba8();
    let b = image::load_from_memory(gpu_png).unwrap().to_rgba8();
    assert_eq!(a.dimensions(), b.dimensions(), "{what}: size mismatch");
    let d: Vec<u32> = a
        .as_raw()
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.as_raw().as_chunks::<4>().0)
        .map(|(x, y)| (0..3).map(|k| (x[k] as u32).abs_diff(y[k] as u32)).max().unwrap_or(0))
        .collect();
    let mut d = d;
    d.sort_unstable();
    let p99 = d[d.len() * 99 / 100];
    let mean = d.iter().sum::<u32>() as f64 / d.len() as f64;
    eprintln!("{what}: GPU vs CPU p99 {p99}, mean {mean:.3}");
    assert!(p99 <= 6 && mean < 1.5, "{what}: GPU parity p99 {p99}, mean {mean:.3}");
}

#[test]
fn gpu_export_matches_cpu_and_counts() {
    let Some(_) = filmagic_gpu::ExportRenderer::new() else {
        eprintln!("SKIPPED (gpu export pipeline): no adapter");
        return;
    };
    // Clean registry first: Auto with no factory must be byte-equal to Off.
    export_png("off-cpu", GpuRendering::Off, (320, 180));
    export_png("off-auto", GpuRendering::Auto, (320, 180));
    assert_eq!(png("off-cpu", "img000.png"), png("off-auto", "img000.png"), "Auto with no registered renderer must be byte-equal to Off");

    register_frame_renderer(|| Some(Box::new(GpuFrameRenderer(filmagic_gpu::ExportRenderer::new().expect("adapter present")))));
    let sizes = [(320u32, 180u32), (17, 13), (1, 1), (257, 129)];
    for (i, size) in sizes.into_iter().enumerate() {
        export_png("cpu", GpuRendering::Off, size);
        export_png("gpu", GpuRendering::Auto, size);
        for f in ["000", "001"] {
            assert_parity(&png("cpu", &format!("img{f}.png")), &png("gpu", &format!("img{f}.png")), &format!("{:?} frame {f}", size));
        }
        let _ = i;
    }
    let GpuRenderStats { frames, .. } = filmagic_export::gpu_render_stats();
    assert!(frames >= 8, "gpu frames counted across the parity exports: {frames}");

    // An adjustment layer is composited by the GPU too (a `PlanStep::Adjust`), not handed to the
    // CPU renderer, and matches the CPU export.
    let composited = filmagic_gpu::export_renderer::timing::get().frames;
    let GpuRenderStats { fallbacks, .. } = filmagic_export::gpu_render_stats();
    export_project_png("adj-cpu", GpuRendering::Off, (320, 180), true);
    export_project_png("adj-gpu", GpuRendering::Auto, (320, 180), true);
    for f in ["000", "001"] {
        assert_parity(&png("adj-cpu", &format!("img{f}.png")), &png("adj-gpu", &format!("img{f}.png")), &format!("adjustment layer frame {f}"));
    }
    assert_ne!(png("adj-cpu", "img000.png"), png("off-cpu", "img000.png"), "the adjustment layer changes the picture");
    let gpu = filmagic_gpu::export_renderer::timing::get().frames - composited;
    assert_eq!(gpu, 2, "both frames with the adjustment layer composited on the GPU");
    assert_eq!(filmagic_export::gpu_render_stats().fallbacks, fallbacks, "no frame with the adjustment layer fell back to the CPU");
}
