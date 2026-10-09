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
