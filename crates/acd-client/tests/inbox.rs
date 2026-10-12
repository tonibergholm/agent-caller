mod common;
use acd_client::actions::ActionOwner;
use acd_client::{fake::*, inbox::*, settings::*, view::*, voice::*};
use serde_json::Value;
use std::{sync::Arc, time::Duration};
const ANSWER_REQ: &str = "r1";
fn pending_agent() -> Value {
    serde_json::json!({"id":"a1","machine_id":"m","source":"herdr","kind":"claude","title":"agent","cwd":null,"project":null,"state":"needs_you","state_since":"2026-10-08T00:00:00.000Z","failed":false,"last_message":null,"can_send":true,"pending":{"id":ANSWER_REQ,"revision":1,"agent_id":"a1","kind":"permission","status":"open","answer_path":"hook","title":"Bash","detail":"npm test","options":[{"id":"allow","label":"Yes","description":null,"effect":"allow"},{"id":"deny","label":"No","description":null,"effect":"deny"},{"id":"always","label":"Yes, always","description":"Bash(npm test:*)","effect":"allow_always"}],"questions":null,"free_text":false,"raw_screen":null,"opened_at":"2026-10-08T00:00:00.000Z"}})
}
async fn setup_with(timing: acd_client::Timing) -> (FakeServer, FakeConn, Arc<Inbox>) {
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
        client_id: "00000000-0000-4000-8000-000000000001".into(),
        hotkey: DEFAULT_HOTKEY.into(),
        show_idle: false,
        show_subagents: false,
        show_hook_only: false,
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
            timing,
            settings_path: None,
            client_name: "t".into(),
            client_version: "0".into(),
        },
    );
    ib.start();
    ib.start(); // Starting an Inbox again must not replace its machine task or spawn another loop.
    let c = f.next_connection().await;
    assert!(
        f.hellos()
            .iter()
            .any(|hello| hello["client"]["id"] == settings_client_id())
    );
    c.send(serde_json::json!({"type":"snapshot","epoch":"e1","seq":1,"agents":[pending_agent()]}))
        .await;
    view_until(&ib, "answerable", |v| v.cards.iter().any(|c| c.answerable)).await;
    assert_eq!(f.connections(), 1, "start is single-use");
    (f, c, ib)
}
fn settings_client_id() -> &'static str {
    "00000000-0000-4000-8000-000000000001"
}
async fn setup() -> (FakeServer, FakeConn, Arc<Inbox>) {
    setup_with(common::fast()).await
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
    ib.pick("m1/a1", ANSWER_REQ, "allow", 1).unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(answers(&f), 0);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(answers(&f), 1);
}
#[tokio::test]
async fn escape_right_after_pick_cancels_and_sends_nothing() {
    let (f, _, ib) = setup().await;
    ib.pick("m1/a1", ANSWER_REQ, "allow", 1).unwrap();
    assert!(ib.escape());
    assert!(!ib.escape());
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(answers(&f), 0);
}
#[tokio::test]
async fn countdown_cancels_when_request_resolves() {
    let (f, c, ib) = setup().await;
    ib.pick("m1/a1", ANSWER_REQ, "allow", 1).unwrap();
    c.send(serde_json::json!({"type":"request.resolved","epoch":"e1","seq":2,"request_id":ANSWER_REQ,"agent_id":"a1","by":"terminal","option_id":null})).await;
    let mut resolved_agent = pending_agent();
    resolved_agent["pending"] = Value::Null;
    c.send(serde_json::json!({"type":"agent.updated","epoch":"e1","seq":3,"agent":resolved_agent}))
        .await;
    view_until(&ib, "removed action", |v| {
        v.actions.is_empty()
            && v.cards
                .first()
                .and_then(|c| c.notice.as_ref())
                .is_some_and(|n| {
                    matches!(
                        n.text.as_str(),
                        "Answered in the terminal" | "Answered elsewhere"
                    )
                })
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(answers(&f), 0);
}
#[tokio::test]
async fn countdown_cancels_when_option_semantics_change() {
    let (f, c, ib) = setup().await;
    ib.pick("m1/a1", ANSWER_REQ, "always", 1).unwrap();
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
async fn stale_view_revision_is_rejected_without_action_or_frame() {
    let (f, c, ib) = setup().await;
    let shown = ib
        .view()
        .borrow()
        .cards
        .iter()
        .find(|card| card.key == "m1/a1")
        .unwrap()
        .agent
        .pending
        .as_ref()
        .map(|pending| pending.revision);
    assert_eq!(shown, Some(1));
    let mut agent = pending_agent();
    agent["pending"]["revision"] = Value::from(2);
    c.send(serde_json::json!({"type":"agent.updated","epoch":"e1","seq":2,"agent":agent}))
        .await;
    view_until(&ib, "revision 2", |v| {
        v.cards.iter().any(|card| {
            card.key == "m1/a1"
                && card
                    .agent
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.revision == 2)
        })
    })
    .await;
    assert_eq!(
        ib.pick("m1/a1", ANSWER_REQ, "allow", shown.unwrap()),
        Err(PickError::StaleRequest)
    );
    assert!(ib.view().borrow().actions.is_empty());
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(answers(&f), 0, "stale revision must not send a frame");
}
#[tokio::test]
async fn reconnect_within_undo_window_cancels() {
    for how in ["4408", "resync"] {
        let (f, c, ib) = setup().await;
        ib.pick("m1/a1", ANSWER_REQ, "allow", 1).unwrap();
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
    ib.pick("m1/a1", ANSWER_REQ, "allow", 1).unwrap();
    assert_eq!(
        ib.pick("m1/a1", ANSWER_REQ, "deny", 1),
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
        ib.pick("m1/a1", ANSWER_REQ, "allow", 1),
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
        .filter(|v| v["type"] == "agent.send" && v["agent_id"] == "a1" && v["text"] == "next step")
        .count();
    assert_eq!(sent, 1);
}

#[tokio::test]
async fn import_pairing_stores_the_secret_and_never_reports_it() {
    let f = FakeServer::start(common::TOKEN, Identity::Paired).await;
    let secrets = Arc::new(MemorySecrets::default());
    let ib = Inbox::new(
        Settings {
            machines: vec![],
            ..Settings::fresh()
        },
        secrets.clone(),
        InboxOptions {
            timing: common::fast(),
            settings_path: None,
            client_name: "t".into(),
            client_version: "0".into(),
        },
    );
    ib.start();
    let line = format!(
        "acd://127.0.0.1:{}?pin={}#{}",
        f.port(),
        f.pin(),
        common::TOKEN
    );
    let err = ib.import_pairing("garbage #secret-ish").unwrap_err();
    assert!(!err.contains("secret-ish"));
    let e = ib.import_pairing(&line).unwrap();
    let stored = MachineSecret::from_json(&secrets.get(&e.key).unwrap().unwrap()).unwrap();
    assert_eq!(
        (stored.token.as_str(), stored.pin.clone()),
        (common::TOKEN, f.pin())
    );
    let listed = ib.machines();
    assert!(
        listed
            .iter()
            .any(|m| m.key == e.key && m.pairing == PairingState::Paired)
    );
    let json = serde_json::to_string(&listed).unwrap()
        + &serde_json::to_string(&*ib.view().borrow()).unwrap();
    assert!(
        !json.contains(common::TOKEN) && !json.contains(&f.pin()),
        "no secret or pin in anything the webview gets"
    );
    let c = f.next_connection().await;
    let snapshot = serde_json::json!({"type":"snapshot","epoch":"e1","seq":1,"agents":[]});
    c.send(snapshot).await;
    view_until(&ib, "online", |v| {
        v.machines
            .iter()
            .any(|m| m.key == e.key && m.status == acd_client::conn::MachineStatus::Online)
    })
    .await;
    let again = ib.import_pairing(&line).unwrap();
    assert_eq!(
        again.key, e.key,
        "re-pairing the same host:port updates the entry"
    );
    tokio::time::timeout(Duration::from_secs(3), async {
        while f.connections() < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("replacement connection should reach the fake server");
}

#[tokio::test]
async fn start_after_import_keeps_the_imported_connection_handle() {
    let f = FakeServer::start(common::TOKEN, Identity::Paired).await;
    let ib = Inbox::new(
        Settings {
            machines: vec![],
            ..Settings::fresh()
        },
        Arc::new(MemorySecrets::default()),
        InboxOptions {
            timing: common::fast(),
            settings_path: None,
            client_name: "t".into(),
            client_version: "0".into(),
        },
    );
    let line = format!(
        "acd://127.0.0.1:{}?pin={}#{}",
        f.port(),
        f.pin(),
        common::TOKEN
    );
    let entry = ib.import_pairing(&line).unwrap();
    let conn = f.next_connection().await;
    ib.start();
    let snapshot = serde_json::json!({"type":"snapshot","epoch":"e1","seq":1,"agents":[]});
    conn.send(snapshot).await;
    view_until(&ib, "imported machine online", |v| {
        v.machines
            .iter()
            .any(|m| m.key == entry.key && m.status == acd_client::conn::MachineStatus::Online)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        f.connections(),
        1,
        "start must retain the running imported handle"
    );
}

#[tokio::test]
async fn notifications_only_for_opened_events_and_deduped() {
    let (f, c, ib) = setup().await;
    let mut events = ib.events();
    let mut pending = pending_agent()["pending"].clone();
    pending["title"] = Value::String("Bash".into());
    c.send(serde_json::json!({"type":"snapshot","epoch":"e1","seq":1,"agents":[pending_agent()]}))
        .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    c.send(serde_json::json!({"type":"request.opened","epoch":"e1","seq":2,"request":pending}))
        .await;
    let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        event,
        InboxEvent::RequestOpened {
            machine_key: "m1".into(),
            machine_name: "Box".into(),
            agent_title: "agent".into(),
            request_title: "Bash".into(),
            request_id: ANSWER_REQ.into()
        }
    );
    c.close(1001).await;
    let c2 = f.next_connection().await;
    c2.send(serde_json::json!({"type":"snapshot","epoch":"e1","seq":1,"agents":[pending_agent()]}))
        .await;
    c2.send(serde_json::json!({"type":"request.opened","epoch":"e1","seq":2,"request":pending_agent()["pending"]})).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    assert_eq!(f.connections(), 2);
}

#[tokio::test]
async fn duplicate_machine_is_hidden() {
    let f = FakeServer::start(common::TOKEN, Identity::Paired).await;
    let secrets = Arc::new(MemorySecrets::default());
    for key in ["m1", "m2"] {
        secrets
            .set(
                key,
                &MachineSecret {
                    token: common::TOKEN.into(),
                    pin: f.pin(),
                }
                .to_json(),
            )
            .unwrap();
    }
    let mut settings = Settings {
        client_id: settings_client_id().into(),
        hotkey: DEFAULT_HOTKEY.into(),
        show_idle: false,
        show_subagents: false,
        show_hook_only: false,
        machines: vec![],
    };
    for (key, name) in [("m1", "One"), ("m2", "Two")] {
        settings.machines.push(MachineEntry {
            key: key.into(),
            name: name.into(),
            endpoint: EndpointConfig::Tcp {
                host: "127.0.0.1".into(),
                port: f.port(),
            },
        });
    }
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
    let c1 = f.next_connection().await;
    let c2 = f.next_connection().await;
    let snapshot = serde_json::json!({"type":"snapshot","epoch":"e1","seq":1,"machine":{"id":"same","name":"daemon"},"agents":[pending_agent()]});
    c1.send(snapshot.clone()).await;
    c2.send(snapshot).await;
    view_until(&ib, "duplicate hidden", |v| {
        v.machines
            .iter()
            .any(|m| m.key == "m2" && m.duplicate_of.as_deref() == Some("m1"))
            && v.cards.iter().all(|c| !c.key.starts_with("m2/"))
    })
    .await;
}

#[tokio::test]
async fn repairing_inside_the_undo_window_sends_nothing() {
    let (f, _, ib) = setup().await;
    ib.pick("m1/a1", ANSWER_REQ, "allow", 1).unwrap();
    let line = format!(
        "acd://127.0.0.1:{}?pin={}#{}",
        f.port(),
        f.pin(),
        common::TOKEN
    );
    ib.import_pairing(&line).unwrap();
    let new_conn = f.next_connection().await;
    new_conn
        .send(
            serde_json::json!({"type":"snapshot","epoch":"e1","seq":1,"agents":[pending_agent()]}),
        )
        .await;
    view_until(&ib, "replacement online", |v| {
        v.machines
            .iter()
            .any(|m| m.key == "m1" && m.status == acd_client::conn::MachineStatus::Online)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(answers(&f), 0);
    let view = ib.view();
    assert!(view.borrow().actions.is_empty());
    assert!(
        view.borrow()
            .cards
            .iter()
            .find(|c| c.key == "m1/a1")
            .and_then(|c| c.notice.as_ref())
            .is_some_and(|n| n.text == "Offline. Nothing was sent.")
    );
    ib.pick("m1/a1", ANSWER_REQ, "allow", 1).unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(answers(&f), 1);
}

#[tokio::test]
async fn removing_a_machine_cancels_its_countdowns() {
    let (f, _, ib) = setup().await;
    ib.pick("m1/a1", ANSWER_REQ, "allow", 1).unwrap();
    ib.remove_machine("m1").unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(answers(&f), 0);
    assert!(ib.view().borrow().actions.is_empty());
}

struct EchoFailSecrets;
impl SecretStore for EchoFailSecrets {
    fn get(&self, _: &str) -> Result<Option<String>, SecretError> {
        Ok(None)
    }
    fn set(&self, _: &str, value: &str) -> Result<(), SecretError> {
        Err(SecretError(value.into()))
    }
    fn delete(&self, _: &str) -> Result<(), SecretError> {
        Ok(())
    }
}

fn empty_inbox(
    secrets: Arc<dyn SecretStore>,
    settings_path: Option<std::path::PathBuf>,
) -> Arc<Inbox> {
    Inbox::new(
        Settings {
            machines: vec![],
            ..Settings::fresh()
        },
        secrets,
        InboxOptions {
            timing: common::fast(),
            settings_path,
            client_name: "t".into(),
            client_version: "0".into(),
        },
    )
}

#[tokio::test]
async fn secret_backend_errors_never_echo_pairing_credentials() {
    let f = FakeServer::start(common::TOKEN, Identity::Paired).await;
    let ib = empty_inbox(Arc::new(EchoFailSecrets), None);
    let line = format!(
        "acd://127.0.0.1:{}?pin={}#{}",
        f.port(),
        f.pin(),
        common::TOKEN
    );
    let error = ib.import_pairing(&line).unwrap_err();
    assert!(!error.contains(common::TOKEN));
    assert!(!error.contains(&f.pin()));
    assert_eq!(error, "Could not save machine credentials");
}

#[tokio::test]
async fn pairing_import_uses_full_ip_as_default_machine_name() {
    let f = FakeServer::start(common::TOKEN, Identity::Paired).await;
    let ib = empty_inbox(Arc::new(MemorySecrets::default()), None);
    let line = format!(
        "acd://127.0.0.1:{}?pin={}#{}",
        f.port(),
        f.pin(),
        common::TOKEN
    );
    assert_eq!(ib.import_pairing(&line).unwrap().name, "127.0.0.1");
}

#[tokio::test]
async fn save_failure_preserves_import_rename_remove_and_hotkey_state() {
    let f = FakeServer::start(common::TOKEN, Identity::Paired).await;
    let d = tempfile::tempdir().unwrap();
    let blocker = d.path().join("not-a-directory");
    std::fs::write(&blocker, "file").unwrap();
    let settings_path = blocker.join("settings.json");
    let secrets = Arc::new(MemorySecrets::default());
    let ib = Inbox::new(
        Settings {
            machines: vec![MachineEntry {
                key: "11111111-1111-4111-8111-111111111111".into(),
                name: "Old".into(),
                endpoint: EndpointConfig::Tcp {
                    host: "127.0.0.1".into(),
                    port: f.port(),
                },
            }],
            ..Settings::fresh()
        },
        secrets.clone(),
        InboxOptions {
            timing: common::fast(),
            settings_path: Some(settings_path),
            client_name: "t".into(),
            client_version: "0".into(),
        },
    );
    secrets
        .set(
            "11111111-1111-4111-8111-111111111111",
            &MachineSecret {
                token: common::TOKEN.into(),
                pin: f.pin(),
            }
            .to_json(),
        )
        .unwrap();
    let before = ib.settings();
    assert!(
        ib.rename_machine("11111111-1111-4111-8111-111111111111", "New")
            .is_err()
    );
    assert_eq!(ib.settings(), before);
    assert!(ib.set_hotkey("Ctrl+X").is_err());
    assert_eq!(ib.settings(), before);
    assert!(
        ib.remove_machine("11111111-1111-4111-8111-111111111111")
            .is_err()
    );
    assert_eq!(ib.settings(), before);
    assert!(
        secrets
            .get("11111111-1111-4111-8111-111111111111")
            .unwrap()
            .is_some()
    );
    let line = format!(
        "acd://127.0.0.1:{}?pin={}#{}",
        f.port(),
        "QWERTYUIOPASDFGHJKLZXCVBNM1234567890abcd",
        "NEW-IMPORT-TOKEN-THAT-MUST-NOT-REMAIN"
    );
    assert!(ib.import_pairing(&line).is_err());
    assert_eq!(ib.settings(), before);
    assert_eq!(
        secrets
            .get("11111111-1111-4111-8111-111111111111")
            .unwrap()
            .unwrap(),
        MachineSecret {
            token: common::TOKEN.into(),
            pin: f.pin()
        }
        .to_json()
    );
}

#[tokio::test]
async fn import_reports_failed_credential_recovery_without_leaking_details() {
    use std::sync::Mutex;
    struct FailingRecovery(Mutex<(usize, Option<String>)>);
    impl SecretStore for FailingRecovery {
        fn get(&self, _: &str) -> Result<Option<String>, SecretError> {
            Ok(self.0.lock().unwrap().1.clone())
        }
        fn set(&self, _: &str, value: &str) -> Result<(), SecretError> {
            let mut state = self.0.lock().unwrap();
            state.0 += 1;
            if state.0 == 2 {
                return Err(SecretError("private secret detail".into()));
            }
            state.1 = Some(value.into());
            Ok(())
        }
        fn delete(&self, _: &str) -> Result<(), SecretError> {
            self.0.lock().unwrap().1 = None;
            Ok(())
        }
    }
    let f = FakeServer::start(common::TOKEN, Identity::Paired).await;
    let d = tempfile::tempdir().unwrap();
    let blocker = d.path().join("file");
    std::fs::write(&blocker, "block").unwrap();
    let key = "11111111-1111-4111-8111-111111111111";
    let secrets = Arc::new(FailingRecovery(Mutex::new((
        0,
        Some("previous secret".into()),
    ))));
    let ib = Inbox::new(
        Settings {
            machines: vec![MachineEntry {
                key: key.into(),
                name: "Box".into(),
                endpoint: EndpointConfig::Tcp {
                    host: "127.0.0.1".into(),
                    port: f.port(),
                },
            }],
            ..Settings::fresh()
        },
        secrets,
        InboxOptions {
            timing: common::fast(),
            settings_path: Some(blocker.join("settings.json")),
            client_name: "t".into(),
            client_version: "0".into(),
        },
    );
    let line = format!(
        "acd://127.0.0.1:{}?pin={}#{}",
        f.port(),
        f.pin(),
        common::TOKEN
    );
    assert_eq!(
        ib.import_pairing(&line).unwrap_err(),
        "Machine pairing recovery failed"
    );
}

#[tokio::test]
async fn remove_reports_failed_settings_recovery_without_leaking_details() {
    struct BreakSettingsOnDelete(std::path::PathBuf);
    impl SecretStore for BreakSettingsOnDelete {
        fn get(&self, _: &str) -> Result<Option<String>, SecretError> {
            Ok(Some("old credentials".into()))
        }
        fn set(&self, _: &str, _: &str) -> Result<(), SecretError> {
            Ok(())
        }
        fn delete(&self, _: &str) -> Result<(), SecretError> {
            std::fs::remove_file(&self.0).unwrap();
            std::fs::create_dir(&self.0).unwrap();
            Err(SecretError("private secret detail".into()))
        }
    }
    let d = tempfile::tempdir().unwrap();
    let settings_path = d.path().join("settings.json");
    let key = "11111111-1111-4111-8111-111111111111";
    let ib = Inbox::new(
        Settings {
            machines: vec![MachineEntry {
                key: key.into(),
                name: "Box".into(),
                endpoint: EndpointConfig::Tcp {
                    host: "127.0.0.1".into(),
                    port: 1234,
                },
            }],
            ..Settings::fresh()
        },
        Arc::new(BreakSettingsOnDelete(settings_path.clone())),
        InboxOptions {
            timing: common::fast(),
            settings_path: Some(settings_path),
            client_name: "t".into(),
            client_version: "0".into(),
        },
    );
    let error = ib.remove_machine(key).unwrap_err();
    assert_eq!(error, "Machine removal recovery failed");
    assert!(!error.contains("private secret detail"));
}

#[tokio::test]
async fn unreadable_pairing_state_is_reported() {
    let f = FakeServer::start(common::TOKEN, Identity::Paired).await;
    let secrets = Arc::new(MemorySecrets::default());
    secrets.set("m1", "not-json").unwrap();
    let ib = Inbox::new(
        Settings {
            machines: vec![MachineEntry {
                key: "m1".into(),
                name: "Box".into(),
                endpoint: EndpointConfig::Tcp {
                    host: "127.0.0.1".into(),
                    port: f.port(),
                },
            }],
            ..Settings::fresh()
        },
        secrets,
        InboxOptions {
            timing: common::fast(),
            settings_path: None,
            client_name: "t".into(),
            client_version: "0".into(),
        },
    );
    assert_eq!(ib.machines()[0].pairing, PairingState::Unreadable);
}

#[tokio::test]
async fn rename_and_hotkey_are_persisted() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("settings.json");
    let ib = Inbox::new(
        Settings::fresh(),
        Arc::new(MemorySecrets::default()),
        InboxOptions {
            timing: common::fast(),
            settings_path: Some(path.clone()),
            client_name: "t".into(),
            client_version: "0".into(),
        },
    );
    ib.rename_machine("local", "Work Mac").unwrap();
    ib.set_hotkey("Ctrl+Space").unwrap();
    let saved = Settings::load_or_init(&path).unwrap();
    assert_eq!(saved.machines[0].name, "Work Mac");
    assert_eq!(saved.hotkey, "Ctrl+Space");
}

#[tokio::test]
async fn resolution_event_sets_terminal_notice() {
    let (_f, c, ib) = setup().await;
    c.send(serde_json::json!({"type":"request.resolved","epoch":"e1","seq":2,"request_id":ANSWER_REQ,"agent_id":"a1","by":"terminal","option_id":null})).await;
    view_until(&ib, "resolution notice", |v| {
        v.cards
            .first()
            .and_then(|c| c.notice.as_ref())
            .is_some_and(|n| n.text == "Answered in the terminal")
    })
    .await;
}

#[tokio::test]
async fn replacement_cancels_machine_with_pending_submit_clone() {
    let (f, old_conn, ib) = setup().await;
    ib.pick("m1/a1", ANSWER_REQ, "allow", 1).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while answers(&f) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let line = format!(
        "acd://127.0.0.1:{}?pin={}#{}",
        f.port(),
        f.pin(),
        common::TOKEN
    );
    ib.import_pairing(&line).unwrap();
    old_conn.close(1001).await;
    let replacement = f.next_connection().await;
    replacement
        .send(
            serde_json::json!({"type":"snapshot","epoch":"e1","seq":1,"agents":[pending_agent()]}),
        )
        .await;
    view_until(&ib, "replacement ready after pending submit", |v| {
        v.machines
            .iter()
            .any(|m| m.key == "m1" && m.status == acd_client::conn::MachineStatus::Online)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(
        answers(&f),
        1,
        "old submit must not retry after replacement"
    );
    assert_eq!(f.connections(), 2, "cancelled machine must not reconnect");
}

#[tokio::test]
async fn removal_cancels_machine_with_pending_submit_clone() {
    let (f, old_conn, ib) = setup().await;
    ib.pick("m1/a1", ANSWER_REQ, "allow", 1).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while answers(&f) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    ib.remove_machine("m1").unwrap();
    old_conn.close(1001).await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(
        answers(&f),
        1,
        "removed machine must not retry its pending submit"
    );
    assert_eq!(f.connections(), 1, "removed machine must not reconnect");
}
#[tokio::test]
async fn typed_answer_needs_a_free_text_request_and_goes_through_the_undo() {
    let (f, c, ib) = setup().await;
    let card = || ib.view().borrow().cards[0].clone();
    assert!(card().answerable && !card().free_text_answerable);
    assert_eq!(
        ib.answer_text("m1/a1", ANSWER_REQ, 1, "purple".into()),
        Err(PickError::NotAnswerable)
    );
    let mut agent = pending_agent();
    agent["pending"]["free_text"] = Value::Bool(true);
    c.send(serde_json::json!({"type":"agent.updated","epoch":"e1","seq":2,"agent":agent}))
        .await;
    view_until(&ib, "free text card", |v| {
        v.cards.iter().any(|c| c.free_text_answerable)
    })
    .await;
    for bad in [
        "  \n ",
        "a\u{1b}b",
        "a\u{202E}b",
        "a\u{2028}b",
        &"x".repeat(16 * 1024 + 1),
    ] {
        assert_eq!(
            ib.answer_text("m1/a1", ANSWER_REQ, 1, bad.into()),
            Err(PickError::BadAnswerText)
        );
    }
    assert_eq!(
        ib.answer_text("m1/a1", ANSWER_REQ, 9, "purple".into()),
        Err(PickError::StaleRequest)
    );
    ib.answer_text("m1/a1", ANSWER_REQ, 1, "  purple\r\nplease ".into())
        .unwrap();
    view_until(&ib, "countdown", |v| !v.actions.is_empty()).await;
    let action = ib.view().borrow().actions[0].clone();
    assert_eq!(action.detail.as_deref(), Some("purple\nplease"));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(answers(&f), 0);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let sent: Vec<_> = f
        .received()
        .into_iter()
        .filter(|v| v["type"] == "request.answer")
        .collect();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["request_id"], ANSWER_REQ);
    assert_eq!(sent[0]["text"], "purple\nplease");
    assert!(sent[0].get("option_id").is_none_or(Value::is_null));
}
#[tokio::test]
async fn typed_answer_countdown_cancels_when_free_text_is_withdrawn() {
    let (f, c, ib) = setup().await;
    let mut agent = pending_agent();
    agent["pending"]["free_text"] = Value::Bool(true);
    c.send(serde_json::json!({"type":"agent.updated","epoch":"e1","seq":2,"agent":agent}))
        .await;
    view_until(&ib, "free text card", |v| {
        v.cards.iter().any(|c| c.free_text_answerable)
    })
    .await;
    ib.answer_text("m1/a1", ANSWER_REQ, 1, "purple".into())
        .unwrap();
    c.send(
        serde_json::json!({"type":"agent.updated","epoch":"e1","seq":3,"agent":pending_agent()}),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(answers(&f), 0);
}
#[tokio::test]
async fn typed_answer_countdown_cancels_when_the_revision_bumps() {
    let (f, c, ib) = setup().await;
    let mut agent = pending_agent();
    agent["pending"]["free_text"] = Value::Bool(true);
    c.send(serde_json::json!({"type":"agent.updated","epoch":"e1","seq":2,"agent":agent.clone()}))
        .await;
    view_until(&ib, "free text card", |v| {
        v.cards.iter().any(|c| c.free_text_answerable)
    })
    .await;
    ib.answer_text("m1/a1", ANSWER_REQ, 1, "purple".into())
        .unwrap();
    agent["pending"]["revision"] = serde_json::json!(2);
    c.send(serde_json::json!({"type":"agent.updated","epoch":"e1","seq":3,"agent":agent}))
        .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(answers(&f), 0, "a changed revision must not send");
}

fn subagent_agent() -> Value {
    let mut a = pending_agent();
    a["id"] = "a2".into();
    a["title"] = "sub".into();
    a["role"] = "subagent".into();
    a["parent_title"] = "agent".into();
    a["pending"]["id"] = "r2".into();
    a["pending"]["agent_id"] = "a2".into();
    a
}

#[tokio::test]
async fn subagents_are_hidden_and_silent_until_shown() {
    let (_f, c, ib) = setup().await;
    let mut events = ib.events();
    c.send(serde_json::json!({"type":"snapshot","epoch":"e1","seq":2,"agents":[pending_agent(), subagent_agent()]}))
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let v = ib.view().borrow().clone();
    assert_eq!(v.cards.len(), 1, "the subagent is not a card");
    assert_eq!(v.tray.needs_you, 1, "nor is it counted");
    assert!(!v.show_subagents);
    // Its request does not notify while hidden.
    c.send(serde_json::json!({"type":"request.opened","epoch":"e1","seq":3,"request":subagent_agent()["pending"]}))
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    ib.set_show_subagents(true).unwrap();
    view_until(&ib, "subagent shown", |v| {
        v.show_subagents
            && v.cards.len() == 2
            && v.tray.needs_you == 2
            && v.cards.iter().any(|c| {
                c.agent.role == acd_protocol::AgentRole::Subagent
                    && c.agent.parent_title.as_deref() == Some("agent")
            })
    })
    .await;
    ib.set_show_subagents(false).unwrap();
    view_until(&ib, "hidden again", |v| v.cards.len() == 1 && v.tray.needs_you == 1).await;
}

#[tokio::test]
async fn show_subagents_is_remembered() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("settings.json");
    let ib = empty_inbox(Arc::new(MemorySecrets::default()), Some(path.clone()));
    assert!(!Settings::load_or_init(&path).map(|s| s.show_subagents).unwrap_or(false));
    ib.set_show_subagents(true).unwrap();
    assert!(Settings::load_or_init(&path).unwrap().show_subagents);
    assert!(ib.settings().show_subagents);
}

#[tokio::test]
async fn hiding_subagents_cancels_their_countdown_so_nothing_fires_unseen() {
    let (f, c, ib) = setup().await;
    ib.set_show_subagents(true).unwrap();
    c.send(serde_json::json!({"type":"snapshot","epoch":"e1","seq":2,"agents":[pending_agent(), subagent_agent()]}))
        .await;
    view_until(&ib, "subagent shown", |v| v.cards.len() == 2).await;
    ib.pick("m1/a2", "r2", "allow", 1).unwrap();
    view_until(&ib, "counting", |v| v.actions.len() == 1).await;
    ib.set_show_subagents(false).unwrap();
    // Esc has nothing invisible to cancel, and nothing is left to fire.
    assert!(!ib.escape(), "no hidden action is left for Esc");
    view_until(&ib, "no actions", |v| v.actions.is_empty() && v.cards.len() == 1).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(answers(&f), 0, "the hidden countdown never fired");
    // Showing them again does not bring the action back.
    ib.set_show_subagents(true).unwrap();
    view_until(&ib, "shown again", |v| v.cards.len() == 2).await;
    assert!(ib.view().borrow().actions.is_empty());
}

#[tokio::test]
async fn a_reply_countdown_survives_the_agent_turning_idle_with_idle_hidden() {
    let (f, c, ib) = setup().await;
    let mut working = pending_agent();
    working["state"] = Value::String("working".into());
    working["pending"] = Value::Null;
    c.send(serde_json::json!({"type":"agent.updated","epoch":"e1","seq":2,"agent":working.clone()}))
        .await;
    view_until(&ib, "reply card", |v| v.cards.iter().any(|c| c.can_reply)).await;
    ib.reply("m1/a1", "next step".into()).unwrap();
    view_until(&ib, "counting", |v| v.actions.len() == 1).await;
    // A remote event idles the agent while idle agents are hidden.
    let mut idle = working;
    idle["state"] = Value::String("idle".into());
    c.send(serde_json::json!({"type":"agent.updated","epoch":"e1","seq":3,"agent":idle}))
        .await;
    view_until(&ib, "idle but kept", |v| {
        v.cards.iter().any(|c| c.agent.state == acd_protocol::AgentState::Idle && c.action.is_some())
    })
    .await;
    assert_eq!(ib.view().borrow().hidden_idle, 0);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let sent = f
        .received()
        .iter()
        .filter(|v| v["type"] == "agent.send" && v["text"] == "next step")
        .count();
    assert_eq!(sent, 1, "the reply was still delivered");
    view_until(&ib, "hidden once done", |v| v.cards.is_empty() && v.hidden_idle == 1).await;
}

/// Countdowns that never end on their own; tests end them with
/// `expire_countdowns_for_test`, so ordering is decided by the test.
fn held() -> acd_client::Timing {
    acd_client::Timing { undo: Duration::from_secs(3600), ..common::fast() }
}

fn working_agent(id: &str) -> Value {
    let mut a = pending_agent();
    a["id"] = id.into();
    a["state"] = "working".into();
    a["pending"] = Value::Null;
    a
}

async fn next_outcome(rx: &mut tokio::sync::broadcast::Receiver<ActionEvent>, id: &str) -> ActionOutcome {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = rx.recv().await.expect("the outcome stream is open");
            if event.action_id == id {
                return event.outcome;
            }
        }
    })
    .await
    .expect("timed out waiting for an action outcome")
}

/// The first frame of this type the fake daemon received.
async fn frame_of_type(f: &FakeServer, ty: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(v) = f.received().into_iter().find(|v| v["type"] == ty) {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no {ty} frame"))
}

async fn answer_frame(f: &FakeServer) -> Value {
    frame_of_type(f, "request.answer").await
}

fn guard_for(ib: &Inbox, card: &str) -> Guard {
    let (machine, agent) = card.split_once('/').unwrap();
    let request = ib
        .view()
        .borrow()
        .cards
        .iter()
        .find(|c| c.key == card)
        .and_then(|c| c.agent.pending.as_ref().map(|r| (r.id.clone(), r.revision)));
    Guard { conn: ib.conn_stamp(machine).unwrap(), agent_id: agent.into(), request, named_send_only: false }
}

#[tokio::test]
async fn a_delivered_answer_reports_fired_then_delivered() {
    let (f, c, ib) = setup().await;
    let mut rx = ib.action_outcomes();
    let id = ib.pick("m1/a1", ANSWER_REQ, "allow", 1).unwrap();
    assert_eq!(next_outcome(&mut rx, &id).await, ActionOutcome::Fired);
    let frame = answer_frame(&f).await;
    c.send(serde_json::json!({"type":"result","id":frame["id"],"ok":true,"outcome":"delivered"})).await;
    assert_eq!(next_outcome(&mut rx, &id).await, ActionOutcome::Delivered);
}

#[tokio::test]
async fn a_rejected_answer_reports_its_code() {
    let (f, c, ib) = setup().await;
    let mut rx = ib.action_outcomes();
    let id = ib.pick("m1/a1", ANSWER_REQ, "allow", 1).unwrap();
    assert_eq!(next_outcome(&mut rx, &id).await, ActionOutcome::Fired);
    let frame = answer_frame(&f).await;
    c.send(serde_json::json!({"type":"result","id":frame["id"],"ok":false,"error":{"code":"request_gone","message":"gone"}})).await;
    assert_eq!(next_outcome(&mut rx, &id).await, ActionOutcome::Rejected { code: acd_protocol::ErrorCode::RequestGone });
}

#[tokio::test]
async fn an_answer_lost_across_a_daemon_restart_is_uncertain() {
    let (f, c, ib) = setup().await;
    let mut rx = ib.action_outcomes();
    let id = ib.pick("m1/a1", ANSWER_REQ, "allow", 1).unwrap();
    assert_eq!(next_outcome(&mut rx, &id).await, ActionOutcome::Fired);
    answer_frame(&f).await;
    // A new epoch revokes every in-flight command (commands.rs on_welcome).
    f.set_epoch("e2");
    c.close(1001).await;
    let c2 = f.next_connection().await;
    c2.send(serde_json::json!({"type":"snapshot","epoch":"e2","seq":1,"agents":[]})).await;
    assert_eq!(next_outcome(&mut rx, &id).await, ActionOutcome::Uncertain);
}

#[tokio::test]
async fn cancel_and_escape_report_cancelled() {
    let (_f, _c, ib) = setup_with(held()).await;
    let mut rx = ib.action_outcomes();
    let id = ib.pick("m1/a1", ANSWER_REQ, "allow", 1).unwrap();
    assert!(ib.cancel(&id));
    assert_eq!(next_outcome(&mut rx, &id).await, ActionOutcome::Cancelled);
    let id = ib.pick("m1/a1", ANSWER_REQ, "deny", 1).unwrap();
    assert!(ib.escape());
    assert_eq!(next_outcome(&mut rx, &id).await, ActionOutcome::Cancelled);
}

#[tokio::test]
async fn dropped_countdowns_report_why_nothing_was_sent() {
    // The request changed.
    let (_f, c, ib) = setup_with(held()).await;
    let mut rx = ib.action_outcomes();
    let id = ib.pick("m1/a1", ANSWER_REQ, "allow", 1).unwrap();
    let mut agent = pending_agent();
    agent["pending"]["revision"] = serde_json::json!(2);
    c.send(serde_json::json!({"type":"agent.updated","epoch":"e1","seq":2,"agent":agent})).await;
    assert_eq!(next_outcome(&mut rx, &id).await, ActionOutcome::NotSent { reason: NotSentReason::Changed });
    // The connection ended.
    let (_f, _c, ib) = setup_with(held()).await;
    let mut rx = ib.action_outcomes();
    let id = ib.pick("m1/a1", ANSWER_REQ, "allow", 1).unwrap();
    ib.drop_connections_for_test();
    assert_eq!(next_outcome(&mut rx, &id).await, ActionOutcome::NotSent { reason: NotSentReason::Offline });
    // The user hid the card.
    let (_f, c, ib) = setup_with(held()).await;
    ib.set_show_subagents(true).unwrap();
    c.send(serde_json::json!({"type":"snapshot","epoch":"e1","seq":2,"agents":[pending_agent(), subagent_agent()]})).await;
    view_until(&ib, "subagent shown", |v| v.cards.len() == 2).await;
    let mut rx = ib.action_outcomes();
    let id = ib.pick("m1/a2", "r2", "allow", 1).unwrap();
    ib.set_show_subagents(false).unwrap();
    assert_eq!(next_outcome(&mut rx, &id).await, ActionOutcome::NotSent { reason: NotSentReason::Hidden });
}

/// Review Focus 5, deterministic half one: a cancelled countdown never
/// fires. A sentinel on the same card proves the timer ran after the cancel.
#[tokio::test]
async fn a_cancelled_countdown_never_fires() {
    let (f, _c, ib) = setup_with(held()).await;
    let mut rx = ib.action_outcomes();
    let id = ib.pick("m1/a1", ANSWER_REQ, "allow", 1).unwrap();
    assert!(ib.cancel(&id));
    ib.expire_countdowns_for_test();
    let sentinel = ib.pick("m1/a1", ANSWER_REQ, "deny", 1).unwrap();
    ib.expire_countdowns_for_test();
    let mut seen = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let e = rx.recv().await.unwrap();
            let done = e.action_id == sentinel && e.outcome == ActionOutcome::Fired;
            seen.push(e);
            if done {
                break;
            }
        }
    })
    .await
    .expect("the sentinel fired");
    let mine: Vec<ActionOutcome> = seen.iter().filter(|e| e.action_id == id).map(|e| e.outcome).collect();
    assert_eq!(mine, [ActionOutcome::Cancelled]);
    assert_eq!(answer_frame(&f).await["option_id"], "deny", "only the sentinel was sent");
}

/// Review Focus 5, half two: once `Fired` is out, cancel is refused.
#[tokio::test]
async fn a_fired_countdown_cannot_be_cancelled() {
    let (f, _c, ib) = setup_with(held()).await;
    let mut rx = ib.action_outcomes();
    let id = ib.pick("m1/a1", ANSWER_REQ, "allow", 1).unwrap();
    ib.expire_countdowns_for_test();
    assert_eq!(next_outcome(&mut rx, &id).await, ActionOutcome::Fired);
    assert!(!ib.cancel(&id), "the countdown ended");
    assert_eq!(answer_frame(&f).await["option_id"], "allow");
}

#[tokio::test]
async fn actions_work_with_no_outcome_subscriber() {
    let (f, _c, ib) = setup_with(held()).await;
    ib.pick("m1/a1", ANSWER_REQ, "allow", 1).unwrap();
    ib.expire_countdowns_for_test();
    assert_eq!(answer_frame(&f).await["option_id"], "allow");
}

#[tokio::test]
async fn a_slow_subscriber_is_told_it_lagged_and_the_stream_goes_on() {
    let (_f, _c, ib) = setup_with(held()).await;
    let mut rx = ib.action_outcomes();
    for _ in 0..300 {
        let id = ib.pick("m1/a1", ANSWER_REQ, "allow", 1).unwrap();
        assert!(ib.cancel(&id));
    }
    assert!(matches!(rx.recv().await, Err(tokio::sync::broadcast::error::RecvError::Lagged(_))));
    let id = ib.pick("m1/a1", ANSWER_REQ, "allow", 1).unwrap();
    assert!(ib.cancel(&id));
    // The queue is still full of old events, so the new one may push more out
    // before it is read: more `Lagged` is fine, the stream must still reach it.
    let outcome = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match rx.recv().await {
                Ok(e) if e.action_id == id => return e.outcome,
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(e) => panic!("the outcome stream closed: {e}"),
            }
        }
    })
    .await
    .expect("the stream goes on after a lag");
    assert_eq!(outcome, ActionOutcome::Cancelled);
}

/// B1: with `rebuild` held across a reconnect, the stamped view keeps the
/// stamps of the cards it shows; a step planned on it is then refused by
/// the live connection. Released, both move together.
#[tokio::test]
async fn the_stamped_view_never_pairs_new_stamps_with_old_cards() {
    let (f, c, ib) = setup().await;
    let before = ib.stamped_view();
    assert_eq!(before.stamps.get("m1").copied(), ib.conn_stamp("m1"));
    ib.hold_rebuild_for_test(true);
    c.close(4408).await;
    let c2 = f.next_connection().await;
    c2.send(serde_json::json!({"type":"snapshot","epoch":"e1","seq":1,"agents":[pending_agent()]})).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while ib.conn_stamp("m1") == before.stamps.get("m1").copied() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the live connection moved on");
    let held = ib.stamped_view();
    assert_eq!(held, before, "stamps stay with the cards they were published with");
    let planned = Guard {
        conn: held.stamps["m1"],
        agent_id: "a1".into(),
        request: Some((ANSWER_REQ.into(), 1)),
        named_send_only: false,
    };
    assert_eq!(ib.pick_guarded("m1/a1", "allow", &planned), Err(PickError::ConnectionChanged));
    ib.hold_rebuild_for_test(false);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let now = ib.stamped_view();
            if now.stamps.get("m1").copied() == ib.conn_stamp("m1") && now.view.cards.iter().any(|c| c.answerable) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("released: view and stamps move together");
}

#[tokio::test]
async fn the_stamped_view_carries_the_idle_cards_the_view_hides() {
    let (_f, c, ib) = setup().await;
    let mut idle = working_agent("a3");
    idle["state"] = "idle".into();
    c.send(serde_json::json!({"type":"agent.updated","epoch":"e1","seq":2,"agent":idle})).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !ib.stamped_view().hidden_idle_cards.iter().any(|c| c.key == "m1/a3") {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the hidden idle card");
    assert!(!ib.view().borrow().cards.iter().any(|c| c.key == "m1/a3"), "the panel still hides it");
}

/// B2: the full guard. Connection, request appearing, disappearing and
/// changing revision are each refused; the current guard passes.
#[tokio::test]
async fn guarded_operations_check_connection_agent_and_request() {
    let (f, c, ib) = setup_with(held()).await;
    // The connection changed.
    let old = guard_for(&ib, "m1/a1");
    c.close(4408).await;
    let c = f.next_connection().await;
    c.send(serde_json::json!({"type":"snapshot","epoch":"e1","seq":1,"agents":[pending_agent(), working_agent("a2")]})).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while ib.conn_stamp("m1") == Some(old.conn) || ib.view().borrow().cards.len() < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("reconnected");
    assert_eq!(ib.pick_guarded("m1/a1", "allow", &old), Err(PickError::ConnectionChanged));
    // A request appeared on an agent planned without one.
    let quiet = guard_for(&ib, "m1/a2");
    assert_eq!(quiet.request, None);
    let mut busy = pending_agent();
    busy["id"] = "a2".into();
    busy["pending"]["id"] = "r9".into();
    busy["pending"]["agent_id"] = "a2".into();
    c.send(serde_json::json!({"type":"agent.updated","epoch":"e1","seq":2,"agent":busy})).await;
    view_until(&ib, "a2 needs you", |v| v.cards.iter().any(|c| c.key == "m1/a2" && c.agent.pending.is_some())).await;
    assert_eq!(ib.reply_guarded("m1/a2", "go on".into(), &quiet), Err(PickError::StaleRequest));
    // The revision changed.
    let planned = guard_for(&ib, "m1/a1");
    let mut bumped = pending_agent();
    bumped["pending"]["revision"] = serde_json::json!(2);
    c.send(serde_json::json!({"type":"agent.updated","epoch":"e1","seq":3,"agent":bumped})).await;
    view_until(&ib, "revision 2", |v| v.cards.iter().any(|c| c.agent.pending.as_ref().is_some_and(|p| p.revision == 2))).await;
    assert_eq!(ib.pick_guarded("m1/a1", "allow", &planned), Err(PickError::StaleRequest));
    assert_eq!(ib.answer_text_guarded("m1/a1", "x".into(), &planned), Err(PickError::StaleRequest));
    // The request disappeared.
    let planned = guard_for(&ib, "m1/a1");
    let mut gone = pending_agent();
    gone["pending"] = Value::Null;
    c.send(serde_json::json!({"type":"agent.updated","epoch":"e1","seq":4,"agent":gone})).await;
    view_until(&ib, "answered", |v| v.cards.iter().any(|c| c.key == "m1/a1" && c.agent.pending.is_none())).await;
    assert_eq!(ib.pick_guarded("m1/a1", "allow", &planned), Err(PickError::StaleRequest));
    assert!(ib.view().borrow().actions.is_empty());
    // The current guard passes.
    let now = guard_for(&ib, "m1/a2");
    ib.pick_guarded("m1/a2", "allow", &now).unwrap();
}

/// D7 at the client: a guarded send reaches an idle agent the panel hides
/// (no published card); the keyboard path still needs a card on screen.
#[tokio::test]
async fn a_guarded_send_reaches_a_hidden_idle_agent() {
    let (f, c, ib) = setup().await;
    let mut idle = working_agent("a3");
    idle["state"] = "idle".into();
    c.send(serde_json::json!({"type":"agent.updated","epoch":"e1","seq":2,"agent":idle})).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !ib.stamped_view().hidden_idle_cards.iter().any(|c| c.key == "m1/a3") {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the hidden idle card");
    assert_eq!(ib.reply("m1/a3", "x".into()), Err(PickError::UnknownCard), "keyboard path unchanged");
    // Only a send that named it while it was out of view reaches it (D7).
    let unnamed = Guard { conn: ib.conn_stamp("m1").unwrap(), agent_id: "a3".into(), request: None, named_send_only: false };
    assert_eq!(ib.reply_guarded("m1/a3", "rebase".into(), &unnamed), Err(PickError::Hidden));
    let guard = Guard { named_send_only: true, ..unnamed };
    ib.reply_guarded("m1/a3", "rebase".into(), &guard).unwrap();
    let sent = frame_of_type(&f, "agent.send").await;
    assert_eq!((sent["agent_id"].as_str(), sent["text"].as_str()), (Some("a3"), Some("rebase")));
}

/// The frozen plan passed its identity guard, but the user hid the agent
/// while the step waited for Confirm. Every guarded operation checks the
/// live settings under the action lock and sends nothing (final review B1).
#[tokio::test]
async fn a_guarded_step_on_a_subagent_hidden_since_the_freeze_is_refused() {
    let (f, c, ib) = setup_with(held()).await;
    ib.set_show_subagents(true).unwrap();
    let mut sub = subagent_agent();
    sub["can_send"] = true.into();
    sub["pending"]["free_text"] = true.into();
    c.send(serde_json::json!({"type":"snapshot","epoch":"e1","seq":2,"agents":[pending_agent(), sub]})).await;
    view_until(&ib, "subagent shown", |v| v.cards.len() == 2).await;
    let frozen = guard_for(&ib, "m1/a2");
    // Keep the published card, so only the live check can refuse.
    ib.hold_rebuild_for_test(true);
    ib.set_show_subagents(false).unwrap();
    assert_eq!(ib.pick_guarded("m1/a2", "allow", &frozen), Err(PickError::Hidden));
    assert_eq!(ib.answer_text_guarded("m1/a2", "SECRET".into(), &frozen), Err(PickError::Hidden));
    let named = Guard { named_send_only: true, ..frozen.clone() };
    assert_eq!(ib.reply_guarded("m1/a2", "go on".into(), &named), Err(PickError::Hidden), "D7 does not reach a hidden subagent");
    ib.hold_rebuild_for_test(false);
    ib.expire_countdowns_for_test();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let ops = f.received().iter().filter(|v| v["type"] == "request.answer" || v["type"] == "agent.send").count();
    assert_eq!(ops, 0, "no daemon operation");
}

#[tokio::test]
async fn a_guarded_send_to_a_hook_only_agent_hidden_since_the_freeze_is_refused() {
    let (f, c, ib) = setup_with(held()).await;
    ib.set_show_hook_only(true).unwrap();
    let mut quiet = quiet_hook_only_agent();
    quiet["can_send"] = true.into();
    c.send(serde_json::json!({"type":"snapshot","epoch":"e1","seq":2,"agents":[pending_agent(), quiet]})).await;
    view_until(&ib, "hook-only shown", |v| v.cards.iter().any(|c| c.key == "m1/a3")).await;
    let frozen = guard_for(&ib, "m1/a3");
    ib.hold_rebuild_for_test(true);
    ib.set_show_hook_only(false).unwrap();
    assert_eq!(ib.reply_guarded("m1/a3", "go on".into(), &frozen), Err(PickError::Hidden));
    let named = Guard { named_send_only: true, ..frozen };
    assert_eq!(ib.reply_guarded("m1/a3", "go on".into(), &named), Err(PickError::Hidden), "D7 covers idle agents only");
    ib.hold_rebuild_for_test(false);
    ib.expire_countdowns_for_test();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!f.received().iter().any(|v| v["type"] == "agent.send"), "no daemon operation");
}

#[test]
fn debug_output_shows_text_lengths_only() {
    use acd_client::actions::{ActionKind, TextSemantics};
    use acd_client::commands::{Command, Outcome};
    let shown = [
        format!("{:?}", Command::Send { agent_id: "a".into(), text: "SECRET-1".into() }),
        format!("{:?}", Command::AnswerText { request_id: "r".into(), revision: 1, text: "SECRET-2".into() }),
        format!("{:?}", Outcome::Failed { code: acd_protocol::ErrorCode::BadRequest, message: "SECRET-3".into() }),
        format!("{:?}", ActionKind::Reply { text: "SECRET-4".into() }),
    ];
    for s in shown {
        assert!(!s.contains("SECRET"), "{s}");
    }
    let req: acd_protocol::Request = serde_json::from_value(pending_agent()["pending"].clone()).unwrap();
    assert!(!format!("{:?}", TextSemantics::of(&req, "SECRET-5")).contains("SECRET"));
}

/// B5 through the real transport: a daemon error frame's message never
/// reaches the logs. Current-thread runtime: `set_default` is per thread.
#[tokio::test(flavor = "current_thread")]
async fn daemon_error_messages_never_reach_the_logs() {
    use std::sync::Mutex as StdMutex;
    #[derive(Clone, Default)]
    struct Buffer(Arc<StdMutex<Vec<u8>>>);
    impl std::io::Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let buffer = Buffer::default();
    let writer = buffer.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let (f, c, ib) = setup().await;
    c.send(serde_json::json!({"type":"error","code":"bad_request","message":"SECRET-DAEMON echo of run the tests"})).await;
    let mut rx = ib.action_outcomes();
    let id = ib.reply("m1/a1", "SECRET-REPLY".into()).unwrap();
    assert_eq!(next_outcome(&mut rx, &id).await, ActionOutcome::Fired);
    let sent = frame_of_type(&f, "agent.send").await;
    c.send(serde_json::json!({"type":"result","id":sent["id"],"ok":false,"error":{"code":"agent_busy","message":"SECRET-RESULT"}})).await;
    next_outcome(&mut rx, &id).await;
    let logs = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("daemon error"), "the capture works: {logs}");
    assert!(!logs.contains("SECRET"), "{logs}");
}

/// F3: a countdown's reply text must not leak through the derived `Debug`
/// of the views and actions that carry it.
#[tokio::test]
async fn debug_of_views_with_a_running_countdown_hides_the_text() {
    let (_f, _c, ib) = setup_with(held()).await;
    ib.reply("m1/a1", "SECRET-REPLY typed by the user".into()).unwrap();
    view_until(&ib, "countdown shown", |v| v.actions.len() == 1).await;
    let stamped = ib.stamped_view();
    assert_eq!(stamped.view.actions.len(), 1);
    let shown = [
        format!("{:?}", stamped.view.actions[0]),
        format!("{:?}", stamped.view),
        format!("{:?}", stamped),
        format!("{:?}", ib.view().borrow().cards),
    ];
    for s in &shown {
        assert!(!s.contains("SECRET"), "{s}");
    }
    assert!(shown[0].contains("detail_bytes"), "{}", shown[0]);
}


fn hook_only_agent() -> Value {
    let mut a = pending_agent();
    a["id"] = "a3".into();
    a["title"] = "hooked".into();
    a["source"] = "hooks".into();
    a["can_send"] = false.into();
    a["pending"]["id"] = "r3".into();
    a["pending"]["agent_id"] = "a3".into();
    a
}

fn quiet_hook_only_agent() -> Value {
    let mut a = hook_only_agent();
    a["state"] = "working".into();
    a["pending"] = Value::Null;
    a
}

#[tokio::test]
async fn hook_only_agents_show_with_a_request_and_notify_as_before() {
    let (_f, c, ib) = setup().await;
    let mut events = ib.events();
    let mut quiet = quiet_hook_only_agent();
    quiet["id"] = "a4".into();
    c.send(serde_json::json!({"type":"snapshot","epoch":"e1","seq":2,"agents":[pending_agent(), quiet, quiet_hook_only_agent()]}))
        .await;
    view_until(&ib, "quiet hook-only hidden", |v| {
        v.cards.len() == 1 && v.cards[0].agent.id == "a1" && v.tray.needs_you == 1
    })
    .await;
    assert!(!ib.view().borrow().show_hook_only);
    // A request on a hook-only agent shows and notifies.
    // The daemon sends request.opened before the agent update.
    c.send(serde_json::json!({"type":"request.opened","epoch":"e1","seq":3,"request":hook_only_agent()["pending"]}))
        .await;
    c.send(serde_json::json!({"type":"agent.updated","epoch":"e1","seq":4,"agent":hook_only_agent()}))
        .await;
    view_until(&ib, "hook-only request shown", |v| {
        v.cards.iter().any(|c| c.agent.id == "a3" && c.answerable) && v.tray.needs_you == 2
    })
    .await;
    let mut notified = false;
    while let Ok(e) = events.try_recv() {
        notified |= matches!(e, InboxEvent::RequestOpened { ref request_id, .. } if request_id == "r3");
    }
    assert!(notified, "the hook-only request notified");
    // Once it resolves the card goes away again.
    let mut quiet_again = quiet_hook_only_agent();
    quiet_again["id"] = "a3".into();
    c.send(serde_json::json!({"type":"agent.updated","epoch":"e1","seq":5,"agent":quiet_again}))
        .await;
    view_until(&ib, "hidden again", |v| v.cards.len() == 1 && v.tray.needs_you == 1).await;
    ib.set_show_hook_only(true).unwrap();
    view_until(&ib, "all shown", |v| v.show_hook_only && v.cards.len() == 3).await;
}

#[tokio::test]
async fn show_hook_only_is_remembered() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("settings.json");
    let ib = empty_inbox(Arc::new(MemorySecrets::default()), Some(path.clone()));
    assert!(!Settings::load_or_init(&path).unwrap().show_hook_only);
    ib.set_show_hook_only(true).unwrap();
    assert!(Settings::load_or_init(&path).unwrap().show_hook_only);
    assert!(ib.settings().show_hook_only);
}

#[tokio::test]
async fn a_countdown_keeps_a_hook_only_card_visible_after_its_request_resolves() {
    let (_f, c, ib) = setup().await;
    let mut hooked = hook_only_agent();
    hooked["can_send"] = true.into(); // only so a reply countdown can run
    c.send(serde_json::json!({"type":"snapshot","epoch":"e1","seq":2,"agents":[hooked]}))
        .await;
    view_until(&ib, "hook-only card", |v| v.cards.iter().any(|c| c.key == "m1/a3")).await;
    ib.reply("m1/a3", "next".into()).unwrap();
    view_until(&ib, "counting", |v| v.actions.len() == 1).await;
    let mut quiet = quiet_hook_only_agent();
    quiet["id"] = "a3".into();
    quiet["can_send"] = true.into();
    c.send(serde_json::json!({"type":"agent.updated","epoch":"e1","seq":3,"agent":quiet}))
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let v = ib.view().borrow().clone();
    assert_eq!(v.actions.len(), 1, "the countdown is still running");
    assert!(
        v.cards.iter().any(|c| c.key == "m1/a3" && c.action.is_some()),
        "the card stays while the countdown runs"
    );
}

#[tokio::test]
async fn turning_hook_only_off_cancels_countdowns_on_quiet_hook_only_cards() {
    let (f, c, ib) = setup().await;
    ib.set_show_hook_only(true).unwrap();
    let mut quiet = quiet_hook_only_agent();
    quiet["can_send"] = true.into(); // only so a reply countdown can run
    c.send(serde_json::json!({"type":"snapshot","epoch":"e1","seq":2,"agents":[pending_agent(), quiet]}))
        .await;
    view_until(&ib, "both shown", |v| v.cards.len() == 2).await;
    ib.reply("m1/a3", "next".into()).unwrap();
    view_until(&ib, "counting", |v| v.actions.len() == 1).await;
    ib.set_show_hook_only(false).unwrap();
    assert!(!ib.escape(), "no hidden action is left for Esc");
    view_until(&ib, "no actions", |v| v.actions.is_empty() && v.cards.len() == 1).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        f.received().iter().filter(|v| v["type"] == "agent.send").count(),
        0,
        "the hidden countdown never fired"
    );
}

#[tokio::test]
async fn a_pending_hook_only_countdown_survives_turning_the_setting_off() {
    let (f, c, ib) = setup().await;
    ib.set_show_hook_only(true).unwrap();
    c.send(serde_json::json!({"type":"snapshot","epoch":"e1","seq":2,"agents":[hook_only_agent()]}))
        .await;
    view_until(&ib, "hook-only card", |v| v.cards.iter().any(|c| c.key == "m1/a3")).await;
    ib.pick("m1/a3", "r3", "allow", 1).unwrap();
    view_until(&ib, "counting", |v| v.actions.len() == 1).await;
    ib.set_show_hook_only(false).unwrap();
    view_until(&ib, "card kept", |v| {
        v.actions.len() == 1 && v.cards.iter().any(|c| c.key == "m1/a3")
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(answers(&f), 1, "the countdown still fired");
}

#[tokio::test]
async fn a_hook_only_subagent_request_stays_hidden_and_silent() {
    let (_f, c, ib) = setup().await;
    let mut events = ib.events();
    let mut sub = hook_only_agent();
    sub["role"] = "subagent".into();
    c.send(serde_json::json!({"type":"snapshot","epoch":"e1","seq":2,"agents":[pending_agent(), sub.clone()]}))
        .await;
    c.send(serde_json::json!({"type":"request.opened","epoch":"e1","seq":3,"request":sub["pending"]}))
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let v = ib.view().borrow().clone();
    assert_eq!(v.cards.len(), 1);
    assert_eq!(v.tray.needs_you, 1);
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
}

/// Merge check: the hook-only drop path ends a countdown with exactly one
/// terminal outcome, `NotSent { Hidden }`. Rebuild is held so the request
/// going away is not turned into `AnsweredElsewhere` first.
#[tokio::test]
async fn hiding_hook_only_agents_reports_hidden_for_their_countdowns() {
    let (_f, c, ib) = setup_with(held()).await;
    c.send(serde_json::json!({"type":"agent.updated","epoch":"e1","seq":2,"agent":hook_only_agent()})).await;
    view_until(&ib, "hook-only card with a request", |v| v.cards.iter().any(|c| c.key == "m1/a3")).await;
    let mut rx = ib.action_outcomes();
    let id = ib.pick("m1/a3", "r3", "allow", 1).unwrap();
    ib.hold_rebuild_for_test(true);
    c.send(serde_json::json!({"type":"agent.updated","epoch":"e1","seq":3,"agent":quiet_hook_only_agent()})).await;
    // Retry until the machine state has the quiet agent; each early try drops nothing.
    let outcome = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            ib.set_show_hook_only(false).unwrap();
            if let Ok(e) = rx.try_recv() {
                assert_eq!(e.action_id, id);
                return e.outcome;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the countdown was dropped");
    assert_eq!(outcome, ActionOutcome::NotSent { reason: NotSentReason::Hidden });
    assert!(!ib.cancel(&id), "one terminal outcome only");
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn the_stamped_view_applies_the_hook_only_filter_like_the_published_view() {
    let (_f, c, ib) = setup().await;
    c.send(serde_json::json!({"type":"agent.updated","epoch":"e1","seq":2,"agent":quiet_hook_only_agent()})).await;
    ib.set_show_hook_only(false).unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let stamped = ib.stamped_view();
    assert!(!stamped.view.cards.iter().any(|c| c.key == "m1/a3"));
    assert!(!stamped.hidden_idle_cards.iter().any(|c| c.key == "m1/a3"));
    assert_eq!(stamped.view, *ib.view().borrow());
    ib.set_show_hook_only(true).unwrap();
    view_until(&ib, "shown", |v| v.cards.iter().any(|c| c.key == "m1/a3")).await;
    assert_eq!(ib.stamped_view().view, *ib.view().borrow());
}

fn second_pending_agent() -> Value {
    let mut a = pending_agent();
    a["id"] = "a2".into();
    a["pending"]["id"] = "r2".into();
    a["pending"]["agent_id"] = "a2".into();
    a
}

/// Two answerable cards, `m1/a1` (request `r1`) and `m1/a2` (request `r2`).
async fn setup_two(timing: acd_client::Timing) -> (FakeServer, FakeConn, Arc<Inbox>) {
    let (f, c, ib) = setup_with(timing).await;
    c.send(serde_json::json!({"type":"agent.updated","epoch":"e1","seq":2,"agent":second_pending_agent()}))
        .await;
    view_until(&ib, "two answerable cards", |v| v.cards.iter().filter(|c| c.answerable).count() == 2).await;
    (f, c, ib)
}

/// Starts and cancels a sentinel action on `m1/a2`, then reads outcomes up
/// to the sentinel's: none of them may belong to `id`.
async fn no_more_outcomes_for(ib: &Arc<Inbox>, rx: &mut tokio::sync::broadcast::Receiver<ActionEvent>, id: &str) {
    let sentinel = ib.pick("m1/a2", "r2", "deny", 1).unwrap();
    assert!(ib.cancel(&sentinel));
    loop {
        let e = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.unwrap().unwrap();
        assert_ne!(e.action_id, id, "a second outcome for one action: {:?}", e.outcome);
        if e.action_id == sentinel {
            return;
        }
    }
}

async fn deliver(f: &FakeServer, c: &FakeConn) {
    let frame = answer_frame(f).await;
    c.send(serde_json::json!({"type":"result","id":frame["id"],"ok":true,"outcome":"delivered"})).await;
}

#[tokio::test]
async fn escape_owned_cancels_only_that_owners_newest() {
    let (_f, _c, ib) = setup_two(held()).await;
    let mut rx = ib.action_outcomes();
    let (one, two) = (ActionOwner(1), ActionOwner(2));
    let a = ib.pick_owned(one, "m1/a1", ANSWER_REQ, "allow", 1).unwrap();
    let b = ib.pick_owned(two, "m1/a2", "r2", "allow", 1).unwrap();
    assert!(ib.escape_owned(one));
    assert_eq!(next_outcome(&mut rx, &a).await, ActionOutcome::Cancelled);
    assert!(!ib.escape_owned(one), "owner one has nothing left; two's countdown is not one's");
    view_until(&ib, "one countdown left", |v| v.actions.len() == 1 && v.actions[0].id == b).await;
    assert!(ib.cancel(&b), "a card's cancel control cancels any owner's action");
    assert_eq!(next_outcome(&mut rx, &b).await, ActionOutcome::Cancelled);
}

#[tokio::test]
async fn cancel_owned_ends_every_countdown_of_that_owner_and_nothing_fires() {
    let (f, _c, ib) = setup_two(held()).await;
    let mut rx = ib.action_outcomes();
    let a = ib.pick_owned(ActionOwner(7), "m1/a1", ANSWER_REQ, "allow", 1).unwrap();
    let b = ib.pick_owned(ActionOwner(7), "m1/a2", "r2", "deny", 1).unwrap();
    assert_eq!(ib.cancel_owned(ActionOwner(7)), 2);
    assert_eq!(ib.cancel_owned(ActionOwner(7)), 0);
    assert_eq!(next_outcome(&mut rx, &a).await, ActionOutcome::Cancelled);
    assert_eq!(next_outcome(&mut rx, &b).await, ActionOutcome::Cancelled);
    ib.expire_countdowns_for_test();
    view_until(&ib, "no countdowns", |v| v.actions.is_empty()).await;
    assert_eq!(answers(&f), 0);
}

#[tokio::test]
async fn escape_without_an_owner_still_takes_the_newest_of_any_owner() {
    let (_f, _c, ib) = setup_two(held()).await;
    ib.pick_owned(ActionOwner(1), "m1/a1", ANSWER_REQ, "allow", 1).unwrap();
    let newest = ib.pick_owned(ActionOwner(2), "m1/a2", "r2", "allow", 1).unwrap();
    let mut rx = ib.action_outcomes();
    assert!(ib.escape());
    assert_eq!(next_outcome(&mut rx, &newest).await, ActionOutcome::Cancelled);
}

#[tokio::test]
async fn outcomes_name_the_card() {
    let (_f, _c, ib) = setup_with(held()).await;
    let mut rx = ib.action_outcomes();
    let id = ib.pick("m1/a1", ANSWER_REQ, "allow", 1).unwrap();
    assert!(ib.cancel(&id));
    let event = rx.recv().await.unwrap();
    assert_eq!((event.action_id.as_str(), event.card_key.as_str()), (id.as_str(), "m1/a1"));
}

#[tokio::test]
async fn escape_owned_before_expiry_cancels_once_and_sends_nothing() {
    let (f, _c, ib) = setup_two(held()).await;
    let mut rx = ib.action_outcomes();
    let owner = ActionOwner(3);
    let id = ib.pick_owned(owner, "m1/a1", ANSWER_REQ, "allow", 1).unwrap();
    assert!(ib.escape_owned(owner));
    ib.expire_countdowns_for_test();
    assert_eq!(next_outcome(&mut rx, &id).await, ActionOutcome::Cancelled);
    no_more_outcomes_for(&ib, &mut rx, &id).await;
    assert_eq!(answers(&f), 0);
}

#[tokio::test]
async fn escape_owned_after_expiry_is_refused_and_the_send_ends_once() {
    let (f, c, ib) = setup_two(held()).await;
    let mut rx = ib.action_outcomes();
    let owner = ActionOwner(3);
    let id = ib.pick_owned(owner, "m1/a1", ANSWER_REQ, "allow", 1).unwrap();
    ib.expire_countdowns_for_test();
    assert_eq!(next_outcome(&mut rx, &id).await, ActionOutcome::Fired);
    assert!(!ib.escape_owned(owner), "too late once fired");
    deliver(&f, &c).await;
    assert_eq!(next_outcome(&mut rx, &id).await, ActionOutcome::Delivered);
    no_more_outcomes_for(&ib, &mut rx, &id).await;
}

#[tokio::test]
async fn cancel_owned_before_expiry_cancels_once_and_sends_nothing() {
    let (f, _c, ib) = setup_two(held()).await;
    let mut rx = ib.action_outcomes();
    let owner = ActionOwner(4);
    let id = ib.pick_owned(owner, "m1/a1", ANSWER_REQ, "allow", 1).unwrap();
    assert_eq!(ib.cancel_owned(owner), 1);
    ib.expire_countdowns_for_test();
    assert_eq!(next_outcome(&mut rx, &id).await, ActionOutcome::Cancelled);
    no_more_outcomes_for(&ib, &mut rx, &id).await;
    assert_eq!(answers(&f), 0);
}

#[tokio::test]
async fn cancel_owned_after_expiry_cancels_nothing_and_the_send_ends_once() {
    let (f, c, ib) = setup_two(held()).await;
    let mut rx = ib.action_outcomes();
    let owner = ActionOwner(4);
    let id = ib.pick_owned(owner, "m1/a1", ANSWER_REQ, "allow", 1).unwrap();
    ib.expire_countdowns_for_test();
    assert_eq!(next_outcome(&mut rx, &id).await, ActionOutcome::Fired);
    assert_eq!(ib.cancel_owned(owner), 0, "a disconnect after the countdown cannot stop it");
    deliver(&f, &c).await;
    assert_eq!(next_outcome(&mut rx, &id).await, ActionOutcome::Delivered);
    no_more_outcomes_for(&ib, &mut rx, &id).await;
}
