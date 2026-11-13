//! Smart render (#18): re-exporting after a small change re-encodes only what changed.
//!
//! With [`ExportSettings::smart_render`], Filmagic's H.264 encoder codes the export in independent
//! segments of one keyframe interval: every segment is a fresh encoder (an IDR picture first, its own
//! rate control, no reference into another segment), so its packets depend on nothing but the
//! encoder settings and the pictures it encodes. Its cache key is a SHA-256 over
//!
//! * the encoder configuration (`EncoderConfig`'s full debug form: profile, preset, rate control,
//!   keyframe distance, slices, colour description …),
//! * the bytes this encoder writes for a fixed probe clip with that configuration, so any change in
//!   the encoder itself (a new build that codes differently) gives new keys instead of old packets,
//! * the number of pictures, and the SHA-256 of every 8-bit Y'CbCr picture handed to the encoder.
//!
//! The key is made from the encoder's *input*, not from the project, so there is nothing to
//! invalidate: a change to anything that shows (a caption, a grade, a source file replaced on disk,
//! a font, an export effect, the frame size) changes those pictures and so the key, and a change
//! that does not show cannot change the stream. The price is that every frame is still rendered;
//! what a hit saves is the encoding, the larger part of an H.264 export's time.
//!
//! A segment's packets come out of [`VideoEncoder::encode`] when the *next* segment is complete (and
//! the last two at [`VideoEncoder::flush`]), whether they were encoded or found in the cache, so the
//! container is written the same way and the file is byte-identical with a cold or a warm cache
//! (`smart_tests`). The segment being encoded runs on a thread of its own while the next one is
//! rendered.
//!
//! Cache files (`<key>.seg` in [`ExportSettings::smart_render_dir`]) are hostile input like any
//! file: a magic, the key, bounded packet counts and sizes, and a SHA-256 of the whole file are
//! checked, and anything that does not match is a miss (the file is replaced). Writes go to a
//! temporary file that is renamed into place, so a crash or a concurrent export never leaves half a
//! segment. When the directory grows over [`MAX_CACHE_BYTES`], the oldest segments are removed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use filmagic_h264enc::{Encoder, EncoderConfig, YuvFrame};
use sha2::{Digest as _, Sha256};

use crate::encoders::h264_input;
use crate::format::ColorSignal;
use crate::registry::{EncodedPacket, EncoderFrame, PrepareAhead, VideoEncoder};
use crate::stats::{Stage, timed};
use crate::{ExportError, FrameRate, Result, SampleEntry};

/// A SHA-256 digest.
pub(crate) type Digest = [u8; 32];

/// File magic and version of a cached segment.
const MAGIC: &[u8; 8] = b"FMSEG\x00\x01\x00";

/// Most bytes of buffered pictures in one segment (8-bit 4:2:0): a segment is one keyframe interval,
/// shortened so that this many bytes hold it (4K: 16 frames). Two segments are alive at a time.
pub(crate) const SEGMENT_BUDGET: u64 = 192 << 20;

/// The cache directory is trimmed to this many bytes after an export, oldest segments first.
pub const MAX_CACHE_BYTES: u64 = 4 << 30;

/// Largest cached segment file read back (anything bigger is not one of ours).
const MAX_SEGMENT_FILE: u64 = 1 << 30;

/// Frames per segment: the keyframe distance, at most what [`SEGMENT_BUDGET`] holds, at least one.
pub(crate) fn segment_frames(keyint: u32, w: u32, h: u32) -> usize {
    let picture = u64::from(w).saturating_mul(u64::from(h)).saturating_add(2 * u64::from(w.div_ceil(2)) * u64::from(h.div_ceil(2)));
    let fit = SEGMENT_BUDGET / picture.max(1);
    usize::try_from(u64::from(keyint.max(1)).min(fit).max(1)).unwrap_or(1)
}

/// The SHA-256 of one 8-bit 4:2:0 picture (each plane length-prefixed, so planes cannot run into
/// one another).
pub(crate) fn picture_digest(y: &[u8], u: &[u8], v: &[u8]) -> Digest {
    let mut h = Sha256::new();
    for plane in [y, u, v] {
        h.update((plane.len() as u64).to_le_bytes());
        h.update(plane);
    }
    h.finalize().into()
}

/// The part of every key that stands for the encoder: its configuration and what it writes for a
/// small fixed clip with that configuration (another encoder build that codes differently gives
/// other keys). `Err` when the encoder refuses the configuration.
fn fingerprint(cfg: &EncoderConfig) -> std::result::Result<Digest, String> {
    let mut probe = cfg.clone();
    probe.width = 64;
    probe.height = 48;
    probe.slices = 1;
    probe.threads = 1;
    let mut enc = Encoder::new(probe).map_err(|e| e.to_string())?;
    let mut h = Sha256::new();
    h.update(MAGIC);
    h.update(format!("{cfg:?}").as_bytes());
    let (w, ht) = (64usize, 48usize);
    for t in 0..3usize {
        let y: Vec<u8> = (0..w * ht).map(|i| ((i % w) * 3 + (i / w) * 5 + t * 17) as u8).collect();
        let u: Vec<u8> = (0..w * ht / 4).map(|i| (128 + (i % 7) * 3 + t) as u8).collect();
        let v: Vec<u8> = (0..w * ht / 4).map(|i| (120 + (i % 5) * 4 + t * 2) as u8).collect();
        let frame = YuvFrame { y: &y, u: &u, v: &v, y_stride: w, uv_stride: w / 2 };
        for p in enc.try_encode(&frame, t as i64).map_err(|e| e.to_string())? {
            h.update(&p.data);
        }
    }
    for p in enc.flush() {
        h.update(&p.data);
    }
    Ok(h.finalize().into())
}

/// The cache key of a segment of pictures with these digests.
fn segment_key(fingerprint: &Digest, pictures: &[Digest]) -> Digest {
    let mut h = Sha256::new();
    h.update(fingerprint);
    h.update((pictures.len() as u64).to_le_bytes());
    for d in pictures {
        h.update(d);
    }
    h.finalize().into()
}

fn hex(d: &Digest) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// The cache file of `key`.
fn segment_path(dir: &Path, key: &Digest) -> PathBuf {
    dir.join(format!("{}.seg", hex(key)))
}

/// A segment's packets as a cache file.
pub(crate) fn encode_segment_file(key: &Digest, packets: &[EncodedPacket]) -> Vec<u8> {
    let mut out = Vec::with_capacity(packets.iter().map(|p| p.data.len() + 13).sum::<usize>() + 80);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(key);
    out.extend_from_slice(&(packets.len() as u32).to_le_bytes());
    for p in packets {
        out.push(u8::from(p.key));
        out.extend_from_slice(&p.composition_offset.to_le_bytes());
        out.extend_from_slice(&p.duration.to_le_bytes());
        out.extend_from_slice(&u32::try_from(p.data.len()).unwrap_or(u32::MAX).to_le_bytes());
    }
    for p in packets {
        out.extend_from_slice(&p.data);
    }
    let sum: Digest = Sha256::digest(&out).into();
    out.extend_from_slice(&sum);
    out
}

/// The packets of a cache file for `key` holding exactly `count` pictures, or `None` for anything
/// else: another key, a count that does not match, sizes that do not add up, a damaged file.
pub(crate) fn decode_segment_file(bytes: &[u8], key: &Digest, count: usize) -> Option<Vec<EncodedPacket>> {
    let body_len = bytes.len().checked_sub(32)?;
    let (body, sum) = bytes.split_at(body_len);
    if Sha256::digest(body).as_slice() != sum {
        return None;
    }
    let mut at = 0usize;
    let mut take = |n: usize| -> Option<&[u8]> {
        let s = body.get(at..at.checked_add(n)?)?;
        at += n;
        Some(s)
    };
    if take(8)? != MAGIC || take(32)? != key {
        return None;
    }
    let n = u32::from_le_bytes(take(4)?.try_into().ok()?) as usize;
    if n != count {
        return None;
    }
    let mut heads = Vec::with_capacity(n.min(1 << 16));
    for _ in 0..n {
        let flags = *take(1)?.first()?;
        let offset = i32::from_le_bytes(take(4)?.try_into().ok()?);
        let duration = u32::from_le_bytes(take(4)?.try_into().ok()?);
        let len = u32::from_le_bytes(take(4)?.try_into().ok()?) as usize;
        if flags > 1 {
            return None;
        }
        heads.push((flags == 1, offset, duration, len));
    }
    let mut packets = Vec::with_capacity(heads.len());
    for (key, composition_offset, duration, len) in heads {
        packets.push(EncodedPacket { data: take(len)?.to_vec(), key, duration, composition_offset });
    }
    (at == body.len()).then_some(packets)
}

/// The cached packets of `key`, if the cache has a good copy.
fn load(dir: &Path, key: &Digest, count: usize) -> Option<Vec<EncodedPacket>> {
    let path = segment_path(dir, key);
    let len = std::fs::metadata(&path).ok()?.len();
    if len > MAX_SEGMENT_FILE {
        return None;
    }
    let bytes = std::fs::read(&path).ok()?;
    let packets = decode_segment_file(&bytes, key, count);
    if packets.is_none() {
        log::info!("smart render: {} is not a usable segment; encoding it again", path.display());
    }
    packets
}

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Keep `packets` under `key` (best effort: a cache that cannot be written only costs speed).
fn store(dir: &Path, key: &Digest, packets: &[EncodedPacket]) {
    let write = || -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = dir.join(format!(".{}-{}-{n}.tmp", hex(key), std::process::id()));
        std::fs::write(&tmp, encode_segment_file(key, packets))?;
        // Windows refuses to rename over a file another export has just put there: that file holds the same packets
        if let Err(e) = std::fs::rename(&tmp, segment_path(dir, key)) {
            let _ = std::fs::remove_file(&tmp);
            if !segment_path(dir, key).exists() {
                return Err(e);
            }
        }
        Ok(())
    };
    if let Err(e) = write() {
        log::info!("smart render: segment not cached in {}: {e}", dir.display());
    }
}

/// Remove the oldest segments until the directory holds at most `limit` bytes of them.
pub(crate) fn prune(dir: &Path, limit: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "seg"))
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((m.modified().unwrap_or(std::time::UNIX_EPOCH), m.len(), e.path()))
        })
        .collect();
    let mut total: u64 = files.iter().map(|f| f.1).fold(0, u64::saturating_add);
    if total <= limit {
        return;
    }
    files.sort();
    for (_, len, path) in files {
        if total <= limit {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(len);
        }
    }
}

/// The default cache directory.
pub fn default_dir() -> PathBuf {
    std::env::temp_dir().join("filmagic-smart-render")
}

/// A converted picture: its planes and digest.
struct Picture {
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    digest: Digest,
}

/// What a segment came to.
struct SegmentOut {
    packets: Vec<EncodedPacket>,
    reused: bool,
}

/// Encode (or find in the cache) one segment: fresh encoder, pictures numbered from 0.
fn run_segment(cfg: EncoderConfig, fingerprint: Digest, dir: Option<PathBuf>, pictures: Vec<Picture>, den: i64) -> Result<SegmentOut> {
    let digests: Vec<Digest> = pictures.iter().map(|p| p.digest).collect();
    let key = segment_key(&fingerprint, &digests);
    if let Some(packets) = dir.as_deref().and_then(|d| load(d, &key, pictures.len())) {
        return Ok(SegmentOut { packets, reused: true });
    }
    let (w, cw) = (cfg.width as usize, (cfg.width as usize).div_ceil(2));
    let mut enc = Encoder::new(cfg).map_err(|e| ExportError::Encode(e.to_string()))?;
    let mut coded = Vec::with_capacity(pictures.len());
    for (i, p) in pictures.iter().enumerate() {
        let frame = YuvFrame { y: &p.y, u: &p.u, v: &p.v, y_stride: w, uv_stride: cw };
        coded.extend(timed(Stage::Encode, || enc.try_encode(&frame, i as i64 * den)).map_err(|e| ExportError::Encode(e.to_string()))?);
    }
    coded.extend(enc.flush());
    drop(pictures);
    let packets: Vec<EncodedPacket> = coded
        .into_iter()
        .map(|p| EncodedPacket {
            data: p.data,
            key: p.keyframe,
            duration: u32::try_from(den).unwrap_or(u32::MAX),
            composition_offset: i32::try_from(p.pts.saturating_sub(p.dts)).unwrap_or(0),
        })
        .collect();
    if packets.len() != digests.len() {
        return Err(ExportError::Encode(format!("internal: a smart-render segment of {} pictures gave {} packets", digests.len(), packets.len())));
    }
    if let Some(d) = dir.as_deref() {
        store(d, &key, &packets);
    }
    Ok(SegmentOut { packets, reused: false })
}

/// A segment encoding on its own thread (or, without threads, already done).
enum InFlight {
    Thread(std::thread::JoinHandle<Result<SegmentOut>>),
    Done(Result<SegmentOut>),
}

impl InFlight {
    fn start(cfg: EncoderConfig, fingerprint: Digest, dir: Option<PathBuf>, pictures: Vec<Picture>, den: i64) -> Result<Self> {
        if cfg!(target_arch = "wasm32") {
            return Ok(InFlight::Done(run_segment(cfg, fingerprint, dir, pictures, den)));
        }
        std::thread::Builder::new()
            .name("filmagic-smart-render".into())
            .spawn(move || run_segment(cfg, fingerprint, dir, pictures, den))
            .map(InFlight::Thread)
            .map_err(|e| ExportError::Encode(format!("cannot start the smart-render encoder thread: {e}")))
    }

    fn finish(self) -> Result<SegmentOut> {
        match self {
            InFlight::Done(r) => r,
            InFlight::Thread(t) => {
                t.join().map_err(|_| ExportError::Encode("internal error while encoding a smart-render segment (the panic was logged)".into()))?
            }
        }
    }
}

/// Filmagic's H.264 encoder in smart-render segments (see the module docs).
pub(crate) struct SmartH264 {
    cfg: EncoderConfig,
    /// Never encodes: the parameter sets and the B-frame delay, the same for every segment.
    proto: Encoder,
    fingerprint: Digest,
    w: u32,
    h: u32,
    rate: FrameRate,
    signal: ColorSignal,
    segment: usize,
    current: Vec<Picture>,
    in_flight: Option<InFlight>,
    dir: Option<PathBuf>,
    /// Pictures the render threads converted (and hashed) ahead, by index.
    ready: Arc<Mutex<HashMap<u64, Picture>>>,
    reused: u64,
    encoded: u64,
}

impl SmartH264 {
    pub(crate) fn new(cfg: EncoderConfig, w: u32, h: u32, rate: FrameRate, signal: ColorSignal, dir: Option<PathBuf>) -> Result<Self> {
        let fingerprint = fingerprint(&cfg).map_err(ExportError::Encode)?;
        let proto = Encoder::new(cfg.clone()).map_err(|e| ExportError::Encode(e.to_string()))?;
        let segment = segment_frames(cfg.keyint, w, h);
        Ok(Self {
            cfg,
            proto,
            fingerprint,
            w,
            h,
            rate,
            signal,
            segment,
            current: Vec::new(),
            in_flight: None,
            dir,
            ready: Arc::default(),
            reused: 0,
            encoded: 0,
        })
    }

    /// The finished segment before the one just started, if there was one.
    fn rotate(&mut self) -> Result<Vec<EncodedPacket>> {
        let pictures = std::mem::take(&mut self.current);
        let next = InFlight::start(self.cfg.clone(), self.fingerprint, self.dir.clone(), pictures, self.rate.den)?;
        match self.in_flight.replace(next) {
            Some(prev) => self.collect(prev),
            None => Ok(Vec::new()),
        }
    }

    fn collect(&mut self, seg: InFlight) -> Result<Vec<EncodedPacket>> {
        let out = seg.finish()?;
        if out.reused {
            self.reused += 1;
        } else {
            self.encoded += 1;
        }
        Ok(out.packets)
    }
}

impl VideoEncoder for SmartH264 {
    fn name(&self) -> &str {
        "Filmagic H.264 (software, smart render)"
    }
    fn sample_entry(&self) -> SampleEntry {
        let cfg = filmagic_isobmff::AvcConfig::parse(&self.proto.avcc()).unwrap_or_else(|_| {
            let (sps, pps) = self.proto.sps_pps();
            filmagic_isobmff::AvcConfig::new(vec![sps], vec![pps], 4)
        });
        let mut e = SampleEntry::avc(cfg, self.w as u16, self.h as u16);
        self.signal.apply_to(&mut e, false);
        e
    }
    fn timescale(&self) -> u32 {
        self.rate.num as u32
    }
    fn encode(&mut self, f: &EncoderFrame) -> Result<Vec<EncodedPacket>> {
        let (w, h) = (f.width as usize, f.height as usize);
        let chroma = w.div_ceil(2) * h.div_ceil(2);
        let ahead = {
            let mut ready = self.ready.lock().unwrap_or_else(PoisonError::into_inner);
            let mine = ready.remove(&f.index);
            ready.retain(|&i, _| i > f.index);
            mine
        };
        let picture = match ahead.filter(|p| p.y.len() == w * h && p.u.len() == chroma && p.v.len() == chroma) {
            Some(p) => p,
            None => {
                let (mut y, mut u, mut v) = (Vec::new(), Vec::new(), Vec::new());
                let (proto, signal) = (&self.proto, self.signal);
                timed(Stage::Convert, || proto.install(|| h264_input(f, signal, &mut y, &mut u, &mut v)));
                let digest = picture_digest(&y, &u, &v);
                Picture { y, u, v, digest }
            }
        };
        if picture.y.len() != w * h || (self.w, self.h) != (f.width, f.height) {
            return Err(ExportError::Encode(format!("smart render: a {}x{} picture for a {}x{} export", f.width, f.height, self.w, self.h)));
        }
        self.current.push(picture);
        if self.current.len() >= self.segment { self.rotate() } else { Ok(Vec::new()) }
    }
    fn flush(&mut self) -> Result<Vec<EncodedPacket>> {
        let mut out = if self.current.is_empty() { Vec::new() } else { self.rotate()? };
        if let Some(last) = self.in_flight.take() {
            out.extend(self.collect(last)?);
        }
        if let Some(dir) = &self.dir {
            prune(dir, MAX_CACHE_BYTES);
        }
        Ok(out)
    }
    fn media_start(&self) -> Option<i64> {
        (self.proto.delay() > 0).then_some(self.rate.den)
    }
    fn prepare_ahead(&self) -> Option<PrepareAhead> {
        let (ready, signal) = (self.ready.clone(), self.signal);
        Some(Arc::new(move |f: &EncoderFrame| {
            let (mut y, mut u, mut v) = (Vec::new(), Vec::new(), Vec::new());
            timed(Stage::Convert, || h264_input(f, signal, &mut y, &mut u, &mut v));
            let digest = picture_digest(&y, &u, &v);
            ready.lock().unwrap_or_else(PoisonError::into_inner).insert(f.index, Picture { y, u, v, digest });
        }))
    }
    fn smart_render_stats(&self) -> Option<(u64, u64)> {
        Some((self.reused, self.encoded))
    }
}
