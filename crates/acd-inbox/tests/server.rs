mod common;

use std::time::Duration;

use acd_client::{
    actions::ActionOwner,
    voice::{ActionEvent, ActionOutcome},
};
use acd_inbox::server::ServerOptions;
use common::*;
use serde_json::json;

#[tokio::test]
async fn a_view_gets_hello_settings_and_view_in_order() {
    let rig = rig(vec![pending_agent("a1", "r1")], fast()).await;
    let mut v = View::connect(&rig.socket).await;
    let (hello, settings, view) = v.greeting().await;
    assert_eq!(hello, json!({"type":"hello","protocol":1,"service":env!("CARGO_PKG_VERSION")}));
    assert_eq!(settings["machines"][0]["key"], "m1");
    assert_eq!(settings["machines"][0]["secret"], "ok");
    assert_eq!(settings["notifications"], true);
    assert_eq!(view["view"]["cards"][0]["key"], "m1/a1");
    assert_eq!(view["omitted"], json!({"done":0,"working":0}));
}

#[tokio::test]
async fn outcomes_reach_every_view_with_their_card() {
    let rig = rig(vec![pending_agent("a1", "r1")], held()).await;
    let mut a = View::connect(&rig.socket).await;
    let mut b = View::connect(&rig.socket).await;
    a.greeting().await;
    b.greeting().await;
    let id = rig.inbox.pick("m1/a1", "r1", "allow", 1).unwrap();
    assert!(rig.inbox.cancel(&id));
    for v in [&mut a, &mut b] {
        let o = v.recv_until(|m| m["type"] == "outcome").await;
        assert_eq!(o, json!({"type":"outcome","action":id,"card":"m1/a1","result":{"kind":"cancelled"}}));
    }
}

#[tokio::test]
async fn a_disconnect_cancels_that_views_countdowns_only() {
    let rig = rig(vec![pending_agent("a1", "r1"), pending_agent("a2", "r2")], held()).await;
    let mut a = View::connect(&rig.socket).await;
    a.greeting().await; // connection 1
    let mut b = View::connect(&rig.socket).await;
    b.greeting().await; // connection 2
    let mine = rig.inbox.pick_owned(ActionOwner(1), "m1/a1", "r1", "allow", 1).unwrap();
    let theirs = rig.inbox.pick_owned(ActionOwner(2), "m1/a2", "r2", "allow", 1).unwrap();
    drop(a);
    let o = b.recv_until(|m| m["type"] == "outcome").await;
    assert_eq!(o["action"], mine.as_str());
    assert_eq!(o["result"]["kind"], "cancelled");
    view_until(&rig.inbox, |v| v.actions.len() == 1 && v.actions[0].id == theirs).await;
    rig.inbox.expire_countdowns_for_test();
    let fired = b.recv_until(|m| m["type"] == "outcome" && m["action"] == theirs.as_str()).await;
    assert_eq!(fired["result"]["kind"], "fired");
    let frame = frame_of_type(&rig.f, "request.answer").await;
    assert_eq!(frame["request_id"], "r2");
    assert_eq!(answers(&rig.f), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_view_that_stops_reading_is_disconnected_and_its_countdowns_cancelled() {
    let mut big = pending_agent("a1", "r1");
    big["pending"]["raw_screen"] = "x".repeat(2 * 1024 * 1024).into();
    let rig = rig_with(RigOptions {
        agents: vec![big],
        timing: held(),
        server: ServerOptions { write_timeout: Duration::from_millis(300), ..Default::default() },
        ..Default::default()
    })
    .await;
    let mut watcher = View::connect(&rig.socket).await;
    watcher.greeting().await; // connection 1 reads everything
    let stalled = tokio::net::UnixStream::connect(&rig.socket).await.unwrap(); // connection 2 never reads
    until("the second connection", || rig.svc.connections() == 2).await;
    let id = rig.inbox.pick_owned(ActionOwner(2), "m1/a1", "r1", "allow", 1).unwrap();
    let o = watcher.recv_until(|m| m["type"] == "outcome" && m["action"] == id.as_str()).await;
    assert_eq!(o["result"]["kind"], "cancelled");
    assert_eq!(rig.svc.connections(), 1);
    drop(stalled);
}

#[tokio::test]
async fn an_oversized_line_closes_the_connection() {
    let rig = rig_with(RigOptions {
        server: ServerOptions { max_line: 16 * 1024, ..Default::default() },
        ..Default::default()
    })
    .await;
    let mut v = View::connect(&rig.socket).await;
    v.greeting().await;
    v.send_raw(&vec![b'a'; 20_000]).await;
    v.closed().await;
}

#[tokio::test]
async fn malformed_lines_and_bad_ids_close_the_connection() {
    let rig = rig(vec![pending_agent("a1", "r1")], fast()).await;
    for bad in [
        b"not json\n".to_vec(),
        b"[1,2]\n".to_vec(),
        b"{\"type\":\"escape\"}\n".to_vec(),
        format!("{{\"type\":\"escape\",\"id\":\"{}\"}}\n", "x".repeat(65)).into_bytes(),
        b"{\"type\":7,\"id\":\"a\"}\n".to_vec(),
    ] {
        let mut v = View::connect(&rig.socket).await;
        v.greeting().await;
        v.send_raw(&bad).await;
        v.closed().await;
    }
}

#[tokio::test]
async fn an_unknown_type_is_unsupported_and_a_repeated_id_is_refused() {
    let rig = rig(vec![pending_agent("a1", "r1")], fast()).await;
    let mut v = View::connect(&rig.socket).await;
    v.greeting().await;
    v.send(&json!({"type":"from_the_future","id":"x1"})).await;
    let r = v.recv_until(|m| m["id"] == "x1").await;
    assert_eq!(r, json!({"type":"error","id":"x1","code":"unsupported"}));
    v.send(&json!({"type":"from_the_future","id":"x1"})).await;
    let r = v.recv_until(|m| m["id"] == "x1").await;
    assert_eq!(r["code"], "duplicate_id");
}

#[tokio::test]
async fn a_view_that_cannot_fit_is_reported_and_recovers() {
    let agents: Vec<serde_json::Value> = (1..=5)
        .map(|i| {
            let mut a = pending_agent(&format!("a{i}"), &format!("r{i}"));
            a["title"] = "t".repeat(400).into();
            a
        })
        .collect();
    let rig = rig_with(RigOptions {
        agents,
        server: ServerOptions { max_line: 3000, ..Default::default() },
        ..Default::default()
    })
    .await;
    let mut v = View::connect(&rig.socket).await;
    assert_eq!(v.recv().await.unwrap()["type"], "hello");
    assert_eq!(v.recv().await.unwrap()["type"], "settings");
    assert_eq!(v.recv().await.unwrap(), json!({"type":"view_overflow","needs_you":5}));
    let c = rig.c.as_ref().unwrap();
    for (seq, id) in [(2, "a2"), (3, "a3"), (4, "a4"), (5, "a5")] {
        c.send(json!({"type":"agent.removed","epoch":"e1","seq":seq,"agent_id":id})).await;
    }
    // Under load an intermediate view (2 to 4 cards) can arrive first.
    let view = v.recv_until(|m| m["type"] == "view" && m["view"]["cards"].as_array().is_some_and(|c| c.len() == 1)).await;
    assert_eq!(view["view"]["cards"].as_array().unwrap().len(), 1, "the inbox shows again once it fits");
}

#[tokio::test]
async fn a_lagged_outcome_stream_tells_every_view() {
    let rig = rig(vec![pending_agent("a1", "r1")], fast()).await;
    let mut a = View::connect(&rig.socket).await;
    let mut b = View::connect(&rig.socket).await;
    a.greeting().await;
    b.greeting().await;
    let (tx, rx) = tokio::sync::broadcast::channel(1);
    for i in 0..3 {
        tx.send(ActionEvent { action_id: format!("x{i}"), card_key: "m1/a1".into(), outcome: ActionOutcome::Cancelled })
            .unwrap();
    }
    tokio::spawn(rig.svc.clone().route_outcomes(rx));
    for v in [&mut a, &mut b] {
        v.recv_until(|m| m["type"] == "outcomes_lost").await;
        let next = v.recv_until(|m| m["type"] == "outcome").await;
        assert_eq!(next["action"], "x2");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_connection_receives_outcomes_while_its_greeting_is_still_being_written() {
    let mut big = pending_agent("a1", "r1");
    big["pending"]["raw_screen"] = "x".repeat(2 * 1024 * 1024).into();
    let rig = rig_with(RigOptions { agents: vec![big], timing: held(), ..Default::default() }).await;
    // Nothing is read yet: the view line is stuck in the socket buffer.
    let mut v = View::connect(&rig.socket).await;
    until("the connection to be subscribed", || rig.svc.connections() == 1).await;
    let id = rig.inbox.pick("m1/a1", "r1", "allow", 1).unwrap();
    assert!(rig.inbox.cancel(&id));
    v.greeting().await;
    let o = v.recv_until(|m| m["type"] == "outcome").await;
    assert_eq!(o["action"], id.as_str());
}

#[tokio::test]
async fn one_view_line_serves_every_connection() {
    let rig = rig(vec![pending_agent("a1", "r1")], fast()).await;
    let mut a = View::connect(&rig.socket).await;
    let mut b = View::connect(&rig.socket).await;
    let (_, _, va) = a.greeting().await;
    let (_, _, vb) = b.greeting().await;
    assert_eq!(va, vb);
    rig.c.as_ref().unwrap().send(json!({"type":"agent.removed","epoch":"e1","seq":2,"agent_id":"a1"})).await;
    for v in [&mut a, &mut b] {
        let m = v.recv_until(|m| m["type"] == "view" && m["view"]["cards"].as_array().is_some_and(|c| c.is_empty())).await;
        assert_eq!(m["omitted"], json!({"done":0,"working":0}));
    }
}

#[tokio::test]
async fn aborting_the_server_closes_every_connection_and_cancels_its_countdowns() {
    let rig = rig(vec![pending_agent("a1", "r1")], held()).await;
    let mut v = View::connect(&rig.socket).await;
    v.greeting().await; // connection 1
    let mut rx = rig.inbox.action_outcomes();
    let id = rig.inbox.pick_owned(ActionOwner(1), "m1/a1", "r1", "allow", 1).unwrap();
    rig.server.abort();
    let event = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.expect("an outcome").unwrap();
    assert_eq!((event.action_id.as_str(), event.outcome), (id.as_str(), ActionOutcome::Cancelled));
    assert_eq!(rig.svc.connections(), 0);
    v.send_raw(b"{\"type\":\"panel\",\"id\":\"late\",\"open\":true}\n").await;
    v.closed().await;
    assert_eq!(answers(&rig.f), 0);
}
