//! Coverage-driven tests (docs/research/2026-11-05-coverage.md).
mod common;
use common::acd;
use std::fs::{self, File};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::time::{Duration, SystemTime};

fn aged(path: &std::path::Path, age: Duration) {
    let f = File::options().write(true).open(path).unwrap();
    f.set_modified(SystemTime::now() - age).unwrap();
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
    aged(&lookalike, Duration::from_secs(3600));
    aged(&other, Duration::from_secs(3600));

    let target = tmp.path().join("precious");
    fs::write(&target, b"keep").unwrap();
    aged(&target, Duration::from_secs(3600));
    let link = dir.join("tls.pem.444.tmp");
    symlink(&target, &link).unwrap();

    let out = acd(&dir).arg("init").output().unwrap();
    assert!(out.status.success(), "init failed: {}", String::from_utf8_lossy(&out.stderr));

    assert!(!old.exists(), "an old temp must be removed");
    assert!(fresh.exists(), "a fresh temp may belong to a rotation in progress");
    assert!(lookalike.exists());
    assert!(other.exists());
    assert!(link.symlink_metadata().is_ok(), "a symlink is never followed or removed");
    assert_eq!(fs::read(&target).unwrap(), b"keep");
}
