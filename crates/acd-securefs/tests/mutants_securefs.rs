//! Tests added from a cargo-mutants run (docs/research/2026-11-04-mutation-testing.md).
//! Each test names the mutant it kills.
#![cfg(owner_checks)]

use acd_securefs::*;
use std::{fs, os::unix::fs::PermissionsExt, path::Path};

fn mode(p: &Path, m: u32) {
    fs::set_permissions(p, fs::Permissions::from_mode(m)).unwrap();
}

fn private_dir() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    mode(d.path(), 0o700);
    d
}

fn private_file(d: &Path, name: &str, body: &[u8]) -> std::path::PathBuf {
    let p = d.join(name);
    fs::write(&p, body).unwrap();
    mode(&p, 0o600);
    p
}

fn chown(p: &Path, uid: u32) {
    let c = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: valid NUL-terminated path; gid -1 leaves the group alone.
    assert_eq!(unsafe { libc::chown(c.as_ptr(), uid, u32::MAX) }, 0);
}

fn is_root() -> bool {
    euid() == 0
}

/// `check_private_file -> Ok(())`
#[test]
fn check_private_file_judges_mode_symlink_and_type() {
    let d = private_dir();
    let p = private_file(d.path(), "f", b"x");
    assert!(check_private_file(&p, 0o600).is_ok());
    mode(&p, 0o640);
    assert!(matches!(check_private_file(&p, 0o600), Err(SecureFsError::Mode { .. })));
    mode(&p, 0o600);
    let link = d.path().join("l");
    std::os::unix::fs::symlink(&p, &link).unwrap();
    assert!(matches!(check_private_file(&link, 0o600), Err(SecureFsError::Symlink(_))));
    assert!(matches!(check_private_file(d.path(), 0o700), Err(SecureFsError::NotRegular(_))));
}

/// `open_path`: delete match arm ENOENT, `check_private_dir` Missing.
#[test]
fn a_missing_path_is_missing() {
    let d = private_dir();
    let p = d.path().join("nope");
    assert!(matches!(read_private(&p, 0o600, 8), Err(SecureFsError::Missing(_))));
    assert!(matches!(check_private_file(&p, 0o600), Err(SecureFsError::Missing(_))));
    assert!(matches!(check_private_dir(&p, 0o700), Err(SecureFsError::Missing(_))));
    assert!(matches!(open_owned_file(&p), Err(SecureFsError::Missing(_))));
}

/// `open_path`: delete match arm ENOTDIR (a file used as a directory), and
/// the `|=` -> `&=` flags mutant (O_DIRECTORY dropped).
#[test]
fn a_file_is_not_a_directory_and_a_directory_is_not_a_file() {
    let d = private_dir();
    let f = private_file(d.path(), "f", b"x");
    assert!(matches!(check_private_dir(&f, 0o700), Err(SecureFsError::NotRegular(_))));
    assert!(matches!(read_private(&f.join("child"), 0o600, 8), Err(SecureFsError::NotRegular(_))));
    assert!(matches!(check_private_file(d.path(), 0o700), Err(SecureFsError::NotRegular(_))));
}

/// `|=` -> `&=` in `open_path`: with the directory flag the other flags were
/// replaced by 0, so a symlinked directory was followed and accepted.
#[test]
fn a_symlinked_directory_is_refused() {
    let d = private_dir();
    let real = d.path().join("real");
    fs::create_dir(&real).unwrap();
    mode(&real, 0o700);
    assert!(check_private_dir(&real, 0o700).is_ok());
    let link = d.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    // O_NOFOLLOW|O_DIRECTORY on a symlink reports ENOTDIR on Linux, so the
    // kind is NotRegular there; what matters is that it is refused.
    assert!(matches!(
        check_private_dir(&link, 0o700),
        Err(SecureFsError::Symlink(_) | SecureFsError::NotRegular(_))
    ));
}

/// `read_private`: `>` -> `>=`. A file of exactly `max_len` bytes is allowed,
/// one byte more is not.
#[test]
fn the_length_limit_is_inclusive() {
    let d = private_dir();
    let p = private_file(d.path(), "f", &[b'A'; 16]);
    assert_eq!(read_private(&p, 0o600, 16).unwrap().len(), 16);
    assert!(matches!(read_private(&p, 0o600, 15), Err(SecureFsError::TooLarge(_))));
    let empty = private_file(d.path(), "e", b"");
    assert_eq!(read_private(&empty, 0o600, 0).unwrap(), b"");
    assert!(matches!(read_private(&p, 0o600, 0), Err(SecureFsError::TooLarge(_))));
}

/// `open_owned_file`: accepts any mode of an owned regular file, refuses
/// symlinks and other types, and returns the opened file itself.
#[test]
fn open_owned_file_returns_the_file_it_checked() {
    use std::io::Read;
    let d = private_dir();
    let p = private_file(d.path(), "f", b"abc");
    mode(&p, 0o666);
    let mut got = String::new();
    open_owned_file(&p).unwrap().read_to_string(&mut got).unwrap();
    assert_eq!(got, "abc");
    let link = d.path().join("l");
    std::os::unix::fs::symlink(&p, &link).unwrap();
    assert!(matches!(open_owned_file(&link), Err(SecureFsError::Symlink(_))));
    assert!(matches!(open_owned_file(d.path()), Err(SecureFsError::NotRegular(_))));
}

/// `check_socket_path_as`: delete match arm NotFound.
#[test]
fn a_missing_socket_path_is_missing() {
    let d = private_dir();
    assert!(matches!(check_socket_path(&d.path().join("h.sock")), Err(SecureFsError::Missing(_))));
}

/// Owner checks against a file that really belongs to someone else. Needs
/// chown, so it only runs as root (it returns early otherwise).
#[test]
fn a_file_owned_by_someone_else_is_refused() {
    if !is_root() {
        return;
    }
    let d = private_dir();
    let p = private_file(d.path(), "f", b"x");
    chown(&p, 12345);
    assert!(matches!(check_private_file(&p, 0o600), Err(SecureFsError::Owner(_))));
    assert!(matches!(read_private(&p, 0o600, 8), Err(SecureFsError::Owner(_))));
    assert!(matches!(open_owned_file(&p), Err(SecureFsError::Owner(_))));
    chown(&p, 0);
    assert!(check_private_file(&p, 0o600).is_ok());
    let sub = d.path().join("d");
    fs::create_dir(&sub).unwrap();
    mode(&sub, 0o700);
    chown(&sub, 12345);
    assert!(matches!(check_private_dir(&sub, 0o700), Err(SecureFsError::Owner(_))));
}

const CHILD: &str = "ACD_MUTANTS_SECUREFS_CHILD";

/// Runs inside the unprivileged child started by the next test; a no-op when
/// the test suite runs it directly.
#[test]
fn unprivileged_child() {
    let Ok(dir) = std::env::var(CHILD) else { return };
    let dir = std::path::PathBuf::from(dir);
    assert_ne!(euid(), 0, "the child must have dropped root");
    // Root-owned and world-readable: opens fine, but the owner is not us.
    let open = dir.join("open");
    assert!(matches!(check_private_file(&open, 0o644), Err(SecureFsError::Owner(_))));
    assert!(matches!(read_private(&open, 0o644, 8), Err(SecureFsError::Owner(_))));
    assert!(matches!(open_owned_file(&open), Err(SecureFsError::Owner(_))));
    assert!(matches!(check_private_dir(&dir, 0o755), Err(SecureFsError::Owner(_))));
    // Root-owned 0600: the open itself is denied, and the denial is
    // diagnosed as a wrong owner, not a bare I/O error.
    let secret = dir.join("secret");
    assert!(matches!(read_private(&secret, 0o600, 8), Err(SecureFsError::Owner(_))));
    assert!(matches!(check_private_file(&secret, 0o600), Err(SecureFsError::Owner(_))));
    println!("child checked");
}

/// `euid -> 0`, `PermissionDenied` guard mutants in `open_path`: with the euid
/// forced to 0 root-owned files look like ours. Needs root to drop privileges,
/// so it returns early otherwise (the mutant is then caught by every test
/// that creates a file of its own).
#[test]
fn root_owned_files_are_refused_to_an_unprivileged_user() {
    if !is_root() || std::env::var(CHILD).is_ok() {
        return;
    }
    use std::os::unix::process::CommandExt;
    let d = tempfile::tempdir().unwrap();
    mode(d.path(), 0o755);
    let open = d.path().join("open");
    fs::write(&open, b"x").unwrap();
    mode(&open, 0o644);
    private_file(d.path(), "secret", b"x");
    let exe = d.path().join("exe");
    fs::copy(std::env::current_exe().unwrap(), &exe).unwrap();
    mode(&exe, 0o755);
    let out = match std::process::Command::new(&exe)
        .args(["--exact", "unprivileged_child", "--nocapture", "--test-threads=1"])
        .env(CHILD, d.path())
        .uid(65534)
        .gid(65534)
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            eprintln!("cannot drop privileges here: {e}");
            return;
        }
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("child checked"),
        "{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// KNOWN BUG, not fixed here (docs/research/2026-11-04-mutation-testing.md): a symlinked
/// directory is refused, but as `NotRegular`, not `Symlink`. On Linux
/// `open(O_NOFOLLOW | O_DIRECTORY)` on a symlink fails with ENOTDIR, which `open_path`
/// maps to `NotRegular` before ELOOP could say `Symlink`. The refusal is correct
/// (fail closed); only the reported reason, and so the message the user reads, is wrong.
#[test]
#[ignore = "known bug: reports NotRegular for a symlinked directory"]
fn a_symlinked_directory_reports_a_symlink() {
    let d = private_dir();
    let real = d.path().join("real");
    fs::create_dir(&real).unwrap();
    mode(&real, 0o700);
    let link = d.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert!(matches!(check_private_dir(&link, 0o700), Err(SecureFsError::Symlink(_))));
}
