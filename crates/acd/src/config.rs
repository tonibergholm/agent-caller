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
    pub claude: ClaudeSection,
    /// Claude entrypoints (such as the Agent SDK's) whose sessions are not
    /// shown unless they are bound to a herdr pane.
    pub ignored_entrypoints: Vec<String>,
    /// The raw `[push]` section. Kept as a value so that a mistake in it can
    /// never stop the daemon from starting; `push_setting` interprets it and
    /// fails closed (spec 2026-11-03-relay-push-design §5.2).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub push: Option<toml::Value>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct ClaudeSection {
    /// Seconds acd holds a Claude permission prompt (spec §9).
    pub permission_hold: u64,
}

impl Default for ClaudeSection {
    fn default() -> Self {
        Self {
            permission_hold: 3600,
        }
    }
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
            claude: ClaudeSection::default(),
            ignored_entrypoints: vec!["sdk-ts".into()],
            push: None,
        }
    }
}

/// A free loopback port other than `avoid`, for a new config's `hook_port`
/// (amendment A1: not a well-known default).
pub fn random_free_port(avoid: u16) -> Result<u16> {
    for _ in 0..16 {
        let port = std::net::TcpListener::bind(("127.0.0.1", 0))?
            .local_addr()?
            .port();
        if port != avoid {
            return Ok(port);
        }
    }
    anyhow::bail!("could not find a free loopback port")
}

impl Config {
    /// Whether push is on, and the relay URL if so. Default off; a malformed
    /// section, an unknown key, an `http://` URL or a `relay_pin` (no verifier
    /// yet) all mean off. Not wired to the daemon yet (relay spec P5b).
    #[allow(dead_code)]
    pub fn push_setting(&self) -> acd_relay::client::PushSetting {
        acd_relay::client::resolve(self.push.as_ref())
    }

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
    use acd_relay::client::PushSetting;

    fn push_of(toml_src: &str) -> PushSetting {
        toml::from_str::<Config>(toml_src).unwrap().push_setting()
    }

    #[test]
    fn push_is_off_by_default() {
        assert_eq!(push_of(""), PushSetting::Off(None));
        assert_eq!(Config::default().push_setting(), PushSetting::Off(None));
        assert_eq!(push_of("[push]\n"), PushSetting::Off(None));
    }

    #[test]
    fn push_unknown_key_turns_push_off() {
        assert!(matches!(push_of("[push]\nrelai = \"https://r.example\"\n"), PushSetting::Off(Some(_))));
        // A good relay next to a typo'd key is still off.
        assert!(matches!(
            push_of("[push]\nrelay = \"https://r.example\"\nrelay_url = \"x\"\n"),
            PushSetting::Off(Some(_))
        ));
    }

    #[test]
    fn push_http_url_turns_push_off() {
        for u in ["http://r.example", "http://127.0.0.1:8787", "ftp://r.example", "r.example", "https://u:p@r.example"] {
            assert!(matches!(push_of(&format!("[push]\nrelay = \"{u}\"\n")), PushSetting::Off(Some(_))), "{u}");
        }
    }

    #[test]
    fn push_pin_turns_push_off_until_a_verifier_exists() {
        let s = push_of("[push]\nrelay = \"https://r.example\"\nrelay_pin = \"sha256:00\"\n");
        assert!(matches!(s, PushSetting::Off(Some(_))), "{s:?}");
    }

    #[test]
    fn malformed_push_section_does_not_break_the_config() {
        let c: Config = toml::from_str("port = 9000\npush = 5\n").unwrap();
        assert_eq!(c.port, 9000);
        assert!(matches!(c.push_setting(), PushSetting::Off(Some(_))));
        let c: Config = toml::from_str("port = 9001\n[push]\nrelay = 7\n").unwrap();
        assert_eq!(c.port, 9001);
        assert!(matches!(c.push_setting(), PushSetting::Off(Some(_))));
        // The default config still serialises (no `push` key is written).
        let out = toml::to_string(&Config::default()).unwrap();
        assert!(!out.contains("push"), "{out}");
    }

    #[test]
    fn push_valid_https_url_enables() {
        match push_of("[push]\nrelay = \"https://relay.example.net/\"\n") {
            PushSetting::On(u) => assert_eq!(u.as_str(), "https://relay.example.net"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn partial_file_fills_defaults_and_ignores_unknown_keys() {
        let c: Config = toml::from_str("port = 9000\n[herdr]\nenabled = true\n").unwrap();
        assert_eq!(c.port, 9000);
        assert_eq!(c.hook_port, DEFAULT_HOOK_PORT);
        assert_eq!(c.listen, vec!["loopback", "tailscale"]);
        assert!(c.herdr.enabled);
    }

    #[test]
    fn ignored_entrypoints_default_and_parse() {
        let c: Config = toml::from_str("").unwrap();
        assert_eq!(c.ignored_entrypoints, vec!["sdk-ts"]);
        let c: Config = toml::from_str("ignored_entrypoints = []\n").unwrap();
        assert!(c.ignored_entrypoints.is_empty());
        let c: Config = toml::from_str("ignored_entrypoints = [\"a\", \"b\"]\n").unwrap();
        assert_eq!(c.ignored_entrypoints, vec!["a", "b"]);
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
