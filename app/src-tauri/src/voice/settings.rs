//! Voice switches (voice spec §8, the step-1 subset), in `voice.json` next
//! to `settings.json` (D12). Only switches live here, never voice content.
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use acd_voice::speak::{Rate, SpeechSettings};
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
    /// Spoken alerts for new requests (spec §7, §8). Off by default.
    #[serde(default)]
    pub alerts: bool,
    /// A platform voice id; `None` is the system's English voice (plan V15).
    #[serde(default)]
    pub speech_voice: Option<String>,
    #[serde(default)]
    pub speech_rate: Rate,
}

fn default_toggle() -> Option<String> {
    Some(DEFAULT_VOICE_TOGGLE.to_owned())
}

impl Default for VoiceSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            debug_input: false,
            hold_key: HoldKey::default(),
            toggle_shortcut: default_toggle(),
            alerts: false,
            speech_voice: None,
            speech_rate: Rate::default(),
        }
    }
}

impl VoiceSettings {
    /// A platform voice id is short; anything longer is not one (plan V15).
    pub fn speech_voice_is_valid(&self) -> bool {
        self.speech_voice.as_ref().is_none_or(|v| v.len() <= 200)
    }

    /// What the controller needs for spoken output.
    pub fn speech(&self) -> SpeechSettings {
        SpeechSettings { alerts: self.alerts, voice: self.speech_voice.clone(), rate: self.speech_rate }
    }

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

    /// Plan V15: spoken output settings default off and survive old files.
    #[test]
    fn speech_settings_default_and_load_from_an_old_file() {
        use acd_voice::speak::{Rate, SpeechSettings};
        let dir = std::env::temp_dir().join(format!("agent-caller-voice-speech-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("voice.json");
        std::fs::write(&path, r#"{"enabled":true}"#).unwrap();
        let s = VoiceSettings::load(&path);
        // An old file parses (rather than falling back to the defaults, which
        // would also give these speech values): its own field survives.
        assert!(s.enabled, "the old file was read, not replaced by the defaults");
        assert_eq!(s.speech(), SpeechSettings { alerts: false, voice: None, rate: Rate::Normal });
        let on = VoiceSettings { alerts: true, speech_voice: Some("com.apple.voice.Ava".into()), speech_rate: Rate::Fast, ..s };
        on.save(&path).unwrap();
        assert_eq!(VoiceSettings::load(&path), on);
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("\"speech_rate\": \"fast\""), "{raw}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_voice_id_that_is_not_one_is_refused() {
        let ok = VoiceSettings { speech_voice: Some("com.apple.voice.Ava".into()), ..VoiceSettings::default() };
        assert!(ok.speech_voice_is_valid());
        assert!(VoiceSettings::default().speech_voice_is_valid(), "the system voice");
        let long = VoiceSettings { speech_voice: Some("x".repeat(201)), ..VoiceSettings::default() };
        assert!(!long.speech_voice_is_valid());
    }
}
