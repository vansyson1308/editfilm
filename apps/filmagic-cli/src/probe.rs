//! `filmagic-cli probe`: media info as JSON, plus container details for MPEG TS / PS (program,
//! streams with their codecs, access units, picture types and timestamp ranges), MXF (operational
//! pattern, tracks, index, timecode), Ogg (logical streams, pre-skip, granules), Broadcast WAV
//! (time reference) and image sequences (frames, missing numbers).

use std::sync::Arc;

use serde_json::{Value, json};

fn rate(r: filmagic_mxf::Rational) -> String {
    format!("{}/{}", r.num, r.den)
}

fn mxf_details(src: &filmagic_codecs::MxfSource) -> Value {
    let f = src.file();
    let tracks: Vec<Value> = f
        .tracks
        .iter()
        .map(|t| {
            let mut v = json!({
                "trackId": t.track_id,
                "trackNumber": format!("{:08x}", t.track_number),
                "kind": format!("{:?}", t.kind),
                "codec": t.codec.name(),
                "wrapping": format!("{:?}", t.wrapping),
                "editRate": rate(t.edit_rate),
                "origin": t.origin,
                "startPosition": t.start_position,
                "duration": t.duration,
            });
            if t.kind == filmagic_mxf::TrackKind::Picture {
                v["pictures"] = json!(t.samples.len());
                v["keyFrames"] = json!(t.samples.iter().filter(|s| s.key).count());
                v["indexed"] = json!(t.indexed);
                v["temporalOffsets"] = json!(t.temporal_offsets);
                v["reorderedFromBitstream"] = json!(t.needs_reorder);
            }
            if let Some(s) = &t.sound {
                v["sampleRate"] = json!(rate(s.sample_rate));
                v["channels"] = json!(s.channels);
                v["bits"] = json!(s.bits);
                v["sampleFrames"] = json!(t.stored_sample_frames());
            }
            v
        })
        .collect();
    json!({
        "operationalPattern": f.operational_pattern.name(),
        "timecode": f.timecode.map(|t| json!({"start": t.start, "roundedBase": t.rounded_base, "dropFrame": t.drop_frame, "text": t.format()})),
        "materialPackage": f.material_package_name,
        "product": f.product_name,
        "company": f.company_name,
        "partitions": f.partitions.len(),
        "indexSegments": f.index_segments.len(),
        "runIn": f.run_in,
        "tracks": tracks,
        "warnings": f.warnings,
    })
}

fn ogg_details(src: &filmagic_codecs::OggSource) -> Value {
    let f = src.file();
    let streams: Vec<Value> = f
        .streams
        .iter()
        .map(|s| {
            let mut v = json!({"serial": s.serial, "codec": s.codec.name(), "packets": s.packets.len(), "pages": s.pages, "lastGranule": s.last_granule, "endOfStream": s.saw_eos});
            if s.codec == filmagic_ogg::Codec::Opus
                && let Some(h) = s.headers.first()
                && h.len() >= 12
            {
                let pre_skip = u16::from_le_bytes([h[10], h[11]]);
                let t = filmagic_ogg::OpusTiming::of(s, pre_skip as u32);
                v["preSkip"] = json!(pre_skip);
                v["samples48k"] = json!(t.total);
            }
            v
        })
        .collect();
    json!({"streams": streams, "warnings": f.warnings})
}

fn mpeg_details(src: &filmagic_codecs::MpegSource) -> Value {
    let Some(f) = src.file() else {
        return json!({"format": "MPEG video elementary stream"});
    };
    let (vi, ai) = src.stream_indices();
    let streams: Vec<Value> = f
        .streams
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let id = match s.id {
                filmagic_mpegts::StreamId::Pid(pid) => json!({"pid": pid}),
                filmagic_mpegts::StreamId::Ps { stream_id, sub_id } => {
                    json!({"streamId": format!("0x{stream_id:02x}"), "subStreamId": sub_id.map(|x| format!("0x{x:02x}"))})
                }
            };
            let mut v = json!({
                "id": id,
                "streamType": format!("0x{:02x}", s.stream_type),
                "codec": s.codec.name(),
                "kind": format!("{:?}", s.kind()),
                "units": s.units.len(),
                "keyUnits": s.units.iter().filter(|u| u.key).count(),
                "pesPackets": s.pes_packets,
                "ptsRange": s.pts_range().map(|(a, b)| json!([a, b])),
                "used": Some(i) == vi || Some(i) == ai,
            });
            if let Some(l) = &s.language {
                v["language"] = json!(l);
            }
            if let Some(l) = s.lpcm {
                v["lpcm"] = json!({"sampleRate": l.sample_rate, "channels": l.channels, "bits": l.bits});
            }
            if s.units.iter().any(|u| u.picture.is_some()) {
                let p = |t: u8| s.units.iter().filter(|u| u.picture.is_some_and(|p| p.coding_type == t)).count();
                v["pictures"] =
                    json!({"I": p(1), "P": p(2), "B": p(3), "fieldPairs": s.units.iter().filter(|u| u.picture.is_some_and(|p| p.structure != 3)).count()});
            }
            v
        })
        .collect();
    json!({
        "format": f.format.name(),
        "program": f.program,
        "pcrRange": f.pcr_range.map(|(a, b)| json!([a, b])),
        "streams": streams,
        "warnings": f.warnings,
    })
}

/// Probe `path`; with `image_sequence`, `path` is the first frame of a numbered still sequence.
/// Media info as JSON with the duration also in seconds and in frames (#13): `durationSeconds`,
/// `durationFrames` at the video stream's frame rate (absent without video), and `durationSamples`
/// on each audio stream, next to the `duration` in ticks.
pub fn probe(path: &str, image_sequence: bool) -> Result<Value, String> {
    let mut v = probe_info(path, image_sequence)?;
    add_duration_units(&mut v);
    Ok(v)
}

/// Adds the duration in seconds, video frames and audio samples next to `duration` (ticks).
pub(crate) fn add_duration_units(v: &mut Value) {
    let Some(ticks) = v.get("duration").and_then(Value::as_i64) else { return };
    let d = filmagic_time::Tick(ticks);
    v["durationSeconds"] = json!((d.seconds() * 1e6).round() / 1e6);
    let rate =
        v.get("video").and_then(|x| x.get("frame_rate")).and_then(|r| Some(filmagic_time::FrameRate::new(r.get("num")?.as_i64()?, r.get("den")?.as_i64()?)));
    if let Some(r) = rate.filter(|r| r.num > 0 && r.den > 0) {
        // whole frames the media holds (a partial last frame counts)
        v["durationFrames"] = json!(r.frame_at(filmagic_time::Tick(ticks.saturating_add(r.frame_duration().0).saturating_sub(1))));
    }
    if let Some(streams) = v.get_mut("audio_streams").and_then(Value::as_array_mut) {
        for a in streams {
            if let Some(sr) = a.get("sample_rate").and_then(Value::as_i64).filter(|r| *r > 0) {
                a["durationSamples"] = json!(d.to_units_floor(sr));
            }
        }
    }
}

fn probe_info(path: &str, image_sequence: bool) -> Result<Value, String> {
    if image_sequence {
        use filmagic_media::sequence::{ImageSequenceSource, Numbered, sequence_frames};
        let n = Numbered::parse(path).ok_or_else(|| format!("{path}: not a numbered still image"))?;
        let dir = if n.dir.is_empty() { ".".to_string() } else { n.dir.clone() };
        let names: Vec<String> =
            std::fs::read_dir(&dir).map_err(|e| format!("{dir}: {e}"))?.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        let frames = sequence_frames(&n, &names);
        let name = std::path::Path::new(path).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let loader: filmagic_media::sequence::FrameLoader = Arc::new(|p: &str| filmagic_media::reader::read_regular_file(std::path::Path::new(p)));
        // the timebase of a fresh install (Settings ▸ Media ▸ Indeterminate Media Timebase)
        let src = ImageSequenceSource::new(&name, frames, filmagic_time::FrameRate::FPS_29_97, loader).map_err(|e| format!("{path}: {e}"))?;
        let mut v = serde_json::to_value(filmagic_media::MediaSource::info(&src)).unwrap_or_default();
        v["imageSequence"] = json!({
            "first": src.frame_path(0),
            "firstNumber": n.number,
            "lastNumber": n.number + src.frame_count() as u64 - 1,
            "frames": src.frame_count(),
            "missing": src.missing_frames().iter().map(|i| n.number + *i as u64).collect::<Vec<_>>(),
        });
        return Ok(v);
    }
    let bytes: Arc<[u8]> = filmagic_media::reader::read_regular_file(std::path::Path::new(path)).map_err(|e| format!("{path}: {e}"))?.into();
    let name = std::path::Path::new(path).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string());
    let head = &bytes[..bytes.len().min(70_000)];
    if filmagic_mxf::sniff(head) {
        let src = filmagic_codecs::MxfSource::open(&name, bytes).map_err(|e| format!("{path}: {e}"))?;
        let mut v = serde_json::to_value(filmagic_media::MediaSource::info(&src)).unwrap_or_default();
        v["mxf"] = mxf_details(&src);
        return Ok(v);
    }
    if filmagic_codecs::mpeg::sniff(head) {
        let src = filmagic_codecs::MpegSource::open(&name, bytes).map_err(|e| format!("{path}: {e}"))?;
        let mut v = serde_json::to_value(filmagic_media::MediaSource::info(&src)).unwrap_or_default();
        v["mpeg"] = mpeg_details(&src);
        return Ok(v);
    }
    if filmagic_ogg::sniff(head)
        && let Ok(src) = filmagic_codecs::OggSource::open(&name, bytes.clone())
    {
        let mut v = serde_json::to_value(filmagic_media::MediaSource::info(&src)).unwrap_or_default();
        v["ogg"] = ogg_details(&src);
        return Ok(v);
    }
    let src = filmagic_codecs::open_bytes(&name, bytes.clone()).map_err(|e| format!("{path}: {e}"))?;
    let mut v = serde_json::to_value(src.info()).unwrap_or_default();
    if filmagic_media::wav::sniff(head)
        && let Ok(w) = filmagic_media::wav::WavSource::parse(&name, bytes)
        && let Some(tr) = w.time_reference()
    {
        v["bwf"] = json!({"timeReference": tr});
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    /// #13: the duration is also in seconds, video frames and audio samples, next to the ticks.
    #[test]
    fn duration_is_reported_in_seconds_frames_and_samples() {
        let ticks = filmagic_time::Tick::from_seconds_f64(907.9).0;
        let mut v = json!({
            "duration": ticks,
            "video": {"frame_rate": {"num": 30000, "den": 1001}},
            "audio_streams": [{"sample_rate": 48000}, {"sample_rate": 0}],
        });
        super::add_duration_units(&mut v);
        assert_eq!(v["duration"], ticks, "ticks unchanged");
        assert_eq!(v["durationSeconds"], 907.9);
        // 907.9 s at 29.97 fps is 27 209.79 frames: the partial last frame counts
        assert_eq!(v["durationFrames"], 27210);
        assert_eq!(v["audio_streams"][0]["durationSamples"], 43_579_200);
        assert!(v["audio_streams"][1].get("durationSamples").is_none(), "no rate, no samples");
        // audio only: no frames; a zero frame rate: no frames, no crash
        let mut a = json!({"duration": ticks, "video": null, "audio_streams": []});
        super::add_duration_units(&mut a);
        assert!(a.get("durationFrames").is_none() && a["durationSeconds"] == 907.9);
        let mut z = json!({"duration": ticks, "video": {"frame_rate": {"num": 0, "den": 0}}});
        super::add_duration_units(&mut z);
        assert!(z.get("durationFrames").is_none());
        // no duration: unchanged
        let mut n = json!({"name": "x"});
        super::add_duration_units(&mut n);
        assert_eq!(n, json!({"name": "x"}));
    }
}
