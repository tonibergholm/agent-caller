use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const DEFAULT_PORT: u16 = 47800;
pub const DEFAULT_HOOK_PORT: u16 = 47801;

/// `config.toml`. Unknown keys are ignored, so later plans can add sections.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct Config {
    pub machine_name: String,
    pub port: u16,
    pub hook_port: u16,
    pub listen: Vec<String>,
    pub allowed_origins: Vec<String>,
    pub herdr: HerdrSection,
    pub t3: T3Section,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct T3Section {
    pub enabled: bool,
    pub url: Option<String>,
}

impl Default for T3Section {
    fn default() -> Self {
        Self {
            enabled: true,
            url: None,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct HerdrSection {
    pub enabled: bool,
    pub extra_sockets: Vec<String>,
}

impl Default for HerdrSection {
    fn default() -> Self {
        Self {
            enabled: true,
            extra_sockets: vec![],
        }
    }
}

/// Replaces a leading `~/` with `$HOME/`; anything else is returned unchanged.
pub fn expand_home(s: &str) -> PathBuf {
    match (s.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(s),
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            machine_name: gethostname::gethostname().to_string_lossy().into_owned(),
            port: DEFAULT_PORT,
            hook_port: DEFAULT_HOOK_PORT,
            listen: vec!["loopback".into(), "tailscale".into()],
            allowed_origins: vec![],
            herdr: HerdrSection::default(),
            t3: T3Section::default(),
        }
    }
}

impl Config {
    pub fn load(dir: &Path) -> Result<Config> {
        let path = dir.join("config.toml");
        if !path.exists() {
            return Ok(Config::default());
        }
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
        assert_eq!(c.hook_port, DEFAULT_HOOK_PORT);
        assert_eq!(c.listen, vec!["loopback", "tailscale"]);
        assert!(c.herdr.enabled);
    }

    #[test]
    fn t3_section_defaults_and_parses() {
        let c: Config = toml::from_str("").unwrap();
        assert!(c.t3.enabled);
        assert!(c.t3.url.is_none());
        let c: Config =
            toml::from_str("[t3]\nenabled = false\nurl = \"http://127.0.0.1:3773\"\n").unwrap();
        assert!(!c.t3.enabled);
        assert_eq!(c.t3.url.as_deref(), Some("http://127.0.0.1:3773"));
    }

    #[test]
    fn herdr_section_defaults_and_parses() {
        let c: Config = toml::from_str("").unwrap();
        assert!(c.herdr.enabled);
        assert!(c.herdr.extra_sockets.is_empty());
        let c: Config =
            toml::from_str("[herdr]\nenabled = false\nextra_sockets = [\"~/x.sock\"]\n").unwrap();
        assert!(!c.herdr.enabled);
        assert_eq!(c.herdr.extra_sockets, vec!["~/x.sock"]);
    }

    #[test]
    fn expand_home_only_expands_a_leading_tilde_slash() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(expand_home("~/x"), PathBuf::from(home).join("x"));
        assert_eq!(expand_home("/abs"), PathBuf::from("/abs"));
        assert_eq!(expand_home("~user/x"), PathBuf::from("~user/x"));
    }
}
