//! Regression: found by reading the token code for the `relay_fcm` fuzz
//! target (docs/research/2026-11-04-fuzz-campaign.md). A token response with
//! `expires_in` near `u64::MAX` made `now + ttl` overflow: a panic in a debug
//! build, a wrapped (already expired) expiry in a release build, which sent
//! every push through a new token request.
mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering;

use acd_relay::ManualClock;
use acd_relay::fcm::{Address, PushSender};
use acd_relay::wire::AddressKind;
use common::*;

const HOSTILE: &str = include_str!("../../../tests/fixtures/fuzz/relay_fcm-token-expires-in-max.json");

#[tokio::test]
async fn a_huge_expires_in_neither_panics_nor_defeats_the_token_cache() {
    let fake = fake_fcm().await;
    *fake.st.token_reply.lock().unwrap() = (200, HOSTILE.to_string());
    let c = fcm_client(&fake, Arc::new(ManualClock::new(T0)));
    let to = Address { kind: AddressKind::FcmToken, value: "tok:abc".into() };
    c.wake(&to).await.unwrap();
    c.wake(&to).await.unwrap();
    assert_eq!(fake.st.token_requests.load(Ordering::SeqCst), 1, "the token was not cached");
}
