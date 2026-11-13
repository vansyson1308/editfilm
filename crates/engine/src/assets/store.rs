//! Installed packs on disk: `<data dir>/Packs/<id>/` with a `pack.json` each, one `index.json`
//! across them, and the staging and trash folders that make install and remove atomic.
//!
//! Install writes everything into `Packs/.staging/<n>/` first and renames the finished folder into
//! place: a pack is either all there or not there. Remove renames the pack into `Packs/.trash/`
//! before deleting it, so a pack never appears half deleted (a file Windows still holds open is
//! cleared on a later install or remove).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::manifest::{self, Item, MANIFEST, Manifest};

/// The packs folder under the data directory.
pub const PACKS_DIR: &str = "Packs";
/// The index across every installed pack.
pub const INDEX: &str = "index.json";
const STAGING: &str = ".staging";
const TRASH: &str = ".trash";

/// Most files a zip may hold.
pub const MAX_ARCHIVE_ENTRIES: usize = 20_000;
/// Most bytes a pack may unpack to.
pub const MAX_PACK_BYTES: u64 = 4 << 30;
/// Largest single file in a pack.
pub const MAX_FILE_BYTES: u64 = 2 << 30;
/// Largest download.
pub const MAX_DOWNLOAD_BYTES: u64 = 4 << 30;

/// One installed pack in the index.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackSummary {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub license: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution: Option<String>,
    pub items: usize,
    /// Bytes on disk.
    pub bytes: u64,
    /// Items listed with a URL that are not downloaded yet.
    pub remote: usize,
    /// Item count per kind.
    pub kinds: std::collections::BTreeMap<String, usize>,
}

/// One item in the index: the manifest item and the pack it is in.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexItem {
    pub pack: String,
    #[serde(flatten)]
    pub item: Item,
}

/// The index: every installed pack and item, and the packs that could not be read.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Index {
    pub format: u32,
    pub packs: Vec<PackSummary>,
    pub items: Vec<IndexItem>,
    /// Folders under `Packs/` whose manifest is missing or broken: `[folder, why]`.
    #[serde(default)]
    pub problems: Vec<(String, String)>,
}

impl Index {
    pub fn item(&self, pack: &str, item: &str) -> Option<&IndexItem> {
        self.items.iter().find(|i| i.pack == pack && i.item.id == item)
    }
    pub fn pack(&self, id: &str) -> Option<&PackSummary> {
        self.packs.iter().find(|p| p.id == id)
    }
}

/// Where packs live for this session.
#[derive(Clone, Debug, Default)]
pub struct PackStore {
    dir: Option<PathBuf>,
}

impl PackStore {
    /// Keep packs under `data_dir/Packs`.
    pub fn set_dir(&mut self, data_dir: &Path) {
        self.dir = Some(data_dir.join(PACKS_DIR));
    }

    /// The packs folder, or why there is none.
    pub fn root(&self) -> Result<&Path, String> {
        self.dir
            .as_deref()
            .ok_or_else(|| "asset packs need a data directory: start filmagic-cli with --data-dir <dir> (the desktop app uses its own)".to_string())
    }

    /// The index, rebuilt when it is missing, unreadable, or does not match the pack folders.
    pub fn index(&self) -> Result<Index, String> {
        let root = self.root()?;
        let dirs = pack_dirs(root);
        if let Ok(b) = read_capped(&root.join(INDEX), 64 << 20)
            && let Ok(ix) = serde_json::from_slice::<Index>(&b)
            && ix.format == manifest::FORMAT
        {
            let mut have: Vec<&str> = ix.packs.iter().map(|p| p.id.as_str()).chain(ix.problems.iter().map(|p| p.0.as_str())).collect();
            have.sort_unstable();
            let mut want: Vec<&str> = dirs.iter().map(String::as_str).collect();
            want.sort_unstable();
            if have == want {
                return Ok(ix);
            }
        }
        self.rebuild_index()
    }

    /// Read every pack's manifest and write `index.json` again.
    pub fn rebuild_index(&self) -> Result<Index, String> {
        let root = self.root()?;
        let mut ix = Index { format: manifest::FORMAT, ..Default::default() };
        for d in pack_dirs(root) {
            match read_pack(&root.join(&d)) {
                Ok(m) if m.id == d => {
                    let pdir = root.join(&d);
                    let mut kinds = std::collections::BTreeMap::new();
                    let mut remote = 0;
                    for it in &m.items {
                        *kinds.entry(it.kind.id().to_string()).or_insert(0) += 1;
                        if it.url.is_some() && !pdir.join(&it.file).is_file() {
                            remote += 1;
                        }
                    }
                    ix.packs.push(PackSummary {
                        id: m.id.clone(),
                        name: m.name.clone(),
                        version: m.version.clone(),
                        license: m.license.clone(),
                        source: m.source.clone(),
                        author: m.author.clone(),
                        attribution: m.attribution.clone(),
                        items: m.items.len(),
                        bytes: dir_bytes(&pdir),
                        remote,
                        kinds,
                    });
                    ix.items.extend(m.items.into_iter().map(|item| IndexItem { pack: d.clone(), item }));
                }
                Ok(m) => ix.problems.push((d.clone(), format!("its {MANIFEST} names the pack `{}`; the folder must have the pack's id", m.id))),
                Err(e) => ix.problems.push((d.clone(), e)),
            }
        }
        if std::fs::create_dir_all(root).is_ok() {
            let tmp = root.join(format!("{INDEX}.tmp"));
            if let Ok(b) = serde_json::to_vec_pretty(&ix)
                && std::fs::write(&tmp, b).is_ok()
                && std::fs::rename(&tmp, root.join(INDEX)).is_err()
            {
                let _ = std::fs::remove_file(&tmp);
            }
        }
        Ok(ix)
    }

    /// A new, empty staging folder (removed again when the guard drops).
    pub fn stage(&self) -> Result<Staging, String> {
        let root = self.root()?;
        sweep(root);
        let dir = root.join(STAGING).join(nonce());
        std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        Ok(Staging { dir })
    }

    /// Move a finished pack folder (`from`, inside staging, holding a valid `pack.json`) into
    /// `Packs/<id>`. With `replace`, an installed pack of that id is swapped out in the same step.
    pub fn commit(&self, from: &Path, id: &str, replace: bool) -> Result<Index, String> {
        manifest::check_id(id)?;
        let root = self.root()?;
        let dest = root.join(id);
        let mut old = None;
        if dest.exists() {
            if !replace {
                return Err(format!("the pack `{id}` is installed already: pass \"replace\":true to update it, or run assets.remove first"));
            }
            let t = trash_path(root)?;
            std::fs::rename(&dest, &t).map_err(|e| format!("cannot move the installed `{id}` aside (is a file of it open?): {e}"))?;
            old = Some(t);
        }
        if let Err(e) = std::fs::rename(from, &dest) {
            if let Some(t) = &old {
                let _ = std::fs::rename(t, &dest);
            }
            return Err(format!("cannot move the pack into {}: {e}", dest.display()));
        }
        if let Some(t) = old {
            let _ = std::fs::remove_dir_all(t);
        }
        self.rebuild_index()
    }

    /// Remove `Packs/<id>`: renamed into the trash first (atomic), then deleted.
    pub fn remove(&self, id: &str) -> Result<(Index, bool), String> {
        manifest::check_id(id).map_err(|e| format!("pack id: {e}"))?;
        let root = self.root()?;
        let dest = root.join(id);
        if !dest.is_dir() {
            let installed = pack_dirs(root);
            return Err(format!("no installed pack `{id}`; installed: {}", if installed.is_empty() { "none".to_string() } else { installed.join(", ") }));
        }
        let t = trash_path(root)?;
        std::fs::rename(&dest, &t).map_err(|e| format!("cannot remove `{id}` (is a file of it open in another program?): {e}"))?;
        let cleared = std::fs::remove_dir_all(&t).is_ok();
        Ok((self.rebuild_index()?, cleared))
    }

    /// The folder of an installed pack.
    pub fn pack_dir(&self, id: &str) -> Result<PathBuf, String> {
        manifest::check_id(id)?;
        Ok(self.root()?.join(id))
    }
}

/// A staging folder that is deleted when dropped, unless it was committed (renamed away).
pub struct Staging {
    pub dir: PathBuf,
}

impl Drop for Staging {
    fn drop(&mut self) {
        if self.dir.exists() {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

fn trash_path(root: &Path) -> Result<PathBuf, String> {
    let t = root.join(TRASH);
    std::fs::create_dir_all(&t).map_err(|e| format!("cannot create {}: {e}", t.display()))?;
    Ok(t.join(nonce()))
}

/// Delete what an interrupted install or remove left in staging and trash.
fn sweep(root: &Path) {
    for d in [STAGING, TRASH] {
        if let Ok(rd) = std::fs::read_dir(root.join(d)) {
            for e in rd.flatten() {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
}

/// A name no other install of this or another process uses at the same time.
fn nonce() -> String {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{}-{t}-{n}", std::process::id())
}

/// The pack folders under `root` (not the hidden staging, trash or index).
fn pack_dirs(root: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(root) else { return Vec::new() };
    let mut v: Vec<String> = rd
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| !n.starts_with('.'))
        .collect();
    v.sort();
    v
}

/// Read and validate `dir/pack.json`.
pub fn read_pack(dir: &Path) -> Result<Manifest, String> {
    let b = read_capped(&dir.join(MANIFEST), manifest::MAX_MANIFEST_BYTES).map_err(|e| format!("no readable {MANIFEST}: {e}"))?;
    let mut m = manifest::parse(&b)?;
    manifest::validate(&mut m)?;
    Ok(m)
}

/// Write `dir/pack.json`.
pub fn write_manifest(dir: &Path, m: &Manifest) -> Result<(), String> {
    let b = serde_json::to_vec_pretty(m).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(MANIFEST), b).map_err(|e| format!("cannot write {MANIFEST}: {e}"))
}

/// A whole file, refused when it is larger than `cap`.
pub fn read_capped(path: &Path, cap: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let f = std::fs::File::open(path)?;
    let mut out = Vec::new();
    f.take(cap.saturating_add(1)).read_to_end(&mut out)?;
    if out.len() as u64 > cap {
        return Err(std::io::Error::other(format!("larger than {cap} bytes")));
    }
    Ok(out)
}

/// Bytes under `dir` (bounded walk).
fn dir_bytes(dir: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![(dir.to_path_buf(), 0usize)];
    let mut seen = 0usize;
    while let Some((d, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            seen += 1;
            if seen > 200_000 {
                return total;
            }
            let Ok(md) = e.metadata() else { continue };
            if md.is_dir() && depth < 32 {
                stack.push((e.path(), depth + 1));
            } else if md.is_file() {
                total = total.saturating_add(md.len());
            }
        }
    }
    total
}

/// SHA-256 of a file, hex.
pub fn sha256_file(path: &Path) -> Result<String, String> {
    use sha2::Digest;
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut h = sha2::Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf).map_err(|e| format!("{}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        h.update(buf.get(..n).unwrap_or(&[]));
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// The files of a pack worth keeping from an archive or folder: media Filmagic opens, the manifest,
/// and licence / readme text. Shortcuts (`.url`), `desktop.ini`, scripts and executables stay out.
pub fn keep_file(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    if name.starts_with('.') || name.eq_ignore_ascii_case("desktop.ini") || name.eq_ignore_ascii_case("thumbs.db") {
        return false;
    }
    if rel == MANIFEST {
        return true;
    }
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    if matches!(ext.as_str(), "txt" | "md") {
        let lower = name.to_ascii_lowercase();
        return ["license", "licence", "readme", "credits", "attribution", "copying"].iter().any(|k| lower.contains(k));
    }
    filmagic_media::is_importable(Path::new(name))
}

/// Unpack a zip into `dest` with every entry name checked ([`manifest::safe_rel_path`]) and the
/// size and count caps enforced on the bytes really written, not only on what the archive claims.
/// Symlinks, encrypted entries and files [`keep_file`] rejects are skipped. Returns the skipped names.
#[cfg(not(target_arch = "wasm32"))]
pub fn extract_zip(archive: &Path, dest: &Path) -> Result<Vec<String>, String> {
    use std::io::{Read, Write};
    let f = std::fs::File::open(archive).map_err(|e| format!("{}: {e}", archive.display()))?;
    let mut z = zip::ZipArchive::new(std::io::BufReader::new(f)).map_err(|e| format!("not a readable zip archive: {e}"))?;
    if z.len() > MAX_ARCHIVE_ENTRIES {
        return Err(format!("the archive holds {} entries; at most {MAX_ARCHIVE_ENTRIES} are allowed", z.len()));
    }
    let mut total = 0u64;
    let mut skipped = Vec::new();
    let mut written = std::collections::HashSet::new();
    for i in 0..z.len() {
        let mut e = z.by_index(i).map_err(|e| format!("archive entry {i}: {e}"))?;
        let name = e.name().to_string();
        if e.is_dir() {
            continue;
        }
        if manifest::safe_rel_path(&name).is_err() || e.is_symlink() || e.encrypted() || !keep_file(&name) {
            skipped.push(manifest::truncate(&name));
            continue;
        }
        if !written.insert(name.to_lowercase()) {
            return Err(format!("the archive holds `{}` twice", manifest::truncate(&name)));
        }
        if e.size() > MAX_FILE_BYTES {
            return Err(format!("`{}` unpacks to more than {} MiB", manifest::truncate(&name), MAX_FILE_BYTES >> 20));
        }
        let out_path = dest.join(&name);
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let mut out = std::fs::File::create(&out_path).map_err(|e| format!("{}: {e}", out_path.display()))?;
        let budget = MAX_FILE_BYTES.min(MAX_PACK_BYTES.saturating_sub(total)).saturating_add(1);
        let mut limited = (&mut e).take(budget);
        let mut buf = vec![0u8; 1 << 16];
        let mut n_file = 0u64;
        loop {
            let n = limited.read(&mut buf).map_err(|e| format!("`{}`: {e}", manifest::truncate(&name)))?;
            if n == 0 {
                break;
            }
            n_file += n as u64;
            if n_file >= budget {
                return Err(format!("the archive unpacks to more than its limits ({} MiB a file, {} MiB a pack)", MAX_FILE_BYTES >> 20, MAX_PACK_BYTES >> 20));
            }
            out.write_all(buf.get(..n).unwrap_or(&[])).map_err(|e| format!("{}: {e}", out_path.display()))?;
        }
        total = total.saturating_add(n_file);
    }
    Ok(skipped)
}

#[cfg(target_arch = "wasm32")]
pub fn extract_zip(_archive: &Path, _dest: &Path) -> Result<Vec<String>, String> {
    Err("zip archives cannot be installed in the web build".into())
}

/// Download `url` into `dest`, keeping it only if its SHA-256 (and size, when given) match. The
/// download streams into `dest.part` and is capped at `max` bytes.
#[cfg(all(feature = "pack-download", not(target_arch = "wasm32")))]
pub fn download(url: &str, sha256: &str, size: Option<u64>, max: u64, dest: &Path) -> Result<u64, String> {
    use sha2::Digest;
    use std::io::{Read, Write};
    manifest::check_url(url)?;
    let want = manifest::check_sha256(sha256)?;
    let provider = std::sync::Arc::new(rustls_rustcrypto::provider());
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::Rustls)
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .unversioned_rustls_crypto_provider(provider)
                .build(),
        )
        .timeout_connect(Some(std::time::Duration::from_secs(30)))
        .user_agent("filmagic-asset-packs")
        .build()
        .into();
    let resp = agent.get(url).call().map_err(|e| format!("downloading {url}: {e}"))?;
    let mut body = resp.into_body();
    let cap = size.unwrap_or(max).min(max);
    let mut reader = body.as_reader().take(cap.saturating_add(1));
    let mut name = dest.as_os_str().to_owned();
    name.push(".part");
    let part = PathBuf::from(name);
    let mut out = std::io::BufWriter::new(std::fs::File::create(&part).map_err(|e| format!("{}: {e}", part.display()))?);
    let mut h = sha2::Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut got = 0u64;
    let fail = |part: &Path, msg: String| {
        let _ = std::fs::remove_file(part);
        Err(msg)
    };
    loop {
        let n = match reader.read(&mut buf) {
            Ok(n) => n,
            Err(e) => {
                drop(out);
                return fail(&part, format!("downloading {url}: {e}"));
            }
        };
        if n == 0 {
            break;
        }
        got += n as u64;
        if got > cap {
            drop(out);
            return fail(&part, format!("{url} is larger than {cap} bytes"));
        }
        let chunk = buf.get(..n).unwrap_or(&[]);
        h.update(chunk);
        if let Err(e) = out.write_all(chunk) {
            drop(out);
            return fail(&part, format!("{}: {e}", part.display()));
        }
    }
    if let Err(e) = out.flush() {
        drop(out);
        return fail(&part, format!("{}: {e}", part.display()));
    }
    drop(out);
    let sha: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if sha != want || size.is_some_and(|s| s != got) {
        return fail(
            &part,
            format!(
                "{url}: checksum mismatch (got {sha}, {got} bytes; expected {want}{}); nothing was installed",
                size.map(|s| format!(", {s} bytes")).unwrap_or_default()
            ),
        );
    }
    std::fs::rename(&part, dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    Ok(got)
}

#[cfg(not(all(feature = "pack-download", not(target_arch = "wasm32"))))]
pub fn download(url: &str, sha256: &str, _size: Option<u64>, _max: u64, _dest: &Path) -> Result<u64, String> {
    manifest::check_url(url)?;
    manifest::check_sha256(sha256)?;
    Err(format!(
        "this build cannot download (built without the `pack-download` feature): download {url} yourself, check its SHA-256, and run assets.install with \"path\" set to the file"
    ))
}
