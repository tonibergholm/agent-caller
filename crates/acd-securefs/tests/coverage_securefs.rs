//! Coverage-driven tests (docs/research/2026-11-05-coverage.md).
use acd_securefs::{check_socket_path_as, euid, read_private, SecureFsError, OWNER_CHECKS};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

/// The text is what an operator reads in `acd doctor`; each variant must name
/// the path and the reason, and the mode error must print octal.
#[test]
fn every_error_names_its_path_and_reason() {
    let p = || PathBuf::from("/x/secret");
    let cases = [
        (SecureFsError::Missing(p()), "/x/secret is missing"),
        (SecureFsError::Symlink(p()), "/x/secret is a symlink"),
        (SecureFsError::NotRegular(p()), "/x/secret is not the expected file type"),
        (SecureFsError::Owner(p()), "/x/secret has the wrong owner"),
        (
            SecureFsError::Mode { path: p(), mode: 0o644, want: 0o600 },
            "/x/secret has mode 644; expected a subset of 600",
        ),
        (SecureFsError::TooLarge(p()), "/x/secret is too large"),
    ];
    for (e, want) in cases {
        assert_eq!(e.to_string(), want);
    }
    let io = SecureFsError::Io(p(), std::io::Error::other("boom"));
    assert_eq!(io.to_string(), "/x/secret: boom");
}

#[test]
fn a_regular_file_is_not_a_socket_and_a_foreign_euid_is_refused() {
    if !OWNER_CHECKS {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let file = dir.path().join("not-a-socket");
    std::fs::write(&file, b"x").unwrap();
    // Fail closed: a plain file never passes as the socket.
    assert!(matches!(check_socket_path_as(&file, euid()), Err(SecureFsError::Owner(_))));

    let sock = dir.path().join("s.sock");
    let _l = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    let canon = check_socket_path_as(&sock, euid()).expect("our own socket passes");
    assert_eq!(canon, std::fs::canonicalize(&sock).unwrap());
    // The same socket judged for another user is not trusted.
    assert!(matches!(check_socket_path_as(&sock, euid() + 1), Err(SecureFsError::Owner(_))));
}

#[test]
fn a_file_one_byte_over_the_limit_is_too_large_even_when_the_mode_is_fine() {
    if !OWNER_CHECKS {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let f = dir.path().join("t");
    std::fs::write(&f, b"abcd").unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(matches!(read_private(&f, 0o600, 3), Err(SecureFsError::TooLarge(_))));
    assert_eq!(read_private(&f, 0o600, 4).unwrap(), b"abcd");
}
