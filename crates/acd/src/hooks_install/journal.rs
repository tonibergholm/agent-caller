//! Settings and record updates are journaled so interrupted writes can recover.
use super::{FileRecord, Record};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub const JOURNAL_FILE: &str = "hooks-journal.json";
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Journal {
    pub destination: PathBuf,
    pub before: Option<String>,
    pub after: Option<String>,
    pub record_after: Option<FileRecord>,
}
pub fn digest(text: Option<&str>) -> Option<String> {
    text.map(|t| {
        Sha256::digest(t.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    })
}
/// J1: refuses to overwrite a journal, which may be the only evidence of an
/// interrupted write. The caller holds the writer lock.
pub fn begin(config_dir: &Path, j: &Journal) -> Result<()> {
    let path = config_dir.join(JOURNAL_FILE);
    if path.symlink_metadata().is_ok() {
        bail!(
            "{} exists: an earlier acd hooks command was interrupted. Run `acd hooks install` or `uninstall` to recover it, or inspect it first",
            path.display()
        );
    }
    crate::secrets::write_structured(
        &config_dir.join(JOURNAL_FILE),
        &serde_json::to_string_pretty(j)?,
    )
}
pub fn finish(config_dir: &Path) -> Result<()> {
    match std::fs::remove_file(config_dir.join(JOURNAL_FILE)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}
/// J2 for the cases Task 24 can settle. The journal is deleted only when the
/// settings match the `before` digest (rolled back) or the `after` digest
/// (committed; the record is then saved). Anything else, including a file
/// that cannot be read, keeps the journal and fails; Task 25 reconstructs.
pub fn recover(config_dir: &Path) -> Result<()> {
    let path = config_dir.join(JOURNAL_FILE);
    if path.symlink_metadata().is_err() {
        return Ok(());
    }
    let j: Journal = serde_json::from_str(&crate::secrets::read_structured(&path)?)
        .map_err(|e| anyhow::anyhow!("{} is not valid: {e}", path.display()))?;
    let conflict = |why: String| {
        anyhow::anyhow!(
            "conflict: {} was changed after an interrupted acd hooks command ({why}). The journal {} was kept; fix the file, then retry",
            j.destination.display(),
            path.display()
        )
    };
    let now = match super::read_settings(&j.destination) {
        Ok(text) => digest(text.as_deref()),
        Err(e) => return Err(conflict(format!("{e:#}"))),
    };
    if now == j.after {
        let mut record = Record::load(config_dir)?;
        match j.record_after {
            Some(r) => {
                record.files.insert(j.destination.clone(), r);
            }
            None => {
                record.files.remove(&j.destination);
            }
        }
        record.save(config_dir)?;
    } else if now != j.before {
        return Err(conflict(
            "it matches neither the old nor the new content".into(),
        ));
    }
    finish(config_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks_install::Target;

    fn setup() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let cfg = t.path().join("cfg");
        crate::secrets::ensure_private_dir(&cfg).unwrap();
        let cfg = std::fs::canonicalize(cfg).unwrap();
        let dest = std::fs::canonicalize(t.path())
            .unwrap()
            .join("settings.json");
        (t, cfg, dest)
    }

    fn journal(dest: &Path, before: Option<&str>, after: Option<&str>) -> Journal {
        Journal {
            destination: dest.to_path_buf(),
            before: digest(before),
            after: digest(after),
            record_after: Some(FileRecord {
                target: Target::Claude,
                created_file: false,
                created_hooks_key: false,
                created_events: vec![],
            }),
        }
    }

    #[test]
    fn begin_refuses_to_overwrite_a_journal() {
        let (_t, cfg, dest) = setup();
        begin(&cfg, &journal(&dest, Some("a"), Some("b"))).unwrap();
        let first = std::fs::read(cfg.join(JOURNAL_FILE)).unwrap();
        let err = begin(&cfg, &journal(&dest, Some("c"), Some("d"))).unwrap_err();
        assert!(format!("{err:#}").contains(JOURNAL_FILE), "{err:#}");
        assert_eq!(std::fs::read(cfg.join(JOURNAL_FILE)).unwrap(), first);
    }

    #[test]
    fn recover_keeps_the_journal_when_settings_match_neither_digest() {
        let (_t, cfg, dest) = setup();
        begin(&cfg, &journal(&dest, Some("old"), Some("new"))).unwrap();
        std::fs::write(&dest, "something else").unwrap();
        let err = recover(&cfg).unwrap_err();
        assert!(format!("{err:#}").contains("conflict"), "{err:#}");
        assert!(cfg.join(JOURNAL_FILE).exists());
        assert!(!cfg.join(super::super::RECORD_FILE).exists());
        // An unsafe file is the same conflict.
        std::fs::remove_file(&dest).unwrap();
        std::os::unix::fs::symlink("/etc/hosts", &dest).unwrap();
        assert!(recover(&cfg).is_err());
        assert!(cfg.join(JOURNAL_FILE).exists());
    }

    #[test]
    fn recover_finishes_on_the_before_and_after_digests() {
        let (_t, cfg, dest) = setup();
        begin(&cfg, &journal(&dest, Some("old"), Some("new"))).unwrap();
        std::fs::write(&dest, "old").unwrap();
        recover(&cfg).unwrap();
        assert!(!cfg.join(JOURNAL_FILE).exists());
        assert!(
            !cfg.join(super::super::RECORD_FILE).exists(),
            "rolled back: no record"
        );

        begin(&cfg, &journal(&dest, Some("old"), Some("new"))).unwrap();
        std::fs::write(&dest, "new").unwrap();
        recover(&cfg).unwrap();
        assert!(!cfg.join(JOURNAL_FILE).exists());
        assert!(
            Record::load(&cfg).unwrap().files.contains_key(&dest),
            "committed: record saved"
        );
    }
}
