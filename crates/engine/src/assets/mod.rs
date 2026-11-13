//! Asset packs (`assets.*`): music, sound effects, ambience, memes and overlays as data that is
//! installed next to Filmagic, never inside a project or this repository, and removed again when a
//! project no longer needs it.
//!
//! A pack is a folder `<data dir>/Packs/<id>/` holding its files and a `pack.json` manifest
//! ([`manifest`]); `Packs/index.json` lists every installed item ([`store`]). Packs come from a
//! folder, a zip, a URL pinned by SHA-256, or the default catalogue ([`catalog`]); a manifest item
//! with its own URL is downloaded only when it is first placed. `assets.buildPack` makes a pack
//! from a plain folder of one's own files ([`scan`]).
//!
//! | command | does |
//! |---|---|
//! | `assets.list` | installed packs, or the items of one pack |
//! | `assets.catalog` | the default catalogue and which of its packs are installed |
//! | `assets.install` | install from `path` (folder, zip or `pack.json`), `url` + `sha256`, or `catalog` |
//! | `assets.remove` | remove an installed pack |
//! | `assets.search` | items by words, kind, mood, length, transparency |
//! | `assets.buildPack` | a pack from a folder of files: tagged from names, probed, keyed |
//! | `assets.place` | import an item and put it on the timeline: free track, keyer, audio type, ducking |

pub mod catalog;
pub mod manifest;
mod scan;
pub mod store;

use std::path::{Path, PathBuf};

use filmagic_project::{ItemKind, MediaRef, TrackId};
use filmagic_time::Tick;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, NamedKeys, always, bad, has_seq, tick_or_time_p};
use crate::{EngineError, MediaPool, Result, Services, Session};
use manifest::{Kind, MANIFEST, Manifest};
pub use store::PackStore;

type Run = fn(&mut Session, &Value) -> Result<Value>;
type Enabled = fn(&Session) -> std::result::Result<(), String>;

fn spec(id: &'static str, label: &'static str, params: &'static str, enabled: Enabled, run: Run, journal: bool) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled, run, journal, named: NamedKeys::NONE }
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec("assets.list", "List Asset Packs", r#"{"pack":packId?}"#, always, list, false),
        spec("assets.catalog", "Asset Pack Catalogue", "{}", always, catalogue, false),
        spec(
            "assets.install",
            "Install Asset Pack",
            r#"{"path":"folder|file.zip|pack.json"?,"url":str?,"sha256":hex?,"size":bytes?,"catalog":entryId?,"id":packId?,"name":str?,"license":str?,"source":str?,"author":str?,"replace":bool?,"wait":bool?}"#,
            always,
            install,
            false,
        ),
        spec("assets.remove", "Remove Asset Pack", r#"{"pack":packId}"#, always, remove, false),
        spec("assets.fetch", "Download Assets", r#"{"asset":"pack/item"?,"pack":packId?,"wait":bool?}"#, always, fetch, false),
        spec(
            "assets.search",
            "Search Assets",
            r#"{"query":str?,"kind":"music|sfx|ambience|meme-image|meme-gif|meme-video|overlay"|[kind]?,"mood":str?,"pack":packId?,"alpha":bool?,"minSeconds":n?,"maxSeconds":n?,"limit":n?}"#,
            always,
            search,
            false,
        ),
        spec(
            "assets.buildPack",
            "Build Asset Pack",
            r#"{"from":folder,"id":packId?,"name":str?,"license":str?,"source":str?,"author":str?,"attribution":str?,"install":bool?,"replace":bool?}"#,
            always,
            build_pack,
            false,
        ),
        spec(
            "assets.place",
            "Place Asset",
            r##"{"asset":"pack/item","seconds":n?,"frame":n?,"timecode":str?,"time":ticks?,"duration":ticks|{"frame"|"seconds"|"timecode"}?,"untilEnd":bool?,"track":trackId|"V2"|"new"?,"audioTrack":trackId|"A2"|"new"?,"key":bool?,"keyColor":"#rrggbb"?,"duck":bool?}"##,
            has_seq,
            place,
            true,
        ),
    ]
}

fn err(cmd: &str) -> impl Fn(String) -> EngineError + '_ {
    move |m| bad(cmd, m)
}

/// An optional string parameter; any other type is an error, never silently ignored.
fn opt_str<'a>(p: &'a Value, k: &str, cmd: &str) -> Result<Option<&'a str>> {
    match p.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.as_str())),
        Some(_) => Err(bad(cmd, format!("`{k}` must be a string"))),
    }
}

fn opt_bool(p: &Value, k: &str, cmd: &str) -> Result<Option<bool>> {
    match p.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(bad(cmd, format!("`{k}` must be true or false"))),
    }
}

/// A finite, non-negative number of seconds.
fn opt_seconds(p: &Value, k: &str, cmd: &str) -> Result<Option<f64>> {
    match p.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => match v.as_f64() {
            Some(x) if x.is_finite() && x >= 0.0 => Ok(Some(x)),
            _ => Err(bad(cmd, format!("`{k}` must be a number of seconds, 0 or more"))),
        },
    }
}

// ------------------------------------------------------------------------------------- queries

fn list(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "assets.list";
    let ix = s.packs.index().map_err(err(cmd))?;
    let root = s.packs.root().map_err(err(cmd))?.to_string_lossy().to_string();
    if let Some(id) = opt_str(p, "pack", cmd)? {
        let pk = ix.pack(id).ok_or_else(|| bad(cmd, not_installed(id, &ix)))?;
        let items: Vec<Value> = ix.items.iter().filter(|i| i.pack == id).map(|i| item_json(i, &root)).collect();
        return Ok(json!({"pack": pk, "items": items}));
    }
    let notes = if ix.packs.is_empty() {
        vec!["no packs installed: assets.catalog lists packs to download, and assets.buildPack makes one from a folder of your own files".to_string()]
    } else {
        Vec::new()
    };
    Ok(
        json!({"dir": root, "packs": ix.packs, "problems": ix.problems.iter().map(|(d, w)| json!({"folder": d, "problem": w})).collect::<Vec<_>>(), "notes": notes}),
    )
}

fn not_installed(id: &str, ix: &store::Index) -> String {
    let have: Vec<&str> = ix.packs.iter().map(|p| p.id.as_str()).collect();
    format!(
        "no installed pack `{}`; installed: {}",
        manifest::truncate(id),
        if have.is_empty() { "none (see assets.catalog)".to_string() } else { have.join(", ") }
    )
}

fn item_json(i: &store::IndexItem, root: &str) -> Value {
    let mut v = serde_json::to_value(&i.item).unwrap_or(Value::Null);
    if let Some(o) = v.as_object_mut() {
        o.insert("asset".into(), json!(format!("{}/{}", i.pack, i.item.id)));
        o.insert("pack".into(), json!(i.pack));
        o.insert("path".into(), json!(Path::new(root).join(&i.pack).join(&i.item.file).to_string_lossy()));
        if i.item.url.is_some() {
            o.insert("downloaded".into(), json!(Path::new(root).join(&i.pack).join(&i.item.file).is_file()));
        }
    }
    v
}

fn catalogue(s: &mut Session, _p: &Value) -> Result<Value> {
    let installed: Vec<String> = s.packs.index().map(|ix| ix.packs.into_iter().map(|p| p.id).collect()).unwrap_or_default();
    let entries: Vec<Value> = catalog::CATALOG
        .iter()
        .map(|e| {
            let mut v = serde_json::to_value(e).unwrap_or(Value::Null);
            v["installed"] = json!(installed.iter().any(|i| i == e.id));
            v
        })
        .collect();
    Ok(json!({
        "entries": entries,
        "notes": ["install one with assets.install {\"catalog\": id}; only the pack you install is downloaded, and its SHA-256 is checked first"],
        "canDownload": cfg!(all(feature = "pack-download", not(target_arch = "wasm32"))),
    }))
}

fn search(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "assets.search";
    let query = opt_str(p, "query", cmd)?.unwrap_or("");
    let kinds: Option<Vec<Kind>> = match p.get("kind") {
        None | Some(Value::Null) => None,
        Some(Value::String(k)) => Some(vec![parse_kind(k, cmd)?]),
        Some(Value::Array(a)) => Some(
            a.iter()
                .map(|v| v.as_str().ok_or_else(|| bad(cmd, "`kind` must be a kind or a list of kinds")).and_then(|k| parse_kind(k, cmd)))
                .collect::<Result<_>>()?,
        ),
        Some(_) => return Err(bad(cmd, "`kind` must be a kind or a list of kinds")),
    };
    let mood = opt_str(p, "mood", cmd)?.map(manifest::fold);
    let pack = opt_str(p, "pack", cmd)?;
    let alpha = opt_bool(p, "alpha", cmd)?;
    let min_s = opt_seconds(p, "minSeconds", cmd)?;
    let max_s = opt_seconds(p, "maxSeconds", cmd)?;
    let limit = match p.get("limit") {
        None | Some(Value::Null) => 20,
        Some(v) => match v.as_u64() {
            Some(n @ 1..=500) => n as usize,
            _ => return Err(bad(cmd, "`limit` must be a whole number from 1 to 500")),
        },
    };
    let ix = s.packs.index().map_err(err(cmd))?;
    if let Some(id) = pack
        && ix.pack(id).is_none()
    {
        return Err(bad(cmd, not_installed(id, &ix)));
    }
    let root = s.packs.root().map_err(err(cmd))?.to_string_lossy().to_string();
    let terms = manifest::words(query);
    let mut hits: Vec<(u32, &store::IndexItem)> = ix
        .items
        .iter()
        .filter(|i| pack.is_none_or(|p| i.pack == p))
        .filter(|i| kinds.as_ref().is_none_or(|k| k.contains(&i.item.kind)))
        .filter(|i| mood.as_ref().is_none_or(|m| i.item.mood.iter().any(|x| x == m) || i.item.tags.iter().any(|x| x == m)))
        .filter(|i| alpha.is_none_or(|a| (i.item.alpha || i.item.key_color.is_some()) == a))
        .filter(|i| min_s.is_none_or(|m| i.item.duration.is_some_and(|d| d >= m)))
        .filter(|i| max_s.is_none_or(|m| i.item.duration.is_none_or(|d| d <= m)))
        .filter_map(|i| score(i, ix.pack(&i.pack).map_or(&[][..], |p| p.tags.as_slice()), &terms).map(|sc| (sc, i)))
        .collect();
    hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.pack.cmp(&b.1.pack)).then_with(|| a.1.item.id.cmp(&b.1.item.id)));
    let total = hits.len();
    let items: Vec<Value> = hits
        .into_iter()
        .take(limit)
        .map(|(sc, i)| {
            let mut v = item_json(i, &root);
            v["score"] = json!(sc);
            v
        })
        .collect();
    let mut notes = Vec::new();
    if ix.packs.is_empty() {
        notes.push("no packs installed: see assets.catalog, or assets.buildPack for a folder of your own files".to_string());
    } else if total == 0 {
        notes.push("nothing matched: try fewer words, another kind, or assets.list {\"pack\": id} to see a pack's items".to_string());
    }
    Ok(json!({"total": total, "items": items, "notes": notes}))
}

fn parse_kind(k: &str, cmd: &str) -> Result<Kind> {
    Kind::parse(k).ok_or_else(|| bad(cmd, format!("unknown kind {:?}: use one of {}", manifest::truncate(k), Kind::ALL.map(Kind::id).join(", "))))
}

/// How well an item matches the query words (None: a word matched nothing). Every word must
/// match a tag, mood, id, file name or the kind of the item, or a tag of its pack, which counts
/// least: a pack's words fit every item of it equally.
fn score(i: &store::IndexItem, pack_tags: &[String], terms: &[String]) -> Option<u32> {
    if terms.is_empty() {
        return Some(1);
    }
    let it = &i.item;
    let id = manifest::fold(&it.id);
    let file = manifest::fold(&it.file);
    let mut total = 0;
    for t in terms {
        let s = if it.tags.iter().any(|x| x == t) || it.mood.iter().any(|x| x == t) {
            4
        } else if it.kind.id().split('-').any(|k| k == t) {
            3
        } else if it.tags.iter().any(|x| x.starts_with(t.as_str())) {
            2
        } else if id.contains(t.as_str()) || file.contains(t.as_str()) || pack_tags.iter().any(|x| x == t || x.split('-').any(|w| w == t)) {
            1
        } else {
            return None;
        };
        total += s;
    }
    Some(total)
}

// ------------------------------------------------------------------------------------- install

/// Where the pack's metadata comes from when its folder has no `pack.json`.
#[derive(Default)]
struct Meta {
    id: Option<String>,
    /// The id from a folder or file name, used only when there is no manifest and no `id`.
    derived_id: Option<String>,
    name: Option<String>,
    license: Option<String>,
    source: Option<String>,
    author: Option<String>,
    attribution: Option<String>,
    kind: Option<Kind>,
    /// Pack-wide tags (a catalogue entry's).
    tags: Vec<String>,
    /// The archive came from a URL: without a manifest its licence is unknown, not the user's own.
    downloaded: bool,
}

impl Meta {
    fn from_params(p: &Value, cmd: &str) -> Result<Meta> {
        let s = |k: &str| opt_str(p, k, cmd).map(|v| v.map(str::to_string));
        Ok(Meta {
            id: s("id")?,
            name: s("name")?,
            license: s("license")?,
            source: s("source")?,
            author: s("author")?,
            attribution: s("attribution")?,
            ..Default::default()
        })
    }
}

/// The license an own-files pack gets when none is given.
const OWN_FILES_LICENSE: &str = "LicenseRef-Own-Files";
/// The license a downloaded archive without a manifest gets when none is given.
const UNKNOWN_LICENSE: &str = "LicenseRef-Unknown";

fn install(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "assets.install";
    let path = opt_str(p, "path", cmd)?;
    let url = opt_str(p, "url", cmd)?;
    let entry = opt_str(p, "catalog", cmd)?;
    let replace = opt_bool(p, "replace", cmd)?.unwrap_or(false);
    let wait = opt_bool(p, "wait", cmd)?.unwrap_or(false);
    let mut meta = Meta::from_params(p, cmd)?;
    if let Some(id) = &meta.id {
        manifest::check_id(id).map_err(|e| bad(cmd, format!("`id`: {e}")))?;
    }
    let size = match p.get("size") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_u64().filter(|n| *n > 0).ok_or_else(|| bad(cmd, "`size` must be a whole number of bytes"))?),
    };
    let sources = [path.is_some(), url.is_some(), entry.is_some()].iter().filter(|x| **x).count();
    if sources != 1 {
        return Err(bad(cmd, "give exactly one of `path` (a folder, a .zip or a pack.json), `url` with `sha256`, or `catalog` (an id from assets.catalog)"));
    }
    // a download runs as a job: the app's control channel and MCP requests are answered on its UI
    // thread, which must not wait on the network
    let remote = if let Some(id) = entry {
        let e = catalog::find(id).ok_or_else(|| bad(cmd, format!("no catalogue entry `{}`: assets.catalog lists them", manifest::truncate(id))))?;
        meta.id = meta.id.or(Some(e.id.into()));
        meta.name = meta.name.or(Some(e.name.into()));
        meta.license = Some(e.license.into());
        meta.source = Some(e.page.into());
        meta.author = Some(e.author.into());
        meta.attribution = Some(e.attribution.into());
        meta.kind = Kind::parse(e.kind);
        meta.tags = e.tags.iter().map(|t| t.to_string()).collect();
        Some((e.url.to_string(), e.sha256.to_string(), Some(e.size), e.name.to_string()))
    } else if let Some(u) = url {
        let sha = opt_str(p, "sha256", cmd)?
            .ok_or_else(|| bad(cmd, "a `url` needs its `sha256` (64 hex digits) so the download can be checked before it is installed"))?;
        manifest::check_url(u).map_err(err(cmd))?;
        let sha = manifest::check_sha256(sha).map_err(err(cmd))?;
        meta.source = meta.source.or(Some(u.to_string()));
        meta.downloaded = true;
        Some((u.to_string(), sha, size, manifest::truncate(u)))
    } else {
        None
    };
    if let Some((u, sha, size, label)) = remote {
        let store = s.packs.clone();
        store.root().map_err(err(cmd))?;
        let (media, services) = (s.media.clone(), s.services.clone());
        return run_job(s, cmd, format!("Installing {label}"), wait, move |progress| {
            let stage = store.stage()?;
            let file = stage.dir.join("download");
            store::download(&u, &sha, size, store::MAX_DOWNLOAD_BYTES, &file, progress)?;
            let mut notes = Vec::new();
            let (content, found) = if looks_like_json(&file) { manifest_only(&file, &stage.dir)? } else { from_zip(&file, &stage.dir, &mut notes)? };
            let (m, warnings) = finish(&media, &*services, &content, found, meta, &mut notes)?;
            let ix = store.commit(&content, &m.id, replace)?;
            drop(stage);
            Ok(installed(&m, &ix, warnings, notes))
        });
    }
    let stage = s.packs.stage().map_err(err(cmd))?;
    let mut notes = Vec::new();
    let src = PathBuf::from(path.unwrap_or_default());
    let (content, found) = if src.is_dir() {
        let dest = stage.dir.join("pack");
        std::fs::create_dir_all(&dest).map_err(|e| bad(cmd, format!("{}: {e}", dest.display())))?;
        let (files, skipped) = scan::walk(&src, false).map_err(err(cmd))?;
        scan::copy_into(&files, &dest).map_err(err(cmd))?;
        notes.extend(skipped.into_iter().map(|f| format!("skipped {f}")));
        meta.source = meta.source.or(Some("own files".into()));
        meta.derived_id = src.file_name().map(|n| manifest::slug(&n.to_string_lossy(), "my-pack"));
        meta.name = meta.name.or_else(|| src.file_name().map(|n| n.to_string_lossy().to_string()));
        let has = dest.join(MANIFEST).is_file();
        (dest, has)
    } else if src.is_file() {
        if src.file_name().is_some_and(|n| n == MANIFEST) || looks_like_json(&src) {
            let folder = folder_of(&src);
            let others =
                folder.read_dir().map(|r| r.flatten().any(|e| e.file_name() != MANIFEST && !e.file_name().to_string_lossy().starts_with('.'))).unwrap_or(false);
            if src.file_name().is_some_and(|n| n == MANIFEST) && others {
                return Err(bad(
                    cmd,
                    format!("{} sits in a folder with other files: install the folder (\"path\": its folder) so its files come along", src.display()),
                ));
            }
            manifest_only(&src, &stage.dir).map_err(err(cmd))?
        } else {
            meta.source = meta.source.or(Some("own files".into()));
            meta.derived_id = src.file_stem().map(|n| manifest::slug(&n.to_string_lossy(), "my-pack"));
            from_zip(&src, &stage.dir, &mut notes).map_err(err(cmd))?
        }
    } else {
        return Err(bad(cmd, format!("{} is not a folder or file Filmagic can read", manifest::truncate(&src.to_string_lossy()))));
    };
    let (m, warnings) = finish(&s.media, &*s.services, &content, found, meta, &mut notes).map_err(err(cmd))?;
    let ix = s.packs.commit(&content, &m.id, replace).map_err(err(cmd))?;
    drop(stage);
    Ok(installed(&m, &ix, warnings, notes))
}

/// The folder a file is in; a bare name (`pack.json`) has an empty parent, which is the
/// working directory.
fn folder_of(file: &Path) -> &Path {
    file.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."))
}

/// Run `work` as a job (`jobs.list`, `jobs.cancel`): on a thread, or here with `wait` (the CLI,
/// which exits after the command, always waits). `work` gets a progress callback for downloads
/// that returns false once the job is cancelled. A job that waited returns `work`'s result; one
/// that did not returns the job to poll, and its finished status says what it did.
fn run_job(
    s: &mut Session,
    cmd: &'static str,
    label: String,
    wait: bool,
    work: impl FnOnce(store::OnProgress) -> std::result::Result<Value, String> + Send + 'static,
) -> Result<Value> {
    use std::sync::PoisonError;
    use std::sync::atomic::Ordering;
    let id = s.jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1;
    let job = crate::Job { id, label, progress: Default::default(), result: Default::default() };
    let (prog, res) = (job.progress.clone(), job.result.clone());
    let out: std::sync::Arc<std::sync::Mutex<Option<std::result::Result<Value, String>>>> = Default::default();
    let out2 = out.clone();
    let body = move || {
        let t0 = web_time::Instant::now();
        let on = |done: u64, total: u64| {
            prog.done.store(done, Ordering::Relaxed);
            if total > 0 {
                prog.total.store(total, Ordering::Relaxed);
            }
            !prog.cancel.load(Ordering::Relaxed)
        };
        let r = work(&on);
        let status = match &r {
            Ok(v) => v.get("status").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| "done".into()),
            Err(e) => e.clone(),
        };
        *prog.status.lock().unwrap_or_else(PoisonError::into_inner) = status.clone();
        let report = match &r {
            Ok(_) => Ok(filmagic_export::Report {
                path: String::new(),
                frames: 0,
                seconds: t0.elapsed().as_secs_f64(),
                bytes: 0,
                render_fps: 0.0,
                extra_files: Vec::new(),
            }),
            Err(e) => {
                *prog.error.lock().unwrap_or_else(PoisonError::into_inner) = Some(e.clone());
                Err(e.clone())
            }
        };
        prog.finished.store(true, Ordering::Relaxed);
        *res.lock().unwrap_or_else(PoisonError::into_inner) = Some(report);
        *out2.lock().unwrap_or_else(PoisonError::into_inner) = Some(r);
    };
    let run = crate::export_tools::guard_job(job.progress.clone(), job.result.clone(), body);
    s.jobs.push(job);
    if wait || cfg!(target_arch = "wasm32") {
        run();
        let r = out.lock().unwrap_or_else(PoisonError::into_inner).take();
        return match r {
            Some(Ok(mut v)) => {
                v["job"] = json!(id);
                Ok(v)
            }
            Some(Err(e)) => Err(bad(cmd, e)),
            None => Err(bad(cmd, "the job stopped before it finished (see the crash log)")),
        };
    }
    std::thread::Builder::new().name("filmagic-assets".into()).spawn(run).map_err(|e| EngineError::Other(e.to_string()))?;
    Ok(
        json!({"job": id, "poll": "jobs.list", "cancel": "jobs.cancel", "notes": ["downloading in the background: poll jobs.list until this job is finished; its status says what was installed"]}),
    )
}

/// The result of an install: the pack's summary and what to do next.
fn installed(m: &Manifest, ix: &store::Index, mut warnings: Vec<String>, mut notes: Vec<String>) -> Value {
    if m.license == UNKNOWN_LICENSE {
        warnings.push("the archive has no pack.json and no `license` was given: its licence is recorded as unknown. Find out what it is before a video using it is published, and reinstall with \"license\" (and \"replace\": true) to record it".into());
    }
    if m.license == OWN_FILES_LICENSE {
        notes.push(
            "licence recorded as your own files: make sure you may publish them before a video using them goes out (pass \"license\" to record another)".into(),
        );
    }
    let remote = m.items.iter().filter(|i| i.url.is_some()).count();
    if remote > 0 {
        notes.push(format!(
            "{remote} items are not downloaded yet: assets.fetch {{\"pack\": \"{}\"}} downloads them, or one with {{\"asset\": \"{}/<item>\"}}",
            m.id, m.id
        ));
    }
    notes.push(format!(
        "find items with assets.search {{\"pack\": \"{}\"}} and put one on the timeline with assets.place, passing the `asset` a search result gives",
        m.id
    ));
    let status = format!("Installed the pack `{}` ({} items)", m.id, m.items.len());
    json!({"status": status, "pack": ix.pack(&m.id), "warnings": warnings, "notes": notes})
}

fn looks_like_json(path: &Path) -> bool {
    store::read_capped(path, manifest::MAX_MANIFEST_BYTES).ok().is_some_and(|b| b.iter().find(|c| !c.is_ascii_whitespace()) == Some(&b'{'))
}

/// A `pack.json` alone (its items download on first use): copied into its own staging folder.
fn manifest_only(file: &Path, stage: &Path) -> std::result::Result<(PathBuf, bool), String> {
    let dest = stage.join("pack");
    std::fs::create_dir_all(&dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    let b = store::read_capped(file, manifest::MAX_MANIFEST_BYTES).map_err(|e| format!("{}: {e}", file.display()))?;
    std::fs::write(dest.join(MANIFEST), b).map_err(|e| e.to_string())?;
    Ok((dest, true))
}

/// Unpack a zip into staging; the pack is its top folder when it holds nothing else (most
/// downloads wrap their files in one folder).
fn from_zip(zip: &Path, stage: &Path, notes: &mut Vec<String>) -> std::result::Result<(PathBuf, bool), String> {
    let dest = stage.join("unpacked");
    std::fs::create_dir_all(&dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    let skipped = store::extract_zip(zip, &dest)?;
    if !skipped.is_empty() {
        let shown: Vec<&str> = skipped.iter().take(10).map(String::as_str).collect();
        notes.push(format!(
            "left out {} files that are not media, a manifest or licence text: {}{}",
            skipped.len(),
            shown.join(", "),
            if skipped.len() > 10 { ", …" } else { "" }
        ));
    }
    // a zipped folder: its manifest is one folder down
    let mut root = dest.clone();
    if !root.join(MANIFEST).is_file()
        && let Ok(rd) = std::fs::read_dir(&root)
    {
        let entries: Vec<_> = rd.flatten().collect();
        if let [only] = entries.as_slice()
            && only.path().join(MANIFEST).is_file()
        {
            root = only.path();
        }
    }
    let has = root.join(MANIFEST).is_file();
    Ok((root, has))
}

/// Read (or make) the manifest of the pack in `dir`, check that every local item is there, and
/// write the checked manifest back.
fn finish(
    media: &MediaPool,
    services: &dyn Services,
    dir: &Path,
    has_manifest: bool,
    meta: Meta,
    notes: &mut Vec<String>,
) -> std::result::Result<(Manifest, Vec<String>), String> {
    let (m, mut warnings) = if has_manifest {
        let b = store::read_capped(&dir.join(MANIFEST), manifest::MAX_MANIFEST_BYTES).map_err(|e| e.to_string())?;
        let mut m = manifest::parse(&b)?;
        if let Some(id) = &meta.id {
            m.id = id.clone();
        }
        let ignored: Vec<&str> = [("name", &meta.name), ("license", &meta.license), ("author", &meta.author), ("attribution", &meta.attribution)]
            .iter()
            .filter(|(_, v)| v.is_some())
            .map(|(k, _)| *k)
            .collect();
        if !ignored.is_empty() && !meta.downloaded {
            notes.push(format!("the pack's {MANIFEST} names these, so `{}` was not used: edit the manifest to change them", ignored.join("`, `")));
        }
        let w = manifest::validate(&mut m)?;
        (m, w)
    } else {
        let (mut items, w) = scan::items(media, services, dir, None)?;
        if items.is_empty() {
            return Err("found no media Filmagic can open in it (audio, video or pictures)".into());
        }
        for it in &mut items {
            if let Some(k) = meta.kind
                && k.is_audio()
                && it.kind.is_audio()
            {
                it.kind = k;
            }
        }
        let id = meta.id.clone().or_else(|| meta.derived_id.clone()).unwrap_or_else(|| "my-pack".into());
        let mut m = Manifest {
            format: manifest::FORMAT,
            name: meta.name.clone().unwrap_or_else(|| id.clone()),
            id,
            version: None,
            description: None,
            license: meta.license.clone().unwrap_or_else(|| if meta.downloaded { UNKNOWN_LICENSE } else { OWN_FILES_LICENSE }.into()),
            source: meta.source.clone().unwrap_or_else(|| "own files".into()),
            author: meta.author.clone(),
            attribution: meta.attribution.clone(),
            tags: meta.tags.clone(),
            items,
        };
        let mut w2 = manifest::validate(&mut m)?;
        w2.extend(w);
        (m, w2)
    };
    let missing: Vec<&str> = m.items.iter().filter(|i| i.url.is_none() && !dir.join(&i.file).is_file()).map(|i| i.file.as_str()).collect();
    if !missing.is_empty() {
        let shown: Vec<&str> = missing.iter().take(10).copied().collect();
        return Err(format!("{MANIFEST} lists {} files the pack does not hold: {}", missing.len(), shown.join(", ")));
    }
    // a remote item already on disk must be the one the manifest pins
    for it in m.items.iter().filter(|i| i.sha256.is_some() && dir.join(&i.file).is_file()) {
        let got = store::sha256_file(&dir.join(&it.file))?;
        if Some(&got) != it.sha256.as_ref() {
            return Err(format!("`{}` does not match the sha256 its manifest pins", it.file));
        }
    }
    if m.items.is_empty() {
        warnings.push("the pack lists no items".into());
    }
    store::write_manifest(dir, &m)?;
    Ok((m, warnings))
}

fn remove(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "assets.remove";
    let id = opt_str(p, "pack", cmd)?.ok_or_else(|| bad(cmd, "need `pack`: an id from assets.list"))?;
    let (ix, cleared) = s.packs.remove(id).map_err(err(cmd))?;
    let root = s.packs.root().map_err(err(cmd))?.join(id).to_string_lossy().to_string();
    let used: usize = s
        .project
        .items
        .values()
        .filter(|i| matches!(&i.kind, ItemKind::Media(m) if matches!(&m.media, MediaRef::File { path } if Path::new(path).starts_with(&root))))
        .count();
    let mut warnings = Vec::new();
    if used > 0 {
        warnings.push(format!("{used} items of the open project came from this pack and are offline now: reinstall the pack, or relink them"));
    }
    let mut notes = Vec::new();
    if !cleared {
        notes.push("some files were still in use (open in another program); they are deleted on the next install or remove".to_string());
    }
    Ok(json!({"removed": id, "packs": ix.packs.len(), "warnings": warnings, "notes": notes}))
}

/// `assets.fetch`: download remote items of an installed pack (one `asset`, or every one of a
/// `pack` not downloaded yet), as a job.
fn fetch(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "assets.fetch";
    let wait = opt_bool(p, "wait", cmd)?.unwrap_or(false);
    let (asset, pack) = (opt_str(p, "asset", cmd)?, opt_str(p, "pack", cmd)?);
    let ix = s.packs.index().map_err(err(cmd))?;
    let (pack, only) = match (asset, pack) {
        (Some(r), None) => {
            let (pk, id) = split_ref(r, cmd)?;
            (pk.to_string(), Some(id.to_string()))
        }
        (None, Some(pk)) => (pk.to_string(), None),
        _ => return Err(bad(cmd, "give `asset` (\"pack/item\") or `pack`, not both")),
    };
    if ix.pack(&pack).is_none() {
        return Err(bad(cmd, not_installed(&pack, &ix)));
    }
    let dir = s.packs.pack_dir(&pack).map_err(err(cmd))?;
    let todo: Vec<manifest::Item> = ix
        .items
        .iter()
        .filter(|i| i.pack == pack && only.as_ref().is_none_or(|o| &i.item.id == o))
        .map(|i| i.item.clone())
        .filter(|it| it.url.is_some() && !dir.join(&it.file).is_file())
        .collect();
    if let Some(o) = &only
        && ix.item(&pack, o).is_none()
    {
        return Err(bad(cmd, format!("the pack `{pack}` has no item `{}`", manifest::truncate(o))));
    }
    if todo.is_empty() {
        return Ok(json!({"status": "nothing to download", "downloaded": 0, "notes": ["every item asked for is on disk already: assets.place it"]}));
    }
    let store = s.packs.clone();
    let n = todo.len();
    run_job(s, cmd, format!("Downloading {n} assets of {pack}"), wait, move |progress| {
        for it in &todo {
            manifest::safe_rel_path(&it.file)?;
            let (Some(url), Some(sha)) = (&it.url, &it.sha256) else { continue };
            let file = dir.join(&it.file);
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
            }
            store::download(url, sha, it.size, manifest::MAX_REMOTE_ITEM_BYTES, &file, progress).map_err(|e| format!("`{pack}/{}`: {e}", it.id))?;
        }
        let _ = store.rebuild_index();
        Ok(json!({"status": format!("Downloaded {n} assets of {pack}"), "downloaded": n, "notes": ["place them with assets.place"]}))
    })
}

// ------------------------------------------------------------------------------------- build

fn build_pack(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "assets.buildPack";
    let from = PathBuf::from(opt_str(p, "from", cmd)?.ok_or_else(|| bad(cmd, "need `from`: the folder that holds your files"))?);
    if !from.is_dir() {
        return Err(bad(cmd, format!("{} is not a folder", manifest::truncate(&from.to_string_lossy()))));
    }
    let do_install = opt_bool(p, "install", cmd)?.unwrap_or(true);
    let replace = opt_bool(p, "replace", cmd)?.unwrap_or(false);
    let meta = Meta::from_params(p, cmd)?;
    // a pack.json already in the folder keeps the id, tags, kinds and credits someone wrote into it
    let previous = store::read_pack(&from).ok();
    let id = match (&meta.id, &previous) {
        (Some(id), _) => {
            manifest::check_id(id).map_err(|e| bad(cmd, format!("`id`: {e}")))?;
            id.clone()
        }
        (None, Some(m)) => m.id.clone(),
        (None, None) => from.file_name().map(|n| manifest::slug(&n.to_string_lossy(), "my-pack")).unwrap_or_else(|| "my-pack".into()),
    };
    let (items, mut warnings) = scan::items(&s.media, &*s.services, &from, previous.as_ref()).map_err(err(cmd))?;
    if items.is_empty() {
        return Err(bad(cmd, format!("{} holds no media Filmagic can open (audio, video or pictures)", from.display())));
    }
    let pick = |new: &Option<String>, old: Option<&String>| new.clone().or_else(|| old.cloned());
    let mut m = Manifest {
        format: manifest::FORMAT,
        id: id.clone(),
        name: pick(&meta.name, previous.as_ref().map(|m| &m.name))
            .unwrap_or_else(|| from.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| id.clone())),
        version: previous.as_ref().and_then(|m| m.version.clone()),
        description: previous.as_ref().and_then(|m| m.description.clone()),
        license: pick(&meta.license, previous.as_ref().map(|m| &m.license)).unwrap_or_else(|| OWN_FILES_LICENSE.into()),
        source: pick(&meta.source, previous.as_ref().map(|m| &m.source)).unwrap_or_else(|| "own files".into()),
        author: pick(&meta.author, previous.as_ref().and_then(|m| m.author.as_ref())),
        attribution: pick(&meta.attribution, previous.as_ref().and_then(|m| m.attribution.as_ref())),
        tags: previous.as_ref().map(|m| m.tags.clone()).unwrap_or_default(),
        items,
    };
    warnings.extend(manifest::validate(&mut m).map_err(err(cmd))?);
    let mut kinds = std::collections::BTreeMap::new();
    for it in &m.items {
        *kinds.entry(it.kind.id()).or_insert(0usize) += 1;
    }
    let keyed = m.items.iter().filter(|i| i.key_color.is_some()).count();
    let summary =
        json!({"id": m.id, "name": m.name, "items": m.items.len(), "kinds": kinds, "keyed": keyed, "transparent": m.items.iter().filter(|i| i.alpha).count()});
    if !do_install {
        let target = from.join(MANIFEST);
        if target.exists() && !replace && previous.is_none() {
            return Err(bad(cmd, format!("{} exists and is not a pack manifest; pass \"replace\":true to overwrite it", target.display())));
        }
        store::write_manifest(&from, &m).map_err(err(cmd))?;
        return Ok(json!({
            "manifest": target.to_string_lossy(),
            "pack": summary,
            "warnings": warnings,
            "notes": [format!("edit tags, kinds or moods in {MANIFEST} if you like, then run assets.install {{\"path\": {:?}}}", from.to_string_lossy())],
        }));
    }
    let stage = s.packs.stage().map_err(err(cmd))?;
    let dest = stage.dir.join("pack");
    std::fs::create_dir_all(&dest).map_err(|e| bad(cmd, format!("{}: {e}", dest.display())))?;
    let (files, skipped) = scan::walk(&from, false).map_err(err(cmd))?;
    let files: Vec<scan::Found> = files.into_iter().filter(|f| f.rel != MANIFEST).collect();
    scan::copy_into(&files, &dest).map_err(err(cmd))?;
    store::write_manifest(&dest, &m).map_err(err(cmd))?;
    let ix = s.packs.commit(&dest, &m.id, replace).map_err(err(cmd))?;
    drop(stage);
    let mut v = installed(&m, &ix, warnings, skipped.into_iter().map(|f| format!("skipped {f}")).collect());
    v["built"] = summary;
    Ok(v)
}

// ------------------------------------------------------------------------------------- place

/// `pack/item` as its two ids.
fn split_ref<'a>(r: &'a str, cmd: &str) -> Result<(&'a str, &'a str)> {
    r.split_once('/')
        .filter(|(a, b)| manifest::check_id(a).is_ok() && manifest::check_id(b).is_ok())
        .ok_or_else(|| bad(cmd, format!("`asset` {:?} must be \"pack/item\", as assets.search returns it", manifest::truncate(r))))
}

/// The item's file on disk, downloaded first when it is a remote item not fetched yet.
/// The item's file on disk, or what to run first when it is not there.
fn local_file(s: &Session, pack: &str, it: &manifest::Item, cmd: &str) -> Result<PathBuf> {
    let dir = s.packs.pack_dir(pack).map_err(err(cmd))?;
    manifest::safe_rel_path(&it.file).map_err(err(cmd))?;
    let file = dir.join(&it.file);
    if file.is_file() {
        return Ok(file);
    }
    if it.url.is_some() {
        // a download here would hold up the app's UI thread: it is a job of its own
        return Err(bad(
            cmd,
            format!(
                "`{pack}/{}` is not downloaded yet: run assets.fetch {{\"asset\": \"{pack}/{}\"}}, wait until its job is finished (jobs.list), then place it",
                it.id, it.id
            ),
        ));
    }
    Err(bad(cmd, format!("{} is missing from the pack: reinstall it (assets.install with \"replace\":true)", file.display())))
}

fn place(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "assets.place";
    let r = opt_str(p, "asset", cmd)?.ok_or_else(|| bad(cmd, "need `asset`: \"pack/item\" from assets.search"))?;
    let (pack, id) = split_ref(r, cmd)?;
    let ix = s.packs.index().map_err(err(cmd))?;
    let Some(entry) = ix.item(pack, id).cloned() else {
        if ix.pack(pack).is_none() {
            return Err(bad(cmd, not_installed(pack, &ix)));
        }
        return Err(bad(cmd, format!("the pack `{pack}` has no item `{id}`: assets.search {{\"pack\": \"{pack}\", \"query\": \"…\"}} finds its items")));
    };
    let it = entry.item;
    // every parameter is checked before the first change
    let at = crate::commands::sequence_time_p(s, p, "", cmd)?.unwrap_or(s.playhead());
    let rate = s.sequence_rate();
    let df = s.active_sequence().map(|q| q.settings.drop_frame).unwrap_or(false);
    let duration = tick_or_time_p(p, "duration", rate, df, cmd)?;
    if duration.is_some_and(|d| d <= Tick::ZERO) {
        return Err(bad(cmd, "`duration` must be longer than 0"));
    }
    let until_end = opt_bool(p, "untilEnd", cmd)?.unwrap_or(false);
    if until_end && duration.is_some() {
        return Err(bad(cmd, "give `duration` or `untilEnd`, not both"));
    }
    let seq_end = s.active_sequence().map(|q| q.duration()).unwrap_or_default();
    if until_end && seq_end <= at {
        return Err(bad(cmd, "`untilEnd`: the sequence ends before that time; give a `duration` instead"));
    }
    let key = opt_bool(p, "key", cmd)?;
    let key_color = match opt_str(p, "keyColor", cmd)? {
        Some(c) => Some(manifest::parse_hex_color(c).map(manifest::hex).ok_or_else(|| bad(cmd, "`keyColor` must be #rrggbb"))?),
        None => it.key_color.clone(),
    };
    if key == Some(true) && key_color.is_none() {
        return Err(bad(cmd, "this item has no key colour on record: pass \"keyColor\": \"#rrggbb\" (the colour of its screen)"));
    }
    let duck = opt_bool(p, "duck", cmd)?.unwrap_or(true);
    // tracks are resolved before the first change: an id, a name (`V2`, `A1`) or "new"
    let mut dest = [None, None];
    for (n, k) in ["track", "audioTrack"].into_iter().enumerate() {
        dest[n] = match p.get(k) {
            None | Some(Value::Null) => None,
            Some(Value::String(v)) if v.eq_ignore_ascii_case("new") => Some(json!("new")),
            Some(v) if v.is_u64() || v.is_string() => crate::commands::track_p(s, p, k, cmd)?.map(|t| json!(t.0)),
            Some(_) => return Err(bad(cmd, format!("`{k}` must be a track id, a track name like \"V2\" or \"A1\", or \"new\""))),
        };
    }
    let [track_dest, audio_dest] = dest;
    let file = local_file(s, pack, &it, cmd)?;
    let path = file.to_string_lossy().to_string();
    let label = format!("Place {}", it.id);
    let mut warnings: Vec<String> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    let result = s.edit_group(&label, |s| {
        let item = match existing_item(s, &path) {
            Some(i) => i,
            None => {
                let r = s.execute("file.import", json!({"paths": [path]}))?;
                let first = r.get("items").and_then(Value::as_array).and_then(|a| a.first()).and_then(Value::as_u64);
                match first {
                    Some(i) => filmagic_project::ItemId(i),
                    None => {
                        let why = r.get("errors").map(Value::to_string).unwrap_or_default();
                        return Err(bad(cmd, format!("{path} did not import: {why}")));
                    }
                }
            }
        };
        let (has_v, has_a) = s.project.source_streams(item);
        let sound_only = it.kind.is_audio() || !has_v;
        let mut q = json!({"item": item.0, "time": at.0});
        let len = match (duration, until_end) {
            (Some(d), _) => Some(d),
            (None, true) => {
                let full = crate::media_duration(&s.project, &s.media, item).unwrap_or(seq_end - at);
                Some((seq_end - at).min(full))
            }
            _ => None,
        };
        if let Some(d) = len {
            q["duration"] = json!(d.0);
        }
        if sound_only {
            q["video"] = json!(false);
        }
        let (t, a) = (track_dest.clone(), audio_dest.clone());
        let end = at + len.unwrap_or_else(|| crate::media_duration(&s.project, &s.media, item).unwrap_or(rate.frame_duration()));
        if sound_only {
            let dest = a.or(t).unwrap_or_else(|| free_track(s, false, at, end).map(|t| json!(t.0)).unwrap_or(json!("new")));
            if dest.is_string() { q["audioTrack"] = dest } else { q["track"] = dest }
        } else {
            q["track"] = t.unwrap_or_else(|| free_track(s, true, at, end).map(|t| json!(t.0)).unwrap_or(json!("new")));
            if has_a {
                q["audioTrack"] = a.unwrap_or_else(|| free_track(s, false, at, end).map(|t| json!(t.0)).unwrap_or(json!("new")));
            }
        }
        let placed = s.execute("timeline.place", q)?;
        let clips: Vec<u64> = placed.get("clips").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).collect()).unwrap_or_default();
        let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
        let on = |video: bool, c: u64| {
            let tracks = if video { &seq.video_tracks } else { &seq.audio_tracks };
            tracks.iter().find(|t| t.items.iter().any(|i| i.id.0 == c)).map(|t| t.id.0)
        };
        let vclip = clips.iter().copied().find(|c| on(true, *c).is_some());
        let aclips: Vec<u64> = clips.iter().copied().filter(|c| on(false, *c).is_some()).collect();
        let (vtrack, atrack) = (vclip.and_then(|c| on(true, c)), aclips.first().and_then(|c| on(false, *c)));
        let mut keyed = None;
        if let (Some(v), Some(color)) = (vclip, &key_color)
            && key != Some(false)
        {
            if it.alpha && key.is_none() {
                notes.push("the picture is transparent already; no key applied".into());
            } else {
                s.execute("effects.apply", json!({"clips": [v], "effect": "ultra_key"}))?;
                s.execute("effects.setParam", json!({"clip": v, "effect": "ultra_key", "param": "key_color", "value": color}))?;
                keyed = Some(color.clone());
            }
        }
        let audio_type = (!aclips.is_empty()).then(|| it.kind.audio_type());
        if let Some(t) = audio_type {
            s.execute("essentialSound.setType", json!({"clips": aclips, "type": t}))?;
        }
        let mut ducking = Value::Null;
        if matches!(it.kind, Kind::Music | Kind::Ambience) && !aclips.is_empty() && duck {
            match s.execute("essentialSound.generateDucking", json!({"clips": aclips})) {
                Ok(r) => {
                    let n: usize = r.get("clips").and_then(Value::as_array).map(|a| a.iter().map(|c| c.get("regions").and_then(Value::as_array).map_or(0, Vec::len)).sum()).unwrap_or(0);
                    if n == 0 {
                        notes.push("ducking is on, but no Dialogue clip plays under it yet: mark speech with essentialSound.setType {\"type\":\"dialogue\"}, then run essentialSound.generateDucking on this clip again".into());
                    }
                    ducking = r;
                }
                Err(e) => warnings.push(format!("ducking was not set up: {e}")),
            }
        }
        Ok(json!({"item": item.0, "clips": clips, "videoClip": vclip, "audioClips": aclips, "track": vtrack, "audioTrack": atrack, "keyed": keyed, "audioType": audio_type, "ducking": ducking}))
    })?;
    let licence = it.license.clone().or_else(|| ix.pack(pack).map(|p| p.license.clone()));
    let credit = it.attribution.clone().or_else(|| ix.pack(pack).and_then(|p| p.attribution.clone()));
    if licence.as_deref().is_some_and(|l| l.to_ascii_uppercase().starts_with("CC-BY")) {
        notes.push(format!(
            "this item's licence ({}) asks for credit: add \"{}\" to the video's description or end credits",
            licence.clone().unwrap_or_default(),
            credit.clone().unwrap_or_else(|| "its author".into())
        ));
    }
    let mut v = result;
    v["asset"] = json!(r);
    v["kind"] = json!(it.kind.id());
    v["path"] = json!(path);
    v["license"] = json!(licence);
    v["attribution"] = json!(credit);
    v["warnings"] = json!(warnings);
    v["notes"] = json!(notes);
    Ok(v)
}

/// A project item that already holds `path` (placing an asset twice imports it once).
fn existing_item(s: &Session, path: &str) -> Option<filmagic_project::ItemId> {
    s.project.items.values().find(|i| matches!(&i.kind, ItemKind::Media(m) if matches!(&m.media, MediaRef::File { path: p } if p == path))).map(|i| i.id)
}

/// The first unlocked track of the kind with nothing on it between `start` and `end`. Pictures go
/// above the first video track when they can, so a meme lands over the footage, not in it.
fn free_track(s: &Session, video: bool, start: Tick, end: Tick) -> Option<TrackId> {
    let seq = s.active_sequence()?;
    let range = filmagic_time::TimeRange::new(start, end - start);
    let free = |t: &filmagic_project::Track| !t.locked && t.items.iter().all(|i| !i.range().overlaps(&range));
    if video {
        seq.video_tracks.iter().skip(1).chain(seq.video_tracks.first()).find(|t| free(t)).map(|t| t.id)
    } else {
        seq.audio_tracks.iter().find(|t| free(t)).map(|t| t.id)
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[test]
fn a_bare_manifest_name_is_in_the_working_directory() {
    assert_eq!(folder_of(Path::new("pack.json")), Path::new("."));
    assert_eq!(folder_of(Path::new("memes/pack.json")), Path::new("memes"));
}
