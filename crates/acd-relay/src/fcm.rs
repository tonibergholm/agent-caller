//! Push sending: the `PushSender` trait, the FCM HTTP v1 bodies and client.
//!
//! The real client reads its credentials from a file path. The service
//! account key, the signed assertion and the access token never reach a log
//! line or an error string.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::STANDARD, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use ring::rand::SystemRandom;
use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
use serde_json::{Value, json};

use crate::http::HttpClient;
use crate::provisional::{WAKE_BODY, WAKE_TITLE};
use crate::wire::{AddressKind, Platform};
use crate::Clock;

/// A push address as opened from a handle or sent in a registration.
#[derive(Clone, PartialEq, Eq)]
pub struct Address {
    pub kind: AddressKind,
    pub value: String,
}

impl std::fmt::Debug for Address {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Address({:?}, len {})", self.kind, self.value.len())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendError {
    /// The push service says the address is dead.
    Gone,
    /// Anything else: outage, quota, auth, a rejected request.
    Unavailable,
}

#[async_trait]
pub trait PushSender: Send + Sync {
    fn supports(&self, platform: Platform) -> bool;
    async fn wake(&self, to: &Address) -> Result<(), SendError>;
    async fn challenge(&self, to: &Address, code: &str) -> Result<(), SendError>;
}

fn target(a: &Address) -> (&'static str, &str) {
    match a.kind {
        AddressKind::FcmFid => ("fid", &a.value),
        _ => ("token", &a.value),
    }
}

/// The wake body (spec §3.2). Fixed text only (P-A).
pub fn wake_body(to: &Address) -> Value {
    let (k, v) = target(to);
    json!({"message": {
        k: v,
        "data": {"v": "1"},
        "androidV2": {
            "collapseKey": "acd-wake",
            "ttl": "3600s",
            "remoteNotification": {"notification": {
                "title": WAKE_TITLE,
                "body": WAKE_BODY,
                "tag": "acd-wake",
                "channelId": "acd_wake",
                "visibility": "PRIVATE"
            }}
        }
    }})
}

/// The challenge body (spec §3.2): data only.
pub fn challenge_body(to: &Address, code: &str) -> Value {
    let (k, v) = target(to);
    json!({"message": {
        k: v,
        "data": {"v": "1", "t": "challenge", "c": code},
        "androidV2": {"ttl": "60s", "backgroundSync": {}}
    }})
}

/// Map an FCM error response (spec §5.3.5). Only `UNREGISTERED` is a dead
/// address; `INVALID_ARGUMENT`, `NOT_FOUND` (can be a wrong project) and all
/// the rest are `Unavailable`.
pub fn map_fcm_error(_status: u16, body: &[u8]) -> SendError {
    let Ok(v) = serde_json::from_slice::<Value>(body) else {
        return SendError::Unavailable;
    };
    let err = &v["error"];
    let unregistered = err["status"] == "UNREGISTERED"
        || err["details"]
            .as_array()
            .is_some_and(|d| d.iter().any(|x| x["errorCode"] == "UNREGISTERED"));
    if unregistered { SendError::Gone } else { SendError::Unavailable }
}

/// `token_uri` must be usable: non-empty, `https`, a host, no userinfo
/// (spec §5.7). The error never echoes it. `allow_plain` is for tests.
fn check_token_uri(uri: &str, allow_plain: bool) -> Result<(), String> {
    let bad = || "credentials file: invalid token_uri".to_string();
    let u: hyper::Uri = uri.parse().map_err(|_| bad())?;
    let scheme_ok = u.scheme_str() == Some("https") || (allow_plain && u.scheme_str() == Some("http"));
    let authority_ok = u.authority().is_some_and(|a| !a.as_str().contains('@'));
    if uri.is_empty() || !scheme_ok || !authority_ok || u.host().is_none_or(str::is_empty) {
        return Err(bad());
    }
    Ok(())
}

pub struct ServiceAccount {
    client_email: String,
    private_key: Vec<u8>,
    token_uri: String,
}

impl std::fmt::Debug for ServiceAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ServiceAccount(..)")
    }
}

impl ServiceAccount {
    /// Read the credentials file. It must be owned by this user, mode 0600 or
    /// stricter.
    pub fn from_file(path: &std::path::Path) -> Result<ServiceAccount, String> {
        let raw = acd_securefs::read_private(path, 0o600, 64 * 1024)
            .map_err(|e| format!("credentials file: {e}"))?;
        ServiceAccount::from_json(&raw)
    }

    pub fn from_json(raw: &[u8]) -> Result<ServiceAccount, String> {
        #[derive(serde::Deserialize)]
        struct F {
            client_email: String,
            private_key: String,
            token_uri: String,
        }
        let f: F = serde_json::from_slice(raw).map_err(|_| "credentials file: invalid json".to_string())?;
        check_token_uri(&f.token_uri, false)?;
        Self::build(f.client_email, f.private_key, f.token_uri)
    }

    /// Like [`ServiceAccount::from_json`] but lets `token_uri` be plain
    /// `http://`, for tests against local fakes. Production uses `from_json`.
    pub fn from_json_for_tests(raw: &[u8]) -> Result<ServiceAccount, String> {
        #[derive(serde::Deserialize)]
        struct F {
            client_email: String,
            private_key: String,
            token_uri: String,
        }
        let f: F = serde_json::from_slice(raw).map_err(|_| "credentials file: invalid json".to_string())?;
        check_token_uri(&f.token_uri, true)?;
        Self::build(f.client_email, f.private_key, f.token_uri)
    }

    fn build(client_email: String, private_key: String, token_uri: String) -> Result<ServiceAccount, String> {
        let mut der_b64 = String::new();
        for l in private_key.lines() {
            if !l.starts_with("-----") {
                der_b64.push_str(l.trim());
            }
        }
        let der = STANDARD
            .decode(der_b64)
            .map_err(|_| "credentials file: invalid private key".to_string())?;
        RsaKeyPair::from_pkcs8(&der).map_err(|_| "credentials file: invalid private key".to_string())?;
        Ok(ServiceAccount { client_email, private_key: der, token_uri })
    }

    fn assertion(&self, now: u64) -> Result<String, ()> {
        let key = RsaKeyPair::from_pkcs8(&self.private_key).map_err(|_| ())?;
        let head = B64.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
        let claims = json!({
            "iss": self.client_email,
            "scope": "https://www.googleapis.com/auth/firebase.messaging",
            "aud": self.token_uri,
            "iat": now,
            "exp": now + 3600
        });
        let signing = format!("{head}.{}", B64.encode(claims.to_string()));
        let mut sig = vec![0u8; key.public().modulus_len()];
        key.sign(&RSA_PKCS1_SHA256, &SystemRandom::new(), signing.as_bytes(), &mut sig)
            .map_err(|_| ())?;
        Ok(format!("{signing}.{}", B64.encode(sig)))
    }
}

struct Token {
    value: String,
    expires_at: u64,
}

/// What the token mutex guards.
#[derive(Default)]
struct TokenState {
    token: Option<Token>,
    /// Refreshes are refused until this time (seconds on the clock) after a
    /// failed or abandoned one, so a stalled or refusing OAuth endpoint is
    /// asked once, not once per send.
    retry_after: u64,
}

/// Seconds a failed token refresh is remembered.
pub const REFRESH_BACKOFF_SECS: u64 = 5;

/// Records a failed refresh unless disarmed. It also fires when the future
/// is dropped mid-request (the overall deadline), which an error path would
/// miss.
struct RefreshGuard<'a> {
    state: &'a mut TokenState,
    clock: &'a dyn Clock,
    done: bool,
}

impl Drop for RefreshGuard<'_> {
    fn drop(&mut self) {
        if !self.done {
            // From the moment of failure or cancellation, not from the start
            // of the refresh: a refresh that stalled to the deadline must
            // still hold off the next one for the full backoff (spec §5.7).
            self.state.retry_after = self.clock.now_secs() + REFRESH_BACKOFF_SECS;
        }
    }
}

/// The real FCM HTTP v1 sender.
pub struct FcmClient {
    http: HttpClient,
    account: ServiceAccount,
    /// e.g. `https://fcm.googleapis.com`
    endpoint: String,
    project_id: String,
    clock: Arc<dyn Clock>,
    token: tokio::sync::Mutex<TokenState>,
}

impl FcmClient {
    pub fn new(
        http: HttpClient,
        account: ServiceAccount,
        endpoint: String,
        project_id: String,
        clock: Arc<dyn Clock>,
    ) -> FcmClient {
        FcmClient { http, account, endpoint, project_id, clock, token: tokio::sync::Mutex::new(TokenState::default()) }
    }

    /// A cached token, refreshed under the lock (single flight) when it is
    /// within 60 s of expiry. After a failed or abandoned refresh, later
    /// calls fail at once for [`REFRESH_BACKOFF_SECS`].
    async fn access_token(&self) -> Result<String, SendError> {
        let mut g = self.token.lock().await;
        let now = self.clock.now_secs();
        if let Some(t) = g.token.as_ref()
            && t.expires_at > now + 60
        {
            return Ok(t.value.clone());
        }
        if now < g.retry_after {
            return Err(SendError::Unavailable);
        }
        let mut guard = RefreshGuard { state: &mut g, clock: &*self.clock, done: false };
        let jwt = self.account.assertion(now).map_err(|_| {
            tracing::error!(class = "oauth", "cannot sign the assertion");
            SendError::Unavailable
        })?;
        let form = format!(
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion={jwt}"
        );
        let res = self
            .http
            .post(&self.account.token_uri, "application/x-www-form-urlencoded", &[], form.into_bytes())
            .await
            .map_err(|e| {
                tracing::warn!(class = "oauth", error = %e, "token request failed");
                SendError::Unavailable
            })?;
        let parsed: Option<(String, u64)> = (res.status == 200)
            .then(|| serde_json::from_slice::<Value>(&res.body).ok())
            .flatten()
            .and_then(|v| Some((v["access_token"].as_str()?.to_string(), v["expires_in"].as_u64()?)));
        let Some((value, ttl)) = parsed else {
            tracing::warn!(class = "oauth", status = res.status, "token request refused");
            return Err(SendError::Unavailable);
        };
        guard.done = true;
        guard.state.token = Some(Token { value: value.clone(), expires_at: now.saturating_add(ttl) });
        Ok(value)
    }

    /// One send under one overall deadline: the token mutex wait, a token
    /// refresh and the push exchange together (spec §4.6).
    async fn send(&self, body: Value) -> Result<(), SendError> {
        match tokio::time::timeout(SEND_DEADLINE, self.send_inner(body)).await {
            Ok(r) => r,
            Err(_) => {
                tracing::warn!(class = "fcm", "send deadline passed");
                Err(SendError::Unavailable)
            }
        }
    }

    async fn send_inner(&self, body: Value) -> Result<(), SendError> {
        let token = self.access_token().await?;
        let url = format!("{}/v1/projects/{}/messages:send", self.endpoint, self.project_id);
        let auth = format!("Bearer {token}");
        let res = self
            .http
            .post(&url, "application/json", &[("authorization", &auth)], body.to_string().into_bytes())
            .await
            .map_err(|e| {
                tracing::warn!(class = "fcm", error = %e, "send failed");
                SendError::Unavailable
            })?;
        if (200..300).contains(&res.status) {
            return Ok(());
        }
        if res.status == 401 {
            self.token.lock().await.token = None;
        }
        let mapped = map_fcm_error(res.status, &res.body);
        tracing::warn!(class = "fcm", status = res.status, gone = (mapped == SendError::Gone), "push refused");
        Err(mapped)
    }
}

#[async_trait]
impl PushSender for FcmClient {
    fn supports(&self, platform: Platform) -> bool {
        platform == Platform::Android
    }
    async fn wake(&self, to: &Address) -> Result<(), SendError> {
        self.send(wake_body(to)).await
    }
    async fn challenge(&self, to: &Address, code: &str) -> Result<(), SendError> {
        self.send(challenge_body(to, code)).await
    }
}

/// Default deadline for one HTTP exchange with Google.
pub const SEND_TIMEOUT: Duration = Duration::from_secs(10);

/// Deadline for a whole send: waiting for the token lock, refreshing the
/// token and the push exchange, together.
pub const SEND_DEADLINE: Duration = Duration::from_secs(10);

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(kind: AddressKind) -> Address {
        Address { kind, value: "tok:abc".into() }
    }

    #[test]
    fn fcm_wake_body_is_golden() {
        let got = wake_body(&addr(AddressKind::FcmToken));
        let want: Value = serde_json::from_str(
            r#"{"message":{"token":"tok:abc","data":{"v":"1"},"androidV2":{
            "collapseKey":"acd-wake","ttl":"3600s","remoteNotification":{"notification":{
            "title":"Agent inbox","body":"An agent may need you","tag":"acd-wake",
            "channelId":"acd_wake","visibility":"PRIVATE"}}}}}"#,
        )
        .unwrap();
        assert_eq!(got, want);
        let fid = wake_body(&addr(AddressKind::FcmFid));
        assert_eq!(fid["message"]["fid"], "tok:abc");
        assert!(fid["message"].get("token").is_none());
    }

    #[test]
    fn fcm_challenge_body_is_golden() {
        let got = challenge_body(&addr(AddressKind::FcmToken), "CODE");
        let want: Value = serde_json::from_str(
            r#"{"message":{"token":"tok:abc","data":{"v":"1","t":"challenge","c":"CODE"},
            "androidV2":{"ttl":"60s","backgroundSync":{}}}}"#,
        )
        .unwrap();
        assert_eq!(got, want);
    }

    fn keys(v: &Value) -> Vec<&str> {
        let mut k: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        k.sort();
        k
    }

    #[test]
    fn fcm_bodies_have_exactly_the_listed_keys() {
        let w = wake_body(&addr(AddressKind::FcmToken));
        let m = &w["message"];
        assert_eq!(keys(m), ["androidV2", "data", "token"]);
        assert_eq!(keys(&m["data"]), ["v"]);
        assert_eq!(keys(&m["androidV2"]), ["collapseKey", "remoteNotification", "ttl"]);
        assert_eq!(
            keys(&m["androidV2"]["remoteNotification"]["notification"]),
            ["body", "channelId", "tag", "title", "visibility"]
        );
        let c = challenge_body(&addr(AddressKind::FcmToken), "C");
        assert_eq!(keys(&c["message"]["data"]), ["c", "t", "v"]);
        // The wake never carries a code or a handle.
        assert!(!w.to_string().contains("\"c\""));
    }

    #[test]
    fn only_unregistered_maps_to_gone() {
        let b = |s: &str| format!(r#"{{"error":{{"code":404,"status":"{s}"}}}}"#);
        assert_eq!(map_fcm_error(404, b("UNREGISTERED").as_bytes()), SendError::Gone);
        for s in ["INVALID_ARGUMENT", "NOT_FOUND", "UNAVAILABLE", "INTERNAL", "QUOTA_EXCEEDED", "UNAUTHENTICATED"] {
            assert_eq!(map_fcm_error(400, b(s).as_bytes()), SendError::Unavailable, "{s}");
        }
        let detail = br#"{"error":{"status":"NOT_FOUND","details":[{"errorCode":"UNREGISTERED"}]}}"#;
        assert_eq!(map_fcm_error(404, detail), SendError::Gone);
        assert_eq!(map_fcm_error(500, b"<html>"), SendError::Unavailable);
    }
}
