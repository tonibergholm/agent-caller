use acd_adapters::herdr::{
    client::Herdr,
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
    let handle = spawn(herdr, core.clone(), panes.clone(), timing);
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
async fn status_change_before_resubscribe_is_caught_without_a_list() {
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
    // Let the connect-time resync list finish so a later list would be a bug.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let lists = || {
        r.fake
            .calls()
            .iter()
            .filter(|(m, _)| m == "agent.list")
            .count()
    };
    let lists_before = lists();
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
    assert_eq!(lists(), lists_before, "found by agent.get, not a list");
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
