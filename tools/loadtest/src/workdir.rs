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
}
