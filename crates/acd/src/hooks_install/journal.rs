//! Settings and record updates are journaled so interrupted writes can recover.
use super::{FileRecord, Record};
use anyhow::Result;
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
pub fn begin(config_dir: &Path, j: &Journal) -> Result<()> {
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
pub fn recover(config_dir: &Path) -> Result<()> {
    let path = config_dir.join(JOURNAL_FILE);
    if path.symlink_metadata().is_err() {
        return Ok(());
    }
    let j: Journal = serde_json::from_str(&crate::secrets::read_structured(&path)?)?;
    let now = digest(super::read_settings(&j.destination)?.as_deref());
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
        eprintln!(
            "warning: {} changed since an interrupted acd hooks command; its install record was left as it was",
            j.destination.display()
        );
    }
    finish(config_dir)
}
