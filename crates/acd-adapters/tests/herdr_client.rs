use acd_adapters::herdr::{client::*, fake::{FakeHerdr, fake_agent}, wire::*};

fn fixture(name: &str) -> serde_json::Value {
    let p = format!("{}/../../tests/fixtures/herdr/api/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

#[test]
fn recorded_frames_parse() {
    let info: AgentInfo = serde_json::from_value(fixture("agent.get.claude-blocked.json")["result"]["agent"].clone()).unwrap();
    assert_eq!((info.pane_id.as_str(), info.agent.as_deref(), info.agent_status), ("w1:p2", Some("claude"), AgentStatus::Blocked));
    assert!(info.agent_session.unwrap().value.is_some());
    let list: Vec<AgentInfo> = serde_json::from_value(fixture("agent.list.claude-blocked.json")["result"]["agents"].clone()).unwrap();
    assert!(!list.is_empty());
    let line = |n: &str| fixture(n).to_string();
    assert_eq!(parse_event(&line("event-pane_created.json")), Event::Created { pane_id: "w1:p3".into() });
    assert_eq!(parse_event(&line("event-pane_closed.json")), Event::Closed { pane_id: "w1:p3".into() });
    assert_eq!(parse_event(&line("event-pane_agent_detected.json")), Event::AgentDetected { pane_id: "w1:p2".into() });
    assert_eq!(parse_event(&line("event-pane_agent_detected.released.json")), Event::AgentReleased { pane_id: "w1:p1".into() });
    assert_eq!(parse_event(&line("event-pane_updated.json")), Event::Updated { pane_id: "w1:p2".into() });
    assert_eq!(parse_event(&line("event-pane.agent_status_changed.blocked.json")),
        Event::StatusChanged { pane_id: "w1:p2".into(), status: AgentStatus::Blocked });
    assert_eq!(parse_event("not json"), Event::Other);
    assert_eq!(parse_event(r#"{"event":"pane_exploded","data":{}}"#), Event::Other);
}

#[tokio::test]
async fn client_tolerates_unknown_fields_and_errors() {
    let fake = FakeHerdr::start().await;
    fake.set_agent(serde_json::json!({
        "pane_id": "w1:p2", "terminal_id": "t", "workspace_id": "w1", "tab_id": "w1:t1",
        "focused": false, "revision": 1, "agent": "claude", "agent_status": "teleporting", "new_field": [1]
    }));
    let h = Herdr::new(fake.path().to_path_buf());
    let pong = h.ping().await.unwrap();
    assert_eq!((pong.version.as_str(), pong.protocol), ("0.9.3", 22));
    let list = h.list().await.unwrap();
    assert_eq!(list[0].agent_status, AgentStatus::Unknown);
    assert!(h.get("w9:p9").await.unwrap().is_none(), "agent_not_found is Ok(None)");
    match h.call("no.such.method", serde_json::json!({})).await {
        Err(CallError::Server { code, .. }) => assert_eq!(code, "invalid_request"),
        other => panic!("{other:?}"),
    }
    assert_eq!(fake.connections_reused(), 0, "the client must open one connection per request");
}

#[tokio::test]
async fn write_then_silence_is_no_reply_and_counts_as_reached() {
    let fake = FakeHerdr::start().await;
    fake.set_agent(fake_agent("w1:p2", "claude", "idle"));
    fake.hang_next("agent.prompt"); // reads the request, records it, never replies
    let h = Herdr::new(fake.path().to_path_buf());
    let err = tokio::time::timeout(std::time::Duration::from_secs(5), h.prompt("w1:p2", "hi")).await.unwrap().unwrap_err();
    assert!(matches!(err, CallError::NoReply(_)), "{err:?}");
    assert!(err.reached_agent());
    assert_eq!(fake.prompts(), vec![("w1:p2".to_string(), "hi".to_string())]);
}

#[tokio::test]
async fn connect_failure_did_not_reach_the_agent() {
    let dir = tempfile::tempdir().unwrap();
    let h = Herdr::new(dir.path().join("missing.sock"));
    let err = h.prompt("w1:p2", "hi").await.unwrap_err();
    assert!(matches!(err, CallError::Connect(_)));
    assert!(!err.reached_agent());
}

#[test]
fn only_known_refusals_count_as_not_reached() {
    let e = |code: &str| CallError::Server { code: code.into(), message: String::new() };
    assert!(!e("agent_blocked").reached_agent());
    assert!(!e("invalid_key").reached_agent());
    assert!(e("internal_error").reached_agent(), "an unknown error after the write may have acted");
}

#[tokio::test]
async fn whole_call_shares_one_deadline() {
    let fake = FakeHerdr::start().await;
    fake.hang_next("agent.list");
    let h = Herdr::new(fake.path().to_path_buf());
    let t = std::time::Instant::now();
    let _ = h.list().await;
    assert!(t.elapsed() < std::time::Duration::from_millis(3500), "connect + write + reply share CALL_TIMEOUT");
}

#[tokio::test]
async fn blocked_prompt_is_a_server_error() {
    let fake = FakeHerdr::start().await;
    fake.set_agent(fake_agent("w1:p2", "claude", "blocked"));
    let h = Herdr::new(fake.path().to_path_buf());
    match h.prompt("w1:p2", "hi").await {
        Err(CallError::Server { code, .. }) => assert_eq!(code, "agent_blocked"),
        other => panic!("{other:?}"),
    }
    assert!(fake.prompts().is_empty());
}

#[tokio::test]
async fn subscription_streams_pushed_events_and_ends_on_close() {
    let fake = FakeHerdr::start().await;
    let h = Herdr::new(fake.path().to_path_buf());
    let mut sub = h.subscribe(&["w1:p2".into()]).await.unwrap();
    assert_eq!(fake.subscriptions().last().unwrap(), &vec!["pane.created", "pane.closed", "pane.agent_detected", "pane.updated", "pane.agent_status_changed:w1:p2"]);
    fake.push_raw("garbage");
    fake.push_event(serde_json::json!({"event":"pane_closed","data":{"pane_id":"w1:p2","type":"pane_closed","workspace_id":"w1"}}));
    assert_eq!(sub.next().await, Some(Event::Other));
    assert_eq!(sub.next().await, Some(Event::Closed { pane_id: "w1:p2".into() }));
    fake.drop_subscribers();
    assert_eq!(sub.next().await, None);
}
