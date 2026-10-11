//! Voice switches (voice spec §8, the step-1 subset), in `voice.json` next
//! to `settings.json` (D12). Only switches live here, never voice content.
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceSettings {
    /// Off by default (spec §8).
    #[serde(default)]
    pub enabled: bool,
    /// Show the typed command field in the inbox panel (D13).
    #[serde(default)]
    pub debug_input: bool,
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
        let on = VoiceSettings { enabled: true, debug_input: true };
        on.save(&path).unwrap();
        assert_eq!(VoiceSettings::load(&path), on);
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(VoiceSettings::load(&path), VoiceSettings::default(), "unreadable means off");
        let _ = std::fs::remove_dir_all(dir);
    }
}
