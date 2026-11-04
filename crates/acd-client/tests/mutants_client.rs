// Tests added from a cargo-mutants run (docs/research/2026-11-04-mutation-testing.md).
// Unix only: these drive the fake server.
#![cfg(owner_checks)]

mod common;
use acd_client::{conn::*, fake::*};
use std::time::Duration;

async fn wait_for_connections(f: &FakeServer, expected: usize) {
    // The timeout only matters on failure; success returns as soon as the connection arrives.
    tokio::time::timeout(Duration::from_secs(10), async {
        while f.connections() < expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {expected} fake connections"));
}

/// conn.rs:678 `delete !` and `wait_retry -> true`: a rejected pasted token is
/// not retried on its own; `retry_now` makes exactly one more attempt and the
/// machine stays alive for the next one.
#[tokio::test]
async fn a_rejected_token_waits_for_retry_now_and_then_tries_again() {
    let f = FakeServer::start(common::TOKEN, Identity::Paired).await;
    let h = spawn_machine(cfg_for(&f, "wrong-token", common::fast()));
    let mut rx = h.state();
    common::wait_for(&mut rx, "auth failed", |s| matches!(s.status, MachineStatus::AuthFailed { .. })).await;
    assert_eq!(f.connections(), 1, "no retry loop until retry_now");
    h.retry_now();
    wait_for_connections(&f, 2).await;
    common::wait_for(&mut rx, "auth failed again", |s| matches!(s.status, MachineStatus::AuthFailed { .. })).await;
    h.retry_now();
    wait_for_connections(&f, 3).await;
    assert!(f.hellos().iter().all(|hello| hello["token"] == "wrong-token"));
}
