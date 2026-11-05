//! Relay configuration (spec §5.7). Fails closed: every problem is an error
//! before the relay listens.

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::handle::Keys;
use crate::wire::app_ok;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    listen: Option<String>,
    allow_non_loopback: Option<bool>,
    #[serde(default)]
    allowed_apps: Vec<String>,
    sealing_keys_file: String,
    trusted_proxy: Option<IpAddr>,
    fcm: RawFcm,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFcm {
    project_id: String,
    credentials_file: String,
    ca_file: Option<String>,
}

#[derive(Debug)]
pub struct FcmConfig {
    pub project_id: String,
    pub credentials_file: PathBuf,
    pub ca_file: PathBuf,
}

#[derive(Debug)]
pub struct Config {
    pub listen: SocketAddr,
    pub allowed_apps: Vec<String>,
    pub keys: Keys,
    pub trusted_proxy: Option<IpAddr>,
    pub fcm: FcmConfig,
}

pub const DEFAULT_LISTEN: &str = "127.0.0.1:8787";
pub const DEFAULT_CA_FILE: &str = "/etc/ssl/certs/ca-certificates.crt";

impl Config {
    pub fn load(path: &Path) -> Result<Config, String> {
        let raw = std::fs::read_to_string(path).map_err(|e| format!("reading config: {e}"))?;
        Config::parse(&raw)
    }

    pub fn parse(raw: &str) -> Result<Config, String> {
        let r: Raw = toml::from_str(raw).map_err(|e| format!("config: {}", e.message()))?;
        let listen: SocketAddr = r
            .listen
            .as_deref()
            .unwrap_or(DEFAULT_LISTEN)
            .parse()
            .map_err(|_| "config: invalid listen address".to_string())?;
        if !listen.ip().is_loopback() && r.allow_non_loopback != Some(true) {
            return Err("config: a non-loopback listen address needs allow_non_loopback = true".into());
        }
        if r.allowed_apps.is_empty() {
            return Err("config: allowed_apps must not be empty".into());
        }
        if r.allowed_apps.iter().any(|a| !app_ok(a)) {
            return Err("config: invalid app id in allowed_apps".into());
        }
        let keys_path = Path::new(&r.sealing_keys_file);
        let raw_keys = acd_securefs::read_private(keys_path, 0o600, 64 * 1024)
            .map_err(|e| format!("sealing keys file: {e}"))?;
        let keys = Keys::from_json(&raw_keys).map_err(|e| format!("sealing keys file: {e}"))?;
        let cred = PathBuf::from(&r.fcm.credentials_file);
        acd_securefs::check_private_file(&cred, 0o600).map_err(|e| format!("credentials file: {e}"))?;
        let ca_file = PathBuf::from(r.fcm.ca_file.as_deref().unwrap_or(DEFAULT_CA_FILE));
        if !ca_file.is_file() {
            return Err("config: the CA file cannot be read".into());
        }
        if r.fcm.project_id.is_empty()
            || !r.fcm.project_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err("config: invalid fcm.project_id".into());
        }
        Ok(Config {
            listen,
            allowed_apps: r.allowed_apps,
            keys,
            trusted_proxy: r.trusted_proxy,
            fcm: FcmConfig { project_id: r.fcm.project_id, credentials_file: cred, ca_file },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    struct Fixture {
        dir: tempfile::TempDir,
    }

    fn write(path: &Path, body: &str, mode: u32) {
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let k = base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, [1u8; 32]);
        write(&dir.path().join("keys.json"), &format!(r#"{{"keys":[{{"kid":1,"key":"{k}"}}]}}"#), 0o600);
        write(&dir.path().join("cred.json"), "{}", 0o600);
        write(&dir.path().join("ca.pem"), "x", 0o644);
        Fixture { dir }
    }

    impl Fixture {
        fn toml(&self, extra: &str) -> String {
            let d = self.dir.path().display();
            format!(
                "allowed_apps = [\"com.example.app\"]\nsealing_keys_file = \"{d}/keys.json\"\n{extra}\n[fcm]\nproject_id = \"p-1\"\ncredentials_file = \"{d}/cred.json\"\nca_file = \"{d}/ca.pem\"\n"
            )
        }
    }

    #[test]
    fn valid_config_loads() {
        let f = fixture();
        let c = Config::parse(&f.toml("")).unwrap();
        assert_eq!(c.listen, DEFAULT_LISTEN.parse().unwrap());
        assert_eq!(c.allowed_apps, ["com.example.app"]);
    }

    #[test]
    fn unknown_key_is_refused() {
        let f = fixture();
        assert!(Config::parse(&f.toml("allowd_apps = []")).is_err());
        let with_fcm_typo = f.toml("").replace("project_id", "projectid");
        assert!(Config::parse(&with_fcm_typo).is_err());
    }

    #[test]
    fn missing_or_loose_keys_file_is_refused() {
        let f = fixture();
        std::fs::set_permissions(f.dir.path().join("keys.json"), std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(Config::parse(&f.toml("")).is_err());
        std::fs::remove_file(f.dir.path().join("keys.json")).unwrap();
        assert!(Config::parse(&f.toml("")).is_err());
        let f = fixture();
        std::fs::set_permissions(f.dir.path().join("cred.json"), std::fs::Permissions::from_mode(0o640)).unwrap();
        assert!(Config::parse(&f.toml("")).is_err());
    }

    #[test]
    fn empty_allowed_apps_is_refused() {
        let f = fixture();
        let t = f.toml("").replace("allowed_apps = [\"com.example.app\"]", "allowed_apps = []");
        assert!(Config::parse(&t).is_err());
        let t = f.toml("").replace("allowed_apps = [\"com.example.app\"]", "");
        assert!(Config::parse(&t).is_err());
    }

    #[test]
    fn non_loopback_listen_needs_opt_in() {
        let f = fixture();
        assert!(Config::parse(&f.toml("listen = \"0.0.0.0:8787\"")).is_err());
        assert!(Config::parse(&f.toml("listen = \"[::1]:8787\"")).is_ok());
        let c = Config::parse(&f.toml("listen = \"0.0.0.0:8787\"\nallow_non_loopback = true")).unwrap();
        assert!(!c.listen.ip().is_loopback());
    }
}
