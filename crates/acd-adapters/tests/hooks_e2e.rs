mod common;
use common::*;

use std::time::Duration;

use acd_adapters::herdr::fake::fake_agent;
use acd_adapters::hooks::testing::read_response;
use acd_protocol::*;
use tokio::time::Instant;

fn herdr_screen(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../../tests/fixtures/herdr/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
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

#[tokio::test]
async fn criterion_1_a_herdr_bound_claude_permission_is_answerable_within_2_s_and_yes_unblocks() {
    let r = rig(&[fake_agent("w1:p1", "claude", "blocked")], |_| {}).await;
    r.fake.as_ref().unwrap().set_screen(
        "w1:p1",
        "detection",
        &herdr_screen("claude-permission-bash.txt"),
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
    let sock = r.sock();
    let h = [
        ("X-Herdr-Pane", "w1:p1"),
        ("X-Herdr-Socket", sock.as_str()),
        ("X-Claude-Entrypoint", "cli"),
    ];
    let (_, mut rx) = r.core.attach(None);
    let t = Instant::now();
    let held = r
        .hold(&fx("claude/payload-PermissionRequest-Bash.json"), &h)
        .await;
    let rid = loop {
        if let ServerMessage::RequestOpened { request, .. } = rx.recv().await.unwrap()
            && request.kind == RequestKind::Permission
        {
            break request.id;
        }
    };
    assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
    assert!(answer(&r, "a1", &rid, "allow").await.ok);
    assert_eq!(
        read_response(held).await.1,
        acd_adapters::hooks::wire::reply_allow()
    );
    assert_eq!(
        answer(&r, "a2", &rid, "deny").await.error_code(),
        Some(ErrorCode::RequestBusy),
        "the agent applies only the first answer"
    );
    assert_eq!(r.core.request(&rid).unwrap().agent_id, agent);
}

#[tokio::test]
async fn criterion_4_answered_in_the_terminal_first_gives_request_gone() {
    let r = rig(&[], |_| {}).await;
    let held = r
        .hold(&fx("claude/payload-PermissionRequest-Bash.json"), &CLI)
        .await;
    let rid = until("request", || {
        r.agent(&format!("claude:{SID}"))
            .and_then(|a| a.pending)
            .map(|p| p.id)
    })
    .await;
    drop(held); // terminal deny
    let mut stop = fx("claude/payload-Stop-after-deny.json");
    stop["session_id"] = SID.into();
    r.hook("claude", "Stop", &stop, &CLI).await;
    until("resolved", || r.core.request(&rid).is_none().then_some(())).await;
    assert_eq!(
        answer(&r, "a1", &rid, "allow").await.error_code(),
        Some(ErrorCode::RequestGone)
    );
}
