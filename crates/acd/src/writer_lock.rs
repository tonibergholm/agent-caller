//! `<config_dir>/hooks.lock` (contract T6). Acquired only by top-level
//! entry points; every helper that needs it takes `&WriterLock`.
use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::Path;

use anyhow::{Result, bail};

pub const LOCK_FILE: &str = "hooks.lock";

pub struct WriterLock(#[allow(dead_code)] std::fs::File);

pub fn acquire(config_dir: &Path) -> Result<WriterLock> {
    let path = config_dir.join(LOCK_FILE);
    let f = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)?;
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
