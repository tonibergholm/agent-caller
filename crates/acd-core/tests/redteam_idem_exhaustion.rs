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

fn send(id: &str) -> ClientMessage {
    ClientMessage::AgentSend { id: id.into(), agent_id: "a2".into(), text: "hi".into() }
}

fn open_agent(core: &Core) {
    core.observe(Observation::AgentUpsert(agent_obs("a1")));
    core.observe(Observation::AgentUpsert(agent_obs("a2")));
}

fn open_request(core: &Core) -> String {
    open_agent(core);
    core.observe(Observation::RequestSeen(permission_obs("a1", "k1")));
    let (initial, _) = core.attach(None);
    let ServerMessage::Snapshot { agents, .. } = &initial[0] else { panic!("expected snapshot") };
    agents[0].pending.as_ref().unwrap().id.clone()
}

fn src(n: u8) -> Origin {
    Origin::net(std::net::IpAddr::from([198, 51, 100, n]))
}

/// Sends from `origin` under `clients` in turn until the table refuses one;
/// returns how many were accepted.
async fn flood(core: &Core, origin: Origin, clients: &[&str]) -> usize {
    for i in 0.. {
        let client = clients[i % clients.len()];
        let r = core.handle_from(client, origin, send(&format!("f-{i}"))).await;
        if !r.ok {
            assert_eq!(r.error_code(), Some(ErrorCode::BadRequest), "{r:?}");
            assert!(r.error.as_ref().unwrap().message.contains("too many recent commands"), "{r:?}");
            return i;
        }
        assert!(i <= IDEM_CAPACITY, "the flood was never refused");
    }
    unreachable!()
}

/// `agent.mark_seen` is safe to run twice, so it takes no table entry: any
/// number of them leaves the sender (and everyone else) able to send.
/// Fails if mark_seen is cached: the 4,097th is refused by the local quota.
#[tokio::test]
async fn mark_seen_does_not_consume_table_entries() {
    let fake = FakeAdapter::new();
    let core = Core::new(test_machine(), vec![fake.clone()]);
    open_agent(&core);
    for (n, origin) in [Origin::Local, src(1)].into_iter().enumerate() {
        for i in 0..IDEM_CAPACITY + 100 {
            let r = core.handle_from("c", origin, mark_seen(&format!("m-{i}"))).await;
            assert!(r.ok, "mark_seen #{i} from {origin:?}: {r:?}");
        }
        assert!(core.handle_from("c", origin, send(&format!("after-{n}"))).await.ok, "{origin:?} can still send");
    }
    assert_eq!(fake.sent().len(), 2);
}

/// One source cannot get more room by changing `client.id`: it is refused at
/// its own quota, and an unrelated client (other source, and the local socket)
/// still gets its first command through.
#[tokio::test]
async fn a_source_under_many_client_ids_cannot_lock_out_other_clients() {
    let core = Core::new(test_machine(), vec![FakeAdapter::new()]);
    open_agent(&core);
    let ids = ["x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7"];
    let n = flood(&core, src(1), &ids).await;
    assert_eq!(n, IDEM_CAPACITY / IDEM_SOURCE_SHARE_DIVISOR, "stopped at the source quota, not at the per-client one");
    // A ninth id from the same source is refused whatever its quota.
    assert_eq!(
        core.handle_from("x8", src(1), send("n")).await.error_code(),
        Some(ErrorCode::BadRequest)
    );
    assert!(core.handle_from("b", src(2), send("first")).await.ok, "an unrelated client's first command");
    assert!(core.handle_from("l", Origin::Local, send("first")).await.ok, "the local socket");
}

/// A delivered answer is never evicted before its TTL: a flood of successful
/// commands from the same source (under the same id, then others) is refused
/// at the quota instead, so the replay returns the original result without
/// delivering again and a changed payload is still a duplicate_mismatch.
#[tokio::test]
async fn a_delivered_answer_survives_a_flood_from_the_same_source() {
    let fake = FakeAdapter::new();
    let core = Core::new(test_machine(), vec![fake.clone()]);
    let rid = open_request(&core);
    let first = core.handle_from("victim", src(1), answer_to("ans", &rid, "allow")).await;
    assert_eq!(first, CommandResult::delivered("ans"));

    let under_same_id = flood(&core, src(1), &["victim"]).await;
    // The answer holds one entry of the source's quota.
    assert_eq!(under_same_id, IDEM_CAPACITY / IDEM_SOURCE_SHARE_DIVISOR - 1, "ran to the source quota, evicting nothing");
    let sent_before = fake.sent().len();
    let under_other_ids = flood(&core, src(1), &["y0", "y1", "y2"]).await;
    assert_eq!(under_other_ids, 0, "the source is already at its quota");

    assert_eq!(
        core.handle_from("victim", src(1), answer_to("ans", &rid, "allow")).await,
        CommandResult::delivered("ans"),
        "the replay returns the original result"
    );
    assert_eq!(
        core.handle_from("victim", src(1), answer_to("ans", &rid, "deny")).await.error_code(),
        Some(ErrorCode::DuplicateMismatch)
    );
    // The same id and payload as an early agent.send replays too: no second delivery.
    assert!(core.handle_from("victim", src(1), send("f-0")).await.ok);
    assert_eq!(fake.delivered().len(), 1);
    assert_eq!(fake.sent().len(), sent_before, "replays deliver nothing");
}

/// A source cannot use more than a handful of client ids.
#[tokio::test]
async fn one_source_is_limited_in_the_client_ids_it_can_use() {
    let core = Core::new(test_machine(), vec![FakeAdapter::new()]);
    open_agent(&core);
    for i in 0..IDEM_CLIENT_IDS_PER_SOURCE {
        assert!(core.handle_from(&format!("c{i}"), src(1), send("1")).await.ok);
    }
    let r = core.handle_from("one-too-many", src(1), send("1")).await;
    assert_eq!(r.error_code(), Some(ErrorCode::BadRequest));
    assert!(core.handle_from("c0", src(1), send("2")).await.ok, "an id it already holds still works");
    assert!(core.handle_from("one-too-many", src(2), send("1")).await.ok, "another source is unaffected");
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
