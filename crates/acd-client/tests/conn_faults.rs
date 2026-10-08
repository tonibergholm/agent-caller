mod common;
use acd_client::{conn::*, fake::*};
use acd_protocol::*;
use std::time::Duration;

fn snapshot(epoch: &str, seq: u64, ids: &[&str]) -> serde_json::Value {
    serde_json::json!({"type":"snapshot","epoch":epoch,"seq":seq,"agents": ids.iter().map(|i| agent(i, "working")).collect::<Vec<_>>()})
}
fn agent(id: &str, state: &str) -> serde_json::Value {
    serde_json::json!({"id":id,"machine_id":"m","source":"herdr","kind":"claude","title":"t","cwd":null,"project":null,
        "state":state,"state_since":"2026-10-08T00:00:00.000Z","failed":false,"last_message":null,"pending":null,"can_send":true})
}
fn upd(epoch: &str, seq: u64, id: &str, state: &str) -> serde_json::Value {
    serde_json::json!({"type":"agent.updated","epoch":epoch,"seq":seq,"agent":agent(id, state)})
}
async fn start() -> (FakeServer, MachineHandle) {
    let f = FakeServer::start(common::TOKEN, Identity::Paired).await;
    let h = spawn_machine(cfg_for(&f, common::TOKEN, common::fast()));
    (f, h)
}

async fn wait_for_connections(f: &FakeServer, expected: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while f.connections() < expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {expected} fake connections"));
}

#[tokio::test]
async fn impostor_tls_server_receives_no_hello() {
    let f = FakeServer::start(common::TOKEN, Identity::Impostor).await;
    let h = spawn_machine(cfg_for(&f, common::TOKEN, common::fast()));
    let mut rx = h.state();
    common::wait_for(&mut rx, "identity_changed", |s| {
        matches!(s.status, MachineStatus::IdentityChanged { .. })
    })
    .await;
    assert_eq!(
        f.tls_accepts(),
        0,
        "the client aborts inside the TLS handshake"
    );
    assert!(
        f.hellos().is_empty(),
        "no acd/1 byte, so no token, reaches the impostor"
    );
}

#[tokio::test]
async fn new_epoch_stays_syncing_until_snapshot() {
    let (f, h) = start().await;
    let c = f.next_connection().await;
    c.send(snapshot("e1", 1, &["a"])).await;
    let mut rx = h.state();
    common::wait_for(&mut rx, "online", |s| s.status == MachineStatus::Online).await;
    f.set_epoch("e2");
    c.close(1001).await;
    let c2 = f.next_connection().await;
    let st = common::wait_for(&mut rx, "syncing", |s| s.status == MachineStatus::Syncing).await;
    assert!(
        st.session.agents.contains_key("a"),
        "stale agent still shown, read-only"
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        rx.borrow().status,
        MachineStatus::Syncing,
        "not ready without the snapshot"
    );
    c2.send(snapshot("e2", 1, &["b"])).await;
    let st = common::wait_for(&mut rx, "online", |s| s.status == MachineStatus::Online).await;
    assert!(!st.session.agents.contains_key("a"));
}

#[tokio::test]
async fn replay_is_ready_at_the_watermark_and_zero_event_resume_at_once() {
    let (f, h) = start().await;
    let c = f.next_connection().await;
    c.send(snapshot("e1", 5, &[])).await;
    let mut rx = h.state();
    common::wait_for(&mut rx, "online", |s| s.status == MachineStatus::Online).await;
    f.set_welcome_seq(7);
    c.close(1001).await;
    let c2 = f.next_connection().await;
    common::wait_for(&mut rx, "syncing", |s| s.status == MachineStatus::Syncing).await;
    c2.send(upd("e1", 6, "a", "working")).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        rx.borrow().status,
        MachineStatus::Syncing,
        "6 < watermark 7"
    );
    c2.send(upd("e1", 7, "a", "done")).await;
    common::wait_for(&mut rx, "online", |s| s.status == MachineStatus::Online).await;
    c2.close(1001).await;
    let _c3 = f.next_connection().await;
    common::wait_for(&mut rx, "online at once", |s| {
        s.status == MachineStatus::Online && s.conn_gen == 3
    })
    .await;
}

#[tokio::test]
async fn unknown_types_are_skipped_and_unknown_states_decode() {
    let (f, h) = start().await;
    let c = f.next_connection().await;
    c.send(snapshot("e1", 1, &[])).await;
    c.send(serde_json::json!({"type":"agent.launched","epoch":"e1","seq":2}))
        .await;
    c.send(upd("e1", 2, "a", "dreaming")).await;
    let mut rx = h.state();
    let s = common::wait_for(&mut rx, "agent a", |s| s.session.agents.contains_key("a")).await;
    assert_eq!(s.session.agents["a"].state, AgentState::Unknown);
    assert_eq!(f.connections(), 1, "no reconnect");
}

#[tokio::test]
async fn malformed_known_frame_forces_resync_without_resume() {
    let (f, _h) = start().await;
    let c = f.next_connection().await;
    c.send(snapshot("e1", 1, &[])).await;
    c.send(serde_json::json!({"type":"agent.updated","epoch":"e1","seq":2}))
        .await;
    let _c2 = f.next_connection().await;
    assert!(f.hellos()[1].get("resume").is_none());
}

#[tokio::test]
async fn seq_gap_forces_resync_without_resume() {
    let (f, _h) = start().await;
    let c = f.next_connection().await;
    c.send(snapshot("e1", 1, &[])).await;
    c.send(upd("e1", 3, "a", "working")).await;
    let _c2 = f.next_connection().await;
    assert!(f.hellos()[1].get("resume").is_none());
}

#[tokio::test]
async fn resume_carries_the_cursor_after_a_drop() {
    let (f, h) = start().await;
    let c = f.next_connection().await;
    c.send(snapshot("e1", 5, &[])).await;
    c.send(upd("e1", 6, "a", "working")).await;
    let mut rx = h.state();
    common::wait_for(&mut rx, "cursor at seq 6", |s| {
        s.session
            .cursor()
            .is_some_and(|cursor| cursor.epoch == "e1" && cursor.seq == 6)
    })
    .await;
    c.close(1001).await;
    let _c2 = f.next_connection().await;
    assert_eq!(
        f.hellos()[1]["resume"],
        serde_json::json!({"epoch":"e1","seq":6})
    );
    assert_eq!(
        f.hellos()[1]["token"],
        common::TOKEN,
        "token travels only inside the pinned channel"
    );
}

#[tokio::test]
async fn too_slow_reconnects_with_resume() {
    let (f, h) = start().await;
    let c = f.next_connection().await;
    c.send(snapshot("e1", 2, &[])).await;
    let mut rx = h.state();
    common::wait_for(&mut rx, "cursor at seq 2", |s| {
        s.session.cursor().is_some_and(|cursor| cursor.seq == 2)
    })
    .await;
    c.close(4408).await;
    let _c2 = f.next_connection().await;
    assert_eq!(f.hellos()[1]["resume"]["seq"], 2);
}

#[tokio::test]
async fn silent_server_triggers_ping_then_reconnect() {
    let f = FakeServer::start(common::TOKEN, Identity::Paired).await;
    f.set_answer_pings(false);
    let _h = spawn_machine(cfg_for(&f, common::TOKEN, common::fast()));
    let c = f.next_connection().await;
    c.send(snapshot("e1", 1, &[])).await;
    let _c2 = f.next_connection().await;
    assert!(f.received().iter().any(|v| v["type"] == "ping"));
}

#[tokio::test]
async fn incompatible_protocol_stops() {
    let f = FakeServer::start(common::TOKEN, Identity::Paired).await;
    f.set_handshake_error(Some(serde_json::json!({"type":"error","code":"unsupported_protocol","message":"m","supported":["acd/2"]})), Some(4400));
    let h = spawn_machine(cfg_for(&f, common::TOKEN, common::fast()));
    let mut rx = h.state();
    common::wait_for(&mut rx, "incompatible", |s| {
        matches!(s.status, MachineStatus::Incompatible { .. })
    })
    .await;
    assert_eq!(f.connections(), 1, "no retry loop until retry_now");
    h.retry_now();
    wait_for_connections(&f, 2).await;
    assert_eq!(f.connections(), 2);
}
