//! Manual pre-merge race gate. Requires a throwaway T3 server and explicit environment.

use std::time::Duration;

use acd_adapters::t3::{
    conn,
    outcome::{self, Outcome},
    wire,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::sync::Barrier;
use tokio::time::Instant;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct Live {
    url: String,
    token: String,
    thread: String,
    request: String,
}

struct AcdLive {
    url: String,
    token: String,
}

fn acd_live() -> AcdLive {
    AcdLive {
        url: std::env::var("ACD_LIVE_URL").expect("ACD_LIVE_URL"),
        token: std::env::var("ACD_LIVE_TOKEN").expect("ACD_LIVE_TOKEN"),
    }
}

async fn acd_connect(url: &str) -> Ws {
    connect_async(url).await.expect("connect to acd").0
}

async fn acd_send(ws: &mut Ws, v: Value) {
    ws.send(Message::text(v.to_string()))
        .await
        .expect("send to acd");
}

async fn acd_next(ws: &mut Ws) -> Value {
    loop {
        match tokio::time::timeout(Duration::from_secs(20), ws.next())
            .await
            .expect("timed out waiting for acd frame")
        {
            Some(Ok(Message::Text(t))) => return serde_json::from_str(t.as_str()).unwrap(),
            Some(Ok(_)) => continue,
            Some(Err(e)) => panic!("acd websocket error: {e}"),
            None => panic!("acd websocket closed"),
        }
    }
}

async fn acd_matching(ws: &mut Ws, id: &str, kind: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let v = tokio::time::timeout_at(deadline, acd_next(ws))
            .await
            .expect("timed out waiting for acd event");
        if v["type"] == kind && v["request_id"] == id {
            return v;
        }
    }
}

async fn acd_auth(url: &str, token: &str, client_id: &str) -> (Ws, Value) {
    let mut ws = acd_connect(url).await;
    acd_send(
        &mut ws,
        json!({
            "type":"hello", "protocol":"acd/1", "token":token,
            "client":{"id":client_id,"name":"t3-live-race","version":"0"}
        }),
    )
    .await;
    let welcome = acd_next(&mut ws).await;
    assert_eq!(welcome["type"], "welcome", "acd hello failed: {welcome}");
    let snapshot = acd_next(&mut ws).await;
    assert_eq!(
        snapshot["type"], "snapshot",
        "expected acd snapshot: {snapshot}"
    );
    (ws, snapshot)
}

async fn acd_answer(
    ws: &mut Ws,
    request_id: &str,
    command_id: &str,
    start: Option<std::sync::Arc<Barrier>>,
) -> (Value, Value) {
    if let Some(start) = start {
        start.wait().await;
    }
    acd_send(
        ws,
        json!({
            "type":"request.answer", "id":command_id,
            "request_id":request_id, "option_id":"accept"
        }),
    )
    .await;
    let result = loop {
        let v = acd_next(ws).await;
        if v["type"] == "result" && v["id"] == command_id {
            break v;
        }
    };
    let resolved = acd_matching(ws, request_id, "request.resolved").await;
    (result, resolved)
}

fn live() -> Live {
    Live {
        url: std::env::var("T3_LIVE_URL").expect("T3_LIVE_URL"),
        token: std::env::var("T3_LIVE_TOKEN").expect("T3_LIVE_TOKEN"),
        thread: std::env::var("T3_LIVE_THREAD").expect("T3_LIVE_THREAD"),
        request: std::env::var("T3_LIVE_REQUEST").expect("T3_LIVE_REQUEST"),
    }
}

async fn connect(live: &Live) -> (conn::Conn, tokio::sync::mpsc::Receiver<conn::StreamMsg>) {
    conn::connect(&live.url, &live.token, &conn::ConnTiming::default())
        .await
        .expect("connect to throwaway T3")
}

async fn projection(live: &Live) -> Value {
    let (conn, mut events) = connect(live).await;
    let stream = conn
        .subscribe(wire::SUBSCRIBE_THREAD, json!({"threadId":live.thread}))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let msg = tokio::time::timeout_at(deadline, events.recv())
            .await
            .expect("thread projection timeout")
            .expect("T3 stream closed");
        if let conn::StreamMsg::Chunk { stream: id, values } = msg {
            if id != stream {
                continue;
            }
            if let Some(p) = values
                .iter()
                .find(|v| v["kind"] == "snapshot")
                .and_then(|v| v["projection"].as_object())
            {
                return Value::Object(p.clone());
            }
        }
    }
}

async fn respond(
    live: &Live,
    decision: &str,
    start: std::sync::Arc<Barrier>,
) -> Result<Value, conn::CallError> {
    let (conn, _events) = connect(live).await;
    let payload = json!({
        "type":"runtime-request.respond",
        "commandId":uuid::Uuid::new_v4().to_string(),
        "threadId":live.thread,
        "requestId":live.request,
        "decision":decision
    });
    start.wait().await;
    conn.call(
        wire::DISPATCH,
        &payload,
        Instant::now() + Duration::from_secs(10),
    )
    .await
}

#[tokio::test]
#[ignore = "live race gate; run only against a throwaway T3 server"]
async fn concurrent_raw_responds_have_one_winner() {
    let live = std::sync::Arc::new(live());
    let gate = std::sync::Arc::new(Barrier::new(3));
    let left_live = live.clone();
    let right_live = live.clone();
    let left_gate = gate.clone();
    let right_gate = gate.clone();
    let left = tokio::spawn(async move { respond(&left_live, "accept", left_gate).await });
    let right = tokio::spawn(async move { respond(&right_live, "decline", right_gate).await });
    gate.wait().await;
    let left = left.await.unwrap();
    let right = right.await.unwrap();
    let successes = usize::from(left.is_ok()) + usize::from(right.is_ok());
    assert_eq!(
        successes, 1,
        "exactly one raw respond must succeed: {left:?}, {right:?}"
    );
    let winner = if left.is_ok() { "accept" } else { "decline" };
    let loser = if left.is_err() { &left } else { &right };
    assert!(
        matches!(
            outcome::classify(loser, Some(&live.request)),
            Outcome::NotDelivered {
                code: acd_protocol::ErrorCode::RequestGone,
                end: Some(acd_protocol::ResolvedBy::Terminal),
                ..
            }
        ),
        "loser must be the terminal resolved failure: {loser:?}"
    );
    let projection = projection(&live).await;
    let request = projection["runtimeRequests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == live.request)
        .expect("runtime request in projection");
    assert_eq!(
        request["decision"], winner,
        "projection must record the winning decision"
    );
}

#[tokio::test]
#[ignore = "live race gate; run only against a throwaway T3 server"]
async fn acd_and_raw_client_race_in_both_orders() {
    let live = std::sync::Arc::new(live());
    let acd = acd_live();
    let order = std::env::var("T3_LIVE_ORDER").expect("T3_LIVE_ORDER");
    assert!(
        matches!(order.as_str(), "raw_first" | "acd_first" | "simultaneous"),
        "T3_LIVE_ORDER must be raw_first, acd_first, or simultaneous"
    );

    let (mut ws, snapshot) = acd_auth(&acd.url, &acd.token, "t3-live-race").await;
    let agent = snapshot["agents"]
        .as_array()
        .expect("agents in snapshot")
        .iter()
        .find(|a| a["id"] == format!("t3:{}", live.thread))
        .unwrap_or_else(|| panic!("T3 agent {} not in acd snapshot", live.thread));
    let request_id = agent["pending"]["id"]
        .as_str()
        .expect("T3 agent must have a pending acd request")
        .to_owned();
    // A fresh command id per run: acd caches results by (client, id) for 15 minutes.
    let answer_id = format!("t3-live-{}", uuid::Uuid::new_v4());
    let (resolved, recorded) = match order.as_str() {
        "raw_first" => {
            let raw = respond(&live, "decline", std::sync::Arc::new(Barrier::new(1))).await;
            assert!(raw.is_ok(), "raw decline should win: {raw:?}");
            let (result, resolved) = acd_answer(&mut ws, &request_id, &answer_id, None).await;
            assert_eq!(
                result["ok"], false,
                "acd answer should be rejected: {result}"
            );
            assert_eq!(result["error"]["code"], "request_gone");
            assert_eq!(resolved["by"], "terminal");
            assert!(resolved.get("option_id").is_none());
            (resolved, "decline".to_owned())
        }
        "acd_first" => {
            let (result, resolved) = acd_answer(&mut ws, &request_id, &answer_id, None).await;
            assert_eq!(result["ok"], true, "acd answer failed: {result}");
            let raw = respond(&live, "decline", std::sync::Arc::new(Barrier::new(1))).await;
            assert!(
                matches!(
                    outcome::classify(&raw, Some(&live.request)),
                    Outcome::NotDelivered { .. }
                ),
                "late raw respond must fail: {raw:?}"
            );
            // classify gives RequestGone only for T3's exact "… is resolved." / "is cancelled." /
            // "was not found." messages for this request id (the error's Display is a fixed string).
            assert!(
                matches!(
                    outcome::classify(&raw, Some(&live.request)),
                    Outcome::NotDelivered {
                        code: acd_protocol::ErrorCode::RequestGone,
                        ..
                    }
                ),
                "late raw respond must be classified request_gone: {raw:?}"
            );
            assert_eq!(resolved["by"], "inbox");
            assert_eq!(resolved["option_id"], "accept");
            (resolved, "accept".to_owned())
        }
        _ => {
            let gate = std::sync::Arc::new(Barrier::new(2));
            let raw_live = live.clone();
            let raw_gate = gate.clone();
            let raw = tokio::spawn(async move { respond(&raw_live, "decline", raw_gate).await });
            let acd_start = gate.clone();
            let acd_task = tokio::spawn(async move {
                let (result, resolved) =
                    acd_answer(&mut ws, &request_id, &answer_id, Some(acd_start)).await;
                (result, resolved)
            });
            let raw = raw.await.unwrap();
            let (result, resolved) = acd_task.await.unwrap();
            let projection = projection(&live).await;
            let decision = projection["runtimeRequests"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["id"] == live.request)
                .expect("request in projection")["decision"]
                .as_str()
                .expect("recorded decision")
                .to_owned();
            assert_eq!(
                decision == "accept",
                resolved["by"] == "inbox",
                "acd attribution must match T3 decision: decision={decision}, resolution={resolved}"
            );
            if decision == "accept" {
                assert_eq!(result["ok"], true, "acd answer failed: {result}");
                assert!(
                    raw.is_err(),
                    "raw decline must lose when accept is recorded: {raw:?}"
                );
                assert_eq!(resolved["option_id"], "accept");
            } else {
                assert_eq!(result["ok"], false, "acd answer should lose: {result}");
                assert_eq!(result["error"]["code"], "request_gone");
                assert!(raw.is_ok(), "raw decline should win when recorded: {raw:?}");
                assert_eq!(resolved["by"], "terminal");
                assert!(resolved.get("option_id").is_none());
            }
            (resolved, decision)
        }
    };

    let projection = projection(&live).await;
    let recorded = projection["runtimeRequests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == live.request)
        .expect("request in projection")["decision"]
        .as_str()
        .unwrap_or(&recorded)
        .to_owned();
    println!("RACE_RESULT decision={recorded} acd_resolution={resolved}");
}
