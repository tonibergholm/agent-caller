use acd_adapters::t3::{conn::*, fake::*, reconciler::*};
use acd_core::{
    Core,
    testing::{FakeAdapter, test_machine},
};
use acd_protocol::*;
use serde_json::json;
use std::{os::unix::fs::PermissionsExt, sync::Arc, time::Duration};

/// Up to 10 s: the discovery tests walk 100 threads one at a time.
async fn until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    for _ in 0..500 {
        if let Some(v) = f() {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}

pub struct Rig {
    pub fake: FakeT3,
    pub core: Arc<Core>,
    pub shared: Arc<Shared>,
    pub sync: SyncHandle,
    pub _dir: tempfile::TempDir,
}

pub fn fast_timing() -> T3Timing {
    T3Timing {
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
        learn_window: Duration::from_secs(86_400),
        relearn_after: Duration::from_millis(200),
        sync_timeout: Duration::from_millis(500),
        degraded_for: Duration::from_secs(600),
    }
}

pub async fn rig_with(fake: FakeT3, adapter: Option<Arc<FakeAdapter>>, timing: T3Timing) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let token = dir.path().join("t3.token");
    std::fs::write(&token, FAKE_TOKEN).unwrap();
    std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
    let adapters: Vec<Arc<dyn acd_core::Adapter>> = adapter
        .into_iter()
        .map(|a| a as Arc<dyn acd_core::Adapter>)
        .collect();
    let core = Core::new(test_machine(), adapters);
    let shared = Arc::new(Shared::default());
    let sync = spawn(
        SyncConfig {
            ws_url: ws_url(&fake.url()),
            token_path: token,
            timing,
        },
        core.clone(),
        shared.clone(),
    );
    Rig {
        fake,
        core,
        shared,
        sync,
        _dir: dir,
    }
}
pub async fn rig(fake: FakeT3) -> Rig {
    rig_with(fake, None, fast_timing()).await
}

fn t3_state(r: &Rig) -> SourceState {
    let ServerMessage::Welcome { machine, .. } = r.core.welcome() else {
        panic!()
    };
    machine.sources.t3.state
}
fn agent(r: &Rig, thread: &str) -> Option<Agent> {
    r.core.agent(&format!("t3:{thread}"))
}

#[tokio::test]
async fn threads_appear_with_mapped_state() {
    let fake = FakeT3::start().await;
    fake.put_thread(shell_thread("a", "idle", None));
    fake.put_thread(shell_thread("b", "completed", None));
    fake.put_thread(shell_thread("c", "waiting", None));
    let r = rig(fake).await;
    until("ok", || (t3_state(&r) == SourceState::Ok).then_some(())).await;
    assert_eq!(agent(&r, "a").unwrap().state, AgentState::Idle);
    assert_eq!(agent(&r, "b").unwrap().state, AgentState::Done);
    assert_eq!(
        agent(&r, "c").unwrap().state,
        AgentState::Working,
        "waiting without a request is working"
    );
    assert_eq!(agent(&r, "a").unwrap().source, Source::T3);
    r.fake.put_thread(shell_thread("a", "running", None));
    until("a working", || {
        (agent(&r, "a")?.state == AgentState::Working).then_some(())
    })
    .await;
}

#[tokio::test]
async fn project_only_snapshot_keeps_threads_and_cursor() {
    let fake = FakeT3::start().await;
    fake.put_thread(shell_thread("a", "idle", None));
    let r = rig(fake).await;
    until("a", || agent(&r, "a")).await;
    r.fake.push_project_only_snapshot();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        agent(&r, "a").is_some(),
        "a project-only snapshot never removes threads"
    );
    let head = r.fake.head();
    r.fake.drop_connections();
    until("unknown", || {
        (agent(&r, "a")?.state == AgentState::Unknown).then_some(())
    })
    .await;
    // While disconnected, one change at head + 1. The resume replies with a
    // project-only snapshot whose snapshotSequence is head + 1, then that
    // event. Moving the cursor on the snapshot would drop the event.
    r.fake.put_thread(shell_thread("a", "completed", None));
    until("done", || {
        (agent(&r, "a")?.state == AgentState::Done).then_some(())
    })
    .await;
    let resumes: Vec<_> = r
        .fake
        .subscriptions()
        .into_iter()
        .filter(|(tag, p)| {
            tag == "orchestration.subscribeShell" && p.get("afterSequence").is_some()
        })
        .collect();
    assert_eq!(resumes[0].1["afterSequence"], json!(head));
}

#[tokio::test]
async fn connection_loss_marks_agents_unknown_then_resyncs() {
    let fake = FakeT3::start().await;
    fake.put_thread(shell_thread("a", "completed", None));
    let r = rig(fake).await;
    until("done", || {
        (agent(&r, "a")?.state == AgentState::Done).then_some(())
    })
    .await;
    r.fake.drop_connections();
    until("unknown", || {
        (agent(&r, "a")?.state == AgentState::Unknown).then_some(())
    })
    .await;
    assert_eq!(t3_state(&r), SourceState::Degraded);
    until("restored", || {
        (agent(&r, "a")?.state == AgentState::Done && t3_state(&r) == SourceState::Ok).then_some(())
    })
    .await;
}

#[tokio::test]
async fn full_snapshot_on_resume_removes_vanished_threads() {
    let fake = FakeT3::start().await;
    fake.put_thread(shell_thread("a", "idle", None));
    fake.put_thread(shell_thread("b", "idle", None));
    let r = rig(fake).await;
    until("b", || agent(&r, "b")).await;
    r.fake.drop_connections();
    r.fake.remove_thread("b");
    r.fake.compact();
    until("b gone", || agent(&r, "b").is_none().then_some(())).await;
    assert!(agent(&r, "a").is_some());
}

#[tokio::test]
async fn removed_and_archived_threads_are_gone() {
    let fake = FakeT3::start().await;
    fake.put_thread(shell_thread("a", "idle", None));
    fake.put_thread(shell_thread("b", "idle", None));
    let r = rig(fake).await;
    until("both", || agent(&r, "b").and(agent(&r, "a"))).await;
    r.fake.remove_thread("a");
    let mut b = shell_thread("b", "idle", None);
    b["archivedAt"] = json!("2026-10-08T07:00:00.000Z");
    r.fake.put_thread(b);
    until("gone", || {
        (agent(&r, "a").is_none() && agent(&r, "b").is_none()).then_some(())
    })
    .await;
}

#[tokio::test]
async fn bad_thread_entry_degrades_only_that_agent() {
    let fake = FakeT3::start().await;
    fake.put_thread(shell_thread("a", "idle", None));
    fake.put_thread(shell_thread("b", "idle", None));
    let r = rig(fake).await;
    until("b", || agent(&r, "b")).await;
    let mut bad = shell_thread("b", "idle", None);
    bad["status"] = json!(42);
    r.fake.put_thread(bad);
    until("b unknown", || {
        (agent(&r, "b")?.state == AgentState::Unknown).then_some(())
    })
    .await;
    assert_eq!(agent(&r, "a").unwrap().state, AgentState::Idle);
    assert_eq!(t3_state(&r), SourceState::Degraded);
}

#[tokio::test]
async fn session_ids_are_published_and_removed() {
    let fake = FakeT3::start().await;
    fake.put_thread(shell_thread("a", "running", None));
    fake.set_projection("a", approval_projection("a", "rq-unused", Some("sess-1")));
    let r = rig(fake).await;
    let changes = r.shared.sessions.changes();
    until("owner", || r.shared.sessions.owner("sess-1")).await;
    assert_eq!(r.shared.sessions.owner("sess-1").as_deref(), Some("t3:a"));
    r.fake.remove_thread("a");
    until("no owner", || {
        r.shared.sessions.owner("sess-1").is_none().then_some(())
    })
    .await;
    assert!(changes.has_changed().unwrap());
}

#[tokio::test]
async fn idle_recent_threads_are_learned_one_at_a_time() {
    let fake = FakeT3::start().await;
    for (t, s) in [("a", "s-a"), ("b", "s-b"), ("c", "s-c")] {
        let mut e = shell_thread(t, "idle", None);
        e["updatedAt"] = json!(chrono::Utc::now().to_rfc3339());
        fake.put_thread(e);
        fake.set_projection(t, approval_projection(t, "rq-unused", Some(s)));
    }
    let r = rig(fake).await;
    until("all learned", || {
        ["s-a", "s-b", "s-c"]
            .iter()
            .all(|s| r.shared.sessions.owner(s).is_some())
            .then_some(())
    })
    .await;
    assert_eq!(r.fake.thread_stream_peak(), 1);
    until("released", || {
        r.fake.live_thread_streams().is_empty().then_some(())
    })
    .await;
}

#[tokio::test]
async fn live_streams_are_bounded_and_released() {
    let fake = FakeT3::start().await;
    for t in ["a", "b", "c"] {
        fake.put_thread(shell_thread(t, "running", None));
        fake.set_projection(t, approval_projection(t, "rq-unused", None));
    }
    // learn_window ZERO disables only recent-thread discovery: the third
    // running thread is over the cap and may still get the discovery stream.
    let r = rig_with(
        fake,
        None,
        T3Timing {
            max_live: 2,
            learn_window: Duration::ZERO,
            ..fast_timing()
        },
    )
    .await;
    until("two working", || {
        (r.shared.stats().working_streams == 2).then_some(())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let st = r.shared.stats();
    assert!(st.peak_working_streams <= 2, "{st:?}");
    assert!(st.peak_discovery_streams <= 1, "{st:?}");
    assert_eq!(st.request_streams, 0, "{st:?}");
    assert!(
        r.fake.thread_stream_peak() <= 3,
        "two working plus one discovery"
    );
    for t in ["a", "b", "c"] {
        r.fake.put_thread(shell_thread(t, "completed", None));
    }
    until("released", || {
        r.fake.live_thread_streams().is_empty().then_some(())
    })
    .await;
}

#[tokio::test]
async fn token_problems_set_the_source_status() {
    let fake = FakeT3::start().await;
    let r = rig(fake).await;
    until("ok", || (t3_state(&r) == SourceState::Ok).then_some(())).await;
    std::fs::write(r._dir.path().join("t3.token"), "wrong").unwrap();
    r.fake.drop_connections();
    until("degraded", || {
        let ServerMessage::Welcome { machine, .. } = r.core.welcome() else {
            panic!()
        };
        (machine.sources.t3.state == SourceState::Degraded
            && machine
                .sources
                .t3
                .message
                .as_deref()
                .is_some_and(|m| m.contains("rejected the token")))
        .then_some(())
    })
    .await;
    std::fs::remove_file(r._dir.path().join("t3.token")).unwrap();
    until("unconfigured", || {
        (t3_state(&r) == SourceState::Unconfigured).then_some(())
    })
    .await;
}

fn recent(mut e: serde_json::Value) -> serde_json::Value {
    e["updatedAt"] = json!(chrono::Utc::now().to_rfc3339());
    e
}

#[tokio::test]
async fn null_session_id_is_learned_later() {
    // Sol round 1, finding 4: checked while not loaded must not mean "done".
    let fake = FakeT3::start().await;
    let mut e = recent(shell_thread("a", "idle", None));
    e["activeProviderThreadId"] = json!("pt-1");
    fake.put_thread(e.clone());
    fake.set_projection(
        "a",
        with_provider(approval_projection("a", "rq-unused", None), "pt-1", None),
    );
    let r = rig(fake).await;
    until("checked", || {
        r.fake
            .subscriptions()
            .iter()
            .any(|(t, p)| t == "orchestration.subscribeThread" && p["threadId"] == "a")
            .then_some(())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(r.shared.sessions.owner("s-late").is_none());
    // The first turn starts later: the same provider thread gets its native id.
    r.fake.set_projection(
        "a",
        with_provider(
            approval_projection("a", "rq-unused", None),
            "pt-1",
            Some("s-late"),
        ),
    );
    r.fake.put_thread(recent(e));
    until("learned", || r.shared.sessions.owner("s-late")).await;
}

#[tokio::test]
async fn provider_switch_while_idle_moves_ownership() {
    let fake = FakeT3::start().await;
    let mut e = recent(shell_thread("a", "idle", None));
    e["activeProviderThreadId"] = json!("pt-1");
    fake.put_thread(e.clone());
    fake.set_projection(
        "a",
        with_provider(
            approval_projection("a", "rq-unused", None),
            "pt-1",
            Some("s1"),
        ),
    );
    let r = rig(fake).await;
    until("s1", || r.shared.sessions.owner("s1")).await;
    e["activeProviderThreadId"] = json!("pt-2");
    r.fake.set_projection(
        "a",
        with_provider(
            approval_projection("a", "rq-unused", None),
            "pt-2",
            Some("s2"),
        ),
    );
    r.fake.put_thread(recent(e));
    until("s2", || r.shared.sessions.owner("s2")).await;
    assert!(
        r.shared.sessions.owner("s1").is_none(),
        "the old session no longer belongs to T3"
    );
}

#[tokio::test]
async fn capped_working_threads_still_get_session_ids() {
    let fake = FakeT3::start().await;
    for i in 0..5 {
        let t = format!("w{i}");
        fake.put_thread(shell_thread(&t, "running", None));
        fake.set_projection(
            &t,
            approval_projection(&t, "rq-unused", Some(&format!("s-{t}"))),
        );
    }
    let r = rig_with(
        fake,
        None,
        T3Timing {
            max_live: 2,
            ..fast_timing()
        },
    )
    .await;
    until("all five", || {
        (0..5)
            .all(|i| r.shared.sessions.owner(&format!("s-w{i}")).is_some())
            .then_some(())
    })
    .await;
    assert!(
        r.fake.thread_stream_peak() <= 3,
        "two working streams plus one discovery stream"
    );
}

fn running_on(t: &str, pt: &str) -> serde_json::Value {
    let mut e = shell_thread(t, "running", None);
    e["activeProviderThreadId"] = json!(pt);
    e
}
fn provider_event(pt: &str, native: Option<&str>) -> serde_json::Value {
    json!({"id": pt, "nativeThreadRef": native.map(|n| json!({"nativeId": n}))})
}

/// A running thread on pt-1 / s1 with a live working stream.
async fn live_on_s1() -> Rig {
    let fake = FakeT3::start().await;
    fake.put_thread(running_on("a", "pt-1"));
    fake.set_projection(
        "a",
        with_provider(
            approval_projection("a", "rq-unused", None),
            "pt-1",
            Some("s1"),
        ),
    );
    let r = rig(fake).await;
    until("s1", || r.shared.sessions.owner("s1")).await;
    r
}

#[tokio::test]
async fn shell_first_switch_clears_then_publishes_when_detail_agrees() {
    // Sol round 2, finding 1.
    let r = live_on_s1().await;
    r.fake.put_thread(running_on("a", "pt-2"));
    until("cleared", || {
        r.shared.sessions.owner("s1").is_none().then_some(())
    })
    .await;
    // An old-provider event arrives in between: it must not restore s1.
    r.fake.push_thread_event(
        "a",
        "provider-thread.updated",
        provider_event("pt-1", Some("s1")),
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        r.shared.sessions.owner("s1").is_none(),
        "detail still on pt-1 disagrees with the shell"
    );
    r.fake.push_thread_event(
        "a",
        "provider-thread.updated",
        provider_event("pt-2", Some("s2")),
    );
    r.fake.push_thread_event(
        "a",
        "thread.metadata-updated",
        json!({"activeProviderThreadId": "pt-2"}),
    );
    until("s2", || r.shared.sessions.owner("s2")).await;
    assert!(r.shared.sessions.owner("s1").is_none());
}

#[tokio::test]
async fn detail_first_switch_waits_for_the_shell() {
    let r = live_on_s1().await;
    r.fake.push_thread_event(
        "a",
        "provider-thread.updated",
        provider_event("pt-2", Some("s2")),
    );
    r.fake.push_thread_event(
        "a",
        "thread.metadata-updated",
        json!({"activeProviderThreadId": "pt-2"}),
    );
    until("cleared", || {
        r.shared.sessions.owner("s1").is_none().then_some(())
    })
    .await;
    assert!(
        r.shared.sessions.owner("s2").is_none(),
        "the shell still says pt-1"
    );
    r.fake.push_thread_event(
        "a",
        "provider-thread.updated",
        provider_event("pt-1", Some("s1")),
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        r.shared.sessions.owner("s1").is_none(),
        "an old-provider event between the two"
    );
    r.fake.put_thread(running_on("a", "pt-2"));
    until("s2", || r.shared.sessions.owner("s2")).await;
}

#[tokio::test]
async fn a_transition_to_null_clears_ownership() {
    let r = live_on_s1().await;
    r.fake.push_thread_event(
        "a",
        "thread.metadata-updated",
        json!({"activeProviderThreadId": null}),
    );
    until("cleared", || {
        r.shared.sessions.owner("s1").is_none().then_some(())
    })
    .await;
}

#[tokio::test]
async fn a_null_overflow_thread_does_not_starve_recent_discovery() {
    // Sol round 2, finding 2. Every working thread is over the cap
    // (max_live 0), and this one stays null forever.
    let fake = FakeT3::start().await;
    fake.put_thread(shell_thread("w", "running", None));
    fake.set_projection("w", approval_projection("w", "rq-unused", None));
    fake.put_thread(recent(shell_thread("i", "idle", None)));
    fake.set_projection("i", approval_projection("i", "rq-unused", Some("s-i")));
    let r = rig_with(
        fake,
        None,
        T3Timing {
            max_live: 0,
            ..fast_timing()
        },
    )
    .await;
    let started = std::time::Instant::now();
    until("recent learned", || r.shared.sessions.owner("s-i")).await;
    // Budget: one overflow burst (3 picks, each at most sync time plus a
    // tick) and then the recent pick, well under 2 s at test timings.
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    let w_subs = r
        .fake
        .subscriptions()
        .iter()
        .filter(|(t, p)| t == "orchestration.subscribeThread" && p["threadId"] == "w")
        .count();
    let elapsed_ms = started.elapsed().as_millis() as usize;
    assert!(
        w_subs <= 1 + elapsed_ms / 200,
        "relearn_after limits retries of a null thread: {w_subs}"
    );
}

#[tokio::test]
async fn a_discovery_stream_that_never_synchronizes_is_retried() {
    // Sol rounds 2, 3 and 5: an idle recent thread (discovery path, no
    // working stream) sends a valid snapshot with a known session, then
    // never `synchronized`. A request stream is withheld the same way.
    let fake = FakeT3::start().await;
    for (t, sess) in [("stuck", "s-stuck"), ("ok", "s-ok")] {
        fake.put_thread(recent(shell_thread(t, "idle", None)));
        fake.set_projection(t, approval_projection(t, "rq-unused", Some(sess)));
    }
    fake.withhold_sync("stuck");
    fake.put_thread(shell_thread("req", "running", Some(("rq-1", "command"))));
    fake.set_projection("req", approval_projection("req", "rq-1", None));
    fake.withhold_sync("req");
    let r = rig(fake).await;
    let subs = |id: &str| {
        r.fake
            .subscriptions()
            .iter()
            .filter(|(t, p)| t == "orchestration.subscribeThread" && p["threadId"] == id)
            .count()
    };
    until("ok learned", || r.shared.sessions.owner("s-ok")).await;
    // The snapshot alone publishes the owner and sets Known (Sol round 5)...
    until("stuck published", || r.shared.sessions.owner("s-stuck")).await;
    // ...so only the timeout's reset to Unknown can make it eligible again.
    // Removing that reset makes this wait fail.
    until("retried", || {
        (subs("stuck") >= 2 && subs("req") >= 2).then_some(())
    })
    .await;
    assert_eq!(
        r.shared.sessions.owner("s-stuck").as_deref(),
        Some("t3:stuck"),
        "the timeout keeps the owner"
    );
    assert_eq!(
        t3_state(&r),
        SourceState::Ok,
        "a stalled thread stream is not a connection problem"
    );
}

#[tokio::test]
async fn a_validated_owner_survives_a_stalled_resume() {
    // Sol round 4: stalls, timeouts and retries are not identity evidence.
    let r = live_on_s1().await;
    r.fake.withhold_sync("a");
    let before = r.fake.subscriptions().len();
    r.fake.drop_connections();
    let subs = || {
        r.fake
            .subscriptions()
            .iter()
            .skip(before)
            .filter(|(t, p)| t == "orchestration.subscribeThread" && p["threadId"] == "a")
            .count()
    };
    // Sample through the reconnect, the sync timeout and at least one retry,
    // within a bounded time.
    tokio::time::timeout(Duration::from_secs(5), async {
        while subs() < 2 {
            assert_eq!(
                r.shared.sessions.owner("s1").as_deref(),
                Some("t3:a"),
                "owner kept while stalled"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the stalled stream is retried within 5 s");
    assert_eq!(r.shared.sessions.owner("s1").as_deref(), Some("t3:a"));
}

#[tokio::test]
async fn a_null_native_id_before_sync_clears_the_owner_at_once() {
    // Sol round 4: invalidation runs before the synchronization gate.
    let r = live_on_s1().await;
    r.fake.withhold_sync("a");
    let before = r.fake.subscriptions().len(); // before the disconnect
    r.fake.drop_connections();
    // While acd is disconnected, the same provider thread loses its native id.
    r.fake
        .push_thread_event("a", "provider-thread.updated", provider_event("pt-1", None));
    until("cleared", || {
        r.shared.sessions.owner("s1").is_none().then_some(())
    })
    .await;
    let resumed = r
        .fake
        .subscriptions()
        .iter()
        .skip(before)
        .filter(|(t, p)| t == "orchestration.subscribeThread" && p["threadId"] == "a")
        .count();
    assert_eq!(
        resumed, 1,
        "cleared by the resumed stream's event, before its sync timeout"
    );
}

#[tokio::test]
async fn a_changed_owner_is_published_at_once_without_synchronized() {
    // Sol round 5: s1 -> s2 with shell and detail agreeing publishes s2
    // immediately, even though that stream never synchronizes.
    let r = live_on_s1().await;
    r.fake.withhold_sync("a");
    let before = r.fake.subscriptions().len();
    r.fake.drop_connections();
    r.fake.push_thread_event(
        "a",
        "provider-thread.updated",
        provider_event("pt-1", Some("s2")),
    );
    until("s2", || r.shared.sessions.owner("s2")).await;
    assert!(r.shared.sessions.owner("s1").is_none());
    let resumed = r
        .fake
        .subscriptions()
        .iter()
        .skip(before)
        .filter(|(t, p)| t == "orchestration.subscribeThread" && p["threadId"] == "a")
        .count();
    assert_eq!(
        resumed, 1,
        "published from the resumed stream, before any timeout or retry"
    );
}

#[tokio::test]
async fn streams_release_under_a_steady_event_stream() {
    // Sol round 1, finding 3: busy streams must not starve the tick.
    let fake = FakeT3::start().await;
    for t in ["a", "busy"] {
        fake.put_thread(shell_thread(t, "running", None));
        fake.set_projection(t, approval_projection(t, "rq-unused", None));
    }
    let r = rig_with(
        fake,
        None,
        T3Timing {
            learn_window: Duration::ZERO,
            ..fast_timing()
        },
    )
    .await;
    until("two live", || {
        (r.fake.live_thread_streams().len() == 2).then_some(())
    })
    .await;
    let f = r.fake.clone();
    let noise = tokio::spawn(async move {
        loop {
            f.push_thread_event(
                "busy",
                "turn-item.updated",
                json!({"id": "n", "type": "assistant_message"}),
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    });
    r.fake.put_thread(shell_thread("a", "completed", None));
    until("a released", || {
        (!r.fake.live_thread_streams().contains(&"a".to_string())).then_some(())
    })
    .await;
    noise.abort();
}

#[tokio::test]
async fn retained_detail_is_bounded() {
    // Sol round 1, finding 5.
    let fake = FakeT3::start().await;
    fake.put_thread(shell_thread("a", "running", None));
    fake.set_projection("a", approval_projection("a", "rq-unused", None));
    for i in 0..100 {
        let t = format!("idle{i}");
        fake.put_thread(recent(shell_thread(&t, "idle", None)));
        fake.set_projection(
            &t,
            approval_projection(&t, "rq-unused", Some(&format!("s-{t}"))),
        );
    }
    let r = rig(fake).await;
    until("live", || {
        r.fake
            .live_thread_streams()
            .contains(&"a".to_string())
            .then_some(())
    })
    .await;
    for i in 0..500 {
        let rq = format!("rq-{i}");
        r.fake.push_thread_event(
            "a",
            "runtime-request.updated",
            json!({"id": rq, "kind": "command", "status": "pending"}),
        );
        r.fake.push_thread_event(
            "a",
            "runtime-request.updated",
            json!({"id": rq, "kind": "command", "status": "resolved"}),
        );
        r.fake.push_thread_event("a", "turn-item.updated", json!({"id": format!("cmd-{i}"), "type": "command_execution", "status": "completed", "input": "ls"}));
    }
    until("all learned", || {
        (0..100)
            .all(|i| r.shared.sessions.owner(&format!("s-idle{i}")).is_some())
            .then_some(())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let st = r.shared.stats();
    assert!(
        st.details <= 2,
        "{st:?}: only threads with an open stream keep detail"
    );
    assert!(st.requests <= 2, "{st:?}");
    assert!(
        st.items <= acd_adapters::t3::detail::MAX_COMMAND_ITEMS + 4,
        "{st:?}"
    );
    assert!(st.discovery_streams <= 1, "{st:?}");
}

#[tokio::test]
async fn a_churning_null_overflow_thread_is_retried_once_per_relearn_after() {
    // Step 6: a running thread's shell entry changes all the time; that must
    // not bypass `relearn_after` for a null overflow thread.
    let fake = FakeT3::start().await;
    fake.put_thread(shell_thread("w", "running", None));
    fake.set_projection("w", approval_projection("w", "rq-unused", None));
    let r = rig_with(
        fake,
        None,
        T3Timing {
            max_live: 0,
            ..fast_timing()
        },
    )
    .await;
    let started = std::time::Instant::now();
    while started.elapsed() < Duration::from_secs(2) {
        r.fake
            .put_thread(recent(shell_thread("w", "running", None)));
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    let w_subs = r
        .fake
        .subscriptions()
        .iter()
        .filter(|(t, p)| t == "orchestration.subscribeThread" && p["threadId"] == "w")
        .count();
    let bound = 1 + started.elapsed().as_millis() as usize / 200;
    assert!(w_subs <= bound, "{w_subs} subscriptions, at most {bound}");
}

fn thread_subs(r: &Rig, id: &str) -> Vec<serde_json::Value> {
    r.fake
        .subscriptions()
        .into_iter()
        .filter(|(t, p)| t == "orchestration.subscribeThread" && p["threadId"] == id)
        .map(|(_, p)| p)
        .collect()
}

#[tokio::test]
async fn an_unreadable_shell_entry_keeps_owner_stream_and_detail() {
    // R10: an unreadable entry is neither identity evidence nor a reason
    // to drop the thread's request stream.
    let fake = FakeT3::start().await;
    let entry = || shell_thread("a", "running", Some(("rq-1", "command")));
    fake.put_thread(entry());
    fake.set_projection("a", approval_projection("a", "rq-1", Some("s1")));
    let r = rig(fake).await;
    until("owner", || r.shared.sessions.owner("s1")).await;
    until("request stream", || {
        (r.shared.stats().request_streams == 1).then_some(())
    })
    .await;
    let before = thread_subs(&r, "a").len();
    let mut bad = entry();
    bad["status"] = json!(42);
    r.fake.put_thread(bad);
    until("a unknown", || {
        (agent(&r, "a")?.state == AgentState::Unknown).then_some(())
    })
    .await;
    // Longer than release_after: an unneeded stream would be gone by now.
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(r.shared.sessions.owner("s1").as_deref(), Some("t3:a"));
    let st = r.shared.stats();
    assert_eq!(st.request_streams, 1, "{st:?}");
    assert!(st.details >= 1, "{st:?}");
    assert_eq!(r.fake.live_thread_streams(), vec!["a".to_string()]);
    r.fake.put_thread(entry());
    until("needs you", || {
        (agent(&r, "a")?.state == AgentState::NeedsYou).then_some(())
    })
    .await;
    assert_eq!(r.shared.sessions.owner("s1").as_deref(), Some("t3:a"));
    assert_eq!(
        thread_subs(&r, "a").len(),
        before,
        "the same stream throughout"
    );
}

#[tokio::test]
async fn decode_degradation_clears_after_degraded_for() {
    // R12: one unreadable item does not degrade the source until restart.
    let fake = FakeT3::start().await;
    fake.put_thread(shell_thread("a", "idle", None));
    fake.put_thread(shell_thread("b", "idle", None));
    let timing = T3Timing {
        degraded_for: Duration::from_millis(400),
        ..fast_timing()
    };
    let r = rig_with(fake, None, timing).await;
    until("ok", || (t3_state(&r) == SourceState::Ok).then_some(())).await;
    let mut bad = shell_thread("b", "idle", None);
    bad["status"] = json!(42);
    r.fake.put_thread(bad);
    until("degraded", || {
        (t3_state(&r) == SourceState::Degraded).then_some(())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        t3_state(&r),
        SourceState::Degraded,
        "not before degraded_for"
    );
    until("ok again", || {
        (t3_state(&r) == SourceState::Ok).then_some(())
    })
    .await;
}

fn t3_message(r: &Rig) -> Option<String> {
    let ServerMessage::Welcome { machine, .. } = r.core.welcome() else {
        panic!()
    };
    machine.sources.t3.message
}

#[tokio::test]
async fn an_adapter_reported_problem_clears_after_degraded_for() {
    let fake = FakeT3::start().await;
    let timing = T3Timing {
        degraded_for: Duration::from_millis(400),
        ..fast_timing()
    };
    let r = rig_with(fake, None, timing).await;
    until("ok", || (t3_state(&r) == SourceState::Ok).then_some(())).await;
    r.shared
        .mailbox
        .degraded("the T3 token lacks the orchestration:operate scope");
    until("degraded", || {
        t3_message(&r).filter(|m| m.contains("operate scope"))
    })
    .await;
    until("ok again", || {
        (t3_state(&r) == SourceState::Ok).then_some(())
    })
    .await;
}

#[tokio::test]
async fn an_adapter_reported_problem_clears_on_reconnect() {
    let fake = FakeT3::start().await;
    let r = rig(fake).await; // degraded_for: 10 minutes
    until("ok", || (t3_state(&r) == SourceState::Ok).then_some(())).await;
    r.shared
        .mailbox
        .degraded("the T3 token lacks the orchestration:operate scope");
    until("degraded", || {
        t3_message(&r).filter(|m| m.contains("operate scope"))
    })
    .await;
    r.fake.drop_connections();
    until("lost", || {
        t3_message(&r).filter(|m| m.contains("connection to T3 lost"))
    })
    .await;
    until("ok again", || {
        (t3_state(&r) == SourceState::Ok).then_some(())
    })
    .await;
}

#[tokio::test]
async fn the_recent_group_gets_a_pick_after_an_overflow_burst() {
    // Every working thread is over the cap and stays null; the recent
    // thread is discovered within OVERFLOW_BURST + 1 discovery rounds.
    let fake = FakeT3::start().await;
    for i in 0..8 {
        let t = format!("w{i}");
        fake.put_thread(shell_thread(&t, "running", None));
        fake.set_projection(&t, approval_projection(&t, "rq-unused", None));
    }
    fake.put_thread(recent(shell_thread("i", "idle", None)));
    fake.set_projection("i", approval_projection("i", "rq-unused", Some("s-i")));
    let r = rig_with(
        fake,
        None,
        T3Timing {
            max_live: 0,
            ..fast_timing()
        },
    )
    .await;
    until("recent learned", || r.shared.sessions.owner("s-i")).await;
    let order: Vec<String> = r
        .fake
        .subscriptions()
        .into_iter()
        .filter(|(t, _)| t == "orchestration.subscribeThread")
        .map(|(_, p)| p["threadId"].as_str().unwrap().to_owned())
        .collect();
    let pos = order.iter().position(|t| t == "i").unwrap();
    assert!(pos <= OVERFLOW_BURST, "{order:?}");
}

#[tokio::test]
async fn a_persistent_shell_detail_disagreement_keeps_the_relearn_bound() {
    // R9, compared like with like: the shell keeps naming pt-1 while the
    // detail names pt-2, and the entry churns.
    let fake = FakeT3::start().await;
    fake.put_thread(running_on("w", "pt-1"));
    fake.set_projection(
        "w",
        with_provider(
            approval_projection("w", "rq-unused", None),
            "pt-2",
            Some("s-x"),
        ),
    );
    let r = rig_with(
        fake,
        None,
        T3Timing {
            max_live: 0,
            ..fast_timing()
        },
    )
    .await;
    let started = std::time::Instant::now();
    while started.elapsed() < Duration::from_secs(2) {
        r.fake.put_thread(recent(running_on("w", "pt-1")));
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    let w_subs = thread_subs(&r, "w").len();
    let bound = 1 + started.elapsed().as_millis() as usize / 200;
    assert!(w_subs <= bound, "{w_subs} subscriptions, at most {bound}");
    assert!(
        r.shared.sessions.owner("s-x").is_none(),
        "shell and detail never agreed"
    );
}

#[tokio::test]
async fn an_unreadable_thread_snapshot_is_released_and_retried() {
    let fake = FakeT3::start().await;
    fake.put_thread(shell_thread("a", "running", None));
    fake.set_projection("a", json!("not a projection"));
    let r = rig(fake).await;
    until("retried", || {
        (thread_subs(&r, "a").len() >= 2).then_some(())
    })
    .await;
    assert_eq!(t3_state(&r), SourceState::Degraded);
    r.fake
        .set_projection("a", approval_projection("a", "rq-unused", Some("s1")));
    until("learned", || r.shared.sessions.owner("s1")).await;
}

#[tokio::test]
async fn detail_and_cursor_survive_a_reconnect() {
    // The resumed stream continues from the cursor with no new snapshot;
    // only the kept detail can turn its event into the new owner.
    let r = live_on_s1().await;
    let before = thread_subs(&r, "a").len();
    r.fake.drop_connections();
    r.fake.push_thread_event(
        "a",
        "provider-thread.updated",
        provider_event("pt-1", Some("s2")),
    );
    until("s2", || r.shared.sessions.owner("s2")).await;
    let resumed = &thread_subs(&r, "a")[before];
    assert!(resumed.get("afterSequence").is_some(), "{resumed}");
}
