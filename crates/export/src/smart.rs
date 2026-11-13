//! Smart render (#18): re-exporting after a small change re-encodes only what changed.
//!
//! With [`ExportSettings::smart_render`], Filmagic's H.264 encoder codes the export in independent
//! segments of one keyframe interval (shorter where [`SEGMENT_BUDGET`] says so): every segment is a
//! fresh encoder (an IDR picture first, its own rate control, no reference into another segment), so
//! its packets depend on nothing but the encoder settings, the segment's parity (below) and the
//! pictures it encodes. Its cache key is a SHA-256 over
//!
//! * the encoder: [`ENCODER_REVISION`], the crate version, the configuration's full debug form
//!   (profile, preset, rate control, keyframe distance, slices, colour description …), the
//!   real-size parameter sets, and the bytes this encoder writes for a fixed probe clip with that
//!   configuration;
//! * the segment's parity: neighbouring segments number their IDR pictures from 0 and from 2, so two
//!   consecutive IDR pictures never share an `idr_pic_id` across a join (H.264 §7.4.3);
//! * the number of pictures, and the SHA-256 of every 8-bit Y'CbCr picture handed to the encoder.
//!
//! The key is made from the encoder's *input*, not from the project, so there is nothing to
//! invalidate: a change to anything that shows (a caption, a grade, a source file replaced on disk,
//! a font, an export effect, the frame size) changes those pictures and so the key, and a change
//! that does not show cannot change the stream. The price is that every frame is still rendered;
//! what a hit saves is the encoding, the larger part of an H.264 export's time.
//!
//! **A smart export is not the stream a normal export writes**: each segment starts its rate
//! control (and VBV) again, so sizes and quality per segment differ, and CBR / VBR hold their rate
//! per segment, not across the file. It is byte-identical to a smart export of the same pictures
//! with an empty cache. A ripple or trim moves every later picture to another segment, so
//! everything after the change is encoded again.
//!
//! Timing: a segment's decoding times are worked out here, not taken from each fresh encoder (whose
//! B-frame delay is unknown until it has seen two pictures): the k-th packet of a segment decodes at
//! `k` frames minus the stream's reordering delay, the same as in one continuous stream, so the
//! composition offsets agree with the track's edit list whatever the segment's length.
//!
//! A segment's packets come out of [`VideoEncoder::encode`] when the *next* segment is complete (and
//! the last two at [`VideoEncoder::flush`]), whether they were encoded or found in the cache, so the
//! container is written the same way and the file is byte-identical with a cold or a warm cache
//! (`smart_tests`). The segment being encoded runs on a thread of its own while the next one is
//! rendered; video therefore reaches the container up to two segments after its audio, which a
//! fast-start MP4 plays without trouble but interleaves less tightly than a normal export.
//!
//! Cache files (`<64 hex digits>.seg`) are hostile input like any file: a magic, the key, bounded
//! packet counts and sizes, the frame duration, a keyframe first and a SHA-256 of the whole file are
//! checked, and anything that does not match is a miss (the file is replaced). Writes go to a
//! temporary file that is renamed into place (and removed on any error), so a crash or a concurrent
//! export never leaves half a segment. The default directory is per user ([`default_dir`]); on Unix a
//! directory owned by another user, or writable by others, is not used. A hit refreshes the file's
//! time, and when the directory grows over [`MAX_CACHE_BYTES`] the least recently used segments are
//! removed: only files that are named and start like ours.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use filmagic_h264enc::{ENCODER_REVISION, Encoder, EncoderConfig, YuvFrame};
use sha2::{Digest as _, Sha256};

use crate::encoders::h264_input;
use crate::format::ColorSignal;
use crate::registry::{EncodedPacket, EncoderFrame, PrepareAhead, VideoEncoder};
use crate::stats::{Stage, timed};
use crate::{ExportError, FrameRate, Result, SampleEntry};

/// A SHA-256 digest.
pub(crate) type Digest = [u8; 32];

/// File magic and version of a cached segment.
const MAGIC: &[u8; 8] = b"FMSEG\x00\x02\x00";

/// Most bytes of buffered pictures in one segment (8-bit 4:2:0): a segment is one keyframe interval,
/// shortened so that this many bytes hold it (4K: 16 frames). Two segments are alive at a time.
pub(crate) const SEGMENT_BUDGET: u64 = 192 << 20;

/// The cache directory is trimmed to this many bytes after an export, least recently used first.
pub const MAX_CACHE_BYTES: u64 = 4 << 30;

/// Largest cached segment file read back (anything bigger is not one of ours).
const MAX_SEGMENT_FILE: u64 = 1 << 30;

/// Temporary files older than this are left over from an export that stopped: prune removes them.
const STALE_TMP: std::time::Duration = std::time::Duration::from_secs(3600);

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

/// The part of every key that stands for the encoder: [`ENCODER_REVISION`], the crate version, the
/// configuration, the parameter sets of `proto` (the real-size encoder) and what the encoder writes
/// for a small fixed clip with that configuration. `Err` when the encoder refuses the configuration.
fn fingerprint(cfg: &EncoderConfig, proto: &Encoder) -> std::result::Result<Digest, String> {
    let mut probe = cfg.clone();
    probe.width = 64;
    probe.height = 48;
    probe.slices = 1;
    probe.threads = 1;
    let mut enc = Encoder::new(probe).map_err(|e| e.to_string())?;
    let mut h = Sha256::new();
    h.update(MAGIC);
    h.update(ENCODER_REVISION.to_le_bytes());
    h.update(env!("CARGO_PKG_VERSION").as_bytes());
    h.update(format!("{cfg:?}").as_bytes());
    h.update(proto.avcc());
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

/// The `idr_pic_id` base of segment `index`: 0 and 2 alternate, so neighbours never share an id.
fn idr_base(index: u64) -> u32 {
    if index.is_multiple_of(2) { 0 } else { 2 }
}

/// The cache key of a segment of pictures with these digests.
fn segment_key(fingerprint: &Digest, idr_base: u32, pictures: &[Digest]) -> Digest {
    let mut h = Sha256::new();
    h.update(fingerprint);
    h.update(idr_base.to_le_bytes());
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

/// Whether `name` is a segment file name: 64 lowercase hex digits and `.seg`.
fn is_segment_name(name: &str) -> bool {
    name.strip_suffix(".seg").is_some_and(|stem| stem.len() == 64 && stem.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
}

/// Whether `name` is one of our temporary files: `.<64 hex digits>-<pid>-<n>.tmp`.
fn is_temp_name(name: &str) -> bool {
    name.strip_prefix('.').and_then(|n| n.strip_suffix(".tmp")).is_some_and(|n| n.len() > 65 && is_segment_name(&format!("{}.seg", &n[..64])))
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

/// The packets of a cache file for `key` holding exactly `count` pictures of `duration` each, a
/// keyframe first, or `None` for anything else: another key, a count or duration that does not
/// match, sizes that do not add up, a damaged file.
pub(crate) fn decode_segment_file(bytes: &[u8], key: &Digest, count: usize, duration: u32) -> Option<Vec<EncodedPacket>> {
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
    if n != count || n == 0 {
        return None;
    }
    let mut heads = Vec::with_capacity(n.min(1 << 16));
    for _ in 0..n {
        let flags = *take(1)?.first()?;
        let offset = i32::from_le_bytes(take(4)?.try_into().ok()?);
        let d = u32::from_le_bytes(take(4)?.try_into().ok()?);
        let len = u32::from_le_bytes(take(4)?.try_into().ok()?) as usize;
        if flags > 1 || d != duration {
            return None;
        }
        heads.push((flags == 1, offset, len));
    }
    if !heads.first()?.0 {
        return None;
    }
    let mut packets = Vec::with_capacity(heads.len());
    for (key, composition_offset, len) in heads {
        packets.push(EncodedPacket { data: take(len)?.to_vec(), key, duration, composition_offset });
    }
    (at == body.len()).then_some(packets)
}

/// The cached packets of `key`, if the cache has a good copy (a hit marks the file as just used).
fn load(dir: &Path, key: &Digest, count: usize, duration: u32) -> Option<Vec<EncodedPacket>> {
    let path = segment_path(dir, key);
    let file = std::fs::File::open(&path).ok()?;
    let mut bytes = Vec::new();
    // read at most one byte more than a segment may have, whatever the file grew to since
    file.take(MAX_SEGMENT_FILE + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MAX_SEGMENT_FILE {
        return None;
    }
    let packets = decode_segment_file(&bytes, key, count, duration);
    match &packets {
        None => log::info!("smart render: {} is not a usable segment; encoding it again", path.display()),
        Some(_) => {
            if let Ok(f) = std::fs::File::options().write(true).open(&path) {
                let _ = f.set_modified(std::time::SystemTime::now());
            }
        }
    }
    packets
}

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Keep `packets` under `key` (best effort: a cache that cannot be written only costs speed).
fn store(dir: &Path, key: &Digest, packets: &[EncodedPacket]) {
    let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".{}-{}-{n}.tmp", hex(key), std::process::id()));
    let write = || -> std::io::Result<()> {
        std::fs::write(&tmp, encode_segment_file(key, packets))?;
        // Windows refuses to rename over a file another export has just put there: that file holds the same packets
        if let Err(e) = std::fs::rename(&tmp, segment_path(dir, key))
            && !segment_path(dir, key).exists()
        {
            return Err(e);
        }
        Ok(())
    };
    let result = write();
    // whatever happened, no temporary file stays behind
    let _ = std::fs::remove_file(&tmp);
    if let Err(e) = result {
        log::info!("smart render: segment not cached in {}: {e}", dir.display());
    }
}

/// Whether the file at `path` starts with our magic.
fn starts_like_a_segment(path: &Path) -> bool {
    let mut head = [0u8; 8];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut head)).is_ok() && &head == MAGIC
}

/// Remove the least recently used segments until the directory holds at most `limit` bytes of
/// them, and temporary files an export left behind. Only files named and starting like ours are
/// touched: the directory may be one an agent or a preset chose.
pub(crate) fn prune(dir: &Path, limit: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let now = std::time::SystemTime::now();
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = Vec::new();
    for e in entries.filter_map(|e| e.ok()) {
        let name = e.file_name().to_string_lossy().into_owned();
        let Ok(m) = e.metadata() else { continue };
        if !m.is_file() {
            continue;
        }
        let modified = m.modified().unwrap_or(std::time::UNIX_EPOCH);
        if is_temp_name(&name) {
            if now.duration_since(modified).is_ok_and(|age| age > STALE_TMP) {
                let _ = std::fs::remove_file(e.path());
            }
        } else if is_segment_name(&name) && starts_like_a_segment(&e.path()) {
            files.push((modified, m.len(), e.path()));
        }
    }
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

/// The default cache directory: the user's own cache directory (`%LOCALAPPDATA%` on Windows,
/// `~/Library/Caches` on macOS, `$XDG_CACHE_HOME` or `~/.cache` elsewhere), never a shared
/// temporary directory. `None` when the system names none.
pub fn default_dir() -> Option<PathBuf> {
    let var = |k: &str| std::env::var_os(k).map(PathBuf::from).filter(|p| p.is_absolute());
    if cfg!(target_os = "windows") {
        return var("LOCALAPPDATA").map(|d| d.join("Filmagic").join("smart-render"));
    }
    if cfg!(target_os = "macos") {
        return var("HOME").map(|d| d.join("Library").join("Caches").join("Filmagic").join("smart-render"));
    }
    var("XDG_CACHE_HOME").or_else(|| var("HOME").map(|h| h.join(".cache"))).map(|d| d.join("filmagic").join("smart-render"))
}

/// Make `dir` ready for the cache, or say why it cannot be used. On Unix it must belong to the user
/// running the export and must not be writable by others (another user could otherwise plant
/// segments); the default directory is created private (0700).
pub(crate) fn prepare_dir(dir: &Path, private: bool) -> std::result::Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        let mut b = std::fs::DirBuilder::new();
        b.recursive(true);
        if private {
            b.mode(0o700);
        }
        b.create(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let meta = std::fs::symlink_metadata(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
        if !meta.is_dir() {
            return Err(format!("{} is not a directory", dir.display()));
        }
        // who am I: the owner of a file this process creates there
        let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let probe = dir.join(format!(".owner-{}-{n}.probe", std::process::id()));
        std::fs::write(&probe, b"").map_err(|e| format!("cannot write to {}: {e}", dir.display()))?;
        let me = std::fs::metadata(&probe).map(|m| m.uid());
        let _ = std::fs::remove_file(&probe);
        let me = me.map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
        if meta.uid() != me {
            return Err(format!("{} belongs to another user", dir.display()));
        }
        let mode = meta.permissions().mode();
        if private && mode & 0o077 != 0 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| format!("cannot make {} private: {e}", dir.display()))?;
        } else if !private && mode & 0o022 != 0 {
            return Err(format!("{} is writable by other users", dir.display()));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = private;
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))
    }
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

/// One segment's work, moved to its thread.
struct SegmentJob {
    cfg: EncoderConfig,
    fingerprint: Digest,
    dir: Option<PathBuf>,
    pictures: Vec<Picture>,
    /// Frame duration in the encoder's timescale.
    den: i64,
    /// The stream's reordering delay in frames (1 with B-frames).
    delay: i64,
    index: u64,
    cancel: Arc<AtomicBool>,
}

/// Encode (or find in the cache) one segment: a fresh encoder, pictures numbered from 0, decoding
/// times as in one continuous stream (see the module docs).
fn run_segment(job: SegmentJob) -> Result<SegmentOut> {
    let SegmentJob { mut cfg, fingerprint, dir, pictures, den, delay, index, cancel } = job;
    let duration = u32::try_from(den).map_err(|_| ExportError::Encode("smart render: the frame duration does not fit the container".into()))?;
    cfg.idr_pic_id_base = idr_base(index);
    let digests: Vec<Digest> = pictures.iter().map(|p| p.digest).collect();
    let key = segment_key(&fingerprint, cfg.idr_pic_id_base, &digests);
    if let Some(packets) = dir.as_deref().and_then(|d| load(d, &key, pictures.len(), duration)) {
        return Ok(SegmentOut { packets, reused: true });
    }
    let (w, cw) = (cfg.width as usize, (cfg.width as usize).div_ceil(2));
    let mut enc = Encoder::new(cfg).map_err(|e| ExportError::Encode(e.to_string()))?;
    let mut coded = Vec::with_capacity(pictures.len());
    for (i, p) in pictures.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(ExportError::Cancelled);
        }
        let frame = YuvFrame { y: &p.y, u: &p.u, v: &p.v, y_stride: w, uv_stride: cw };
        coded.extend(enc.try_encode(&frame, i as i64 * den).map_err(|e| ExportError::Encode(e.to_string()))?);
    }
    coded.extend(enc.flush());
    drop(pictures);
    if coded.len() != digests.len() {
        return Err(ExportError::Encode(format!("internal: a smart-render segment of {} pictures gave {} packets", digests.len(), coded.len())));
    }
    // the k-th packet (decoding order) decodes at k frames minus the reordering delay
    let mut packets = Vec::with_capacity(coded.len());
    for (k, p) in coded.into_iter().enumerate() {
        let dts = (k as i64 - delay).saturating_mul(den);
        let offset = p.pts.saturating_sub(dts);
        let composition_offset = i32::try_from(offset)
            .ok()
            .filter(|o| *o >= 0)
            .ok_or_else(|| ExportError::Encode(format!("internal: smart-render picture {k} would present before it decodes ({offset})")))?;
        packets.push(EncodedPacket { data: p.data, key: p.keyframe, duration, composition_offset });
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(ExportError::Cancelled);
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
    fn start(job: SegmentJob) -> Result<Self> {
        if cfg!(target_arch = "wasm32") {
            return Ok(InFlight::Done(run_segment(job)));
        }
        std::thread::Builder::new()
            .name("filmagic-smart-render".into())
            .spawn(move || run_segment(job))
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
    /// Segments started so far (the next one's index).
    started: u64,
    dir: Option<PathBuf>,
    /// Set when the encoder is dropped (a cancelled or failed export): the segment thread stops.
    cancel: Arc<AtomicBool>,
    /// Pictures the render threads converted (and hashed) ahead, by index.
    ready: Arc<Mutex<HashMap<u64, Picture>>>,
    reused: u64,
    encoded: u64,
    notes: Vec<String>,
}

impl SmartH264 {
    /// `dir`: the cache directory, `Err` with the reason when there is none (the export is still
    /// coded in segments, so the file is the same, but nothing is reused); `notes` go into the result.
    pub(crate) fn new(
        cfg: EncoderConfig,
        w: u32,
        h: u32,
        rate: FrameRate,
        signal: ColorSignal,
        dir: std::result::Result<PathBuf, String>,
        mut notes: Vec<String>,
    ) -> Result<Self> {
        let proto = Encoder::new(cfg.clone()).map_err(|e| ExportError::Encode(e.to_string()))?;
        let fingerprint = fingerprint(&cfg, &proto).map_err(ExportError::Encode)?;
        let segment = segment_frames(cfg.keyint, w, h);
        let dir = match dir {
            Ok(d) => Some(d),
            Err(why) => {
                notes.push(format!("smartRender: no segment is cached or reused ({why}); set settings.smartRenderDir to a directory of your own"));
                None
            }
        };
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
            started: 0,
            dir,
            cancel: Arc::default(),
            ready: Arc::default(),
            reused: 0,
            encoded: 0,
            notes,
        })
    }

    /// The finished segment before the one just started, if there was one.
    fn rotate(&mut self) -> Result<Vec<EncodedPacket>> {
        let job = SegmentJob {
            cfg: self.cfg.clone(),
            fingerprint: self.fingerprint,
            dir: self.dir.clone(),
            pictures: std::mem::take(&mut self.current),
            den: self.rate.den,
            delay: i64::from(self.proto.delay() > 0),
            index: self.started,
            cancel: self.cancel.clone(),
        };
        self.started += 1;
        let next = InFlight::start(job)?;
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

impl Drop for SmartH264 {
    /// A segment still encoding when the export ends early (cancelled, failed) stops at its next
    /// picture and caches nothing.
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
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
    fn notes(&self) -> Vec<String> {
        self.notes.clone()
    }
}
