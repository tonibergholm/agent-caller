use acd_securefs::*;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    time::{Duration, Instant},
};

fn private_file(body: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
    let d = tempfile::tempdir().unwrap();
    fs::set_permissions(d.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let p = d.path().join("s");
    fs::write(&p, body).unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).unwrap();
    (d, p)
}

#[test]
fn reads_a_private_file() {
    let (_d, p) = private_file(b"hello");
    assert_eq!(read_private(&p, 0o600, 64).unwrap(), b"hello");
}

#[test]
fn refuses_symlink_fifo_loose_mode_and_oversized() {
    let (d, p) = private_file(b"x");
    let link = d.path().join("link");
    std::os::unix::fs::symlink(&p, &link).unwrap();
    assert!(matches!(
        read_private(&link, 0o600, 64),
        Err(SecureFsError::Symlink(_))
    ));

    let fifo = d.path().join("fifo");
    let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: valid NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    let t = Instant::now();
    assert!(matches!(
        read_private(&fifo, 0o600, 64),
        Err(SecureFsError::NotRegular(_))
    ));
    assert!(
        t.elapsed() < Duration::from_secs(1),
        "never blocks on a FIFO"
    );

    fs::set_permissions(&p, fs::Permissions::from_mode(0o640)).unwrap();
    assert!(matches!(
        read_private(&p, 0o600, 64),
        Err(SecureFsError::Mode { .. })
    ));

    let (_d2, big) = private_file(&vec![b'A'; 10_000]);
    assert!(matches!(
        read_private(&big, 0o600, 4096),
        Err(SecureFsError::TooLarge(_))
    ));
}

#[test]
fn owner_predicate_and_dir_check() {
    assert!(owner_ok(501, 501) && !owner_ok(0, 501));
    let d = tempfile::tempdir().unwrap();
    fs::set_permissions(d.path(), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        check_private_dir(d.path(), 0o700),
        Err(SecureFsError::Mode { .. })
    ));
}
