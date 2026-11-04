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
        // A long undo period, so a countdown never fires while a test looks at it.
        timing: acd_client::Timing { undo: Duration::from_secs(60), ..common::fast() },
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

// ---- typed answers and replies ----
use acd_client::actions::ActionOwner;
use acd_client::voice::Guard;

const ALL: [&str; 4] = ["answer", "send", "mark_seen", "answer.revision"];
const UNDO_MS: f64 = 60_000.; // `options()`: undo is 60 s

fn free_text_agent() -> Value {
    agent(|p| p["free_text"] = json!(true))
}

/// A working agent that takes a reply and has no request.
fn quiet_agent() -> Value {
    let mut a = agent(|_| {});
    a["pending"] = Value::Null;
    a["state"] = json!("working");
    a
}

async fn only_action(ib: &Inbox) -> ActionView {
    view_until(ib, "a countdown", |v| !v.actions.is_empty()).await;
    let v = ib.view().borrow().clone();
    assert_eq!(v.actions.len(), 1, "{:?}", v.actions);
    v.actions[0].clone()
}

/// inbox.rs:1184 / 1250 / 1308 (`fires_at_ms = now_ms() + undo * 1000`): the
/// countdown the user sees ends one undo period from now, for a pick, a typed
/// answer and a reply.
#[tokio::test]
async fn the_countdown_shown_ends_one_undo_period_from_now() {
    use acd_client::conn::now_ms;
    let (_f, ib) = inbox_with(&ALL, agent(|_| {})).await;
    let before = now_ms();
    ib.pick("m1/a1", REQ, "allow", 1).unwrap();
    let after = now_ms();
    let shown = only_action(&ib).await.fires_at_ms;
    assert!(before + UNDO_MS <= shown && shown <= after + UNDO_MS, "pick: {before} {shown} {after}");

    let (_f, ib) = inbox_with(&ALL, free_text_agent()).await;
    let before = now_ms();
    ib.answer_text("m1/a1", REQ, 1, "go on".into()).unwrap();
    let after = now_ms();
    let shown = only_action(&ib).await.fires_at_ms;
    assert!(before + UNDO_MS <= shown && shown <= after + UNDO_MS, "answer_text: {before} {shown} {after}");

    let (_f, ib) = inbox_with(&ALL, quiet_agent()).await;
    let before = now_ms();
    ib.reply("m1/a1", "hello".into()).unwrap();
    let after = now_ms();
    let shown = only_action(&ib).await.fires_at_ms;
    assert!(before + UNDO_MS <= shown && shown <= after + UNDO_MS, "reply: {before} {shown} {after}");
}

/// inbox.rs:1203 `>` -> `>=` (`answer_text_with`): a typed answer of exactly 16 KiB is
/// accepted, one byte more is not.
#[tokio::test]
async fn a_typed_answer_may_be_exactly_16_kib() {
    assert_eq!(ANSWER_TEXT_MAX_BYTES, 16 * 1024);
    let (_f, ib) = inbox_with(&ALL, free_text_agent()).await;
    assert_eq!(
        ib.answer_text("m1/a1", REQ, 1, "x".repeat(ANSWER_TEXT_MAX_BYTES + 1)),
        Err(PickError::BadAnswerText)
    );
    assert!(ib.answer_text("m1/a1", REQ, 1, "x".repeat(ANSWER_TEXT_MAX_BYTES)).is_ok());
}

/// inbox.rs:1212 `||` -> `&&` (`answer_text_with`): no answering without the capability.
#[tokio::test]
async fn a_typed_answer_needs_the_answer_capability() {
    let (_f, ib) = inbox_with(&["send", "mark_seen"], free_text_agent()).await;
    assert_eq!(ib.answer_text("m1/a1", REQ, 1, "go".into()), Err(PickError::NotAnswerable));
}

/// inbox.rs:1339 `answer_text_owned -> Ok(String::new() / "xyzzy")`: it returns
/// the id of the action it started, owned by the caller.
#[tokio::test]
async fn an_owned_typed_answer_returns_its_action_id() {
    let (_f, ib) = inbox_with(&ALL, free_text_agent()).await;
    let id = ib.answer_text_owned(ActionOwner(7), "m1/a1", REQ, 1, "go".into()).unwrap();
    assert_eq!(only_action(&ib).await.id, id);
    assert_eq!(ib.answer_text_owned(ActionOwner(8), "m1/a1", REQ, 1, "again".into()), Err(PickError::AlreadyCounting));
}

/// inbox.rs:1258 `>` -> `==` / `>=`, `*` -> `+` (`reply_with`): a reply of exactly
/// 64 KiB is accepted, one byte more or a blank one is not.
#[tokio::test]
async fn a_reply_may_be_exactly_64_kib() {
    let (_f, ib) = inbox_with(&ALL, quiet_agent()).await;
    assert_eq!(ib.reply("m1/a1", "x".repeat(64 * 1024 + 1)), Err(PickError::BadText));
    assert_eq!(ib.reply("m1/a1", "x".repeat(64 * 1024 + 2000)), Err(PickError::BadText));
    assert_eq!(ib.reply("m1/a1", "  \n ".into()), Err(PickError::BadText));
    assert!(ib.reply("m1/a1", "x".repeat(64 * 1024)).is_ok());
}

fn voice_guard(ib: &Inbox) -> Guard {
    Guard { conn: ib.conn_stamp("m1").unwrap(), agent_id: "a1".into(), request: None, named_send_only: false }
}

/// inbox.rs:1269 / 1270 `&&` -> `||` (`reply_with`, voice path): the live state
/// must be online, with the send capability, and an agent that can take a reply.
#[tokio::test]
async fn a_voice_reply_needs_the_send_capability_and_an_agent_that_takes_one() {
    let (_f, ib) = inbox_with(&ALL, quiet_agent()).await;
    assert!(ib.reply_guarded("m1/a1", "hi".into(), &voice_guard(&ib)).is_ok());

    let (_f, ib) = inbox_with(&["answer", "mark_seen", "answer.revision"], quiet_agent()).await;
    assert_eq!(ib.reply_guarded("m1/a1", "hi".into(), &voice_guard(&ib)), Err(PickError::CannotReply));

    let mut mute = quiet_agent();
    mute["can_send"] = json!(false);
    let (_f, ib) = inbox_with(&ALL, mute).await;
    assert_eq!(ib.reply_guarded("m1/a1", "hi".into(), &voice_guard(&ib)), Err(PickError::CannotReply));
}

/// inbox.rs:577 / 631 (`rename_machine`, `remove_machine_checked`): the running
/// machine list follows a rename and a removal, not only the saved settings.
#[tokio::test]
async fn the_live_machine_list_follows_rename_and_remove() {
    let f = FakeServer::start(common::TOKEN, Identity::Paired).await;
    let g = FakeServer::start(common::TOKEN, Identity::Paired).await;
    let ib = Inbox::new(settings_for(&f), secrets_for(&f), options());
    ib.start();
    let second = ib.pair_machine(&line(&g)).unwrap();
    view_until(&ib, "both machines", |v| v.machines.len() == 2).await;
    ib.rename_machine(&second.key, "Other").unwrap();
    view_until(&ib, "the new name", |v| {
        v.machines.iter().any(|m| m.key == second.key && m.name == "Other")
            && v.machines.iter().any(|m| m.key == "m1" && m.name == "Box")
    })
    .await;
    ib.remove_machine_checked(&second.key).unwrap();
    view_until(&ib, "one machine", |v| v.machines.len() == 1 && v.machines[0].key == "m1").await;
}

// ---- session limits ----
use acd_client::session::{Applied, Session};
use acd_protocol::{MAX_AGENT_BYTES, MAX_AGENTS_PER_MACHINE, ServerMessage};

fn proto_agent(id: &str, title: &str) -> acd_protocol::Agent {
    serde_json::from_value(json!({"id":id,"machine_id":"m","source":"herdr","kind":"claude","title":title,"cwd":null,"project":null,"state":"working","state_since":"2026-10-08T00:00:00Z","failed":false,"last_message":null,"can_send":true,"pending":null})).unwrap()
}

/// An agent whose JSON encoding is exactly `bytes` long.
fn agent_of_size(id: &str, bytes: usize) -> acd_protocol::Agent {
    let base = serde_json::to_vec(&proto_agent(id, "")).unwrap().len();
    let a = proto_agent(id, &"x".repeat(bytes - base));
    assert_eq!(serde_json::to_vec(&a).unwrap().len(), bytes);
    a
}

fn welcomed() -> Session {
    let mut s = Session::default();
    let machine: acd_protocol::Machine = serde_json::from_value(json!({"id":"m","name":"M","acd_version":"0","sources":{"hooks":{"state":"ok"},"herdr":{"state":"ok"},"t3":{"state":"ok"}}})).unwrap();
    s.welcome(machine, vec!["answer".into()], "e", 0);
    s
}

fn snapshot_msg(seq: u64, agents: Vec<acd_protocol::Agent>) -> ServerMessage {
    ServerMessage::Snapshot { epoch: "e".into(), seq, agents }
}

fn updated(seq: u64, agent: acd_protocol::Agent) -> ServerMessage {
    ServerMessage::AgentUpdated { epoch: "e".into(), seq, agent }
}

/// session.rs:42 `>` -> `>=` (`agent_too_big`): an agent of exactly 64 KiB is kept, one byte more ends the sync.
#[test]
fn an_agent_may_be_exactly_64_kib() {
    let mut s = welcomed();
    assert_eq!(s.apply(&snapshot_msg(1, vec![agent_of_size("a", MAX_AGENT_BYTES)])), Applied::Changed);
    assert!(matches!(s.apply(&updated(2, agent_of_size("a", MAX_AGENT_BYTES))), Applied::Changed));
    let mut s = welcomed();
    assert!(matches!(s.apply(&snapshot_msg(1, vec![agent_of_size("a", MAX_AGENT_BYTES + 1)])), Applied::Resync(_)));
    let mut s = welcomed();
    s.apply(&snapshot_msg(1, vec![]));
    assert!(matches!(s.apply(&updated(2, agent_of_size("a", MAX_AGENT_BYTES + 1))), Applied::Resync(_)));
}

/// session.rs:119 `>` -> `>=` and :157 `delete !`: 1000 agents are kept, the 1001st ends
/// the sync, and a full machine still takes updates to agents it already has.
#[test]
fn a_machine_may_hold_exactly_1000_agents() {
    let agents = |n: usize| (0..n).map(|i| proto_agent(&format!("a{i}"), "t")).collect::<Vec<_>>();
    let mut s = welcomed();
    assert_eq!(s.apply(&snapshot_msg(1, agents(MAX_AGENTS_PER_MACHINE))), Applied::Changed);
    assert_eq!(s.agents.len(), MAX_AGENTS_PER_MACHINE);
    assert!(matches!(s.apply(&updated(2, proto_agent("a0", "renamed"))), Applied::Changed), "an existing agent");
    assert!(matches!(s.apply(&updated(3, proto_agent("brand-new", "t"))), Applied::Resync(_)), "a 1001st agent");
    let mut s = welcomed();
    assert!(matches!(s.apply(&snapshot_msg(1, agents(MAX_AGENTS_PER_MACHINE + 1))), Applied::Resync(_)));
}

/// session.rs:70 `&&` -> `||` (`synced`): a session with no cursor is not synced.
#[test]
fn a_session_is_synced_only_with_a_cursor_and_a_snapshot() {
    assert!(!Session::default().synced());
    let mut s = welcomed();
    assert!(!s.synced());
    s.apply(&snapshot_msg(0, vec![]));
    assert!(s.synced());
    s.force_resync();
    assert!(!s.synced());
}

// ---- settings ----

/// settings.rs:156 `Debug` for `MachineSecret`: the token never prints.
#[test]
fn a_machine_secret_prints_without_its_token() {
    let s = MachineSecret { token: common::TOKEN.into(), pin: "sha256:PIN".into() };
    let shown = format!("{s:?}");
    assert!(!shown.contains(common::TOKEN), "{shown}");
    assert!(shown.starts_with("MachineSecret") && shown.contains("sha256:PIN") && shown.contains("redacted"), "{shown}");
}

fn tcp_entry(name: &str, host: &str, port: u16) -> MachineEntry {
    MachineEntry {
        key: "11111111-1111-4111-8111-111111111111".into(),
        name: name.into(),
        endpoint: EndpointConfig::Tcp { host: host.into(), port },
    }
}

/// settings.rs:193 / 198 / 199 (`MachineEntry::validate`): the name, host and port limits.
#[test]
fn machine_entries_are_validated_at_their_limits() {
    assert!(tcp_entry(&"n".repeat(64), "box.example", 1).validate().is_ok());
    assert!(tcp_entry(&"n".repeat(65), "box.example", 1).validate().is_err());
    assert!(tcp_entry("  ", "box.example", 1).validate().is_err());
    assert!(tcp_entry("n", &"h".repeat(253), 1).validate().is_ok());
    assert!(tcp_entry("n", &"h".repeat(254), 1).validate().is_err());
    assert!(tcp_entry("n", "", 1).validate().is_err());
    for bad in ["ws://box", "box host", "box/path", "box\tx"] {
        assert!(tcp_entry("n", bad, 1).validate().is_err(), "{bad}");
    }
    assert!(tcp_entry("n", "box.example", 0).validate().is_err());
    assert!(tcp_entry("n", "100.64.0.1", 65535).validate().is_ok());
    let mut k = tcp_entry("n", "box.example", 1);
    k.key = "not-a-uuid".into();
    assert!(k.validate().is_err());
    k.key = "11111111-1111-4111-8111-11111111111".into();
    assert!(k.validate().is_err());
}

/// settings.rs:60 `default_hotkey`, :235 and :278 match guards (`load_or_init`).
#[test]
fn settings_load_with_defaults_and_refuse_what_they_cannot_read() {
    let dir = tempfile::tempdir().unwrap();
    // A file without a hotkey takes the default one.
    let ok = dir.path().join("ok.json");
    std::fs::write(&ok, json!({"client_id": CLIENT_ID, "machines": []}).to_string()).unwrap();
    let s = Settings::load_or_init(&ok).unwrap();
    assert_eq!(s.hotkey, DEFAULT_HOTKEY);
    assert_eq!(s.client_id, CLIENT_ID);
    // A client id that is not a UUID: the file is set aside and replaced.
    let bad = dir.path().join("bad.json");
    std::fs::write(&bad, json!({"client_id": "nope", "machines": []}).to_string()).unwrap();
    let s = Settings::load_or_init(&bad).unwrap();
    assert_ne!(s.client_id, "nope");
    assert!(dir.path().join("bad.json.bad").exists());
    // No file yet: created. A path that cannot be read as a file is an error, not a fresh start.
    let missing = dir.path().join("sub").join("new.json");
    assert!(Settings::load_or_init(&missing).is_ok() && missing.exists());
    assert!(Settings::load_or_init(dir.path()).is_err());
}

// ---- inbox: mark seen, owners, hiding ----

/// inbox.rs:1401 / 1407 / 1409 (`mark_seen`): a known card with the capability sends
/// `agent.mark_seen`; an unknown card or a missing capability is refused.
#[tokio::test]
async fn mark_seen_sends_for_a_known_card_only() {
    let mut done = agent(|_| {});
    done["pending"] = Value::Null;
    done["state"] = json!("done");
    let (f, ib) = inbox_with(&ALL, done.clone()).await;
    assert_eq!(ib.mark_seen("m1/nobody"), Err(PickError::UnknownCard));
    ib.mark_seen("m1/a1").unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !f.received().iter().any(|v| v["type"] == "agent.mark_seen" && v["agent_id"] == "a1") {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the mark_seen frame reached the daemon");

    let (_f, ib) = inbox_with(&["answer", "send"], done).await;
    assert_eq!(ib.mark_seen("m1/a1"), Err(PickError::CannotReply));
}

/// inbox.rs:1395 `delete !` (`cancel_owned`) and :1360: only the owner's countdowns end, and the count is right.
#[tokio::test]
async fn cancel_owned_ends_only_that_owners_countdowns() {
    let (_f, ib) = inbox_with(&ALL, agent(|_| {})).await;
    ib.pick_owned(ActionOwner(1), "m1/a1", REQ, "allow", 1).unwrap();
    view_until(&ib, "the countdown", |v| v.actions.len() == 1).await;
    assert_eq!(ib.cancel_owned(ActionOwner(2)), 0);
    assert_eq!(ib.view().borrow().actions.len(), 1);
    assert_eq!(ib.cancel_owned(ActionOwner(1)), 1);
    view_until(&ib, "no countdown", |v| v.actions.is_empty()).await;
}

/// inbox.rs:1431 `set_show_idle -> ()` and :1466 `delete !` (`drop_hidden_actions`):
/// turning idle agents off stops a countdown on an idle card, and the setting is stored.
#[tokio::test]
async fn hiding_idle_agents_cancels_a_countdown_on_one() {
    let f = FakeServer::start(common::TOKEN, Identity::Paired).await;
    let mut settings = settings_for(&f);
    settings.show_idle = true;
    let ib = Inbox::new(settings, secrets_for(&f), options());
    ib.start();
    let c = f.next_connection().await;
    let mut idle = quiet_agent();
    idle["state"] = json!("idle");
    c.send(json!({"type":"snapshot","epoch":"e1","seq":1,"agents":[idle]})).await;
    view_until(&ib, "the idle card", |v| v.cards.iter().any(|c| c.key == "m1/a1")).await;
    ib.reply("m1/a1", "hello".into()).unwrap();
    view_until(&ib, "the countdown", |v| v.actions.len() == 1).await;
    ib.set_show_idle(false);
    assert!(!ib.settings().show_idle);
    view_until(&ib, "the countdown to end", |v| v.actions.is_empty()).await;
    ib.set_show_idle(true);
    assert!(ib.settings().show_idle);
}
