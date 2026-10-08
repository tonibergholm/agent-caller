//! Manual pre-merge race gate. Requires a throwaway T3 server and explicit environment.

use std::time::Duration;

use acd_adapters::t3::{
    conn,
    outcome::{self, Outcome},
    wire,
};
use serde_json::{Value, json};
use tokio::sync::Barrier;
use tokio::time::Instant;

struct Live {
    url: String,
    token: String,
    thread: String,
    request: String,
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
                .and_then(|v| v["snapshot"]["projection"].as_object())
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
    let _ = live();
    todo!("requires a fresh approval for each ordering and simultaneous run")
}
