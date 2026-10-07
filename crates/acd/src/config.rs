use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const DEFAULT_PORT: u16 = 47800;

/// `config.toml`. Unknown keys are ignored, so later plans can add sections.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct Config {
    pub machine_name: String,
    pub port: u16,
    pub listen: Vec<String>,
    pub allowed_origins: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            machine_name: gethostname::gethostname().to_string_lossy().into_owned(),
            port: DEFAULT_PORT,
            listen: vec!["loopback".into(), "tailscale".into()],
            allowed_origins: vec![],
        }
    }
}

impl Config {
    #[allow(dead_code)] // used by `acd run` in Task 10
    pub fn load(dir: &Path) -> Result<Config> {
        let path = dir.join("config.toml");
        if !path.exists() { return Ok(Config::default()); }
        crate::secrets::check_mode(&path, 0o600)?;
        let raw = std::fs::read_to_string(&path)?;
        toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_file_fills_defaults_and_ignores_unknown_keys() {
        let c: Config = toml::from_str("port = 9000\n[herdr]\nenabled = true\n").unwrap();
        assert_eq!(c.port, 9000);
        assert_eq!(c.listen, vec!["loopback", "tailscale"]);
    }
}
