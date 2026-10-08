use std::{os::unix::fs::PermissionsExt, sync::Arc, time::Duration};

use acd_adapters::t3::{T3Adapter, T3Config, conn::ConnTiming, fake::*, reconciler::T3Timing};
use acd_core::{Adapter, Core, DeliverError, testing::test_machine};
use acd_protocol::*;
use serde_json::json;
use uuid::Uuid;

async fn until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    for _ in 0..250 {
        if let Some(v) = f() {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}

struct Rig {
    fake: FakeT3,
    core: Arc<Core>,
    adapter: Arc<T3Adapter>,
    _dir: tempfile::TempDir,
}

async fn rig(fake: FakeT3) -> Rig {
    let dir = tempfile::tempdir().unwrap();
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
    let cfg = T3Config {
        enabled: true,
        url: Some(fake.url()),
        token_path: token,
        timing,
        dispatch_budget: Duration::from_millis(500),
    };
    let adapter = T3Adapter::new(cfg);
    let core = Core::new(test_machine(), vec![adapter.clone()]);
    adapter.start(core.clone());
    Rig {
        fake,
        core,
        adapter,
        _dir: dir,
    }
}

async fn with_pending(projection: serde_json::Value, kind: &str) -> (Rig, Request) {
    let fake = FakeT3::start().await;
    fake.put_thread(shell_thread("a", "running", Some(("rq-1", kind))));
    fake.set_projection("a", projection);
    let r = rig(fake).await;
    let req = until("pending", || r.core.agent("t3:a")?.pending).await;
    (r, req)
}

async fn answer(r: &Rig, req: &Request, option: &str) -> CommandResult {
    r.core
        .handle(
            "c",
            ClientMessage::RequestAnswer {
                id: "x1".into(),
                request_id: req.id.clone(),
                option_id: Some(option.into()),
                text: None,
            },
        )
        .await
}

fn uuid4(v: &serde_json::Value) -> bool {
    v.as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .is_some_and(|u| u.get_version_num() == 4)
}

#[tokio::test]
async fn approval_answer_is_delivered_and_resolves() {
    let (r, req) = with_pending(approval_projection("a", "rq-1", None), "command").await;
    let (_, mut rx) = r.core.attach(None);
    let res = answer(&r, &req, "accept").await;
    assert_eq!(res.outcome, Some(Outcome::Delivered), "{res:?}");
    let sent = r.fake.dispatched();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        (
            sent[0]["type"].as_str(),
            sent[0]["decision"].as_str(),
            sent[0]["requestId"].as_str(),
            sent[0]["threadId"].as_str()
        ),
        (
            Some("runtime-request.respond"),
            Some("accept"),
            Some("rq-1"),
            Some("a")
        )
    );
    assert!(uuid4(&sent[0]["commandId"]));
    loop {
        if let ServerMessage::RequestResolved {
            request_id,
            by,
            option_id,
            ..
        } = rx.recv().await.unwrap()
            && request_id == req.id
        {
            assert_eq!(
                (by, option_id.as_deref()),
                (ResolvedBy::Inbox, Some("accept"))
            );
            break;
        }
    }
}

#[tokio::test]
async fn question_answer_sends_value_by_question_id() {
    let (r, req) = with_pending(question_projection("a", "rq-1"), "user_input").await;
    assert_eq!(
        answer(&r, &req, "o2").await.outcome,
        Some(Outcome::Delivered)
    );
    assert_eq!(
        r.fake.dispatched()[0]["answers"],
        json!({"Which color do you prefer?": "blue"})
    );
}

#[tokio::test]
async fn source_terminal_end_waits_for_core_claim_release() {
    let (r, req) = with_pending(approval_projection("a", "rq-1", None), "command").await;
    r.fake
        .fail_next_dispatch("cmd_exit_respond_fail_already_resolved.json");
    assert_eq!(
        answer(&r, &req, "accept").await.error_code(),
        Some(ErrorCode::RequestGone)
    );
    until("terminal request end", || {
        r.core.request(&req.id).is_none().then_some(())
    })
    .await;
}

#[tokio::test]
async fn cancelled_request_ends_as_lost() {
    let (r, req) = with_pending(approval_projection("a", "rq-1", None), "command").await;
    r.fake
        .fail_next_dispatch("cmd_exit_respond_fail_cancelled_after_restart.json");
    assert_eq!(
        answer(&r, &req, "accept").await.error_code(),
        Some(ErrorCode::RequestGone)
    );
    until("lost request end", || {
        r.core.request(&req.id).is_none().then_some(())
    })
    .await;
}

#[tokio::test]
async fn missing_operate_scope_keeps_request_open_and_degrades_source() {
    let (r, req) = with_pending(approval_projection("a", "rq-1", None), "command").await;
    r.fake.deny_operate(true);
    assert_eq!(
        answer(&r, &req, "accept").await.error_code(),
        Some(ErrorCode::SourceUnavailable)
    );
    assert_eq!(r.core.request(&req.id).unwrap().status, RequestStatus::Open);
    until("degraded source", || {
        let ServerMessage::Welcome { machine, .. } = r.core.welcome() else {
            panic!()
        };
        (machine.sources.t3.state == SourceState::Degraded).then_some(())
    })
    .await;
}

#[tokio::test]
async fn disconnected_answer_is_source_unavailable_and_sends_nothing() {
    let (r, req) = with_pending(approval_projection("a", "rq-1", None), "command").await;
    r.fake.reject(Some(Reject::Token));
    r.fake.drop_connections();
    until("unknown agent after disconnect", || {
        (r.core.agent("t3:a")?.state == AgentState::Unknown).then_some(())
    })
    .await;
    assert_eq!(
        answer(&r, &req, "accept").await.error_code(),
        Some(ErrorCode::SourceUnavailable)
    );
    assert_eq!(r.core.request(&req.id).unwrap().status, RequestStatus::Open);
    assert!(r.fake.dispatched().is_empty());
}

#[tokio::test]
async fn lost_reply_is_uncertain_and_not_resent() {
    let (r, req) = with_pending(approval_projection("a", "rq-1", None), "command").await;
    r.fake.hang_next_dispatch();
    let res = answer(&r, &req, "accept").await;
    assert_eq!(res.outcome, Some(Outcome::Delivered));
    assert_eq!(r.fake.dispatched().len(), 1);
    assert_eq!(answer(&r, &req, "accept").await, res);
}

#[tokio::test]
async fn follow_ups_dispatch_with_fresh_ids_and_validate_text() {
    let fake = FakeT3::start().await;
    fake.put_thread(shell_thread("a", "completed", None));
    let r = rig(fake).await;
    let agent = until("agent", || r.core.agent("t3:a")).await;
    r.adapter.send(&agent, "run tests").await.unwrap();
    r.adapter.send(&agent, "push").await.unwrap();
    let sent = r.fake.dispatched();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0]["type"], "message.dispatch");
    assert_eq!(sent[0]["dispatchMode"], json!({"type":"start_immediately"}));
    assert!(uuid4(&sent[0]["commandId"]) && uuid4(&sent[1]["commandId"]));
    assert_ne!(sent[0]["commandId"], sent[1]["commandId"]);
    assert!(uuid4(&sent[0]["messageId"]) && uuid4(&sent[1]["messageId"]));
    assert_ne!(sent[0]["messageId"], sent[1]["messageId"]);
    assert!(matches!(
        r.adapter.send(&agent, "a\u{202E}b").await,
        Err(DeliverError::NotDelivered(ErrorCode::BadRequest))
    ));
    let mut gone = agent.clone();
    gone.id = "t3:nope".into();
    assert!(matches!(
        r.adapter.send(&gone, "hi").await,
        Err(DeliverError::NotDelivered(ErrorCode::AgentGone))
    ));
    assert_eq!(r.fake.dispatched().len(), 2);
}

#[tokio::test]
async fn a_written_follow_up_retry_returns_the_cached_uncertain_result() {
    let fake = FakeT3::start().await;
    fake.put_thread(shell_thread("a", "completed", None));
    let r = rig(fake).await;
    let agent = until("agent", || r.core.agent("t3:a")).await;
    r.adapter
        .current_conn_for_tests()
        .unwrap()
        .fail_after_write_next();
    let msg = ClientMessage::AgentSend {
        id: "f1".into(),
        agent_id: agent.id,
        text: "hi".into(),
    };
    let first = r.core.handle("c", msg.clone()).await;
    assert_eq!(first.outcome, Some(Outcome::Delivered));
    until("reconnected", || r.adapter.current_conn_for_tests()).await;
    assert_eq!(r.core.handle("c", msg).await, first);
    assert_eq!(r.fake.dispatched().len(), 1);
}

#[tokio::test]
async fn disabled_and_unconfigured_report_status() {
    for (enabled, url, want) in [
        (
            false,
            Some("http://127.0.0.1:1".to_owned()),
            SourceState::Disabled,
        ),
        (true, None, SourceState::Unconfigured),
    ] {
        let adapter = T3Adapter::new(T3Config::new(enabled, url, "/nonexistent/t3.token".into()));
        let core = Core::new(test_machine(), vec![adapter.clone()]);
        adapter.start(core.clone());
        let ServerMessage::Welcome { machine, .. } = core.welcome() else {
            panic!()
        };
        assert_eq!(machine.sources.t3.state, want);
    }
}
