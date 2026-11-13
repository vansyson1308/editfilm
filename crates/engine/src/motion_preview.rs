//! `preview.motion` (#16): a few seconds of the active sequence as a small animated GIF or MP4, so
//! a transition, a zoom, a speed change or a moment of action can be checked as it moves; a still
//! (`render_frame`) cannot show that.
//!
//! The preview is capped: at most [`MAX_SECONDS`] seconds, [`MAX_FPS`] frames per second and
//! [`MAX_WIDTH`] pixels wide, so it renders in seconds and stays small enough to hand to an MCP
//! client as an image. It is written synchronously; for anything longer use `file.exportMedia`.

use filmagic_time::{FrameRate, Tick, TimeRange};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, NamedKeys, bad, has_seq, str_p};
use crate::{EngineError, Result, Session};

const CMD: &str = "preview.motion";
/// Longest preview, seconds.
pub const MAX_SECONDS: f64 = 10.0;
/// Highest frame rate of a preview.
pub const MAX_FPS: f64 = 30.0;
/// Widest preview, pixels.
pub const MAX_WIDTH: u32 = 640;

fn num_p(p: &Value, key: &str, lo: f64, hi: f64, what: &str) -> Result<Option<f64>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_f64().filter(|x| x.is_finite() && (lo..=hi).contains(x)).map(Some).ok_or_else(|| bad(CMD, format!("`{key}` must be {what}"))),
    }
}

fn even(v: f64) -> u32 {
    ((v / 2.0).round() as u32 * 2).max(2)
}

fn preview(s: &mut Session, p: &Value) -> Result<Value> {
    let seq = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let q = s.project.sequence(seq).ok_or(EngineError::NoSequence)?;
    let path = str_p(p, "path")
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(crate::export_tools::expand_home)
        .ok_or_else(|| bad(CMD, "need `path`: where to write the preview (.gif or .mp4)"))?;
    let lower = path.to_ascii_lowercase();
    let (format, kind) = if lower.ends_with(".gif") {
        (filmagic_export::Format::Gif, "gif")
    } else if lower.ends_with(".mp4") {
        (filmagic_export::Format::H264, "mp4")
    } else {
        return Err(bad(CMD, "`path` must end in .gif (an animated image an MCP client can show) or .mp4"));
    };
    if crate::layers::is_project_media(s, &path) {
        return Err(bad(CMD, format!("{path} is media of this project; write the preview to another file")));
    }
    let end = q.duration();
    if end <= Tick::ZERO {
        return Err(bad(CMD, "the sequence is empty: there is nothing to preview"));
    }
    let rate = q.frame_rate();
    let from_s = num_p(p, "fromSeconds", 0.0, 1e7, "seconds of the sequence, 0 or more")?;
    let to_s = num_p(p, "toSeconds", 0.0, 1e7, "seconds of the sequence, 0 or more")?;
    let from = from_s.map_or_else(|| s.playhead(), Tick::from_seconds_f64).min(end);
    let to = to_s.map_or_else(|| from + Tick::from_seconds_f64(3.0), Tick::from_seconds_f64).min(end);
    if to <= from {
        return Err(bad(
            CMD,
            format!("nothing to preview between {:.3} s and {:.3} s (the sequence ends at {:.3} s)", from.seconds(), to.seconds(), end.seconds()),
        ));
    }
    if (to - from).seconds() > MAX_SECONDS + 1e-6 {
        return Err(bad(
            CMD,
            format!(
                "{:.2} s is longer than a preview ({MAX_SECONDS:.0} s at most); pass `fromSeconds`/`toSeconds` closer together, or export with `file.exportMedia`",
                (to - from).seconds()
            ),
        ));
    }
    let seq_fps = rate.as_f64();
    let fps = num_p(p, "fps", 1.0, MAX_FPS, "frames per second from 1 to 30")?
        .unwrap_or(if kind == "gif" { 12.0 } else { seq_fps.min(MAX_FPS) })
        .min(seq_fps.max(1.0));
    let width = num_p(p, "width", 64.0, f64::from(MAX_WIDTH), "pixels from 64 to 640")?.unwrap_or(480.0);
    let (sw, sh) = (f64::from(q.settings.width.max(1)), f64::from(q.settings.height.max(1)));
    let par = filmagic_project::par_ratio(filmagic_project::sane_par(q.settings.par));
    let width = width.min(sw * par);
    let height = width * sh / (sw * par);
    let (w, h) = (even(width), even(height));
    let settings = filmagic_export::ExportSettings {
        format,
        path: path.clone(),
        range: Some(TimeRange::from_bounds(from, to)),
        scale: (f64::from(w) / sw) as f32,
        include_audio: kind == "mp4",
        frame_size: Some((w, h)),
        frame_rate: Some(FrameRate::from_f64(fps)),
        bitrate_kbps: 1500,
        burn_captions: true,
        ..Default::default()
    };
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let report = filmagic_export::export(&s.project, seq, &settings, &provider, &Default::default()).map_err(|e| EngineError::Other(format!("{CMD}: {e}")))?;
    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(report.bytes);
    Ok(json!({
        "path": path,
        "format": kind,
        "fromSeconds": (from.seconds() * 1000.0).round() / 1000.0,
        "toSeconds": (to.seconds() * 1000.0).round() / 1000.0,
        "fps": fps,
        "width": w,
        "height": h,
        "frames": report.frames,
        "bytes": bytes,
        "note": "a reduced preview for checking motion (transitions, zooms, speed changes); export with `file.exportMedia` for the real thing",
    }))
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: CMD,
        label: "Motion Preview",
        menu: &[],
        shortcut: None,
        params: r#"{"path":str (.gif: animated image; .mp4: H.264 with sound),"fromSeconds":f64? (default the playhead),"toSeconds":f64? (default 3 s later; at most 10 s after fromSeconds),"fps":1..30? (default 12 for GIF, the sequence's rate up to 30 for MP4),"width":px=480 (64..640)}"#,
        enabled: has_seq,
        run: preview,
        journal: false,
        named: NamedKeys::NONE,
    }]
}

#[cfg(test)]
mod tests;
