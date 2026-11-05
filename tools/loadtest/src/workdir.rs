//! Scratch directory handling. The tool deletes only a child directory it created itself.
use std::path::{Path, PathBuf};

const MARKER: &str = ".acd-loadtest-work";

/// Whether `dir` carries a marker that is a regular file. A symlink (even to a regular file) is
/// not accepted and never followed.
fn marker_state(dir: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(dir.join(MARKER)) {
        Ok(m) if m.file_type().is_file() => Ok(true),
        Ok(_) => Err(format!("refusing {}: {MARKER} is not a regular file", dir.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("marker in {}: {}", dir.display(), e.kind())),
    }
}

/// Writes the marker only when none exists (O_EXCL, so it never follows a link or truncates).
fn write_marker(dir: &Path, text: &[u8]) -> Result<(), String> {
    use std::io::Write;
    if marker_state(dir)? {
        return Ok(());
    }
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join(MARKER))
        .map_err(|e| format!("marker: {}", e.kind()))?;
    f.write_all(text).map_err(|e| format!("marker: {}", e.kind()))
}

/// Creates `path` fresh: an existing file or link at the path is unlinked (never followed),
/// then the file is created with O_EXCL and mode 0600. A directory there is refused.
pub fn create_fresh(path: &Path) -> Result<std::fs::File, String> {
    use std::os::unix::fs::OpenOptionsExt;
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("refusing {}: {}", path.display(), e.kind())),
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("{}: {}", path.display(), e.kind()))
}

/// Writes `body` to `path` through a uniquely named temp file created with O_EXCL (a
/// pre-existing or linked temp name is refused, never followed), then renames it over `path`.
/// The rename replaces a link at `path` itself and never writes through it.
pub fn write_atomic(path: &Path, body: &[u8]) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    static N: AtomicU64 = AtomicU64::new(0);
    let name = path.file_name().ok_or("no file name")?.to_string_lossy().into_owned();
    let tmp = path.with_file_name(format!(
        ".{name}.{}.{}.{}.tmp",
        std::process::id(),
        crate::rng::now_us(),
        N.fetch_add(1, Relaxed)
    ));
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| format!("{}: {}", tmp.display(), e.kind()))?;
    let done = f.write_all(body).and_then(|()| f.flush());
    drop(f);
    if let Err(e) = done.and_then(|()| std::fs::rename(&tmp, path)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("{}: {}", path.display(), e.kind()));
    }
    Ok(())
}

fn real_config_dir() -> Option<PathBuf> {
    let d = std::env::var_os("ACD_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".config/acd")))?;
    std::fs::canonicalize(d).ok()
}

/// Checks that `dir` is a harness-owned run directory: a real directory (not a link), owned by
/// the current user, not the real acd configuration directory, with a regular marker file.
/// It changes nothing.
pub fn validate(dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::symlink_metadata(dir).map_err(|e| format!("{}: {}", dir.display(), e.kind()))?;
    if !m.file_type().is_dir() {
        return Err(format!("refusing {}: not a real directory (a symlink is never followed)", dir.display()));
    }
    // SAFETY: geteuid has no preconditions.
    if m.uid() != unsafe { libc::geteuid() } {
        return Err(format!("refusing {}: not owned by the current user", dir.display()));
    }
    if let (Ok(c), Some(real)) = (std::fs::canonicalize(dir), real_config_dir())
        && c == real
    {
        return Err(format!("refusing {}: it is the real acd configuration directory", dir.display()));
    }
    if !marker_state(dir)? {
        return Err(format!("refusing {}: no {MARKER} marker; it was not created by acd-loadtest", dir.display()));
    }
    Ok(())
}

/// Creates `dir` (with parents) and marks it, or accepts one that already validates. An existing
/// directory without the marker is refused, empty or not.
pub fn claim(dir: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(dir) {
        Ok(_) => validate(dir),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = dir.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {}", parent.display(), e.kind()))?;
            }
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new().mode(0o700).create(dir).map_err(|e| format!("create {}: {}", dir.display(), e.kind()))?;
            write_marker(dir, b"acd-loadtest run directory\n")?;
            validate(dir)
        }
        Err(e) => Err(format!("{}: {}", dir.display(), e.kind())),
    }
}

/// A new, unique, marked directory for a direct `host` run (never an existing one).
pub fn unique_host_dir() -> Result<PathBuf, String> {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("acd-loadtest-host-{}-{nanos}", std::process::id()));
    claim(&dir)?;
    Ok(dir)
}

/// Returns a fresh `<work>/loadtest-<pid>` directory. `work` is created if missing; if it exists
/// and is not empty it must carry the marker this tool writes, otherwise the run is refused and
/// nothing is deleted. Only the child (which must also carry the marker) is ever removed.
pub fn prepare(work: &Path) -> Result<PathBuf, String> {
    match std::fs::read_dir(work) {
        Ok(mut it) => {
            if it.next().is_some() && !marker_state(work)? {
                return Err(format!(
                    "refusing --work {}: it exists, is not empty and was not created by acd-loadtest (no {MARKER} marker); pass an empty or new directory",
                    work.display()
                ));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("--work {}: {}", work.display(), e.kind())),
    }
    std::fs::create_dir_all(work).map_err(|e| format!("create {}: {}", work.display(), e.kind()))?;
    write_marker(work, b"acd-loadtest scratch root\n")?;
    let child = work.join(format!("loadtest-{}", std::process::id()));
    if std::fs::symlink_metadata(&child).is_ok() {
        if !child.is_dir() || child.is_symlink() || !marker_state(&child)? {
            return Err(format!("refusing to clear {}: no {MARKER} marker", child.display()));
        }
        std::fs::remove_dir_all(&child).map_err(|e| format!("clear {}: {}", child.display(), e.kind()))?;
    }
    std::fs::create_dir_all(&child).map_err(|e| format!("create {}: {}", child.display(), e.kind()))?;
    write_marker(&child, b"acd-loadtest run directory\n")?;
    Ok(child)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lt-workdir-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn an_unrelated_file_in_a_non_empty_work_dir_is_refused_and_survives() {
        let d = scratch("unrelated");
        std::fs::write(d.join("precious.txt"), b"keep").unwrap();
        assert!(prepare(&d).is_err());
        assert_eq!(std::fs::read(d.join("precious.txt")).unwrap(), b"keep");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_marked_work_dir_is_reused_and_only_the_child_is_cleared() {
        let d = scratch("marked");
        let c1 = prepare(&d).unwrap();
        std::fs::write(c1.join("stale"), b"x").unwrap();
        std::fs::write(d.join("sibling.txt"), b"keep").unwrap();
        let c2 = prepare(&d).unwrap();
        assert_eq!(c1, c2);
        assert!(!c2.join("stale").exists());
        assert_eq!(std::fs::read(d.join("sibling.txt")).unwrap(), b"keep");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_unmarked_child_is_not_cleared() {
        let d = scratch("child");
        let child = d.join(format!("loadtest-{}", std::process::id()));
        std::fs::create_dir_all(&child).unwrap();
        std::fs::write(child.join("mine.txt"), b"keep").unwrap();
        // The parent is validly marked, so only the child guard can refuse.
        std::fs::write(d.join(MARKER), b"acd-loadtest scratch root\n").unwrap();
        assert!(prepare(&d).is_err());
        assert!(child.join("mine.txt").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_marker_is_refused_and_its_target_survives() {
        let d = scratch("symlink");
        let target = d.join("valuable.txt");
        std::fs::write(&target, b"keep").unwrap();
        std::os::unix::fs::symlink(&target, d.join(MARKER)).unwrap();
        assert!(prepare(&d).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"keep");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_existing_marker_is_not_overwritten() {
        let d = scratch("keepmarker");
        std::fs::write(d.join(MARKER), b"mine").unwrap();
        prepare(&d).unwrap();
        assert_eq!(std::fs::read(d.join(MARKER)).unwrap(), b"mine");
        let _ = std::fs::remove_dir_all(&d);
    }
    #[test]
    fn a_symlinked_temp_name_never_truncates_its_target() {
        let d = std::env::temp_dir().join(format!("lt-wa-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let target = d.join("valuable");
        std::fs::write(&target, b"keep").unwrap();
        // The old fixed temp name, and a link planted at the final name.
        std::os::unix::fs::symlink(&target, d.join("sizes.json.tmp")).unwrap();
        std::os::unix::fs::symlink(&target, d.join("sizes.json")).unwrap();
        write_atomic(&d.join("sizes.json"), b"{}").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"keep");
        assert_eq!(std::fs::read(d.join("sizes.json")).unwrap(), b"{}");
        std::os::unix::fs::symlink(&target, d.join("soak.csv")).unwrap();
        use std::io::Write;
        create_fresh(&d.join("soak.csv")).unwrap().write_all(b"h").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"keep");
        let _ = std::fs::remove_dir_all(&d);
    }
}
