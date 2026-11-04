//! Red-team finding (docs/research/2026-11-04-red-team-acd.md,
//! "idempotency table exhaustion"): one authenticated-but-hostile client
//! can fill the daemon-wide idempotency table
//! (`crates/acd-core/src/idem.rs`, `IDEM_CAPACITY` = 16,384 entries,
//! shared by every connected client) in well under a second, by sending
//! `request.answer` for nonexistent `request_id`s. The resulting
//! `RequestGone` result is treated as `cacheable` by
//! `Core::handle`'s delivery/`cacheable` computation
//! (`crates/acd-core/src/engine.rs`, the `cacheable = delivery !=
//! Delivery::NotDelivered && !matches!(result.error_code(), Some(...))`
//! line), unlike `RequestNotReady`/`RequestBusy`/`SourceUnavailable`/
//! `RequestChanged`, which release their reservation. Once the table is
//! full, *every* client's *next new* command id gets `bad_request: "too
//! many recent commands; retry later"` -- not just the flooder's -- for
//! up to the 15-minute TTL (`IDEM_TTL`).
//!
//! This is `#[ignore]`d because it is slow (>16,384 real `Core::handle`
//! calls) and is a demonstration of accepted-but-underweighted behavior,
//! not a regression to gate CI on; the dynamic reproducer against a real
//! running daemon is `tests/redteam/idem_table_flood.py`. No production
//! code is changed. Run with:
//!
//!     cargo test -p acd-core --test redteam_idem_exhaustion -- --ignored --nocapture
use acd_core::testing::*;
use acd_core::*;
use acd_protocol::*;

fn answer_nonexistent(id: &str, request_id: &str) -> ClientMessage {
    ClientMessage::RequestAnswer {
        id: id.into(),
        request_id: request_id.into(),
        revision: None,
        option_id: Some("allow".into()),
        text: None,
    }
}

#[tokio::test]
#[ignore = "slow (fills the real 16,384-entry idempotency table); see docs/research/2026-11-04-red-team-acd.md"]
async fn one_hostile_client_locks_out_every_other_client_s_next_command() {
    let fake = FakeAdapter::new();
    let core = Core::new(test_machine(), vec![fake]);

    // The victim: an unrelated client, answering a request that does not
    // exist (as any client would after a stale card, a race, or simply a
    // typo) -- this must normally be `request_gone`, cheaply, forever.
    let victim_before = core
        .handle("victim-client", answer_nonexistent("v0", "victim-req-0"))
        .await;
    assert_eq!(
        victim_before.error_code(),
        Some(ErrorCode::RequestGone),
        "sanity: a nonexistent request_id must read as request_gone before the flood"
    );

    // The attacker: one connection (one client.id), IDEM_CAPACITY + a
    // margin of distinct command ids, each answering a distinct
    // nonexistent request_id. Every one of these gets cached as `Done`
    // (see the module doc above), consuming the shared table.
    let fill = IDEM_CAPACITY + 200;
    let mut last_code = None;
    for i in 0..fill {
        let id = format!("flood-{i}");
        let request_id = format!("nonexistent-{i}");
        let result = core
            .handle("attacker-client", answer_nonexistent(&id, &request_id))
            .await;
        last_code = result.error_code();
    }
    assert_eq!(
        last_code,
        Some(ErrorCode::BadRequest),
        "the flood should have filled the table well before {fill} calls \
         (IDEM_CAPACITY = {IDEM_CAPACITY}); if this fails, the table did \
         not fill -- re-check cacheable's exclusion list in engine.rs"
    );

    // The actual finding: a brand-new client.id's very first command --
    // nothing to do with the flooder, not a retry, not a reused id --
    // must still succeed (fast, cheap request_gone). It does not: the
    // shared table has no room left to even *reserve* its id.
    let victim_after = core
        .handle("a-completely-different-victim", answer_nonexistent("v1", "victim-req-1"))
        .await;
    assert_eq!(
        victim_after.error_code(),
        Some(ErrorCode::RequestGone),
        "FINDING: a brand-new, unrelated client's first-ever command was \
         refused with {:?} because one hostile peer filled the shared \
         idempotency table. Expected request_gone (as before the flood), \
         got the daemon-wide 'too many recent commands' refusal instead.",
        victim_after.error_code()
    );
}
