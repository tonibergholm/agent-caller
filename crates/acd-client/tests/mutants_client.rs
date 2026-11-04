// Tests added from a cargo-mutants run (docs/research/2026-11-04-mutation-testing.md).
// Unix only: these drive the fake server.
#![cfg(owner_checks)]

mod common;
use acd_client::{conn::*, fake::*};
use std::time::Duration;

async fn wait_for_connections(f: &FakeServer, expected: usize) {
    // The timeout only matters on failure; success returns as soon as the connection arrives.
    tokio::time::timeout(Duration::from_secs(10), async {
        while f.connections() < expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {expected} fake connections"));
}

/// conn.rs:678 `delete !` and `wait_retry -> true`: a rejected pasted token is
/// not retried on its own; `retry_now` makes exactly one more attempt and the
/// machine stays alive for the next one.
#[tokio::test]
async fn a_rejected_token_waits_for_retry_now_and_then_tries_again() {
    let f = FakeServer::start(common::TOKEN, Identity::Paired).await;
    let h = spawn_machine(cfg_for(&f, "wrong-token", common::fast()));
    let mut rx = h.state();
    common::wait_for(&mut rx, "auth failed", |s| matches!(s.status, MachineStatus::AuthFailed { .. })).await;
    assert_eq!(f.connections(), 1, "no retry loop until retry_now");
    h.retry_now();
    wait_for_connections(&f, 2).await;
    common::wait_for(&mut rx, "auth failed again", |s| matches!(s.status, MachineStatus::AuthFailed { .. })).await;
    h.retry_now();
    wait_for_connections(&f, 3).await;
    assert!(f.hellos().iter().all(|hello| hello["token"] == "wrong-token"));
}

// ---- inbox ----
use acd_client::{inbox::*, settings::*, view::*};
use serde_json::{Value, json};
use std::sync::Arc;

const REQ: &str = "r1";
const CLIENT_ID: &str = "00000000-0000-4000-8000-000000000001";

fn agent(mutate: impl Fn(&mut Value)) -> Value {
    let mut a = json!({"id":"a1","machine_id":"m","source":"herdr","kind":"claude","title":"agent","cwd":null,"project":null,"state":"needs_you","state_since":"2026-10-08T00:00:00.000Z","failed":false,"last_message":null,"can_send":true,"pending":{"id":REQ,"revision":1,"agent_id":"a1","kind":"permission","status":"open","answer_path":"hook","title":"Bash","detail":"npm test","options":[{"id":"allow","label":"Yes","description":null,"effect":"allow"},{"id":"deny","label":"No","description":null,"effect":"deny"}],"questions":null,"free_text":false,"raw_screen":null,"opened_at":"2026-10-08T00:00:00.000Z"}});
    mutate(&mut a["pending"]);
    a
}

fn settings_for(f: &FakeServer) -> Settings {
    Settings {
        client_id: CLIENT_ID.into(),
        hotkey: DEFAULT_HOTKEY.into(),
        hotkey_migrated: true,
        show_idle: false,
        show_subagents: false,
        show_hook_only: false,
        machines: vec![MachineEntry {
            key: "m1".into(),
            name: "Box".into(),
            endpoint: EndpointConfig::Tcp { host: "127.0.0.1".into(), port: f.port() },
        }],
    }
}

fn secrets_for(f: &FakeServer) -> Arc<MemorySecrets> {
    let secrets = Arc::new(MemorySecrets::default());
    secrets
        .set("m1", &MachineSecret { token: common::TOKEN.into(), pin: f.pin() }.to_json())
        .unwrap();
    secrets
}

fn options() -> InboxOptions {
    InboxOptions {
        timing: common::fast(),
        settings_path: None,
        client_name: "t".into(),
        client_version: "0".into(),
    }
}

async fn view_until(ib: &Inbox, what: &str, pred: impl Fn(&InboxView) -> bool) {
    let mut rx = ib.view();
    tokio::time::timeout(Duration::from_secs(10), async {
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

/// An inbox on a fake daemon that advertises `caps`, showing `agent`.
async fn inbox_with(caps: &[&str], agent: Value) -> (FakeServer, Arc<Inbox>) {
    let f = FakeServer::start(common::TOKEN, Identity::Paired).await;
    f.set_capabilities(caps);
    let ib = Inbox::new(settings_for(&f), secrets_for(&f), options());
    ib.start();
    let c = f.next_connection().await;
    c.send(json!({"type":"snapshot","epoch":"e1","seq":1,"agents":[agent]})).await;
    view_until(&ib, "the card", |v| v.cards.iter().any(|c| c.key == "m1/a1")).await;
    (f, ib)
}

/// inbox.rs:1136 `||` -> `&&` and :1152 `||` -> `&&` (`pick_with`): a request
/// the daemon cannot take an answer for is refused before any countdown starts.
#[tokio::test]
async fn pick_refuses_what_cannot_be_answered() {
    let all = ["answer", "send", "mark_seen", "answer.revision"];
    // The daemon does not advertise answering.
    let (_f, ib) = inbox_with(&["send", "mark_seen"], agent(|_| {})).await;
    assert_eq!(ib.pick("m1/a1", REQ, "allow", 1), Err(PickError::NotAnswerable));
    // A request already claimed by another client.
    let (_f, ib) = inbox_with(&all, agent(|p| p["status"] = json!("answering"))).await;
    assert_eq!(ib.pick("m1/a1", REQ, "allow", 1), Err(PickError::NotAnswerable));
    // A request no path can answer.
    let (_f, ib) = inbox_with(&all, agent(|p| p["answer_path"] = json!("none"))).await;
    assert_eq!(ib.pick("m1/a1", REQ, "allow", 1), Err(PickError::NotAnswerable));
    // And the normal case still starts a countdown, with a stale revision refused.
    let (_f, ib) = inbox_with(&all, agent(|_| {})).await;
    assert_eq!(ib.pick("m1/a1", REQ, "allow", 2), Err(PickError::StaleRequest));
    assert_eq!(ib.pick("m1/a1", "other", "allow", 1), Err(PickError::StaleRequest));
    assert_eq!(ib.pick("m1/a1", REQ, "nope", 1), Err(PickError::UnknownOption));
    assert!(ib.pick("m1/a1", REQ, "allow", 1).is_ok());
}

// ---- machine management ----
use std::sync::atomic::{AtomicUsize, Ordering};

fn line(f: &FakeServer) -> String {
    format!("acd://127.0.0.1:{}?pin={}#{}", f.port(), f.pin(), common::TOKEN)
}

fn machine_keys(ib: &Inbox) -> Vec<String> {
    let mut keys: Vec<String> = ib.machines().into_iter().map(|m| m.key).collect();
    keys.sort();
    keys
}

/// A store whose reads fail with an error that is not "unknown", counting writes.
struct Unreadable {
    inner: MemorySecrets,
    sets: AtomicUsize,
}
impl SecretStore for Unreadable {
    fn get(&self, _: &str) -> Result<Option<String>, SecretError> {
        Err(SecretError("denied".into()))
    }
    fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
        self.sets.fetch_add(1, Ordering::SeqCst);
        self.inner.set(key, value)
    }
    fn delete(&self, key: &str) -> Result<(), SecretError> {
        self.inner.delete(key)
    }
}

/// inbox.rs:487 match guard `kind == Unknown` -> `true` (`pair_machine`): when the
/// old secret cannot be read, pairing stops (fail closed) instead of
/// overwriting what it could not look at.
#[tokio::test]
async fn pairing_fails_closed_when_the_old_secret_cannot_be_read() {
    let f = FakeServer::start(common::TOKEN, Identity::Paired).await;
    let store = Arc::new(Unreadable { inner: MemorySecrets::default(), sets: AtomicUsize::new(0) });
    let ib = Inbox::new(settings_for(&f), store.clone(), options());
    let before = machine_keys(&ib);
    let err = ib.pair_machine(&line(&f)).unwrap_err();
    assert!(matches!(err, MachineError::SecretRead(_)), "{err:?}");
    assert_eq!(store.sets.load(Ordering::SeqCst), 0, "nothing was written");
    assert_eq!(machine_keys(&ib), before);
}

/// inbox.rs:519 `==` -> `!=` (`pair_machine`): pairing the same host and port
/// again re-pairs that machine; another port or host is a new machine.
#[tokio::test]
async fn pairing_the_same_endpoint_again_keeps_the_machine() {
    let a = FakeServer::start(common::TOKEN, Identity::Paired).await;
    let b = FakeServer::start(common::TOKEN, Identity::Paired).await;
    let mut empty = settings_for(&a);
    empty.machines.clear();
    let ib = Inbox::new(empty, Arc::new(MemorySecrets::default()), options());
    let first = ib.pair_machine(&line(&a)).unwrap();
    assert_eq!(ib.pair_machine(&line(&a)).unwrap().key, first.key);
    assert_eq!(machine_keys(&ib).len(), 1);
    let other_port = ib.pair_machine(&line(&b)).unwrap();
    assert_ne!(other_port.key, first.key);
    assert_eq!(machine_keys(&ib).len(), 2);
    let other_host = line(&a).replace("127.0.0.1", "localhost");
    assert_ne!(ib.pair_machine(&other_host).unwrap().key, first.key);
}

/// inbox.rs:577 `==` -> `!=` (`rename_machine`): only the named machine is renamed.
#[tokio::test]
async fn rename_changes_only_the_named_machine() {
    let a = FakeServer::start(common::TOKEN, Identity::Paired).await;
    let ib = Inbox::new(settings_for(&a), Arc::new(MemorySecrets::default()), options());
    let second = ib.pair_machine(&line(&a).replace("127.0.0.1", "localhost")).unwrap();
    assert_eq!(ib.rename_machine("nope", "x"), Err("Unknown machine".to_string()));
    ib.rename_machine(&second.key, "Renamed").unwrap();
    let names: std::collections::BTreeMap<String, String> =
        ib.machines().into_iter().map(|m| (m.key, m.name)).collect();
    assert_eq!(names[&second.key], "Renamed");
    assert_eq!(names["m1"], "Box");
    assert_eq!(ib.settings().machines.iter().find(|m| m.key == "m1").unwrap().name, "Box");
}

/// inbox.rs:600/612/631 (`remove_machine_checked`): the named machine goes from
/// memory, the settings file and the secret store; the others stay.
#[tokio::test]
async fn removing_a_machine_removes_exactly_that_machine() {
    let a = FakeServer::start(common::TOKEN, Identity::Paired).await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let secrets = secrets_for(&a);
    let mut opts = options();
    opts.settings_path = Some(path.clone());
    let ib = Inbox::new(settings_for(&a), secrets.clone(), opts);
    let second = ib.pair_machine(&line(&a).replace("127.0.0.1", "localhost")).unwrap();
    assert!(matches!(ib.remove_machine_checked("nope"), Err(MachineError::UnknownMachine)));
    assert_eq!(machine_keys(&ib).len(), 2);
    ib.remove_machine_checked(&second.key).unwrap();
    assert_eq!(machine_keys(&ib), vec!["m1".to_string()]);
    assert_eq!(secrets.get(&second.key).unwrap(), None, "the secret is gone");
    assert!(secrets.get("m1").unwrap().is_some(), "the other secret stays");
    let saved: Settings = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(saved.machines.iter().map(|m| m.key.as_str()).collect::<Vec<_>>(), vec!["m1"]);
    assert_eq!(ib.settings().machines.len(), 1);
}

/// inbox.rs:648/654 `retry_machine -> ()` / `==` -> `!=`: it retries the named machine.
#[tokio::test]
async fn retry_machine_retries_the_named_machine() {
    let f = FakeServer::start(common::TOKEN, Identity::Paired).await;
    f.set_handshake_error(
        Some(json!({"type":"error","code":"unsupported_protocol","message":"m","supported":["acd/2"]})),
        Some(4400),
    );
    let ib = Inbox::new(settings_for(&f), secrets_for(&f), options());
    ib.start();
    wait_for_connections(&f, 1).await;
    ib.retry_machine("nobody");
    ib.retry_machine("m1");
    wait_for_connections(&f, 2).await;
}

/// tls.rs:74 `is_pin_mismatch -> false`: a pin mismatch is recognised by its message.
#[test]
fn a_pin_mismatch_is_recognised() {
    use acd_client::tls::{PIN_MISMATCH, is_pin_mismatch};
    assert!(is_pin_mismatch(&std::io::Error::other(PIN_MISMATCH)));
    assert!(is_pin_mismatch(&std::io::Error::other(format!("tls: {PIN_MISMATCH}"))));
    assert!(!is_pin_mismatch(&std::io::Error::other("connection refused")));
}

/// voice.rs:39 `is_terminal -> true / false`, `!=` -> `==`: only `Fired` is not final.
#[test]
fn only_fired_is_not_terminal() {
    use acd_client::voice::{ActionOutcome, NotSentReason};
    assert!(!ActionOutcome::Fired.is_terminal());
    for o in [
        ActionOutcome::Delivered,
        ActionOutcome::Rejected { code: acd_protocol::ErrorCode::BadRequest },
        ActionOutcome::Uncertain,
        ActionOutcome::Cancelled,
        ActionOutcome::NotSent { reason: NotSentReason::Offline },
    ] {
        assert!(o.is_terminal(), "{o:?}");
    }
}
