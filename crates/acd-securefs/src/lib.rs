#[cfg(owner_checks)]
use std::{
    fs::File,
    io::{Read, Take},
    os::fd::FromRawFd,
};
use std::{
    fmt,
    path::{Path, PathBuf},
};

mod platform;
pub use platform::platform_supports_owner_checks;

/// True when this build has the real owner-only checks (`cfg(owner_checks)`):
/// unix except Android. Everywhere else every check refuses.
pub const OWNER_CHECKS: bool = cfg!(owner_checks);

#[derive(Debug)]
pub enum SecureFsError {
    Missing(PathBuf),
    Symlink(PathBuf),
    NotRegular(PathBuf),
    Owner(PathBuf),
    Mode { path: PathBuf, mode: u32, want: u32 },
    TooLarge(PathBuf),
    Io(PathBuf, std::io::Error),
}

impl fmt::Display for SecureFsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(p) => write!(f, "{} is missing", p.display()),
            Self::Symlink(p) => write!(f, "{} is a symlink", p.display()),
            Self::NotRegular(p) => write!(f, "{} is not the expected file type", p.display()),
            Self::Owner(p) => write!(f, "{} has the wrong owner", p.display()),
            Self::Mode { path, mode, want } => write!(
                f,
                "{} has mode {:o}; expected a subset of {:o}",
                path.display(),
                mode,
                want
            ),
            Self::TooLarge(p) => write!(f, "{} is too large", p.display()),
            Self::Io(p, e) => write!(f, "{}: {e}", p.display()),
        }
    }
}
impl std::error::Error for SecureFsError {}

/// Supported unix only (not Android). There is no `euid` elsewhere: a made-up user id would let a caller
/// believe an owner check had happened.
#[cfg(owner_checks)]
pub fn euid() -> u32 {
    unsafe { libc::geteuid() }
}
pub fn owner_ok(file_uid: u32, expected_uid: u32) -> bool {
    file_uid == expected_uid
}

#[cfg(owner_checks)]
/// Decide, from lstat facts, whether a permission-denied open is really a
/// symlink or wrong-owner problem. Diagnostic only: never grants access.
fn classify_denied(
    path: &Path,
    meta_uid: u32,
    expected_uid: u32,
    is_symlink: bool,
) -> Option<SecureFsError> {
    if is_symlink {
        Some(SecureFsError::Symlink(path.into()))
    } else if !owner_ok(meta_uid, expected_uid) {
        Some(SecureFsError::Owner(path.into()))
    } else {
        None
    }
}

#[cfg(owner_checks)]
fn diagnose_denied(path: &Path, open_err: std::io::Error) -> SecureFsError {
    use std::os::unix::fs::MetadataExt;
    match std::fs::symlink_metadata(path) {
        Ok(m) => classify_denied(path, m.uid(), euid(), m.file_type().is_symlink())
            .unwrap_or_else(|| SecureFsError::Io(path.into(), open_err)),
        Err(_) => SecureFsError::Io(path.into(), open_err),
    }
}

#[cfg(owner_checks)]
fn open_path(path: &Path, directory: bool) -> Result<File, SecureFsError> {
    use std::os::unix::ffi::OsStrExt;
    let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        SecureFsError::Io(
            path.into(),
            std::io::Error::from(std::io::ErrorKind::InvalidInput),
        )
    })?;
    let mut flags = libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC;
    if directory {
        flags |= libc::O_DIRECTORY;
    }
    // SAFETY: cpath is NUL terminated and flags request a descriptor only.
    let fd = unsafe { libc::open(cpath.as_ptr(), flags) };
    if fd < 0 {
        let e = std::io::Error::last_os_error();
        return Err(match e.raw_os_error() {
            Some(libc::ELOOP) => SecureFsError::Symlink(path.into()),
            Some(libc::ENOENT) => SecureFsError::Missing(path.into()),
            Some(libc::ENOTDIR) => SecureFsError::NotRegular(path.into()),
            _ if e.kind() == std::io::ErrorKind::PermissionDenied => diagnose_denied(path, e),
            _ => SecureFsError::Io(path.into(), e),
        });
    }
    // SAFETY: fd is newly returned and ownership transfers to File.
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(owner_checks)]
fn verify(file: &File, path: &Path, directory: bool, max_mode: u32) -> Result<(), SecureFsError> {
    let m = file
        .metadata()
        .map_err(|e| SecureFsError::Io(path.into(), e))?;
    if (directory && !m.is_dir()) || (!directory && !m.is_file()) {
        return Err(SecureFsError::NotRegular(path.into()));
    }
    use std::os::unix::fs::MetadataExt;
    if !owner_ok(m.uid(), euid()) {
        return Err(SecureFsError::Owner(path.into()));
    }
    let mode = m.mode() & 0o7777;
    if mode & !max_mode != 0 {
        return Err(SecureFsError::Mode {
            path: path.into(),
            mode,
            want: max_mode,
        });
    }
    Ok(())
}

#[cfg(owner_checks)]
pub fn check_private_dir(dir: &Path, max_mode: u32) -> Result<(), SecureFsError> {
    let file = open_path(dir, true)?;
    verify(&file, dir, true, max_mode)
}

/// No-follow open, regular file, owned by the euid, mode within `max_mode`.
#[cfg(owner_checks)]
pub fn check_private_file(path: &Path, max_mode: u32) -> Result<(), SecureFsError> {
    let file = open_path(path, false)?;
    verify(&file, path, false, max_mode)
}

/// No-follow, non-blocking open of a regular file owned by the euid, with no
/// mode requirement. The returned descriptor is what callers should inspect
/// and `fchmod`, so nothing can be swapped in between check and use.
#[cfg(owner_checks)]
pub fn open_owned_file(path: &Path) -> Result<File, SecureFsError> {
    let file = open_path(path, false)?;
    verify(&file, path, false, 0o7777)?;
    Ok(file)
}

#[cfg(owner_checks)]
pub fn read_private(path: &Path, max_mode: u32, max_len: u64) -> Result<Vec<u8>, SecureFsError> {
    let mut file = open_path(path, false)?;
    verify(&file, path, false, max_mode)?;
    let cap = max_len.saturating_add(1);
    let mut reader: Take<&mut File> = (&mut file).take(cap);
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|e| SecureFsError::Io(path.into(), e))?;
    if bytes.len() as u64 > max_len {
        return Err(SecureFsError::TooLarge(path.into()));
    }
    Ok(bytes)
}

/// The error every owner-only check returns where this crate has no
/// implementation. It is an `Io` error of kind `Unsupported`, so callers that
/// already treat `Io` as a failure keep failing closed.
// Built on every target so the Linux tests can pin its text; only the
// refusing functions below call it.
#[cfg_attr(owner_checks, allow(dead_code))]
fn unsupported(path: &Path) -> SecureFsError {
    SecureFsError::Io(
        path.into(),
        std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "owner-only file checks are unsupported on this platform",
        ),
    )
}

// shortcut: every owner-only check refuses where `cfg(owner_checks)` is unset (Windows, Android),
// upgrade when a Windows client needs to read acd's local files or the acd
// daemon itself is ported: open with FILE_FLAG_OPEN_REPARSE_POINT, check the
// owner SID and that the DACL grants nobody else (see the feasibility note,
// section B4). Until then a caller gets an error, never "ok".
#[cfg(not(owner_checks))]
pub fn check_private_dir(dir: &Path, _max_mode: u32) -> Result<(), SecureFsError> {
    Err(unsupported(dir))
}

#[cfg(not(owner_checks))]
pub fn check_private_file(path: &Path, _max_mode: u32) -> Result<(), SecureFsError> {
    Err(unsupported(path))
}

/// Refusing targets: always refuses. Returns `std::fs::File` on unix.
#[cfg(not(owner_checks))]
pub fn open_owned_file(path: &Path) -> Result<std::fs::File, SecureFsError> {
    Err(unsupported(path))
}

#[cfg(not(owner_checks))]
pub fn read_private(path: &Path, _max_mode: u32, _max_len: u64) -> Result<Vec<u8>, SecureFsError> {
    Err(unsupported(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_is_an_unsupported_io_error() {
        let err = unsupported(Path::new("/x"));
        assert!(
            matches!(&err, SecureFsError::Io(_, e) if e.kind() == std::io::ErrorKind::Unsupported),
            "{err:?}"
        );
        assert!(err.to_string().contains("unsupported on this platform"));
    }

    /// The decision itself, for every target, on any host.
    #[test]
    fn android_and_windows_refuse_and_the_other_unixes_do_not() {
        for (os, unix, want) in [
            ("linux", true, true),
            ("macos", true, true),
            ("ios", true, true),
            ("freebsd", true, true),
            ("android", true, false),
            ("windows", false, false),
        ] {
            assert_eq!(platform_supports_owner_checks(os, unix), want, "{os}");
        }
    }

    /// The compiled cfg agrees with the rule for the target being built.
    #[test]
    fn the_compiled_cfg_follows_the_rule() {
        assert_eq!(
            OWNER_CHECKS,
            platform_supports_owner_checks(std::env::consts::OS, cfg!(unix))
        );
    }

    /// Runs on Windows and Android, where this is the whole implementation.
    #[cfg(not(owner_checks))]
    #[test]
    fn every_check_refuses_even_for_a_good_file() {
        let dir = std::env::temp_dir();
        let f = dir.join("acd-securefs-unsupported-probe");
        std::fs::write(&f, b"x").unwrap();
        assert!(check_private_dir(&dir, 0o700).is_err());
        assert!(check_private_file(&f, 0o600).is_err());
        assert!(open_owned_file(&f).is_err());
        assert!(read_private(&f, 0o600, 16).is_err());
        let _ = std::fs::remove_file(&f);
    }

    /// Android has `cfg(unix)` but must still refuse. Not run in the Linux
    /// sandbox; type-checked with `--target aarch64-linux-android --tests`.
    #[cfg(target_os = "android")]
    #[test]
    fn android_takes_the_refusing_path() {
        assert!(cfg!(unix) && !OWNER_CHECKS);
        let f = std::env::temp_dir().join("acd-securefs-android-probe");
        std::fs::write(&f, b"x").unwrap();
        let err = read_private(&f, 0o600, 16).unwrap_err();
        assert!(matches!(&err, SecureFsError::Io(_, e) if e.kind() == std::io::ErrorKind::Unsupported));
        let _ = std::fs::remove_file(&f);
    }

    #[cfg(owner_checks)]
    #[test]
    fn classify_denied_decisions() {
        let p = Path::new("/x");
        assert!(matches!(
            classify_denied(p, 0, 501, false),
            Some(SecureFsError::Owner(_))
        ));
        assert!(matches!(
            classify_denied(p, 501, 501, true),
            Some(SecureFsError::Symlink(_))
        ));
        assert!(matches!(
            classify_denied(p, 0, 501, true),
            Some(SecureFsError::Symlink(_))
        ));
        assert!(classify_denied(p, 501, 501, false).is_none());
    }

    #[cfg(owner_checks)]
    #[test]
    fn denied_open_of_own_file_stays_io() {
        use std::os::unix::fs::PermissionsExt;
        if euid() == 0 {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("t");
        std::fs::write(&f, b"x").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o000)).unwrap();
        let err = read_private(&f, 0o600, 16).unwrap_err();
        assert!(
            matches!(&err, SecureFsError::Io(_, e) if e.kind() == std::io::ErrorKind::PermissionDenied),
            "{err:?}"
        );
    }

    #[cfg(owner_checks)]
    #[test]
    fn denied_traverse_of_own_dir_stays_io() {
        use std::os::unix::fs::PermissionsExt;
        if euid() == 0 {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().join("d");
        std::fs::create_dir(&d).unwrap();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o000)).unwrap();
        let err = check_private_dir(&d, 0o700).unwrap_err();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            matches!(&err, SecureFsError::Io(_, e) if e.kind() == std::io::ErrorKind::PermissionDenied),
            "{err:?}"
        );
    }
}
