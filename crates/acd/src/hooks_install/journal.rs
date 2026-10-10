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
    /// `None` means the record entry is removed (uninstall).
    pub record_after: Option<FileRecord>,
    /// Journals written before this field existed were Claude's.
    #[serde(default = "legacy_target")]
    pub target: super::Target,
    /// J3: why recovery could not decide; kept until it can.
    #[serde(default)]
    pub conflict: Option<String>,
}
fn legacy_target() -> super::Target {
    super::Target::Claude
}
pub fn pending(config_dir: &Path) -> bool {
    config_dir.join(JOURNAL_FILE).symlink_metadata().is_ok()
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
    if pending(config_dir) {
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
fn apply(config_dir: &Path, dest: &Path, rec: Option<FileRecord>) -> Result<()> {
    let mut record = Record::load(config_dir)?;
    record.set(config_dir, dest, rec);
    record.save(config_dir)
}

/// J3: keeps the journal, records why, and refuses.
fn conflict(config_dir: &Path, mut j: Journal, why: String) -> Result<Option<String>> {
    if j.conflict.as_deref() != Some(why.as_str()) {
        j.conflict = Some(why.clone());
        crate::secrets::write_structured(
            &config_dir.join(JOURNAL_FILE),
            &serde_json::to_string_pretty(&j)?,
        )?;
    }
    bail!(
        "conflict: {} was changed after an interrupted acd hooks command ({why}). The journal was kept; fix the file, then run `acd hooks status` or any `acd hooks` command again",
        j.destination.display()
    )
}

/// J2 and J3. Run at the start of every installer entry point, under the
/// writer lock. Settings in the `after` state: apply `record_after`. In the
/// `before` state: nothing changed. Otherwise someone (typically Claude's own
/// atomic rewrite) changed the file and ownership follows the acd entries it
/// holds. A file that cannot be read safely keeps the journal as a conflict.
/// Returns a note when the record was rebuilt rather than applied as journaled.
pub fn recover(config_dir: &Path) -> Result<Option<String>> {
    let path = config_dir.join(JOURNAL_FILE);
    if !pending(config_dir) {
        return Ok(None);
    }
    let j: Journal = serde_json::from_str(&crate::secrets::read_structured(&path)?)
        .map_err(|e| anyhow::anyhow!("{} is not valid: {e}", path.display()))?;
    let text = match super::read_settings(&j.destination) {
        Ok(t) => t,
        Err(e) => return conflict(config_dir, j, format!("{e:#}")),
    };
    let now = digest(text.as_deref());
    let mut note = None;
    if now == j.after {
        apply(config_dir, &j.destination, j.record_after.clone())?;
    } else if now != j.before {
        let owned = match text
            .as_deref()
            .map(|t| super::owned_entries(t, j.target))
            .transpose()
        {
            Ok(n) => n.unwrap_or(0),
            Err(e) => return conflict(config_dir, j, format!("not valid JSON: {e:#}")),
        };
        let rec = (owned > 0).then(|| {
            j.record_after.clone().unwrap_or(FileRecord {
                target: j.target,
                created_file: false,
                created_hooks_key: false,
                created_events: vec![],
            })
        });
        note = Some(if owned > 0 {
            format!(
                "{} changed during an interrupted acd hooks command; its install record now follows the {owned} acd entries it holds",
                j.destination.display()
            )
        } else {
            format!(
                "{} changed during an interrupted acd hooks command and holds no acd entries; record removed",
                j.destination.display()
            )
        });
        apply(config_dir, &j.destination, rec)?;
    }
    finish(config_dir)?;
    Ok(note)
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
            target: Target::Claude,
            conflict: None,
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

    fn j(dest: &str) -> Journal {
        Journal {
            destination: dest.into(),
            before: None,
            after: None,
            record_after: None,
            target: Target::Claude,
            conflict: None,
        }
    }

    #[test]
    fn a_journal_for_a_long_path_round_trips() {
        let d = tempfile::tempdir().unwrap();
        crate::secrets::ensure_private_dir(d.path()).unwrap();
        let long = format!("/{}/settings.json", "deep/".repeat(900)); // about 4.5 KiB
        begin(d.path(), &j(&long)).unwrap();
        assert!(pending(d.path()));
        let back: Journal = serde_json::from_str(
            &crate::secrets::read_structured(&d.path().join(JOURNAL_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(back.destination, PathBuf::from(long), "F1: more than 4 KiB");
    }

    #[test]
    fn a_rewritten_file_without_acd_entries_drops_the_record() {
        let (_t, cfg, dest) = setup();
        let foreign =
            r#"{"model":"opus","hooks":{"Stop":[{"hooks":[{"type":"command","command":"x"}]}]}}"#;
        let jr = journal(&dest, Some("old"), Some("new"));
        let mut r = Record {
            version: 2,
            ..Default::default()
        };
        r.files
            .insert(dest.clone(), jr.record_after.clone().unwrap());
        r.save(&cfg).unwrap();
        std::fs::write(&dest, foreign).unwrap();
        begin(&cfg, &jr).unwrap();
        let note = recover(&cfg).unwrap().unwrap();
        assert!(note.contains("record removed"), "{note}");
        assert!(!Record::load(&cfg).unwrap().files.contains_key(&dest));
        assert!(!pending(&cfg));
    }

    #[test]
    fn recovery_counts_only_the_journals_target_shape() {
        let (_t, cfg, dest) = setup();
        let codex_only = format!(
            r#"{{"hooks":{{"Stop":[{{"hooks":[{{"type":"command","command":"/bin/acd hook codex Stop --config-dir /c {}"}}]}}]}}}}"#,
            crate::hooks_install::MARKER
        );
        assert_eq!(
            crate::hooks_install::owned_entries(&codex_only, Target::Codex).unwrap(),
            1
        );
        std::fs::write(&dest, &codex_only).unwrap();
        begin(&cfg, &journal(&dest, Some("old"), Some("new"))).unwrap(); // Claude
        let note = recover(&cfg).unwrap().unwrap();
        assert!(note.contains("record removed"), "{note}");
    }

    #[test]
    fn an_older_journal_without_a_target_still_loads() {
        let t = serde_json::json!({"destination": "/a/settings.json", "before": null, "after": null, "record_after": null});
        let j: Journal = serde_json::from_value(t).unwrap();
        assert_eq!(j.target, Target::Claude);
        assert_eq!(j.conflict, None);
    }

    #[test]
    fn a_conflict_is_recorded_in_the_kept_journal() {
        let (_t, cfg, dest) = setup();
        begin(&cfg, &journal(&dest, Some("old"), Some("new"))).unwrap();
        std::fs::write(&dest, "not json").unwrap();
        assert!(recover(&cfg).is_err());
        let kept: Journal = serde_json::from_str(
            &crate::secrets::read_structured(&cfg.join(JOURNAL_FILE)).unwrap(),
        )
        .unwrap();
        assert!(kept.conflict.unwrap().contains("not valid JSON"));
    }
}
