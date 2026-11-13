//! The export audio path: the sequence mix at the output sample rate, folded to the output
//! channel count, with Export ▸ Effects ▸ Loudness Normalization (a BS.1770-4 measuring pass over
//! the export range, then bounded gain refinement through a look-ahead true-peak limiter) or,
//! without a loudness target, the True-Peak Limiter at −1 dBTP (on by default). The delivered
//! audio is metered as it is written ([`AudioReport`]): its true peak is always reported, with a
//! warning above −1 dBTP.
//!
//! The limiter's look-ahead delays its output; [`AudioOut`] mixes that many samples ahead (silence
//! past the end of the range) and drops the first ones, so the exported audio stays aligned with
//! the picture and exactly as long as the range.

use std::sync::Arc;

use filmagic_audio_dsp::AudioEffect;
use filmagic_audio_dsp::effects::Limiter;
use filmagic_project::{ItemId, Project, Sequence};
use filmagic_render::SourceProvider;
use filmagic_time::TimeRange;

use crate::{ExportError, ExportSettings, Result};

/// What the export's audio measured as it was written: its true peak and loudness after every
/// gain and limiter, which limiter ran, and warnings an agent can act on.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioReport {
    /// True peak (dBTP, BS.1770-4 4× oversampled) of the written audio; `None` when silent.
    pub true_peak_dbtp: Option<f64>,
    /// Integrated loudness (LUFS) of the written audio; `None` when silent.
    pub integrated_lufs: Option<f64>,
    /// `"loudness"` (Loudness Normalization's limiter), `"truePeak"` (the True-Peak Limiter) or `"off"`.
    pub limiter: &'static str,
    /// The ceiling the limiter held (dBTP), when one ran.
    pub ceiling_dbtp: Option<f64>,
    /// How far the mix went over the ceiling before the limiter (dB, ≥ 0, from its sample peak):
    /// about the deepest gain reduction the limiter applied.
    pub limited_db: f64,
    pub warnings: Vec<String>,
}

/// Peaks above this (dBTP) are reported: lossy encoders and players can clip them.
pub const PEAK_WARNING_DBTP: f64 = -1.0;

/// What loudness normalization measured and did.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize)]
pub struct LoudnessReport {
    pub measured_lufs: f64,
    /// Applied gain before the limiter, including bounded loudness correction.
    pub gain_db: f64,
}

pub(crate) struct AudioOut {
    project: Arc<Project>,
    /// The sequence with the output sample rate.
    seq: Sequence,
    pub sr: u32,
    pub channels: usize,
    gain: f32,
    limiter: Option<Limiter>,
    /// Which limiter `limiter` is (see [`AudioReport::limiter`]) and its ceiling.
    limiter_kind: &'static str,
    ceiling_dbtp: f64,
    /// Meters the delivered audio (true peak, loudness) for [`AudioOut::report`].
    meter: filmagic_audio_dsp::LoudnessMeter,
    /// Largest gain reduction of the limiter so far (dB, ≤ 0), from the input's sample peak over
    /// the ceiling.
    max_reduction_db: f32,
    latency: i64,
    /// Next sample to mix (runs `latency` ahead of `out`).
    next_in: i64,
    /// Next sample handed out.
    pub out: i64,
    /// End of the range (samples); input past it is silence.
    end: i64,
    pub loudness: Option<LoudnessReport>,
}

impl AudioOut {
    pub fn new(project: Arc<Project>, seq: ItemId, settings: &ExportSettings, range: TimeRange) -> Result<Self> {
        let q = project.sequence(seq).ok_or(ExportError::NoSequence)?;
        let r = settings.resolve(q.settings.width, q.settings.height, q.settings.frame_rate, q.settings.sample_rate);
        let mut seq = q.clone();
        seq.settings.sample_rate = r.sample_rate;
        let sr = r.sample_rate;
        let start = range.start.to_units_floor(sr as i64);
        let end = range.end().to_units_floor(sr as i64);
        let ln = &settings.effects.loudness;
        let pk = &settings.effects.peak_limiter;
        let ceiling = |v: f64| if v.is_finite() { v.clamp(-24.0, 0.0) } else { crate::PEAK_CEILING_DBTP };
        // Loudness Normalization brings its own limiter; otherwise the True-Peak Limiter (on by
        // default) keeps drafts from clipping. Below its ceiling the limiter is an exact delay,
        // which the look-ahead handling below removes, so quiet mixes are written bit for bit.
        let (limiter_kind, ceiling_dbtp) = if ln.enabled {
            ("loudness", ceiling(ln.true_peak_dbtp))
        } else if pk.enabled {
            ("truePeak", ceiling(pk.ceiling_dbtp))
        } else {
            ("off", 0.0)
        };
        let limiter = (limiter_kind != "off").then(|| {
            let mut l = Limiter::new(sr as f32, r.channels as usize);
            l.set_param("ceiling", ceiling_dbtp as f32);
            l.set_param("true_peak", 1.0);
            l.set_param("release", 80.0);
            l.reset();
            l
        });
        let latency = limiter.as_ref().map(|l| l.latency() as i64).unwrap_or(0);
        let meter = filmagic_audio_dsp::LoudnessMeter::new(sr as f64, r.channels as usize);
        Ok(AudioOut {
            project,
            seq,
            sr,
            channels: r.channels as usize,
            gain: 1.0,
            limiter,
            limiter_kind,
            ceiling_dbtp,
            meter,
            max_reduction_db: 0.0,
            latency,
            next_in: start,
            out: start,
            end,
            loudness: None,
        })
    }

    /// The loudness pass: measure the integrated loudness of the whole range and set the gain
    /// that brings it to the target. `cancel` is polled between chunks.
    pub fn measure(&mut self, settings: &ExportSettings, sources: &dyn SourceProvider, cancel: &dyn Fn() -> bool) -> Result<()> {
        let ln = &settings.effects.loudness;
        if !ln.enabled {
            return Ok(());
        }
        let mut meter = filmagic_audio_dsp::LoudnessMeter::new(self.sr as f64, self.channels);
        let mut pos = self.out;
        while pos < self.end {
            if cancel() {
                return Err(ExportError::Cancelled);
            }
            let n = (self.end - pos).min(self.sr as i64) as usize;
            let buf = self.mix(pos, n, sources);
            let refs: Vec<&[f32]> = buf.iter().map(Vec::as_slice).collect();
            meter.process(&refs);
            pos += n as i64;
        }
        let measured = meter.integrated();
        let gain_db = filmagic_audio_dsp::normalize_gain_db(measured, ln.target_lufs);
        // Limiting changes integrated loudness. Measure the actual limited signal and
        // refine its INPUT gain; the peak ceiling remains in force on every pass.
        // Four trials and at most 9 dB of extra drive bound both work and compression.
        let start = self.out;
        let mut trial_gain = gain_db;
        let mut best_gain = gain_db;
        let mut best_error = f64::INFINITY;
        let mut previous = None;
        if measured.is_finite() && ln.target_lufs.is_finite() {
            for _ in 0..4 {
                self.rewind(start, trial_gain);
                let mut limited = filmagic_audio_dsp::LoudnessMeter::new(self.sr as f64, self.channels);
                while self.out < self.end {
                    if cancel() {
                        self.rewind(start, best_gain);
                        return Err(ExportError::Cancelled);
                    }
                    let until = self.out.saturating_add(self.sr as i64).min(self.end);
                    if let Some(buf) = self.pull(until, sources) {
                        let refs: Vec<&[f32]> = buf.iter().map(Vec::as_slice).collect();
                        limited.process(&refs);
                    }
                }
                let achieved = limited.integrated();
                if !achieved.is_finite() {
                    break;
                }
                let error = ln.target_lufs - achieved;
                if error.abs() < best_error {
                    best_error = error.abs();
                    best_gain = trial_gain;
                }
                // Stop on convergence or a ceiling-limited plateau. An impossible target
                // must not cause unbounded passes or ever bypass the limiter.
                if error.abs() <= 0.1 || previous.is_some_and(|last: f64| (achieved - last).abs() < 0.02) {
                    break;
                }
                previous = Some(achieved);
                trial_gain += error.clamp(-3.0, 3.0);
            }
        }
        self.rewind(start, best_gain);
        self.loudness = Some(LoudnessReport { measured_lufs: measured, gain_db: best_gain });
        Ok(())
    }

    /// Restore the output cursor and limiter history between measuring passes and delivery.
    fn rewind(&mut self, start: i64, gain_db: f64) {
        self.out = start;
        self.next_in = start;
        self.gain = 10f64.powf(gain_db / 20.0) as f32;
        if let Some(limiter) = self.limiter.as_mut() {
            limiter.reset();
        }
        self.meter.reset();
        self.max_reduction_db = 0.0;
    }

    /// What the audio delivered so far measured (call it once the range is written).
    pub fn report(&self) -> AudioReport {
        let tp = self.meter.true_peak_dbtp();
        let il = self.meter.integrated();
        let finite = |v: f64| v.is_finite().then(|| (v * 100.0).round() / 100.0);
        let mut warnings = Vec::new();
        if tp.is_finite() && tp > PEAK_WARNING_DBTP + 0.05 {
            warnings.push(match self.limiter_kind {
                "off" => format!(
                    "the audio peaks at {tp:+.1} dBTP, above {PEAK_WARNING_DBTP} dBTP, and the True-Peak Limiter is off: it may clip. Export again with the limiter on (`truePeakLimiter: true`) or lower the mix (mix.check)"
                ),
                _ => format!("the audio peaks at {tp:+.1} dBTP, above {PEAK_WARNING_DBTP} dBTP (the limiter ceiling is {:.1} dBTP)", self.ceiling_dbtp),
            });
        }
        let limited = -(self.max_reduction_db as f64);
        if self.limiter_kind == "truePeak" && limited > 6.0 {
            warnings.push(format!(
                "the True-Peak Limiter reduced peaks by up to {limited:.1} dB: the mix is too hot and may sound squashed. Lower the loudest track (mix.check shows it) or set a loudness target (loudnessLufs)"
            ));
        }
        AudioReport {
            true_peak_dbtp: finite(tp),
            integrated_lufs: finite(il),
            limiter: self.limiter_kind,
            ceiling_dbtp: (self.limiter_kind != "off").then_some(self.ceiling_dbtp),
            limited_db: (limited.max(0.0) * 100.0).round() / 100.0,
            warnings,
        }
    }

    /// Mix `n` samples from `pos`, folded to the output channels (silence past the range end).
    /// Stereo output of a 5.1 Mix is the ITU-R BS.775 downmix; 5.1 output of a stereo Mix places it
    /// on L and R; 5.1 is in L, R, C, LFE, Ls, Rs order.
    fn mix(&self, pos: i64, n: usize, sources: &dyn SourceProvider) -> Vec<Vec<f32>> {
        let real = (self.end - pos).clamp(0, n as i64) as usize;
        let out_ch = if self.channels == 6 { 6 } else { 2 };
        let mut st = if real > 0 {
            let layout = filmagic_audio_dsp::channels::Layout::from_channels(out_ch);
            filmagic_render::audio::mix_sequence_layout(&self.project, &self.seq, pos, real, sources, layout, filmagic_audio_dsp::channels::Mixdown::FrontRear)
                .channels
        } else {
            vec![Vec::new(); out_ch]
        };
        st.resize(out_ch, Vec::new());
        for c in st.iter_mut() {
            c.resize(n, 0.0);
        }
        if self.channels == 1 { vec![st[0].iter().zip(&st[1]).map(|(l, r)| 0.5 * (l + r)).collect()] } else { st }
    }

    /// Output samples up to `until` (exclusive), planar; None when there is nothing new.
    pub fn pull(&mut self, until: i64, sources: &dyn SourceProvider) -> Option<Vec<Vec<f32>>> {
        let until = until.min(self.end);
        if until <= self.out {
            return None;
        }
        let want_in = until + self.latency;
        let n_in = (want_in - self.next_in).max(0) as usize;
        let mut buf = self.mix(self.next_in, n_in, sources);
        // the first call also fills the limiter's look-ahead
        let drop = (self.latency - (self.next_in - self.out)).clamp(0, n_in as i64) as usize;
        self.next_in = want_in;
        if self.gain != 1.0 {
            buf.iter_mut().for_each(|c| c.iter_mut().for_each(|s| *s *= self.gain));
        }
        if let Some(l) = self.limiter.as_mut() {
            // how far the input goes over the ceiling: at least what the limiter takes off
            let peak = buf.iter().flat_map(|c| c.iter()).fold(0.0f32, |m, x| if x.is_finite() { m.max(x.abs()) } else { m });
            if peak > 0.0 {
                let over = (self.ceiling_dbtp as f32) - 20.0 * peak.log10();
                self.max_reduction_db = self.max_reduction_db.min(over.min(0.0));
            }
            let mut refs: Vec<&mut [f32]> = buf.iter_mut().map(Vec::as_mut_slice).collect();
            l.process(&mut refs);
        }
        let n_out = (until - self.out) as usize;
        for c in buf.iter_mut() {
            c.drain(..drop.min(c.len()));
            c.resize(n_out, 0.0);
        }
        self.out = until;
        let refs: Vec<&[f32]> = buf.iter().map(Vec::as_slice).collect();
        self.meter.process(&refs);
        Some(buf)
    }

    /// Output samples still to come (per channel).
    pub fn remaining(&self) -> u64 {
        u64::try_from(self.end.saturating_sub(self.out)).unwrap_or(0)
    }

    /// Everything still to come (to the end of the range).
    pub fn rest(&mut self, sources: &dyn SourceProvider) -> Option<Vec<Vec<f32>>> {
        self.pull(self.end, sources)
    }
}

/// Interleave planar samples.
pub(crate) fn interleave(planar: &[Vec<f32>]) -> Vec<f32> {
    let n = planar.first().map_or(0, Vec::len);
    let mut out = Vec::with_capacity(n * planar.len());
    for i in 0..n {
        for c in planar {
            out.push(c.get(i).copied().unwrap_or(0.0));
        }
    }
    out
}

#[cfg(test)]
mod bounds_tests {
    #[test]
    fn ragged_audio_planes_keep_channel_positions_without_panicking() {
        assert_eq!(super::interleave(&[vec![1.0, 2.0], vec![3.0]]), vec![1.0, 3.0, 2.0, 0.0]);
        assert!(super::interleave(&[]).is_empty());
    }
}
