//! What Settings shows about voice (spec §6.4, §8, §9). No voice content.
use acd_voice::listen::{Capabilities, Unavailable};
use acd_voice::router::model::ModelState;
use serde::{Deserialize, Serialize};

use super::permissions::{Permission, PermissionState, SPEECH_PERMISSION_REQUIRED, TAP_NEEDS_RELAUNCH};

/// A voice Settings can offer. Names come from the system, not from voice content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceOption {
    pub id: String,
    pub name: String,
    pub language: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VoiceStatus {
    pub unavailable: Option<Unavailable>,
    pub streams_partials: bool,
    pub locale: Option<String>,
    pub microphone: PermissionState,
    /// `None` when Speech Recognition is not needed (plan S14).
    pub speech: Option<PermissionState>,
    pub input_monitoring: PermissionState,
    pub hold_key_active: bool,
    /// Input Monitoring is granted but the tap waits for a relaunch (S17).
    pub relaunch_for_hold_key: bool,
    /// Why the voice shortcut could not be registered. The previous voice
    /// shortcut stays live.
    pub toggle_error: Option<String>,
    /// The on-device intent model (spec §9). Free-form commands need it.
    pub model: ModelState,
    /// The English voices Settings can offer (spec §8).
    pub voices: Vec<VoiceOption>,
}

/// A plain constructor: every argument is one field of the status, and the
/// two production callers (macOS and the rest) spell them all out.
#[allow(clippy::too_many_arguments)]
pub fn build(
    caps: Capabilities,
    locale: Option<String>,
    perm: impl Fn(Permission) -> PermissionState,
    hold_key_active: bool,
    hold_key_wanted: bool,
    toggle_error: Option<String>,
    model: ModelState,
    voices: Vec<VoiceOption>,
) -> VoiceStatus {
    let input_monitoring = perm(Permission::InputMonitoring);
    VoiceStatus {
        unavailable: caps.unavailable,
        streams_partials: caps.streams_partials,
        locale,
        microphone: perm(Permission::Microphone),
        speech: SPEECH_PERMISSION_REQUIRED.then(|| perm(Permission::Speech)),
        input_monitoring,
        hold_key_active,
        relaunch_for_hold_key: TAP_NEEDS_RELAUNCH
            && hold_key_wanted
            && !hold_key_active
            && input_monitoring == PermissionState::Granted,
        toggle_error,
        model,
        voices,
    }
}

#[cfg(target_os = "macos")]
pub fn current(app: &tauri::AppHandle) -> VoiceStatus {
    use acd_voice::listen::SpeechInput;
    use tauri::Manager;

    use super::settings::HoldKey;

    let voice = app.state::<super::VoiceState>();
    let settings = voice.settings.lock().unwrap().clone();
    build(
        voice.speech.capabilities(),
        voice.speech.locale(),
        super::permissions::status,
        super::trigger::hold_active(),
        settings.enabled && settings.hold_key != HoldKey::Off,
        voice.trigger_error.lock().unwrap().clone(),
        voice.model.state(),
        voice.voices.lock().unwrap().clone(),
    )
}

/// Voice needs macOS.
#[cfg(not(target_os = "macos"))]
pub fn current(_app: &tauri::AppHandle) -> VoiceStatus {
    let caps = Capabilities { unavailable: Some(Unavailable::NotMac), streams_partials: false, sample_rate: 0 };
    build(
        caps,
        None,
        |_| PermissionState::NotDetermined,
        false,
        false,
        None,
        ModelState::Unavailable(acd_voice::router::model::ModelUnavailable::NotMac),
        Vec::new(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use acd_voice::router::model::{ModelState, ModelUnavailable};

    fn caps(unavailable: Option<Unavailable>) -> Capabilities {
        Capabilities { unavailable, streams_partials: true, sample_rate: 16_000 }
    }

    #[test]
    fn the_status_reports_availability_and_each_permission() {
        let s = build(
            caps(None),
            Some("en_GB".into()),
            |p| match p {
                Permission::Microphone => PermissionState::Granted,
                Permission::Speech => PermissionState::Denied,
                Permission::InputMonitoring => PermissionState::NotDetermined,
            },
            false,
            true,
            None,
            ModelState::Unavailable(ModelUnavailable::IntelligenceOff),
            Vec::new(),
        );
        assert_eq!(s.unavailable, None);
        assert_eq!(s.locale.as_deref(), Some("en_GB"));
        assert_eq!(s.microphone, PermissionState::Granted);
        assert_eq!(s.input_monitoring, PermissionState::NotDetermined);
        assert_eq!(s.speech.is_some(), SPEECH_PERMISSION_REQUIRED, "Speech Recognition shows only if needed (S14)");
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["input_monitoring"], "not_determined");
        assert_eq!(v["model"], serde_json::json!({"state": "unavailable", "reason": "intelligence_off"}));
        let s = build(
            caps(Some(Unavailable::AssetsMissing)),
            None,
            |_| PermissionState::Granted,
            true,
            true,
            Some("Could not register".into()),
            ModelState::Available,
            Vec::new(),
        );
        assert_eq!(serde_json::to_value(&s).unwrap()["unavailable"], "assets_missing");
        assert_eq!(s.toggle_error.as_deref(), Some("Could not register"));
    }

    /// Plan S17.
    #[test]
    fn a_relaunch_is_asked_for_only_when_it_would_help() {
        let granted = |_| PermissionState::Granted;
        let s = build(caps(None), None, granted, false, true, None, ModelState::Available, Vec::new());
        assert_eq!(s.relaunch_for_hold_key, TAP_NEEDS_RELAUNCH);
        assert!(!build(caps(None), None, granted, true, true, None, ModelState::Available, Vec::new()).relaunch_for_hold_key, "running already");
        assert!(!build(caps(None), None, granted, false, false, None, ModelState::Available, Vec::new()).relaunch_for_hold_key, "no hold key wanted");
        assert!(!build(caps(None), None, |_| PermissionState::Denied, false, true, None, ModelState::Available, Vec::new()).relaunch_for_hold_key);
    }
}
