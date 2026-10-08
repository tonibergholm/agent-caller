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

async fn online(f: &FakeServer, h: &MachineHandle) -> FakeConn {
    let c = f.next_connection().await;
    c.send(snapshot("e1", 1, &[])).await;
    let mut rx = h.state();
    common::wait_for(&mut rx, "online", |s| s.status == MachineStatus::Online).await;
    c
}

fn answers(f: &FakeServer) -> Vec<serde_json::Value> {
    f.received()
        .into_iter()
        .filter(|v| v["type"] == "request.answer")
        .collect()
}

async fn until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(value) = f() {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
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

fn command_id(frame: &serde_json::Value) -> Option<&serde_json::Value> {
    matches!(
        frame["type"].as_str(),
        Some("request.answer" | "agent.send" | "agent.mark_seen")
    )
    .then(|| &frame["id"])
}

fn assert_no_command_after(f: &FakeServer, received_offset: usize, id: &serde_json::Value) {
    assert!(
        f.received()[received_offset..]
            .iter()
            .all(|v| command_id(v) != Some(id))
    );
}

async fn recording_server() -> (tempfile::TempDir, FakeServer, MachineHandle, FakeConn) {
    let dir = common::local_dir(common::TOKEN);
    let f = FakeServer::start(common::TOKEN, Identity::Paired).await;
    let mut cfg = cfg_for(&f, common::TOKEN, common::fast());
    cfg.credential = Credential::LocalFile(dir.path().to_owned());
    let h = spawn_machine(cfg);
    let c = f.next_connection().await;
    c.send(snapshot("e1", 1, &[])).await;
    let mut rx = h.state();
    common::wait_for(&mut rx, "online", |s| s.status == MachineStatus::Online).await;
    (dir, f, h, c)
}

async fn submit_answer(
    h: &MachineHandle,
) -> tokio::task::JoinHandle<acd_client::commands::Outcome> {
    let h = std::sync::Arc::new(h.clone());
    tokio::spawn(async move {
        h.submit(
            acd_client::commands::Command::Answer {
                request_id: "r1".into(),
                revision: 1,
                option_id: "allow".into(),
            },
            None,
        )
        .await
    })
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

#[tokio::test]
async fn not_ready_retry_reuses_the_id_then_succeeds() {
    let (f, h) = start().await;
    let c = online(&f, &h).await;
    let h = std::sync::Arc::new(h);
    let h2 = h.clone();
    let task = tokio::spawn(async move {
        h2.submit(
            acd_client::commands::Command::Answer {
                request_id: "r1".into(),
                revision: 1,
                option_id: "allow".into(),
            },
            None,
        )
        .await
    });
    for n in 1..=2 {
        let a = until("answer", || answers(&f).get(n - 1).cloned()).await;
        c.send(serde_json::json!({"type":"result","id":a["id"],"ok":false,"error":{"code":"request_not_ready","message":"m"}})).await;
    }
    let a = until("third answer", || answers(&f).get(2).cloned()).await;
    c.send(serde_json::json!({"type":"result","id":a["id"],"ok":true,"outcome":"delivered"}))
        .await;
    assert_eq!(
        task.await.unwrap(),
        acd_client::commands::Outcome::Delivered
    );
    let ids: Vec<_> = answers(&f).iter().map(|v| v["id"].clone()).collect();
    assert!(
        ids.windows(2).all(|w| w[0] == w[1]),
        "same command id on every retry: {ids:?}"
    );
}

#[tokio::test]
async fn lost_result_is_resent_identically() {
    let (f, h) = start().await;
    let c = online(&f, &h).await;
    let h = std::sync::Arc::new(h);
    let h2 = h.clone();
    let task = tokio::spawn(async move {
        h2.submit(
            acd_client::commands::Command::Send {
                agent_id: "a".into(),
                text: "hi".into(),
            },
            None,
        )
        .await
    });
    let first = until("send", || {
        f.received().into_iter().find(|v| v["type"] == "agent.send")
    })
    .await;
    c.close(1001).await;
    let c2 = f.next_connection().await;
    let again = until("resend", || {
        f.received()
            .into_iter()
            .filter(|v| v["type"] == "agent.send")
            .nth(1)
    })
    .await;
    assert_eq!(again, first);
    c2.send(serde_json::json!({"type":"result","id":first["id"],"ok":true,"outcome":"delivered"}))
        .await;
    assert_eq!(
        task.await.unwrap(),
        acd_client::commands::Outcome::Delivered
    );
}

#[tokio::test]
async fn new_epoch_without_rotation_marks_uncertain() {
    let (f, h) = start().await;
    let c = online(&f, &h).await;
    let h = std::sync::Arc::new(h);
    let h2 = h.clone();
    let task = tokio::spawn(async move {
        h2.submit(
            acd_client::commands::Command::Answer {
                request_id: "r1".into(),
                revision: 1,
                option_id: "allow".into(),
            },
            None,
        )
        .await
    });
    until("answer", || answers(&f).first().cloned()).await;
    f.set_epoch("e2");
    c.close(1001).await;
    let c2 = f.next_connection().await;
    c2.send(snapshot("e2", 1, &[])).await;
    assert_eq!(
        task.await.unwrap(),
        acd_client::commands::Outcome::Uncertain
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(answers(&f).len(), 1);
}

#[tokio::test]
async fn capability_removed_across_reconnect_is_not_resent() {
    let (f, h) = start().await;
    let c = online(&f, &h).await;
    let h = std::sync::Arc::new(h);
    let h2 = h.clone();
    let task = tokio::spawn(async move {
        h2.submit(
            acd_client::commands::Command::Answer {
                request_id: "r1".into(),
                revision: 1,
                option_id: "allow".into(),
            },
            None,
        )
        .await
    });
    until("answer", || answers(&f).first().cloned()).await;
    f.set_capabilities(&["send"]);
    c.close(1001).await;
    let _c2 = f.next_connection().await;
    assert_eq!(
        task.await.unwrap(),
        acd_client::commands::Outcome::Uncertain
    );
    assert_eq!(answers(&f).len(), 1);
}

#[tokio::test]
async fn retry_result_lost_is_resent_and_recovers() {
    let f = FakeServer::start(common::TOKEN, Identity::Paired).await;
    let mut timing = common::fast();
    timing.not_ready_window = Duration::from_millis(300);
    timing.not_ready_interval = Duration::from_millis(50);
    let h = spawn_machine(cfg_for(&f, common::TOKEN, timing));
    let c = online(&f, &h).await;
    let h = std::sync::Arc::new(h);
    let h2 = h.clone();
    let task = tokio::spawn(async move {
        h2.submit(
            acd_client::commands::Command::Answer {
                request_id: "r1".into(),
                revision: 1,
                option_id: "allow".into(),
            },
            None,
        )
        .await
    });
    let first = until("first answer", || answers(&f).first().cloned()).await;
    c.send(serde_json::json!({"type":"result","id":first["id"],"ok":false,"error":{"code":"request_not_ready","message":"m"}})).await;
    let retry = until("retry answer", || answers(&f).get(1).cloned()).await;
    f.set_accept_delay(Duration::from_millis(500));
    c.close(1001).await;
    let c2 = f.next_connection().await;
    let again = until("resend after window", || answers(&f).get(2).cloned()).await;
    assert_eq!(retry, again);
    c2.send(serde_json::json!({"type":"result","id":again["id"],"ok":true,"outcome":"delivered"}))
        .await;
    assert_eq!(
        task.await.unwrap(),
        acd_client::commands::Outcome::Delivered
    );
}

#[tokio::test]
async fn cancelling_during_welcome_handshake_does_not_resend_pending_commands() {
    let f = FakeServer::start(common::TOKEN, Identity::Paired).await;
    let h = spawn_machine(cfg_for(&f, common::TOKEN, common::fast()));
    let c = online(&f, &h).await;
    let h = std::sync::Arc::new(h);
    let submit = {
        let h = h.clone();
        tokio::spawn(async move {
            h.submit(
                acd_client::commands::Command::Answer {
                    request_id: "r1".into(),
                    revision: 1,
                    option_id: "allow".into(),
                },
                None,
            )
            .await
        })
    };
    let first = until("first answer", || answers(&f).first().cloned()).await;
    c.send(serde_json::json!({"type":"result","id":first["id"],"ok":false,"error":{"code":"request_not_ready","message":"m"}})).await;
    tokio::time::sleep(Duration::from_millis(10)).await;
    f.set_welcome_delay(Duration::from_millis(1500));
    c.close(1001).await;
    f.next_hello().await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    h.cancel();
    assert_eq!(
        tokio::time::timeout(Duration::from_millis(200), submit)
            .await
            .expect("cancellation interrupts a held handshake")
            .unwrap(),
        acd_client::commands::Outcome::Uncertain
    );
    f.next_welcome_release().await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        answers(&f).len(),
        1,
        "no command is resent after cancellation"
    );
}

#[tokio::test]
async fn cancellation_interrupts_backpressured_socket_writes() {
    let (f, h) = start().await;
    let _c = online(&f, &h).await;
    f.set_reading(false);
    let h = std::sync::Arc::new(h);
    let mut submits = Vec::new();
    for _ in 0..64 {
        let h = h.clone();
        submits.push(tokio::spawn(async move {
            h.submit(
                acd_client::commands::Command::Send {
                    agent_id: "a".into(),
                    text: "x".repeat(64 * 1024),
                },
                None,
            )
            .await
        }));
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    h.cancel();
    for submit in submits {
        let outcome = tokio::time::timeout(Duration::from_secs(1), submit)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            outcome,
            acd_client::commands::Outcome::Uncertain
                | acd_client::commands::Outcome::NotSent(acd_client::commands::NotSent::Offline)
        ));
    }
    let received_before_resume = f.received().len();
    assert!(
        received_before_resume < 64,
        "server stopped reading, so the connection must not finish every write"
    );
    f.set_reading(true);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        f.received().len(),
        received_before_resume,
        "cancelled writes must never complete later"
    );
}

#[tokio::test]
async fn rotation_while_disconnected_revokes_undelivered() {
    let (dir, f, h, c) = recording_server().await;
    let task = submit_answer(&h).await;
    let first = until("first answer", || answers(&f).first().cloned()).await;
    c.send(serde_json::json!({"type":"result","id":first["id"],"ok":false,"error":{"code":"request_not_ready","message":"m"}})).await;
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(answers(&f).len(), 1, "retry remains pending at 10 ms");
    let new = "QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVphYmNkZWY";
    common::write_token(&dir, new);
    f.set_token(new);
    let old_frame_count = f.received().len();
    c.close(1001).await;
    assert_eq!(
        task.await.unwrap(),
        acd_client::commands::Outcome::Uncertain
    );
    wait_for_connections(&f, 2).await;
    let _c2 = f.next_connection().await;
    until("new-token hello", || {
        (f.hellos().len() >= 2 && f.hellos()[1]["token"] == new).then_some(())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_no_command_after(&f, old_frame_count, &first["id"]);
}

#[tokio::test]
async fn rotation_while_disconnected_revokes_gated() {
    let (dir, f, h, c) = recording_server().await;
    let task = submit_answer(&h).await;
    let first = until("first answer", || answers(&f).first().cloned()).await;
    // The already observed frame is left unanswered, so delivery remains Unknown.
    assert_eq!(first["type"], "request.answer");
    let new = "QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVphYmNkZWY";
    common::write_token(&dir, new);
    f.set_token(new);
    let old_frame_count = f.received().len();
    c.close(1001).await;
    assert_eq!(
        task.await.unwrap(),
        acd_client::commands::Outcome::Uncertain
    );
    wait_for_connections(&f, 2).await;
    let _c2 = f.next_connection().await;
    until("new-token hello", || {
        (f.hellos().len() >= 2 && f.hellos()[1]["token"] == new).then_some(())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_no_command_after(&f, old_frame_count, &first["id"]);
}

#[tokio::test]
async fn revoked_command_id_never_reappears_after_4401() {
    let (dir, f, h, c) = recording_server().await;
    let task = submit_answer(&h).await;
    let first = until("first answer", || answers(&f).first().cloned()).await;
    let new = "QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVphYmNkZWY";
    common::write_token(&dir, new);
    f.set_token(new);
    let old_frame_count = f.received().len();
    c.close(4401).await;
    assert_eq!(
        task.await.unwrap(),
        acd_client::commands::Outcome::Uncertain
    );
    wait_for_connections(&f, 2).await;
    let _c2 = f.next_connection().await;
    until("new-token hello", || {
        (f.hellos().len() >= 2 && f.hellos()[1]["token"] == new).then_some(())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_no_command_after(&f, old_frame_count, &first["id"]);
}

#[tokio::test]
async fn stale_generation_is_refused() {
    let (f, h) = start().await;
    let c = online(&f, &h).await;
    let generation = h.state().borrow().conn_gen;
    c.close(1001).await;
    let mut rx = h.state();
    common::wait_for(&mut rx, "reconnected", |s| {
        s.status == MachineStatus::Online && s.conn_gen > generation
    })
    .await;
    let outcome = h
        .submit(
            acd_client::commands::Command::MarkSeen {
                agent_id: "a".into(),
            },
            Some(generation),
        )
        .await;
    assert_eq!(
        outcome,
        acd_client::commands::Outcome::NotSent(acd_client::commands::NotSent::Offline)
    );
    assert!(!f.received().iter().any(|v| v["type"] == "agent.mark_seen"));
}
