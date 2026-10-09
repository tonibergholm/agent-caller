use acd_adapters::herdr::{
    HerdrAdapter, HerdrConfig,
    fake::{FakeHerdr, fake_agent},
    keys::*,
    reconciler::Timing,
};
use acd_core::{
    Adapter, Core, CorrelationKey, DeliverError, Observation,
    testing::{permission_obs, test_machine},
};
use acd_protocol::*;
use std::{sync::Arc, time::Duration};

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
    adapter: Arc<HerdrAdapter>,
    _home: tempfile::TempDir,
}

async fn rig(agents: &[(&str, &str, &str)]) -> Rig {
    rig_with_timing(agents, Duration::from_millis(50)).await
}

/// `poll` is the reconciler's screen-read interval.
async fn rig_with_timing(agents: &[(&str, &str, &str)], poll: Duration) -> Rig {
    let home = tempfile::Builder::new()
        .prefix("acd")
        .tempdir_in("/tmp")
        .unwrap(); // short: macOS socket paths max 104 bytes
    let sock = home.path().join(".config/herdr/herdr.sock");
    std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
    let fake = FakeHerdr::start_at(&sock).await;
    for (p, a, s) in agents {
        fake.set_agent(fake_agent(p, a, s));
    }
    let timing = Timing {
        list_idle: Duration::from_millis(300),
        list_blocked: Duration::from_millis(100),
        backoff_max: Duration::from_millis(200),
        fallback_after: Duration::from_millis(200),
        poll,
    };
    let cfg = HerdrConfig {
        enabled: true,
        home: home.path().into(),
        env_socket: None,
        extra_sockets: vec![],
        scan_interval: Duration::from_millis(100),
        timing,
        keys: KeysTiming {
            evidence_timeout: Duration::from_millis(500),
            ..KeysTiming::default()
        },
    };
    let adapter = HerdrAdapter::new(cfg);
    let core = Core::new(test_machine(), vec![adapter.clone()]);
    adapter.start(core.clone());
    until("agents", || {
        (core.attach(None).0.len() == 1 && adapter.panes().read().unwrap().len() == agents.len())
            .then_some(())
    })
    .await;
    Rig {
        fake,
        core,
        adapter,
        _home: home,
    }
}

fn agent(r: &Rig) -> Agent {
    r.core
        .agent(r.adapter.panes().read().unwrap().keys().next().unwrap())
        .unwrap()
}

#[tokio::test]
async fn follow_up_reaches_an_idle_agent() {
    let r = rig(&[("w1:p1", "claude", "idle")]).await;
    r.adapter.send(&agent(&r), "run the tests").await.unwrap();
    assert_eq!(
        r.fake.prompts(),
        vec![("w1:p1".to_string(), "run the tests".to_string())]
    );
    until("working", || {
        (agent(&r).state == AgentState::Working).then_some(())
    })
    .await;
}

#[tokio::test]
async fn follow_up_errors_map_to_not_delivered() {
    let r = rig(&[("w1:p1", "claude", "blocked")]).await;
    let a = agent(&r);
    assert!(matches!(
        r.adapter.send(&a, "hi").await,
        Err(DeliverError::NotDelivered(ErrorCode::AgentBusy))
    ));
    assert!(matches!(
        r.adapter.send(&a, "a\u{202E}b").await,
        Err(DeliverError::NotDelivered(ErrorCode::BadRequest))
    ));
    assert!(matches!(
        r.adapter.send(&a, &"x".repeat(16 * 1024 + 1)).await,
        Err(DeliverError::NotDelivered(ErrorCode::BadRequest))
    ));
    let mut gone = a.clone();
    gone.id = "herdr:000000000000:w9:p9".into();
    assert!(matches!(
        r.adapter.send(&gone, "hi").await,
        Err(DeliverError::NotDelivered(ErrorCode::AgentGone))
    ));
    assert!(r.fake.prompts().is_empty());
}

#[tokio::test]
async fn prompt_reply_lost_after_write_is_uncertain() {
    let r = rig(&[("w1:p1", "claude", "idle")]).await;
    r.fake.hang_next("agent.prompt");
    let res = tokio::time::timeout(Duration::from_secs(11), r.adapter.send(&agent(&r), "hi"))
        .await
        .unwrap();
    assert!(matches!(res, Err(DeliverError::Uncertain(_))), "{res:?}");
    assert_eq!(r.fake.prompts().len(), 1);
}

#[tokio::test]
async fn mark_seen_never_touches_herdr() {
    let r = rig(&[("w1:p1", "claude", "done")]).await;
    let before = r.fake.calls().len();
    r.core
        .handle(
            "c",
            ClientMessage::AgentMarkSeen {
                id: "m1".into(),
                agent_id: agent(&r).id,
            },
        )
        .await;
    assert_eq!(agent(&r).state, AgentState::Idle);
    assert!(
        r.fake.calls()[before..]
            .iter()
            .all(|(m, _)| m != "agent.focus")
    );
}

#[tokio::test]
async fn disabled_config_reports_disabled_and_starts_nothing() {
    let home = tempfile::tempdir().unwrap();
    let cfg = HerdrConfig {
        enabled: false,
        ..HerdrConfig::from_env(false, vec![])
    };
    let cfg = HerdrConfig {
        home: home.path().into(),
        ..cfg
    };
    let adapter = HerdrAdapter::new(cfg);
    let core = Core::new(test_machine(), vec![adapter.clone()]);
    adapter.start(core.clone());
    let ServerMessage::Welcome { machine, .. } = core.welcome() else {
        panic!()
    };
    assert_eq!(machine.sources.herdr.state, SourceState::Disabled);
}

#[tokio::test]
async fn unknown_server_code_is_uncertain() {
    let r = rig(&[("w1:p1", "claude", "idle")]).await;
    r.fake.fail_next("agent.prompt", "internal_error");
    let res = r.adapter.send(&agent(&r), "hi").await;
    assert!(matches!(res, Err(DeliverError::Uncertain(_))), "{res:?}");
    assert!(r.fake.prompts().is_empty(), "fail_next applies nothing");
}

#[tokio::test]
async fn known_refusal_is_not_delivered_source_unavailable() {
    let r = rig(&[("w1:p1", "claude", "idle")]).await;
    r.fake.fail_next("agent.prompt", "invalid_params");
    let res = r.adapter.send(&agent(&r), "hi").await;
    assert!(
        matches!(
            res,
            Err(DeliverError::NotDelivered(ErrorCode::SourceUnavailable))
        ),
        "{res:?}"
    );
    assert!(r.fake.prompts().is_empty());
}

#[tokio::test]
async fn connect_failure_is_not_delivered_source_unavailable() {
    let r = rig(&[("w1:p1", "claude", "idle")]).await;
    let a = agent(&r);
    r.fake.stop().await;
    let res = r.adapter.send(&a, "hi").await;
    assert!(
        matches!(
            res,
            Err(DeliverError::NotDelivered(ErrorCode::SourceUnavailable))
        ),
        "{res:?}"
    );
}

// ---- Keys delivery engine (Task 9) ----

fn screen(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../../tests/fixtures/herdr/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

struct Fixed(Verdict);
#[async_trait::async_trait]
impl Evidence for Fixed {
    async fn outcome(&self, _: &KeysTarget, _: &str, _: tokio::time::Instant) -> Verdict {
        self.0
    }
}
struct Never;
#[async_trait::async_trait]
impl Evidence for Never {
    async fn outcome(&self, _: &KeysTarget, _: &str, _: tokio::time::Instant) -> Verdict {
        std::future::pending().await
    }
}

/// Opens a keys request showing the Claude permission dialog. When a key
/// arrives, the screens switch to `after_detection` / `after_visible`.
async fn keys_rig(after_detection: &'static str, after_visible: &'static str) -> (Rig, String) {
    keys_rig_with(after_detection, after_visible, false).await
}

async fn keys_rig_with(
    after_detection: &'static str,
    after_visible: &'static str,
    watch: bool,
) -> (Rig, String) {
    let r = rig_with_timing(
        &[("w1:p1", "claude", "working")],
        if watch {
            Duration::from_millis(50)
        } else {
            Duration::from_secs(3600)
        },
    )
    .await;
    let dialog = screen("claude-permission-bash.txt");
    r.fake.set_screen("w1:p1", "detection", &dialog);
    r.fake.set_screen("w1:p1", "visible", &dialog);
    r.fake.on_keys(move |f, pane, _keys| {
        f.set_screen(pane, "detection", &screen(after_detection));
        f.set_screen(pane, "visible", &screen(after_visible));
        f.set_status(pane, "working");
    });
    let id = agent(&r).id;
    let mut obs = permission_obs(&id, "k1");
    obs.answer_path = AnswerPath::Keys;
    r.core.observe(Observation::RequestSeen(obs));
    r.fake.set_status("w1:p1", "blocked");
    let key = CorrelationKey("k1".into());
    let rid = r.core.request_id_for(&id, &key).unwrap();
    r.adapter
        .keys_targets()
        .set(KeysTarget::from_screen(rid.clone(), id, key, &dialog, allow_deny()).unwrap())
        .unwrap();
    (r, rid)
}

fn allow_deny() -> std::collections::HashMap<String, String> {
    [
        ("allow".to_string(), "1".to_string()),
        ("deny".to_string(), "esc".to_string()),
    ]
    .into()
}

/// The permission dialog after a redraw of a transcript line above it: same
/// dialog, different whole-screen fingerprint.
fn redrawn_dialog() -> String {
    let s = screen("claude-permission-bash.txt");
    let r = s.replace("⏺ Creating empty file", "⏺ Creating an empty file");
    assert_ne!(s, r);
    r
}

/// The permission dialog as a narrower pane draws it: shorter rules, the
/// transcript prompt wrapped, and either the command (`"command"`) or option
/// 2's label (`"label"`) wrapped onto a continuation line. Same dialog,
/// different whole-screen fingerprint.
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

/// `agent.read` calls of `source` so far.
fn reads(r: &Rig, source: &str) -> usize {
    r.fake
        .calls()
        .iter()
        .filter(|(m, p)| m == "agent.read" && p["source"] == source)
        .count()
}

async fn answer(r: &Rig, cmd: &str, rid: &str, option: &str) -> CommandResult {
    r.core
        .handle(
            "c",
            ClientMessage::RequestAnswer {
                id: cmd.into(),
                request_id: rid.into(),
                revision: None,
                option_id: Some(option.into()),
                text: None,
            },
        )
        .await
}

/// The first `request.resolved` on `rx` (attach before answering).
async fn resolution(
    rx: &mut tokio::sync::broadcast::Receiver<ServerMessage>,
) -> (ResolvedBy, Option<String>) {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            match rx.recv().await {
                Ok(ServerMessage::RequestResolved { by, option_id, .. }) => {
                    return (by, option_id);
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(e) => panic!("event stream ended: {e}"),
            }
        }
    })
    .await
    .expect("no request.resolved within 8 s")
}

fn delivered(res: &CommandResult) -> bool {
    res.ok && res.outcome == Some(Outcome::Delivered)
}

#[tokio::test]
async fn closed_dialog_without_evidence_is_uncertain() {
    let (r, rid) = keys_rig("claude-idle-detection.txt", "claude-idle-prompt.txt").await;
    let (_, mut rx) = r.core.attach(None);
    assert!(delivered(&answer(&r, "a1", &rid, "allow").await));
    assert_eq!(resolution(&mut rx).await, (ResolvedBy::Uncertain, None));
    assert_eq!(
        r.fake.keys_sent(),
        vec![("w1:p1".to_string(), vec!["1".to_string()])]
    );
    assert!(
        r.adapter.keys_targets().get(&rid).is_none(),
        "closed: the target is removed"
    );
}

#[tokio::test]
async fn consistent_evidence_is_inbox() {
    let (r, rid) = keys_rig("claude-idle-detection.txt", "claude-idle-prompt.txt").await;
    r.adapter.set_evidence(Arc::new(Fixed(Verdict::Consistent)));
    let (_, mut rx) = r.core.attach(None);
    answer(&r, "a1", &rid, "allow").await;
    assert_eq!(
        resolution(&mut rx).await,
        (ResolvedBy::Inbox, Some("allow".into()))
    );
}

#[tokio::test]
async fn contradicting_evidence_is_terminal() {
    let (r, rid) = keys_rig("claude-idle-detection.txt", "claude-idle-prompt.txt").await;
    r.adapter
        .set_evidence(Arc::new(Fixed(Verdict::Contradicts)));
    let (_, mut rx) = r.core.attach(None);
    answer(&r, "a1", &rid, "allow").await;
    assert_eq!(resolution(&mut rx).await, (ResolvedBy::Terminal, None));
}

#[tokio::test]
async fn hanging_evidence_is_bounded() {
    let (r, rid) = keys_rig("claude-idle-detection.txt", "claude-idle-prompt.txt").await;
    r.adapter.set_evidence(Arc::new(Never));
    let (_, mut rx) = r.core.attach(None);
    answer(&r, "a1", &rid, "allow").await;
    assert_eq!(resolution(&mut rx).await.0, ResolvedBy::Uncertain);
}

#[tokio::test]
async fn mismatch_sends_nothing() {
    let (r, rid) = keys_rig("claude-idle-detection.txt", "claude-idle-prompt.txt").await;
    let changed =
        screen("claude-permission-bash.txt").replace("touch /tmp/acp0/t1.txt", "rm -rf /tmp/acp0");
    r.fake.set_screen("w1:p1", "detection", &changed);
    assert_eq!(
        answer(&r, "a1", &rid, "allow").await.error_code(),
        Some(ErrorCode::ScreenMismatch)
    );
    assert!(r.fake.keys_sent().is_empty());
    assert_eq!(
        r.core.request(&rid).unwrap().status,
        RequestStatus::Open,
        "nothing reached the agent: back to open"
    );
}

#[tokio::test]
async fn no_dialog_on_screen_is_a_mismatch() {
    let (r, rid) = keys_rig("claude-idle-detection.txt", "claude-idle-prompt.txt").await;
    r.fake
        .set_screen("w1:p1", "detection", &screen("claude-idle-detection.txt"));
    assert_eq!(
        answer(&r, "a1", &rid, "allow").await.error_code(),
        Some(ErrorCode::ScreenMismatch)
    );
    assert!(r.fake.keys_sent().is_empty());
}

#[tokio::test]
async fn truncated_guard_read_sends_nothing() {
    let (r, rid) = keys_rig("claude-idle-detection.txt", "claude-idle-prompt.txt").await;
    r.fake.set_truncated("w1:p1", "detection", true);
    assert_eq!(
        answer(&r, "a1", &rid, "allow").await.error_code(),
        Some(ErrorCode::ScreenMismatch)
    );
    assert!(r.fake.keys_sent().is_empty());
    assert_eq!(r.core.request(&rid).unwrap().status, RequestStatus::Open);
}

#[tokio::test]
async fn option_keys_must_be_digits_or_esc() {
    let targets = KeysTargets::default();
    let dialog = screen("claude-permission-bash.txt"); // options 1..4
    let keys = |keys: &[&str]| -> std::collections::HashMap<String, String> {
        keys.iter()
            .enumerate()
            .map(|(i, k)| (format!("o{i}"), k.to_string()))
            .collect()
    };
    let target = |k: &[&str]| {
        KeysTarget::from_screen(
            "r1".into(),
            "a".into(),
            CorrelationKey("k".into()),
            &dialog,
            keys(k),
        )
    };
    for bad in ["enter", "0", "10", "", "backspace", "1 ", "Esc", "y", "5"] {
        assert!(target(&["1", bad]).is_err(), "{bad:?}");
    }
    // A target can only come from `from_screen`; `set` re-checks key names
    // (unit test `set_rejects_a_target_altered_after_from_screen`).
    targets
        .set(target(&["1", "2", "3", "4", "esc"]).unwrap())
        .unwrap();
    assert!(targets.get("r1").is_some());
    assert_eq!(targets.for_agent("a").len(), 1);
    assert!(targets.remove("r1").is_some());
}

#[tokio::test]
async fn superseded_during_the_guard_read_sends_nothing() {
    let (r, rid) = keys_rig("claude-idle-detection.txt", "claude-idle-prompt.txt").await;
    let gate = r.fake.block_next("agent.read");
    let agent_id = agent(&r).id;
    let pending = tokio::spawn({
        let core = r.core.clone();
        let rid = rid.clone();
        async move {
            core.handle(
                "c",
                ClientMessage::RequestAnswer {
                    id: "a1".into(),
                    request_id: rid,
                    revision: None,
                    option_id: Some("allow".into()),
                    text: None,
                },
            )
            .await
        }
    });
    gate.arrived().await; // the guard read is held
    r.core.observe(Observation::RequestSeen(permission_obs(
        &agent_id,
        "another-dialog",
    )));
    gate.release();
    let res = pending.await.unwrap();
    assert_eq!(res.error_code(), Some(ErrorCode::RequestGone), "{res:?}");
    assert!(
        r.fake.keys_sent().is_empty(),
        "a superseded request must never send a key"
    );
}

#[tokio::test]
async fn cancelled_during_the_guard_read_never_sends_later() {
    // No watch: the gate must catch the guard read.
    let (r, rid) = keys_rig("claude-idle-detection.txt", "claude-idle-prompt.txt").await;
    let gate = r.fake.block_next("agent.read");
    let task = tokio::spawn({
        let core = r.core.clone();
        let rid = rid.clone();
        async move {
            core.handle(
                "c",
                ClientMessage::RequestAnswer {
                    id: "a1".into(),
                    request_id: rid,
                    revision: None,
                    option_id: Some("allow".into()),
                    text: None,
                },
            )
            .await
        }
    });
    gate.arrived().await; // the guard read is in flight: delivery has begun
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    gate.release();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        r.fake.keys_sent().is_empty(),
        "a dropped delivery must never complete a send"
    );
    assert!(
        r.core.request(&rid).is_none(),
        "AbandonGuard resolved it uncertain"
    );
    assert!(
        r.adapter.keys_targets().get(&rid).is_some(),
        "the watch keeps monitoring the dialog"
    );
}

#[tokio::test]
async fn uncertain_occurrence_is_monitored_until_gone() {
    // The key does not close the dialog.
    let (r, rid) = keys_rig_with(
        "claude-permission-bash.txt",
        "claude-permission-bash.txt",
        true,
    )
    .await;
    let (_, mut rx) = r.core.attach(None);
    answer(&r, "a1", &rid, "allow").await;
    assert_eq!(resolution(&mut rx).await.0, ResolvedBy::Uncertain);
    assert!(
        r.adapter.keys_targets().get(&rid).is_some(),
        "still on screen: keep watching"
    );
    r.fake
        .set_screen("w1:p1", "detection", &screen("claude-idle-detection.txt"));
    until("monitor removes the target", || {
        r.adapter.keys_targets().get(&rid).is_none().then_some(())
    })
    .await;
    // Confirmed gone: the same reusable key now opens a new occurrence.
    let mut again = permission_obs(&agent(&r).id, "k1");
    again.answer_path = AnswerPath::Keys;
    r.core.observe(Observation::RequestSeen(again));
    let new_id = r
        .core
        .request_id_for(&agent(&r).id, &CorrelationKey("k1".into()));
    assert!(new_id.is_some());
    assert_ne!(new_id, Some(rid));
}

#[tokio::test]
async fn stray_digit_in_prompt_is_uncertain() {
    let (r, rid) = keys_rig(
        "claude-idle-detection-stray-1.txt",
        "claude-idle-prompt-stray-1.txt",
    )
    .await;
    r.adapter.set_evidence(Arc::new(Fixed(Verdict::Consistent)));
    let (_, mut rx) = r.core.attach(None);
    answer(&r, "a1", &rid, "allow").await;
    assert_eq!(
        resolution(&mut rx).await.0,
        ResolvedBy::Uncertain,
        "a stray key beats consistent evidence"
    );
    assert_eq!(r.fake.keys_sent().len(), 1, "never a corrective key");
}

#[tokio::test]
async fn transient_stray_is_caught_even_when_the_ack_is_late() {
    let (r, rid) = keys_rig("claude-idle-detection.txt", "claude-idle-prompt.txt").await;
    // The key leaves a stray "1" for ~600 ms, then it is gone; the ack arrives after 1.5 s.
    r.fake.on_keys(|f, pane, _| {
        f.set_screen(
            pane,
            "detection",
            &screen("claude-idle-detection-stray-1.txt"),
        );
        f.set_screen(pane, "visible", &screen("claude-idle-prompt-stray-1.txt"));
        let f = f.clone_handle();
        let pane = pane.to_string();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(600)).await;
            f.set_screen(&pane, "detection", &screen("claude-idle-detection.txt"));
            f.set_screen(&pane, "visible", &screen("claude-idle-prompt.txt"));
        });
    });
    r.fake
        .delay_next("agent.send_keys", Duration::from_millis(1500));
    r.adapter.set_evidence(Arc::new(Fixed(Verdict::Consistent)));
    let (_, mut rx) = r.core.attach(None);
    answer(&r, "a1", &rid, "allow").await;
    assert_eq!(
        resolution(&mut rx).await.0,
        ResolvedBy::Uncertain,
        "the 300 ms check runs from the write, not the ack"
    );
    // Prove the digit was actually observed, not that a check was merely
    // unavailable: a visible read happened while the stray was on screen
    // (< 600 ms after the keys arrived).
    let calls = r.fake.calls_timed();
    let keys_at = calls
        .iter()
        .find(|(_, m, _)| m == "agent.send_keys")
        .unwrap()
        .0;
    assert!(calls.iter().any(|(t, m, p)| m == "agent.read"
        && p["source"] == "visible"
        && *t > keys_at
        && *t < keys_at + Duration::from_millis(600)));
}

#[tokio::test]
async fn late_refusal_publishes_nothing_even_if_the_dialog_closed() {
    // The terminal closes the dialog on its own while herdr's refusal of our key is delayed.
    let (r, rid) = keys_rig("claude-idle-detection.txt", "claude-idle-prompt.txt").await;
    r.adapter.set_evidence(Arc::new(Fixed(Verdict::Consistent)));
    r.fake.fail_next("agent.send_keys", "invalid_key");
    let gate = r.fake.block_next("agent.send_keys"); // the write has arrived; the reply is held
    let task = tokio::spawn({
        let core = r.core.clone();
        let rid = rid.clone();
        async move {
            core.handle(
                "c",
                ClientMessage::RequestAnswer {
                    id: "a1".into(),
                    request_id: rid,
                    revision: None,
                    option_id: Some("allow".into()),
                    text: None,
                },
            )
            .await
        }
    });
    gate.arrived().await;
    // Closed at the terminal.
    r.fake
        .set_screen("w1:p1", "detection", &screen("claude-idle-detection.txt"));
    tokio::time::sleep(Duration::from_millis(600)).await; // confirmation sees two absent reads
    gate.release();
    let res = task.await.unwrap();
    assert_eq!(res.error_code(), Some(ErrorCode::SourceUnavailable));
    tokio::time::sleep(Duration::from_millis(3500)).await; // past the confirmation window
    assert_eq!(
        r.core.request(&rid).map(|q| q.status),
        Some(RequestStatus::Open),
        "never inbox without an acd delivery"
    );
    assert!(r.fake.keys_sent().is_empty());
}

#[tokio::test]
async fn esc_gets_no_prompt_check() {
    let (r, rid) = keys_rig(
        "claude-idle-detection-stray-1.txt",
        "claude-idle-prompt-stray-1.txt",
    )
    .await;
    r.adapter.set_evidence(Arc::new(Fixed(Verdict::Consistent)));
    let (_, mut rx) = r.core.attach(None);
    answer(&r, "a1", &rid, "deny").await;
    assert_eq!(
        resolution(&mut rx).await,
        (ResolvedBy::Inbox, Some("deny".into()))
    );
    assert_eq!(
        r.fake.keys_sent(),
        vec![("w1:p1".to_string(), vec!["esc".to_string()])]
    );
}

#[tokio::test]
async fn dialog_that_never_closes_is_uncertain() {
    let (r, rid) = keys_rig("claude-permission-bash.txt", "claude-permission-bash.txt").await;
    r.adapter.set_evidence(Arc::new(Fixed(Verdict::Consistent)));
    let (_, mut rx) = r.core.attach(None);
    answer(&r, "a1", &rid, "allow").await;
    assert_eq!(resolution(&mut rx).await.0, ResolvedBy::Uncertain);
    assert!(r.adapter.keys_targets().get(&rid).is_some());
}

#[tokio::test]
async fn no_target_is_not_ready_and_text_sends_nothing() {
    let (r, rid) = keys_rig("claude-idle-detection.txt", "claude-idle-prompt.txt").await;
    r.adapter.keys_targets().remove(&rid);
    assert_eq!(
        answer(&r, "a1", &rid, "allow").await.error_code(),
        Some(ErrorCode::RequestNotReady)
    );
    let res = r
        .core
        .handle(
            "c",
            ClientMessage::RequestAnswer {
                id: "t1".into(),
                request_id: rid.clone(),
                revision: None,
                option_id: None,
                text: Some("hi".into()),
            },
        )
        .await;
    assert!(res.error_code().is_some());
    assert!(r.fake.keys_sent().is_empty());
}

#[tokio::test]
async fn text_answer_is_not_supported_by_the_engine() {
    let (r, rid) = keys_rig("claude-idle-detection.txt", "claude-idle-prompt.txt").await;
    let req = r.core.request(&rid).unwrap();
    assert!(matches!(
        r.adapter
            .deliver(&req, &acd_core::Answer::Text("hi".into()))
            .await,
        Err(DeliverError::NotDelivered(ErrorCode::NotSupported))
    ));
    assert!(r.fake.keys_sent().is_empty());
}

#[tokio::test]
async fn send_keys_reply_lost_is_uncertain_and_sends_once() {
    let (r, rid) = keys_rig("claude-idle-detection.txt", "claude-idle-prompt.txt").await;
    r.fake.hang_next("agent.send_keys");
    let (_, mut rx) = r.core.attach(None);
    assert!(delivered(&answer(&r, "a1", &rid, "allow").await));
    assert_eq!(resolution(&mut rx).await.0, ResolvedBy::Uncertain);
    assert!(
        r.adapter.keys_targets().get(&rid).is_some(),
        "a lost reply keeps the occurrence monitored"
    );
    // The identical command is answered from the idempotency cache...
    assert!(delivered(&answer(&r, "a1", &rid, "allow").await));
    // ...and a new command finds the request gone.
    assert_eq!(
        answer(&r, "a2", &rid, "allow").await.error_code(),
        Some(ErrorCode::RequestGone)
    );
    assert_eq!(r.fake.keys_sent().len(), 1);
}

#[tokio::test]
async fn unclaimed_request_answered_in_the_terminal_resolves_terminal() {
    let (r, rid) = keys_rig_with("claude-idle-detection.txt", "claude-idle-prompt.txt", true).await;
    let (_, mut rx) = r.core.attach(None);
    // The user answered at the terminal.
    r.fake
        .set_screen("w1:p1", "detection", &screen("claude-idle-detection.txt"));
    assert_eq!(resolution(&mut rx).await, (ResolvedBy::Terminal, None));
    assert!(r.adapter.keys_targets().get(&rid).is_none());
    assert!(r.fake.keys_sent().is_empty());
}

#[tokio::test]
async fn a_gone_pane_drops_its_targets() {
    let (r, rid) = keys_rig_with("claude-idle-detection.txt", "claude-idle-prompt.txt", true).await;
    r.fake.remove_agent("w1:p1");
    r.fake.push_event(serde_json::json!({
        "event": "pane_closed",
        "data": {"pane_id": "w1:p1", "workspace_id": "w1"},
    }));
    until("target dropped", || {
        r.adapter.keys_targets().get(&rid).is_none().then_some(())
    })
    .await;
    assert!(r.fake.keys_sent().is_empty());
}

#[tokio::test]
async fn from_screen_needs_a_fingerprinted_dialog() {
    for name in [
        "codex-permission-collapsed-small-pane-detection.txt",
        "codex-trust-dialog-detection.txt",
        "claude-idle-detection.txt",
    ] {
        let t = KeysTarget::from_screen(
            "r1".into(),
            "a".into(),
            CorrelationKey("k".into()),
            &screen(name),
            [("x".to_string(), "esc".to_string())].into(),
        );
        assert!(t.is_err(), "{name}");
    }
}

#[tokio::test]
async fn redraw_above_an_unclaimed_dialog_does_not_resolve_it() {
    let (r, rid) = keys_rig_with("claude-idle-detection.txt", "claude-idle-prompt.txt", true).await;
    r.fake.set_screen("w1:p1", "detection", &redrawn_dialog());
    let before = reads(&r, "detection");
    until("several watch reads", || {
        (reads(&r, "detection") >= before + 5).then_some(())
    })
    .await;
    assert_eq!(
        r.core.request(&rid).map(|q| q.status),
        Some(RequestStatus::Open)
    );
    assert!(r.adapter.keys_targets().get(&rid).is_some());
}

#[tokio::test]
async fn redraw_after_an_accepted_key_is_not_a_close() {
    let (r, rid) = keys_rig("claude-idle-detection.txt", "claude-idle-prompt.txt").await;
    r.fake.on_keys(|f, pane, _| {
        f.set_screen(pane, "detection", &redrawn_dialog());
        f.set_screen(pane, "visible", &redrawn_dialog());
    });
    r.adapter.set_evidence(Arc::new(Fixed(Verdict::Consistent)));
    let (_, mut rx) = r.core.attach(None);
    answer(&r, "a1", &rid, "allow").await;
    assert_eq!(
        resolution(&mut rx).await.0,
        ResolvedBy::Uncertain,
        "the dialog is still up: never inbox"
    );
    assert!(r.adapter.keys_targets().get(&rid).is_some());
}

#[tokio::test]
async fn rewrap_of_an_unclaimed_dialog_does_not_resolve_it() {
    let (r, rid) = keys_rig_with("claude-idle-detection.txt", "claude-idle-prompt.txt", true).await;
    for what in ["command", "label"] {
        r.fake
            .set_screen("w1:p1", "detection", &rewrapped_dialog(what));
        let before = reads(&r, "detection");
        until("several watch reads", || {
            (reads(&r, "detection") >= before + 5).then_some(())
        })
        .await;
        assert_eq!(
            r.core.request(&rid).map(|q| q.status),
            Some(RequestStatus::Open),
            "{what}"
        );
        assert!(r.adapter.keys_targets().get(&rid).is_some(), "{what}");
    }
}

#[tokio::test]
async fn rewrap_after_an_accepted_key_is_not_a_close() {
    for what in ["command", "label"] {
        let (r, rid) = keys_rig("claude-idle-detection.txt", "claude-idle-prompt.txt").await;
        let after = rewrapped_dialog(what);
        r.fake.on_keys(move |f, pane, _| {
            f.set_screen(pane, "detection", &after);
            f.set_screen(pane, "visible", &after);
        });
        r.adapter.set_evidence(Arc::new(Fixed(Verdict::Consistent)));
        let (_, mut rx) = r.core.attach(None);
        answer(&r, "a1", &rid, "allow").await;
        assert_eq!(
            resolution(&mut rx).await.0,
            ResolvedBy::Uncertain,
            "{what}: the dialog is still up: never inbox"
        );
        assert!(r.adapter.keys_targets().get(&rid).is_some(), "{what}");
    }
}

#[tokio::test]
async fn guard_after_a_redraw_is_a_mismatch() {
    let (r, rid) = keys_rig("claude-idle-detection.txt", "claude-idle-prompt.txt").await;
    r.fake.set_screen("w1:p1", "detection", &redrawn_dialog());
    assert_eq!(
        answer(&r, "a1", &rid, "allow").await.error_code(),
        Some(ErrorCode::ScreenMismatch)
    );
    assert!(r.fake.keys_sent().is_empty());
}

#[tokio::test]
async fn truncated_reads_are_neutral_in_confirmation() {
    let (r, rid) = keys_rig("claude-idle-detection.txt", "claude-idle-prompt.txt").await;
    r.fake.on_keys(|f, pane, _| {
        f.set_screen(pane, "detection", &screen("claude-idle-detection.txt"));
        f.set_screen(pane, "visible", &screen("claude-idle-prompt.txt"));
        f.set_truncated(pane, "detection", true);
    });
    r.adapter.set_evidence(Arc::new(Fixed(Verdict::Consistent)));
    let (_, mut rx) = r.core.attach(None);
    answer(&r, "a1", &rid, "allow").await;
    assert_eq!(
        resolution(&mut rx).await.0,
        ResolvedBy::Uncertain,
        "truncated reads never prove a close"
    );
    assert!(r.adapter.keys_targets().get(&rid).is_some());
}

#[tokio::test]
async fn truncated_reads_are_neutral_in_the_watch() {
    let (r, rid) = keys_rig_with("claude-idle-detection.txt", "claude-idle-prompt.txt", true).await;
    let (_, mut rx) = r.core.attach(None);
    r.fake.set_truncated("w1:p1", "detection", true);
    r.fake
        .set_screen("w1:p1", "detection", &screen("claude-idle-detection.txt"));
    let before = reads(&r, "detection");
    until("several watch reads", || {
        (reads(&r, "detection") >= before + 5).then_some(())
    })
    .await;
    assert_eq!(
        r.core.request(&rid).map(|q| q.status),
        Some(RequestStatus::Open)
    );
    // The same screen, read whole, does count.
    r.fake.set_truncated("w1:p1", "detection", false);
    assert_eq!(resolution(&mut rx).await, (ResolvedBy::Terminal, None));
}

#[tokio::test]
async fn a_target_cannot_be_replaced_mid_delivery() {
    let (r, rid) = keys_rig("claude-idle-detection.txt", "claude-idle-prompt.txt").await;
    let gate = r.fake.block_next("agent.read");
    let pending = tokio::spawn({
        let core = r.core.clone();
        let rid = rid.clone();
        async move {
            core.handle(
                "c",
                ClientMessage::RequestAnswer {
                    id: "a1".into(),
                    request_id: rid,
                    revision: None,
                    option_id: Some("allow".into()),
                    text: None,
                },
            )
            .await
        }
    });
    gate.arrived().await; // the guard read is held
    let old = r.adapter.keys_targets().get(&rid).unwrap();
    let replaced = KeysTarget::from_screen(
        rid.clone(),
        old.agent_id().to_string(),
        old.key().clone(),
        &redrawn_dialog(),
        allow_deny(),
    )
    .unwrap();
    assert!(r.adapter.keys_targets().set(replaced).is_err());
    gate.release();
    let res = pending.await.unwrap();
    assert!(delivered(&res), "{res:?}");
    assert_eq!(r.fake.keys_sent().len(), 1, "the original target was used");
}

#[tokio::test]
async fn a_held_agent_lock_delays_the_key() {
    let (r, rid) = keys_rig("claude-idle-detection.txt", "claude-idle-prompt.txt").await;
    let agent_id = r.core.request(&rid).unwrap().agent_id;
    let guard = r.adapter.agent_locks().lock(&agent_id).await;
    let pending = tokio::spawn({
        let core = r.core.clone();
        let rid = rid.clone();
        async move {
            core.handle(
                "c",
                ClientMessage::RequestAnswer {
                    id: "a1".into(),
                    request_id: rid,
                    revision: None,
                    option_id: Some("allow".into()),
                    text: None,
                },
            )
            .await
        }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        r.fake.keys_sent().is_empty(),
        "no key while another path holds the agent"
    );
    let mut other = permission_obs(&agent_id, "k2");
    other.answer_path = AnswerPath::Keys;
    r.core.observe(Observation::RequestSeen(other));
    drop(guard);
    let res = pending.await.unwrap();
    assert_eq!(res.error_code(), Some(ErrorCode::RequestGone), "{res:?}");
    assert!(r.fake.keys_sent().is_empty());
}

#[tokio::test]
async fn a_dialog_change_while_waiting_for_the_lock_sends_nothing() {
    let (r, rid) = keys_rig("claude-idle-detection.txt", "claude-idle-prompt.txt").await;
    let agent_id = r.core.request(&rid).unwrap().agent_id;
    let guard = r.adapter.agent_locks().lock(&agent_id).await;
    let pending = tokio::spawn({
        let core = r.core.clone();
        let rid = rid.clone();
        async move {
            core.handle(
                "c",
                ClientMessage::RequestAnswer {
                    id: "a1".into(),
                    request_id: rid,
                    revision: None,
                    option_id: Some("allow".into()),
                    text: None,
                },
            )
            .await
        }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    r.fake.set_screen("w1:p1", "detection", &redrawn_dialog());
    drop(guard);
    let res = pending.await.unwrap();
    assert_eq!(res.error_code(), Some(ErrorCode::ScreenMismatch), "{res:?}");
    assert!(r.fake.keys_sent().is_empty(), "zero keys");
}

#[tokio::test]
async fn stray_keystroke_carries_its_reason() {
    let (r, rid) = keys_rig(
        "claude-idle-detection.txt",
        "claude-idle-prompt-stray-1.txt",
    )
    .await;
    r.adapter.set_evidence(Arc::new(Fixed(Verdict::Consistent)));
    let (_, mut rx) = r.core.attach(None);
    answer(&r, "a1", &rid, "allow").await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let msg = tokio::time::timeout_at(deadline, rx.recv())
            .await
            .expect("no resolution within 5 s");
        if let Err(tokio::sync::broadcast::error::RecvError::Closed) = msg {
            panic!("event channel closed before the resolution");
        }
        if let Ok(ServerMessage::RequestResolved {
            request_id,
            by,
            reason,
            ..
        }) = msg
            && request_id == rid
        {
            assert_eq!(
                (by, reason),
                (ResolvedBy::Uncertain, Some(ResolveReason::StrayKeystroke))
            );
            break;
        }
    }
}

#[tokio::test]
async fn failed_prompt_check_read_is_uncertain() {
    let (r, rid) = keys_rig("claude-idle-detection.txt", "claude-idle-prompt.txt").await;
    r.fake.fail_next_read("visible", "internal_error");
    r.adapter.set_evidence(Arc::new(Fixed(Verdict::Consistent)));
    let (_, mut rx) = r.core.attach(None);
    answer(&r, "a1", &rid, "allow").await;
    assert_eq!(
        resolution(&mut rx).await.0,
        ResolvedBy::Uncertain,
        "an unavailable prompt check counts like a stray"
    );
}

#[tokio::test]
async fn truncated_prompt_check_read_is_uncertain() {
    let (r, rid) = keys_rig("claude-idle-detection.txt", "claude-idle-prompt.txt").await;
    r.fake.on_keys(|f, pane, _| {
        f.set_screen(pane, "detection", &screen("claude-idle-detection.txt"));
        f.set_screen(pane, "visible", &screen("claude-idle-prompt.txt"));
        f.set_truncated(pane, "visible", true);
    });
    r.adapter.set_evidence(Arc::new(Fixed(Verdict::Consistent)));
    let (_, mut rx) = r.core.attach(None);
    answer(&r, "a1", &rid, "allow").await;
    assert_eq!(resolution(&mut rx).await.0, ResolvedBy::Uncertain);
}

#[tokio::test]
async fn unknown_send_keys_error_is_uncertain() {
    let (r, rid) = keys_rig("claude-idle-detection.txt", "claude-idle-prompt.txt").await;
    r.fake.fail_next("agent.send_keys", "internal_error");
    r.adapter.set_evidence(Arc::new(Fixed(Verdict::Consistent)));
    let (_, mut rx) = r.core.attach(None);
    assert!(delivered(&answer(&r, "a1", &rid, "allow").await));
    assert_eq!(resolution(&mut rx).await, (ResolvedBy::Uncertain, None));
    assert!(r.core.request(&rid).is_none());
    assert!(
        r.adapter.keys_targets().get(&rid).is_some(),
        "the occurrence stays monitored"
    );
}
