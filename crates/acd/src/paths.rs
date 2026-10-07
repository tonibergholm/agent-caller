use std::path::PathBuf;

use anyhow::{anyhow, Result};

/// `$ACD_CONFIG_DIR`, or `~/.config/acd`.
pub fn config_dir() -> Result<PathBuf> {
    if let Some(d) = std::env::var_os("ACD_CONFIG_DIR") { return Ok(PathBuf::from(d)); }
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow!("HOME is not set"))?;
    Ok(PathBuf::from(home).join(".config").join("acd"))
}
