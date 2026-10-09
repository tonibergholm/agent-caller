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

pub fn machine_id(dir: &Path) -> Result<String> {
    let id = std::fs::read_to_string(dir.join("machine-id"))
        .context("machine-id missing; run `acd init`")?;
    let id = id.trim();
    if id.is_empty() {
        anyhow::bail!("machine-id is empty; run `acd init` to repair it");
    }
    Ok(id.to_string())
}
