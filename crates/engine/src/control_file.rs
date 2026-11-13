//! The control folder: how running desktop apps tell local agents where their control servers are,
//! and how an agent tells a real Filmagic from a program that only took over its port (#17).
//!
//! Every app holds an **instance lock** while it runs, `instance-<pid>.lock`, with or without
//! `--control`. The operating system releases the lock when the process ends, however it ends, so
//! a reader can tell a live app from one that crashed without asking the network.
//!
//! An app whose control server is running also writes `app-<pid>.json`: its port, its process id and
//! version, and, when the app made its own session credential, that credential. A credential that
//! came from `FILMAGIC_CONTROL_TOKEN` is never written: the clients it was meant for already have it
//! in their environment, and the variable exists so that it never has to be stored. Each app writes
//! and removes only its own file, so a second app does not hide the first.
//!
//! **Where the folder is.** It holds a secret, so it must be readable by the current user only,
//! whatever the data directory is:
//! - on Windows it is `%LOCALAPPDATA%\Filmagic\control\<key>\`, `<key>` naming the data directory.
//!   `%LOCALAPPDATA%` is in the user's profile, which other users who are not administrators cannot
//!   open. The data directory itself may not be private: the portable build keeps it next to the
//!   executable, and a folder such as `C:\Tools\Filmagic` inherits `C:\`'s permissions, which let
//!   every user read it;
//! - elsewhere it is `<data dir>/control/`, created with mode `0700`, and files are created `0600`.
//!   A reader refuses a folder or file that another user owns or that others can write, and a file
//!   others can read.
//!
//! **Proving the server.** A file names a port, and any program can listen on a port once the app
//! that had it exits. So a client never sends the credential first: it sends `control.hello` with
//! a random nonce, and the server answers with [`proof`], an HMAC-SHA256 under the credential of
//! the nonce and the port it listens on. A client sends the credential, or anything else, only to
//! a server whose proof checks out ([`verify_proof`]). The port is in the proof so that a program
//! on one port cannot pass off the answer of a real app on another.
//!
//! Every file is written atomically (a temporary file in the same folder, then a rename) and every
//! read is size-capped and validated: the folder's contents are untrusted input.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The folder's name inside the data directory (Unix) or under `%LOCALAPPDATA%\Filmagic` (Windows).
pub const DIR_NAME: &str = "control";

/// The largest endpoint file a reader accepts. A real one is about 150 bytes.
pub const MAX_FILE_BYTES: u64 = 4096;

/// The most endpoint files a reader looks at, newest first. Crashed apps leave theirs behind
/// until a reader finds their lock free and removes them.
pub const MAX_ENDPOINTS: usize = 16;

/// The most folder entries a reader goes through.
const MAX_ENTRIES: usize = 512;

/// The context string of the hello proof; a new protocol version changes it.
const PROOF_CONTEXT: &str = "filmagic-control-hello-v1";

/// Where a running app's control server listens.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlEndpoint {
    /// TCP port on 127.0.0.1.
    pub port: u16,
    /// The app's process id: names its files and its instance lock.
    pub pid: u32,
    /// The app's version.
    #[serde(default)]
    pub version: String,
    /// The session credential (64 hex characters), or `None` when the app uses
    /// `FILMAGIC_CONTROL_TOKEN`, which clients take from their own environment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

impl ControlEndpoint {
    /// `127.0.0.1:<port>`.
    pub fn addr(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }
}

/// Whether `t` is a usable control credential: exactly 64 hex characters.
pub fn is_valid_token(t: &str) -> bool {
    t.len() == 64 && t.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Whether `n` is a usable hello nonce: exactly 64 hex characters (32 random bytes).
pub fn is_valid_nonce(n: &str) -> bool {
    is_valid_token(n)
}

/// The control folder for the app whose data directory is `data_dir`. `None` on Windows without
/// `%LOCALAPPDATA%`: there is then no private place for it, and no file is written.
pub fn control_dir(data_dir: &Path) -> Option<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty()).map(PathBuf::from);
    control_dir_for(data_dir, cfg!(windows), local)
}

/// [`control_dir`] for a given platform and `%LOCALAPPDATA%`.
pub fn control_dir_for(data_dir: &Path, windows: bool, local_app_data: Option<PathBuf>) -> Option<PathBuf> {
    if windows { Some(local_app_data?.join("Filmagic").join(DIR_NAME).join(data_dir_key(data_dir, true))) } else { Some(data_dir.join(DIR_NAME)) }
}

/// A short name for a data directory: the first 8 bytes of the SHA-256 of its absolute path, in
/// hex. `fold_case` for Windows, where paths differing in case name one folder.
pub fn data_dir_key(data_dir: &Path, fold_case: bool) -> String {
    use sha2::Digest as _;
    let abs = std::path::absolute(data_dir).unwrap_or_else(|_| data_dir.to_path_buf());
    let mut text = abs.to_string_lossy().into_owned();
    while text.len() > 1 && (text.ends_with('/') || text.ends_with('\\')) {
        text.pop();
    }
    if fold_case {
        text = text.to_lowercase();
    }
    let digest = sha2::Sha256::digest(text.as_bytes());
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

fn endpoint_path(dir: &Path, pid: u32) -> PathBuf {
    dir.join(format!("app-{pid}.json"))
}

fn lock_path(dir: &Path, pid: u32) -> PathBuf {
    dir.join(format!("instance-{pid}.lock"))
}

/// The process id in a file name `<prefix><pid><suffix>`.
fn pid_in(name: &str, prefix: &str, suffix: &str) -> Option<u32> {
    let digits = name.strip_prefix(prefix)?.strip_suffix(suffix)?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

#[cfg(unix)]
fn current_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// Whether the folder may be trusted: a real folder (not a link), and on Unix owned by the current
/// user and not writable by others.
fn check_private_dir(dir: &Path) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if !meta.is_dir() {
        return Err(format!("{} is not a folder", dir.display()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if meta.uid() != current_uid() {
            return Err(format!("{} belongs to another user", dir.display()));
        }
        if meta.mode() & 0o022 != 0 {
            return Err(format!("{} can be written by other users", dir.display()));
        }
    }
    Ok(())
}

/// Whether an endpoint file may be trusted: a regular file (not a link, a pipe or a device), and on
/// Unix owned by the current user and neither readable nor writable by others.
fn check_private_file(path: &Path, meta: &std::fs::Metadata) -> Result<(), String> {
    if !meta.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if meta.uid() != current_uid() {
            return Err(format!("{} belongs to another user", path.display()));
        }
        if meta.mode() & 0o077 != 0 {
            return Err(format!("{} can be read or written by other users", path.display()));
        }
    }
    Ok(())
}

/// Create the folder (owner-only on Unix) and check that it may be trusted.
fn prepare_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(dir)?;
    // An older folder of ours with wider permissions: narrow it. Someone else's is refused below.
    #[cfg(unix)]
    if let Ok(meta) = std::fs::symlink_metadata(dir) {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        if meta.is_dir() && meta.uid() == current_uid() && meta.mode() & 0o077 != 0 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    check_private_dir(dir).map_err(|e| std::io::Error::new(std::io::ErrorKind::PermissionDenied, e))
}

/// Create `path` new (replacing a leftover temporary file), owner-only on Unix.
fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let f = options.open(path)?;
    // The umask can only remove bits from 0600; set them explicitly all the same.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(f)
}

/// Write `endpoint` to `<dir>/app-<pid>.json` atomically. Returns the file's path.
pub fn write(dir: &Path, endpoint: &ControlEndpoint) -> std::io::Result<PathBuf> {
    if endpoint.token.as_deref().is_some_and(|t| !is_valid_token(t)) || endpoint.port == 0 {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "control endpoint needs a port and a 64-hex-character credential"));
    }
    prepare_dir(dir)?;
    let target = endpoint_path(dir, endpoint.pid);
    let body = serde_json::to_vec_pretty(endpoint).map_err(std::io::Error::other)?;
    let tmp = dir.join(format!(".app-{}.json.tmp", endpoint.pid));
    let written = create_private(&tmp).and_then(|mut f| f.write_all(&body).and_then(|()| f.sync_all())).and_then(|()| std::fs::rename(&tmp, &target));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(target)
}

/// Read one endpoint file. Untrusted: checked, size-capped and validated.
pub fn read_file(path: &Path) -> Result<ControlEndpoint, String> {
    use std::io::Read as _;
    // Before opening: a pipe named like an endpoint would block the reader forever.
    let meta = std::fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    check_private_file(path, &meta)?;
    let f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut bytes = Vec::new();
    f.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(format!("{} is larger than {MAX_FILE_BYTES} bytes", path.display()));
    }
    let endpoint: ControlEndpoint = serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    if endpoint.token.as_deref().is_some_and(|t| !is_valid_token(t)) {
        return Err(format!("{}: the credential is not 64 hex characters", path.display()));
    }
    if endpoint.port == 0 {
        return Err(format!("{}: port 0", path.display()));
    }
    Ok(endpoint)
}

/// The endpoints in `dir`, newest first (at most [`MAX_ENDPOINTS`]), each read with
/// [`read_file`]; a file whose name and process id disagree is an error. Empty when the folder
/// does not exist; one error when it cannot be trusted.
pub fn endpoints(dir: &Path) -> Vec<Result<ControlEndpoint, String>> {
    match std::fs::symlink_metadata(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        _ => {}
    }
    if let Err(e) = check_private_dir(dir) {
        return vec![Err(e)];
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<(std::time::SystemTime, u32, PathBuf)> = entries
        .take(MAX_ENTRIES)
        .flatten()
        .filter_map(|e| {
            let pid = pid_in(e.file_name().to_str()?, "app-", ".json")?;
            let modified = e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
            Some((modified, pid, e.path()))
        })
        .collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    files
        .into_iter()
        .take(MAX_ENDPOINTS)
        .map(|(_, pid, path)| {
            read_file(&path).and_then(|e| if e.pid == pid { Ok(e) } else { Err(format!("{}: names process {} inside", path.display(), e.pid)) })
        })
        .collect()
}

/// Remove `<dir>/app-<pid>.json` if it still names `pid` and `port`: an app removes only its own
/// file. Returns whether a file was removed.
pub fn remove_own(dir: &Path, pid: u32, port: u16) -> std::io::Result<bool> {
    let path = endpoint_path(dir, pid);
    match read_file(&path) {
        Ok(e) if e.pid == pid && e.port == port => match std::fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        },
        _ => Ok(false),
    }
}

/// This process's instance lock: held while the app runs, released by the operating system when
/// the process ends however it ends. Dropping it removes the file.
pub struct InstanceLock {
    file: Option<std::fs::File>,
    path: PathBuf,
}

impl InstanceLock {
    /// Create and lock `<dir>/instance-<pid>.lock`. The file is locked under a temporary name and
    /// then renamed, so a reader never finds it unlocked while this process lives.
    pub fn acquire(dir: &Path) -> std::io::Result<Self> {
        prepare_dir(dir)?;
        let pid = std::process::id();
        let tmp = dir.join(format!(".instance-{pid}.tmp"));
        let file = create_private(&tmp)?;
        let locked = file.try_lock().map_err(|e| match e {
            std::fs::TryLockError::WouldBlock => std::io::Error::new(std::io::ErrorKind::WouldBlock, "instance lock is held"),
            std::fs::TryLockError::Error(e) => e,
        });
        let path = lock_path(dir, pid);
        if let Err(e) = locked.and_then(|()| std::fs::rename(&tmp, &path)) {
            drop(file);
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        Ok(Self { file: Some(file), path })
    }

    /// The lock file's path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        // close first: Windows removes an open file only once every handle is closed
        if let Some(f) = self.file.take() {
            let _ = f.unlock();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Whether the app with process id `pid` still holds its instance lock in `dir`.
pub fn is_live(dir: &Path, pid: u32) -> bool {
    lock_state(dir, pid) == Some(true)
}

/// `Some(true)` when the app's lock is held, `Some(false)` when it is free (that app has ended: its
/// lock file and endpoint file are removed), `None` when there is no lock file or it cannot be
/// checked (an app whose lock could not be taken still writes its endpoint: ask it).
pub fn lock_state(dir: &Path, pid: u32) -> Option<bool> {
    let path = lock_path(dir, pid);
    let meta = std::fs::symlink_metadata(&path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let file = std::fs::File::open(&path).ok()?;
    match file.try_lock() {
        Ok(()) => {
            let _ = file.unlock();
            drop(file);
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(endpoint_path(dir, pid));
            Some(false)
        }
        Err(std::fs::TryLockError::WouldBlock) => Some(true),
        Err(std::fs::TryLockError::Error(_)) => None,
    }
}

/// The process ids of the apps holding an instance lock in `dir`. Locks left by apps that ended
/// are removed on the way, with their endpoint files.
pub fn live_instances(dir: &Path) -> Vec<u32> {
    if check_private_dir(dir).is_err() {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let pids: Vec<u32> = entries.take(MAX_ENTRIES).flatten().filter_map(|e| pid_in(e.file_name().to_str()?, "instance-", ".lock")).collect();
    pids.into_iter().filter(|&pid| lock_state(dir, pid) == Some(true)).collect()
}

type HmacSha256 = hmac::Hmac<sha2::Sha256>;

fn mac(token: &str, port: u16, nonce: &str) -> Option<HmacSha256> {
    use hmac::Mac as _;
    let mut m = <HmacSha256 as hmac::Mac>::new_from_slice(token.as_bytes()).ok()?;
    m.update(format!("{PROOF_CONTEXT}\n{port}\n{nonce}").as_bytes());
    Some(m)
}

/// The server's answer to `control.hello`: HMAC-SHA256 under `token` of the nonce and the port the
/// server listens on, in hex.
pub fn proof(token: &str, port: u16, nonce: &str) -> Option<String> {
    use hmac::Mac as _;
    let tag = mac(token, port, nonce)?.finalize().into_bytes();
    Some(tag.iter().map(|b| format!("{b:02x}")).collect())
}

/// Whether `proof_hex` is the [`proof`] for `token`, `port` and `nonce` (compared in constant time).
pub fn verify_proof(token: &str, port: u16, nonce: &str, proof_hex: &str) -> bool {
    use hmac::Mac as _;
    let Some(bytes) = decode_hex(proof_hex) else { return false };
    mac(token, port, nonce).is_some_and(|m| m.verify_slice(&bytes).is_ok())
}

/// 32 bytes from 64 hex characters.
fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() != 64 || !s.is_ascii() {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| s.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok())).collect()
}

#[cfg(test)]
mod tests;
