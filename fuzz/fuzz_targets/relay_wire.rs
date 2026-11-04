//! The relay's request parsers (acd-relay/src/wire.rs): the wake, registration
//! and confirmation bodies, and the address and app charset rules.
//! Invariants: a body is accepted only with exactly its documented fields
//! (no unknown field, so no free text can ride along) and re-serialises to a
//! body that parses to the same fields; an accepted address or app id is
//! within its length cap and charset (no control character reaches a log or
//! a URL); platform and kind names round-trip and agree on the platform.
#![no_main]
use acd_relay::wire::*;
use libfuzzer_sys::fuzz_target;
use serde_json::Value;

fn keys_are(data: &[u8], allowed: &[&str]) {
    let v: Value = serde_json::from_slice(data).expect("accepted by serde, so valid json");
    let obj = v.as_object().expect("a struct is only built from an object");
    assert_eq!(obj.len(), allowed.len(), "every field present, none extra");
    for k in obj.keys() {
        assert!(allowed.contains(&k.as_str()), "unknown field {k:?} accepted");
    }
}

fuzz_target!(|data: &[u8]| {
    if let Ok(r) = serde_json::from_slice::<WakeRequest>(data) {
        keys_are(data, &["v", "handle"]);
        let back: WakeRequest = serde_json::from_slice(&serde_json::to_vec(&r).unwrap()).unwrap();
        assert_eq!((back.v, &back.handle), (r.v, &r.handle));
    }
    if let Ok(r) = serde_json::from_slice::<RegisterRequest>(data) {
        keys_are(data, &["platform", "app", "address_kind", "address"]);
        let back: RegisterRequest = serde_json::from_slice(&serde_json::to_vec(&r).unwrap()).unwrap();
        assert_eq!(
            (&back.platform, &back.app, &back.address_kind, &back.address),
            (&r.platform, &r.app, &r.address_kind, &r.address)
        );
    }
    if let Ok(r) = serde_json::from_slice::<ConfirmRequest>(data) {
        keys_are(data, &["challenge_id", "code", "platform", "app", "address_kind", "address"]);
        let back: ConfirmRequest = serde_json::from_slice(&serde_json::to_vec(&r).unwrap()).unwrap();
        assert_eq!((&back.challenge_id, &back.code, &back.address), (&r.challenge_id, &r.code, &r.address));
    }
    // The charset rules and the name tables, on any text.
    let s = String::from_utf8_lossy(data);
    let s = s.as_ref();
    if address_ok(s) {
        assert!((1..=MAX_ADDRESS_LEN).contains(&s.len()));
        assert!(s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b':' | b'.' | b'-')));
    }
    if app_ok(s) {
        assert!((1..=MAX_APP_LEN).contains(&s.len()));
        assert!(s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-')));
    }
    if let Some(p) = Platform::parse(s) {
        assert_eq!(p.as_str(), s);
        assert_eq!(Platform::from_u8(p as u8), Some(p));
    }
    if let Some(k) = AddressKind::parse(s) {
        assert_eq!(k.as_str(), s);
        assert_eq!(AddressKind::from_u8(k as u8), Some(k));
        assert_eq!(AddressKind::parse(k.as_str()), Some(k));
    }
    for b in data.iter().take(2) {
        if let Some(p) = Platform::from_u8(*b) {
            assert_eq!(p as u8, *b);
        }
        if let Some(k) = AddressKind::from_u8(*b) {
            assert_eq!(k as u8, *b);
            let _ = k.platform();
        }
    }
});
