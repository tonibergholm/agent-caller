//! Files the service keeps (omarchy spec §4.1) and the `settings` snapshot
//! (§5.4).
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use acd_client::{
    inbox::Inbox,
    settings::{LOCAL_KEY, Settings, SettingsError},
};
use serde::{Deserialize, Serialize};

use crate::protocol::SettingsSnapshot;

pub const SOCKET_NAME: &str = "acd-inbox.sock";
pub const LOCK_NAME: &str = "acd-inbox.lock";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub settings: PathBuf,
    pub service: PathBuf,
    pub runtime_dir: PathBuf,
    pub socket: PathBuf,
    pub lock: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum PathsError {
    #[error("XDG_RUNTIME_DIR is not set; acd-inbox runs inside a user session")]
    NoRuntimeDir,
    #[error("neither XDG_CONFIG_HOME nor HOME is set")]
    NoConfigHome,
}

impl Paths {
    pub fn new(config_home: &Path, runtime_dir: &Path) -> Paths {
        let config_dir = config_home.join("acd-inbox");
        Paths {
            settings: config_dir.join("settings.json"),
            service: config_dir.join("service.json"),
            config_dir,
            runtime_dir: runtime_dir.to_path_buf(),
            socket: runtime_dir.join(SOCKET_NAME),
            lock: runtime_dir.join(LOCK_NAME),
        }
    }

    /// `$XDG_CONFIG_HOME` (else `~/.config`) and `$XDG_RUNTIME_DIR`.
    pub fn from_env() -> Result<Paths, PathsError> {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .filter(|v| !v.is_empty())
            .ok_or(PathsError::NoRuntimeDir)?;
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .ok_or(PathsError::NoConfigHome)?;
        Ok(Paths::new(&config, Path::new(&runtime)))
    }
}

/// `service.json` (omarchy spec §4.1). No secrets.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Prefs {
    #[serde(default = "on")]
    pub notifications: bool,
}

fn on() -> bool {
    true
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs { notifications: true }
    }
}

impl Prefs {
    /// A missing or unreadable file gives the defaults.
    pub fn load(path: &Path) -> Prefs {
        match std::fs::read(path) {
            Ok(raw) => serde_json::from_slice(&raw).unwrap_or_else(|_| {
                tracing::warn!("service.json is unreadable; using defaults");
                Prefs::default()
            }),
            Err(_) => Prefs::default(),
        }
    }

    /// Atomic write, mode 0600, parent directory 0700.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let tmp = path.with_extension("json.tmp");
        let _ = std::fs::remove_file(&tmp);
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        f.write_all(&serde_json::to_vec_pretty(self).expect("serializable"))?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    }
}

/// This host's first DNS label, at most 64 characters; else "This computer"
/// (plan D18).
pub fn local_name(hostname: &str) -> String {
    let label = hostname.split('.').next().unwrap_or("").trim();
    if label.is_empty() {
        return "This computer".into();
    }
    label.chars().take(64).collect()
}

/// Loads `settings.json`. On first run it writes one whose local machine is
/// named `local_name`, not "This Mac" (plan S6).
pub fn init_settings(path: &Path, local_name: &str) -> Result<Settings, SettingsError> {
    if std::fs::symlink_metadata(path).is_err() {
        let mut fresh = Settings::fresh();
        if let Some(local) = fresh.machines.iter_mut().find(|m| m.key == LOCAL_KEY) {
            local.name = local_name.to_owned();
        }
        fresh.save(path)?;
    }
    Settings::load_or_init(path)
}

/// The `settings` message. Calls `Inbox::machines`, which asks the
/// SecretStore for each machine's status: run it off the async workers.
pub fn snapshot(inbox: &Inbox, prefs: Prefs) -> SettingsSnapshot {
    let s = inbox.settings();
    SettingsSnapshot {
        machines: inbox.machines(),
        notifications: prefs.notifications,
        show_idle: s.show_idle,
        show_subagents: s.show_subagents,
        show_hook_only: s.show_hook_only,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acd_client::{
        Timing,
        inbox::InboxOptions,
        settings::{EndpointConfig, MachineEntry, MachineSecret, MemorySecrets, SecretStore},
    };
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;

    const TOKEN: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8";
    const PIN: &str = "sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

    #[test]
    fn paths_follow_the_spec_layout() {
        let p = Paths::new(Path::new("/home/u/.config"), Path::new("/run/user/1000"));
        assert_eq!(p.settings, Path::new("/home/u/.config/acd-inbox/settings.json"));
        assert_eq!(p.service, Path::new("/home/u/.config/acd-inbox/service.json"));
        assert_eq!(p.socket, Path::new("/run/user/1000/acd-inbox.sock"));
        assert_eq!(p.lock, Path::new("/run/user/1000/acd-inbox.lock"));
    }

    #[test]
    fn prefs_default_on_survive_a_round_trip_and_are_private() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("acd-inbox/service.json");
        assert_eq!(Prefs::load(&path), Prefs { notifications: true });
        Prefs { notifications: false }.save(&path).unwrap();
        assert_eq!(Prefs::load(&path), Prefs { notifications: false });
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(Prefs::load(&path), Prefs::default());
    }

    #[test]
    fn first_run_names_the_local_machine_after_this_host() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("acd-inbox/settings.json");
        let s = init_settings(&path, &local_name("omarchy.tail1234.ts.net")).unwrap();
        assert_eq!(s.machines[0].name, "omarchy");
        let again = init_settings(&path, "other").unwrap();
        assert_eq!(again.machines[0].name, "omarchy", "an existing file is kept");
        assert_eq!(local_name(""), "This computer");
        assert_eq!(local_name(&"h".repeat(100)).chars().count(), 64);
    }

    #[test]
    fn the_snapshot_has_status_and_toggles_but_no_token_or_pin() {
        let secrets = Arc::new(MemorySecrets::default());
        secrets.set("m1", &MachineSecret { token: TOKEN.into(), pin: PIN.into() }.to_json()).unwrap();
        let settings = Settings {
            show_subagents: true,
            machines: vec![MachineEntry {
                key: "m1".into(),
                name: "Box".into(),
                endpoint: EndpointConfig::Tcp { host: "box".into(), port: 47800 },
            }],
            ..Settings::fresh()
        };
        let inbox = acd_client::inbox::Inbox::new(
            settings,
            secrets,
            InboxOptions { timing: Timing::default(), settings_path: None, client_name: "t".into(), client_version: "0".into() },
        );
        let s = snapshot(&inbox, Prefs { notifications: false });
        let line = crate::protocol::settings_line(&s, crate::protocol::MAX_LINE);
        assert!(!line.contains(TOKEN) && !line.contains("sha256:"), "secrets in the settings line");
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["machines"][0]["secret"], "ok");
        assert_eq!(v["notifications"], false);
        assert_eq!(v["show_subagents"], true);
        assert!(v["machines"][0].get("status").is_none(), "connection status stays in the view (S1)");
    }
}
