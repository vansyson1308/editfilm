//! Make pack items from a plain folder of files: walk it (bounded), probe each file's duration and
//! loudness, look at a frame for transparency or a green screen, and name, tag and sort each file
//! from its own name and the names of the folders it is in (`memes/green screen/Bruh.mp4` →
//! kind `meme-video`, tags `memes`, `green`, `screen`, `bruh`, key colour of the screen).

use std::path::{Path, PathBuf};

use filmagic_media::{FrameRequest, MediaKind};
use filmagic_time::Tick;

use super::manifest::{self, Item, Kind, Manifest};
use super::store;
use crate::{MediaPool, Services};

/// Most files one folder may hold for a pack.
pub const MAX_FILES: usize = 5_000;
/// Deepest folder walked.
const MAX_DEPTH: usize = 12;
/// Seconds of sound measured for loudness (a minute tells how loud a file plays).
const LOUDNESS_SECONDS: i64 = 60;
/// Most folders walked (a deep tree of empty folders costs time without files to cap it).
const MAX_FOLDERS: usize = 2_000;
/// Sound at least this long, with nothing in its name to say otherwise, is music.
const MUSIC_SECONDS: f64 = 45.0;

/// One file found by [`walk`]: its path inside the folder (with `/`) and on disk.
pub struct Found {
    pub rel: String,
    pub path: PathBuf,
}

/// The files of `root` worth a pack ([`store::keep_file`]), in name order. Hidden files, symlinks
/// and names unsafe on another operating system are skipped (and listed); more than [`MAX_FILES`]
/// files, or more than the pack size limit, is an error.
pub fn walk(root: &Path, media_only: bool) -> Result<(Vec<Found>, Vec<String>), String> {
    let mut found = Vec::new();
    let mut skipped = Vec::new();
    let mut total = 0u64;
    let mut stack = vec![(root.to_path_buf(), String::new(), 0usize)];
    let mut folders = 0usize;
    while let Some((dir, prefix, depth)) = stack.pop() {
        folders += 1;
        if folders > MAX_FOLDERS {
            return Err(format!("the folder holds more than {MAX_FOLDERS} folders; split it into smaller packs"));
        }
        let rd = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let Some(name) = e.file_name().to_str().map(str::to_string) else {
                skipped.push(format!("{prefix}{} (name is not UTF-8)", e.file_name().to_string_lossy()));
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            let rel = format!("{prefix}{name}");
            let Ok(md) = std::fs::symlink_metadata(e.path()) else { continue };
            if md.file_type().is_symlink() {
                skipped.push(format!("{rel} (a link)"));
                continue;
            }
            if manifest::safe_rel_path(&rel).is_err() {
                skipped.push(format!("{rel} (a name another operating system cannot hold)"));
                continue;
            }
            if md.is_dir() {
                if depth + 1 >= MAX_DEPTH {
                    skipped.push(format!("{rel}/ (nested too deep)"));
                } else {
                    stack.push((e.path(), format!("{rel}/"), depth + 1));
                }
                continue;
            }
            let keep = if media_only { filmagic_media::is_importable(Path::new(&name)) } else { store::keep_file(&rel) };
            if !md.is_file() || !keep {
                continue;
            }
            if md.len() > store::MAX_FILE_BYTES {
                skipped.push(format!("{rel} (larger than {} MiB)", store::MAX_FILE_BYTES >> 20));
                continue;
            }
            total = total.saturating_add(md.len());
            if total > store::MAX_PACK_BYTES {
                return Err(format!("the folder holds more than {} MiB of media; split it into smaller packs", store::MAX_PACK_BYTES >> 20));
            }
            found.push(Found { rel, path: e.path() });
            if found.len() > MAX_FILES {
                return Err(format!("the folder holds more than {MAX_FILES} files; split it into smaller packs"));
            }
        }
    }
    found.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok((found, skipped))
}

/// Copy `files` from their folder into `dest`, keeping their relative paths.
pub fn copy_into(files: &[Found], dest: &Path) -> Result<(), String> {
    for f in files {
        let to = dest.join(&f.rel);
        if let Some(p) = to.parent() {
            std::fs::create_dir_all(p).map_err(|e| format!("{}: {e}", p.display()))?;
        }
        std::fs::copy(&f.path, &to).map_err(|e| format!("copying {}: {e}", f.rel))?;
    }
    Ok(())
}

/// What a probe found out about one file.
#[derive(Debug, Default)]
struct Probe {
    kind: Option<MediaKind>,
    video: bool,
    duration: Option<f64>,
    loudness: Option<f64>,
    alpha: bool,
    key_color: Option<[u8; 3]>,
}

/// The items for the media files under `root`. An item of `previous` (a manifest the user may
/// have edited) for the same file keeps its id, kind, tags, mood, key colour and credits; the
/// measured fields are measured again. Files that will not open are listed in the warnings.
pub fn items(media: &MediaPool, services: &dyn Services, root: &Path, previous: Option<&Manifest>) -> Result<(Vec<Item>, Vec<String>), String> {
    let (files, mut warnings) = walk(root, true)?;
    let mut out: Vec<Item> = Vec::new();
    let mut ids = std::collections::HashSet::new();
    for f in &files {
        let probe = match probe(media, services, &f.path) {
            Ok(p) => p,
            Err(e) => {
                warnings.push(format!("{} skipped: {e}", f.rel));
                continue;
            }
        };
        let old = previous.and_then(|m| m.items.iter().find(|i| i.file == f.rel));
        let words = path_words(&f.rel);
        let kind = old.map(|i| i.kind).unwrap_or_else(|| classify(&f.rel, &words, &probe));
        let base = old.map(|i| i.id.clone()).unwrap_or_else(|| manifest::slug(f.rel.rsplit_once('.').map(|(a, _)| a).unwrap_or(&f.rel), "item"));
        let mut id = base.clone();
        let mut n = 2;
        while !ids.insert(id.clone()) {
            id = format!("{}-{n}", base.chars().take(120).collect::<String>());
            n += 1;
        }
        let mut it = Item::new(id, f.rel.clone(), kind);
        it.tags = old.map(|i| i.tags.clone()).unwrap_or_else(|| tags(&words));
        it.mood = old.map(|i| i.mood.clone()).unwrap_or_else(|| words.iter().filter(|w| MOODS.contains(&w.as_str())).cloned().collect());
        it.duration = probe.duration;
        it.loudness = probe.loudness.map(|l| (l * 10.0).round() / 10.0);
        it.alpha = probe.alpha;
        it.key_color = old.and_then(|i| i.key_color.clone()).or(probe.key_color.map(manifest::hex));
        if let Some(o) = old {
            it.license = o.license.clone();
            it.source = o.source.clone();
            it.attribution = o.attribution.clone();
        }
        out.push(it);
    }
    Ok((out, warnings))
}

fn probe(media: &MediaPool, services: &dyn Services, path: &Path) -> Result<Probe, String> {
    let p = path.to_string_lossy().to_string();
    let src = media.probe_file(&p, services).map_err(|e| e.to_string())?;
    let info = src.info().clone();
    let mut out = Probe { kind: Some(info.kind), video: info.has_video(), ..Default::default() };
    let still = matches!(info.kind, MediaKind::Still);
    if !still && info.duration > Tick::ZERO {
        out.duration = Some((info.duration.seconds() * 1000.0).round() / 1000.0);
    }
    if let Some(a) = info.audio() {
        out.loudness = loudness(&*src, a.sample_rate, a.channels, info.duration);
    }
    if info.has_video() {
        // a frame a little way in: many clips open on black or a title
        let at = if still { Tick::ZERO } else { Tick::from_seconds_f64((info.duration.seconds() * 0.1).min(2.0)) };
        if let Ok(f) = src.video_frame(FrameRequest { time: at, scale: 0.25 }) {
            let (alpha, key) = look(f.width as usize, f.height as usize, &f.to_rgba8());
            out.alpha = alpha;
            out.key_color = if alpha { None } else { key };
        }
    }
    Ok(out)
}

/// Integrated loudness of the first [`LOUDNESS_SECONDS`] seconds, LUFS.
fn loudness(src: &dyn filmagic_media::MediaSource, rate: u32, channels: u32, duration: Tick) -> Option<f64> {
    let sr = rate.clamp(8_000, 192_000);
    let total = (duration.seconds().max(0.0).min(LOUDNESS_SECONDS as f64) * f64::from(sr)) as i64;
    let mut meter = filmagic_audio_dsp::LoudnessMeter::new(f64::from(sr), if channels <= 1 { 1 } else { 2 });
    let mut pos = 0i64;
    let mut any = false;
    while pos < total {
        let n = (total - pos).min(i64::from(sr)) as usize;
        let buf = src.audio(pos, n, sr).ok()?;
        match buf.channels.as_slice() {
            [] => return None,
            [m] => meter.process(&[m.as_slice()]),
            [l, r, ..] if channels > 1 => meter.process(&[l.as_slice(), r.as_slice()]),
            [m, ..] => meter.process(&[m.as_slice()]),
        }
        any = true;
        pos += n as i64;
    }
    if !any {
        return None;
    }
    Some(meter.integrated()).filter(|v| v.is_finite())
}

/// Most a screen's colour may vary along the border (per channel, standard deviation in 8-bit
/// steps). A lit green or blue screen is one flat colour; grass, foliage, sky and sea vary far
/// more (a noisy lawn measures 25 and up).
const SCREEN_MAX_STD: f64 = 14.0;
/// Least saturation of a screen colour ((max − min) / max of its channels).
const SCREEN_MIN_SATURATION: f64 = 0.45;

/// Whether a frame (RGBA, `w`×`h`) has transparency of its own, and the colour of a green or blue
/// screen behind it, if it has one: at least 60 % of the outer band of the picture is one flat,
/// saturated green or blue ([`SCREEN_MAX_STD`], [`SCREEN_MIN_SATURATION`]). Grass or sky at the
/// edges of an outdoor shot is not a screen, and is not keyed out.
pub fn look(w: usize, h: usize, rgba: &[u8]) -> (bool, Option<[u8; 3]>) {
    let px = w.saturating_mul(h);
    if px == 0 || rgba.len() < px.saturating_mul(4) {
        return (false, None);
    }
    let see_through = rgba.as_chunks::<4>().0.iter().filter(|p| p[3] < 250).count();
    let alpha = see_through * 200 >= px; // 0.5 % of the picture or more
    let band_x = (w / 12).max(1);
    let band_y = (h / 12).max(1);
    let mut n = 0usize;
    let (mut green, mut blue) = (Vec::new(), Vec::new());
    for y in 0..h {
        for x in 0..w {
            if x >= band_x && x + band_x < w && y >= band_y && y + band_y < h {
                continue;
            }
            n += 1;
            let i = (y * w + x) * 4;
            let Some(p) = rgba.get(i..i + 4) else { continue };
            let (r, g, b) = (u32::from(p[0]), u32::from(p[1]), u32::from(p[2]));
            if g >= 90 && g * 10 >= r * 13 && g * 10 >= b * 13 {
                green.push([p[0], p[1], p[2]]);
            } else if b >= 90 && b * 10 >= r * 13 && b * 10 >= g * 12 {
                blue.push([p[0], p[1], p[2]]);
            }
        }
    }
    let pick = |px: &[[u8; 3]]| -> Option<[u8; 3]> {
        if n == 0 || px.len() * 10 < n * 6 {
            return None;
        }
        let count = px.len() as f64;
        let mean: [f64; 3] = std::array::from_fn(|c| px.iter().map(|p| f64::from(p[c])).sum::<f64>() / count);
        let flat = (0..3).all(|c| (px.iter().map(|p| (f64::from(p[c]) - mean[c]).powi(2)).sum::<f64>() / count).sqrt() <= SCREEN_MAX_STD);
        let (hi, lo) = (mean.iter().copied().fold(0.0, f64::max), mean.iter().copied().fold(255.0, f64::min));
        let saturated = hi > 0.0 && (hi - lo) / hi >= SCREEN_MIN_SATURATION;
        (flat && saturated).then(|| mean.map(|v| v.round().clamp(0.0, 255.0) as u8))
    };
    (alpha, pick(&green).or_else(|| pick(&blue)))
}

/// The words of a file's path: its folders and its name without the extension.
fn path_words(rel: &str) -> Vec<String> {
    let stem = rel.rsplit_once('.').map(|(a, _)| a).unwrap_or(rel);
    manifest::words(stem)
}

const STOP: &[&str] = &[
    "the", "a", "an", "and", "of", "to", "in", "on", "for", "final", "copy", "new", "untitled", "file", "files", "img", "image", "images", "mov", "vid",
    "clip", "clips", "audio", "video", "videos", "my", "mp", "wav", "edit", "export", "version",
];

/// Moods recognised in names (English and a few Vietnamese words without accents).
pub const MOODS: &[&str] = &[
    "happy",
    "sad",
    "epic",
    "funny",
    "comedy",
    "tense",
    "tension",
    "calm",
    "chill",
    "relaxing",
    "dark",
    "upbeat",
    "energetic",
    "dramatic",
    "romantic",
    "scary",
    "horror",
    "suspense",
    "inspiring",
    "uplifting",
    "mysterious",
    "emotional",
    "angry",
    "cute",
    "hype",
    "peaceful",
    "triumphant",
    "sexy",
    "awkward",
    "vui",
    "buon",
];

/// Search tags from path words: no stop words, no bare numbers, no single letters.
fn tags(words: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in words {
        if w.chars().count() < 2 || w.chars().all(|c| c.is_ascii_digit()) || STOP.contains(&w.as_str()) || out.contains(w) {
            continue;
        }
        out.push(w.clone());
        if out.len() >= manifest::MAX_TAGS {
            break;
        }
    }
    out
}

/// The kind of a file, from words in its path first and what the probe found second.
fn classify(rel: &str, words: &[String], p: &Probe) -> Kind {
    let has = |set: &[&str]| words.iter().any(|w| set.contains(&w.as_str()));
    let ext = rel.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    if !p.video {
        if has(&["ambience", "ambiance", "ambient", "atmos", "atmosphere", "roomtone", "room", "background", "nen"]) && !has(&["music", "nhac"]) {
            return Kind::Ambience;
        }
        if has(&["music", "song", "songs", "bgm", "bed", "beds", "soundtrack", "score", "nhac", "loop"]) {
            return Kind::Music;
        }
        if has(&[
            "sfx",
            "fx",
            "sound",
            "sounds",
            "effect",
            "effects",
            "whoosh",
            "swoosh",
            "boom",
            "impact",
            "hit",
            "click",
            "ui",
            "transition",
            "meme",
            "memes",
        ]) {
            return Kind::Sfx;
        }
        return if p.duration.is_some_and(|d| d >= MUSIC_SECONDS) { Kind::Music } else { Kind::Sfx };
    }
    if has(&["overlay", "overlays", "lightleak", "leak", "leaks", "vignette", "border", "grain", "dust"]) {
        return Kind::Overlay;
    }
    match p.kind {
        Some(MediaKind::Still) => Kind::MemeImage,
        _ if matches!(ext.as_str(), "gif" | "apng" | "webp" | "png") => Kind::MemeGif,
        _ => Kind::MemeVideo,
    }
}
