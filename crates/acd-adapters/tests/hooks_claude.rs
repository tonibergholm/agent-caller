mod common;
use acd_adapters::herdr::fake::fake_agent;
use acd_core::{CorrelationKey, Observation};
use acd_protocol::*;
use common::*;
use serde_json::json;
use std::time::Duration;

#[tokio::test]
async fn a_hooks_only_session_appears_on_its_first_hook() {
    let r = rig(&[], |_| {}).await;
    r.hook(
        "claude",
        "UserPromptSubmit",
        &fx("claude/payload-UserPromptSubmit.json"),
        &CLI,
    )
    .await;
    let a = until("agent", || r.agent(&format!("claude:{SID}"))).await;
    assert_eq!(
        (a.source, a.kind, a.state, a.can_send),
        (Source::Hooks, AgentKind::Claude, AgentState::Working, false)
    );
    assert_eq!(a.project.as_deref(), Some("claude1"));
}
#[tokio::test]
async fn stop_sets_done_and_caps_the_last_message() {
    let r = rig(&[], |_| {}).await;
    let mut stop = fx("claude/payload-Stop.json");
    stop["last_assistant_message"] = "x".repeat(900).into();
    r.hook("claude", "Stop", &stop, &CLI).await;
    let a = until("done", || {
        r.agent(&format!("claude:{SID}"))
            .filter(|a| a.state == AgentState::Done)
    })
    .await;
    assert_eq!(a.last_message.unwrap().chars().count(), 500);
}
#[tokio::test]
async fn an_ignored_entrypoint_without_a_binding_is_not_shown() {
    let r = rig(&[], |_| {}).await;
    r.hook(
        "claude",
        "UserPromptSubmit",
        &fx("claude/payload-UserPromptSubmit.json"),
        &[("X-Claude-Entrypoint", "sdk-ts")],
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(r.agent(&format!("claude:{SID}")).is_none());
}
#[tokio::test]
async fn subagent_payloads_are_dropped() {
    let r = rig(&[], |_| {}).await;
    let mut p = fx("claude/payload-UserPromptSubmit.json");
    p["agent_id"] = "sub-1".into();
    r.hook("claude", "UserPromptSubmit", &p, &CLI).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(r.agent(&format!("claude:{SID}")).is_none());
}
#[tokio::test]
async fn a_t3_owned_session_is_dropped_and_comes_back_when_t3_lets_go() {
    let r = rig(&[], |_| {}).await;
    r.hook(
        "claude",
        "UserPromptSubmit",
        &fx("claude/payload-UserPromptSubmit.json"),
        &CLI,
    )
    .await;
    until("agent", || r.agent(&format!("claude:{SID}"))).await;
    r.t3.set_for_tests("t3:thread-1", Some(SID));
    until("dropped", || {
        r.agent(&format!("claude:{SID}")).is_none().then_some(())
    })
    .await;
    r.hook(
        "claude",
        "UserPromptSubmit",
        &fx("claude/payload-UserPromptSubmit.json"),
        &CLI,
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        r.agent(&format!("claude:{SID}")).is_none(),
        "T3 still owns it"
    );
    r.t3.set_for_tests("t3:thread-1", None);
    r.hook(
        "claude",
        "UserPromptSubmit",
        &fx("claude/payload-UserPromptSubmit.json"),
        &CLI,
    )
    .await;
    until("back", || r.agent(&format!("claude:{SID}"))).await;
}
fn registry_entry(r: &Rig, status: &str) {
    let mut e = fx("claude/session-registry-entry.json");
    e["sessionId"] = SID.into();
    e["entrypoint"] = "cli".into();
    e["status"] = status.into();
    let p = r.home.path().join(".claude/sessions/14517.json");
    std::fs::write(&p, e.to_string()).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
}
#[tokio::test]
async fn the_registry_scan_creates_an_agent_before_any_hook_and_never_clobbers_done() {
    let r = rig(&[], |_| {}).await;
    registry_entry(&r, "busy");
    let a = until("scanned agent", || r.agent(&format!("claude:{SID}"))).await;
    assert_eq!(a.state, AgentState::Working);
    assert_eq!(a.title, "s4-de");
    r.hook("claude", "Stop", &fx("claude/payload-Stop.json"), &CLI)
        .await;
    until("done", || {
        r.agent(&format!("claude:{SID}"))
            .filter(|a| a.state == AgentState::Done)
    })
    .await;
    registry_entry(&r, "idle");
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        r.agent(&format!("claude:{SID}")).unwrap().state,
        AgentState::Done,
        "D11"
    );
}
#[tokio::test]
async fn session_end_removes_the_agent_after_the_grace() {
    let r = rig(&[], |_| {}).await;
    r.hook(
        "claude",
        "UserPromptSubmit",
        &fx("claude/payload-UserPromptSubmit.json"),
        &CLI,
    )
    .await;
    until("agent", || r.agent(&format!("claude:{SID}"))).await;
    let mut end = fx("claude/payload-Stop.json");
    end["hook_event_name"] = "SessionEnd".into();
    r.hook("claude", "SessionEnd", &end, &CLI).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        r.agent(&format!("claude:{SID}")).is_some(),
        "kept during the grace"
    );
    until("removed", || {
        r.agent(&format!("claude:{SID}")).is_none().then_some(())
    })
    .await;
}
#[tokio::test]
async fn a_binding_that_appears_later_migrates_the_agent() {
    let mut pane = fake_agent("w1:p1", "claude", "idle");
    let r = rig(&[pane.clone()], |_| {}).await;
    r.hook(
        "claude",
        "UserPromptSubmit",
        &fx("claude/payload-UserPromptSubmit.json"),
        &CLI,
    )
    .await;
    until("hooks-only", || r.agent(&format!("claude:{SID}"))).await;
    // A pending request only migration carries over (same id, new agent).
    let hooks_id = format!("claude:{SID}");
    let key = CorrelationKey("k1".into());
    r.core
        .observe(Observation::RequestSeen(acd_core::testing::permission_obs(
            &hooks_id, "k1",
        )));
    let rid = r
        .core
        .request_id_for(&hooks_id, &key)
        .expect("request on the hooks-only agent");
    let herdr_id = r
        .herdr
        .as_ref()
        .unwrap()
        .panes()
        .read()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    pane["agent_session"] = json!({"value": SID});
    r.fake.as_ref().unwrap().set_agent(pane);
    r.fake
        .as_ref()
        .unwrap()
        .push_event(json!({"event": "pane_updated", "data": {"pane_id": "w1:p1"}}));
    until("pane session", || {
        r.herdr
            .as_ref()
            .unwrap()
            .panes()
            .read()
            .unwrap()
            .values()
            .find_map(|p| p.session.clone())
    })
    .await;
    r.hook(
        "claude",
        "UserPromptSubmit",
        &fx("claude/payload-UserPromptSubmit.json"),
        &CLI,
    )
    .await;
    until("migrated", || {
        r.agent(&format!("claude:{SID}")).is_none().then_some(())
    })
    .await;
    assert_eq!(
        r.core.request_id_for(&herdr_id, &key),
        Some(rid.clone()),
        "the pending request moved to the herdr agent with its id"
    );
    assert_eq!(r.core.request(&rid).unwrap().agent_id, herdr_id);
}

fn hooks_only_id() -> String {
    format!("claude:{SID}")
}

#[tokio::test]
async fn a_header_bound_session_stays_bound_across_scans() {
    // The pane does not report agent_session: only the headers bind.
    let r = rig(&[fake_agent("w1:p1", "claude", "idle")], |_| {}).await;
    let herdr_id = until("pane", || {
        r.herdr
            .as_ref()
            .unwrap()
            .panes()
            .read()
            .unwrap()
            .keys()
            .next()
            .cloned()
    })
    .await;
    until("herdr agent", || r.agent(&herdr_id)).await;
    let sock = r.sock();
    registry_entry(&r, "busy");
    r.hook(
        "claude",
        "UserPromptSubmit",
        &fx("claude/payload-UserPromptSubmit.json"),
        &[
            ("X-Claude-Entrypoint", "cli"),
            ("X-Herdr-Pane", "w1:p1"),
            ("X-Herdr-Socket", &sock),
        ],
    )
    .await;
    // Several scan intervals (100 ms) and tick cycles.
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(
        r.agent(&hooks_only_id()).is_none(),
        "no duplicate hooks-only agent"
    );
    assert!(r.agent(&herdr_id).is_some());
}

#[tokio::test]
async fn activity_after_session_end_cancels_the_removal() {
    let r = rig(&[], |_| {}).await;
    let up = fx("claude/payload-UserPromptSubmit.json");
    r.hook("claude", "UserPromptSubmit", &up, &CLI).await;
    until("agent", || r.agent(&hooks_only_id())).await;
    let mut end = fx("claude/payload-Stop.json");
    end["hook_event_name"] = "SessionEnd".into();
    r.hook("claude", "SessionEnd", &end, &CLI).await;
    r.hook("claude", "UserPromptSubmit", &up, &CLI).await;
    // end_grace is 200 ms.
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(
        r.agent(&hooks_only_id()).is_some(),
        "later activity clears the end mark"
    );
}
