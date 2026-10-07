use std::{sync::Arc, time::Duration};

use acd_core::testing::*;
use acd_core::*;
use acd_protocol::*;

fn answer(id: &str, request_id: &str) -> ClientMessage {
    ClientMessage::RequestAnswer { id: id.into(), request_id: request_id.into(), option_id: Some("allow".into()), text: None }
}

fn open_request(core: &Core) -> String {
    core.observe(Observation::AgentUpsert(agent_obs("a1")));
    core.observe(Observation::RequestSeen(permission_obs("a1", "k1")));
    let (initial, _) = core.attach(None);
    let ServerMessage::Snapshot { agents, .. } = &initial[0] else { panic!("expected snapshot") };
    agents[0].pending.as_ref().unwrap().id.clone()
}

#[tokio::test]
async fn answer_is_delivered_then_resolved_by_source_evidence() {
    let fake = FakeAdapter::new();
    let core = Core::new(test_machine(), vec![fake.clone()]);
    let rid = open_request(&core);
    let res = core.handle("cl", answer("c1", &rid)).await;
    assert_eq!(res, CommandResult::delivered("c1"));
    assert_eq!(fake.delivered(), vec![(rid.clone(), Answer::Option("allow".into()))]);
    let (_, mut rx) = core.attach(None);
    core.observe(Observation::RequestEnded { key: CorrelationKey("k1".into()), by: ResolvedBy::Terminal });
    loop {
        if let ServerMessage::RequestResolved { by, option_id, .. } = rx.recv().await.unwrap() {
            assert_eq!((by, option_id.as_deref()), (ResolvedBy::Inbox, Some("allow")));
            break;
        }
    }
}

#[tokio::test]
async fn pre_delivery_error_releases_and_is_not_cached() {
    let fake = FakeAdapter::new();
    let core = Core::new(test_machine(), vec![fake.clone()]);
    let rid = open_request(&core);
    fake.fail_next(DeliverError::NotDelivered(ErrorCode::ScreenMismatch));
    let first = core.handle("cl", answer("c1", &rid)).await;
    assert_eq!(first.error_code(), Some(ErrorCode::ScreenMismatch));
    let retry = core.handle("cl", answer("c2", &rid)).await;
    assert_eq!(retry, CommandResult::delivered("c2"), "request went back to open");

    let fake2 = FakeAdapter::new();
    let core2 = Core::new(test_machine(), vec![fake2.clone()]);
    let rid2 = open_request(&core2);
    fake2.fail_next(DeliverError::NotDelivered(ErrorCode::RequestNotReady));
    assert_eq!(core2.handle("cl", answer("same", &rid2)).await.error_code(), Some(ErrorCode::RequestNotReady));
    assert_eq!(core2.handle("cl", answer("same", &rid2)).await, CommandResult::delivered("same"));
}

#[tokio::test]
async fn concurrent_retry_delivers_once() {
    let fake = FakeAdapter::gated();
    let core = Core::new(test_machine(), vec![fake.clone()]);
    let rid = open_request(&core);
    let (c1, c2) = (core.clone(), core.clone());
    let (r1, r2) = (rid.clone(), rid.clone());
    let a = tokio::spawn(async move { c1.handle("cl", answer("dup", &r1)).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let b = tokio::spawn(async move { c2.handle("cl", answer("dup", &r2)).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    fake.open_gate();
    let (ra, rb) = (a.await.unwrap(), b.await.unwrap());
    assert_eq!(ra, rb);
    assert_eq!(fake.delivered().len(), 1);
}

#[tokio::test]
async fn reused_id_with_different_payload_is_rejected() {
    let fake = FakeAdapter::new();
    let core = Core::new(test_machine(), vec![fake]);
    let rid = open_request(&core);
    core.handle("cl", answer("c1", &rid)).await;
    let other = ClientMessage::AgentMarkSeen { id: "c1".into(), agent_id: "a1".into() };
    assert_eq!(core.handle("cl", other).await.error_code(), Some(ErrorCode::DuplicateMismatch));
}

#[tokio::test]
async fn agent_gone_during_delivery_resolves_lost_without_panic() {
    let fake = FakeAdapter::gated();
    let core = Core::new(test_machine(), vec![fake.clone()]);
    let rid = open_request(&core);
    let c = core.clone();
    let r = rid.clone();
    let task = tokio::spawn(async move { c.handle("cl", answer("c1", &r)).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    core.observe(Observation::AgentGone { agent_id: "a1".into() });
    fake.fail_next(DeliverError::NotDelivered(ErrorCode::SourceUnavailable));
    fake.open_gate();
    let res = task.await.unwrap();
    assert_eq!(res.error_code(), Some(ErrorCode::SourceUnavailable));
    let (initial, _) = core.attach(None);
    let ServerMessage::Snapshot { agents, .. } = &initial[0] else { panic!() };
    assert!(agents.is_empty());
}

#[tokio::test]
async fn uncertain_delivery_resolves_uncertain() {
    let fake = FakeAdapter::new();
    let core = Core::new(test_machine(), vec![fake.clone()]);
    let rid = open_request(&core);
    fake.fail_next(DeliverError::Uncertain("no confirmation".into()));
    let (_, mut rx) = core.attach(None);
    assert_eq!(core.handle("cl", answer("c1", &rid)).await, CommandResult::delivered("c1"));
    loop {
        if let ServerMessage::RequestResolved { by, .. } = rx.recv().await.unwrap() {
            assert_eq!(by, ResolvedBy::Uncertain);
            break;
        }
    }
}

#[tokio::test]
async fn missing_adapter_returns_source_unavailable_and_reopens() {
    let core = Core::new(test_machine(), vec![]);
    let rid = open_request(&core);
    assert_eq!(core.handle("cl", answer("c1", &rid)).await.error_code(), Some(ErrorCode::SourceUnavailable));
    let (initial, _) = core.attach(None);
    let ServerMessage::Snapshot { agents, .. } = &initial[0] else { panic!() };
    assert_eq!(agents[0].pending.as_ref().unwrap().status, RequestStatus::Open);
}

#[tokio::test]
async fn attach_replays_within_epoch_and_snapshots_otherwise() {
    let core = Core::new(test_machine(), vec![]);
    core.observe(Observation::AgentUpsert(agent_obs("a1")));
    let epoch = core.epoch();
    core.observe(Observation::AgentUpsert(AgentObs { title: "renamed".into(), ..agent_obs("a1") }));
    let (replay, _) = core.attach(Some(&Cursor { epoch: epoch.clone(), seq: 1 }));
    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0].seq(), Some(2));
    let (snap, _) = core.attach(Some(&Cursor { epoch: "old".into(), seq: 1 }));
    assert!(matches!(snap[0], ServerMessage::Snapshot { seq: 2, .. }));
}

#[tokio::test]
async fn follow_up_send_goes_to_source_adapter() {
    let fake = FakeAdapter::new();
    let core = Core::new(test_machine(), vec![fake.clone()]);
    core.observe(Observation::AgentUpsert(AgentObs { state: AgentState::Done, ..agent_obs("a1") }));
    let res = core.handle("cl", ClientMessage::AgentSend { id: "s1".into(), agent_id: "a1".into(), text: "run tests".into() }).await;
    assert_eq!(res, CommandResult::delivered("s1"));
    assert_eq!(fake.sent(), vec![("a1".to_string(), "run tests".to_string())]);
}

#[test]
fn welcome_lists_v1_capabilities() {
    let core = Core::new(test_machine(), vec![]);
    let ServerMessage::Welcome { capabilities, .. } = core.welcome() else { panic!() };
    assert_eq!(capabilities, vec!["answer", "send", "mark_seen"]);
    let _: Arc<Core> = core;
}

#[tokio::test]
async fn panicking_adapter_releases_id_and_claim() {
    let fake = FakeAdapter::new();
    let core = Core::new(test_machine(), vec![fake.clone()]);
    let rid = open_request(&core);
    fake.panic_next();
    let c = core.clone();
    let r = rid.clone();
    let joined = tokio::spawn(async move { c.handle("cl", answer("p", &r)).await }).await;
    assert!(joined.is_err(), "handle should have panicked");
    let retry = tokio::time::timeout(Duration::from_secs(2), core.handle("cl", answer("p", &rid)))
        .await
        .expect("retry must not hang");
    assert_eq!(retry, CommandResult::delivered("p"));
    assert_eq!(fake.delivered().len(), 1);
}
