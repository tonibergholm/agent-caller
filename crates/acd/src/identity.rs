use std::path::Path;

use anyhow::{Context, Result};

/// `<hostname>-<8 hex>`, created once by `acd init`.
pub fn create_machine_id(dir: &Path) -> Result<String> {
    let host = gethostname::gethostname()
        .to_string_lossy()
        .to_lowercase()
        .replace(|c: char| !c.is_ascii_alphanumeric(), "-");
    let mut b = [0u8; 4];
    getrandom::fill(&mut b).expect("OS randomness is available");
    let id = format!("{host}-{:02x}{:02x}{:02x}{:02x}", b[0], b[1], b[2], b[3]);
    crate::secrets::write_secret(&dir.join("machine-id"), &id)?;
    Ok(id)
}

fn read_id(dir: &Path) -> Result<String> {
    // No-follow, owner-checked, size-bounded; a FIFO or symlink is refused.
    let bytes = acd_securefs::read_private(&dir.join("machine-id"), 0o7777, 256)
        .map_err(|e| match e {
            acd_securefs::SecureFsError::Missing(_) => {
                anyhow::anyhow!("machine-id missing; run `acd init`")
            }
            other => anyhow::anyhow!("{other}"),
        })?;
    Ok(String::from_utf8(bytes)
        .context("machine-id is not UTF-8")?
        .trim()
        .to_string())
}

pub fn machine_id(dir: &Path) -> Result<String> {
    let id = read_id(dir)?;
    if id.is_empty() {
        anyhow::bail!("machine-id is empty; run `acd init` to repair it");
    }
    Ok(id)
}

/// True when machine-id is a safely readable file with no content.
pub fn machine_id_is_empty(dir: &Path) -> bool {
    read_id(dir).is_ok_and(|s| s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_id_refuses_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::write(&real, "host-deadbeef\n").unwrap();
        std::os::unix::fs::symlink(&real, dir.path().join("machine-id")).unwrap();
        let err = machine_id(dir.path()).unwrap_err().to_string();
        assert!(err.contains("symlink"), "{err}");
        assert!(!machine_id_is_empty(dir.path()));
    }

    #[test]
    fn machine_id_round_trips_and_rejects_empty() {
        let dir = tempfile::tempdir().unwrap();
        let id = create_machine_id(dir.path()).unwrap();
        assert_eq!(machine_id(dir.path()).unwrap(), id);
        std::fs::write(dir.path().join("machine-id"), "\n").unwrap();
        assert!(machine_id(dir.path()).is_err());
        assert!(machine_id_is_empty(dir.path()));
    }
}
