mod common;
use acd_adapters::herdr::fake::fake_agent;
use acd_protocol::*;
use common::*;
use serde_json::{Value, json};
use std::time::Duration;

const CODEX_SID: &str = "01a116d3-e8a3-7ef0-9f57-79c5413ed009";
fn screen(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../../tests/fixtures/herdr/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}
fn permission(cwd: &str) -> Value {
    let mut p = fx("codex/payload-PermissionRequest-Bash.json");
    p["tool_input"]["command"] = "touch ~/acp0_test_t5_1.txt".into();
    p["cwd"] = cwd.into();
    p
}
fn pane(id: &str, cwd: &str, session: Option<&str>) -> Value {
    let mut a = fake_agent(id, "codex", "working");
    a["cwd"] = cwd.into();
    a["foreground_cwd"] = cwd.into();
    if let Some(s) = session {
        a["agent_session"] = json!({"source":"herdr:codex","agent":"codex","kind":"id","value":s});
    }
    a
}
fn agent(r: &Rig, pane: &str) -> String {
    r.herdr
        .as_ref()
        .unwrap()
        .panes()
        .read()
        .unwrap()
        .iter()
        .find(|(_, p)| p.pane_id == pane)
        .unwrap()
        .0
        .clone()
}
async fn answer(r: &Rig, rid: &str, option: &str) -> CommandResult {
    r.core
        .handle(
            "c",
            ClientMessage::RequestAnswer {
                id: "a1".into(),
                request_id: rid.into(),
                revision: None,
                option_id: Some(option.into()),
                text: None,
            },
        )
        .await
}

#[tokio::test]
async fn a_session_bound_codex_approval_is_answerable() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().to_str().unwrap().to_owned();
    let r = rig(&[pane("w1:p1", &cwd, Some(CODEX_SID))], |_| {}).await;
    r.fake.as_ref().unwrap().set_screen(
        "w1:p1",
        "detection",
        &screen("codex-permission-bash-detection.txt"),
    );
    let a = agent(&r, "w1:p1");
    assert_eq!(
        r.hook("codex", "PermissionRequest", &permission(&cwd), &[])
            .await,
        (200, String::new())
    );
    let rid = until("request", || {
        r.agent(&a).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    until("target", || {
        r.herdr.as_ref().unwrap().keys_targets().get(&rid)
    })
    .await;
    let req = r.core.request(&rid).unwrap();
    assert_eq!((req.answer_path, req.options.len()), (AnswerPath::Keys, 2));
    assert!(answer(&r, &rid, "deny").await.ok);
    assert_eq!(r.fake.as_ref().unwrap().keys_sent().len(), 1);
}

#[tokio::test]
async fn the_only_codex_pane_in_cwd_binds_across_symlinks() {
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let r = rig(&[pane("w1:p1", link.to_str().unwrap(), None)], |_| {}).await;
    r.fake.as_ref().unwrap().set_screen(
        "w1:p1",
        "detection",
        &screen("codex-permission-bash-detection.txt"),
    );
    let a = agent(&r, "w1:p1");
    r.hook(
        "codex",
        "PermissionRequest",
        &permission(real.to_str().unwrap()),
        &[],
    )
    .await;
    until("request", || r.agent(&a).and_then(|a| a.pending)).await;
}

#[tokio::test]
async fn stale_env_and_two_panes_in_one_cwd_stay_read_only() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().to_str().unwrap().to_owned();
    let other = tempfile::tempdir().unwrap();
    let r = rig(
        &[
            pane("w1:p1", &cwd, None),
            pane("w1:p2", &cwd, None),
            pane("w1:p3", other.path().to_str().unwrap(), None),
        ],
        |_| {},
    )
    .await;
    let dialog = screen("codex-permission-bash-detection.txt");
    for p in ["w1:p1", "w1:p2", "w1:p3"] {
        r.fake.as_ref().unwrap().set_screen(p, "detection", &dialog);
    }
    let sock = r.sock();
    r.hook(
        "codex",
        "PermissionRequest",
        &permission(&cwd),
        &[("X-Herdr-Pane", "w1:p3"), ("X-Herdr-Socket", &sock)],
    )
    .await;
    tokio::time::sleep(Duration::from_millis(900)).await;
    for p in ["w1:p1", "w1:p2", "w1:p3"] {
        assert!(
            r.agent(&agent(&r, p))
                .and_then(|a| a.pending)
                .is_none_or(|q| q.kind == RequestKind::BlockedUnparsed),
            "{p}"
        );
    }
    assert!(r.fake.as_ref().unwrap().keys_sent().is_empty());
}

#[tokio::test]
async fn session_pane_that_never_shows_dialog_gets_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().to_str().unwrap().to_owned();
    let r = rig(&[pane("w1:p1", &cwd, Some(CODEX_SID))], |_| {}).await;
    r.fake
        .as_ref()
        .unwrap()
        .set_screen("w1:p1", "detection", &screen("codex-idle-detection.txt"));
    r.hook("codex", "PermissionRequest", &permission(&cwd), &[])
        .await;
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert!(
        r.agent(&agent(&r, "w1:p1"))
            .and_then(|a| a.pending)
            .is_none()
    );
}

#[tokio::test]
async fn truncated_rival_keeps_request_from_opening() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().to_str().unwrap().to_owned();
    let d = tempfile::tempdir().unwrap();
    let r = rig(
        &[
            pane("w1:p1", &cwd, Some(CODEX_SID)),
            pane("w1:p2", d.path().to_str().unwrap(), None),
        ],
        |_| {},
    )
    .await;
    let f = r.fake.as_ref().unwrap();
    f.set_screen(
        "w1:p1",
        "detection",
        &screen("codex-permission-bash-detection.txt"),
    );
    f.set_screen("w1:p2", "detection", &screen("codex-idle-detection.txt"));
    f.set_truncated("w1:p2", "detection", true);
    r.hook("codex", "PermissionRequest", &permission(&cwd), &[])
        .await;
    tokio::time::sleep(Duration::from_millis(900)).await;
    let pending = r.agent(&agent(&r, "w1:p1")).and_then(|a| a.pending);
    assert!(pending.is_none_or(|p| p.kind == RequestKind::BlockedUnparsed));
}

#[tokio::test]
async fn rival_showing_same_command_stays_read_only() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let r = rig(
        &[
            pane("w1:p1", a.path().to_str().unwrap(), Some(CODEX_SID)),
            pane("w1:p2", b.path().to_str().unwrap(), None),
        ],
        |_| {},
    )
    .await;
    let dialog = screen("codex-permission-bash-detection.txt");
    for p in ["w1:p1", "w1:p2"] {
        r.fake.as_ref().unwrap().set_screen(p, "detection", &dialog);
    }
    r.hook(
        "codex",
        "PermissionRequest",
        &permission(a.path().to_str().unwrap()),
        &[],
    )
    .await;
    tokio::time::sleep(Duration::from_millis(900)).await;
    for p in ["w1:p1", "w1:p2"] {
        assert!(
            r.agent(&agent(&r, p))
                .and_then(|a| a.pending)
                .is_none_or(|q| q.kind == RequestKind::BlockedUnparsed),
            "{p}"
        );
    }
}

#[tokio::test]
async fn t3_takeover_during_bind_read_opens_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().to_str().unwrap().to_owned();
    let r = rig(&[pane("w1:p1", &cwd, Some(CODEX_SID))], |_| {}).await;
    let f = r.fake.as_ref().unwrap();
    f.set_screen(
        "w1:p1",
        "detection",
        &screen("codex-permission-bash-detection.txt"),
    );
    let gate = f.block_next("agent.read");
    r.hook("codex", "PermissionRequest", &permission(&cwd), &[])
        .await;
    gate.arrived().await;
    r.t3.set_for_tests("t3:thread-9", Some(CODEX_SID));
    tokio::time::sleep(Duration::from_millis(100)).await;
    gate.release();
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert!(
        r.agent(&agent(&r, "w1:p1"))
            .and_then(|a| a.pending)
            .is_none()
    );
    assert!(f.keys_sent().is_empty());
}

#[tokio::test]
async fn later_codex_event_supersedes_bind_read() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().to_str().unwrap().to_owned();
    let r = rig(&[pane("w1:p1", &cwd, Some(CODEX_SID))], |_| {}).await;
    let f = r.fake.as_ref().unwrap();
    f.set_screen(
        "w1:p1",
        "detection",
        &screen("codex-permission-bash-detection.txt"),
    );
    let gate = f.block_next("agent.read");
    r.hook("codex", "PermissionRequest", &permission(&cwd), &[])
        .await;
    gate.arrived().await;
    let mut stop = fx("codex/payload-UserPromptSubmit.json");
    stop["hook_event_name"] = "Stop".into();
    r.hook("codex", "Stop", &stop, &[]).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    gate.release();
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert!(
        r.agent(&agent(&r, "w1:p1"))
            .and_then(|a| a.pending)
            .is_none()
    );
}

#[tokio::test]
async fn collapsed_dialog_becomes_answerable_after_enlarging() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().to_str().unwrap().to_owned();
    let r = rig(&[pane("w1:p1", &cwd, Some(CODEX_SID))], |_| {}).await;
    let f = r.fake.as_ref().unwrap();
    f.set_screen(
        "w1:p1",
        "detection",
        &screen("codex-permission-collapsed-small-pane-detection.txt"),
    );
    let a = agent(&r, "w1:p1");
    r.hook("codex", "PermissionRequest", &permission(&cwd), &[])
        .await;
    let rid = until("collapsed request", || {
        r.agent(&a).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    assert!(
        r.core
            .request(&rid)
            .unwrap()
            .detail
            .ends_with("Enlarge the pane to answer here.")
    );
    assert_eq!(
        answer(&r, &rid, "allow").await.error_code(),
        Some(ErrorCode::RequestNotReady)
    );
    f.set_screen(
        "w1:p1",
        "detection",
        &screen("codex-permission-bash-detection.txt"),
    );
    until("target after enlargement", || {
        r.herdr.as_ref().unwrap().keys_targets().get(&rid)
    })
    .await;
    assert!(!r.core.request(&rid).unwrap().detail.contains("Enlarge"));
}

#[tokio::test]
async fn rival_added_during_bind_read_prevents_opening() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().to_str().unwrap().to_owned();
    let r = rig(&[pane("w1:p1", &cwd, Some(CODEX_SID))], |_| {}).await;
    let f = r.fake.as_ref().unwrap();
    let dialog = screen("codex-permission-bash-detection.txt");
    f.set_screen("w1:p1", "detection", &dialog);
    let gate = f.block_next("agent.read");
    r.hook("codex", "PermissionRequest", &permission(&cwd), &[])
        .await;
    gate.arrived().await;
    let d = tempfile::tempdir().unwrap();
    f.set_agent(pane("w1:p2", d.path().to_str().unwrap(), None));
    f.set_screen("w1:p2", "detection", &dialog);
    until("rival pane", || {
        (r.herdr.as_ref().unwrap().panes().read().unwrap().len() == 2).then_some(())
    })
    .await;
    gate.release();
    tokio::time::sleep(Duration::from_millis(900)).await;
    let pending = r.agent(&agent(&r, "w1:p1")).and_then(|a| a.pending);
    assert!(pending.is_none_or(|p| p.kind == RequestKind::BlockedUnparsed));
    assert!(f.keys_sent().is_empty());
}

async fn resolution(
    rx: &mut tokio::sync::broadcast::Receiver<ServerMessage>,
    rid: &str,
) -> ResolvedBy {
    let wait = async {
        loop {
            if let ServerMessage::RequestResolved { request_id, by, .. } = rx.recv().await.unwrap()
                && request_id == rid
            {
                return by;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(10), wait)
        .await
        .unwrap_or_else(|_| panic!("{rid} did not resolve"))
}

/// p1 (session-bound) is ready with a target; p2 is a Codex pane in another
/// cwd showing `p2_screen`. Returns the rig, the request id and p2's dir.
async fn ready_with_rival(p2_screen: &str) -> (Rig, String, tempfile::TempDir, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().to_str().unwrap().to_owned();
    let d = tempfile::tempdir().unwrap();
    let r = rig(
        &[
            pane("w1:p1", &cwd, Some(CODEX_SID)),
            pane("w1:p2", d.path().to_str().unwrap(), None),
        ],
        |_| {},
    )
    .await;
    let f = r.fake.as_ref().unwrap();
    f.set_screen(
        "w1:p1",
        "detection",
        &screen("codex-permission-bash-detection.txt"),
    );
    f.set_screen("w1:p2", "detection", &screen(p2_screen));
    let a = agent(&r, "w1:p1");
    r.hook("codex", "PermissionRequest", &permission(&cwd), &[])
        .await;
    let rid = until("request", || {
        r.agent(&a).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    until("target", || {
        r.herdr.as_ref().unwrap().keys_targets().get(&rid)
    })
    .await;
    (r, rid, tmp, d)
}

#[tokio::test]
async fn a_live_target_refuses_while_a_rival_now_shows_the_command() {
    // C7 (Sol T17 Critical): p2's screen changes to the same command after
    // p1's target is live; its id and incarnation do not change.
    let (r, rid, _a, _b) = ready_with_rival("codex-idle-detection.txt").await;
    let f = r.fake.as_ref().unwrap();
    f.set_screen(
        "w1:p2",
        "detection",
        &screen("codex-permission-bash-detection.txt"),
    );
    assert_eq!(
        answer(&r, &rid, "allow").await.error_code(),
        Some(ErrorCode::RequestNotReady)
    );
    assert!(f.keys_sent().is_empty());
}

#[tokio::test]
async fn an_unreadable_or_truncated_rival_refuses_the_key() {
    let (r, rid, _a, _b) = ready_with_rival("codex-idle-detection.txt").await;
    let f = r.fake.as_ref().unwrap();
    f.set_read_failing("w1:p2", "detection", true);
    assert_eq!(
        answer(&r, &rid, "allow").await.error_code(),
        Some(ErrorCode::RequestNotReady)
    );
    f.set_read_failing("w1:p2", "detection", false);
    f.set_truncated("w1:p2", "detection", true);
    assert_eq!(
        answer(&r, &rid, "allow").await.error_code(),
        Some(ErrorCode::RequestNotReady)
    );
    assert!(f.keys_sent().is_empty());
    // Both refusals are retryable: a clean rival read lets the key through.
    f.set_truncated("w1:p2", "detection", false);
    assert!(answer(&r, &rid, "allow").await.ok);
    assert_eq!(f.keys_sent().len(), 1);
}

#[tokio::test]
async fn a_changed_rival_inventory_refuses_as_not_ready() {
    // C6: a Codex pane added after validation; the lease refuses with
    // RequestNotReady (retryable), not RequestGone.
    let (r, rid, _a, _b) = ready_with_rival("codex-idle-detection.txt").await;
    // Without ticks no C5 recheck restores readiness: only the lease decides.
    r.hooks.pause_ticks_for_tests(true);
    let f = r.fake.as_ref().unwrap();
    let d = tempfile::tempdir().unwrap();
    f.set_agent(pane("w1:p3", d.path().to_str().unwrap(), None));
    f.set_screen("w1:p3", "detection", &screen("codex-idle-detection.txt"));
    until("p3 listed", || {
        (r.herdr.as_ref().unwrap().panes().read().unwrap().len() == 3).then_some(())
    })
    .await;
    assert_eq!(
        answer(&r, &rid, "allow").await.error_code(),
        Some(ErrorCode::RequestNotReady)
    );
    assert!(f.keys_sent().is_empty());
}

fn answered_screens(f: &acd_adapters::herdr::fake::FakeHerdr) {
    f.on_keys(|f, pane, _| {
        f.set_screen(pane, "detection", &screen("codex-idle-detection.txt"));
        f.set_screen(pane, "visible", &screen("codex-idle-prompt.txt"));
    });
}

#[tokio::test]
async fn an_allowed_command_that_runs_resolves_inbox() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().to_str().unwrap().to_owned();
    let r = rig(&[pane("w1:p1", &cwd, Some(CODEX_SID))], |_| {}).await;
    let f = r.fake.as_ref().unwrap();
    f.set_screen(
        "w1:p1",
        "detection",
        &screen("codex-permission-bash-detection.txt"),
    );
    answered_screens(f);
    let (_, mut rx) = r.core.attach(None);
    let a = agent(&r, "w1:p1");
    r.hook("codex", "PermissionRequest", &permission(&cwd), &[])
        .await;
    let rid = until("request", || {
        r.agent(&a).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    until("target", || {
        r.herdr.as_ref().unwrap().keys_targets().get(&rid)
    })
    .await;
    assert!(answer(&r, &rid, "allow").await.ok);
    let mut post = fx("codex/payload-PostToolUse-Bash.json");
    post["tool_input"]["command"] = "touch ~/acp0_test_t5_1.txt".into();
    post["cwd"] = cwd.as_str().into();
    r.hook("codex", "PostToolUse", &post, &[]).await;
    assert_eq!(resolution(&mut rx, &rid).await, ResolvedBy::Inbox);
}

/// Answers p1's Codex approval with `option`, then sends a Stop with no
/// PostToolUse, and returns how the request resolved.
async fn stop_without_running_after(option: &str) -> ResolvedBy {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().to_str().unwrap().to_owned();
    let r = rig(&[pane("w1:p1", &cwd, Some(CODEX_SID))], |_| {}).await;
    let f = r.fake.as_ref().unwrap();
    f.set_screen(
        "w1:p1",
        "detection",
        &screen("codex-permission-bash-detection.txt"),
    );
    answered_screens(f);
    let (_, mut rx) = r.core.attach(None);
    let a = agent(&r, "w1:p1");
    r.hook("codex", "PermissionRequest", &permission(&cwd), &[])
        .await;
    let rid = until("request", || {
        r.agent(&a).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    until("target", || {
        r.herdr.as_ref().unwrap().keys_targets().get(&rid)
    })
    .await;
    assert!(answer(&r, &rid, option).await.ok);
    let mut stop = fx("codex/payload-UserPromptSubmit.json");
    stop["hook_event_name"] = "Stop".into();
    stop["cwd"] = cwd.as_str().into();
    r.hook("codex", "Stop", &stop, &[]).await;
    resolution(&mut rx, &rid).await
}

#[tokio::test]
async fn a_denied_command_that_stops_without_running_resolves_inbox() {
    // Spec §6 attribution: deny + Stop with no matching PostToolUse.
    assert_eq!(stop_without_running_after("deny").await, ResolvedBy::Inbox);
}

#[tokio::test]
async fn an_allowed_command_that_never_runs_resolves_terminal() {
    // Contradicting outcome: acd allowed, the tool never ran.
    assert_eq!(
        stop_without_running_after("allow").await,
        ResolvedBy::Terminal
    );
}

#[tokio::test]
async fn a_repeated_dialog_opens_a_new_occurrence_with_a_fresh_target() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().to_str().unwrap().to_owned();
    let r = rig(&[pane("w1:p1", &cwd, Some(CODEX_SID))], |_| {}).await;
    r.fake.as_ref().unwrap().set_screen(
        "w1:p1",
        "detection",
        &screen("codex-permission-bash-detection.txt"),
    );
    let a = agent(&r, "w1:p1");
    let (_, mut rx) = r.core.attach(None);
    r.hook("codex", "PermissionRequest", &permission(&cwd), &[])
        .await;
    let r1 = until("first", || {
        r.agent(&a).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    until("first target", || {
        r.herdr.as_ref().unwrap().keys_targets().get(&r1)
    })
    .await;
    // Answered in the terminal and asked again before the watch saw it go.
    r.hook("codex", "PermissionRequest", &permission(&cwd), &[])
        .await;
    assert_eq!(resolution(&mut rx, &r1).await, ResolvedBy::Terminal);
    let r2 = until("second", || {
        r.agent(&a)
            .and_then(|a| a.pending)
            .map(|p| p.id)
            .filter(|id| *id != r1)
    })
    .await;
    until("second target", || {
        r.herdr.as_ref().unwrap().keys_targets().get(&r2)
    })
    .await;
    assert!(r.herdr.as_ref().unwrap().keys_targets().get(&r1).is_none());
    assert!(answer(&r, &r2, "allow").await.ok);
    assert_eq!(r.fake.as_ref().unwrap().keys_sent().len(), 1);
}

#[tokio::test]
async fn an_enlarged_dialog_with_a_rival_showing_the_command_gets_no_target() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().to_str().unwrap().to_owned();
    let d = tempfile::tempdir().unwrap();
    let r = rig(
        &[
            pane("w1:p1", &cwd, Some(CODEX_SID)),
            pane("w1:p2", d.path().to_str().unwrap(), None),
        ],
        |_| {},
    )
    .await;
    let f = r.fake.as_ref().unwrap();
    f.set_screen(
        "w1:p1",
        "detection",
        &screen("codex-permission-collapsed-small-pane-detection.txt"),
    );
    f.set_screen("w1:p2", "detection", &screen("codex-idle-detection.txt"));
    let a = agent(&r, "w1:p1");
    r.hook("codex", "PermissionRequest", &permission(&cwd), &[])
        .await;
    let rid = until("collapsed request", || {
        r.agent(&a).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    // Enlarged, but another pane now shows the same command (C1: same path).
    let dialog = screen("codex-permission-bash-detection.txt");
    f.set_screen("w1:p2", "detection", &dialog);
    f.set_screen("w1:p1", "detection", &dialog);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(r.herdr.as_ref().unwrap().keys_targets().get(&rid).is_none());
    assert_eq!(
        answer(&r, &rid, "allow").await.error_code(),
        Some(ErrorCode::RequestNotReady)
    );
    assert!(f.keys_sent().is_empty());
}

// Task 18: ownership transitions, C5 and C6.

async fn live_codex_target(r: &Rig, cwd: &str, p: &str) -> String {
    r.fake.as_ref().unwrap().set_screen(
        p,
        "detection",
        &screen("codex-permission-bash-detection.txt"),
    );
    let a = agent(r, p);
    r.hook("codex", "PermissionRequest", &permission(cwd), &[])
        .await;
    let rid = until("request", || {
        r.agent(&a).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    until("target", || {
        r.herdr.as_ref().unwrap().keys_targets().get(&rid)
    })
    .await;
    rid
}

async fn revoked(r: &Rig, rx: &mut tokio::sync::broadcast::Receiver<ServerMessage>, rid: &str) {
    assert_eq!(resolution(rx, rid).await, ResolvedBy::Lost);
    assert!(
        r.herdr.as_ref().unwrap().keys_targets().get(rid).is_none(),
        "target retired"
    );
    assert!(r.fake.as_ref().unwrap().keys_sent().is_empty());
}

async fn add_matching_rival(r: &Rig, other: &std::path::Path) {
    let f = r.fake.as_ref().unwrap();
    f.set_screen(
        "w1:p2",
        "detection",
        &screen("codex-permission-bash-detection.txt"),
    );
    f.set_agent(pane("w1:p2", other.to_str().unwrap(), None));
    // Wait for the rival itself, not a pane count (tests may have other panes).
    until("rival listed", || {
        r.herdr
            .as_ref()
            .unwrap()
            .panes()
            .read()
            .unwrap()
            .values()
            .any(|p| p.pane_id == "w1:p2")
            .then_some(())
    })
    .await;
}

/// Answers `rid` while every read of p1 fails. An answer refused by core's
/// readiness check (C5) never reaches herdr and is `RequestNotReady`; one
/// that reaches delivery fails its guard read (`SourceUnavailable`) first.
async fn answer_refused_by_readiness(r: &Rig, rid: &str) -> Option<ErrorCode> {
    let f = r.fake.as_ref().unwrap();
    f.set_read_failing("w1:p1", "detection", true);
    let code = answer(r, rid, "allow").await.error_code();
    f.set_read_failing("w1:p1", "detection", false);
    code
}

#[tokio::test]
async fn t3_takeover_revokes_a_codex_request() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().to_str().unwrap().to_owned();
    let r = rig(&[pane("w1:p1", &cwd, Some(CODEX_SID))], |_| {}).await;
    let (_, mut rx) = r.core.attach(None);
    let rid = live_codex_target(&r, &cwd, "w1:p1").await;
    r.t3.set_for_tests("t3:thread-9", Some(CODEX_SID));
    revoked(&r, &mut rx, &rid).await;
    assert!(r.agent(&agent(&r, "w1:p1")).is_some());
    assert_eq!(
        answer(&r, &rid, "allow").await.error_code(),
        Some(ErrorCode::RequestGone)
    );
    assert!(r.fake.as_ref().unwrap().keys_sent().is_empty());
}

#[tokio::test]
async fn t3_takeover_during_a_rival_recheck_revokes_and_the_late_result_enables_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().to_str().unwrap().to_owned();
    let r = rig(&[pane("w1:p1", &cwd, Some(CODEX_SID))], |_| {}).await;
    let (_, mut rx) = r.core.attach(None);
    let rid = live_codex_target(&r, &cwd, "w1:p1").await;
    let f = r.fake.as_ref().unwrap();
    let gate = f.block_next("agent.read@w1:p2"); // the recheck's rival read
    let other = tempfile::tempdir().unwrap();
    f.set_screen("w1:p2", "detection", &screen("codex-idle-detection.txt"));
    f.set_agent(pane("w1:p2", other.path().to_str().unwrap(), None));
    tokio::time::timeout(Duration::from_secs(5), gate.arrived())
        .await
        .expect("the recheck read reached the gate");
    r.t3.set_for_tests("t3:thread-9", Some(CODEX_SID));
    revoked(&r, &mut rx, &rid).await;
    gate.release();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(r.herdr.as_ref().unwrap().keys_targets().get(&rid).is_none());
    assert!(
        r.agent(&agent(&r, "w1:p1"))
            .and_then(|a| a.pending)
            .is_none()
    );
    assert!(f.keys_sent().is_empty());
}

#[tokio::test]
async fn moving_the_codex_session_to_another_pane_revokes_the_live_target() {
    // Codex Bound(a) -> Bound(b).
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().to_str().unwrap().to_owned();
    let d2 = tempfile::tempdir().unwrap();
    let p2 = pane("w1:p2", d2.path().to_str().unwrap(), None);
    let r = rig(&[pane("w1:p1", &cwd, Some(CODEX_SID)), p2], |_| {}).await;
    let f = r.fake.as_ref().unwrap();
    f.set_screen("w1:p2", "detection", &screen("codex-idle-detection.txt"));
    let (_, mut rx) = r.core.attach(None);
    let rid = live_codex_target(&r, &cwd, "w1:p1").await;
    f.set_agent(pane("w1:p1", &cwd, None));
    f.set_agent(pane("w1:p2", d2.path().to_str().unwrap(), Some(CODEX_SID)));
    revoked(&r, &mut rx, &rid).await;
    assert!(
        r.agent(&agent(&r, "w1:p1")).is_some(),
        "the herdr agent stays"
    );
}

#[tokio::test]
async fn a_second_codex_pane_in_the_cwd_revokes_an_only_in_cwd_target() {
    // Codex Bound(a) -> Unbound (ambiguous cwd).
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().to_str().unwrap().to_owned();
    let r = rig(&[pane("w1:p1", &cwd, None)], |_| {}).await;
    let (_, mut rx) = r.core.attach(None);
    let rid = live_codex_target(&r, &cwd, "w1:p1").await;
    r.fake
        .as_ref()
        .unwrap()
        .set_agent(pane("w1:p2", &cwd, None));
    revoked(&r, &mut rx, &rid).await;
}

#[tokio::test]
async fn a_new_rival_showing_the_command_revokes_the_live_target() {
    // C5: the binding is no longer unambiguous.
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().to_str().unwrap().to_owned();
    let r = rig(&[pane("w1:p1", &cwd, Some(CODEX_SID))], |_| {}).await;
    let (_, mut rx) = r.core.attach(None);
    let rid = live_codex_target(&r, &cwd, "w1:p1").await;
    let other = tempfile::tempdir().unwrap();
    add_matching_rival(&r, other.path()).await;
    revoked(&r, &mut rx, &rid).await;
}

#[tokio::test]
async fn a_new_rival_that_does_not_show_the_command_keeps_the_target() {
    // C5: a passing recheck stores the new inventory and restores readiness.
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().to_str().unwrap().to_owned();
    let r = rig(&[pane("w1:p1", &cwd, Some(CODEX_SID))], |_| {}).await;
    let rid = live_codex_target(&r, &cwd, "w1:p1").await;
    let other = tempfile::tempdir().unwrap();
    let f = r.fake.as_ref().unwrap();
    f.set_screen("w1:p2", "detection", &screen("codex-idle-detection.txt"));
    f.set_agent(pane("w1:p2", other.path().to_str().unwrap(), None));
    let p2 = until("rival listed", || {
        r.herdr
            .as_ref()
            .unwrap()
            .panes()
            .read()
            .unwrap()
            .iter()
            .find(|(_, p)| p.pane_id == "w1:p2")
            .map(|(id, p)| (id.clone(), p.incarnation))
    })
    .await;
    until("the recheck stored the new inventory", || {
        (r.herdr.as_ref().unwrap().keys_targets().rivals(&rid) == Some(vec![p2.clone()]))
            .then_some(())
    })
    .await;
    assert!(
        r.herdr.as_ref().unwrap().keys_targets().get(&rid).is_some(),
        "the recheck enabled it again"
    );
    assert!(answer(&r, &rid, "allow").await.ok);
    assert_eq!(f.keys_sent().len(), 1);
}

#[tokio::test]
async fn a_live_target_is_not_ready_while_its_rival_recheck_is_blocked() {
    // Sol R3-1: the window between a rival appearing and the recheck result.
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().to_str().unwrap().to_owned();
    let r = rig(&[pane("w1:p1", &cwd, Some(CODEX_SID))], |_| {}).await;
    let rid = live_codex_target(&r, &cwd, "w1:p1").await;
    let f = r.fake.as_ref().unwrap();
    // Holds the recheck's rival read. herdr's own watch polls only p1 (the
    // pane with a target), so it cannot take this gate.
    let gate = f.block_next("agent.read@w1:p2");
    let other = tempfile::tempdir().unwrap();
    add_matching_rival(&r, other.path()).await;
    tokio::time::timeout(Duration::from_secs(5), gate.arrived())
        .await
        .expect("the recheck read reached the gate");
    assert_eq!(
        answer_refused_by_readiness(&r, &rid).await,
        Some(ErrorCode::RequestNotReady),
        "C5: refused by readiness, before any delivery read"
    );
    assert!(
        f.keys_sent().is_empty(),
        "C6: zero keys before the recheck result"
    );
    gate.release();
}

#[tokio::test]
async fn a_live_target_is_not_ready_while_no_read_slot_is_free() {
    // The recheck cannot even start: every read slot is held elsewhere.
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().to_str().unwrap().to_owned();
    // "working": herdr never polls p3 itself, so only the capture reads it.
    let claude = fake_agent("w1:p3", "claude", "working");
    let r = rig(&[pane("w1:p1", &cwd, Some(CODEX_SID)), claude], |c| {
        c.max_reads = 1
    })
    .await;
    let rid = live_codex_target(&r, &cwd, "w1:p1").await;
    // A Claude capture on p3 takes the only slot and blocks.
    let f = r.fake.as_ref().unwrap();
    let dialog = screen("claude-permission-bash.txt");
    f.set_screen("w1:p3", "detection", &dialog);
    let gate = f.block_next("agent.read@w1:p3");
    let sock = r.sock();
    let h = [
        ("X-Herdr-Pane", "w1:p3"),
        ("X-Herdr-Socket", sock.as_str()),
        ("X-Claude-Entrypoint", "cli"),
    ];
    let _held = acd_adapters::hooks::testing::send(
        r.port,
        "/hook/claude/PermissionRequest",
        TOKEN,
        &h,
        &fx("claude/payload-PermissionRequest-Bash.json").to_string(),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), gate.arrived())
        .await
        .expect("the capture read reached the gate");
    let other = tempfile::tempdir().unwrap();
    add_matching_rival(&r, other.path()).await;
    tokio::time::sleep(Duration::from_millis(200)).await; // several ticks: the recheck stays wanted
    assert_eq!(
        answer_refused_by_readiness(&r, &rid).await,
        Some(ErrorCode::RequestNotReady),
        "C5: refused by readiness, before any delivery read"
    );
    assert!(
        f.keys_sent().is_empty(),
        "C6: zero keys with the recheck starved"
    );
    gate.release();
}

#[tokio::test]
async fn the_lease_refuses_before_the_reconciler_notices_a_rival() {
    // No tick runs during the test, so only the lease can catch it (C6).
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().to_str().unwrap().to_owned();
    let r = rig(&[pane("w1:p1", &cwd, Some(CODEX_SID))], |_| {}).await;
    let rid = live_codex_target(&r, &cwd, "w1:p1").await;
    r.hooks.pause_ticks_for_tests(true);
    let other = tempfile::tempdir().unwrap();
    add_matching_rival(&r, other.path()).await;
    let res = answer(&r, &rid, "allow").await;
    assert_eq!(
        res.error_code(),
        Some(ErrorCode::RequestNotReady),
        "{res:?}"
    );
    assert!(r.fake.as_ref().unwrap().keys_sent().is_empty());
    r.hooks.pause_ticks_for_tests(false);
}

#[tokio::test]
async fn a_rival_recheck_that_finds_the_dialog_collapsed_revokes_the_live_target() {
    // C5: a recheck either passes (C3) or revokes; collapsed is not a pass.
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().to_str().unwrap().to_owned();
    let r = rig(&[pane("w1:p1", &cwd, Some(CODEX_SID))], |_| {}).await;
    let (_, mut rx) = r.core.attach(None);
    let rid = live_codex_target(&r, &cwd, "w1:p1").await;
    r.hooks.pause_ticks_for_tests(true);
    let other = tempfile::tempdir().unwrap();
    let f = r.fake.as_ref().unwrap();
    f.set_screen("w1:p2", "detection", &screen("codex-idle-detection.txt"));
    f.set_agent(pane("w1:p2", other.path().to_str().unwrap(), None));
    until("rival listed", || {
        r.herdr
            .as_ref()
            .unwrap()
            .panes()
            .read()
            .unwrap()
            .values()
            .any(|p| p.pane_id == "w1:p2")
            .then_some(())
    })
    .await;
    f.set_screen(
        "w1:p1",
        "detection",
        &screen("codex-permission-collapsed-small-pane-detection.txt"),
    );
    r.hooks.pause_ticks_for_tests(false);
    revoked(&r, &mut rx, &rid).await;
}
