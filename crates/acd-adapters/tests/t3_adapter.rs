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
    answer_with_id(r, req, option, "x1").await
}

async fn answer_with_id(r: &Rig, req: &Request, option: &str, command_id: &str) -> CommandResult {
    r.core
        .handle(
            "c",
            ClientMessage::RequestAnswer {
                id: command_id.into(),
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
    let (_, mut rx) = r.core.attach(None);
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
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let ServerMessage::RequestResolved { request_id, by, .. } = rx.recv().await.unwrap()
                && request_id == req.id
            {
                assert_eq!(by, ResolvedBy::Terminal);
                break;
            }
        }
    })
    .await
    .expect("timed out waiting for terminal resolution");
}

#[tokio::test]
async fn superseding_request_during_held_success_resolves_old_uncertain() {
    let (r, req) = with_pending(approval_projection("a", "rq-1", None), "command").await;
    let (_, mut rx) = r.core.attach(None);
    let gate = r.fake.gate_next_dispatch();
    let core = r.core.clone();
    let id = req.id.clone();
    let answer_task = tokio::spawn(async move {
        core.handle(
            "c",
            ClientMessage::RequestAnswer {
                id: "held".into(),
                request_id: id,
                option_id: Some("accept".into()),
                text: None,
            },
        )
        .await
    });
    until("answering", || {
        (r.core.request(&req.id)?.status == RequestStatus::Answering).then_some(())
    })
    .await;
    r.fake.push_thread_event("a", "runtime-request.updated", json!({"id":"rq-2","kind":"command","status":"pending","title":"new","responseCapability":{"type":"user_action"}}));
    r.fake
        .put_thread(shell_thread("a", "running", Some(("rq-2", "command"))));
    gate.release();
    assert_eq!(answer_task.await.unwrap().outcome, Some(Outcome::Delivered));
    let mut saw_uncertain = false;
    let mut saw_new = false;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !(saw_uncertain && saw_new) {
            match rx.recv().await.unwrap() {
                ServerMessage::RequestResolved { request_id, by, .. } if request_id == req.id => {
                    assert_eq!(by, ResolvedBy::Uncertain);
                    saw_uncertain = true;
                }
                ServerMessage::RequestOpened { request, .. }
                    if request.agent_id == "t3:a" && request.id != req.id =>
                {
                    saw_new = true
                }
                _ => {}
            }
        }
    })
    .await
    .expect("old uncertain and new request opened");
}

#[tokio::test]
async fn removing_thread_during_held_success_resolves_uncertain() {
    let (r, req) = with_pending(approval_projection("a", "rq-1", None), "command").await;
    let (_, mut rx) = r.core.attach(None);
    let gate = r.fake.gate_next_dispatch();
    let core = r.core.clone();
    let id = req.id.clone();
    let answer_task = tokio::spawn(async move {
        core.handle(
            "c",
            ClientMessage::RequestAnswer {
                id: "held-remove".into(),
                request_id: id,
                option_id: Some("accept".into()),
                text: None,
            },
        )
        .await
    });
    until("answering", || {
        (r.core.request(&req.id)?.status == RequestStatus::Answering).then_some(())
    })
    .await;
    r.fake.remove_thread("a");
    gate.release();
    assert_eq!(answer_task.await.unwrap().outcome, Some(Outcome::Delivered));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let ServerMessage::RequestResolved { request_id, by, .. } = rx.recv().await.unwrap()
                && request_id == req.id
            {
                assert_eq!(by, ResolvedBy::Uncertain);
                break;
            }
        }
    })
    .await
    .expect("uncertain removal resolution");
}

#[tokio::test]
async fn cancelled_request_ends_as_lost() {
    let (r, req) = with_pending(approval_projection("a", "rq-1", None), "command").await;
    let (_, mut rx) = r.core.attach(None);
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
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let ServerMessage::RequestResolved { request_id, by, .. } = rx.recv().await.unwrap()
                && request_id == req.id
            {
                assert_eq!(by, ResolvedBy::Lost);
                break;
            }
        }
    })
    .await
    .expect("timed out waiting for lost resolution");
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
    let (_, mut rx) = r.core.attach(None);
    r.fake.hang_next_dispatch();
    let res = answer(&r, &req, "accept").await;
    assert_eq!(res.outcome, Some(Outcome::Delivered));
    assert_eq!(r.fake.dispatched().len(), 1);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let ServerMessage::RequestResolved { request_id, by, .. } = rx.recv().await.unwrap()
                && request_id == req.id
            {
                assert_eq!(by, ResolvedBy::Uncertain);
                break;
            }
        }
    })
    .await
    .expect("timed out waiting for uncertain resolution");
    assert_eq!(answer(&r, &req, "accept").await, res);
}

#[tokio::test]
async fn hostile_failure_text_never_reaches_warn_logs_or_source_status() {
    use std::sync::Mutex as StdMutex;

    #[derive(Clone, Default)]
    struct Buffer(Arc<StdMutex<Vec<u8>>>);
    impl std::io::Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let buffer = Buffer::default();
    let writer = buffer.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let hostile =
        "sk-ant-api03-SECRETSECRET Bearer abc.def.ghi \u{1b}[31m rm -rf ~/work \u{202E}evil";
    let (r, req) = with_pending(approval_projection("a", "rq-1", None), "command").await;
    r.fake.fail_next_dispatch_value(
        json!({"_tag":"Failure","cause":[{"_tag":"Die","defect":hostile}]}),
    );
    let result = answer(&r, &req, "accept").await;
    assert_eq!(result.outcome, Some(Outcome::Delivered));
    let fake = FakeT3::start().await;
    fake.put_thread(shell_thread("send", "completed", None));
    let r_send = rig(fake).await;
    let agent = until("send agent", || r_send.core.agent("t3:send")).await;
    r_send.fake.fail_next_dispatch_value(
        json!({"_tag":"Failure","cause":[{"_tag":"Die","defect":hostile}]}),
    );
    let follow_up = r_send
        .core
        .handle(
            "c",
            ClientMessage::AgentSend {
                id: "follow-up".into(),
                agent_id: agent.id,
                text: "continue".into(),
            },
        )
        .await;
    assert_eq!(follow_up.outcome, Some(Outcome::Delivered), "{follow_up:?}");
    let (r2, req2) = with_pending(approval_projection("a", "rq-1", None), "command").await;
    r2.fake.fail_next_dispatch_value(json!({"_tag":"Failure","cause":[{"_tag":"Fail","error":{"_tag":"EnvironmentAuthorizationError","message":hostile}}]}));
    assert_eq!(
        answer(&r2, &req2, "accept").await.error_code(),
        Some(ErrorCode::SourceUnavailable)
    );
    until("degraded source", || {
        let ServerMessage::Welcome { machine, .. } = r2.core.welcome() else {
            panic!()
        };
        (machine.sources.t3.state == SourceState::Degraded).then_some(())
    })
    .await;
    let logs = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
    assert!(
        logs.contains("answer delivery uncertain"),
        "missing fixed warning: {logs}"
    );
    assert!(
        logs.contains("T3 reported an internal error"),
        "missing fixed diagnostic: {logs}"
    );
    let ServerMessage::Welcome { machine, .. } = r.core.welcome() else {
        panic!()
    };
    let status = machine.sources.t3.message.unwrap_or_default();
    let ServerMessage::Welcome {
        machine: send_machine,
        ..
    } = r_send.core.welcome()
    else {
        panic!()
    };
    let send_status = send_machine.sources.t3.message.unwrap_or_default();
    let ServerMessage::Welcome {
        machine: machine2, ..
    } = r2.core.welcome()
    else {
        panic!()
    };
    let status2 = machine2.sources.t3.message.unwrap_or_default();
    for bad in [
        "sk-ant-",
        "SECRETSECRET",
        "Bearer",
        "abc.def.ghi",
        "rm -rf",
        "\u{1b}",
        "\u{202E}",
    ] {
        assert!(!logs.contains(bad), "warn log leaked {bad:?}: {logs}");
        assert!(
            !status.contains(bad),
            "source status leaked {bad:?}: {status}"
        );
        assert!(
            !send_status.contains(bad),
            "send source status leaked {bad:?}: {send_status}"
        );
        assert!(
            !status2.contains(bad),
            "source status leaked {bad:?}: {status2}"
        );
    }
    assert!(status.is_empty(), "unexpected source status: {status}");
    assert!(
        send_status.is_empty(),
        "unexpected send source status: {send_status}"
    );
    assert_eq!(
        status2,
        "the T3 token lacks the orchestration:operate scope"
    );
}

#[tokio::test]
async fn busy_mailbox_still_updates_answer_state_and_degrades_on_scope_failure() {
    let (r, req) = with_pending(approval_projection("a", "rq-1", None), "command").await;
    let (_, mut rx) = r.core.attach(None);
    let shared = r.adapter.shared_for_tests();
    // Fill the coalescing mailbox with unrelated hints before the answer arrives.
    shared.mailbox.fill_for_tests(10_000);
    assert_eq!(
        answer(&r, &req, "accept").await.outcome,
        Some(Outcome::Delivered)
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let ServerMessage::RequestResolved { request_id, by, .. } = rx.recv().await.unwrap()
                && request_id == req.id
            {
                assert_eq!(by, ResolvedBy::Inbox);
                break;
            }
        }
    })
    .await
    .expect("timed out waiting for inbox resolution");
    until("answered agent state", || {
        (r.core.agent("t3:a")?.state == AgentState::Working).then_some(())
    })
    .await;
    // Keep the shell view stale so the mailbox hint must suppress reopening.
    r.fake
        .put_thread(shell_thread("a", "running", Some(("rq-1", "command"))));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        r.core.agent("t3:a").unwrap().pending.is_none(),
        "answered request reopened from stale shell state"
    );

    r.fake
        .set_projection("b", approval_projection("b", "rq-2", None));
    r.fake
        .put_thread(shell_thread("b", "running", Some(("rq-2", "command"))));
    let req2 = until("second request", || r.core.agent("t3:b")?.pending).await;
    r.fake.deny_operate(true);
    assert_eq!(
        answer_with_id(&r, &req2, "accept", "x2").await.error_code(),
        Some(ErrorCode::SourceUnavailable)
    );
    until("degraded source", || {
        let ServerMessage::Welcome { machine, .. } = r.core.welcome() else {
            panic!()
        };
        (machine.sources.t3.state == SourceState::Degraded).then_some(())
    })
    .await;
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
