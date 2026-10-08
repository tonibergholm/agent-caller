use acd_adapters::t3::{conn::*, fake::*, wire::*};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::time::Instant;

fn fast() -> ConnTiming {
    ConnTiming {
        connect: Duration::from_secs(2),
        ping_every: Duration::from_millis(100),
        pong_timeout: Duration::from_millis(100),
        write_timeout: Duration::from_millis(300),
    }
}
async fn open(fake: &FakeT3) -> (Conn, tokio::sync::mpsc::Receiver<StreamMsg>) {
    connect(&ws_url(&fake.url()).unwrap(), FAKE_TOKEN, &fast())
        .await
        .unwrap()
}
async fn next_chunk(rx: &mut tokio::sync::mpsc::Receiver<StreamMsg>) -> Vec<Value> {
    match tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("chunk")
        .expect("open")
    {
        StreamMsg::Chunk { values, .. } => values,
        StreamMsg::End { .. } => panic!("stream ended"),
    }
}
fn dispatch(cmd: &str) -> Value {
    json!({"type":"message.dispatch","createdBy":"user","creationSource":"web","commandId":cmd,"threadId":"t1",
        "messageId":format!("m-{cmd}"),"text":"hi","attachments":[],"deliveryIntent":"auto","dispatchMode":{"type":"start_immediately"}})
}

#[test]
fn ws_url_accepts_only_local_and_tailscale_hosts() {
    assert_eq!(
        ws_url("http://127.0.0.1:3773").unwrap(),
        "ws://127.0.0.1:3773/ws?orchestrationProtocol=2"
    );
    assert_eq!(
        ws_url("ws://localhost:3773/").unwrap(),
        "ws://localhost:3773/ws?orchestrationProtocol=2"
    );
    assert!(ws_url("ws://100.101.102.103:3773").is_ok());
    assert!(ws_url("ws://[fd7a:115c:a1e0::1]:3773").is_ok());
    for bad in [
        "https://127.0.0.1:3773",
        "ws://example.com:3773",
        "ws://192.168.1.2:3773",
        "ws://127.0.0.1",
        "ws://u:p@127.0.0.1:1",
        "ws://127.0.0.1:1/x",
        "ws://127.0.0.1:1?a=b",
    ] {
        assert!(ws_url(bad).is_err(), "{bad}");
    }
}

#[tokio::test]
async fn connects_with_the_bearer_header_and_acks_every_chunk() {
    let fake = FakeT3::start().await;
    for i in 0..5 {
        fake.put_thread(shell_thread(&format!("t{i}"), "idle", None));
    }
    let (conn, mut rx) = open(&fake).await;
    conn.subscribe(SUBSCRIBE_SHELL, json!({"requestCompletionMarker": true}))
        .unwrap();
    let mut kinds = vec![];
    while kinds.last().map(String::as_str) != Some("synchronized") {
        for v in next_chunk(&mut rx).await {
            kinds.push(v["kind"].as_str().unwrap().to_owned());
        }
    }
    assert_eq!(kinds, ["snapshot", "snapshot", "synchronized"]);
    fake.put_thread(shell_thread("t9", "running", None));
    fake.put_thread(shell_thread("t9", "completed", None));
    assert_eq!(next_chunk(&mut rx).await[0]["thread"]["status"], "running");
    assert_eq!(
        next_chunk(&mut rx).await[0]["thread"]["status"],
        "completed"
    );
    assert_eq!(fake.auth_headers(), vec![format!("Bearer {FAKE_TOKEN}")]);
    assert!(fake.unacked_peak() <= 1);
}

#[tokio::test]
async fn rejected_token_and_protocol_are_reported() {
    let fake = FakeT3::start().await;
    let url = ws_url(&fake.url()).unwrap();
    assert!(matches!(
        connect(&url, "wrong", &fast()).await,
        Err(ConnectError::Unauthorized)
    ));
    fake.reject(Some(Reject::Protocol));
    assert!(matches!(
        connect(&url, FAKE_TOKEN, &fast()).await,
        Err(ConnectError::Protocol)
    ));
    assert!(matches!(
        connect(
            "ws://127.0.0.1:9/ws?orchestrationProtocol=2",
            FAKE_TOKEN,
            &fast()
        )
        .await,
        Err(ConnectError::Unreachable)
    ));
}

#[tokio::test]
async fn calls_decode_success_and_failure() {
    let fake = FakeT3::start().await;
    fake.put_thread(shell_thread("t1", "idle", None));
    let (conn, _rx) = open(&fake).await;
    let v = conn
        .call(
            DISPATCH,
            &dispatch("c1"),
            Instant::now() + Duration::from_secs(2),
        )
        .await
        .unwrap();
    assert!(v["sequence"].as_u64().is_some());
    // A new commandId: the fake, like T3, would replay c1's receipt.
    fake.fail_next_dispatch("cmd_exit_respond_fail_not_found.json");
    let e = conn
        .call(
            DISPATCH,
            &dispatch("c2"),
            Instant::now() + Duration::from_secs(2),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(e, CallError::Failed(Failure::Fail { ref tag, .. }) if tag == "OrchestrationV2DispatchCommandError")
    );
}

#[tokio::test]
async fn reply_lost_after_write_is_no_reply() {
    let fake = FakeT3::start().await;
    let (conn, _rx) = open(&fake).await;
    fake.hang_next_dispatch();
    let e = conn
        .call(
            DISPATCH,
            &dispatch("c1"),
            Instant::now() + Duration::from_millis(300),
        )
        .await
        .unwrap_err();
    assert!(matches!(e, CallError::NoReply(_)), "{e:?}");
    assert_eq!(fake.dispatched().len(), 1);
}

#[tokio::test]
async fn a_send_error_after_the_frame_went_out_is_no_reply() {
    // Sol round 1, finding 1: T3 received the whole frame, but the local
    // send reported an error. This must never look like "not delivered".
    let fake = FakeT3::start().await;
    let (conn, _rx) = open(&fake).await;
    conn.fail_after_write_next();
    let e = conn
        .call(
            DISPATCH,
            &dispatch("c1"),
            Instant::now() + Duration::from_secs(2),
        )
        .await
        .unwrap_err();
    assert!(matches!(e, CallError::NoReply(_)), "{e:?}");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(fake.dispatched().len(), 1, "T3 got it");
}

#[tokio::test]
async fn a_dropped_call_is_never_written_later() {
    let fake = FakeT3::start().await;
    let (conn, _rx) = open(&fake).await;
    let gate = conn.pause_writer();
    let c = conn.clone();
    let call = tokio::spawn(async move {
        c.call(
            DISPATCH,
            &dispatch("c1"),
            Instant::now() + Duration::from_secs(5),
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    call.abort();
    let _ = call.await;
    gate.release();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(fake.dispatched().is_empty());
}

#[tokio::test]
async fn closed_connection_refuses_calls_without_sending() {
    let fake = FakeT3::start().await;
    let (conn, _rx) = open(&fake).await;
    fake.drop_connections();
    tokio::time::timeout(Duration::from_secs(2), conn.closed())
        .await
        .unwrap();
    let e = conn
        .call(
            DISPATCH,
            &dispatch("c1"),
            Instant::now() + Duration::from_secs(1),
        )
        .await
        .unwrap_err();
    assert!(matches!(e, CallError::NotConnected));
    assert!(conn.subscribe(SUBSCRIBE_SHELL, json!({})).is_err());
    assert!(fake.dispatched().is_empty());
}

#[tokio::test]
async fn silence_after_a_ping_closes_the_connection() {
    let fake = FakeT3::start().await;
    let (conn, _rx) = open(&fake).await;
    fake.mute(true);
    let why = tokio::time::timeout(Duration::from_secs(2), conn.closed())
        .await
        .unwrap();
    assert!(why.contains("ping"), "{why}");
}

#[tokio::test]
async fn an_undecodable_frame_closes_the_connection() {
    let fake = FakeT3::start().await;
    let (conn, _rx) = open(&fake).await;
    fake.send_raw("{not json");
    let why = tokio::time::timeout(Duration::from_secs(2), conn.closed())
        .await
        .unwrap();
    assert!(why.contains("cannot read"), "{why}");
}

#[tokio::test]
async fn a_stuck_writer_fills_the_control_lane_and_closes_promptly() {
    let fake = FakeT3::start().await;
    let (conn, _rx) = open(&fake).await;
    let _gate = conn.pause_writer(); // never released
    let started = std::time::Instant::now();
    let mut refused = false;
    for i in 0..(CONTROL_CAPACITY + 10) {
        if conn
            .subscribe(SUBSCRIBE_THREAD, json!({"threadId": format!("t{i}")}))
            .is_err()
        {
            refused = true;
            break;
        }
    }
    assert!(refused, "a full control lane closes the connection");
    tokio::time::timeout(Duration::from_secs(1), conn.closed())
        .await
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "subscribe never waits for the writer"
    );
    let e = conn
        .call(
            DISPATCH,
            &dispatch("c1"),
            Instant::now() + Duration::from_secs(1),
        )
        .await
        .unwrap_err();
    assert!(matches!(e, CallError::NotConnected));
}

#[tokio::test]
async fn a_peer_that_stops_reading_trips_the_write_timeout() {
    let fake = FakeT3::start().await;
    let (conn, _rx) = open(&fake).await;
    fake.stop_reading(true);
    let big = json!({"type": "message.dispatch", "text": "x".repeat(1024 * 1024)});
    let c = conn.clone();
    let flood = tokio::spawn(async move {
        for i in 0..64 {
            let _ = c
                .call(
                    DISPATCH,
                    &json!({"n": i, "p": big}),
                    Instant::now() + Duration::from_secs(5),
                )
                .await;
        }
    });
    let why = tokio::time::timeout(Duration::from_secs(3), conn.closed())
        .await
        .expect("bounded shutdown");
    assert!(why.contains("stalled"), "{why}");
    flood.abort();
}

#[tokio::test]
async fn many_subscriptions_keep_the_reader_moving() {
    let fake = FakeT3::start().await;
    for i in 0..300 {
        let t = format!("t{i}");
        fake.put_thread(shell_thread(&t, "running", None));
        fake.set_projection(&t, approval_projection(&t, "rq-unused", None));
    }
    let (conn, mut rx) = open(&fake).await;
    let ids: Vec<String> = (0..300)
        .map(|i| {
            conn.subscribe(
                SUBSCRIBE_THREAD,
                json!({"threadId": format!("t{i}"), "requestCompletionMarker": true}),
            )
            .unwrap()
        })
        .collect();
    let mut synced = std::collections::HashSet::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        while synced.len() < ids.len() {
            if let Some(StreamMsg::Chunk { stream, values }) = rx.recv().await
                && values.iter().any(|v| v["kind"] == "synchronized")
            {
                synced.insert(stream);
            }
        }
    })
    .await
    .expect("all 300 streams synchronize");
    assert!(!conn.is_closed());
}

fn respond(cmd: &str, thread: &str, req: &str) -> Value {
    json!({"type":"runtime-request.respond","commandId":cmd,"threadId":thread,"requestId":req,"decision":"accept"})
}
fn soon() -> Instant {
    Instant::now() + Duration::from_secs(2)
}

#[tokio::test]
async fn shell_resume_sends_the_project_snapshot_then_the_missed_events() {
    let fake = FakeT3::start().await;
    let first = fake.put_thread(shell_thread("t1", "idle", None));
    let second = fake.put_thread(shell_thread("t1", "running", None));
    let (conn, mut rx) = open(&fake).await;
    conn.subscribe(
        SUBSCRIBE_SHELL,
        json!({"afterSequence": first, "requestCompletionMarker": true}),
    )
    .unwrap();
    let mut seen = vec![];
    while seen
        .last()
        .map(|v: &Value| v["kind"] != "synchronized")
        .unwrap_or(true)
    {
        seen.extend(next_chunk(&mut rx).await);
    }
    assert!(is_project_only(&seen[0]));
    assert_eq!(seen[1]["kind"], "thread.updated");
    assert_eq!(item_sequence(&seen[1]), Some(second));
    assert_eq!(seen.len(), 3);
    // A negative cursor is a Die; the stream ends with it.
    conn.subscribe(SUBSCRIBE_SHELL, json!({"afterSequence": -1}))
        .unwrap();
    loop {
        match rx.recv().await.unwrap() {
            StreamMsg::End { failure, .. } => {
                assert!(matches!(failure, Some(Failure::Die(_))));
                break;
            }
            StreamMsg::Chunk { .. } => {}
        }
    }
}

#[tokio::test]
async fn respond_resolves_once_and_a_reused_command_id_replays_the_receipt() {
    let fake = FakeT3::start().await;
    fake.put_thread(shell_thread("t1", "running", Some(("rq1", "command"))));
    fake.set_projection("t1", approval_projection("t1", "rq1", Some("native-1")));
    let (conn, mut rx) = open(&fake).await;
    conn.subscribe(
        SUBSCRIBE_THREAD,
        json!({"threadId": "t1", "requestCompletionMarker": true}),
    )
    .unwrap();
    let snap = next_chunk(&mut rx).await;
    assert_eq!(snap[0]["kind"], "snapshot");
    assert_eq!(fake.live_thread_streams(), ["t1"]);
    conn.call(DISPATCH, &respond("c1", "t1", "rq1"), soon())
        .await
        .unwrap();
    assert_eq!(next_chunk(&mut rx).await[0]["kind"], "synchronized");
    let ev = next_chunk(&mut rx).await;
    assert_eq!(ev[0]["event"]["type"], "runtime-request.updated");
    assert_eq!(ev[0]["event"]["payload"]["status"], "resolved");
    let e = conn
        .call(DISPATCH, &respond("c2", "t1", "rq1"), soon())
        .await
        .unwrap_err();
    assert!(
        matches!(e, CallError::Failed(Failure::Fail { ref message, .. }) if message == "Runtime request rq1 is resolved."),
        "{e:?}"
    );
    // Same commandId, different payload: the first receipt comes back.
    let again = conn
        .call(DISPATCH, &respond("c1", "t1", "nope"), soon())
        .await
        .unwrap();
    assert!(again["sequence"].as_u64().is_some());
    let e = conn
        .call(DISPATCH, &respond("c3", "t1", "gone"), soon())
        .await
        .unwrap_err();
    assert!(
        matches!(e, CallError::Failed(Failure::Fail { ref message, .. }) if message == "Runtime request gone was not found."),
        "{e:?}"
    );
}

#[tokio::test]
async fn restart_cancels_pending_requests_and_a_resume_sees_them() {
    let fake = FakeT3::start().await;
    fake.put_thread(shell_thread("t1", "running", Some(("rq1", "command"))));
    fake.set_projection("t1", approval_projection("t1", "rq1", None));
    let (conn, _rx) = open(&fake).await;
    let before = fake.head();
    fake.restart();
    tokio::time::timeout(Duration::from_secs(2), conn.closed())
        .await
        .unwrap();
    let (conn2, mut rx2) = open(&fake).await;
    conn2
        .subscribe(
            SUBSCRIBE_THREAD,
            json!({"threadId": "t1", "afterSequence": before, "requestCompletionMarker": true}),
        )
        .unwrap();
    let ev = next_chunk(&mut rx2).await;
    assert_eq!(
        ev[0]["event"]["payload"]["responseCapability"]["type"],
        "not_resumable"
    );
    let e = conn2
        .call(DISPATCH, &respond("c1", "t1", "rq1"), soon())
        .await
        .unwrap_err();
    assert!(
        matches!(e, CallError::Failed(Failure::Fail { .. })),
        "{e:?}"
    );
    assert_eq!(fake.connections(), 2);
}

#[tokio::test]
async fn interrupt_unknown_tags_and_a_held_reply() {
    let fake = FakeT3::start().await;
    fake.put_thread(shell_thread("t1", "idle", None));
    let (conn, mut rx) = open(&fake).await;
    let e = conn
        .call("orchestration.nope", &json!({}), soon())
        .await
        .unwrap_err();
    assert!(
        matches!(e, CallError::Failed(Failure::Die(ref d)) if d.contains("orchestration.nope")),
        "{e:?}"
    );
    let id = conn.subscribe(SUBSCRIBE_SHELL, json!({})).unwrap();
    next_chunk(&mut rx).await;
    conn.interrupt(&id);
    tokio::time::sleep(Duration::from_millis(100)).await;
    fake.put_thread(shell_thread("t1", "running", None)); // nobody is listening
    assert!(!conn.is_closed());
    let gate = fake.gate_next_dispatch();
    let c = conn.clone();
    let call = tokio::spawn(async move {
        c.call(
            DISPATCH,
            &dispatch("c1"),
            Instant::now() + Duration::from_secs(5),
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(fake.dispatched().len(), 1);
    assert!(!call.is_finished());
    gate.release();
    assert!(call.await.unwrap().unwrap()["sequence"].as_u64().is_some());
}

#[tokio::test]
async fn a_denied_dispatch_is_a_typed_failure_and_not_recorded() {
    let fake = FakeT3::start().await;
    fake.deny_operate(true);
    let (conn, _rx) = open(&fake).await;
    let e = conn
        .call(DISPATCH, &dispatch("c1"), soon())
        .await
        .unwrap_err();
    assert!(
        matches!(e, CallError::Failed(Failure::Fail { ref tag, .. }) if tag == "EnvironmentAuthorizationError"),
        "{e:?}"
    );
    assert!(fake.dispatched().is_empty());
}

#[tokio::test]
async fn a_token_that_is_not_visible_ascii_is_rejected_before_connecting() {
    let url = "ws://127.0.0.1:9/ws?orchestrationProtocol=2";
    for bad in ["", "has space", "line\nbreak", "tøken"] {
        assert!(
            matches!(
                connect(url, bad, &fast()).await,
                Err(ConnectError::Unauthorized)
            ),
            "{bad:?}"
        );
    }
}
