use std::path::Path;

use anyhow::{Context, Result};

/// `<hostname>-<8 hex>`, created once by `acd init`.
pub fn create_machine_id(dir: &Path) -> Result<String> {
    let host = gethostname::gethostname().to_string_lossy().to_lowercase().replace(|c: char| !c.is_ascii_alphanumeric(), "-");
    let mut b = [0u8; 4];
    getrandom::fill(&mut b).expect("OS randomness is available");
    let id = format!("{host}-{:02x}{:02x}{:02x}{:02x}", b[0], b[1], b[2], b[3]);
    crate::secrets::write_secret(&dir.join("machine-id"), &id)?;
    Ok(id)
}

#[allow(dead_code)] // used by `acd run` in Task 10
pub fn machine_id(dir: &Path) -> Result<String> {
    Ok(std::fs::read_to_string(dir.join("machine-id")).context("machine-id missing; run `acd init`")?.trim().to_string())
}
