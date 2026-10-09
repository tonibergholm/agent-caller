//! The local machine (decision D2): `acd.sock` for transport, `client.token`
//! read on every connect, through the shared descriptor-based checker.
use std::path::{Path, PathBuf};

use acd_securefs::{self as securefs, SecureFsError};

pub fn default_config_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("ACD_CONFIG_DIR") {
        return Some(PathBuf::from(d));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config").join("acd"))
}

pub fn socket_path(dir: &Path) -> PathBuf {
    dir.join("acd.sock")
}

#[derive(Debug, thiserror::Error)]
pub enum LocalError {
    #[error("acd is not set up on this Mac ({0} is missing)")]
    Missing(PathBuf),
    #[error("{0} is a symlink; refusing it")]
    Symlink(PathBuf),
    #[error("{0} is not a regular file or directory as expected")]
    NotRegular(PathBuf),
    #[error("{0} is not owned by you")]
    Owner(PathBuf),
    #[error(
        "{path} has mode {mode:o}, which other users can read. Fix with: chmod {want:o} {path}"
    )]
    Mode { path: PathBuf, mode: u32, want: u32 },
    #[error("{0} does not hold a valid token")]
    Malformed(PathBuf),
    #[error("reading {0}: {1}")]
    Io(PathBuf, std::io::Error),
}

impl From<SecureFsError> for LocalError {
    fn from(e: SecureFsError) -> Self {
        match e {
            SecureFsError::Missing(p) => LocalError::Missing(p),
            SecureFsError::Symlink(p) => LocalError::Symlink(p),
            SecureFsError::NotRegular(p) => LocalError::NotRegular(p),
            SecureFsError::Owner(p) => LocalError::Owner(p),
            SecureFsError::Mode { path, mode, want } => LocalError::Mode { path, mode, want },
            SecureFsError::TooLarge(p) => LocalError::Malformed(p),
            SecureFsError::Io(p, e) => LocalError::Io(p, e),
        }
    }
}

pub use securefs::owner_ok;

pub fn read_local_token(dir: &Path) -> Result<String, LocalError> {
    securefs::check_private_dir(dir, 0o700)?;
    let path = dir.join("client.token");
    let raw = securefs::read_private(&path, 0o600, 256)?;
    let token = std::str::from_utf8(&raw)
        .map_err(|_| LocalError::Malformed(path.clone()))?
        .trim();
    if !crate::settings::valid_token(token) {
        return Err(LocalError::Malformed(path));
    }
    Ok(token.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    const TOKEN: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8";

    fn setup() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        fs::set_permissions(d.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let t = d.path().join("client.token");
        fs::write(&t, format!("{TOKEN}\n")).unwrap();
        fs::set_permissions(&t, fs::Permissions::from_mode(0o600)).unwrap();
        d
    }

    #[test]
    fn reads_a_private_token() {
        assert_eq!(read_local_token(setup().path()).unwrap(), TOKEN);
    }

    #[test]
    fn refuses_loose_modes_symlinks_missing_and_garbage() {
        let d = setup();
        fs::set_permissions(
            d.path().join("client.token"),
            fs::Permissions::from_mode(0o640),
        )
        .unwrap();
        assert!(matches!(
            read_local_token(d.path()),
            Err(LocalError::Mode { .. })
        ));

        let d = setup();
        fs::set_permissions(d.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            read_local_token(d.path()),
            Err(LocalError::Mode { .. })
        ));

        let d = setup();
        let real = d.path().join("real");
        fs::rename(d.path().join("client.token"), &real).unwrap();
        std::os::unix::fs::symlink(&real, d.path().join("client.token")).unwrap();
        assert!(matches!(
            read_local_token(d.path()),
            Err(LocalError::Symlink(_))
        ));

        let d = setup();
        fs::remove_file(d.path().join("client.token")).unwrap();
        assert!(matches!(
            read_local_token(d.path()),
            Err(LocalError::Missing(_))
        ));

        let d = setup();
        fs::write(d.path().join("client.token"), "not a token").unwrap();
        assert!(matches!(
            read_local_token(d.path()),
            Err(LocalError::Malformed(_))
        ));

        // Non-canonical: the last character carries non-zero trailing bits ('8' -> '9').
        let d = setup();
        fs::write(d.path().join("client.token"), format!("{}9", &TOKEN[..42])).unwrap();
        assert!(matches!(
            read_local_token(d.path()),
            Err(LocalError::Malformed(_))
        ));
    }

    #[test]
    fn a_fifo_is_refused_without_blocking() {
        let d = setup();
        let t = d.path().join("client.token");
        fs::remove_file(&t).unwrap();
        let c = std::ffi::CString::new(t.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: valid NUL-terminated path; mkfifo has no other preconditions.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let started = std::time::Instant::now();
        assert!(matches!(
            read_local_token(d.path()),
            Err(LocalError::NotRegular(_))
        ));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "O_NONBLOCK: no wait for a writer"
        );
    }

    #[test]
    fn owner_predicate() {
        assert!(owner_ok(501, 501));
        assert!(!owner_ok(0, 501));
    }

    #[test]
    fn oversized_file_is_refused() {
        let d = setup();
        fs::write(d.path().join("client.token"), "A".repeat(10_000)).unwrap();
        assert!(matches!(
            read_local_token(d.path()),
            Err(LocalError::Malformed(_))
        ));
    }
}
