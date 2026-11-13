//! Make pack items from a plain folder of files: walk it (bounded), probe each file's duration and
//! loudness, look at a frame for transparency or a green screen, and name, tag and sort each file
//! from its own name and the names of the folders it is in (`memes/green screen/Bruh.mp4` →
//! kind `meme-video`, tags `memes`, `green`, `screen`, `bruh`, key colour of the screen).

use std::path::{Path, PathBuf};

use filmagic_media::{FrameRequest, MediaKind};
use filmagic_time::Tick;

use super::manifest::{self, Item, Kind, Manifest};
use super::store;
use crate::Session;

/// Most files one folder may hold for a pack.
pub const MAX_FILES: usize = 5_000;
/// Deepest folder walked.
const MAX_DEPTH: usize = 12;
/// Seconds of sound measured for loudness.
const LOUDNESS_SECONDS: i64 = 300;
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
    while let Some((dir, prefix, depth)) = stack.pop() {
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
pub fn items(s: &Session, root: &Path, previous: Option<&Manifest>) -> Result<(Vec<Item>, Vec<String>), String> {
    let (files, mut warnings) = walk(root, true)?;
    let mut out: Vec<Item> = Vec::new();
    let mut ids = std::collections::HashSet::new();
    for f in &files {
        let probe = match probe(s, &f.path) {
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

fn probe(s: &Session, path: &Path) -> Result<Probe, String> {
    let p = path.to_string_lossy().to_string();
    let src = s.media.probe_file(&p, &*s.services).map_err(|e| e.to_string())?;
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

/// Whether a frame (RGBA, `w`×`h`) has transparency of its own, and the colour of a green or blue
/// screen filling its border (at least 60 % of the outer band), if it has one.
pub fn look(w: usize, h: usize, rgba: &[u8]) -> (bool, Option<[u8; 3]>) {
    let px = w.saturating_mul(h);
    if px == 0 || rgba.len() < px.saturating_mul(4) {
        return (false, None);
    }
    let see_through = rgba.chunks_exact(4).filter(|p| p.get(3).is_some_and(|a| *a < 250)).count();
    let alpha = see_through * 200 >= px; // 0.5 % of the picture or more
    let band_x = (w / 12).max(1);
    let band_y = (h / 12).max(1);
    let (mut n, mut green, mut blue) = (0usize, [0u64; 4], [0u64; 4]);
    for y in 0..h {
        for x in 0..w {
            if x >= band_x && x + band_x < w && y >= band_y && y + band_y < h {
                continue;
            }
            n += 1;
            let i = (y * w + x) * 4;
            let Some(p) = rgba.get(i..i + 4) else { continue };
            let (r, g, b) = (u32::from(p[0]), u32::from(p[1]), u32::from(p[2]));
            let tally = |acc: &mut [u64; 4]| {
                acc[0] += 1;
                acc[1] += u64::from(r);
                acc[2] += u64::from(g);
                acc[3] += u64::from(b);
            };
            if g >= 90 && g * 10 >= r * 13 && g * 10 >= b * 13 {
                tally(&mut green);
            } else if b >= 90 && b * 10 >= r * 13 && b * 10 >= g * 12 {
                tally(&mut blue);
            }
        }
    }
    let pick = |acc: [u64; 4]| (n > 0 && acc[0] * 10 >= n as u64 * 6).then(|| [(acc[1] / acc[0]) as u8, (acc[2] / acc[0]) as u8, (acc[3] / acc[0]) as u8]);
    (alpha, pick(green).or_else(|| pick(blue)))
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
    "hai",
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
