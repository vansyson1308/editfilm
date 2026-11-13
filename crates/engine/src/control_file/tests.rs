use super::*;

/// Write `body` to `p`, owner-only as the app would, so that only the contents are under test.
fn put(p: &Path, body: impl AsRef<[u8]>) {
    std::fs::write(p, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn endpoint(port: u16, pid: u32, token: Option<char>) -> ControlEndpoint {
    ControlEndpoint { port, pid, version: "0.0.0".into(), token: token.map(|c| c.to_string().repeat(64)) }
}

/// B1 (#17 review): on Windows the folder is under `%LOCALAPPDATA%` whatever the data directory
/// is (the portable build's data folder next to the executable can be readable by every user),
/// one folder per data directory; elsewhere it is inside the data directory.
#[test]
fn the_folder_is_per_user_on_windows_and_per_data_dir_everywhere() {
    let local = PathBuf::from(r"C:\Users\me\AppData\Local");
    let portable = Path::new(r"C:\Tools\Filmagic\data");
    let dir = control_dir_for(portable, true, Some(local.clone())).unwrap();
    assert!(dir.starts_with(local.join("Filmagic").join(DIR_NAME)), "{}", dir.display());
    assert!(!dir.starts_with(portable));
    assert_eq!(control_dir_for(portable, true, None), None, "no private place, no file");
    let other = control_dir_for(Path::new(r"D:\shared\data"), true, Some(local.clone())).unwrap();
    assert_ne!(dir, other, "two data directories, two folders");
    assert_eq!(control_dir_for(Path::new("/home/me/.local/share/filmagic"), false, None), Some(PathBuf::from("/home/me/.local/share/filmagic/control")));
    // one data directory, however it is spelled
    assert_eq!(data_dir_key(Path::new("/a/b/"), false), data_dir_key(Path::new("/a/b"), false));
    assert_eq!(data_dir_key(Path::new("/A/B"), true), data_dir_key(Path::new("/a/b"), true));
    assert_ne!(data_dir_key(Path::new("/A/B"), false), data_dir_key(Path::new("/a/b"), false));
    assert_eq!(data_dir_key(Path::new("/a"), false).len(), 16);
}

#[test]
fn write_then_read_round_trips_and_leaves_nothing_else() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("nested").join(DIR_NAME);
    let e = endpoint(9876, 42, Some('a'));
    let p = write(&dir, &e).unwrap();
    assert_eq!(p, dir.join("app-42.json"));
    assert_eq!(read_file(&p).unwrap(), e);
    assert_eq!(e.addr(), "127.0.0.1:9876");
    let names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(names, [std::ffi::OsString::from("app-42.json")]);
}

/// S6 (#17 review): a credential from FILMAGIC_CONTROL_TOKEN is never written to disk.
#[test]
fn an_inherited_credential_is_not_written() {
    let tmp = tempfile::tempdir().unwrap();
    let p = write(tmp.path(), &endpoint(1, 7, None)).unwrap();
    let text = std::fs::read_to_string(&p).unwrap();
    assert!(!text.contains("token"), "{text}");
    assert_eq!(read_file(&p).unwrap().token, None);
}

#[cfg(unix)]
#[test]
fn files_and_folder_are_owner_only_and_others_are_refused() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join(DIR_NAME);
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    // a leftover temporary file another user could read passes nothing on
    let leftover = dir.join(".app-5.json.tmp");
    std::fs::write(&leftover, b"old").unwrap();
    std::fs::set_permissions(&leftover, std::fs::Permissions::from_mode(0o644)).unwrap();
    let p = write(&dir, &endpoint(1, 5, Some('b'))).unwrap();
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&p), 0o600);
    assert_eq!(mode(&dir), 0o700, "our own wider folder is narrowed");
    assert!(!leftover.exists());
    // a file others can read, or a folder others can write, is not trusted
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(read_file(&p).unwrap_err().contains("other users"));
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
    let all = endpoints(&dir);
    assert!(matches!(all.as_slice(), [Err(e)] if e.contains("other users")), "{all:?}");
    assert!(write(&dir, &endpoint(1, 6, Some('b'))).is_ok(), "our own folder is narrowed again");
}

#[test]
fn a_bad_endpoint_is_not_written() {
    let tmp = tempfile::tempdir().unwrap();
    let mut e = endpoint(1, 1, Some('a'));
    e.token = Some("short".into());
    assert!(write(tmp.path(), &e).is_err());
    assert!(write(tmp.path(), &endpoint(0, 1, Some('a'))).is_err(), "port 0");
    assert!(endpoints(tmp.path()).is_empty());
}

#[test]
fn hostile_files_are_reported_not_trusted() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    write(dir, &endpoint(1, 3, Some('a'))).unwrap();
    let p = dir.join("app-3.json");
    let long = "x".repeat(MAX_FILE_BYTES as usize + 1);
    let bad_token = serde_json::json!({"port": 1, "token": "z".repeat(64), "pid": 3}).to_string();
    let port_zero = serde_json::json!({"port": 0, "pid": 3}).to_string();
    let port_huge = serde_json::json!({"port": 70000, "pid": 3}).to_string();
    let negative = serde_json::json!({"port": -1, "pid": 3}).to_string();
    let other_pid = serde_json::json!({"port": 1, "pid": 4}).to_string();
    for body in ["", "{", "null", "[]", "\u{feff}{}", "{\"port\":\"9876\"}", &long, &bad_token, &port_zero, &port_huge, &negative, &other_pid] {
        put(&p, body);
        assert!(matches!(endpoints(dir).as_slice(), [Err(_)]), "{body:.40}");
    }
    put(&p, [0xff, 0xfe, 0x00]);
    assert!(read_file(&p).is_err());
    // a folder or a link where a file should be
    std::fs::remove_file(&p).unwrap();
    std::fs::create_dir(&p).unwrap();
    assert!(read_file(&p).unwrap_err().contains("not a regular file"));
    // names that are not ours are ignored
    for name in ["app-.json", "app-x.json", "app-1.json.tmp", "app--1.json", "app-99999999999.json", "notes.txt"] {
        put(&dir.join(name), "{}");
    }
    assert_eq!(endpoints(dir).len(), 1, "only the folder named app-3.json");
}

/// A pipe named like an endpoint file must not block the reader (it would wait for a writer forever).
#[cfg(unix)]
#[test]
fn a_pipe_is_refused_without_blocking() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("app-9.json");
    let Ok(status) = std::process::Command::new("mkfifo").arg(&p).status() else { return };
    if !status.success() {
        return;
    }
    assert!(read_file(&p).unwrap_err().contains("not a regular file"));
}

/// S5 (#17 review): two apps keep a file each; the one that exits removes only its own, and the
/// other stays findable.
#[test]
fn two_instances_keep_their_own_files() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    write(dir, &endpoint(4000, 10, Some('a'))).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    write(dir, &endpoint(4001, 11, Some('b'))).unwrap();
    let found: Vec<_> = endpoints(dir).into_iter().map(Result::unwrap).collect();
    assert_eq!(found, [endpoint(4001, 11, Some('b')), endpoint(4000, 10, Some('a'))], "newest first");
    // a different port or process id is not ours to remove
    assert!(!remove_own(dir, 11, 4000).unwrap());
    assert!(!remove_own(dir, 12, 4001).unwrap());
    assert!(remove_own(dir, 11, 4001).unwrap());
    let found: Vec<_> = endpoints(dir).into_iter().map(Result::unwrap).collect();
    assert_eq!(found, [endpoint(4000, 10, Some('a'))], "the earlier app is still found");
    assert!(!remove_own(dir, 11, 4001).unwrap(), "already gone");
}

/// The instance lock tells a live app from one that ended, with no network: a lock file nobody
/// holds is removed with its endpoint file.
#[test]
fn the_instance_lock_tells_live_from_ended() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let me = std::process::id();
    let lock = InstanceLock::acquire(dir).unwrap();
    assert_eq!(lock.path(), dir.join(format!("instance-{me}.lock")));
    assert!(is_live(dir, me));
    assert_eq!(live_instances(dir), [me]);
    // an app that crashed: its lock file is there but free
    let dead = me.wrapping_add(1);
    put(&dir.join(format!("instance-{dead}.lock")), b"");
    write(dir, &endpoint(5000, dead, Some('c'))).unwrap();
    assert!(!is_live(dir, dead));
    assert!(!dir.join(format!("instance-{dead}.lock")).exists());
    assert!(!dir.join(format!("app-{dead}.json")).exists());
    assert_eq!(live_instances(dir), [me]);
    // no lock file at all: not live
    assert!(!is_live(dir, dead.wrapping_add(1)));
    drop(lock);
    assert!(!dir.join(format!("instance-{me}.lock")).exists());
    assert!(live_instances(dir).is_empty());
}

/// S1 (#17 review): the hello proof binds the credential, the nonce and the server's port.
#[test]
fn the_proof_checks_credential_nonce_and_port() {
    let (token, nonce) = ("a".repeat(64), "b".repeat(64));
    let p = proof(&token, 9876, &nonce).unwrap();
    assert_eq!(p.len(), 64);
    assert!(verify_proof(&token, 9876, &nonce, &p));
    assert!(!verify_proof(&token, 9877, &nonce, &p), "an answer from another port");
    assert!(!verify_proof(&token, 9876, &"c".repeat(64), &p), "an old answer");
    assert!(!verify_proof(&"d".repeat(64), 9876, &nonce, &p), "another credential");
    for bad in ["", "zz", &p[..63], &format!("{p}0"), &"é".repeat(32), &"g".repeat(64)] {
        assert!(!verify_proof(&token, 9876, &nonce, bad), "{bad:?}");
    }
    assert!(is_valid_nonce(&nonce) && !is_valid_nonce("short"));
}
