mod common;
use acd_adapters::herdr::fake::fake_agent;
use acd_adapters::hooks::testing::read_response;
use acd_core::{CorrelationKey, Observation};
use acd_protocol::*;
use common::*;
use serde_json::{Value, json};
use std::time::Duration;

fn herdr_screen(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../../tests/fixtures/herdr/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

async fn question_rig() -> (Rig, String) {
    let r = rig(&[fake_agent("w1:p1", "claude", "blocked")], |_| {}).await;
    let q = herdr_screen("claude-askuserquestion-single.txt");
    let f = r.fake.as_ref().unwrap();
    f.set_screen("w1:p1", "detection", &q);
    f.set_screen("w1:p1", "visible", &q);
    f.on_keys(|f, pane, _| {
        let idle = herdr_screen("claude-idle-detection.txt");
        let prompt = herdr_screen("claude-idle-prompt.txt");
        f.set_screen(pane, "detection", &idle);
        f.set_screen(pane, "visible", &prompt);
        f.set_status(pane, "working");
    });
    let agent = r
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
    (r, agent)
}

fn post_answer(label: &str) -> Value {
    let mut p = fx("claude/payload-PostToolUse-AskUserQuestion.json");
    p["tool_response"]["answers"]["Pick a color"] = label.into();
    p["tool_input"]["answers"]["Pick a color"] = label.into();
    p
}

fn pre_question(labels: &[&str]) -> Value {
    let mut p = fx("claude/payload-PreToolUse-AskUserQuestion.json");
    p["tool_input"]["questions"][0]["options"] = labels
        .iter()
        .map(|label| json!({"label": label, "description": "d"}))
        .collect::<Vec<_>>()
        .into();
    p
}

#[tokio::test]
async fn a_single_question_is_answered_with_its_number_key_and_attributed() {
    let (r, agent) = question_rig().await;
    let (_, mut rx) = r.core.attach(None);
    let h = bound_headers(&r);
    let h: Vec<(&str, &str)> = h.iter().map(|(k, v)| (*k, v.as_str())).collect();
    r.hook(
        "claude",
        "PreToolUse",
        &fx("claude/payload-PreToolUse-AskUserQuestion.json"),
        &h,
    )
    .await;
    r.hook(
        "claude",
        "PermissionRequest",
        &fx("claude/payload-PermissionRequest-AskUserQuestion.json"),
        &h,
    )
    .await;
    let rid = until("question", || {
        r.agent(&agent)
            .and_then(|a| a.pending)
            .filter(|p| p.kind == RequestKind::Question)
            .map(|p| p.id)
    })
    .await;
    assert_eq!(r.core.request(&rid).unwrap().answer_path, AnswerPath::Keys);
    until("target", || {
        r.herdr.as_ref().unwrap().keys_targets().get(&rid)
    })
    .await;
    assert!(answer(&r, "a1", &rid, "o1").await.ok);
    assert_eq!(
        r.fake
            .as_ref()
            .unwrap()
            .keys_sent()
            .last()
            .map(|k| k.1.clone()),
        Some(vec!["1".to_string()])
    );
    r.hook("claude", "PostToolUse", &post_answer("Red"), &h)
        .await;
    assert_eq!(resolution(&mut rx, &rid).await, ResolvedBy::Inbox);
}

#[tokio::test]
async fn a_question_answered_with_another_option_is_terminal() {
    let (r, agent) = question_rig().await;
    let (_, mut rx) = r.core.attach(None);
    let h = bound_headers(&r);
    let h: Vec<(&str, &str)> = h.iter().map(|(k, v)| (*k, v.as_str())).collect();
    r.hook(
        "claude",
        "PreToolUse",
        &fx("claude/payload-PreToolUse-AskUserQuestion.json"),
        &h,
    )
    .await;
    let rid = until("question", || {
        r.agent(&agent)
            .and_then(|a| a.pending)
            .filter(|p| p.kind == RequestKind::Question)
            .map(|p| p.id)
    })
    .await;
    until("target", || {
        r.herdr.as_ref().unwrap().keys_targets().get(&rid)
    })
    .await;
    answer(&r, "a1", &rid, "o1").await;
    r.hook("claude", "PostToolUse", &post_answer("Blue"), &h)
        .await;
    assert_eq!(resolution(&mut rx, &rid).await, ResolvedBy::Terminal);
}

#[tokio::test]
async fn a_changed_option_set_shadows_the_older_question_answer() {
    let r = rig(&[], |_| {}).await;
    r.hook(
        "claude",
        "PreToolUse",
        &pre_question(&["Red", "Blue"]),
        &CLI,
    )
    .await;
    let q1 = until("first question", || {
        r.agent(HOOKS_ONLY).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    r.hook(
        "claude",
        "PreToolUse",
        &pre_question(&["Blue", "Green", "Red"]),
        &CLI,
    )
    .await;
    let q2 = until("changed question", || {
        r.agent(HOOKS_ONLY)
            .and_then(|a| a.pending)
            .map(|p| p.id)
            .filter(|id| *id != q1)
    })
    .await;
    r.hook("claude", "PostToolUse", &post_answer("Red"), &CLI)
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        r.core.request(&q2).map(|q| q.status),
        Some(RequestStatus::Open)
    );
}

#[tokio::test]
async fn an_unrelated_question_outcome_does_not_resolve_or_consume_evidence() {
    let r = rig(&[], |_| {}).await;
    r.hook(
        "claude",
        "PreToolUse",
        &pre_question(&["Red", "Blue"]),
        &CLI,
    )
    .await;
    let q = until("question", || {
        r.agent(HOOKS_ONLY).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    let mut other = post_answer("Red");
    other["tool_response"]["answers"] = json!({"A different question?": "Yes"});
    other["tool_input"]["answers"] = json!({"A different question?": "Yes"});
    r.hook("claude", "PostToolUse", &other, &CLI).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        r.core.request(&q).map(|q| q.status),
        Some(RequestStatus::Open)
    );
    let mut matching = post_answer("Blue");
    let questions = pre_question(&["Red", "Blue"])["tool_input"]["questions"].clone();
    matching["tool_input"]["questions"] = questions.clone();
    matching["tool_response"]["questions"] = questions;
    let (_, mut rx) = r.core.attach(None);
    r.hook("claude", "PostToolUse", &matching, &CLI).await;
    assert_eq!(resolution(&mut rx, &q).await, ResolvedBy::Terminal);
}

#[tokio::test]
async fn an_uncaptured_question_is_attributed_by_hooks_only() {
    let r = rig(&[fake_agent("w1:p1", "claude", "blocked")], |_| {}).await;
    r.fake.as_ref().unwrap().set_screen(
        "w1:p1",
        "detection",
        &herdr_screen("claude-idle-detection.txt"),
    );
    let agent = r
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
    let h = bound_headers(&r);
    let h: Vec<(&str, &str)> = h.iter().map(|(k, v)| (*k, v.as_str())).collect();
    r.hook(
        "claude",
        "PreToolUse",
        &fx("claude/payload-PreToolUse-AskUserQuestion.json"),
        &h,
    )
    .await;
    let q = until("read-only question", || {
        r.agent(&agent)
            .and_then(|a| a.pending)
            .filter(|p| p.kind == RequestKind::Question)
            .map(|p| p.id)
    })
    .await;
    until("read-only path", || {
        r.core
            .request(&q)
            .filter(|req| req.answer_path == AnswerPath::None)
    })
    .await;
    let (_, mut rx) = r.core.attach(None);
    r.hook("claude", "PostToolUse", &post_answer("Red"), &h)
        .await;
    assert_eq!(resolution(&mut rx, &q).await, ResolvedBy::Terminal);
}

#[tokio::test]
async fn malformed_question_stays_read_only_without_a_keys_target() {
    let (r, agent) = question_rig().await;
    let h = bound_headers(&r);
    let h: Vec<(&str, &str)> = h.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let mut p = fx("claude/payload-PreToolUse-AskUserQuestion.json");
    p["tool_input"]["questions"][0]["options"][1]["label"] = json!(42);
    r.hook("claude", "PreToolUse", &p, &h).await;
    let q = until("malformed question", || {
        r.agent(&agent)
            .and_then(|a| a.pending)
            .filter(|p| p.kind == RequestKind::Question)
            .map(|p| p.id)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(r.core.request(&q).unwrap().answer_path, AnswerPath::None);
    assert!(r.herdr.as_ref().unwrap().keys_targets().get(&q).is_none());
}

#[tokio::test]
async fn a_question_upgrades_the_existing_blocked_fallback_using_its_open_key() {
    let (r, agent) = bound_rig().await;
    r.fake.as_ref().unwrap().set_screen(
        "w1:p1",
        "detection",
        &herdr_screen("claude-permission-bash.txt"),
    );
    r.fake.as_ref().unwrap().set_screen(
        "w1:p1",
        "visible",
        &herdr_screen("claude-permission-bash.txt"),
    );
    let (_, mut rx) = r.core.attach(None);
    let fallback_id = until("blocked fallback", || {
        r.agent(&agent)
            .and_then(|a| a.pending)
            .filter(|p| p.kind == RequestKind::BlockedUnparsed)
            .map(|p| p.id)
    })
    .await;
    let _fallback = r.core.request(&fallback_id).unwrap();
    r.fake.as_ref().unwrap().set_screen(
        "w1:p1",
        "detection",
        &herdr_screen("claude-askuserquestion-single.txt"),
    );
    r.fake.as_ref().unwrap().set_screen(
        "w1:p1",
        "visible",
        &herdr_screen("claude-askuserquestion-single.txt"),
    );
    let mut h = bound_headers(&r);
    h.push(("X-Claude-Entrypoint", "cli".into()));
    let h: Vec<(&str, &str)> = h.iter().map(|(k, v)| (*k, v.as_str())).collect();
    r.hook(
        "claude",
        "PreToolUse",
        &fx("claude/payload-PreToolUse-AskUserQuestion.json"),
        &h,
    )
    .await;
    let q = until("upgraded question", || {
        r.agent(&agent)
            .and_then(|a| a.pending)
            .filter(|p| p.kind == RequestKind::Question)
            .map(|p| p.id)
    })
    .await;
    assert_eq!(q, fallback_id);
    assert_eq!(r.core.request(&q).unwrap().kind, RequestKind::Question);
    assert_eq!(
        r.core.request(&q).unwrap().questions.as_ref().unwrap()[0].text,
        "Pick a color"
    );
    let _ = &mut rx;
}

#[tokio::test]
async fn a_switched_permission_is_resolved_through_the_evidence_hub() {
    let (r, agent) = bound_rig().await;
    let h = bound_headers(&r);
    let h: Vec<(&str, &str)> = h
        .iter()
        .map(|(key, value)| (*key, value.as_str()))
        .collect();
    let held = r
        .hold(&fx("claude/payload-PermissionRequest-Bash.json"), &h)
        .await;
    let (_, mut rx) = r.core.attach(None);
    let q = until("held permission", || {
        r.agent(&agent).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    drop(held);
    until("keys path", || {
        r.core
            .request(&q)
            .filter(|req| req.answer_path == AnswerPath::Keys && req.status == RequestStatus::Open)
    })
    .await;
    until("switched keys target", || {
        r.herdr.as_ref().unwrap().keys_targets().get(&q)
    })
    .await;
    assert!(answer(&r, "a1", &q, "allow").await.ok);
    let mut post = fx("claude/payload-PostToolUse-Bash.json");
    post["tool_input"] = fx("claude/payload-PermissionRequest-Bash.json")["tool_input"].clone();
    r.hook("claude", "PostToolUse", &post, &CLI).await;
    assert_eq!(resolution(&mut rx, &q).await, ResolvedBy::Uncertain);
}

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

async fn answer(r: &Rig, id: &str, rid: &str, option: &str) -> CommandResult {
    r.core
        .handle(
            "c",
            ClientMessage::RequestAnswer {
                id: id.into(),
                request_id: rid.into(),
                revision: None,
                option_id: Some(option.into()),
                text: None,
            },
        )
        .await
}

async fn resolution(
    rx: &mut tokio::sync::broadcast::Receiver<ServerMessage>,
    rid: &str,
) -> ResolvedBy {
    loop {
        if let ServerMessage::RequestResolved { request_id, by, .. } = rx.recv().await.unwrap()
            && request_id == rid
        {
            return by;
        }
    }
}

const HOOKS_ONLY: &str = "claude:4e3885ee-7454-410d-a2e2-86f4dc2533c0";

/// The gate-2 fixture has a placeholder session id.
fn stop_after_deny() -> Value {
    let mut v = fx("claude/payload-Stop-after-deny.json");
    v["session_id"] = SID.into();
    v
}

#[tokio::test]
async fn allow_from_the_inbox_replies_allow_and_post_tool_use_makes_it_inbox() {
    let r = rig(&[], |_| {}).await;
    let (_, mut rx) = r.core.attach(None);
    let held = r
        .hold(&fx("claude/payload-PermissionRequest-Bash.json"), &CLI)
        .await;
    let rid = until("request", || {
        r.agent(HOOKS_ONLY).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    assert!(answer(&r, "a1", &rid, "allow").await.ok);
    let (status, body) = read_response(held).await;
    assert_eq!(
        (status, body),
        (200, acd_adapters::hooks::wire::reply_allow())
    );
    let mut post = fx("claude/payload-PostToolUse-Bash.json");
    post["tool_input"] = fx("claude/payload-PermissionRequest-Bash.json")["tool_input"].clone();
    r.hook("claude", "PostToolUse", &post, &CLI).await;
    assert_eq!(resolution(&mut rx, &rid).await, ResolvedBy::Inbox);
}

#[tokio::test]
async fn deny_from_the_inbox_then_stop_is_inbox() {
    let r = rig(&[], |_| {}).await;
    let (_, mut rx) = r.core.attach(None);
    let held = r
        .hold(&fx("claude/payload-PermissionRequest-Bash.json"), &CLI)
        .await;
    let rid = until("request", || {
        r.agent(HOOKS_ONLY).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    answer(&r, "a1", &rid, "deny").await;
    assert_eq!(
        read_response(held).await.1,
        acd_adapters::hooks::wire::reply_deny()
    );
    r.hook("claude", "Stop", &stop_after_deny(), &CLI).await;
    assert_eq!(resolution(&mut rx, &rid).await, ResolvedBy::Inbox);
}

#[tokio::test]
async fn terminal_allow_resolves_terminal_and_releases_the_hold() {
    let r = rig(&[], |_| {}).await;
    let (_, mut rx) = r.core.attach(None);
    let held = r
        .hold(&fx("claude/payload-PermissionRequest-Bash.json"), &CLI)
        .await;
    let rid = until("request", || {
        r.agent(HOOKS_ONLY).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    // The user pressed 1 in the terminal: the hold stays open (gate 2b), the tool runs.
    let mut post = fx("claude/payload-PostToolUse-Bash.json");
    post["tool_input"] = fx("claude/payload-PermissionRequest-Bash.json")["tool_input"].clone();
    r.hook("claude", "PostToolUse", &post, &CLI).await;
    assert_eq!(resolution(&mut rx, &rid).await, ResolvedBy::Terminal);
    assert_eq!(
        read_response(held).await,
        (200, String::new()),
        "the hold was released empty"
    );
    assert_eq!(
        answer(&r, "a2", &rid, "allow").await.error_code(),
        Some(ErrorCode::RequestGone)
    );
}

#[tokio::test]
async fn an_inbox_allow_contradicted_by_a_stop_is_terminal() {
    let r = rig(&[], |_| {}).await;
    let (_, mut rx) = r.core.attach(None);
    let held = r
        .hold(&fx("claude/payload-PermissionRequest-Bash.json"), &CLI)
        .await;
    let rid = until("request", || {
        r.agent(HOOKS_ONLY).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    answer(&r, "a1", &rid, "allow").await;
    read_response(held).await;
    // Another hook denied first (gate 2c/2d): no PostToolUse, then Stop.
    r.hook("claude", "Stop", &stop_after_deny(), &CLI).await;
    assert_eq!(resolution(&mut rx, &rid).await, ResolvedBy::Terminal);
}

#[tokio::test]
async fn an_allow_without_evidence_is_uncertain() {
    let r = rig(&[], |_| {}).await;
    let (_, mut rx) = r.core.attach(None);
    let held = r
        .hold(&fx("claude/payload-PermissionRequest-Bash.json"), &CLI)
        .await;
    let rid = until("request", || {
        r.agent(HOOKS_ONLY).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    answer(&r, "a1", &rid, "allow").await;
    read_response(held).await;
    assert_eq!(resolution(&mut rx, &rid).await, ResolvedBy::Uncertain);
}

#[tokio::test]
async fn a_second_prompt_in_the_same_turn_opens_a_new_request() {
    let r = rig(&[], |_| {}).await;
    let (_, mut rx) = r.core.attach(None);
    let first = fx("claude/payload-PermissionRequest-Bash.json");
    let held = r.hold(&first, &CLI).await;
    let rid1 = until("first", || {
        r.agent(HOOKS_ONLY).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    answer(&r, "a1", &rid1, "allow").await;
    read_response(held).await;
    let mut post = fx("claude/payload-PostToolUse-Bash.json");
    post["tool_input"] = first["tool_input"].clone();
    r.hook("claude", "PostToolUse", &post, &CLI).await;
    assert_eq!(resolution(&mut rx, &rid1).await, ResolvedBy::Inbox);
    // Same prompt_id, the same command again (a retry): a new occurrence.
    let _held2 = r.hold(&first, &CLI).await;
    let rid2 = until("second", || {
        r.agent(HOOKS_ONLY).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    assert_ne!(rid1, rid2);
}

#[tokio::test]
async fn a_new_prompt_while_awaiting_evidence_makes_the_old_one_uncertain() {
    let r = rig(&[], |_| {}).await;
    let (_, mut rx) = r.core.attach(None);
    let held = r
        .hold(&fx("claude/payload-PermissionRequest-Bash.json"), &CLI)
        .await;
    let rid1 = until("first", || {
        r.agent(HOOKS_ONLY).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    answer(&r, "a1", &rid1, "allow").await;
    read_response(held).await;
    let mut second = fx("claude/payload-PermissionRequest-Bash.json");
    second["tool_input"]["command"] = "touch /tmp/acp0/t3.txt".into();
    let _held2 = r.hold(&second, &CLI).await;
    assert_eq!(
        resolution(&mut rx, &rid1).await,
        ResolvedBy::Uncertain,
        "D10"
    );
}

#[tokio::test]
async fn allow_always_writes_exactly_the_rule_the_client_showed() {
    use acd_adapters::hooks::{
        mapping::rule_text,
        wire::{AlwaysRule, reply_allow_always},
    };
    let r = rig(&[], |_| {}).await;
    let p = fx("claude/payload-PermissionRequest-Bash.json");
    let held = r.hold(&p, &CLI).await;
    let req = until("request", || r.agent(HOOKS_ONLY).and_then(|a| a.pending)).await;
    let shown = req
        .options
        .iter()
        .find(|o| o.id == "allow_always")
        .and_then(|o| o.description.clone())
        .unwrap();
    let expected = AlwaysRule {
        tool_name: "Bash".into(),
        rule_content: "touch /tmp/acp0/t1.txt".into(),
    };
    assert!(shown.contains(&rule_text(&expected)), "{shown}");
    answer(&r, "a1", &req.id, "allow_always").await;
    assert_eq!(
        read_response(held).await.1,
        reply_allow_always(&expected),
        "ruling Q2: the exact command"
    );
}

#[tokio::test]
async fn an_identical_command_before_evidence_opens_a_new_occurrence() {
    // Sol I3: the first reply was delivered and awaits evidence; the same
    // command in the same turn is a new dialog (O1), with its own hold.
    let r = rig(&[], |_| {}).await;
    let (_, mut rx) = r.core.attach(None);
    let p = fx("claude/payload-PermissionRequest-Bash.json");
    let held1 = r.hold(&p, &CLI).await;
    let rid1 = until("first", || {
        r.agent(HOOKS_ONLY).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    answer(&r, "a1", &rid1, "allow").await;
    read_response(held1).await;
    let held2 = r.hold(&p, &CLI).await;
    assert_eq!(
        resolution(&mut rx, &rid1).await,
        ResolvedBy::Uncertain,
        "O1: delivered, superseded before evidence"
    );
    let rid2 = until("second", || {
        r.agent(HOOKS_ONLY)
            .and_then(|a| a.pending)
            .map(|p| p.id)
            .filter(|i| *i != rid1)
    })
    .await;
    assert!(
        answer(&r, "a2", &rid2, "deny").await.ok,
        "the new occurrence has its own hold"
    );
    assert_eq!(
        read_response(held2).await.1,
        acd_adapters::hooks::wire::reply_deny()
    );
}

#[tokio::test]
async fn delivery_racing_supersession_is_serialized_before_terminal_retirement() {
    let r = rig(&[], |_| {}).await;
    let (_, mut rx) = r.core.attach(None);
    let p = fx("claude/payload-PermissionRequest-Bash.json");
    let held1 = r.hold(&p, &CLI).await;
    let rid1 = until("R1", || {
        r.agent(HOOKS_ONLY).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    let _ = r.core.request(&rid1).expect("R1 registered");
    let (retire_reached, retire_release) = r.hooks.gates().retire.arm();
    let held2 = r.hold(&p, &CLI).await;
    retire_reached.await.unwrap();
    let core = r.core.clone();
    let request_id = rid1.clone();
    let answer = tokio::spawn(async move {
        core.handle(
            "a1",
            ClientMessage::RequestAnswer {
                id: "a1".into(),
                request_id,
                revision: None,
                option_id: Some("allow".into()),
                text: None,
            },
        )
        .await
    });
    tokio::task::yield_now().await;
    assert!(
        !answer.is_finished(),
        "answer waits behind the retirement lock"
    );
    retire_release.send(()).unwrap();
    let result = answer.await.unwrap();
    assert_eq!(result.error_code(), Some(ErrorCode::RequestGone));
    assert_eq!(resolution(&mut rx, &rid1).await, ResolvedBy::Terminal);
    assert_eq!(read_response(held1).await, (200, String::new()));
    drop(held2);
}

#[tokio::test]
async fn a_delayed_outcome_of_an_older_identical_request_does_not_resolve_the_newer_one() {
    // Sol N4, O7: R1 delivered, identical R2 opens, then R1's PostToolUse
    // arrives. It cannot be told apart from R2's, so R2 stays open.
    let r = rig(&[], |_| {}).await;
    let (_, mut rx) = r.core.attach(None);
    let p = fx("claude/payload-PermissionRequest-Bash.json");
    let held1 = r.hold(&p, &CLI).await;
    let rid1 = until("R1", || {
        r.agent(HOOKS_ONLY).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    assert!(answer(&r, "a1", &rid1, "allow").await.ok);
    read_response(held1).await;
    let held2 = r.hold(&p, &CLI).await;
    assert_eq!(resolution(&mut rx, &rid1).await, ResolvedBy::Uncertain);
    let rid2 = until("R2", || {
        r.agent(HOOKS_ONLY)
            .and_then(|a| a.pending)
            .map(|p| p.id)
            .filter(|i| *i != rid1)
    })
    .await;
    let mut post = fx("claude/payload-PostToolUse-Bash.json");
    post["tool_input"] = p["tool_input"].clone();
    r.hook("claude", "PostToolUse", &post, &CLI).await; // R1's delayed outcome
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        r.core.request(&rid2).map(|q| q.status),
        Some(RequestStatus::Open),
        "R2 unresolved"
    );
    // R2 still answers through its own hold.
    assert!(answer(&r, "a2", &rid2, "deny").await.ok);
    assert_eq!(
        read_response(held2).await.1,
        acd_adapters::hooks::wire::reply_deny()
    );
    // Identity evidence cannot confirm R2 either; its turn end decides.
    r.hook("claude", "PostToolUse", &post, &CLI).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        r.core.request(&rid2).is_some(),
        "a second identical PostToolUse is still ambiguous"
    );
    r.hook("claude", "Stop", &fx("claude/payload-Stop.json"), &CLI)
        .await;
    assert_eq!(
        resolution(&mut rx, &rid2).await,
        ResolvedBy::Uncertain,
        "a turn end cannot confirm a shadowed answer"
    );
}

#[tokio::test]
async fn a_new_turn_clears_the_shadow() {
    let r = rig(&[], |_| {}).await;
    let (_, mut rx) = r.core.attach(None);
    let p = fx("claude/payload-PermissionRequest-Bash.json");
    let held1 = r.hold(&p, &CLI).await;
    let rid1 = until("R1", || {
        r.agent(HOOKS_ONLY).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    drop(held1); // terminal deny: ends without identity evidence
    r.hook(
        "claude",
        "UserPromptSubmit",
        &fx("claude/payload-UserPromptSubmit.json"),
        &CLI,
    )
    .await; // turn end
    let _ = resolution(&mut rx, &rid1).await;
    let _held2 = r.hold(&p, &CLI).await;
    let rid2 = until("R2", || {
        r.agent(HOOKS_ONLY)
            .and_then(|a| a.pending)
            .map(|p| p.id)
            .filter(|i| *i != rid1)
    })
    .await;
    let mut post = fx("claude/payload-PostToolUse-Bash.json");
    post["tool_input"] = p["tool_input"].clone();
    r.hook("claude", "PostToolUse", &post, &CLI).await;
    assert_eq!(
        resolution(&mut rx, &rid2).await,
        ResolvedBy::Terminal,
        "not shadowed: a terminal allow"
    );
}

#[tokio::test]
async fn an_identical_command_while_held_releases_the_old_hold() {
    let r = rig(&[], |_| {}).await;
    let (_, mut rx) = r.core.attach(None);
    let p = fx("claude/payload-PermissionRequest-Bash.json");
    let held1 = r.hold(&p, &CLI).await;
    let rid1 = until("first", || {
        r.agent(HOOKS_ONLY).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    let _held2 = r.hold(&p, &CLI).await;
    assert_eq!(
        read_response(held1).await,
        (200, String::new()),
        "O1: the older hold is released empty"
    );
    assert_eq!(resolution(&mut rx, &rid1).await, ResolvedBy::Terminal);
    until("second", || {
        r.agent(HOOKS_ONLY)
            .and_then(|a| a.pending)
            .filter(|q| q.id != rid1)
    })
    .await;
}

#[tokio::test]
async fn t3_taking_over_mid_hold_releases_it() {
    let r = rig(&[], |_| {}).await;
    let held = r
        .hold(&fx("claude/payload-PermissionRequest-Bash.json"), &CLI)
        .await;
    until("request", || r.agent(HOOKS_ONLY).and_then(|a| a.pending)).await;
    r.t3.set_for_tests("t3:thread-1", Some(SID));
    assert_eq!(read_response(held).await, (200, String::new()));
    assert!(r.agent(HOOKS_ONLY).is_none());
}

#[tokio::test]
async fn the_same_command_after_an_uncertain_end_opens_again() {
    let r = rig(&[], |_| {}).await;
    let (_, mut rx) = r.core.attach(None);
    let p = fx("claude/payload-PermissionRequest-Bash.json");
    let held = r.hold(&p, &CLI).await;
    let rid1 = until("first", || {
        r.agent(HOOKS_ONLY).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    answer(&r, "a1", &rid1, "allow").await;
    read_response(held).await;
    assert_eq!(resolution(&mut rx, &rid1).await, ResolvedBy::Uncertain);
    // Same session, prompt and command: a new occurrence, not a dropped replay.
    let _held2 = r.hold(&p, &CLI).await;
    let rid2 = until("second", || {
        r.agent(HOOKS_ONLY).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    assert_ne!(rid1, rid2);
}

fn bound_headers(r: &Rig) -> Vec<(&'static str, String)> {
    vec![
        ("X-Herdr-Pane", "w1:p1".into()),
        ("X-Herdr-Socket", r.sock()),
        ("X-Claude-Entrypoint", "cli".into()),
    ]
}

async fn bound_rig() -> (Rig, String) {
    let r = rig(&[fake_agent("w1:p1", "claude", "blocked")], |_| {}).await;
    let dialog = std::fs::read_to_string(format!(
        "{}/../../tests/fixtures/herdr/claude-permission-bash.txt",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    r.fake
        .as_ref()
        .unwrap()
        .set_screen("w1:p1", "detection", &dialog);
    r.fake
        .as_ref()
        .unwrap()
        .set_screen("w1:p1", "visible", &dialog);
    let agent = r
        .herdr
        .as_ref()
        .unwrap()
        .panes()
        .read()
        .unwrap()
        .iter()
        .find(|(_, pane)| pane.pane_id == "w1:p1")
        .unwrap()
        .0
        .clone();
    (r, agent)
}

async fn unheld_bound_rig() -> (Rig, String, tokio::net::TcpStream) {
    let r = rig(
        &[
            fake_agent("w1:p1", "claude", "working"),
            fake_agent("w1:p2", "claude", "working"),
        ],
        |cfg| {
            cfg.max_holds = 1;
        },
    )
    .await;
    let dialog = std::fs::read_to_string(format!(
        "{}/../../tests/fixtures/herdr/claude-permission-bash.txt",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    for pane in ["w1:p1", "w1:p2"] {
        r.fake
            .as_ref()
            .unwrap()
            .set_screen(pane, "detection", &dialog);
    }
    let socket = r.sock();
    let filler_headers = vec![
        ("X-Herdr-Pane", "w1:p2"),
        ("X-Herdr-Socket", socket.as_str()),
        ("X-Claude-Entrypoint", "cli"),
    ];
    let filler_payload = fx("claude/payload-PermissionRequest-Bash.json");
    let filler = r.hold(&filler_payload, &filler_headers).await;
    let filler_agent = r
        .herdr
        .as_ref()
        .unwrap()
        .panes()
        .read()
        .unwrap()
        .iter()
        .find(|(_, pane)| pane.pane_id == "w1:p2")
        .unwrap()
        .0
        .clone();
    until("hold capacity occupied", || {
        r.agent(&filler_agent).and_then(|agent| agent.pending)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    let mut response = [0_u8; 1];
    assert!(matches!(
        filler.try_read(&mut response),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
    ));
    let agent = r
        .herdr
        .as_ref()
        .unwrap()
        .panes()
        .read()
        .unwrap()
        .iter()
        .find(|(_, pane)| pane.pane_id == "w1:p1")
        .unwrap()
        .0
        .clone();
    (r, agent, filler)
}

#[tokio::test]
async fn a_hooks_only_hold_close_is_terminal_with_stop_evidence_and_uncertain_without() {
    let r = rig(&[], |_| {}).await;
    let (_, mut rx) = r.core.attach(None);
    let held = r
        .hold(&fx("claude/payload-PermissionRequest-Bash.json"), &CLI)
        .await;
    let rid = until("request", || {
        r.agent(HOOKS_ONLY).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    drop(held);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        answer(&r, "a1", &rid, "allow").await.error_code(),
        Some(ErrorCode::RequestGone)
    );
    r.hook("claude", "Stop", &stop_after_deny(), &CLI).await;
    assert_eq!(resolution(&mut rx, &rid).await, ResolvedBy::Terminal);

    let mut second = fx("claude/payload-PermissionRequest-Bash.json");
    second["tool_input"]["command"] = "touch /tmp/acp0/t9.txt".into();
    let held = r.hold(&second, &CLI).await;
    let rid2 = until("second request", || {
        r.agent(HOOKS_ONLY)
            .and_then(|a| a.pending)
            .map(|p| p.id)
            .filter(|id| *id != rid)
    })
    .await;
    drop(held);
    assert_eq!(resolution(&mut rx, &rid2).await, ResolvedBy::Uncertain);
}

#[tokio::test]
async fn a_hooks_only_expiry_is_lost() {
    let r = rig(&[], |cfg| cfg.permission_hold = Duration::from_millis(200)).await;
    let (_, mut rx) = r.core.attach(None);
    let held = r
        .hold(&fx("claude/payload-PermissionRequest-Bash.json"), &CLI)
        .await;
    let rid = until("request", || {
        r.agent(HOOKS_ONLY).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    assert_eq!(read_response(held).await, (200, String::new()));
    assert_eq!(resolution(&mut rx, &rid).await, ResolvedBy::Lost);
}

#[tokio::test]
async fn a_bound_hold_close_with_the_matching_dialog_switches_to_incarnation_bound_keys() {
    let (r, agent) = bound_rig().await;
    let h = bound_headers(&r);
    let h: Vec<(&str, &str)> = h
        .iter()
        .map(|(key, value)| (*key, value.as_str()))
        .collect();
    let held = r
        .hold(&fx("claude/payload-PermissionRequest-Bash.json"), &h)
        .await;
    let rid = until("bound request", || {
        r.agent(&agent).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(r.herdr.as_ref().unwrap().keys_targets().get(&rid).is_none());
    drop(held);
    let req = until("keys path", || {
        r.core
            .request(&rid)
            .filter(|request| request.answer_path == AnswerPath::Keys)
    })
    .await;
    assert_eq!(
        req.options
            .iter()
            .map(|option| option.id.as_str())
            .collect::<Vec<_>>(),
        vec!["allow", "deny"]
    );
    assert!(req.revision >= 2);
    let target = until("keys target", || {
        r.herdr.as_ref().unwrap().keys_targets().get(&rid)
    })
    .await;
    assert!(target.incarnation().is_some());
    assert!(answer(&r, "a1", &rid, "allow").await.ok);
    assert_eq!(r.fake.as_ref().unwrap().keys_sent().len(), 1);
}

#[tokio::test]
async fn a_bound_hold_close_without_the_dialog_waits_for_evidence_then_is_uncertain() {
    let (r, agent) = bound_rig().await;
    let (_, mut rx) = r.core.attach(None);
    let h = bound_headers(&r);
    let h: Vec<(&str, &str)> = h
        .iter()
        .map(|(key, value)| (*key, value.as_str()))
        .collect();
    let held = r
        .hold(&fx("claude/payload-PermissionRequest-Bash.json"), &h)
        .await;
    let rid = until("request", || {
        r.agent(&agent).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let idle = std::fs::read_to_string(format!(
        "{}/../../tests/fixtures/herdr/claude-idle-detection.txt",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    r.fake
        .as_ref()
        .unwrap()
        .set_screen("w1:p1", "detection", &idle);
    drop(held);
    assert_eq!(resolution(&mut rx, &rid).await, ResolvedBy::Uncertain);
}

#[tokio::test]
async fn a_bound_hold_close_without_a_capture_uses_stop_evidence() {
    let (r, agent) = bound_rig().await;
    let (_, mut rx) = r.core.attach(None);
    let h = bound_headers(&r);
    let h: Vec<(&str, &str)> = h
        .iter()
        .map(|(key, value)| (*key, value.as_str()))
        .collect();
    r.fake
        .as_ref()
        .unwrap()
        .set_truncated("w1:p1", "detection", true);
    let held = r
        .hold(&fx("claude/payload-PermissionRequest-Bash.json"), &h)
        .await;
    let rid = until("request", || {
        r.agent(&agent).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(700)).await;
    drop(held);
    r.hook("claude", "Stop", &stop_after_deny(), &h).await;
    assert_eq!(resolution(&mut rx, &rid).await, ResolvedBy::Terminal);
}

#[tokio::test]
async fn a_bound_hold_close_without_a_capture_or_evidence_is_uncertain() {
    let (r, agent) = bound_rig().await;
    let (_, mut rx) = r.core.attach(None);
    let h = bound_headers(&r);
    let h: Vec<(&str, &str)> = h
        .iter()
        .map(|(key, value)| (*key, value.as_str()))
        .collect();
    r.fake
        .as_ref()
        .unwrap()
        .set_truncated("w1:p1", "detection", true);
    let held = r
        .hold(&fx("claude/payload-PermissionRequest-Bash.json"), &h)
        .await;
    let rid = until("request", || {
        r.agent(&agent).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(700)).await;
    drop(held);
    assert_eq!(resolution(&mut rx, &rid).await, ResolvedBy::Uncertain);
}

#[tokio::test]
async fn a_claim_during_the_hold_end_read_is_not_misreported_as_gone() {
    let (r, agent) = bound_rig().await;
    let h = bound_headers(&r);
    let h: Vec<(&str, &str)> = h
        .iter()
        .map(|(key, value)| (*key, value.as_str()))
        .collect();
    let held = r
        .hold(&fx("claude/payload-PermissionRequest-Bash.json"), &h)
        .await;
    let rid = until("request", || {
        r.agent(&agent).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let gate = r.fake.as_ref().unwrap().block_next("agent.read");
    drop(held);
    tokio::time::timeout(Duration::from_secs(3), gate.arrived())
        .await
        .expect("hold-end read reached fake gate");
    assert_eq!(
        answer(&r, "a1", &rid, "allow").await.error_code(),
        Some(ErrorCode::RequestNotReady)
    );
    gate.release();
    until("keys path", || {
        r.core
            .request(&rid)
            .filter(|request| request.answer_path == AnswerPath::Keys)
    })
    .await;
}

#[tokio::test]
async fn unavailable_hold_end_reads_resolve_uncertain() {
    let (r, agent) = bound_rig().await;
    let (_, mut rx) = r.core.attach(None);
    let h = bound_headers(&r);
    let h: Vec<(&str, &str)> = h
        .iter()
        .map(|(key, value)| (*key, value.as_str()))
        .collect();
    let held = r
        .hold(&fx("claude/payload-PermissionRequest-Bash.json"), &h)
        .await;
    let rid = until("request", || {
        r.agent(&agent).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    r.fake
        .as_ref()
        .unwrap()
        .set_truncated("w1:p1", "detection", true);
    drop(held);
    assert_eq!(resolution(&mut rx, &rid).await, ResolvedBy::Uncertain);
}

#[tokio::test]
async fn a_capture_result_waits_for_the_herdr_delivery_lock_before_applying() {
    let (r, agent, _filler) = unheld_bound_rig().await;
    let gate = r.fake.as_ref().unwrap().block_next("agent.read");
    let port = r.port;
    let mut payload = fx("claude/payload-PermissionRequest-Bash.json");
    payload["session_id"] = "target-session".into();
    let body = payload.to_string();
    let headers = bound_headers(&r);
    let hook = tokio::spawn(async move {
        let borrowed: Vec<(&str, &str)> = headers
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect();
        acd_adapters::hooks::testing::post(
            port,
            "/hook/claude/PermissionRequest",
            TOKEN,
            &borrowed,
            &body,
        )
        .await
    });
    let rid = until("request", || {
        r.agent(&agent).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    let initial_request = r.core.request(&rid).unwrap();
    assert_eq!(initial_request.answer_path, AnswerPath::Keys);
    tokio::time::timeout(Duration::from_secs(3), gate.arrived())
        .await
        .expect("capture read reached fake gate");
    let locks = r.herdr.as_ref().unwrap().agent_locks();
    let guard = locks.lock(&agent).await;
    gate.release();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(r.herdr.as_ref().unwrap().keys_targets().get(&rid).is_none());
    assert_eq!(r.core.request(&rid).unwrap(), initial_request);
    drop(guard);
    until("capture applies after delivery lock release", || {
        r.herdr.as_ref().unwrap().keys_targets().get(&rid)
    })
    .await;
    assert_eq!(r.core.request(&rid).unwrap().status, RequestStatus::Open);
    assert_eq!(r.core.request(&rid).unwrap().answer_path, AnswerPath::Keys);
    let _ = hook.await.unwrap();
}

#[tokio::test]
async fn a_capture_from_a_replaced_pane_is_discarded() {
    let (r, agent, _filler) = unheld_bound_rig().await;
    let gate = r.fake.as_ref().unwrap().block_next("agent.read");
    let port = r.port;
    let mut payload = fx("claude/payload-PermissionRequest-Bash.json");
    payload["session_id"] = "target-session".into();
    let body = payload.to_string();
    let headers = bound_headers(&r);
    let hook = tokio::spawn(async move {
        let borrowed: Vec<(&str, &str)> = headers
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect();
        acd_adapters::hooks::testing::post(
            port,
            "/hook/claude/PermissionRequest",
            TOKEN,
            &borrowed,
            &body,
        )
        .await
    });
    let rid = until("request", || {
        r.agent(&agent).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    tokio::time::timeout(Duration::from_secs(3), gate.arrived())
        .await
        .expect("capture read reached fake gate");
    let panes = r.herdr.as_ref().unwrap().panes();
    panes
        .write()
        .unwrap()
        .get_mut(&agent)
        .expect("captured pane remains registered")
        .incarnation += 1;
    gate.release();
    let _ = hook.await.unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(r.herdr.as_ref().unwrap().keys_targets().get(&rid).is_none());
    assert_eq!(r.core.request(&rid).unwrap().status, RequestStatus::Open);
}

#[tokio::test]
async fn a_capture_result_after_t3_takeover_is_discarded() {
    let (r, agent, _filler) = unheld_bound_rig().await;
    let gate = r.fake.as_ref().unwrap().block_next("agent.read");
    let port = r.port;
    let mut payload = fx("claude/payload-PermissionRequest-Bash.json");
    payload["session_id"] = "target-session".into();
    let body = payload.to_string();
    let headers = bound_headers(&r);
    let hook = tokio::spawn(async move {
        let borrowed: Vec<(&str, &str)> = headers
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect();
        acd_adapters::hooks::testing::post(
            port,
            "/hook/claude/PermissionRequest",
            TOKEN,
            &borrowed,
            &body,
        )
        .await
    });
    let rid = until("request", || {
        r.agent(&agent).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    tokio::time::timeout(Duration::from_secs(3), gate.arrived())
        .await
        .expect("capture read reached fake gate");
    r.t3.set_for_tests("t3:thread-target", Some("target-session"));
    gate.release();
    let _ = hook.await.unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(r.herdr.as_ref().unwrap().keys_targets().get(&rid).is_none());
    assert_eq!(r.core.request(&rid).unwrap().status, RequestStatus::Open);
}

#[tokio::test]
async fn a_stale_capture_for_an_unheld_request_cannot_register_a_target() {
    let (r, agent, _filler) = unheld_bound_rig().await;
    let gate = r.fake.as_ref().unwrap().block_next("agent.read");
    let port = r.port;
    let body = fx("claude/payload-PermissionRequest-Bash.json").to_string();
    let headers = bound_headers(&r);
    let hook = tokio::spawn(async move {
        let borrowed: Vec<(&str, &str)> = headers
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect();
        acd_adapters::hooks::testing::post(
            port,
            "/hook/claude/PermissionRequest",
            TOKEN,
            &borrowed,
            &body,
        )
        .await
    });
    let rid = until("request", || {
        r.agent(&agent).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    tokio::time::timeout(Duration::from_secs(3), gate.arrived())
        .await
        .expect("capture read reached the fake gate");
    r.core.observe(Observation::RequestEndedById {
        request_id: rid.clone(),
        by: ResolvedBy::Terminal,
    });
    gate.release();
    let _ = hook.await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(r.herdr.as_ref().unwrap().keys_targets().get(&rid).is_none());
    assert!(r.agent(&agent).and_then(|a| a.pending).is_none());
}

#[tokio::test]
async fn a_hold_end_read_waiting_for_a_slot_still_switches_to_keys() {
    let r = rig(
        &[
            fake_agent("w1:p1", "claude", "blocked"),
            fake_agent("w1:p2", "claude", "blocked"),
        ],
        |cfg| {
            cfg.max_reads = 1;
            cfg.held_dialog_poll = Duration::from_secs(30);
        },
    )
    .await;
    let dialog = std::fs::read_to_string(format!(
        "{}/../../tests/fixtures/herdr/claude-permission-bash.txt",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let fake = r.fake.as_ref().unwrap();
    for pane in ["w1:p1", "w1:p2"] {
        fake.set_screen(pane, "detection", &dialog);
        fake.set_screen(pane, "visible", &dialog);
    }
    let sock = r.sock();
    let on = |pane: &'static str| {
        vec![
            ("X-Herdr-Pane", pane),
            ("X-Herdr-Socket", sock.as_str()),
            ("X-Claude-Entrypoint", "cli"),
        ]
    };
    let agent_for = |pane: &str| {
        r.herdr
            .as_ref()
            .unwrap()
            .panes()
            .read()
            .unwrap()
            .iter()
            .find(|(_, fact)| fact.pane_id == pane)
            .unwrap()
            .0
            .clone()
    };
    let (a1, a2) = (agent_for("w1:p1"), agent_for("w1:p2"));
    let held_a = r
        .hold(
            &fx("claude/payload-PermissionRequest-Bash.json"),
            &on("w1:p1"),
        )
        .await;
    let rid_a = until("A", || r.agent(&a1).and_then(|a| a.pending).map(|p| p.id)).await;
    tokio::time::sleep(Duration::from_millis(700)).await;
    let gate = fake.block_next("agent.read");
    let mut second = fx("claude/payload-PermissionRequest-Bash.json");
    second["session_id"] = "5f4996ff-0000-4000-8000-000000000002".into();
    let _held_b = r.hold(&second, &on("w1:p2")).await;
    until("B", || r.agent(&a2).and_then(|a| a.pending)).await;
    gate.arrived().await;
    drop(held_a);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        r.herdr
            .as_ref()
            .unwrap()
            .keys_targets()
            .get(&rid_a)
            .is_none()
    );
    gate.release();
    until("A keys target", || {
        r.herdr.as_ref().unwrap().keys_targets().get(&rid_a)
    })
    .await;
    assert_eq!(
        r.core.request(&rid_a).map(|request| request.answer_path),
        Some(AnswerPath::Keys)
    );
}

#[tokio::test]
async fn a_held_request_migrates_to_herdr_with_the_same_id() {
    let mut pane = fake_agent("w1:p1", "claude", "blocked");
    let r = rig(&[pane.clone()], |_| {}).await;
    let _held = r
        .hold(&fx("claude/payload-PermissionRequest-Bash.json"), &CLI)
        .await;
    let rid = until("hooks-only pending request", || {
        r.agent(HOOKS_ONLY).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    pane["agent_session"] = json!({"value": SID});
    r.fake.as_ref().unwrap().set_agent(pane);
    r.fake
        .as_ref()
        .unwrap()
        .push_event(json!({"event":"pane_updated","data":{"pane_id":"w1:p1"}}));
    until("herdr session binding", || {
        r.herdr
            .as_ref()
            .unwrap()
            .panes()
            .read()
            .unwrap()
            .values()
            .find_map(|pane| pane.session.clone())
    })
    .await;
    r.hook(
        "claude",
        "Notification",
        &fx("claude/payload-Notification.json"),
        &CLI,
    )
    .await;
    let agent = r
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
    until("migrated request", || {
        r.agent(&agent)
            .and_then(|a| a.pending)
            .filter(|pending| pending.id == rid)
    })
    .await;
    assert!(r.agent(HOOKS_ONLY).is_none());
}
