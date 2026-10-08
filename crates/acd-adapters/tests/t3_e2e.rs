//! End-to-end acd/1 tests for the T3 adapter over `FakeT3`.

use std::{os::unix::fs::PermissionsExt, sync::Arc, time::Duration};

use acd_adapters::t3::{T3Adapter, T3Config, conn::ConnTiming, fake::*, reconciler::T3Timing};
use acd_core::{Auth, Core, ServerOptions, router, testing::test_machine};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct Rig {
    fake: FakeT3,
    url: String,
    _dir: tempfile::TempDir,
}

async fn start(fake: FakeT3) -> Rig {
    let dir = tempfile::Builder::new()
        .prefix("acd")
        .tempdir_in("/tmp")
        .unwrap();
    let token = dir.path().join("t3.token");
    std::fs::write(&token, FAKE_TOKEN).unwrap();
    std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
    let timing = T3Timing {
        conn: ConnTiming {
            connect: Duration::from_secs(2),
            ping_every: Duration::from_secs(5),
            pong_timeout: Duration::from_secs(5),
            write_timeout: Duration::from_secs(2),
        },
        backoff_start: Duration::from_millis(50),
        backoff_max: Duration::from_millis(200),
        tick: Duration::from_millis(50),
        release_after: Duration::from_millis(300),
        max_live: 32,
        learn_window: Duration::ZERO,
        relearn_after: Duration::from_millis(200),
        sync_timeout: Duration::from_millis(500),
        ..T3Timing::default()
    };
    let adapter = T3Adapter::new(T3Config {
        enabled: true,
        url: Some(fake.url()),
        token_path: token,
        timing,
        dispatch_budget: Duration::from_millis(500),
    });
    let core = Core::new(test_machine(), vec![adapter.clone()]);
    adapter.start(core.clone());
    let app = router(
        core,
        Arc::new(Auth::new("secret".into())),
        ServerOptions {
            hello_timeout: Duration::from_millis(300),
            ..Default::default()
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Rig {
        fake,
        url,
        _dir: dir,
    }
}

async fn connect(url: &str) -> Ws {
    connect_async(url).await.unwrap().0
}

async fn send(ws: &mut Ws, v: Value) {
    ws.send(Message::text(v.to_string())).await.unwrap();
}

async fn next(ws: &mut Ws) -> Value {
    loop {
        match tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .expect("timed out waiting for websocket frame")
        {
            Some(Ok(Message::Text(t))) => return serde_json::from_str(t.as_str()).unwrap(),
            Some(Ok(Message::Close(frame))) => {
                return json!({"closed": frame.map(|f| u16::from(f.code))});
            }
            Some(Ok(_)) => continue,
            Some(Err(_)) | None => return json!({"closed": null}),
        }
    }
}

async fn next_matching(ws: &mut Ws, d: Duration, mut pred: impl FnMut(&Value) -> bool) -> Value {
    let deadline = tokio::time::Instant::now() + d;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        let v = tokio::time::timeout(left.max(Duration::from_millis(1)), next(ws))
            .await
            .expect("timed out waiting for matching websocket frame");
        if pred(&v) {
            return v;
        }
    }
}

fn hello(token: &str, resume: Option<Value>, client_id: &str) -> Value {
    let mut v = json!({"type":"hello","protocol":"acd/1","token":token,"client":{"id":client_id,"name":"test","version":"0"}});
    if let Some(r) = resume {
        v["resume"] = r;
    }
    v
}

async fn authed(url: &str, client_id: &str) -> (Ws, Value, Value) {
    let mut ws = connect(url).await;
    send(&mut ws, hello("secret", None, client_id)).await;
    let welcome = next(&mut ws).await;
    let initial = next(&mut ws).await;
    (ws, welcome, initial)
}

async fn with_agent_count(url: &str, count: usize) -> (Ws, Value, Value) {
    for _ in 0..100 {
        let (ws, welcome, snapshot) = authed(url, "c1").await;
        if snapshot["agents"]
            .as_array()
            .is_some_and(|a| a.len() == count)
        {
            return (ws, welcome, snapshot);
        }
        drop(ws);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("no snapshot with {count} agents");
}

async fn result(ws: &mut Ws, id: &str) -> Value {
    next_matching(ws, Duration::from_secs(3), |v| {
        v["type"] == "result" && v["id"] == id
    })
    .await
}

#[tokio::test]
async fn t3_agent_appears_over_the_protocol() {
    let fake = FakeT3::start().await;
    fake.put_thread(shell_thread("claude-done", "completed", None));
    let mut codex = shell_thread("codex-run", "running", None);
    codex["providerInstanceId"] = json!("codexAgent");
    codex["modelSelection"]["instanceId"] = json!("codexAgent");
    fake.put_thread(codex);
    let r = start(fake).await;
    let (_, _, snap) = with_agent_count(&r.url, 2).await;
    let agents = snap["agents"].as_array().unwrap();
    let claude = agents.iter().find(|a| a["id"] == "t3:claude-done").unwrap();
    let codex = agents.iter().find(|a| a["id"] == "t3:codex-run").unwrap();
    assert_eq!(
        (
            claude["source"].as_str(),
            claude["kind"].as_str(),
            claude["state"].as_str(),
            claude["can_send"].as_bool()
        ),
        (Some("t3"), Some("claude"), Some("done"), Some(true))
    );
    assert_eq!(
        (
            codex["source"].as_str(),
            codex["kind"].as_str(),
            codex["state"].as_str(),
            codex["can_send"].as_bool()
        ),
        (Some("t3"), Some("codex"), Some("working"), Some(true))
    );
}

#[tokio::test]
async fn approval_and_question_answered_over_the_protocol() {
    let fake = FakeT3::start().await;
    fake.put_thread(shell_thread(
        "approve",
        "running",
        Some(("rq-approve", "command")),
    ));
    fake.set_projection(
        "approve",
        approval_projection("approve", "rq-approve", None),
    );
    fake.put_thread(shell_thread(
        "question",
        "running",
        Some(("rq-question", "user_input")),
    ));
    fake.set_projection("question", question_projection("question", "rq-question"));
    let r = start(fake).await;
    let (mut ws, _, snap) = with_agent_count(&r.url, 2).await;
    let agents = snap["agents"].as_array().unwrap();
    let approval = agents.iter().find(|a| a["id"] == "t3:approve").unwrap()["pending"].clone();
    let question = agents.iter().find(|a| a["id"] == "t3:question").unwrap()["pending"].clone();
    assert_eq!(
        (&approval["kind"], &approval["answer_path"]),
        (&json!("permission"), &json!("t3"))
    );
    send(
        &mut ws,
        json!({"type":"request.answer","id":"a1","request_id":approval["id"],"option_id":"accept"}),
    )
    .await;
    assert_eq!(result(&mut ws, "a1").await["outcome"], "delivered");
    let resolved = next_matching(&mut ws, Duration::from_secs(2), |v| {
        v["type"] == "request.resolved" && v["request_id"] == approval["id"]
    })
    .await;
    assert_eq!(
        (&resolved["by"], &resolved["option_id"]),
        (&json!("inbox"), &json!("accept"))
    );
    send(
        &mut ws,
        json!({"type":"request.answer","id":"q1","request_id":question["id"],"option_id":"o1"}),
    )
    .await;
    assert_eq!(result(&mut ws, "q1").await["outcome"], "delivered");
    assert_eq!(
        r.fake
            .dispatched()
            .iter()
            .find(|d| d["type"] == "runtime-request.respond" && d["requestId"] == "rq-question")
            .unwrap()["answers"]["Which color do you prefer?"],
        "red"
    );
}

#[tokio::test]
async fn answered_in_t3_resolves_the_card_within_2_s() {
    let fake = FakeT3::start().await;
    fake.put_thread(shell_thread(
        "approve",
        "running",
        Some(("rq-approve", "command")),
    ));
    fake.set_projection(
        "approve",
        approval_projection("approve", "rq-approve", None),
    );
    let r = start(fake).await;
    let (waiter, _, snap) = with_agent_count(&r.url, 1).await;
    let request_id = snap["agents"][0]["pending"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    drop(waiter);
    let mut clients = vec![authed(&r.url, "c2").await.0, authed(&r.url, "c3").await.0];
    r.fake.push_thread_event(
        "approve",
        "runtime-request.updated",
        json!({"id":"rq-approve","kind":"command","status":"resolved","decision":"accept"}),
    );
    r.fake.put_thread(shell_thread("approve", "running", None));
    for ws in &mut clients {
        let resolved = next_matching(ws, Duration::from_secs(2), |v| {
            v["type"] == "request.resolved" && v["request_id"] == request_id
        })
        .await;
        assert_eq!(resolved["by"], "terminal");
    }
    send(
        &mut clients[0],
        json!({"type":"request.answer","id":"late","request_id":request_id,"option_id":"accept"}),
    )
    .await;
    assert_eq!(
        result(&mut clients[0], "late").await["error"]["code"],
        "request_gone"
    );
    assert!(
        r.fake
            .dispatched()
            .iter()
            .all(|d| d["type"] != "runtime-request.respond")
    );
}

#[tokio::test]
async fn follow_up_retry_sends_once() {
    let fake = FakeT3::start().await;
    fake.put_thread(shell_thread("done", "completed", None));
    let r = start(fake).await;
    let (mut ws, welcome, snap) = with_agent_count(&r.url, 1).await;
    let agent_id = snap["agents"][0]["id"].clone();
    let resume = json!({"epoch":welcome["epoch"],"seq":snap["seq"]});
    let cmd = json!({"type":"agent.send","id":"f1","agent_id":agent_id,"text":"run tests"});
    send(&mut ws, cmd.clone()).await;
    drop(ws);
    let mut retry = connect(&r.url).await;
    send(&mut retry, hello("secret", Some(resume), "c1")).await;
    assert_eq!(next(&mut retry).await["type"], "welcome");
    send(&mut retry, cmd).await;
    assert_eq!(result(&mut retry, "f1").await["outcome"], "delivered");
    assert_eq!(
        r.fake
            .dispatched()
            .iter()
            .filter(|d| d["type"] == "message.dispatch")
            .count(),
        1
    );
}

#[tokio::test]
async fn follow_up_reply_lost_is_uncertain_and_not_resent() {
    let fake = FakeT3::start().await;
    fake.put_thread(shell_thread("done", "completed", None));
    let r = start(fake).await;
    let (mut ws, welcome, snap) = with_agent_count(&r.url, 1).await;
    let agent_id = snap["agents"][0]["id"].clone();
    let resume = json!({"epoch":welcome["epoch"],"seq":snap["seq"]});
    r.fake.hang_next_dispatch();
    let cmd = json!({"type":"agent.send","id":"f1","agent_id":agent_id,"text":"run tests"});
    send(&mut ws, cmd.clone()).await;
    assert_eq!(result(&mut ws, "f1").await["outcome"], "delivered");
    drop(ws);
    let mut retry = connect(&r.url).await;
    send(&mut retry, hello("secret", Some(resume), "c1")).await;
    next(&mut retry).await;
    send(&mut retry, cmd).await;
    assert_eq!(result(&mut retry, "f1").await["outcome"], "delivered");
    assert_eq!(
        r.fake
            .dispatched()
            .iter()
            .filter(|d| d["type"] == "message.dispatch")
            .count(),
        1
    );
}

#[tokio::test]
async fn in_flight_duplicate_waits_for_the_first() {
    let fake = FakeT3::start().await;
    fake.put_thread(shell_thread("done", "completed", None));
    let r = start(fake).await;
    let (_, _, snap) = with_agent_count(&r.url, 1).await;
    let cmd = json!({"type":"agent.send","id":"f1","agent_id":snap["agents"][0]["id"],"text":"run tests"});
    let gate = r.fake.gate_next_dispatch();
    let (mut a, _, _) = authed(&r.url, "same-client").await;
    let (mut b, _, _) = authed(&r.url, "same-client").await;
    send(&mut a, cmd.clone()).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    send(&mut b, cmd).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    gate.release();
    assert_eq!(result(&mut a, "f1").await["outcome"], "delivered");
    assert_eq!(result(&mut b, "f1").await["outcome"], "delivered");
    assert_eq!(
        r.fake
            .dispatched()
            .iter()
            .filter(|d| d["type"] == "message.dispatch")
            .count(),
        1
    );
}

#[tokio::test]
async fn reused_id_with_another_payload_is_duplicate_mismatch() {
    let fake = FakeT3::start().await;
    fake.put_thread(shell_thread("done", "completed", None));
    let r = start(fake).await;
    let (mut ws, _, snap) = with_agent_count(&r.url, 1).await;
    let agent_id = snap["agents"][0]["id"].clone();
    send(
        &mut ws,
        json!({"type":"agent.send","id":"f1","agent_id":agent_id,"text":"one"}),
    )
    .await;
    assert_eq!(result(&mut ws, "f1").await["outcome"], "delivered");
    send(
        &mut ws,
        json!({"type":"agent.send","id":"f1","agent_id":agent_id,"text":"two"}),
    )
    .await;
    assert_eq!(
        result(&mut ws, "f1").await["error"]["code"],
        "duplicate_mismatch"
    );
    let dispatches: Vec<_> = r
        .fake
        .dispatched()
        .into_iter()
        .filter(|d| d["type"] == "message.dispatch")
        .collect();
    assert_eq!(dispatches.len(), 1);
    assert_eq!(dispatches[0]["text"], "one");
}

#[tokio::test]
async fn answer_reply_lost_resolves_uncertain_over_the_protocol() {
    let fake = FakeT3::start().await;
    fake.put_thread(shell_thread(
        "approve",
        "running",
        Some(("rq-approve", "command")),
    ));
    fake.set_projection(
        "approve",
        approval_projection("approve", "rq-approve", None),
    );
    let r = start(fake).await;
    let (mut ws, _, snap) = with_agent_count(&r.url, 1).await;
    let request_id = snap["agents"][0]["pending"]["id"].clone();
    r.fake.hang_next_dispatch();
    send(
        &mut ws,
        json!({"type":"request.answer","id":"a1","request_id":request_id,"option_id":"accept"}),
    )
    .await;
    assert_eq!(result(&mut ws, "a1").await["outcome"], "delivered");
    let resolved = next_matching(&mut ws, Duration::from_secs(2), |v| {
        v["type"] == "request.resolved" && v["request_id"] == request_id
    })
    .await;
    assert_eq!(resolved["by"], "uncertain");
    assert!(resolved.get("option_id").is_none());
}
