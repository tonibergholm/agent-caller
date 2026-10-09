mod common;
use acd_adapters::herdr::fake::fake_agent;
use acd_adapters::hooks::testing::read_response;
use acd_core::{CorrelationKey, Observation};
use acd_protocol::*;
use common::*;
use serde_json::{Value, json};
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
