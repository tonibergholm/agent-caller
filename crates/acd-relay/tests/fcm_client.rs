mod common;

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use acd_relay::fcm::{Address, PushSender, SendError, ServiceAccount, challenge_body, wake_body};
use acd_relay::http::{HttpClient, HttpError};
use acd_relay::wire::AddressKind;
use acd_relay::ManualClock;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use common::*;
use ring::signature::{RSA_PKCS1_2048_8192_SHA256, RsaKeyPair, UnparsedPublicKey};

fn addr() -> Address {
    Address { kind: AddressKind::FcmToken, value: "tok:abc".into() }
}

#[tokio::test]
async fn fcm_client_gets_a_token_then_sends() {
    let fake = fake_fcm().await;
    let clock = Arc::new(ManualClock::new(T0));
    let c = fcm_client(&fake, clock.clone());
    c.wake(&addr()).await.unwrap();
    c.challenge(&addr(), "CODE").await.unwrap();
    assert_eq!(fake.st.token_requests.load(Ordering::SeqCst), 1);
    let sends = fake.st.sends.lock().unwrap();
    assert_eq!(sends[0].0, format!("Bearer {ACCESS_TOKEN}"));
    assert_eq!(sends[0].1, wake_body(&addr()));
    assert_eq!(sends[1].1, challenge_body(&addr(), "CODE"));

    // The assertion is a valid RS256 JWT with the right claims.
    let jwt = fake.st.assertions.lock().unwrap()[0].clone();
    let parts: Vec<&str> = jwt.split('.').collect();
    assert_eq!(parts.len(), 3);
    let claims: serde_json::Value = serde_json::from_slice(&B64.decode(parts[1]).unwrap()).unwrap();
    assert_eq!(claims["iss"], "relay@p-1.iam.example");
    assert_eq!(claims["scope"], "https://www.googleapis.com/auth/firebase.messaging");
    assert_eq!(claims["aud"], format!("http://{}/token", fake.addr));
    assert_eq!(claims["iat"], T0);
    assert_eq!(claims["exp"], T0 + 3600);
    let der: Vec<u8> = TEST_KEY_PEM
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .flat_map(|l| base64::engine::general_purpose::STANDARD.decode(l).unwrap())
        .collect();
    let pair = RsaKeyPair::from_pkcs8(&der).unwrap();
    UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, pair.public().as_ref())
        .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &B64.decode(parts[2]).unwrap())
        .expect("signature verifies");
}

#[tokio::test]
async fn fcm_client_caches_the_token() {
    let fake = fake_fcm().await;
    let clock = Arc::new(ManualClock::new(T0));
    let c = fcm_client(&fake, clock.clone());
    for _ in 0..3 {
        c.wake(&addr()).await.unwrap();
    }
    assert_eq!(fake.st.token_requests.load(Ordering::SeqCst), 1);
    clock.advance(3600 - 30); // inside the 60 s refresh margin
    c.wake(&addr()).await.unwrap();
    assert_eq!(fake.st.token_requests.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn fcm_client_maps_unregistered_to_gone() {
    let fake = fake_fcm().await;
    let c = fcm_client(&fake, Arc::new(ManualClock::new(T0)));
    *fake.st.send_reply.lock().unwrap() = (404, r#"{"error":{"status":"UNREGISTERED"}}"#.into());
    assert_eq!(c.wake(&addr()).await, Err(SendError::Gone));
    *fake.st.send_reply.lock().unwrap() = (400, r#"{"error":{"status":"INVALID_ARGUMENT"}}"#.into());
    assert_eq!(c.wake(&addr()).await, Err(SendError::Unavailable));
    // An unreachable endpoint is Unavailable, not Gone.
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead = l.local_addr().unwrap();
    drop(l); // nothing listens here any more
    let creds = serde_json::json!({"client_email":"a@b","private_key":TEST_KEY_PEM,
        "token_uri":format!("http://{dead}/token")}).to_string();
    let c2 = acd_relay::fcm::FcmClient::new(
        HttpClient::plain_for_tests(std::time::Duration::from_secs(2), 1024),
        ServiceAccount::from_json(creds.as_bytes()).unwrap(),
        format!("http://{dead}"),
        "p-1".into(),
        Arc::new(ManualClock::new(T0)),
    );
    assert_eq!(c2.wake(&addr()).await, Err(SendError::Unavailable));
}

#[tokio::test]
async fn fcm_client_errors_never_contain_credentials() {
    // Bad private keys: the error names the file problem, never the key text.
    let pem = TEST_KEY_PEM.replace("MII", "XXX");
    let bad = serde_json::json!({"client_email":"a@b","private_key":pem,"token_uri":"http://x/"}).to_string();
    let e = ServiceAccount::from_json(bad.as_bytes()).unwrap_err();
    assert!(!e.contains("XXX") && !e.contains("PRIVATE"), "{e}");
    let account = ServiceAccount::from_json(credentials_json(&fake_fcm().await).as_bytes()).unwrap();
    let dbg = format!("{account:?}");
    assert!(!dbg.contains("p-1.iam") && !dbg.contains("MII"), "{dbg}");
    // HTTP errors never echo the URL (which may carry a secret).
    let c = HttpClient::plain_for_tests(std::time::Duration::from_secs(2), 1024);
    let e = c.post("http://127.0.0.1:1/SECRET-PATH?k=SECRET", "text/plain", &[("authorization", "Bearer SECRET")], b"SECRET".to_vec()).await.unwrap_err();
    assert_eq!(e, HttpError::Connect);
    assert!(!e.to_string().contains("SECRET"));
    // A refused token request leaks nothing into the (opaque) error either.
    let fake = fake_fcm().await;
    *fake.st.token_reply.lock().unwrap() = (400, format!(r#"{{"error":"{ACCESS_TOKEN}"}}"#));
    let cl = fcm_client(&fake, Arc::new(ManualClock::new(T0)));
    let err = cl.wake(&addr()).await.unwrap_err();
    assert_eq!(err, SendError::Unavailable);
    assert!(!format!("{err:?}").contains("ya29"));
}

#[test]
fn fcm_credentials_file_must_be_private() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("cred.json");
    let body = serde_json::json!({"client_email":"a@b","private_key":TEST_KEY_PEM,"token_uri":"https://oauth2.example/token"}).to_string();
    std::fs::write(&p, &body).unwrap();
    for loose in [0o644, 0o640, 0o604, 0o666] {
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(loose)).unwrap();
        let e = ServiceAccount::from_file(&p).unwrap_err();
        assert!(!e.contains("PRIVATE") && !e.contains("MII"), "{e}");
    }
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(ServiceAccount::from_file(&p).is_ok());
    assert!(ServiceAccount::from_file(&dir.path().join("missing.json")).is_err());
}
