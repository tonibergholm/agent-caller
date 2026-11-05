//! The wake sender `acd` uses (spec §2.3, §5.2, §5.4). Not wired to the
//! engine yet (P5b): this is the transport, the retry and disable rules and
//! the fail-closed configuration.
//!
//! Redirects are never followed, proxy variables are ignored, response bodies
//! are read up to 1 KiB and never logged.

use std::sync::Arc;
use std::time::Duration;

use crate::http::{HttpClient, HttpError};
use crate::wire::WakeRequest;
use crate::Clock;

pub const WAKE_TIMEOUT: Duration = Duration::from_secs(5);
pub const MAX_RESPONSE: usize = 1024;
pub const BACKOFF_BASE_SECS: u64 = 10;
pub const BACKOFF_CAP_SECS: u64 = 900;

/// A validated relay base URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayUrl(String);

impl RelayUrl {
    /// `https://host[:port][/prefix]`, no userinfo, query or fragment.
    pub fn parse(s: &str) -> Result<RelayUrl, String> {
        let uri: hyper::Uri = s.parse().map_err(|_| "relay is not a valid URL".to_string())?;
        if uri.scheme_str() != Some("https") {
            return Err("relay must be an https:// URL".into());
        }
        let auth = uri.authority().ok_or("relay has no host")?;
        if auth.as_str().contains('@') || uri.host().is_none_or(str::is_empty) {
            return Err("relay must have a host and no userinfo".into());
        }
        if uri.query().is_some() || s.contains('#') || s.contains('?') {
            return Err("relay must have no query or fragment".into());
        }
        Ok(RelayUrl(s.trim_end_matches('/').to_string()))
    }

    /// Test builds only: also accepts `http://` (a loopback fake relay).
    pub fn for_tests(s: &str) -> RelayUrl {
        RelayUrl(s.trim_end_matches('/').to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn wake_url(&self) -> String {
        format!("{}/v1/wake", self.0)
    }
}

/// What the `[push]` section of `config.toml` means (spec §5.2).
#[derive(Debug, PartialEq, Eq)]
pub enum PushSetting {
    /// Push off. `None`: not configured. `Some`: configured wrongly (reason
    /// for `acd status`; never the value).
    Off(Option<String>),
    On(RelayUrl),
}

/// Fail closed: anything but a clean `relay = "https://..."` turns push off.
pub fn resolve(section: Option<&toml::Value>) -> PushSetting {
    let Some(v) = section else {
        return PushSetting::Off(None);
    };
    let off = |why: &str| PushSetting::Off(Some(why.to_string()));
    let Some(t) = v.as_table() else {
        return off("[push] is not a table");
    };
    if t.keys().any(|k| k != "relay" && k != "relay_pin") {
        return off("[push] has an unknown key");
    }
    if t.contains_key("relay_pin") {
        return off("relay_pin is not supported by this build");
    }
    let Some(relay) = t.get("relay") else {
        return PushSetting::Off(None);
    };
    let Some(relay) = relay.as_str() else {
        return off("[push] relay is not a string");
    };
    match RelayUrl::parse(relay) {
        Ok(u) => PushSetting::On(u),
        Err(e) => off(&e),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    /// 404 `gone`: disabled.
    Expired,
    /// 400 `invalid_handle`: disabled.
    Invalid,
    /// Transient failure: still enabled, with backoff.
    RelayError,
}

/// Per-registration sender state (kept next to the handle in `push.json`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeState {
    pub status: Status,
    pub failures: u32,
    pub retry_after: u64,
    pub last_wake_at: Option<u64>,
}

impl Default for WakeState {
    fn default() -> Self {
        WakeState { status: Status::Ok, failures: 0, retry_after: 0, last_wake_at: None }
    }
}

impl WakeState {
    /// The device registered again (or `acd push test` succeeded).
    pub fn reregistered(&mut self) {
        *self = WakeState::default();
    }

    pub fn disabled(&self) -> bool {
        matches!(self.status, Status::Expired | Status::Invalid)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Sent,
    /// 429: the wake is dropped, nothing else changes.
    RateLimited,
    /// Inside the backoff window: nothing was sent.
    Skipped,
    /// The registration is disabled: nothing was sent.
    Disabled,
    /// A transient failure; the registration stays enabled.
    Failed,
    /// The relay said the registration is invalid; now disabled.
    Invalidated,
}

#[derive(Debug, PartialEq, Eq)]
enum Class {
    Success,
    Gone,
    InvalidHandle,
    RateLimited,
    Transient,
}

fn error_code(body: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    Some(v.get("error")?.as_str()?.to_string())
}

fn classify(res: Result<(u16, Vec<u8>), HttpError>) -> Class {
    let Ok((status, body)) = res else {
        return Class::Transient;
    };
    match status {
        204 => Class::Success,
        429 => Class::RateLimited,
        404 if error_code(&body).as_deref() == Some("gone") => Class::Gone,
        400 if error_code(&body).as_deref() == Some("invalid_handle") => Class::InvalidHandle,
        _ => Class::Transient,
    }
}

pub struct WakeSender {
    http: HttpClient,
    url: RelayUrl,
    clock: Arc<dyn Clock>,
}

impl WakeSender {
    pub fn new(http: HttpClient, url: RelayUrl, clock: Arc<dyn Clock>) -> WakeSender {
        WakeSender { http, url, clock }
    }

    /// An HTTPS sender that trusts only the certificates in `ca_pem`.
    pub fn https(url: RelayUrl, ca_pem: &[u8], clock: Arc<dyn Clock>) -> Result<WakeSender, HttpError> {
        Ok(WakeSender::new(HttpClient::https(ca_pem, WAKE_TIMEOUT, MAX_RESPONSE)?, url, clock))
    }

    /// Send one wake for `handle` and update `state` by the rules of §5.4.
    pub async fn wake(&self, handle: &str, state: &mut WakeState) -> Outcome {
        let now = self.clock.now_secs();
        if state.disabled() {
            return Outcome::Disabled;
        }
        if now < state.retry_after {
            return Outcome::Skipped;
        }
        let body = serde_json::to_vec(&WakeRequest { v: 1, handle: handle.to_string() }).expect("json");
        let res = self
            .http
            .post(&self.url.wake_url(), "application/json", &[], body)
            .await
            .map(|r| (r.status, r.body));
        match classify(res) {
            Class::Success => {
                *state = WakeState { last_wake_at: Some(now), ..WakeState::default() };
                Outcome::Sent
            }
            Class::RateLimited => Outcome::RateLimited,
            Class::Gone => {
                state.status = Status::Expired;
                Outcome::Invalidated
            }
            Class::InvalidHandle => {
                state.status = Status::Invalid;
                Outcome::Invalidated
            }
            Class::Transient => {
                state.failures = state.failures.saturating_add(1);
                state.status = Status::RelayError;
                // Ten seconds since the failure, not since the start of the request.
                state.retry_after = self.clock.now_secs() + backoff_secs(state.failures);
                Outcome::Failed
            }
        }
    }
}

/// `10 s * 2^(n-1)`, capped at 900 s.
pub fn backoff_secs(failures: u32) -> u64 {
    let exp = failures.saturating_sub(1).min(16);
    (BACKOFF_BASE_SECS << exp).min(BACKOFF_CAP_SECS)
}
