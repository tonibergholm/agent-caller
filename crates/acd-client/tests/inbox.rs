mod common;
use acd_client::{fake::*, inbox::*, settings::*, view::*};
use serde_json::Value;
use std::{sync::Arc, time::Duration};
const ANSWER_REQ: &str = "r1";
fn pending_agent() -> Value {
    serde_json::json!({"id":"a1","machine_id":"m","source":"herdr","kind":"claude","title":"agent","cwd":null,"project":null,"state":"needs_you","state_since":"2026-10-08T00:00:00.000Z","failed":false,"last_message":null,"can_send":true,"pending":{"id":ANSWER_REQ,"agent_id":"a1","kind":"permission","status":"open","answer_path":"hook","title":"Bash","detail":"npm test","options":[{"id":"allow","label":"Yes","description":null,"effect":"allow"},{"id":"deny","label":"No","description":null,"effect":"deny"},{"id":"always","label":"Yes, always","description":"Bash(npm test:*)","effect":"allow_always"}],"questions":null,"free_text":false,"raw_screen":null,"opened_at":"2026-10-08T00:00:00.000Z"}})
}
async fn setup() -> (FakeServer, FakeConn, Arc<Inbox>) {
    let f = FakeServer::start(common::TOKEN, Identity::Paired).await;
    let secrets = Arc::new(MemorySecrets::default());
    secrets
        .set(
            "m1",
            &MachineSecret {
                token: common::TOKEN.into(),
                pin: f.pin(),
            }
            .to_json(),
        )
        .unwrap();
    let settings = Settings {
        client_id: uuid::Uuid::new_v4().to_string(),
        hotkey: DEFAULT_HOTKEY.into(),
        show_idle: false,
        machines: vec![MachineEntry {
            key: "m1".into(),
            name: "Box".into(),
            endpoint: EndpointConfig::Tcp {
                host: "127.0.0.1".into(),
                port: f.port(),
            },
        }],
    };
    let ib = Inbox::new(
        settings,
        secrets,
        InboxOptions {
            timing: common::fast(),
            settings_path: None,
            client_name: "t".into(),
            client_version: "0".into(),
        },
    );
    ib.start();
    let c = f.next_connection().await;
    c.send(serde_json::json!({"type":"snapshot","epoch":"e1","seq":1,"agents":[pending_agent()]}))
        .await;
    view_until(&ib, "answerable", |v| v.cards.iter().any(|c| c.answerable)).await;
    (f, c, ib)
}
async fn view_until(ib: &Inbox, what: &str, pred: impl Fn(&InboxView) -> bool) {
    let mut rx = ib.view();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if pred(&rx.borrow()) {
                break;
            }
            rx.changed().await.unwrap()
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}
fn answers(f: &FakeServer) -> usize {
    f.received()
        .iter()
        .filter(|v| v["type"] == "request.answer")
        .count()
}
#[tokio::test]
async fn pick_waits_for_the_undo_then_sends_once() {
    let (f, _, ib) = setup().await;
    ib.pick("m1/a1", ANSWER_REQ, "allow").unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(answers(&f), 0);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(answers(&f), 1);
}
#[tokio::test]
async fn escape_right_after_pick_cancels_and_sends_nothing() {
    let (f, _, ib) = setup().await;
    ib.pick("m1/a1", ANSWER_REQ, "allow").unwrap();
    assert!(ib.escape());
    assert!(!ib.escape());
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(answers(&f), 0);
}
#[tokio::test]
async fn countdown_cancels_when_request_resolves() {
    let (f, c, ib) = setup().await;
    ib.pick("m1/a1", ANSWER_REQ, "allow").unwrap();
    c.send(serde_json::json!({"type":"request.resolved","epoch":"e1","seq":2,"request_id":ANSWER_REQ,"agent_id":"a1","by":"terminal","option_id":null})).await;
    let mut resolved_agent = pending_agent();
    resolved_agent["pending"] = Value::Null;
    c.send(serde_json::json!({"type":"agent.updated","epoch":"e1","seq":3,"agent":resolved_agent}))
        .await;
    view_until(&ib, "removed action", |v| {
        v.actions.is_empty() && v.cards.first().is_some_and(|c| c.notice.is_some())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(answers(&f), 0);
}
#[tokio::test]
async fn countdown_cancels_when_option_semantics_change() {
    let (f, c, ib) = setup().await;
    ib.pick("m1/a1", ANSWER_REQ, "always").unwrap();
    let mut agent = pending_agent();
    agent["pending"]["options"][2]["description"] = Value::String("Bash(rm:*)".into());
    c.send(serde_json::json!({"type":"agent.updated","epoch":"e1","seq":2,"agent":agent}))
        .await;
    view_until(&ib, "changed request notice", |v| {
        v.actions.is_empty()
            && v.cards
                .first()
                .and_then(|c| c.notice.as_ref())
                .is_some_and(|n| n.text == "The request changed. Nothing was sent.")
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(answers(&f), 0);
}
#[tokio::test]
async fn reconnect_within_undo_window_cancels() {
    for how in ["4408", "resync"] {
        let (f, c, ib) = setup().await;
        ib.pick("m1/a1", ANSWER_REQ, "allow").unwrap();
        if how == "4408" {
            c.close(4408).await
        } else {
            c.send_raw("{\"type\":\"agent.updated\",\"epoch\":\"e1\",\"seq\":2}")
                .await
        }
        let c2 = f.next_connection().await;
        if how == "resync" {
            c2.send(serde_json::json!({"type":"snapshot","epoch":"e1","seq":1,"agents":[pending_agent()]})).await
        }
        view_until(&ib, "offline cancellation", |v| {
            v.actions.is_empty()
                && v.cards
                    .first()
                    .and_then(|c| c.notice.as_ref())
                    .is_some_and(|n| n.text == "Offline. Nothing was sent.")
        })
        .await;
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert_eq!(answers(&f), 0, "{how}");
    }
}
#[tokio::test]
async fn second_pick_on_the_same_card_is_ignored() {
    let (_, _, ib) = setup().await;
    ib.pick("m1/a1", ANSWER_REQ, "allow").unwrap();
    assert_eq!(
        ib.pick("m1/a1", ANSWER_REQ, "deny"),
        Err(PickError::AlreadyCounting)
    );
}
#[tokio::test]
async fn unknown_effect_option_cannot_be_picked() {
    let (_f, c, ib) = setup().await;
    let mut agent = pending_agent();
    agent["pending"]["options"][0]["effect"] = Value::String("weird".into());
    c.send(serde_json::json!({"type":"agent.updated","epoch":"e1","seq":2,"agent":agent}))
        .await;
    view_until(&ib, "answerable card", |v| {
        v.cards
            .iter()
            .any(|c| c.answerable && !c.selectable_options.iter().any(|id| id == "allow"))
    })
    .await;
    assert_eq!(
        ib.pick("m1/a1", ANSWER_REQ, "allow"),
        Err(PickError::UnknownOption)
    );
}
#[tokio::test]
async fn reply_goes_through_the_undo() {
    let (f, c, ib) = setup().await;
    let mut idle = pending_agent();
    idle["state"] = Value::String("idle".into());
    idle["pending"] = Value::Null;
    c.send(serde_json::json!({"type":"agent.updated","epoch":"e1","seq":2,"agent":idle}))
        .await;
    ib.set_show_idle(true);
    view_until(&ib, "reply card", |v| v.cards.iter().any(|c| c.can_reply)).await;
    assert_eq!(ib.reply("m1/a1", "   ".into()), Err(PickError::BadText));
    ib.reply("m1/a1", "next step".into()).unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        f.received()
            .iter()
            .filter(|v| v["type"] == "agent.send")
            .count(),
        0
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    let sent = f
        .received()
        .iter()
        .filter(|v| v["type"] == "agent.send")
        .count();
    assert_eq!(sent, 1);
}
