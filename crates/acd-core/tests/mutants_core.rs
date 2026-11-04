//! Tests added from a cargo-mutants run (docs/research/2026-11-04-mutation-testing.md).
//! Each test names the mutant it kills.
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use acd_core::testing::*;
use acd_core::tls::{KeyFiles, generate};
use acd_core::*;
use acd_protocol::*;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};

fn key(k: &str) -> CorrelationKey {
    CorrelationKey(k.into())
}

fn answer_text(id: &str, request_id: &str, text: &str, revision: Option<u64>) -> ClientMessage {
    ClientMessage::RequestAnswer {
        id: id.into(),
        request_id: request_id.into(),
        revision,
        option_id: None,
        text: Some(text.into()),
    }
}

fn answer_opt(id: &str, request_id: &str, revision: Option<u64>) -> ClientMessage {
    ClientMessage::RequestAnswer {
        id: id.into(),
        request_id: request_id.into(),
        revision,
        option_id: Some("allow".into()),
        text: None,
    }
}

fn send_msg(id: &str, agent_id: &str, text: &str) -> ClientMessage {
    ClientMessage::AgentSend { id: id.into(), agent_id: agent_id.into(), text: text.into() }
}

fn core_with_request(free_text: bool) -> (Arc<Core>, Arc<FakeAdapter>, String) {
    let fake = FakeAdapter::new();
    let core = Core::new(test_machine(), vec![fake.clone()]);
    core.observe(Observation::AgentUpsert(agent_obs("a1")));
    let mut p = permission_obs("a1", "k1");
    p.free_text = free_text;
    core.observe(Observation::RequestSeen(p));
    let rid = core.pending_key("a1").unwrap().0;
    (core, fake, rid)
}

// ---- engine.rs ----

/// engine.rs:18 `64 * 1024` -> `+`; engine.rs `check_limits` `>` -> `<`.
#[tokio::test]
async fn id_and_text_limits_are_inclusive() {
    assert_eq!(MAX_ID_BYTES, 128);
    assert_eq!(MAX_TEXT_BYTES, 65_536);
    let fake = FakeAdapter::new();
    let core = Core::new(test_machine(), vec![fake.clone()]);
    core.observe(Observation::AgentUpsert(agent_obs("a1")));
    let id128 = "i".repeat(128);
    let id129 = "i".repeat(129);
    // Exactly at the limit: accepted and delivered.
    assert_eq!(core.handle("cl", send_msg(&id128, "a1", "hi")).await, CommandResult::delivered(&id128));
    let big = "x".repeat(MAX_TEXT_BYTES);
    assert_eq!(core.handle("cl", send_msg("t1", "a1", &big)).await, CommandResult::delivered("t1"));
    assert_eq!(fake.sent().len(), 2);
    // One byte over: refused before anything is looked up or sent.
    let over = "x".repeat(MAX_TEXT_BYTES + 1);
    assert_eq!(core.handle("cl", send_msg("t2", "a1", &over)).await.error_code(), Some(ErrorCode::BadRequest));
    assert_eq!(core.handle("cl", send_msg(&id129, "a1", "hi")).await.error_code(), Some(ErrorCode::BadRequest));
    let long_agent = "a".repeat(129);
    assert_eq!(core.handle("cl", send_msg("t3", &long_agent, "hi")).await.error_code(), Some(ErrorCode::BadRequest));
    let long_req = answer_opt("t4", &"r".repeat(129), None);
    assert_eq!(core.handle("cl", long_req).await.error_code(), Some(ErrorCode::BadRequest));
    assert_eq!(fake.sent().len(), 2, "nothing over a limit reached the adapter");
}

/// engine.rs `Txn::request`, `Txn::request_id_for`, `Core::pending_key`.
#[test]
fn accessors_expose_the_pending_request() {
    let (core, _fake, rid) = core_with_request(false);
    let (prid, k, kind) = core.pending_key("a1").unwrap();
    assert_eq!((prid.as_str(), k.clone(), kind), (rid.as_str(), key("k1"), RequestKind::Permission));
    assert!(core.pending_key("nobody").is_none());
    core.transact(|t| {
        assert_eq!(t.request(&rid).unwrap().id, rid);
        assert!(t.request("nope").is_none());
        assert_eq!(t.request_id_for("a1", &key("k1")), Some(rid.clone()));
        assert_eq!(t.request_id_for("a1", &key("other")), None);
        assert_eq!(t.request_id_for("a2", &key("k1")), None);
    });
}

/// engine.rs `Core::epoch -> String::new() / "xyzzy"`.
#[test]
fn the_epoch_is_a_fresh_id_shared_with_the_welcome() {
    let a = Core::new(test_machine(), vec![]);
    let b = Core::new(test_machine(), vec![]);
    assert_eq!(a.epoch().len(), 32);
    assert_ne!(a.epoch(), b.epoch());
    let ServerMessage::Welcome { epoch, .. } = a.welcome() else { panic!("welcome") };
    assert_eq!(epoch, a.epoch());
}

/// engine.rs `set_source_status -> ()` and `==` -> `!=`.
#[test]
fn a_source_status_change_is_stored_and_published_once() {
    let core = Core::new(test_machine(), vec![]);
    let (_, mut rx) = core.attach(None);
    let ok = SourceStatus { state: SourceState::Ok, message: None };
    core.set_source_status(Source::Hooks, ok.clone());
    let ServerMessage::Welcome { machine, seq, .. } = core.welcome() else { panic!("welcome") };
    assert_eq!(machine.sources.hooks, ok);
    assert_eq!(machine.sources.t3, SourceStatus::unconfigured());
    assert_eq!(seq, 1);
    assert!(matches!(rx.try_recv(), Ok(ServerMessage::MachineUpdated { .. })));
    // Same value again: nothing changes, nothing is published.
    core.set_source_status(Source::Hooks, ok);
    let ServerMessage::Welcome { seq, .. } = core.welcome() else { panic!("welcome") };
    assert_eq!(seq, 1);
    assert!(rx.try_recv().is_err());
    // Sources without a slot change nothing.
    core.set_source_status(Source::External, SourceStatus { state: SourceState::Degraded, message: None });
    let ServerMessage::Welcome { seq, .. } = core.welcome() else { panic!("welcome") };
    assert_eq!(seq, 1);
}

/// engine.rs `delete match arm (None, Some(t))`: a text answer reaches the adapter as text.
#[tokio::test]
async fn a_text_answer_is_delivered_as_text() {
    let (core, fake, rid) = core_with_request(true);
    assert_eq!(core.handle("cl", answer_text("c1", &rid, "go ahead", None)).await, CommandResult::delivered("c1"));
    assert_eq!(fake.delivered(), vec![(rid, Answer::Text("go ahead".into()))]);
}

/// Exactly one of `option_id` / `text`; neither or both is a bad request.
#[tokio::test]
async fn an_answer_needs_exactly_one_of_option_and_text() {
    let (core, fake, rid) = core_with_request(true);
    let both = ClientMessage::RequestAnswer {
        id: "b".into(), request_id: rid.clone(), revision: None,
        option_id: Some("allow".into()), text: Some("x".into()),
    };
    let neither = ClientMessage::RequestAnswer {
        id: "n".into(), request_id: rid, revision: None, option_id: None, text: None,
    };
    assert_eq!(core.handle("cl", both).await.error_code(), Some(ErrorCode::BadRequest));
    assert_eq!(core.handle("cl", neither).await.error_code(), Some(ErrorCode::BadRequest));
    assert!(fake.delivered().is_empty());
}

/// Revision check through the engine: a stale revision is refused, is not
/// cached (the id stays retryable) and never reaches the adapter.
#[tokio::test]
async fn a_stale_revision_is_refused_and_not_delivered() {
    let (core, fake, rid) = core_with_request(false);
    let res = core.handle("cl", answer_opt("c1", &rid, Some(99))).await;
    assert_eq!(res.error_code(), Some(ErrorCode::RequestChanged));
    assert!(fake.delivered().is_empty());
    assert_eq!(core.handle("cl", answer_opt("c1", &rid, Some(1))).await, CommandResult::delivered("c1"));
    assert_eq!(fake.delivered().len(), 1);
}

// ---- idem.rs ----

/// idem.rs:32 `16 * 1024`.
#[test]
fn the_default_capacity_is_16384() {
    assert_eq!(IDEM_CAPACITY, 16_384);
}

/// idem.rs:52 `<` -> `<=`: a result exactly `ttl` old has expired.
#[test]
fn a_result_exactly_ttl_old_is_expired() {
    let ttl = Duration::from_secs(900);
    let mut c = IdemCache::new(ttl);
    let t = Instant::now();
    c.begin("cl", "1", [1; 32], t);
    c.finish("cl", "1", CommandResult::ok("1"), true, t);
    assert!(matches!(c.begin("cl", "1", [1; 32], t + ttl - Duration::from_nanos(1)), Begin::Done(_)));
    assert!(matches!(c.begin("cl", "1", [1; 32], t + ttl), Begin::Run));
}

/// A full table refuses new ids but still answers known ones.
#[test]
fn a_full_table_refuses_new_ids_only() {
    let t = Instant::now();
    let mut c = IdemCache::with_capacity(Duration::from_secs(900), 1);
    assert!(matches!(c.begin("cl", "1", [1; 32], t), Begin::Run));
    assert!(matches!(c.begin("cl", "2", [2; 32], t), Begin::Full));
    assert!(matches!(c.begin("cl", "1", [1; 32], t), Begin::Wait(_)));
}

// ---- registry.rs ----

fn reg() -> Registry {
    let mut r = Registry::new("m".into());
    for a in ["a1", "a2"] {
        r.apply(Observation::AgentUpsert(agent_obs(a)));
    }
    r
}

fn rid_of(r: &Registry, agent: &str) -> String {
    r.agent(agent).unwrap().pending.as_ref().unwrap().id.clone()
}

fn fallback_obs(agent: &str, k: &str) -> RequestObs {
    let mut o = permission_obs(agent, k);
    o.kind = RequestKind::BlockedUnparsed;
    o.answer_path = AnswerPath::None;
    o.options = vec![];
    o
}

fn resolved_ids(changes: &[Change]) -> Vec<String> {
    changes
        .iter()
        .filter_map(|c| match c {
            Change::RequestResolved { request_id, .. } => Some(request_id.clone()),
            _ => None,
        })
        .collect()
}

fn opened(changes: &[Change]) -> bool {
    changes.iter().any(|c| matches!(c, Change::RequestOpened(_)))
}

/// registry.rs:670/671 `||` -> `&&`: a change to any one field bumps the revision.
#[test]
fn each_semantic_field_bumps_the_revision_alone() {
    type Edit = fn(&mut RequestObs);
    let edits: [(&str, Edit); 7] = [
        ("kind", |o| o.kind = RequestKind::Question),
        ("answer_path", |o| o.answer_path = AnswerPath::Keys),
        ("title", |o| o.title = "other".into()),
        ("detail", |o| o.detail = "other".into()),
        ("options", |o| o.options.pop().map(|_| ()).unwrap_or_default()),
        ("questions", |o| {
            o.questions = Some(vec![Question { id: "q".into(), header: None, text: "t".into(), multi_select: false, options: vec![] }])
        }),
        ("free_text", |o| o.free_text = true),
    ];
    for (name, edit) in edits {
        let mut r = reg();
        r.apply(Observation::RequestSeen(permission_obs("a1", "k1")));
        let rid = rid_of(&r, "a1");
        assert_eq!(r.request(&rid).unwrap().revision, 1);
        let mut o = permission_obs("a1", "k1");
        edit(&mut o);
        r.apply(Observation::RequestSeen(o));
        assert_eq!(r.request(&rid).unwrap().revision, 2, "{name}");
    }
    // Fields that are not semantic leave the revision alone.
    let mut r = reg();
    r.apply(Observation::RequestSeen(permission_obs("a1", "k1")));
    let rid = rid_of(&r, "a1");
    let mut o = permission_obs("a1", "k1");
    o.raw_screen = Some("screen".into());
    r.apply(Observation::RequestSeen(o));
    assert_eq!(r.request(&rid).unwrap().revision, 1);
}

/// registry.rs claim rules: not ready, stale revision, unknown option, option
/// with an unknown effect, text without free_text, blank or forbidden text.
#[test]
fn claim_enforces_every_rule_and_leaves_the_request_open() {
    let mut r = reg();
    let mut p = permission_obs("a1", "k1");
    p.ready = false;
    r.apply(Observation::RequestSeen(p));
    let rid = rid_of(&r, "a1");
    let opt = |s: &str| Answer::Option(s.into());
    assert_eq!(r.claim(&rid, opt("allow"), None).unwrap_err(), ErrorCode::RequestNotReady);
    r.apply(Observation::RequestReady { agent_id: "a1".into(), key: key("k1"), ready: true });
    assert_eq!(r.claim(&rid, opt("allow"), Some(2)).unwrap_err(), ErrorCode::RequestChanged);
    assert_eq!(r.claim(&rid, opt("nope"), None).unwrap_err(), ErrorCode::BadRequest);
    assert_eq!(r.claim(&rid, Answer::Text("hi".into()), None).unwrap_err(), ErrorCode::BadRequest);
    assert_eq!(r.request(&rid).unwrap().status, RequestStatus::Open);
    // Claim succeeds with the matching revision and publishes the new state.
    let (req, changes) = r.claim(&rid, opt("allow"), Some(1)).unwrap();
    assert_eq!(req.status, RequestStatus::Answering);
    assert!(changes.iter().any(|c| matches!(c, Change::AgentUpdated(a)
        if a.pending.as_ref().is_some_and(|p| p.status == RequestStatus::Answering))));
    assert_eq!(r.request(&rid).unwrap().status, RequestStatus::Answering);
    assert_eq!(r.claim(&rid, opt("allow"), None).unwrap_err(), ErrorCode::RequestBusy);
    assert_eq!(r.claim("missing", opt("allow"), None).unwrap_err(), ErrorCode::RequestGone);
}

#[test]
fn claim_checks_the_text_and_effect_rules() {
    let mut r = reg();
    let mut p = permission_obs("a1", "k1");
    p.free_text = true;
    p.options.push(RequestOption { id: "weird".into(), label: "?".into(), description: None, effect: Effect::Unknown, confirm: false });
    r.apply(Observation::RequestSeen(p));
    let rid = rid_of(&r, "a1");
    assert_eq!(r.claim(&rid, Answer::Option("weird".into()), None).unwrap_err(), ErrorCode::BadRequest);
    for bad in ["", "   \n\t", "a\u{0}b", "a\u{202E}b"] {
        assert_eq!(r.claim(&rid, Answer::Text(bad.into()), None).unwrap_err(), ErrorCode::BadRequest, "{bad:?}");
    }
    assert!(r.claim(&rid, Answer::Text("fine\ntext".into()), None).is_ok());
    // A request nothing can answer is not answerable, whatever the text.
    let mut r = reg();
    r.apply(Observation::FallbackSeen(fallback_obs("a1", "f1")));
    let rid = rid_of(&r, "a1");
    assert_eq!(r.claim(&rid, Answer::Option("allow".into()), None).unwrap_err(), ErrorCode::NotAnswerable);
}

/// registry.rs:292/293 (`==` -> `!=`, `&&` -> `||`): FallbackEnded resolves
/// only a request that is still a fallback in both respects (fail closed).
#[test]
fn fallback_ended_resolves_only_a_real_fallback() {
    // A real fallback is resolved by the terminal.
    let mut r = reg();
    r.apply(Observation::FallbackSeen(fallback_obs("a1", "f1")));
    let rid = rid_of(&r, "a1");
    let ch = r.apply(Observation::FallbackEnded { agent_id: "a1".into(), key: key("f1"), request_id: rid.clone() });
    assert_eq!(resolved_ids(&ch), vec![rid.clone()]);
    assert!(r.agent("a1").unwrap().pending.is_none());
    // Structured request: kind differs, answer path matches nothing special.
    let mut r = reg();
    r.apply(Observation::RequestSeen(permission_obs("a1", "k1")));
    let rid = rid_of(&r, "a1");
    let ch = r.apply(Observation::FallbackEnded { agent_id: "a1".into(), key: key("k1"), request_id: rid.clone() });
    assert!(ch.is_empty());
    assert_eq!(r.request(&rid).unwrap().status, RequestStatus::Open);
    // Blocked request that has an answer path: kind matches, path does not.
    let mut r = reg();
    let mut o = fallback_obs("a1", "f1");
    o.answer_path = AnswerPath::Keys;
    r.apply(Observation::RequestSeen(o));
    let rid = rid_of(&r, "a1");
    let ch = r.apply(Observation::FallbackEnded { agent_id: "a1".into(), key: key("f1"), request_id: rid.clone() });
    assert!(ch.is_empty());
    assert!(r.request(&rid).is_some());
    // Structured kind without an answer path: path matches, kind does not.
    let mut r = reg();
    let mut o = permission_obs("a1", "k1");
    o.answer_path = AnswerPath::None;
    r.apply(Observation::RequestSeen(o));
    let rid = rid_of(&r, "a1");
    let ch = r.apply(Observation::FallbackEnded { agent_id: "a1".into(), key: key("k1"), request_id: rid.clone() });
    assert!(ch.is_empty());
    assert!(r.request(&rid).is_some());
}

/// registry.rs:296 `==` -> `!=`: FallbackEnded confirms the key gone only for
/// the request that key last opened, so a stale id cannot reopen the key.
#[test]
fn fallback_ended_confirms_only_the_keys_own_request() {
    // Own request: the key is confirmed gone, so the dialog may reappear.
    let mut r = reg();
    r.apply(Observation::FallbackSeen(fallback_obs("a1", "f1")));
    let rid = rid_of(&r, "a1");
    r.apply(Observation::FallbackEnded { agent_id: "a1".into(), key: key("f1"), request_id: rid });
    let ch = r.apply(Observation::FallbackSeen(fallback_obs("a1", "f1")));
    assert!(opened(&ch), "confirmed gone: a new occurrence opens");

    // Stale request id for a key whose latest request ended unconfirmed.
    let mut r = reg();
    r.apply(Observation::FallbackSeen(fallback_obs("a1", "f1")));
    let old = rid_of(&r, "a1");
    r.apply(Observation::FallbackEnded { agent_id: "a1".into(), key: key("f1"), request_id: old.clone() });
    r.apply(Observation::FallbackSeen(fallback_obs("a1", "f1")));
    let newer = rid_of(&r, "a1");
    assert_ne!(old, newer);
    r.apply(Observation::RequestEndedById { request_id: newer, by: ResolvedBy::Uncertain });
    r.apply(Observation::FallbackEnded { agent_id: "a1".into(), key: key("f1"), request_id: old });
    let ch = r.apply(Observation::FallbackSeen(fallback_obs("a1", "f1")));
    assert!(!opened(&ch), "a stale id must not confirm the newer occurrence gone");
}

/// registry.rs:404 `||` -> `&&`: migrating to itself or from/to an unknown agent changes nothing.
#[test]
fn migrate_ignores_self_and_unknown_agents() {
    let mut r = reg();
    r.apply(Observation::RequestSeen(permission_obs("a1", "k1")));
    let rid = rid_of(&r, "a1");
    assert!(r.apply(Observation::AgentMigrated { from: "a1".into(), to: "a1".into() }).is_empty());
    assert!(r.apply(Observation::AgentMigrated { from: "ghost".into(), to: "a1".into() }).is_empty());
    assert!(r.apply(Observation::AgentMigrated { from: "a1".into(), to: "ghost".into() }).is_empty());
    assert_eq!(rid_of(&r, "a1"), rid);
    assert!(r.agent("a2").is_some());
}

/// registry.rs:459 `>` -> `>=`: when both agents hold the same key at the same
/// generation, the target's state (here: confirmed gone) is kept.
#[test]
fn migration_keeps_the_target_key_on_a_generation_tie() {
    let mut r = reg();
    // a1 (target): k resolved and confirmed gone, generation 1.
    r.apply(Observation::RequestSeen(permission_obs("a1", "k")));
    r.apply(Observation::RequestEnded { agent_id: "a1".into(), key: key("k"), by: ResolvedBy::Terminal });
    // a2 (source): k resolved but not confirmed gone, generation 1; k2 pending.
    r.apply(Observation::RequestSeen(permission_obs("a2", "k")));
    r.apply(Observation::RequestEnded { agent_id: "a2".into(), key: key("k"), by: ResolvedBy::Uncertain });
    r.apply(Observation::RequestSeen(permission_obs("a2", "k2")));
    let moved = rid_of(&r, "a2");
    r.apply(Observation::AgentMigrated { from: "a2".into(), to: "a1".into() });
    assert_eq!(rid_of(&r, "a1"), moved);
    assert!(r.agent("a2").is_none());
    // The dialog for `k` was confirmed gone on the target: it may reopen.
    let ch = r.apply(Observation::RequestSeen(permission_obs("a1", "k")));
    assert!(opened(&ch));
}

// ---- server.rs ----

struct Server {
    url: String,
}

async fn start(opts: ServerOptions) -> Server {
    let core = Core::new(test_machine(), vec![FakeAdapter::new()]);
    let auth = Arc::new(Auth::new("secret".into()));
    let app = router(core, auth, opts);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Server { url }
}

async fn next(ws: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>) -> Value {
    loop {
        match ws.next().await {
            Some(Ok(Message::Text(t))) => return serde_json::from_str(t.as_str()).unwrap(),
            Some(Ok(Message::Close(f))) => return json!({"closed": f.map(|f| u16::from(f.code))}),
            Some(Ok(_)) => continue,
            Some(Err(_)) | None => return json!({"closed": null}),
        }
    }
}

fn hello_with_client_id(id: &str) -> String {
    json!({"type":"hello","protocol":"acd/1","token":"secret","client":{"id":id,"name":"t","version":"0"}}).to_string()
}

/// server.rs:195 `==` -> `!=`: an Origin in the allow-list gets in, any other is refused.
#[tokio::test]
async fn only_listed_origins_may_upgrade() {
    let s = start(ServerOptions { allowed_origins: vec!["https://ok.example".into()], ..Default::default() }).await;
    let with_origin = |o: &str| {
        let mut req = s.url.as_str().into_client_request().unwrap();
        req.headers_mut().insert("Origin", o.parse().unwrap());
        req
    };
    let err = connect_async(with_origin("https://evil.example")).await.unwrap_err();
    assert!(err.to_string().contains("403"), "{err}");
    let (mut ws, _) = connect_async(with_origin("https://ok.example")).await.unwrap();
    ws.send(Message::text(hello_with_client_id("c1"))).await.unwrap();
    assert_eq!(next(&mut ws).await["type"], "welcome");
}

/// server.rs:315 `>` -> `>=`: a client id of exactly 128 bytes is accepted, 129 is not.
#[tokio::test]
async fn client_id_limit_is_inclusive() {
    let s = start(ServerOptions::default()).await;
    let (mut ok, _) = connect_async(s.url.as_str()).await.unwrap();
    ok.send(Message::text(hello_with_client_id(&"c".repeat(128)))).await.unwrap();
    assert_eq!(next(&mut ok).await["type"], "welcome");
    let (mut bad, _) = connect_async(s.url.as_str()).await.unwrap();
    bad.send(Message::text(hello_with_client_id(&"c".repeat(129)))).await.unwrap();
    assert_eq!(next(&mut bad).await["code"], "bad_request");
    assert_eq!(next(&mut bad).await["closed"], 4401);
}

/// server.rs `Auth::generation -> 1`: it counts rotations from zero.
#[test]
fn auth_generation_counts_rotations() {
    let auth = Auth::new("t".into());
    assert_eq!(auth.generation(), 0);
    auth.rotate("u".into());
    assert_eq!(auth.generation(), 1);
    auth.rotate("v".into());
    assert_eq!(auth.generation(), 2);
}

// ---- tls.rs ----

fn bundle_with(extra_key_block: &str) -> String {
    let f = generate("bundle");
    format!("{extra_key_block}{}", f.cert_pem)
}

/// tls.rs:73/78 `delete match arm "RSA PRIVATE KEY" / "EC PRIVATE KEY"`: the
/// bundle accepts PKCS#1 and SEC1 key blocks (the PEM layer only checks
/// framing, so the body is arbitrary base64).
#[test]
fn bundle_accepts_pkcs1_and_sec1_key_blocks() {
    for label in ["RSA PRIVATE KEY", "EC PRIVATE KEY"] {
        let b = bundle_with(&format!("-----BEGIN {label}-----\nAAAA\n-----END {label}-----\n"));
        let f = KeyFiles::from_bundle(&b).unwrap_or_else(|e| panic!("{label}: {e}"));
        assert!(f.key_pem.contains(label));
    }
    let b = bundle_with("-----BEGIN DSA PRIVATE KEY-----\nAAAA\n-----END DSA PRIVATE KEY-----\n");
    assert!(KeyFiles::from_bundle(&b).is_err(), "other labels are refused");
}

/// tls.rs:93 `delete !`: blank lines between blocks are fine, other text outside them is not.
#[test]
fn bundle_allows_blank_lines_but_not_stray_text() {
    let f = generate("bundle");
    let plain = f.to_bundle();
    assert!(KeyFiles::from_bundle(&format!("\n\n{}\n  \n{}\n\n", f.key_pem, f.cert_pem)).is_ok());
    assert!(KeyFiles::from_bundle(&plain).is_ok());
    assert!(KeyFiles::from_bundle(&format!("note\n{plain}")).is_err());
    assert!(KeyFiles::from_bundle(&format!("{}trailing\n{}", f.key_pem, f.cert_pem)).is_err());
}
