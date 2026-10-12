//! Voice switches (voice spec §8, the step-1 subset), in `voice.json` next
//! to `settings.json` (D12). Only switches live here, never voice content.
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use serde::{Deserialize, Serialize};

/// The voice toggle's default (plan S12). The inbox hotkey is Control+Alt+Space.
pub const DEFAULT_VOICE_TOGGLE: &str = "Control+Alt+V";

/// The key held to talk (spec §8, plan S12).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HoldKey {
    #[default]
    RightOption,
    LeftOption,
    RightCommand,
    RightControl,
    Fn,
    Off,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceSettings {
    /// Off by default (spec §8).
    #[serde(default)]
    pub enabled: bool,
    /// Show the typed command field in the inbox panel (D13) and the probe (S20).
    #[serde(default)]
    pub debug_input: bool,
    #[serde(default)]
    pub hold_key: HoldKey,
    /// `None` turns the toggle off; a missing field means the default.
    #[serde(default = "default_toggle")]
    pub toggle_shortcut: Option<String>,
}

fn default_toggle() -> Option<String> {
    Some(DEFAULT_VOICE_TOGGLE.to_owned())
}

impl Default for VoiceSettings {
    fn default() -> Self {
        Self { enabled: false, debug_input: false, hold_key: HoldKey::default(), toggle_shortcut: default_toggle() }
    }
}

impl VoiceSettings {
    /// A missing or unreadable file means the defaults: voice off.
    pub fn load(path: &Path) -> Self {
        fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    /// Atomic write, mode 0600, parent directory 0700 (as `Settings::save`).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        }
        let tmp = path.with_extension("json.tmp");
        let _ = fs::remove_file(&tmp);
        let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        f.write_all(serde_json::to_string_pretty(self).expect("serializable").as_bytes())?;
        f.sync_all()?;
        fs::rename(&tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn voice_is_off_until_saved_on() {
        let dir = std::env::temp_dir().join(format!("agent-caller-voice-{}", std::process::id()));
        let path = dir.join("voice.json");
        assert_eq!(VoiceSettings::load(&path), VoiceSettings::default());
        let on = VoiceSettings { enabled: true, debug_input: true, ..VoiceSettings::default() };
        on.save(&path).unwrap();
        assert_eq!(VoiceSettings::load(&path), on);
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(VoiceSettings::load(&path), VoiceSettings::default(), "unreadable means off");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_old_voice_json_still_loads_with_the_new_defaults() {
        let dir = std::env::temp_dir().join(format!("agent-caller-voice-old-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("voice.json");
        std::fs::write(&path, r#"{"enabled":true,"debug_input":false}"#).unwrap();
        let s = VoiceSettings::load(&path);
        assert!(s.enabled);
        assert_eq!(s.hold_key, HoldKey::RightOption);
        assert_eq!(s.toggle_shortcut.as_deref(), Some(DEFAULT_VOICE_TOGGLE));
        std::fs::write(&path, r#"{"enabled":true,"hold_key":"off","toggle_shortcut":null}"#).unwrap();
        let s = VoiceSettings::load(&path);
        assert_eq!((s.hold_key, s.toggle_shortcut), (HoldKey::Off, None), "turned off stays off");
        let _ = std::fs::remove_dir_all(dir);
    }
}
