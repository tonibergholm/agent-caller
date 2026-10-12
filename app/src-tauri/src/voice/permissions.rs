//! Voice permissions (spec §6.4) and the spike's answers (plan S14, S17).
use serde::{Deserialize, Serialize};

/// Whether SpeechTranscriber needs the Speech Recognition permission.
/// From the step-2 spike (S14): it does.
pub const SPEECH_PERMISSION_REQUIRED: bool = true;

/// Whether the event tap works only after a relaunch once Input Monitoring
/// is granted. From the step-2 spike (S17): it does not.
pub const TAP_NEEDS_RELAUNCH: bool = false;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    Microphone,
    Speech,
    InputMonitoring,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionState {
    NotDetermined,
    Granted,
    Denied,
    Restricted,
}

impl Permission {
    /// `VB_PERM_*`.
    pub fn code(self) -> u32 {
        match self {
            Permission::Microphone => 1,
            Permission::Speech => 2,
            Permission::InputMonitoring => 3,
        }
    }

    pub fn from_code(code: u32) -> Option<Self> {
        match code {
            1 => Some(Permission::Microphone),
            2 => Some(Permission::Speech),
            3 => Some(Permission::InputMonitoring),
            _ => None,
        }
    }

    /// The System Settings pane for this permission (plan S19).
    // Opened from Settings (Task 9).
    #[allow(dead_code)]
    pub fn privacy_url(self) -> &'static str {
        match self {
            Permission::Microphone => "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone",
            Permission::Speech => "x-apple.systempreferences:com.apple.preference.security?Privacy_SpeechRecognition",
            Permission::InputMonitoring => "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent",
        }
    }
}

impl PermissionState {
    /// `VB_PS_*`.
    pub fn from_code(code: u32) -> Self {
        match code {
            1 => PermissionState::Granted,
            2 => PermissionState::Denied,
            3 => PermissionState::Restricted,
            _ => PermissionState::NotDetermined,
        }
    }
}

/// The permissions to ask for when voice is turned on (spec §6.4): those
/// voice needs that the user was never asked about. The microphone always,
/// Speech Recognition if the spike said so, Input Monitoring only with a
/// hold key.
pub fn to_request(hold_key_on: bool, state: impl Fn(Permission) -> PermissionState) -> Vec<Permission> {
    let mut needed = vec![Permission::Microphone];
    if SPEECH_PERMISSION_REQUIRED {
        needed.push(Permission::Speech);
    }
    if hold_key_on {
        needed.push(Permission::InputMonitoring);
    }
    needed.into_iter().filter(|p| state(*p) == PermissionState::NotDetermined).collect()
}

#[cfg(target_os = "macos")]
pub fn status(p: Permission) -> PermissionState {
    // SAFETY: a plain integer in and out (header contract).
    PermissionState::from_code(unsafe { super::bridge::ffi::vb_permission_status(p.code()) })
}

/// Shows the system prompt if never asked; the answer arrives as
/// `BridgeEvent::Permission`.
#[cfg(target_os = "macos")]
pub fn request(p: Permission) {
    // SAFETY: a plain integer; returns at once.
    unsafe { super::bridge::ffi::vb_permission_request(p.code()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_round_trip() {
        for p in [Permission::Microphone, Permission::Speech, Permission::InputMonitoring] {
            assert_eq!(Permission::from_code(p.code()), Some(p));
        }
        assert_eq!(Permission::from_code(0), None);
        assert_eq!(PermissionState::from_code(1), PermissionState::Granted);
        assert_eq!(PermissionState::from_code(2), PermissionState::Denied);
        assert_eq!(PermissionState::from_code(3), PermissionState::Restricted);
        assert_eq!(PermissionState::from_code(42), PermissionState::NotDetermined);
    }

    /// Plan S19: only three fixed panes can be opened.
    #[test]
    fn privacy_urls_are_fixed_settings_panes() {
        assert_eq!(Permission::Microphone.privacy_url(), "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone");
        assert_eq!(Permission::Speech.privacy_url(), "x-apple.systempreferences:com.apple.preference.security?Privacy_SpeechRecognition");
        assert_eq!(Permission::InputMonitoring.privacy_url(), "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent");
    }

    /// Spec §6.4: turning voice on asks only for what voice needs and the
    /// user was never asked about.
    #[test]
    fn turning_voice_on_asks_only_what_is_needed_and_unasked() {
        use PermissionState::*;
        let all_unasked = to_request(true, |_| NotDetermined);
        assert!(all_unasked.contains(&Permission::Microphone));
        assert!(all_unasked.contains(&Permission::InputMonitoring));
        assert_eq!(all_unasked.contains(&Permission::Speech), SPEECH_PERMISSION_REQUIRED);
        assert!(!to_request(false, |_| NotDetermined).contains(&Permission::InputMonitoring), "no hold key, no Input Monitoring");
        assert!(to_request(true, |_| Granted).is_empty());
        assert!(to_request(true, |_| Denied).is_empty(), "a denial is not asked again; Settings links to System Settings");
    }

    #[test]
    fn states_serialize_for_settings() {
        assert_eq!(serde_json::to_value(PermissionState::NotDetermined).unwrap(), "not_determined");
        assert_eq!(serde_json::to_value(Permission::InputMonitoring).unwrap(), "input_monitoring");
    }
}
