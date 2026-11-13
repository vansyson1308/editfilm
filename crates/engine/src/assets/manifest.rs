//! The pack manifest (`pack.json`) and the checks every manifest passes before Filmagic trusts it.
//!
//! A manifest comes from outside (a zip, a download, a folder someone else made), so every field is
//! hostile until [`validate`] has passed it: lengths are capped, numbers must be finite and in
//! range, item files must be relative paths that stay inside the pack on every operating system,
//! and remote items need an `https` URL with a SHA-256.

use serde::{Deserialize, Serialize};

/// The manifest's file name, at the root of a pack.
pub const MANIFEST: &str = "pack.json";
/// The manifest format this build writes and reads.
pub const FORMAT: u32 = 1;
/// Largest `pack.json` read (bytes).
pub const MAX_MANIFEST_BYTES: u64 = 8 << 20;
/// Most items in one pack.
pub const MAX_ITEMS: usize = 10_000;
/// Most tags (and moods) one item keeps.
pub const MAX_TAGS: usize = 32;
/// Largest single remote item a manifest may name (bytes).
pub const MAX_REMOTE_ITEM_BYTES: u64 = 1 << 30;

/// What an item is for. The kind decides where `assets.place` puts it and what it does to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// A music bed: Essential Sound type Music, ducked under dialogue.
    Music,
    /// A sound effect (whoosh, boom, click…): type SFX.
    Sfx,
    /// Room tone, crowd, weather: type Ambience, ducked under dialogue.
    Ambience,
    /// A still meme picture.
    MemeImage,
    /// An animated picture (GIF, APNG, animated WebP).
    MemeGif,
    /// A meme video clip, possibly green-screen or with sound.
    MemeVideo,
    /// A picture or video laid over the footage (frames, light leaks, transparent graphics).
    Overlay,
}

impl Kind {
    pub const ALL: [Kind; 7] = [Kind::Music, Kind::Sfx, Kind::Ambience, Kind::MemeImage, Kind::MemeGif, Kind::MemeVideo, Kind::Overlay];

    pub fn id(self) -> &'static str {
        match self {
            Kind::Music => "music",
            Kind::Sfx => "sfx",
            Kind::Ambience => "ambience",
            Kind::MemeImage => "meme-image",
            Kind::MemeGif => "meme-gif",
            Kind::MemeVideo => "meme-video",
            Kind::Overlay => "overlay",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        let s = s.trim().to_ascii_lowercase().replace('_', "-");
        Kind::ALL.into_iter().find(|k| k.id() == s)
    }

    /// Sound only (it goes on an audio track).
    pub fn is_audio(self) -> bool {
        matches!(self, Kind::Music | Kind::Sfx | Kind::Ambience)
    }

    /// The Essential Sound type its sound gets.
    pub fn audio_type(self) -> &'static str {
        match self {
            Kind::Music => "music",
            Kind::Ambience => "ambience",
            _ => "sfx",
        }
    }
}

/// One `pack.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    #[serde(default = "format_one")]
    pub format: u32,
    /// The pack's id, which is also its folder name under `Packs/`.
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// SPDX id (or licence name) that covers every item without a licence of its own.
    pub license: String,
    /// Where the pack comes from: a URL, or "own files".
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// The credit line a video using these items should carry (CC BY needs one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution: Option<String>,
    #[serde(default)]
    pub items: Vec<Item>,
}

fn format_one() -> u32 {
    FORMAT
}

/// One asset of a pack.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    /// Unique within the pack. An asset is named `pack/item` everywhere else.
    pub id: String,
    /// The file, relative to the pack folder, with `/` separators.
    pub file: String,
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mood: Vec<String>,
    /// Seconds (none for a still).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    /// Integrated loudness, LUFS (none without sound, or for silence).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loudness: Option<f64>,
    /// The picture carries transparency of its own.
    #[serde(default, skip_serializing_if = "is_false")]
    pub alpha: bool,
    /// The colour of a green (or blue) screen behind the subject, `#rrggbb`: `assets.place` keys it out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution: Option<String>,
    /// A remote item: downloaded on first use into `file` (verified against `sha256`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl Item {
    /// A bare item of `kind` for `file`.
    pub fn new(id: String, file: String, kind: Kind) -> Self {
        Item {
            id,
            file,
            kind,
            tags: Vec::new(),
            mood: Vec::new(),
            duration: None,
            loudness: None,
            alpha: false,
            key_color: None,
            license: None,
            source: None,
            attribution: None,
            url: None,
            sha256: None,
            size: None,
        }
    }
}

/// Parse a manifest from bytes (no validation).
pub fn parse(bytes: &[u8]) -> Result<Manifest, String> {
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(format!("{MANIFEST} is larger than {} MiB", MAX_MANIFEST_BYTES >> 20));
    }
    serde_json::from_slice(bytes).map_err(|e| format!("{MANIFEST} is not a valid pack manifest: {e}"))
}

/// Check and normalise a manifest. Returns warnings for fields that were dropped (an out-of-range
/// duration, a malformed key colour); a manifest that cannot be trusted at all is an error.
pub fn validate(m: &mut Manifest) -> Result<Vec<String>, String> {
    let mut warnings = Vec::new();
    if m.format == 0 || m.format > FORMAT {
        return Err(format!("{MANIFEST} has format {}; this Filmagic reads format {FORMAT}: update Filmagic", m.format));
    }
    check_id(&m.id).map_err(|e| format!("pack id: {e}"))?;
    m.name = text("name", &m.name, 200)?;
    if m.name.is_empty() {
        m.name = m.id.clone();
    }
    m.license = text("license", &m.license, 200)?;
    if m.license.is_empty() {
        return Err("the manifest names no `license`: every pack must say what licence covers its items".into());
    }
    m.source = text("source", &m.source, 2048)?;
    for (k, v) in [("version", &mut m.version), ("description", &mut m.description), ("author", &mut m.author), ("attribution", &mut m.attribution)] {
        opt_text(k, v, 4096)?;
    }
    if m.items.len() > MAX_ITEMS {
        return Err(format!("the pack lists {} items; at most {MAX_ITEMS} are allowed", m.items.len()));
    }
    let mut ids = std::collections::HashSet::new();
    let mut files = std::collections::HashSet::new();
    for it in &mut m.items {
        check_id(&it.id).map_err(|e| format!("item id {:?}: {e}", truncate(&it.id)))?;
        if !ids.insert(it.id.clone()) {
            return Err(format!("item id `{}` appears twice", it.id));
        }
        safe_rel_path(&it.file).map_err(|e| format!("item `{}`: file {:?}: {e}", it.id, truncate(&it.file)))?;
        if !files.insert(it.file.to_lowercase()) {
            return Err(format!("item `{}`: file `{}` is listed twice", it.id, it.file));
        }
        it.tags = clean_words(&it.tags);
        it.mood = clean_words(&it.mood);
        if let Some(d) = it.duration
            && !(d.is_finite() && (0.0..=1.0e6).contains(&d))
        {
            warnings.push(format!("item `{}`: duration {d} dropped (not a number of seconds)", it.id));
            it.duration = None;
        }
        if let Some(l) = it.loudness
            && !(l.is_finite() && (-200.0..=30.0).contains(&l))
        {
            warnings.push(format!("item `{}`: loudness {l} dropped (not LUFS)", it.id));
            it.loudness = None;
        }
        if let Some(c) = &it.key_color {
            match parse_hex_color(c) {
                Some(rgb) => it.key_color = Some(hex(rgb)),
                None => {
                    warnings.push(format!("item `{}`: keyColor {:?} dropped (use #rrggbb)", it.id, truncate(c)));
                    it.key_color = None;
                }
            }
        }
        for (k, v) in [("license", &mut it.license), ("source", &mut it.source), ("attribution", &mut it.attribution)] {
            opt_text(k, v, 4096)?;
        }
        match (&it.url, &mut it.sha256) {
            (Some(u), Some(sha)) => {
                check_url(u).map_err(|e| format!("item `{}`: {e}", it.id))?;
                *sha = check_sha256(sha).map_err(|e| format!("item `{}`: {e}", it.id))?;
                if it.size.is_some_and(|n| n > MAX_REMOTE_ITEM_BYTES) {
                    return Err(format!("item `{}`: size is over {} MiB", it.id, MAX_REMOTE_ITEM_BYTES >> 20));
                }
            }
            (Some(_), None) => return Err(format!("item `{}` has a `url` without its `sha256`: remote items must be pinned", it.id)),
            (None, Some(sha)) => *sha = check_sha256(sha).map_err(|e| format!("item `{}`: {e}", it.id))?,
            (None, None) => {}
        }
    }
    Ok(warnings)
}

/// A pack or item id: 1–128 of `a-z A-Z 0-9 . _ -`, not starting with `.` or `-`.
pub fn check_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 128 {
        return Err("must be 1 to 128 characters".into());
    }
    if id.starts_with('.') || id.starts_with('-') {
        return Err("must not start with `.` or `-`".into());
    }
    if !id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')) {
        return Err("may hold only letters a–z, digits, `.`, `_` and `-`".into());
    }
    if is_reserved_windows_name(id) {
        return Err("is a reserved file name on Windows".into());
    }
    Ok(())
}

/// A relative path inside a pack that is safe on Windows, macOS and Linux: `/` separators, no
/// `..`, `.`, empty or absolute parts, no drive letters or alternate data streams, no characters or
/// names Windows refuses. Zip entries and manifest files both pass through this, so nothing can be
/// written or read outside the pack (zip slip).
pub fn safe_rel_path(p: &str) -> Result<(), String> {
    if p.is_empty() || p.len() > 1024 {
        return Err("must be 1 to 1024 bytes".into());
    }
    if p.contains('\\') || p.contains(':') {
        return Err("use `/` between folders; `\\` and `:` are not allowed".into());
    }
    if p.starts_with('/') {
        return Err("must be relative to the pack folder".into());
    }
    let parts: Vec<&str> = p.split('/').collect();
    if parts.len() > 16 {
        return Err("is nested more than 16 folders deep".into());
    }
    for part in parts {
        if part.is_empty() || part == "." || part == ".." {
            return Err("has an empty, `.` or `..` part".into());
        }
        if part.len() > 255 {
            return Err("has a name longer than 255 bytes".into());
        }
        if part.chars().any(|c| c.is_control() || matches!(c, '<' | '>' | '"' | '|' | '?' | '*')) {
            return Err("has a character Windows does not allow in file names".into());
        }
        if part.ends_with('.') || part.ends_with(' ') {
            return Err("has a name ending in `.` or a space".into());
        }
        if is_reserved_windows_name(part) {
            return Err(format!("`{part}` is a reserved file name on Windows"));
        }
    }
    Ok(())
}

fn is_reserved_windows_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || ((stem.starts_with("COM") || stem.starts_with("LPT")) && stem.len() == 4 && stem.as_bytes().get(3).is_some_and(u8::is_ascii_digit))
}

/// `https://…`, or `http://` to this machine only (a local catalogue). A SHA-256 always pins the
/// content, so the URL decides only who can see what is fetched.
pub fn check_url(u: &str) -> Result<(), String> {
    if u.len() > 2048 || u.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("the url must be at most 2048 characters without spaces".into());
    }
    let lower = u.to_ascii_lowercase();
    if lower.starts_with("https://") && u.len() > "https://".len() {
        return Ok(());
    }
    if let Some(rest) = lower.strip_prefix("http://") {
        let host = rest.split(['/', '?', '#']).next().unwrap_or("");
        let host = host.rsplit_once('@').map(|(_, h)| h).unwrap_or(host);
        let name = if host.starts_with('[') {
            host.split(']').next().map(|h| format!("{h}]")).unwrap_or_default()
        } else {
            host.split(':').next().unwrap_or("").to_string()
        };
        if matches!(name.as_str(), "localhost" | "127.0.0.1" | "[::1]") {
            return Ok(());
        }
        return Err("the url must use https:// (plain http:// is allowed only to localhost)".into());
    }
    Err("the url must start with https://".into())
}

/// A SHA-256 as 64 hex digits, lower-cased.
pub fn check_sha256(s: &str) -> Result<String, String> {
    let t = s.trim().to_ascii_lowercase();
    if t.len() == 64 && t.bytes().all(|b| b.is_ascii_hexdigit()) { Ok(t) } else { Err(format!("sha256 {:?} must be 64 hex digits", truncate(s))) }
}

fn text(field: &str, v: &str, max: usize) -> Result<String, String> {
    let t = v.trim();
    if t.len() > max {
        return Err(format!("`{field}` is longer than {max} bytes"));
    }
    if t.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
        return Err(format!("`{field}` holds control characters"));
    }
    Ok(t.to_string())
}

fn opt_text(field: &str, v: &mut Option<String>, max: usize) -> Result<(), String> {
    if let Some(s) = v.as_ref() {
        let t = text(field, s, max)?;
        *v = if t.is_empty() { None } else { Some(t) };
    }
    Ok(())
}

/// At most 60 characters of a hostile string, for error messages.
pub fn truncate(s: &str) -> String {
    s.chars().take(60).collect()
}

/// Tags or moods: folded to lower case without accents, single words or short phrases, deduplicated, capped.
fn clean_words(v: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in v {
        let w: String = fold(w.trim()).chars().filter(|c| !c.is_control()).take(64).collect();
        if !w.is_empty() && !out.contains(&w) {
            out.push(w);
        }
        if out.len() >= MAX_TAGS {
            break;
        }
    }
    out
}

/// `#rrggbb` (or `rrggbb`) as bytes.
pub fn parse_hex_color(s: &str) -> Option<[u8; 3]> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| h.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok());
    Some([byte(0)?, byte(2)?, byte(4)?])
}

pub fn hex(rgb: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
}

/// Lower case without accents (`Nhạc Vui` → `nhac vui`, `Đám đông` → `dam dong`), so a search
/// typed without diacritics finds names written with them. Latin letters only; other scripts are
/// lower-cased as they are.
pub fn fold(s: &str) -> String {
    s.chars().flat_map(char::to_lowercase).map(fold_char).collect()
}

fn fold_char(c: char) -> char {
    const GROUPS: [(char, &str); 7] = [
        ('a', "àáâãäåāăąạảấầẩẫậắằẳẵặǎ"),
        ('e', "èéêëēĕėęěẹẻẽếềểễệ"),
        ('i', "ìíîïĩīĭįıỉịǐ"),
        ('o', "òóôõöøōŏőơọỏốồổỗộớờởỡợǒ"),
        ('u', "ùúûüũūŭůűųưụủứừửữựǔ"),
        ('y', "ýÿŷỳỵỷỹ"),
        ('d', "đď"),
    ];
    if c.is_ascii() {
        return c;
    }
    for (base, set) in GROUPS {
        if set.contains(c) {
            return base;
        }
    }
    match c {
        'ç' | 'ć' | 'č' => 'c',
        'ñ' | 'ń' | 'ň' => 'n',
        'ś' | 'š' | 'ş' => 's',
        'ź' | 'ż' | 'ž' => 'z',
        'ł' => 'l',
        'ř' => 'r',
        'ť' => 't',
        'ğ' => 'g',
        _ => c,
    }
}

/// Words of a name: split at anything not a letter or digit and at lower→upper case changes
/// (`bigBoom_02` → `big`, `boom`, `02`), folded ([`fold`]).
pub fn words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in s.chars() {
        if !c.is_alphanumeric() {
            if !cur.is_empty() {
                out.push(fold(&std::mem::take(&mut cur)));
            }
            prev_lower = false;
            continue;
        }
        if c.is_uppercase() && prev_lower && !cur.is_empty() {
            out.push(fold(&std::mem::take(&mut cur)));
        }
        prev_lower = c.is_lowercase();
        cur.push(c);
    }
    if !cur.is_empty() {
        out.push(fold(&cur));
    }
    out
}

/// An id made from a name: folded words joined by `-` (`Nhạc nền Vui!` → `nhac-nen-vui`), at most
/// 64 characters, `fallback` when nothing is left.
pub fn slug(s: &str, fallback: &str) -> String {
    let w: Vec<String> = words(s).into_iter().map(|w| w.chars().filter(char::is_ascii_alphanumeric).collect::<String>()).filter(|w| !w.is_empty()).collect();
    let mut out = String::new();
    for x in w {
        if out.len() + x.len() + 1 > 64 {
            break;
        }
        if !out.is_empty() {
            out.push('-');
        }
        out.push_str(&x);
    }
    if out.is_empty() || check_id(&out).is_err() { fallback.to_string() } else { out }
}
