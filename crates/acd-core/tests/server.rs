use std::{sync::Arc, time::Duration};

use acd_core::testing::*;
use acd_core::*;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    connect_async, tungstenite::{client::IntoClientRequest, Message}, MaybeTlsStream, WebSocketStream,
};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct Server { core: Arc<Core>, auth: Arc<Auth>, url: String, fake: Arc<FakeAdapter> }

async fn start(fake: Arc<FakeAdapter>) -> Server {
    start_with(fake, ServerOptions { hello_timeout: Duration::from_millis(300), ..Default::default() }).await
}

async fn start_with(fake: Arc<FakeAdapter>, opts: ServerOptions) -> Server {
    let core = Core::new(test_machine(), vec![fake.clone()]);
    let auth = Arc::new(Auth::new("secret".into()));
    let app = router(core.clone(), auth.clone(), opts);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Server { core, auth, url, fake }
}

async fn connect(url: &str) -> Ws { connect_async(url).await.unwrap().0 }

async fn send(ws: &mut Ws, v: Value) { ws.send(Message::text(v.to_string())).await.unwrap(); }

/// The next JSON frame, or the close code as `{"closed": code}`.
async fn next(ws: &mut Ws) -> Value {
    loop {
        match tokio::time::timeout(Duration::from_secs(2), ws.next()).await.expect("timed out") {
            Some(Ok(Message::Text(t))) => return serde_json::from_str(t.as_str()).unwrap(),
            Some(Ok(Message::Close(frame))) => return json!({"closed": frame.map(|f| u16::from(f.code))}),
            Some(Ok(_)) => continue,
            Some(Err(_)) | None => return json!({"closed": null}),
        }
    }
}

fn hello(token: &str, resume: Option<Value>) -> Value {
    let mut v = json!({"type":"hello","protocol":"acd/1","token":token,"client":{"id":"c1","name":"test","version":"0"}});
    if let Some(r) = resume { v["resume"] = r; }
    v
}

async fn authed(url: &str) -> (Ws, Value, Value) {
    let mut ws = connect(url).await;
    send(&mut ws, hello("secret", None)).await;
    let welcome = next(&mut ws).await;
    let initial = next(&mut ws).await;
    (ws, welcome, initial)
}

#[tokio::test]
async fn bad_token_is_closed_with_4401() {
    let s = start(FakeAdapter::new()).await;
    let mut ws = connect(&s.url).await;
    send(&mut ws, hello("wrong", None)).await;
    assert_eq!(next(&mut ws).await["code"], "unauthorized");
    assert_eq!(next(&mut ws).await["closed"], 4401);
}

#[tokio::test]
async fn unknown_protocol_is_rejected_with_4400() {
    let s = start(FakeAdapter::new()).await;
    let mut ws = connect(&s.url).await;
    let mut h = hello("secret", None);
    h["protocol"] = json!("acd/9");
    send(&mut ws, h).await;
    let err = next(&mut ws).await;
    assert_eq!((err["code"].clone(), err["supported"].clone()), (json!("unsupported_protocol"), json!(["acd/1"])));
    assert_eq!(next(&mut ws).await["closed"], 4400);
}

#[tokio::test]
async fn command_before_hello_is_closed_with_4401() {
    let s = start(FakeAdapter::new()).await;
    let mut ws = connect(&s.url).await;
    send(&mut ws, json!({"type":"ping","id":"1"})).await;
    assert_eq!(next(&mut ws).await["closed"], 4401);
}

#[tokio::test]
async fn silent_client_is_closed_after_hello_timeout() {
    let s = start(FakeAdapter::new()).await;
    let mut ws = connect(&s.url).await;
    assert_eq!(next(&mut ws).await["closed"], 4401);
}

#[tokio::test]
async fn origin_header_is_forbidden() {
    let s = start(FakeAdapter::new()).await;
    let mut req = s.url.as_str().into_client_request().unwrap();
    req.headers_mut().insert("Origin", "https://evil.example".parse().unwrap());
    let err = connect_async(req).await.unwrap_err();
    assert!(err.to_string().contains("403"), "got {err}");
}

#[tokio::test]
async fn hello_returns_welcome_then_snapshot_then_live_events() {
    let s = start(FakeAdapter::new()).await;
    let (mut ws, welcome, initial) = authed(&s.url).await;
    assert_eq!(welcome["type"], "welcome");
    assert_eq!(welcome["capabilities"], json!(["answer","send","mark_seen"]));
    assert_eq!(initial["type"], "snapshot");
    s.core.observe(Observation::AgentUpsert(agent_obs("a1")));
    let ev = next(&mut ws).await;
    assert_eq!((ev["type"].clone(), ev["agent"]["id"].clone()), (json!("agent.updated"), json!("a1")));
    assert!(ev["seq"].as_u64().unwrap() > initial["seq"].as_u64().unwrap());
}

#[tokio::test]
async fn answer_round_trip_and_second_client_gets_busy() {
    let fake = FakeAdapter::gated();
    let s = start(fake.clone()).await;
    s.core.observe(Observation::AgentUpsert(agent_obs("a1")));
    s.core.observe(Observation::RequestSeen(permission_obs("a1", "k1")));
    let (mut a, _, snap) = authed(&s.url).await;
    let (mut b, _, _) = authed(&s.url).await;
    let rid = snap["agents"][0]["pending"]["id"].as_str().unwrap().to_string();
    send(&mut a, json!({"type":"request.answer","id":"x1","request_id":rid,"option_id":"allow"})).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    send(&mut b, json!({"type":"request.answer","id":"y1","request_id":rid,"option_id":"deny"})).await;
    let busy = loop { let m = next(&mut b).await; if m["type"] == "result" { break m; } };
    assert_eq!(busy["error"]["code"], "request_busy");
    fake.open_gate();
    let ok = loop { let m = next(&mut a).await; if m["type"] == "result" { break m; } };
    assert_eq!((ok["ok"].clone(), ok["outcome"].clone()), (json!(true), json!("delivered")));
    assert_eq!(s.fake.delivered().len(), 1);
}

#[tokio::test]
async fn resume_in_same_epoch_replays_and_other_epoch_snapshots() {
    let s = start(FakeAdapter::new()).await;
    let (_, welcome, snap) = authed(&s.url).await;
    let epoch = welcome["epoch"].as_str().unwrap().to_string();
    let seq = snap["seq"].as_u64().unwrap();
    s.core.observe(Observation::AgentUpsert(agent_obs("a1")));

    let mut ws = connect(&s.url).await;
    send(&mut ws, hello("secret", Some(json!({"epoch": epoch, "seq": seq})))).await;
    next(&mut ws).await; // welcome
    assert_eq!(next(&mut ws).await["type"], "agent.updated");

    let mut ws2 = connect(&s.url).await;
    send(&mut ws2, hello("secret", Some(json!({"epoch": "stale", "seq": seq})))).await;
    next(&mut ws2).await;
    assert_eq!(next(&mut ws2).await["type"], "snapshot");
}

#[tokio::test]
async fn malformed_frames_get_bad_request_and_connection_stays_open() {
    let s = start(FakeAdapter::new()).await;
    let (mut ws, _, _) = authed(&s.url).await;
    ws.send(Message::text("{not json")).await.unwrap();
    assert_eq!(next(&mut ws).await["code"], "bad_request");
    ws.send(Message::binary(vec![1u8, 2, 3])).await.unwrap();
    assert_eq!(next(&mut ws).await["code"], "bad_request");
    send(&mut ws, json!({"type":"agent.launch","id":"z9"})).await;
    let r = next(&mut ws).await;
    assert_eq!((r["type"].clone(), r["id"].clone(), r["error"]["code"].clone()), (json!("result"), json!("z9"), json!("bad_request")));
    send(&mut ws, json!({"type":"ping","id":"p1"})).await;
    assert_eq!(next(&mut ws).await, json!({"type":"pong","id":"p1"}));
}

#[tokio::test]
async fn second_hello_is_rejected_but_session_continues() {
    let s = start(FakeAdapter::new()).await;
    let (mut ws, _, _) = authed(&s.url).await;
    send(&mut ws, hello("secret", None)).await;
    assert_eq!(next(&mut ws).await["code"], "bad_request");
    send(&mut ws, json!({"type":"ping","id":"p2"})).await;
    assert_eq!(next(&mut ws).await["type"], "pong");
}

#[tokio::test]
async fn token_rotation_closes_existing_sessions() {
    let s = start(FakeAdapter::new()).await;
    let (mut ws, _, _) = authed(&s.url).await;
    s.auth.rotate("new-secret".into());
    assert_eq!(next(&mut ws).await["closed"], 4401);
    let mut fresh = connect(&s.url).await;
    send(&mut fresh, hello("new-secret", None)).await;
    assert_eq!(next(&mut fresh).await["type"], "welcome");
}

#[tokio::test]
async fn oversized_frame_closes_the_connection() {
    let s = start(FakeAdapter::new()).await;
    let (mut ws, _, _) = authed(&s.url).await;
    let big = "x".repeat(2 * 1024 * 1024);
    let _ = ws.send(Message::text(format!("{{\"type\":\"ping\",\"id\":\"{big}\"}}"))).await;
    assert!(next(&mut ws).await.get("closed").is_some());
}

#[tokio::test]
async fn stalled_client_is_closed() {
    let s = start(FakeAdapter::new()).await;
    let (mut ws, _, _) = authed(&s.url).await;
    for i in 0..3000 { // > RING_CAPACITY
        s.core.observe(Observation::AgentUpsert(agent_obs(&format!("a{i}"))));
        if i % 100 == 0 { tokio::task::yield_now().await; }
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(tokio::time::Instant::now() < deadline, "never closed");
        let m = next(&mut ws).await;
        if let Some(code) = m.get("closed") {
            assert!(code.is_null() || *code == 4408, "unexpected close code {code}");
            break;
        }
    }
}

#[tokio::test]
async fn ping_during_large_replay_gets_pong() {
    let s = start(FakeAdapter::new()).await;
    for i in 0..500 {
        s.core.observe(Observation::AgentUpsert(agent_obs(&format!("a{i}"))));
    }
    let mut ws = connect(&s.url).await;
    send(&mut ws, hello("secret", Some(json!({"epoch": s.core.epoch(), "seq": 0})))).await;
    send(&mut ws, json!({"type":"ping","id":"p1"})).await;
    loop {
        let m = next(&mut ws).await;
        assert!(m.get("closed").is_none(), "closed: {m}");
        if m["type"] == "pong" { break; }
    }
}

#[tokio::test]
async fn rotation_closes_idle_session_immediately() {
    let s = start(FakeAdapter::new()).await;
    let (mut ws, _, _) = authed(&s.url).await;
    s.auth.rotate("new".into());
    let m = tokio::time::timeout(Duration::from_millis(200), next(&mut ws)).await.expect("not closed within 200ms");
    assert_eq!(m["closed"], 4401);
}

#[tokio::test]
async fn too_many_in_flight_commands_get_bad_request() {
    let fake = FakeAdapter::gated();
    let s = start(fake.clone()).await;
    for i in 0..33 {
        s.core.observe(Observation::AgentUpsert(agent_obs(&format!("a{i}"))));
        s.core.observe(Observation::RequestSeen(permission_obs(&format!("a{i}"), "k")));
    }
    let (mut ws, _, snap) = authed(&s.url).await;
    let ids: Vec<String> = snap["agents"].as_array().unwrap().iter().map(|a| a["pending"]["id"].as_str().unwrap().to_string()).collect();
    assert_eq!(ids.len(), 33);
    for (i, rid) in ids.iter().take(32).enumerate() {
        send(&mut ws, json!({"type":"request.answer","id":format!("c{i}"),"request_id":rid,"option_id":"allow"})).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    send(&mut ws, json!({"type":"request.answer","id":"c32","request_id":ids[32],"option_id":"allow"})).await;
    let r = loop { let m = next(&mut ws).await; if m["type"] == "result" { break m; } };
    assert_eq!((r["id"].clone(), r["error"]["code"].clone()), (json!("c32"), json!("bad_request")));
    fake.open_gate();
}

#[tokio::test]
async fn preauth_cap_returns_503() {
    let s = start_with(FakeAdapter::new(), ServerOptions { max_preauth: 1, hello_timeout: Duration::from_secs(5), ..Default::default() }).await;
    let _first = connect(&s.url).await;
    let err = connect_async(s.url.as_str()).await.unwrap_err();
    assert!(err.to_string().contains("503"), "got {err}");
}

#[tokio::test]
async fn disconnect_mid_delivery_then_retry_gets_cached_result() {
    let fake = FakeAdapter::gated();
    let s = start(fake.clone()).await;
    s.core.observe(Observation::AgentUpsert(agent_obs("a1")));
    s.core.observe(Observation::RequestSeen(permission_obs("a1", "k1")));
    let (mut a, _, snap) = authed(&s.url).await;
    let rid = snap["agents"][0]["pending"]["id"].as_str().unwrap().to_string();
    let cmd = json!({"type":"request.answer","id":"x","request_id":rid,"option_id":"allow"});
    send(&mut a, cmd.clone()).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(a);
    tokio::time::sleep(Duration::from_millis(100)).await;
    fake.open_gate();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (mut b, _, _) = authed(&s.url).await;
    send(&mut b, cmd).await;
    let r = loop { let m = next(&mut b).await; if m["type"] == "result" { break m; } };
    assert_eq!((r["ok"].clone(), r["outcome"].clone()), (json!(true), json!("delivered")));
    assert_eq!(s.fake.delivered().len(), 1);
}
