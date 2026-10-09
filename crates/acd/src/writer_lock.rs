//! `<config_dir>/hooks.lock` (contract T6). Acquired only by top-level
//! entry points; every helper that needs it takes `&WriterLock`.
use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::Path;

use anyhow::{Context, Result, bail};
use std::os::unix::fs::{MetadataExt, PermissionsExt};

pub const LOCK_FILE: &str = "hooks.lock";

pub struct WriterLock(#[allow(dead_code)] std::fs::File);

/// Checks the opened descriptor (not the path): a regular file owned by the
/// effective uid. A looser mode on an owned file is repaired via the descriptor.
pub(crate) fn validate_descriptor(f: &std::fs::File, path: &Path) -> Result<()> {
    let m = f
        .metadata()
        .with_context(|| format!("could not inspect {}", path.display()))?;
    if !m.file_type().is_file() {
        bail!(
            "{} is not a regular file; remove it and retry",
            path.display()
        );
    }
    // SAFETY: geteuid has no preconditions.
    if m.uid() != unsafe { libc::geteuid() } {
        bail!(
            "{} is owned by another user (uid {}); remove it and retry",
            path.display(),
            m.uid()
        );
    }
    // Any bit beyond owner read/write, special bits included, is repaired.
    if m.mode() & 0o7777 != 0o600 {
        f.set_permissions(std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("could not restrict {} to 0600", path.display()))?;
    }
    Ok(())
}

pub fn acquire(config_dir: &Path) -> Result<WriterLock> {
    let path = config_dir.join(LOCK_FILE);
    let f = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&path)
        .with_context(|| format!("could not open {} (must be a regular file)", path.display()))?;
    validate_descriptor(&f, &path)?;
    // SAFETY: a valid open descriptor; flock has no other preconditions.
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(WriterLock(f));
    }
    #[cfg(test)]
    tests_support::mark_contended(&path);
    // SAFETY: as above.
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } != 0 {
        bail!("could not lock {}", path.display());
    }
    Ok(WriterLock(f))
}

#[cfg(test)]
pub mod tests_support {
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    static CONTENDED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

    pub(super) fn mark_contended(path: &Path) {
        CONTENDED
            .lock()
            .expect("contended")
            .push(path.to_path_buf());
    }

    #[allow(dead_code)] // consumed by later lock contention tests
    pub fn contended(path: &Path) -> bool {
        CONTENDED
            .lock()
            .expect("contended")
            .iter()
            .any(|p| p == path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    fn mode(p: &Path) -> u32 {
        std::fs::metadata(p).unwrap().mode() & 0o7777
    }

    #[test]
    fn a_fifo_is_rejected_without_blocking() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join(LOCK_FILE);
        let c = CString::new(p.as_os_str().as_bytes()).unwrap();
        // SAFETY: valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let dir = d.path().to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(acquire(&dir).err().map(|e| format!("{e:#}")));
        });
        let e = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("did not block")
            .expect("rejected");
        assert!(e.contains(LOCK_FILE) && e.contains("regular file"), "{e}");
    }

    #[test]
    fn a_symlink_is_rejected() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("target");
        std::fs::write(&target, b"").unwrap();
        std::os::unix::fs::symlink(&target, d.path().join(LOCK_FILE)).unwrap();
        let e = format!("{:#}", acquire(d.path()).err().expect("rejected"));
        assert!(e.contains(LOCK_FILE), "{e}");
    }

    #[test]
    fn a_loose_mode_is_repaired_and_a_new_file_is_private() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join(LOCK_FILE);
        drop(acquire(d.path()).unwrap());
        assert_eq!(mode(&p), 0o600);
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o666)).unwrap();
        drop(acquire(d.path()).unwrap());
        assert_eq!(mode(&p), 0o600);
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o4600)).unwrap();
        drop(acquire(d.path()).unwrap());
        assert_eq!(mode(&p), 0o600);
    }
}
