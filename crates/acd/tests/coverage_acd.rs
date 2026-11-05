//! Coverage-driven tests (docs/research/2026-11-05-coverage.md).
mod common;
use common::acd;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::time::{Duration, SystemTime};

/// Set mtime to now minus `age` on the path itself (a symlink is not
/// followed).
fn aged(path: &std::path::Path, age: Duration) {
    set_mtime(path, SystemTime::now() - age);
}

/// A time that stays in the future for the whole test, so "fresh" does not
/// depend on how quickly cleanup runs.
fn future(path: &std::path::Path) {
    set_mtime(path, SystemTime::now() + Duration::from_secs(3600));
}

fn set_mtime(path: &std::path::Path, t: SystemTime) {
    use std::os::unix::ffi::OsStrExt;
    let secs = match t.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as libc::time_t,
        Err(_) => 0,
    };
    let ts = libc::timespec { tv_sec: secs, tv_nsec: 0 };
    let times = [ts, ts];
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    let rc = unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), times.as_ptr(), libc::AT_SYMLINK_NOFOLLOW) };
    assert_eq!(rc, 0, "utimensat failed: {}", std::io::Error::last_os_error());
}

/// Leftover temp files from a crashed TLS rotation are cleaned up by `acd
/// init`, but only regular files that match the exact pattern and are old; a
/// fresh temp (a rotation in progress), a lookalike name and a symlink (and
/// whatever it points to) are left alone.
#[test]
fn init_removes_only_old_regular_tls_temp_files() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("cfg");
    fs::create_dir(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();

    let old = dir.join("tls.pem.111.tmp");
    let fresh = dir.join("tls.pem.222.tmp");
    let lookalike = dir.join("tls.pem.333.bak");
    let other = dir.join("notes.tmp");
    for p in [&old, &fresh, &lookalike, &other] {
        fs::write(p, b"x").unwrap();
    }
    aged(&old, Duration::from_secs(3600));
    future(&fresh);
    aged(&lookalike, Duration::from_secs(3600));
    aged(&other, Duration::from_secs(3600));

    let target = tmp.path().join("precious");
    fs::write(&target, b"keep").unwrap();
    aged(&target, Duration::from_secs(3600));
    let link = dir.join("tls.pem.444.tmp");
    symlink(&target, &link).unwrap();
    // Old by age, so only the regular-file guard keeps it.
    aged(&link, Duration::from_secs(3600));

    let out = acd(&dir).arg("init").output().unwrap();
    assert!(out.status.success(), "init failed: {}", String::from_utf8_lossy(&out.stderr));

    assert!(!old.exists(), "an old temp must be removed");
    assert!(fresh.exists(), "a fresh temp may belong to a rotation in progress");
    assert!(lookalike.exists());
    assert!(other.exists());
    assert!(link.symlink_metadata().is_ok(), "a symlink is never followed or removed");
    assert_eq!(fs::read(&target).unwrap(), b"keep");
}
