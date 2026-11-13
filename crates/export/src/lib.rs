//! Export: render a sequence range and encode it to a file.
//!
//! Frames are rendered in parallel batches (one frame per core, each frame itself row-parallel),
//! then encoded and muxed in order; audio is mixed per batch and interleaved. Progress and cancel
//! are shared atomics so the UI (Export mode, header progress) and MCP can observe/cancel jobs.
//!
//! Video encoders implement [`VideoEncoder`]; codec crates register theirs with
//! [`register_encoder`] (H.264, ProRes …). Built in: Motion-JPEG (MOV), PNG / TIFF / BMP / JPEG /
//! Targa / DPX / OpenEXR sequences, GIF, WAV and AIFF.
//!
//! [`ExportSettings`] carries every Export-mode setting (frame size, rate, bitrate encoding, audio
//! format, multiplexer, captions, effects, metadata) as serde data; [`presets`] defines the
//! built-in presets.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

use std::io::Write;

mod aac_out;
mod audio_out;
mod deflate;
mod job;
mod mp3_out;
mod mxf_out;
mod pace;
mod pcm;
mod pipeline;
pub mod presets;
mod raster;
pub mod settings;
pub mod still;
pub use audio_out::LoudnessReport;
pub use job::{Exporter, Step, stepped};
pub use mxf_out::opatom_audio_paths;
pub use pcm::{image_sequence_path, write_aiff, write_wav};
pub use pipeline::limit_rgba8;
pub use presets::{ExportPreset, builtin_presets};
pub use settings::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, RwLock};

use filmagic_isobmff::SampleEntry;
use filmagic_project::{ItemId, Project};
use filmagic_render::SourceProvider;
use filmagic_time::{FrameRate, Tick, TimeRange};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

mod convert;
mod encoders;
mod format;
mod output;
mod registry;
mod run;
mod smart;
mod stats;
mod still_encode;

pub use convert::{
    Matrix, Planes444, rgba_to_yuv420_8, rgba_to_yuv420_10, rgba_to_yuv422_10, rgbf_to_yuv420_8, rgbf_to_yuv420_10, rgbf_to_yuv422_10, to_yuv444_10,
};
pub(crate) use encoders::FlacAudio;
#[cfg(test)]
use encoders::{aac_factory, h264_factory};
pub use encoders::{apv_profile, dnx_profile, prores_profile};
pub use format::{ColorSignal, DEFAULT_CRF, ExportSettings, Format, H264Pass};
use output::Out;
pub use output::{OutputSink, path_is_file};
pub(crate) use registry::build_frame_renderer;
#[cfg(test)]
pub(crate) use registry::reset_frame_renderers_for_tests;
pub use registry::{
    AudioEncoder, AudioEncoderFactory, EncodedPacket, EncoderFactory, EncoderFrame, FrameRenderer, FrameRendererFactory, PrepareAhead, VideoEncoder, available,
    encoder_registered, frame_renderer_registered, hdr_available, register_audio_encoder, register_encoder, register_format_probe, register_frame_renderer,
    register_hdr_probe,
};
use registry::{audio_factories, video_factories};
#[cfg(test)]
use run::gif_delay_cs;
pub use run::{export, export_range, frame_span, validate_range};
pub(crate) use stats::note_lock_wait;
pub use stats::{
    GpuRenderStats, HwEncodeStats, Stage, export_lock_wait, gpu_render_stats, hw_encode_stats, note_gpu_fallback, note_gpu_frame, note_hw_encode_declined,
    note_hw_encode_declined_because, note_hw_encode_frame, note_hw_encode_session, note_stage, reset_export_lock_wait, stage_times, timed,
};
pub use still_encode::{encode_png, encode_still, encode_still_with, encode_still_with_quality};

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("no such sequence")]
    NoSequence,
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("I/O: {0}")]
    Io(String),
    #[error("encode: {0}")]
    Encode(String),
    #[error("cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, ExportError>;

/// Shared progress/cancel state of an export job.
#[derive(Default)]
pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
    pub cancel: AtomicBool,
    pub finished: AtomicBool,
    pub status: Mutex<String>,
    pub error: Mutex<Option<String>>,
    /// What loudness normalization measured (when it ran).
    pub loudness: Mutex<Option<LoudnessReport>>,
    /// Readings of `done` over time, for [`Progress::eta`].
    pace: Mutex<pace::Pace>,
}

impl Progress {
    pub fn fraction(&self) -> f32 {
        let t = self.total.load(Ordering::Relaxed).max(1);
        self.done.load(Ordering::Relaxed) as f32 / t as f32
    }

    /// The estimated time left, from the job's speed over the last 15 seconds. Each call is also a
    /// reading of that speed, so ask regularly (the UI does on every frame it draws the job).
    /// `None` until a second of readings shows progress, and once the job is done or finished.
    pub fn eta(&self) -> Option<std::time::Duration> {
        self.eta_at(web_time::Instant::now())
    }

    /// [`Progress::eta`] at a given time (tests, and hosts with a clock of their own).
    pub fn eta_at(&self, now: web_time::Instant) -> Option<std::time::Duration> {
        let (done, total) = (self.done.load(Ordering::Relaxed), self.total.load(Ordering::Relaxed));
        let mut pace = self.pace.lock().unwrap_or_else(PoisonError::into_inner);
        pace.observe(now, done);
        if self.finished.load(Ordering::Relaxed) || done >= total {
            return None;
        }
        pace.eta(total - done)
    }
    fn set_status(&self, s: impl Into<String>) {
        *self.status.lock().unwrap_or_else(PoisonError::into_inner) = s.into();
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Report {
    pub path: String,
    pub frames: u64,
    pub seconds: f64,
    pub bytes: u64,
    pub render_fps: f64,
    /// Further files written besides `path` (MXF OP-Atom audio files).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extra_files: Vec<String>,
    /// Which video encoder and renderer did the work, and why hardware was not used (the
    /// encoded-video formats: H.264, H.265, ProRes …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoder: Option<Box<EncoderReport>>,
    /// Seconds per export stage ([`Stage::name`]), so a slow export says where its time went: summed
    /// over the threads that ran the stage (and over any export running at the same time, as the
    /// counters are process-wide), so they do not add up to `seconds`.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub stages: std::collections::BTreeMap<String, f64>,
    /// What an agent can act on (a hardware encoder that was asked for and not used …).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// The encoder and renderer of an export ([`Report::encoder`]).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EncoderReport {
    /// The video encoder that wrote the stream ([`VideoEncoder::name`]).
    pub name: String,
    /// A hardware encoder (NVENC, VideoToolbox) wrote it.
    pub hardware: bool,
    /// `hardwareEncoding` was `auto`.
    pub hardware_requested: bool,
    /// Why each hardware encoder that was asked declined (empty when none was asked or one took it).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hardware_declined: Vec<String>,
    /// `gpuRendering` was `auto`.
    pub gpu_requested: bool,
    /// Frames composited on the GPU, and frames the CPU renderer drew instead (both 0 with
    /// `gpuRendering` off). Process-wide counters taken over the export.
    pub gpu_frames: u64,
    pub cpu_frames: u64,
    /// Smart render: segments whose packets came from the cache, and segments encoded (absent
    /// when the export did not smart-render).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub segments_reused: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub segments_encoded: Option<u64>,
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod settings_tests;

#[cfg(test)]
mod surround_tests;

#[cfg(test)]
mod mxf_tests;

#[cfg(test)]
mod determinism_tests;

#[cfg(test)]
mod hdr_tests;

#[cfg(test)]
mod speed_tests;

#[cfg(test)]
mod smart_tests;
