//! Regression for the red-team finding "idempotency table exhaustion"
//! (docs/research/2026-11-04-red-team-acd.md). One authenticated client used
//! to fill the daemon-wide idempotency table with `request_gone` results and
//! make every other client's next new command fail with "too many recent
//! commands". Now refusals release their reservation, and one client is held
//! to a fixed share of the table whatever it sends. The live reproducer is
//! `tests/redteam/idem_table_flood.py`.
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
async fn a_flood_of_request_gone_does_not_lock_out_other_clients() {
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
        Some(ErrorCode::RequestGone),
        "a refusal must not be cached, so the flood never hits the table cap"
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
        "a brand-new client's first command was refused with {:?} after another client's flood",
        victim_after.error_code()
    );
}

fn mark_seen(id: &str) -> ClientMessage {
    ClientMessage::AgentMarkSeen { id: id.into(), agent_id: "a1".into() }
}

fn open_request(core: &Core) -> String {
    core.observe(Observation::AgentUpsert(agent_obs("a1")));
    core.observe(Observation::RequestSeen(permission_obs("a1", "k1")));
    let (initial, _) = core.attach(None);
    let ServerMessage::Snapshot { agents, .. } = &initial[0] else { panic!("expected snapshot") };
    agents[0].pending.as_ref().unwrap().id.clone()
}

/// Commands that DO get cached (successful ones) fill only the sender's own
/// share; another client still gets in and a delivered answer still replays.
#[tokio::test]
async fn a_flood_of_cached_commands_cannot_starve_other_clients_or_break_replay() {
    let fake = FakeAdapter::new();
    let core = Core::new(test_machine(), vec![fake.clone()]);
    let rid = open_request(&core);
    let first = core.handle("victim", answer_to("ans", &rid, "allow")).await;
    assert_eq!(first, CommandResult::delivered("ans"));

    let share = IDEM_CAPACITY / IDEM_CLIENT_SHARE_DIVISOR;
    for i in 0..share * 2 + 50 {
        let r = core.handle("attacker", mark_seen(&format!("f-{i}"))).await;
        assert!(r.ok, "the flooder's own commands keep working by evicting its oldest: {r:?}");
    }
    // Another client's first command works.
    assert!(core.handle("other", mark_seen("o1")).await.ok);
    // The victim's delivered answer replays with the original result, and is not re-sent.
    let replay = core.handle("victim", answer_to("ans", &rid, "allow")).await;
    assert_eq!(replay, CommandResult::delivered("ans"));
    assert_eq!(fake.delivered().len(), 1);
    // A different payload under the same key is still refused.
    let other = core.handle("victim", answer_to("ans", &rid, "deny")).await;
    assert_eq!(other.error_code(), Some(ErrorCode::DuplicateMismatch));
}

fn answer_to(id: &str, request_id: &str, option: &str) -> ClientMessage {
    ClientMessage::RequestAnswer {
        id: id.into(),
        request_id: request_id.into(),
        revision: None,
        option_id: Some(option.into()),
        text: None,
    }
}

/// A refusal changed no state, so it releases its id: the same id may be
/// reused with a different payload without a duplicate_mismatch.
#[tokio::test]
async fn a_request_gone_refusal_does_not_hold_its_command_id() {
    let core = Core::new(test_machine(), vec![FakeAdapter::new()]);
    let a = core.handle("cl", answer_nonexistent("same", "nope-1")).await;
    assert_eq!(a.error_code(), Some(ErrorCode::RequestGone));
    let b = core.handle("cl", answer_nonexistent("same", "nope-2")).await;
    assert_eq!(b.error_code(), Some(ErrorCode::RequestGone), "not duplicate_mismatch");
}
