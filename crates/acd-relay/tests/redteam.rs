//! Dynamic red-team findings against a running `acd-relay`.
//!
//! Method and full attack log: `docs/research/2026-11-04-red-team-relay.md`.
//! These tests reproduce findings from that session as deterministic,
//! in-process repros (no real sockets or wall clock), matching the style of
//! `tests/api.rs`. Each `#[ignore]` test documents a finding: it fails on
//! the tree as it stands and is expected to pass once the named fix lands.
//! No production code is changed by this file.

mod common;

use std::sync::Arc;

use acd_relay::limits::Limit;
use acd_relay::server::Rates;
use common::*;
use serde_json::json;

async fn plain_with(rates: Rates) -> (Relay, Arc<FakeSender>) {
    let s = Arc::new(FakeSender::default());
    (start(Setup { rates, ..Setup::default() }, s.clone()).await, s)
}

/// Finding 1 (`wake`): the global budget is charged before the per-IP
/// budget (spec §5.3.2's own stated order: "global limit; per-IP limit").
/// `Counter::hit` only refunds nothing on a later rejection, so a source
/// that is already over its OWN per-IP limit keeps spending the SHARED
/// global budget with every further request, because the global check ran
/// (and counted the hit) before the per-IP check that would have rejected
/// it for free. A source with no valid handle at all (never reaches the
/// per-handle/per-address checks) can therefore burn through the entire
/// global wake budget alone, past its own per-IP cap, and deny wakes to
/// every other source and handle on the relay.
///
/// Minimal fix: check the per-IP limit before the global limit (swap the
/// two lines in `wake_inner`, `crates/acd-relay/src/server.rs`), so a
/// request that would be rejected per-IP never also consumes the shared
/// budget. This test fails today and is expected to pass with that swap;
/// reverting the swap reproduces the failure (see the research doc for the
/// before/after run).
#[tokio::test]
#[ignore = "finding: global wake budget is drained by one source past its own per-IP limit (docs/research/2026-11-04-red-team-relay.md)"]
async fn one_source_cannot_drain_the_global_wake_budget_past_its_own_ip_limit() {
    // An attacker's own per-IP cap (3) is far smaller than the global
    // budget (10) shared with everyone else, and the attacker sends more
    // requests than the global budget holds.
    let (r, s) = plain_with(Rates { wake_ip: Limit::hourly(3), wake_global: Limit::hourly(10), ..Rates::default() }).await;
    let victim = r.make_handle(&s, "tok:victim").await;
    let attacker_ip = "198.51.100.9";

    // The attacker has no valid handle; every one of its requests is
    // rejected (400 invalid_handle) *after* the rate checks. Send far more
    // than its own per-IP cap and more than the shared global budget.
    let mut attacker_codes = vec![];
    for _ in 0..15 {
        let (st, _) = r.post("/v1/wake", &json!({"v":1,"handle":"garbage"}), attacker_ip).await;
        attacker_codes.push(st);
    }
    // The attacker's own per-IP limit (3) must cap it at 3 "past the rate
    // limiter" attempts (400s); everything after must be 429 from its own
    // bucket, not silently accepted-and-wasted against the global one.
    let got_past_ip_limit = attacker_codes.iter().filter(|&&c| c == 400).count();
    assert_eq!(got_past_ip_limit, 3, "attacker's own per-IP limit should cap it at 3, got {attacker_codes:?}");

    // A victim on a DIFFERENT IP with a VALID handle must still be able to
    // wake: the attacker's 15 requests must not have spent more than 3
    // units of the 10-request global budget on this attacker's behalf.
    let victim_ip = "203.0.113.50";
    let (st, body) = r.post("/v1/wake", &json!({"v":1,"handle":victim}), victim_ip).await;
    assert_eq!(st, 204, "victim wake must still succeed, got {st} {body}");
}

/// Finding 1 (`register`): the same pattern on `POST /v1/handles`
/// (`issue_global` checked before `issue_ip`/`issue_address`).
#[tokio::test]
#[ignore = "finding: global registration budget is drained by one source past its own per-IP limit (docs/research/2026-11-04-red-team-relay.md)"]
async fn one_source_cannot_drain_the_global_registration_budget_past_its_own_ip_limit() {
    let (r, s) = plain_with(Rates { issue_ip: Limit::hourly(2), issue_global: Limit::hourly(10), ..Rates::default() }).await;
    let _ = &s; // unused sender; kept for symmetry with other tests
    let attacker_ip = "198.51.100.10";
    let mut codes = vec![];
    for i in 0..15 {
        let (st, _) = r.post("/v1/handles", &Relay::reg_body(&format!("attacker{i}:addr")), attacker_ip).await;
        codes.push(st);
    }
    let got_past_ip_limit = codes.iter().filter(|&&c| c == 202).count();
    assert_eq!(got_past_ip_limit, 2, "attacker's own per-IP limit should cap it at 2, got {codes:?}");

    let (st, body) = r.post("/v1/handles", &Relay::reg_body("victim:addr"), "203.0.113.51").await;
    assert_eq!(st, 202, "a victim on another IP must still be able to register, got {st} {body}");
}
