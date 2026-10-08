use acd_adapters::herdr::{
    client::Herdr,
    discovery::Discovery,
    fake::{FakeHerdr, fake_agent},
    map,
    reconciler::*,
};
use acd_core::{Core, testing::test_machine};
use acd_protocol::*;
use std::{sync::Arc, time::Duration};

fn fast() -> Timing {
    Timing {
        list_idle: Duration::from_millis(300),
        list_blocked: Duration::from_millis(100),
        backoff_max: Duration::from_millis(200),
        fallback_after: Duration::from_millis(200),
        poll: Duration::from_millis(50),
    }
}

async fn until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    for _ in 0..200 {
        if let Some(v) = f() {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}

struct Rig {
    fake: FakeHerdr,
    core: Arc<Core>,
    panes: Arc<PaneMap>,
    handle: ReconcilerHandle,
    hash: String,
}

async fn rig(fake: FakeHerdr) -> Rig {
    rig_with(fake, fast()).await
}

async fn rig_with(fake: FakeHerdr, timing: Timing) -> Rig {
    let core = Core::new(test_machine(), vec![]);
    let panes = Arc::new(PaneMap::default());
    let herdr = Herdr::new(fake.path().to_path_buf());
    let hash = map::socket_hash(fake.path());
    let handle = spawn(
        herdr,
        core.clone(),
        panes.clone(),
        timing,
        Default::default(),
    );
    Rig {
        fake,
        core,
        panes,
        handle,
        hash,
    }
}

impl Rig {
    fn id(&self, pane: &str) -> String {
        map::agent_id(&self.hash, pane)
    }
    fn state(&self, pane: &str) -> Option<AgentState> {
        self.core.agent(&self.id(pane)).map(|a| a.state)
    }
}

#[tokio::test]
async fn initial_list_builds_agents_and_ignores_plain_shells() {
    let fake = FakeHerdr::start().await;
    fake.set_agent(fake_agent("w1:p1", "claude", "idle"));
    fake.set_agent(fake_agent("w1:p2", "codex", "working"));
    let r = rig(fake).await;
    until("two agents", || {
        (r.state("w1:p1") == Some(AgentState::Idle)
            && r.state("w1:p2") == Some(AgentState::Working))
        .then_some(())
    })
    .await;
    assert_eq!(r.panes.read().unwrap().len(), 2);
    assert_eq!(*r.handle.health().borrow(), Health::Connected);
}

#[tokio::test]
async fn status_event_triggers_get_and_update() {
    let fake = FakeHerdr::start().await;
    fake.set_agent(fake_agent("w1:p1", "claude", "working"));
    let r = rig(fake).await;
    until("agent", || r.state("w1:p1")).await;
    r.fake.set_status("w1:p1", "blocked");
    until("needs_you", || {
        (r.state("w1:p1") == Some(AgentState::NeedsYou)).then_some(())
    })
    .await;
}

#[tokio::test]
async fn new_pane_is_subscribed_and_released_agent_removed() {
    let fake = FakeHerdr::start().await;
    let r = rig(fake).await;
    until("connected", || {
        (*r.handle.health().borrow() == Health::Connected).then_some(())
    })
    .await;
    r.fake.set_agent(fake_agent("w1:p5", "claude", "idle"));
    r.fake.push_event(serde_json::json!({"event":"pane_agent_detected","data":{"agent":"claude","pane_id":"w1:p5","type":"pane_agent_detected","workspace_id":"w1"}}));
    until("p5", || r.state("w1:p5")).await;
    until("resubscribed", || {
        r.fake
            .subscriptions()
            .last()
            .is_some_and(|s| s.contains(&"pane.agent_status_changed:w1:p5".to_string()))
            .then_some(())
    })
    .await;
    assert_eq!(*r.handle.health().borrow(), Health::Connected);
    r.fake.remove_agent("w1:p5");
    r.fake.push_event(serde_json::json!({"event":"pane_agent_detected","data":{"agent":"claude","final_status":"idle","pane_id":"w1:p5","released":true,"type":"pane_agent_detected","workspace_id":"w1"}}));
    until("p5 gone", || {
        r.core.agent(&r.id("w1:p5")).is_none().then_some(())
    })
    .await;
    assert!(r.panes.read().unwrap().get(&r.id("w1:p5")).is_none());
}

#[tokio::test]
async fn closed_then_reused_pane_is_a_fresh_agent() {
    let fake = FakeHerdr::start().await;
    fake.set_agent(fake_agent("w1:p1", "claude", "done"));
    let r = rig(fake).await;
    until("agent", || r.state("w1:p1")).await;
    r.core
        .handle(
            "c",
            ClientMessage::AgentMarkSeen {
                id: "s1".into(),
                agent_id: r.id("w1:p1"),
            },
        )
        .await;
    assert_eq!(r.state("w1:p1"), Some(AgentState::Idle));
    r.fake.remove_agent("w1:p1");
    r.fake.push_event(serde_json::json!({"event":"pane_closed","data":{"pane_id":"w1:p1","type":"pane_closed","workspace_id":"w1"}}));
    until("gone", || {
        r.core.agent(&r.id("w1:p1")).is_none().then_some(())
    })
    .await;
    r.fake.set_agent(fake_agent("w1:p1", "codex", "done"));
    r.fake.push_event(serde_json::json!({"event":"pane_agent_detected","data":{"agent":"codex","pane_id":"w1:p1","type":"pane_agent_detected","workspace_id":"w1"}}));
    let a = until("re-added", || r.core.agent(&r.id("w1:p1"))).await;
    assert_eq!(
        (a.kind, a.state),
        (AgentKind::Codex, AgentState::Done),
        "the old seen flag must not carry over"
    );
    assert!(a.pending.is_none());
}

#[tokio::test]
async fn missed_event_is_caught_by_periodic_list() {
    let fake = FakeHerdr::start().await;
    fake.set_agent(fake_agent("w1:p1", "claude", "working"));
    let r = rig(fake).await;
    until("agent", || r.state("w1:p1")).await;
    r.fake.set_agent(fake_agent("w1:p1", "claude", "idle")); // no event pushed
    until("idle via list", || {
        (r.state("w1:p1") == Some(AgentState::Idle)).then_some(())
    })
    .await;
}

#[tokio::test]
async fn connection_loss_marks_agents_unknown_then_resyncs() {
    let fake = FakeHerdr::start().await;
    fake.set_agent(fake_agent("w1:p1", "claude", "working"));
    let path = fake.path().to_path_buf();
    let r = rig(fake).await;
    until("agent", || r.state("w1:p1")).await;
    let Rig {
        fake,
        core,
        handle,
        hash,
        ..
    } = r;
    fake.stop().await;
    let id = map::agent_id(&hash, "w1:p1");
    until("unknown", || {
        (core.agent(&id).map(|a| a.state) == Some(AgentState::Unknown)).then_some(())
    })
    .await;
    assert!(matches!(*handle.health().borrow(), Health::Down(_)));
    let fake2 = FakeHerdr::start_at(&path).await;
    fake2.set_agent(fake_agent("w1:p1", "claude", "blocked"));
    until("resynced", || {
        (core.agent(&id).map(|a| a.state) == Some(AgentState::NeedsYou)).then_some(())
    })
    .await;
    assert_eq!(*handle.health().borrow(), Health::Connected);
}

#[tokio::test]
async fn incompatible_version_degrades_and_shows_nothing() {
    let fake = FakeHerdr::start_with_version("0.10.0", 23).await;
    fake.set_agent(fake_agent("w1:p1", "claude", "idle"));
    let r = rig(fake).await;
    until("incompatible", || {
        matches!(*r.handle.health().borrow(), Health::Incompatible(_)).then_some(())
    })
    .await;
    assert!(r.core.agent(&r.id("w1:p1")).is_none());
}

#[tokio::test]
async fn garbage_event_triggers_resync_not_crash() {
    let fake = FakeHerdr::start().await;
    fake.set_agent(fake_agent("w1:p1", "claude", "idle"));
    let hour = Duration::from_secs(3600);
    let r = rig_with(
        fake,
        Timing {
            list_idle: hour,
            list_blocked: hour,
            ..fast()
        },
    )
    .await;
    until("agent", || r.state("w1:p1")).await;
    let lists_before = r
        .fake
        .calls()
        .iter()
        .filter(|(m, _)| m == "agent.list")
        .count();
    r.fake.push_raw("{{{ not json");
    r.fake
        .push_event(serde_json::json!({"event":"pane_exploded","data":{}}));
    until("resync list", || {
        (r.fake
            .calls()
            .iter()
            .filter(|(m, _)| m == "agent.list")
            .count()
            > lists_before)
            .then_some(())
    })
    .await;
    assert_eq!(r.state("w1:p1"), Some(AgentState::Idle));
    assert_eq!(*r.handle.health().borrow(), Health::Connected);
}

#[tokio::test]
async fn state_calls_are_serial() {
    let fake = FakeHerdr::start().await;
    for i in 0..5 {
        fake.set_agent(fake_agent(&format!("w1:p{i}"), "claude", "working"));
    }
    let r = rig(fake).await;
    until("agents", || r.state("w1:p4")).await;
    for i in 0..5 {
        r.fake.set_status(&format!("w1:p{i}"), "blocked");
    }
    until("all blocked", || {
        (0..5)
            .all(|i| r.state(&format!("w1:p{i}")) == Some(AgentState::NeedsYou))
            .then_some(())
    })
    .await;
    assert_eq!(
        r.fake.max_in_flight(),
        1,
        "agent.list/get/read must never overlap"
    );
}

#[tokio::test]
async fn reconnect_with_unchanged_state_restores_it() {
    let fake = FakeHerdr::start().await;
    fake.set_agent(fake_agent("w1:p1", "claude", "working"));
    let path = fake.path().to_path_buf();
    let r = rig(fake).await;
    until("agent", || r.state("w1:p1")).await;
    let Rig {
        fake,
        core,
        hash,
        handle: _handle,
        ..
    } = r;
    fake.stop().await;
    let id = map::agent_id(&hash, "w1:p1");
    until("unknown", || {
        (core.agent(&id).map(|a| a.state) == Some(AgentState::Unknown)).then_some(())
    })
    .await;
    let fake2 = FakeHerdr::start_at(&path).await;
    fake2.set_agent(fake_agent("w1:p1", "claude", "working")); // exactly as before the loss
    until("restored", || {
        (core.agent(&id).map(|a| a.state) == Some(AgentState::Working)).then_some(())
    })
    .await;
}

#[tokio::test]
async fn busy_event_stream_does_not_starve_lists() {
    let fake = FakeHerdr::start().await;
    fake.set_agent(fake_agent("w1:p1", "claude", "working"));
    fake.set_agent(fake_agent("w1:p2", "claude", "working"));
    let r = rig(fake).await;
    until("agents", || r.state("w1:p2")).await;
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let pusher = {
        let f = r.fake.clone_handle();
        let stop = stop.clone();
        tokio::spawn(async move {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                f.push_event(serde_json::json!({"event":"pane.agent_status_changed","data":{"agent":"claude","agent_status":"working","pane_id":"w1:p1","workspace_id":"w1"}}));
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
    };
    r.fake.set_agent(fake_agent("w1:p2", "claude", "idle")); // no event: only a list can find it
    until("list still runs under load", || {
        (r.state("w1:p2") == Some(AgentState::Idle)).then_some(())
    })
    .await;
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    pusher.await.unwrap();
}

#[tokio::test]
async fn event_burst_is_coalesced_and_stop_stays_prompt() {
    let fake = FakeHerdr::start().await;
    fake.set_agent(fake_agent("w1:p1", "claude", "working"));
    let r = rig(fake).await;
    until("agent", || r.state("w1:p1")).await;
    let gets_before = r
        .fake
        .calls()
        .iter()
        .filter(|(m, _)| m == "agent.get")
        .count();
    for _ in 0..200 {
        r.fake.push_event(serde_json::json!({"event":"pane.agent_status_changed","data":{"agent":"claude","agent_status":"working","pane_id":"w1:p1","workspace_id":"w1"}}));
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let gets = r
        .fake
        .calls()
        .iter()
        .filter(|(m, _)| m == "agent.get")
        .count()
        - gets_before;
    assert!(
        gets < 50,
        "200 events for one pane must coalesce, got {gets} gets"
    );
    let t = std::time::Instant::now();
    r.handle.shutdown(false).await;
    assert!(
        t.elapsed() < Duration::from_secs(4),
        "stop is handled within one call deadline"
    );
}

#[tokio::test]
async fn shutdown_with_removal_emits_gone() {
    let fake = FakeHerdr::start().await;
    fake.set_agent(fake_agent("w1:p1", "claude", "idle"));
    let r = rig(fake).await;
    until("agent", || r.state("w1:p1")).await;
    let id = r.id("w1:p1");
    r.handle.shutdown(true).await;
    assert!(r.core.agent(&id).is_none());
    assert!(r.panes.read().unwrap().is_empty());
}

#[tokio::test]
async fn status_change_before_resubscribe_is_caught() {
    let fake = FakeHerdr::start().await;
    let hour = Duration::from_secs(3600);
    let r = rig_with(
        fake,
        Timing {
            list_idle: hour,
            list_blocked: hour,
            ..fast()
        },
    )
    .await;
    until("connected", || {
        (*r.handle.health().borrow() == Health::Connected).then_some(())
    })
    .await;
    r.fake.set_agent(fake_agent("w1:p5", "claude", "working"));
    r.fake.push_event(serde_json::json!({"event":"pane_agent_detected","data":{"agent":"claude","pane_id":"w1:p5","type":"pane_agent_detected","workspace_id":"w1"}}));
    // Spin (no sleeping) until the first get is applied: the reconciler is
    // then still opening the new subscription. Change status with no event.
    for _ in 0..100_000 {
        if r.state("w1:p5").is_some() {
            break;
        }
        tokio::task::yield_now().await;
    }
    r.fake.set_agent(fake_agent("w1:p5", "claude", "blocked"));
    until("needs_you", || {
        (r.state("w1:p5") == Some(AgentState::NeedsYou)).then_some(())
    })
    .await;
    // Every resubscribe also runs one full list (it would catch this too),
    // but the new pane is still fetched with its own agent.get.
    let calls = r.fake.calls();
    let subscribed = calls
        .iter()
        .rposition(|(m, _)| m == "events.subscribe")
        .unwrap();
    assert!(
        calls[subscribed..]
            .iter()
            .any(|(m, p)| m == "agent.get" && p["target"] == "w1:p5"),
        "the new pane gets an agent.get after its subscription"
    );
}

#[tokio::test]
async fn stream_loss_during_a_resubscribe_still_resyncs_existing_panes() {
    let fake = FakeHerdr::start().await;
    fake.set_agent(fake_agent("w1:p1", "claude", "working"));
    let hour = Duration::from_secs(3600);
    let r = rig_with(
        fake,
        Timing {
            list_idle: hour,
            list_blocked: hour,
            ..fast()
        },
    )
    .await;
    until("connected", || {
        (*r.handle.health().borrow() == Health::Connected
            && r.state("w1:p1") == Some(AgentState::Working))
        .then_some(())
    })
    .await;
    // Let the connect-time resync list finish.
    tokio::time::sleep(Duration::from_millis(100)).await;
    // A new pane makes the reconciler resubscribe; hold that subscribe.
    let gate = r.fake.block_next("events.subscribe");
    r.fake.set_agent(fake_agent("w1:p5", "claude", "idle"));
    r.fake.push_event(serde_json::json!({"event":"pane_agent_detected","data":{"agent":"claude","pane_id":"w1:p5","type":"pane_agent_detected","workspace_id":"w1"}}));
    gate.arrived().await;
    // The old stream ends while the new one is not yet open, and an existing
    // pane changes in that gap with no event anyone could see.
    r.fake.drop_subscribers();
    tokio::time::sleep(Duration::from_millis(100)).await;
    r.fake.set_agent(fake_agent("w1:p1", "claude", "blocked"));
    gate.release();
    until("needs_you without a periodic list", || {
        (r.state("w1:p1") == Some(AgentState::NeedsYou)).then_some(())
    })
    .await;
}

#[tokio::test]
async fn failed_connect_list_is_retried_promptly() {
    let fake = FakeHerdr::start().await;
    fake.set_agent(fake_agent("w1:p1", "claude", "working"));
    let path = fake.path().to_path_buf();
    let hour = Duration::from_secs(3600);
    let r = rig_with(
        fake,
        Timing {
            list_idle: hour,
            list_blocked: hour,
            ..fast()
        },
    )
    .await;
    until("agent", || r.state("w1:p1")).await;
    let Rig {
        fake,
        core,
        hash,
        handle,
        ..
    } = r;
    fake.stop().await;
    let id = map::agent_id(&hash, "w1:p1");
    until("unknown", || {
        (core.agent(&id).map(|a| a.state) == Some(AgentState::Unknown)).then_some(())
    })
    .await;
    let fake2 = FakeHerdr::start_at(&path).await;
    fake2.set_agent(fake_agent("w1:p1", "claude", "blocked"));
    fake2.fail_next("agent.list", "internal_error");
    until("restored", || {
        (core.agent(&id).map(|a| a.state) == Some(AgentState::NeedsYou)).then_some(())
    })
    .await;
    assert_eq!(*handle.health().borrow(), Health::Connected);
}

fn screen(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../../tests/fixtures/herdr/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

#[tokio::test]
async fn blocked_pane_gets_a_read_only_request_then_terminal() {
    let fake = FakeHerdr::start().await;
    fake.set_agent(fake_agent("w1:p1", "other-agent", "blocked"));
    fake.set_screen(
        "w1:p1",
        "detection",
        &screen("codex-trust-dialog-detection.txt"),
    );
    let r = rig(fake).await;
    let req = until("fallback", || {
        r.core.agent(&r.id("w1:p1")).and_then(|a| a.pending)
    })
    .await;
    assert_eq!(
        (req.kind, req.answer_path),
        (RequestKind::BlockedUnparsed, AnswerPath::None)
    );
    assert!(!req.is_answerable());
    assert!(req.raw_screen.as_ref().unwrap().lines().count() <= 40);
    let res = r
        .core
        .handle(
            "c",
            ClientMessage::RequestAnswer {
                id: "a1".into(),
                request_id: req.id.clone(),
                option_id: Some("x".into()),
                text: None,
            },
        )
        .await;
    assert_eq!(res.error_code(), Some(ErrorCode::NotAnswerable));
    // Answered in the terminal.
    let (_, mut rx) = r.core.attach(None);
    r.fake
        .set_screen("w1:p1", "detection", &screen("codex-idle-detection.txt"));
    r.fake.set_status("w1:p1", "idle");
    until("cleared", || {
        r.core
            .agent(&r.id("w1:p1"))
            .is_some_and(|a| a.pending.is_none())
            .then_some(())
    })
    .await;
    assert_eq!(resolution(&mut rx).await, (req.id, ResolvedBy::Terminal));
}

/// The first `request.resolved` on `rx`: `(request id, by)`.
async fn resolution(
    rx: &mut tokio::sync::broadcast::Receiver<ServerMessage>,
) -> (String, ResolvedBy) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match rx.recv().await {
                Ok(ServerMessage::RequestResolved { request_id, by, .. }) => {
                    return (request_id, by);
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(e) => panic!("event stream ended: {e}"),
            }
        }
    })
    .await
    .expect("no request.resolved within 5 s")
}

/// The Claude permission dialog after a redraw of a transcript line above it.
fn redrawn_dialog() -> String {
    let s = screen("claude-permission-bash.txt");
    let r = s.replace("⏺ Creating empty file", "⏺ Creating an empty file");
    assert_ne!(s, r);
    r
}

/// The Claude permission dialog as a narrower pane draws it: shorter rules,
/// the transcript prompt wrapped, and either the command (`"command"`) or
/// option 2's label (`"label"`) wrapped onto a continuation line.
fn rewrapped_dialog(what: &str) -> String {
    let narrow: String = screen("claude-permission-bash.txt")
        .lines()
        .map(|l| {
            let t = l.trim();
            let rule = t.chars().count() >= 10 && t.chars().all(|c| c == '─' || c == '╌');
            let l = if rule {
                t.chars().take(50).collect()
            } else {
                l.to_string()
            };
            l + "\n"
        })
        .collect();
    let narrow = narrow.replace("nothing else: touch", "nothing else:\n  touch");
    let out = match what {
        "command" => narrow.replace(
            "\n touch /tmp/acp0/t1.txt\n",
            "\n touch\n /tmp/acp0/t1.txt\n",
        ),
        "label" => narrow.replace(
            "   2. Yes, and always allow access to /tmp/acp0 from this project\n",
            "   2. Yes, and always allow access to /tmp/acp0 from\n      this project\n",
        ),
        _ => unreachable!("{what}"),
    };
    assert_ne!(out, narrow, "{what}");
    out
}

#[tokio::test]
async fn rewrap_or_redraw_keeps_the_read_only_request_open() {
    let fake = FakeHerdr::start().await;
    fake.set_agent(fake_agent("w1:p1", "claude", "blocked"));
    fake.set_screen("w1:p1", "detection", &screen("claude-permission-bash.txt"));
    let r = rig(fake).await;
    let req = until("fallback", || {
        r.core.agent(&r.id("w1:p1")).and_then(|a| a.pending)
    })
    .await;
    let reads = || {
        r.fake
            .calls()
            .iter()
            .filter(|(m, p)| m == "agent.read" && p["source"] == "detection")
            .count()
    };
    for (what, text) in [
        ("redraw", redrawn_dialog()),
        ("command", rewrapped_dialog("command")),
        ("label", rewrapped_dialog("label")),
    ] {
        r.fake.set_screen("w1:p1", "detection", &text);
        let before = reads();
        until("several reads", || (reads() >= before + 4).then_some(())).await;
        let pending = r.core.agent(&r.id("w1:p1")).and_then(|a| a.pending);
        assert_eq!(pending.map(|p| p.id), Some(req.id.clone()), "{what}");
        assert!(r.core.request(&req.id).is_some(), "{what}: not resolved");
    }
}

#[tokio::test]
async fn structured_request_suppresses_the_fallback() {
    let fake = FakeHerdr::start().await;
    fake.set_agent(fake_agent("w1:p1", "claude", "working"));
    fake.set_screen("w1:p1", "detection", &screen("claude-permission-bash.txt"));
    let r = rig(fake).await;
    until("agent", || r.state("w1:p1")).await;
    let mut obs = acd_core::testing::permission_obs(&r.id("w1:p1"), "hook-key");
    obs.answer_path = AnswerPath::Hook;
    r.core.observe(acd_core::Observation::RequestSeen(obs));
    r.fake.set_status("w1:p1", "blocked");
    // Wait for evidence that the fallback ran (several detection reads), not for a fixed time.
    until("fallback reads", || {
        (r.fake
            .calls()
            .iter()
            .filter(|(m, p)| m == "agent.read" && p["source"] == "detection")
            .count()
            >= 3)
            .then_some(())
    })
    .await;
    let p = r.core.agent(&r.id("w1:p1")).unwrap().pending.unwrap();
    assert_eq!(
        p.kind,
        RequestKind::Permission,
        "no blocked_unparsed may replace a structured request"
    );
}

#[tokio::test]
async fn upgraded_fallback_is_not_resolved_terminal_by_the_fallback() {
    let fake = FakeHerdr::start().await;
    fake.set_agent(fake_agent("w1:p1", "claude", "blocked"));
    let dialog = screen("claude-permission-bash.txt");
    fake.set_screen("w1:p1", "detection", &dialog);
    let r = rig(fake).await;
    let fb = until("fallback", || {
        r.core.agent(&r.id("w1:p1")).and_then(|a| a.pending)
    })
    .await;
    // P3 upgrades it in place (same id).
    let fp = acd_adapters::herdr::screen::fallback_fingerprint(&dialog);
    let mut up = acd_core::testing::permission_obs(&r.id("w1:p1"), "hook-key");
    up.upgrade_from = Some(acd_adapters::herdr::fallback::key(&r.hash, "w1:p1", &fp));
    r.core.observe(acd_core::Observation::RequestSeen(up));
    assert_eq!(
        r.core.request(&fb.id).unwrap().kind,
        RequestKind::Permission
    );
    // The dialog closes (as after an inbox answer); the fallback must leave attribution alone.
    let reads = || {
        r.fake
            .calls()
            .iter()
            .filter(|(m, p)| m == "agent.read" && p["source"] == "detection")
            .count()
    };
    let before = reads();
    r.fake
        .set_screen("w1:p1", "detection", &screen("claude-idle-detection.txt"));
    r.fake.set_status("w1:p1", "working");
    // Two absent reads end the fallback and polling stops, so a third read
    // never comes: wait for two, then check that reads have stopped.
    until("two reads after the change", || {
        (reads() >= before + 2).then_some(())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let settled = reads();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(reads(), settled, "polling stops once the fallback ended");
    // The task is serial: an agent.get from here runs after FallbackEnded.
    let gets = || {
        r.fake
            .calls()
            .iter()
            .filter(|(m, _)| m == "agent.get")
            .count()
    };
    let g = gets();
    r.handle
        .hints()
        .send(Hint::Pane("w1:p1".into()))
        .await
        .unwrap();
    until("a later turn", || (gets() > g).then_some(())).await;
    assert!(
        r.core.request(&fb.id).is_some(),
        "only the structured request's owner may resolve it"
    );
}

#[tokio::test]
async fn restart_with_a_blocked_pane_reopens_the_request() {
    // Spec §12 / §14.8: after an acd restart, requests still on screen reappear.
    let fake = FakeHerdr::start().await;
    fake.set_agent(fake_agent("w1:p1", "claude", "blocked"));
    fake.set_screen("w1:p1", "detection", &screen("claude-permission-bash.txt"));
    let r = rig(fake).await; // a fresh Core is a restarted acd
    let req = until("reopened", || {
        r.core.agent(&r.id("w1:p1")).and_then(|a| a.pending)
    })
    .await;
    assert_eq!(req.kind, RequestKind::BlockedUnparsed);
}

#[tokio::test]
async fn discovery_finds_sessions_and_reports_status() {
    let home = tempfile::Builder::new()
        .prefix("acd")
        .tempdir_in("/tmp")
        .unwrap(); // short: macOS socket paths max 104 bytes
    let sock = home.path().join(".config/herdr/sessions/x/herdr.sock");
    std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
    let fake = FakeHerdr::start_at(&sock).await;
    fake.set_agent(fake_agent("w1:p1", "claude", "idle"));
    let core = Core::new(test_machine(), vec![]);
    let panes = Arc::new(PaneMap::default());
    let d = Discovery {
        env_socket: None,
        home: home.path().into(),
        extra: vec![],
        interval: Duration::from_millis(100),
        timing: fast(),
        keys: Default::default(),
    };
    let _task = acd_adapters::herdr::discovery::spawn(d, core.clone(), panes.clone());
    until("agent", || (panes.read().unwrap().len() == 1).then_some(())).await;
    until("ok", || matches!(core.welcome(), ServerMessage::Welcome { ref machine, .. } if machine.sources.herdr.state == SourceState::Ok).then_some(())).await;
}

#[tokio::test]
async fn socket_missing_twice_removes_its_agents() {
    let home = tempfile::Builder::new()
        .prefix("acd")
        .tempdir_in("/tmp")
        .unwrap(); // short: macOS socket paths max 104 bytes
    let sock = home.path().join(".config/herdr/sessions/x/herdr.sock");
    std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
    let fake = FakeHerdr::start_at(&sock).await;
    fake.set_agent(fake_agent("w1:p1", "claude", "idle"));
    let core = Core::new(test_machine(), vec![]);
    let panes = Arc::new(PaneMap::default());
    let d = Discovery {
        env_socket: None,
        home: home.path().into(),
        extra: vec![],
        interval: Duration::from_millis(150),
        timing: fast(),
        keys: Default::default(),
    };
    let _task = acd_adapters::herdr::discovery::spawn(d, core.clone(), panes.clone());
    let id = until("agent", || panes.read().unwrap().keys().next().cloned()).await;
    fake.stop().await;
    until("unknown first", || {
        (core.agent(&id).map(|a| a.state) == Some(AgentState::Unknown)).then_some(())
    })
    .await;
    until("removed", || core.agent(&id).is_none().then_some(())).await;
    until("unconfigured", || matches!(core.welcome(), ServerMessage::Welcome { ref machine, .. } if machine.sources.herdr.state == SourceState::Unconfigured).then_some(())).await;
}
