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
        client_id: "00000000-0000-4000-8000-000000000001".into(),
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
    ib.pick("m1/a1", ANSWER_REQ, "allow").unwrap();
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
    ib.pick("m1/a1", ANSWER_REQ, "allow").unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(answers(&f), 1);
}

#[tokio::test]
async fn removing_a_machine_cancels_its_countdowns() {
    let (f, _, ib) = setup().await;
    ib.pick("m1/a1", ANSWER_REQ, "allow").unwrap();
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
        f.pin(),
        common::TOKEN
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
    ib.pick("m1/a1", ANSWER_REQ, "allow").unwrap();
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
    ib.pick("m1/a1", ANSWER_REQ, "allow").unwrap();
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
