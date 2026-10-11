//! App settings (not secret) and the secret store interface. Remote tokens
//! live in the Keychain (the app's `KeychainSecrets`); the local token is
//! never stored (D2).
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

pub const DEFAULT_PORT: u16 = 47800;
pub const DEFAULT_HOTKEY: &str = "Control+Alt+Space";
pub const LOCAL_KEY: &str = "local";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EndpointConfig {
    Local,
    Tcp { host: String, port: u16 },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MachineEntry {
    pub key: String,
    pub name: String,
    pub endpoint: EndpointConfig,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub client_id: String,
    #[serde(default = "default_hotkey")]
    pub hotkey: String,
    #[serde(default)]
    pub show_idle: bool,
    #[serde(default)]
    pub machines: Vec<MachineEntry>,
}

fn default_hotkey() -> String {
    DEFAULT_HOTKEY.into()
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("{0}")]
    Invalid(String),
    #[error("settings file {0}: {1}")]
    Io(String, std::io::Error),
}

#[derive(Debug, thiserror::Error)]
#[error("secret store: {0}")]
pub struct SecretError(pub String);

/// Canonical base64url without padding, decoding to exactly 32 bytes: the
/// same rule as acd's `parse_token` (non-zero trailing bits are rejected).
pub fn valid_token(s: &str) -> bool {
    decode_canonical_32(s).is_some()
}

fn decode_canonical_32(s: &str) -> Option<[u8; 32]> {
    use base64::Engine;
    if s.len() != 43 {
        return None;
    }
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s)
        .ok()?;
    if decoded.len() != 32 || base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&decoded) != s
    {
        return None;
    }
    decoded.try_into().ok()
}

/// The Keychain value for a remote machine (decision D7).
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct MachineSecret {
    pub token: String,
    pub pin: String,
}

impl std::fmt::Debug for MachineSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MachineSecret")
            .field("token", &"<redacted>")
            .field("pin", &self.pin)
            .finish()
    }
}

impl MachineSecret {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("serializable")
    }
    pub fn from_json(s: &str) -> Result<Self, SecretError> {
        let m: Self = serde_json::from_str(s)
            .map_err(|_| SecretError("stored machine secret is unreadable".into()))?;
        if !valid_token(&m.token) || !valid_pin(&m.pin) {
            return Err(SecretError("stored machine secret is invalid".into()));
        }
        Ok(m)
    }
}

fn valid_pin(s: &str) -> bool {
    let Some(encoded) = s.strip_prefix("sha256:") else {
        return false;
    };
    decode_canonical_32(encoded).is_some()
}

impl MachineEntry {
    pub fn validate(&self) -> Result<(), SettingsError> {
        let bad = |m: &str| Err(SettingsError::Invalid(m.into()));
        let canonical_uuid =
            uuid::Uuid::parse_str(&self.key).is_ok_and(|u| u.hyphenated().to_string() == self.key);
        if self.key != LOCAL_KEY && !canonical_uuid {
            return bad("invalid machine key");
        }
        let name = self.name.trim();
        if name.is_empty() || name.chars().count() > 64 {
            return bad("Name must be 1 to 64 characters");
        }
        if let EndpointConfig::Tcp { host, port } = &self.endpoint {
            if host.is_empty()
                || host.len() > 253
                || host.contains("://")
                || host.chars().any(|c| c.is_whitespace() || c == '/')
            {
                return bad("Enter a host name or address, without ws:// or a path");
            }
            if *port == 0 {
                return bad("Port must be 1 to 65535");
            }
        }
        Ok(())
    }
}

impl Settings {
    pub fn fresh() -> Self {
        Self {
            client_id: uuid::Uuid::new_v4().to_string(),
            hotkey: DEFAULT_HOTKEY.into(),
            show_idle: false,
            machines: vec![MachineEntry {
                key: LOCAL_KEY.into(),
                name: "This Mac".into(),
                endpoint: EndpointConfig::Local,
            }],
        }
    }

    /// Loads `path`, or creates it on first run. A file that does not parse
    /// is renamed to `<path>.bad` and replaced, so the app always starts.
    pub fn load_or_init(path: &Path) -> Result<Self, SettingsError> {
        let io = |e| SettingsError::Io(path.display().to_string(), e);
        match fs::read_to_string(path) {
            Ok(raw) => match serde_json::from_str::<Settings>(&raw) {
                Ok(mut s) if uuid::Uuid::parse_str(&s.client_id).is_ok() => {
                    let before = s.machines.len();
                    s.machines.retain(|m| m.validate().is_ok());
                    if s.machines.len() != before {
                        tracing::warn!(
                            dropped = before - s.machines.len(),
                            "settings file has invalid machines; dropping them"
                        );
                        s.save(path)?;
                    }
                    Ok(s)
                }
                _ => {
                    tracing::warn!(path = %path.display(), "settings file unreadable; starting fresh");
                    fs::rename(path, path.with_extension("json.bad")).map_err(io)?;
                    let s = Self::fresh();
                    s.save(path)?;
                    Ok(s)
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let s = Self::fresh();
                s.save(path)?;
                Ok(s)
            }
            Err(e) => Err(io(e)),
        }
    }

    /// Atomic write, mode 0600, parent directory 0700.
    pub fn save(&self, path: &Path) -> Result<(), SettingsError> {
        let io = |e| SettingsError::Io(path.display().to_string(), e);
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(io)?;
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(io)?;
        }
        let tmp = path.with_extension("json.tmp");
        let _ = fs::remove_file(&tmp);
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(io)?;
        f.write_all(
            serde_json::to_string_pretty(self)
                .expect("serializable")
                .as_bytes(),
        )
        .map_err(io)?;
        f.sync_all().map_err(io)?;
        fs::rename(&tmp, path).map_err(io)
    }
}

pub trait SecretStore: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<String>, SecretError>;
    fn set(&self, key: &str, value: &str) -> Result<(), SecretError>;
    fn delete(&self, key: &str) -> Result<(), SecretError>;
}

#[derive(Default)]
pub struct MemorySecrets(Mutex<HashMap<String, String>>);

impl SecretStore for MemorySecrets {
    fn get(&self, key: &str) -> Result<Option<String>, SecretError> {
        Ok(self.0.lock().unwrap().get(key).cloned())
    }
    fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
        self.0.lock().unwrap().insert(key.into(), value.into());
        Ok(())
    }
    fn delete(&self, key: &str) -> Result<(), SecretError> {
        self.0.lock().unwrap().remove(key);
        Ok(())
    }
}

/// Reads each key from the inner store once per process. The Keychain can
/// ask the user for every read, so listings must not go back to it.
/// Failed reads are not cached: a denied prompt can be retried.
pub struct CachedSecrets<S> {
    inner: S,
    cache: Mutex<HashMap<String, Option<String>>>,
}

impl<S> CachedSecrets<S> {
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            cache: Mutex::new(HashMap::new()),
        }
    }
}

impl<S: SecretStore> SecretStore for CachedSecrets<S> {
    fn get(&self, key: &str) -> Result<Option<String>, SecretError> {
        let mut cache = self.cache.lock().unwrap();
        if let Some(value) = cache.get(key) {
            return Ok(value.clone());
        }
        let value = self.inner.get(key)?;
        cache.insert(key.into(), value.clone());
        Ok(value)
    }
    fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
        let mut cache = self.cache.lock().unwrap();
        cache.remove(key);
        self.inner.set(key, value)?;
        cache.insert(key.into(), Some(value.into()));
        Ok(())
    }
    fn delete(&self, key: &str) -> Result<(), SecretError> {
        let mut cache = self.cache.lock().unwrap();
        cache.remove(key);
        self.inner.delete(key)?;
        cache.insert(key.into(), None);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_run_adds_the_local_machine_and_a_client_id() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("cfg/settings.json");
        let s = Settings::load_or_init(&p).unwrap();
        assert_eq!(
            s.machines,
            vec![MachineEntry {
                key: LOCAL_KEY.into(),
                name: "This Mac".into(),
                endpoint: EndpointConfig::Local
            }]
        );
        assert!(uuid::Uuid::parse_str(&s.client_id).is_ok());
        assert_eq!(s.hotkey, DEFAULT_HOTKEY);
        let again = Settings::load_or_init(&p).unwrap();
        assert_eq!(
            again.client_id, s.client_id,
            "client.id is created once per installation"
        );
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn removing_the_local_machine_is_respected() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.json");
        let mut s = Settings::load_or_init(&p).unwrap();
        s.machines.clear();
        s.save(&p).unwrap();
        assert!(Settings::load_or_init(&p).unwrap().machines.is_empty());
    }

    #[test]
    fn corrupt_file_is_moved_aside_and_replaced() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.json");
        std::fs::write(&p, "{not json").unwrap();
        let s = Settings::load_or_init(&p).unwrap();
        assert_eq!(s.machines.len(), 1);
        assert!(d.path().join("settings.json.bad").exists());
    }

    #[test]
    fn invalid_machines_are_dropped_on_load() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.json");
        let mut s = Settings::fresh();
        s.machines.push(MachineEntry {
            key: "a/b".into(),
            name: "bad".into(),
            endpoint: EndpointConfig::Local,
        });
        std::fs::write(&p, serde_json::to_string(&s).unwrap()).unwrap();
        let loaded = Settings::load_or_init(&p).unwrap();
        assert_eq!(loaded.machines.len(), 1);
        assert_eq!(loaded.machines[0].key, LOCAL_KEY);
        assert_eq!(loaded.client_id, s.client_id);
        let raw = std::fs::read_to_string(&p).unwrap();
        assert!(!raw.contains("a/b"), "cleaned settings are saved");
    }

    #[test]
    fn entry_validation() {
        let tcp = |host: &str, port| MachineEntry {
            key: uuid::Uuid::new_v4().to_string(),
            name: "n".into(),
            endpoint: EndpointConfig::Tcp {
                host: host.into(),
                port,
            },
        };
        assert!(tcp("devbox.tail1234.ts.net", 47800).validate().is_ok());
        let mut local_key = tcp("devbox", 47800);
        local_key.key = LOCAL_KEY.into();
        assert!(local_key.validate().is_ok());
        assert!(tcp("", 47800).validate().is_err());
        assert!(tcp("ws://devbox", 47800).validate().is_err());
        assert!(tcp("dev box", 47800).validate().is_err());
        assert!(tcp("devbox", 0).validate().is_err());
        let mut bad_key = tcp("devbox", 1);
        let id = uuid::Uuid::new_v4();
        let braced = id.braced().to_string();
        let simple = id.simple().to_string();
        let upper = id.hyphenated().to_string().to_uppercase();
        for invalid_key in [
            "",
            "machine",
            "a/b",
            "00000000-0000-0000-0000-00000000000g",
            &braced,
            &simple,
            &upper,
        ] {
            bad_key.key = invalid_key.into();
            assert!(bad_key.validate().is_err(), "accepted key {invalid_key:?}");
        }
        let mut no_name = tcp("devbox", 1);
        no_name.name = " ".into();
        assert!(no_name.validate().is_err());
    }

    #[test]
    fn tokens_and_memory_secrets() {
        assert!(valid_token("AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"));
        assert!(!valid_token("short") && !valid_token(&"!".repeat(43)));
        assert!(
            !valid_token("AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh9"),
            "non-canonical"
        );
        let ms = MachineSecret {
            token: "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8".into(),
            pin: "sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
        };
        assert_eq!(MachineSecret::from_json(&ms.to_json()).unwrap(), ms);
        for bad_pin in [
            "sha256:!",
            "sha256:AA",
            "sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            "sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA_",
        ] {
            let bad = MachineSecret {
                pin: bad_pin.into(),
                ..ms.clone()
            };
            assert!(
                MachineSecret::from_json(&bad.to_json()).is_err(),
                "accepted pin {bad_pin:?}"
            );
        }
        assert!(!format!("{ms:?}").contains("AAEC"));
        let m = MemorySecrets::default();
        assert_eq!(m.get("k").unwrap(), None);
        m.set("k", "v").unwrap();
        assert_eq!(m.get("k").unwrap().as_deref(), Some("v"));
        m.delete("k").unwrap();
        assert_eq!(m.get("k").unwrap(), None);
    }

    /// Counts reads and fails the first `fail` of them.
    struct Counting {
        inner: MemorySecrets,
        reads: Mutex<u32>,
        fail: u32,
    }

    impl SecretStore for Counting {
        fn get(&self, key: &str) -> Result<Option<String>, SecretError> {
            let mut reads = self.reads.lock().unwrap();
            *reads += 1;
            if *reads <= self.fail {
                return Err(SecretError("denied".into()));
            }
            self.inner.get(key)
        }
        fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
            self.inner.set(key, value)
        }
        fn delete(&self, key: &str) -> Result<(), SecretError> {
            self.inner.delete(key)
        }
    }

    fn counting(fail: u32) -> CachedSecrets<Counting> {
        CachedSecrets::new(Counting {
            inner: MemorySecrets::default(),
            reads: Mutex::new(0),
            fail,
        })
    }

    #[test]
    fn cached_secrets_read_the_inner_store_once_per_key() {
        let c = counting(0);
        c.inner.inner.set("k", "v").unwrap();
        for _ in 0..5 {
            assert_eq!(c.get("k").unwrap().as_deref(), Some("v"));
            assert_eq!(c.get("missing").unwrap(), None);
        }
        assert_eq!(*c.inner.reads.lock().unwrap(), 2);
    }

    #[test]
    fn cached_secrets_follow_writes_and_retry_failed_reads() {
        let c = counting(1);
        assert!(c.get("k").is_err());
        assert_eq!(c.get("k").unwrap(), None);
        c.set("k", "v").unwrap();
        assert_eq!(c.get("k").unwrap().as_deref(), Some("v"));
        c.delete("k").unwrap();
        assert_eq!(c.get("k").unwrap(), None);
        assert_eq!(c.inner.inner.get("k").unwrap(), None);
        assert_eq!(*c.inner.reads.lock().unwrap(), 2);
    }
}
