//! Wire types of the relay API (spec §5.3). No request content and no free
//! text exists in any of them; every request refuses unknown fields.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `POST /v1/wake`. The whole body: a version and a handle.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WakeRequest {
    pub v: u8,
    pub handle: String,
}

/// `POST /v1/handles`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RegisterRequest {
    pub platform: String,
    pub app: String,
    pub address_kind: String,
    pub address: String,
}

/// `POST /v1/handles/confirm`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfirmRequest {
    pub challenge_id: String,
    pub code: String,
    pub platform: String,
    pub app: String,
    pub address_kind: String,
    pub address: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChallengeIssued {
    pub challenge_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HandleIssued {
    pub handle: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Android = 1,
    Ios = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressKind {
    FcmToken = 1,
    FcmFid = 2,
    Apns = 3,
}

impl Platform {
    pub fn parse(s: &str) -> Option<Platform> {
        match s {
            "android" => Some(Platform::Android),
            "ios" => Some(Platform::Ios),
            _ => None,
        }
    }
    pub fn from_u8(b: u8) -> Option<Platform> {
        match b {
            1 => Some(Platform::Android),
            2 => Some(Platform::Ios),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Platform::Android => "android",
            Platform::Ios => "ios",
        }
    }
}

impl AddressKind {
    pub fn parse(s: &str) -> Option<AddressKind> {
        match s {
            "fcm_token" => Some(AddressKind::FcmToken),
            "fcm_fid" => Some(AddressKind::FcmFid),
            "apns" => Some(AddressKind::Apns),
            _ => None,
        }
    }
    pub fn from_u8(b: u8) -> Option<AddressKind> {
        match b {
            1 => Some(AddressKind::FcmToken),
            2 => Some(AddressKind::FcmFid),
            3 => Some(AddressKind::Apns),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            AddressKind::FcmToken => "fcm_token",
            AddressKind::FcmFid => "fcm_fid",
            AddressKind::Apns => "apns",
        }
    }
    /// Which platform this kind belongs to.
    pub fn platform(self) -> Platform {
        match self {
            AddressKind::FcmToken | AddressKind::FcmFid => Platform::Android,
            AddressKind::Apns => Platform::Ios,
        }
    }
}

pub const MAX_APP_LEN: usize = 64;
pub const MAX_ADDRESS_LEN: usize = 255;
pub const MAX_HANDLE_LEN: usize = 512;

/// Address charset: what FCM tokens, installation ids and APNs hex tokens
/// use. Keeps control characters out of logs and URLs.
pub fn address_ok(a: &str) -> bool {
    !a.is_empty()
        && a.len() <= MAX_ADDRESS_LEN
        && a.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b':' | b'.' | b'-'))
}

pub fn app_ok(a: &str) -> bool {
    !a.is_empty()
        && a.len() <= MAX_APP_LEN
        && a.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wake_request_has_exactly_v_and_handle() {
        let schema = schemars::schema_for!(WakeRequest);
        let v = serde_json::to_value(&schema).unwrap();
        let props = v["properties"].as_object().unwrap();
        let mut keys: Vec<&str> = props.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(keys, ["handle", "v"]);
        // Both are scalars: no free-text object or array can hide in them.
        assert_eq!(v["additionalProperties"], serde_json::json!(false));
    }

    #[test]
    fn wake_request_rejects_extra_text_field() {
        let r: Result<WakeRequest, _> =
            serde_json::from_str(r#"{"v":1,"handle":"abc","text":"approve the deploy"}"#);
        assert!(r.is_err());
        let ok: Result<WakeRequest, _> = serde_json::from_str(r#"{"v":1,"handle":"abc"}"#);
        assert!(ok.is_ok());
    }

    #[test]
    fn wake_request_rejects_wrong_version() {
        // The type accepts any u8; the server refuses v != 1 (see api tests).
        // Here: a string or float version never parses.
        assert!(serde_json::from_str::<WakeRequest>(r#"{"v":"1","handle":"a"}"#).is_err());
        assert!(serde_json::from_str::<WakeRequest>(r#"{"v":1.5,"handle":"a"}"#).is_err());
    }
}
