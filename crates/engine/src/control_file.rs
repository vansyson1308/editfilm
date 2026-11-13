//! The control file: how a running desktop app tells local agents where its control server is.
//!
//! When `filmagic --control <port>` starts its control server, it writes the port and the session's
//! credential to `<data dir>/control.json`. `filmagic-cli mcp` reads the file to attach to the
//! running app without `FILMAGIC_CONTROL_TOKEN` in its own environment (#17).
//!
//! The file holds a secret, so:
//! - it is written atomically (a temporary file in the same folder, then a rename), so a reader
//!   never sees half a file;
//! - on Unix it is created with mode `0600` (owner read/write only); on Windows it inherits the
//!   folder's access list, and the default data folder (`%APPDATA%\Filmagic`) is in the user's
//!   profile, which other non-administrator users cannot read;
//! - the app removes it when it exits, but only while it still holds that app's credential (a
//!   second app may have replaced it);
//! - a file left by an app that crashed is harmless: readers confirm that the port answers with the
//!   credential before they use it (`control.ping`), and the next app start replaces it.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The file's name inside the data directory.
pub const FILE_NAME: &str = "control.json";

/// The largest control file a reader accepts. A real one is about 150 bytes; anything much larger
/// is not ours and is not read into memory.
pub const MAX_FILE_BYTES: u64 = 4096;

/// Where a running app's control server listens, and the credential it requires.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlEndpoint {
    /// TCP port on 127.0.0.1.
    pub port: u16,
    /// The credential every request must carry: 64 hex characters.
    pub token: String,
    /// The app's process id (for diagnostics; liveness is checked by asking the port).
    pub pid: u32,
    /// The app's version.
    #[serde(default)]
    pub version: String,
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

/// `<data_dir>/control.json`.
pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE_NAME)
}

/// Write `endpoint` to `<data_dir>/control.json` atomically, readable only by the owner (Unix).
/// Creates `data_dir` if needed. Returns the file's path.
pub fn write(data_dir: &Path, endpoint: &ControlEndpoint) -> std::io::Result<PathBuf> {
    if !is_valid_token(&endpoint.token) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "control credential must be 64 hex characters"));
    }
    std::fs::create_dir_all(data_dir)?;
    let target = path(data_dir);
    let body = serde_json::to_vec_pretty(endpoint).map_err(std::io::Error::other)?;
    // A name only this process uses, so two apps starting at once do not write into one file.
    let tmp = data_dir.join(format!(".{FILE_NAME}.{}.tmp", std::process::id()));
    let written = write_private(&tmp, &body).and_then(|()| std::fs::rename(&tmp, &target));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(target)
}

/// Create `path` (replacing a leftover from an earlier start of this process id) with owner-only
/// permissions on Unix, write `body` and flush it to disk.
fn write_private(path: &Path, body: &[u8]) -> std::io::Result<()> {
    // A leftover temporary file from a crashed process with the same id: remove it, so
    // `create_new` below does not fail and no permissions are inherited from it.
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut f = options.open(path)?;
    // The umask can only remove bits from 0600; set them explicitly all the same, so the file is
    // never group- or world-readable whatever created the folder.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    f.write_all(body)?;
    f.sync_all()
}

/// Why no endpoint could be read.
#[derive(Debug, PartialEq, Eq)]
pub enum ReadError {
    /// There is no control file: no app with a control server has started (or it exited cleanly).
    Missing,
    /// The file exists but cannot be used (unreadable, too large, not a control file, bad token).
    Invalid(String),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => write!(f, "no control file"),
            Self::Invalid(why) => write!(f, "control file unusable: {why}"),
        }
    }
}

/// Read `<data_dir>/control.json`. The file is untrusted input: it is size-capped and its credential
/// is validated. A valid file does not mean the app is running; ask the port (`control.ping`).
pub fn read(data_dir: &Path) -> Result<ControlEndpoint, ReadError> {
    use std::io::Read as _;
    let p = path(data_dir);
    let f = match std::fs::File::open(&p) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(ReadError::Missing),
        Err(e) => return Err(ReadError::Invalid(format!("{}: {e}", p.display()))),
    };
    let mut bytes = Vec::new();
    f.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes).map_err(|e| ReadError::Invalid(format!("{}: {e}", p.display())))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(ReadError::Invalid(format!("{} is larger than {MAX_FILE_BYTES} bytes", p.display())));
    }
    let endpoint: ControlEndpoint = serde_json::from_slice(&bytes).map_err(|e| ReadError::Invalid(format!("{}: {e}", p.display())))?;
    if !is_valid_token(&endpoint.token) {
        return Err(ReadError::Invalid(format!("{}: the credential is not 64 hex characters", p.display())));
    }
    if endpoint.port == 0 {
        return Err(ReadError::Invalid(format!("{}: port 0", p.display())));
    }
    Ok(endpoint)
}

/// Remove `<data_dir>/control.json` if it still holds `token` (this app's); another app that
/// started later keeps its file. Returns whether a file was removed.
pub fn remove_if_ours(data_dir: &Path, token: &str) -> std::io::Result<bool> {
    match read(data_dir) {
        Ok(e) if e.token == token => match std::fs::remove_file(path(data_dir)) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        },
        _ => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(port: u16, c: char) -> ControlEndpoint {
        ControlEndpoint { port, token: c.to_string().repeat(64), pid: 42, version: "0.0.0".into() }
    }

    #[test]
    fn write_then_read_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("nested").join("data");
        let e = endpoint(9876, 'a');
        let p = write(&data, &e).unwrap();
        assert_eq!(p, data.join(FILE_NAME));
        assert_eq!(read(&data).unwrap(), e);
        assert_eq!(e.addr(), "127.0.0.1:9876");
        // nothing but the file is left behind
        let names: Vec<_> = std::fs::read_dir(&data).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, [std::ffi::OsString::from(FILE_NAME)]);
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        // a leftover temporary file another user could read must not pass its permissions on
        let tmp = dir.path().join(format!(".{FILE_NAME}.{}.tmp", std::process::id()));
        std::fs::write(&tmp, b"old").unwrap();
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644)).unwrap();
        let p = write(dir.path(), &endpoint(1, 'b')).unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{mode:o}");
        assert!(!tmp.exists());
    }

    #[test]
    fn a_second_write_replaces_the_first() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), &endpoint(1, 'a')).unwrap();
        write(dir.path(), &endpoint(2, 'b')).unwrap();
        assert_eq!(read(dir.path()).unwrap(), endpoint(2, 'b'));
    }

    #[test]
    fn a_bad_token_is_not_written() {
        let dir = tempfile::tempdir().unwrap();
        let mut e = endpoint(1, 'a');
        e.token = "short".into();
        assert!(write(dir.path(), &e).is_err());
        assert_eq!(read(dir.path()), Err(ReadError::Missing));
    }

    #[test]
    fn hostile_files_are_reported_not_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let p = path(dir.path());
        let long = "x".repeat(MAX_FILE_BYTES as usize + 1);
        let bad_token = serde_json::json!({"port": 1, "token": "z".repeat(64), "pid": 1}).to_string();
        let port_zero = serde_json::json!({"port": 0, "token": "a".repeat(64), "pid": 1}).to_string();
        let port_huge = serde_json::json!({"port": 70000, "token": "a".repeat(64), "pid": 1}).to_string();
        let negative = serde_json::json!({"port": -1, "token": "a".repeat(64), "pid": 1}).to_string();
        for body in ["", "{", "null", "[]", "\u{feff}{}", "{\"port\":\"9876\"}", &long, &bad_token, &port_zero, &port_huge, &negative] {
            std::fs::write(&p, body).unwrap();
            assert!(matches!(read(dir.path()), Err(ReadError::Invalid(_))), "{body:.40}");
        }
        std::fs::write(&p, [0xff, 0xfe, 0x00]).unwrap();
        assert!(matches!(read(dir.path()), Err(ReadError::Invalid(_))));
    }

    #[test]
    fn only_the_owner_of_the_credential_removes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), &endpoint(1, 'a')).unwrap();
        // another app started later and replaced the file: it is kept
        assert!(!remove_if_ours(dir.path(), &"b".repeat(64)).unwrap());
        assert!(path(dir.path()).exists());
        assert!(remove_if_ours(dir.path(), &"a".repeat(64)).unwrap());
        assert!(!path(dir.path()).exists());
        // already gone
        assert!(!remove_if_ours(dir.path(), &"a".repeat(64)).unwrap());
    }
}
