//! Property test of the whole inbox over a socket: a real `Core` publishes
//! random changes, a scripted TLS/WebSocket server forwards them to a real
//! `Inbox`, and random faults hit the connection: invalid JSON, an unknown
//! frame type, a duplicate, a gap, an oversized frame (over the 1 MiB limit),
//! an oversized agent, a close. After every reconnect the server resumes
//! from the client's cursor, as the daemon does.
//!
//! The inbox must never stop following the daemon, never show an answerable
//! card for a request that is no longer open, and notify exactly once for each
//! `request.opened` event it was sent (at most four requests open per case,
//! plus a sentinel, so the per-machine rate limit of five is never reached).
//! The notification check waits for the sentinel's notification: the inbox
//! handles a machine's events in order, so everything before the sentinel has
//! been handled, and a suppressed or duplicated notification shows.
//!
//! `the_notified_history_evicts_the_oldest_and_keeps_the_rest` crosses the
//! 256-entry history bound through a second machine.
//!
//! Each case starts a TLS server and takes real time (reconnect backoff), so
//! the default case count is small. The pure session properties live in
//! `proptest_session.rs`.
#![cfg(owner_checks)]
mod proptest_support;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use acd_client::fake::*;
use acd_client::inbox::*;
use acd_client::settings::*;
use acd_core::testing::*;
use acd_core::*;
use acd_protocol::*;
use proptest::prelude::*;
use proptest_support::check;
use serde_json::json;
use tokio::sync::broadcast::Receiver;

const TOKEN: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8";

#[derive(Clone, Copy, Debug)]
enum Fault {
    InvalidJson,
    UnknownType,
    Close(u16),
    OversizedFrame,
    OversizedAgent,
    Duplicate,
    Gap,
    /// A request already announced arrives again under a new seq (a faulty
    /// daemon); the connection then drops.
    ReplayOpened,
}

#[derive(Clone, Debug)]
enum Op {
    Upsert(u8),
    Seen { a: u8, k: u8, var: u8 },
    End { a: u8, k: u8, by: ResolvedBy },
    Gone(u8),
    Fault(Fault),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        2 => (0u8..2).prop_map(Op::Upsert),
        6 => (0u8..2, 0u8..2, 0u8..2).prop_map(|(a, k, var)| Op::Seen { a, k, var }),
        3 => (0u8..2, 0u8..2, proptest::sample::select(vec![ResolvedBy::Inbox, ResolvedBy::Terminal, ResolvedBy::Lost]))
            .prop_map(|(a, k, by)| Op::End { a, k, by }),
        1 => (0u8..2).prop_map(Op::Gone),
        4 => proptest::sample::select(vec![
            Fault::InvalidJson, Fault::UnknownType, Fault::Close(1000), Fault::Close(1011), Fault::Close(4408),
            Fault::OversizedFrame, Fault::OversizedAgent, Fault::Duplicate, Fault::Gap, Fault::ReplayOpened,
        ]).prop_map(Op::Fault),
    ]
}

fn timing() -> acd_client::Timing {
    let ms = Duration::from_millis;
    acd_client::Timing {
        backoff_base: ms(10),
        backoff_max: ms(40),
        identity_retry: ms(200),
        idle_ping: Duration::from_secs(60),
        ping_timeout: Duration::from_secs(60),
        undo: ms(200),
        not_ready_interval: ms(50),
        connect_total: ms(3000),
        ..acd_client::Timing::default()
    }
}

struct Harness {
    f: FakeServer,
    core: Arc<Core>,
    conn: FakeConn,
    rx: Receiver<ServerMessage>,
    last_seq: u64,
    last_frame: Option<serde_json::Value>,
    /// Every request id the daemon ever showed this client.
    shown: HashSet<String>,
    /// Every request the daemon sent as a `request.opened` event (live or
    /// replayed): the inbox must notify each of them once. A request seen only
    /// in a snapshot is not announced.
    opened: HashSet<String>,
    last_opened: Option<Request>,
}

impl Harness {
    /// The daemon's side of a fresh connection: the welcome the fake server
    /// sends comes from its state, the rest from `Core::attach(resume)`.
    async fn serve_connection(&mut self) {
        let hello = self.f.hellos().last().cloned().expect("a hello");
        let resume: Option<Cursor> = serde_json::from_value(hello["resume"].clone()).ok();
        let (initial, rx) = self.core.attach(resume.as_ref());
        self.rx = rx;
        for msg in initial {
            self.note(&msg);
            self.send(&msg).await;
        }
    }

    fn note(&mut self, msg: &ServerMessage) {
        match msg {
            ServerMessage::RequestOpened { request, .. } => {
                self.shown.insert(request.id.clone());
                self.opened.insert(request.id.clone());
                self.last_opened = Some(request.clone());
            }
            ServerMessage::Snapshot { agents, .. } => {
                for p in agents.iter().filter_map(|a| a.pending.as_ref()) {
                    self.shown.insert(p.id.clone());
                }
            }
            _ => {}
        }
        if let Some(seq) = msg.seq() {
            self.last_seq = seq;
        }
    }

    async fn send(&mut self, msg: &ServerMessage) {
        let v = serde_json::to_value(msg).unwrap();
        self.last_frame = msg.seq().map(|_| v.clone());
        self.conn.send(v).await;
    }

    async fn forward(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            self.note(&msg);
            self.send(&msg).await;
        }
    }

    async fn reconnect(&mut self) -> Result<(), TestCaseError> {
        self.conn = tokio::time::timeout(Duration::from_secs(5), self.f.next_connection())
            .await
            .map_err(|_| TestCaseError::fail("the inbox did not reconnect"))?;
        self.serve_connection().await;
        Ok(())
    }

    fn point_welcome_at_the_daemon(&self) {
        self.f.set_epoch(&self.core.epoch());
        let ServerMessage::Welcome { seq, .. } = self.core.welcome() else { unreachable!() };
        self.f.set_welcome_seq(seq);
    }

    async fn fault(&mut self, fault: Fault) -> Result<(), TestCaseError> {
        self.forward().await;
        let epoch = self.core.epoch();
        let reconnects = match fault {
            Fault::InvalidJson | Fault::Close(_) | Fault::OversizedFrame | Fault::OversizedAgent | Fault::Gap => true,
            Fault::UnknownType | Fault::Duplicate => false,
            Fault::ReplayOpened => self.last_opened.is_some(),
        };
        if reconnects {
            self.point_welcome_at_the_daemon();
        }
        match fault {
            Fault::InvalidJson => self.conn.send_raw("{not json").await,
            Fault::UnknownType => self.conn.send(json!({"type": "future.thing", "seq": 999, "x": [1, 2]})).await,
            Fault::Close(code) => self.conn.close(code).await,
            Fault::OversizedFrame => {
                self.conn.send_raw(&"x".repeat(MAX_FRAME_BYTES + 16)).await;
            }
            Fault::OversizedAgent => {
                let mut agent = match self.core.attach(None).0.remove(0) {
                    ServerMessage::Snapshot { agents, .. } => agents.into_iter().next(),
                    _ => None,
                }
                .unwrap_or_else(|| {
                    let c = Core::new(test_machine(), vec![]);
                    c.observe(Observation::AgentUpsert(agent_obs("zz")));
                    match c.attach(None).0.remove(0) {
                        ServerMessage::Snapshot { mut agents, .. } => agents.remove(0),
                        _ => unreachable!(),
                    }
                });
                agent.title = "x".repeat(MAX_AGENT_BYTES + 10);
                let frame = ServerMessage::AgentUpdated { epoch, seq: self.last_seq + 1, agent };
                self.conn.send(serde_json::to_value(&frame).unwrap()).await;
            }
            Fault::Duplicate => {
                if let Some(v) = self.last_frame.clone() {
                    self.conn.send(v).await;
                }
            }
            Fault::ReplayOpened => {
                if let Some(request) = self.last_opened.clone() {
                    let frame = ServerMessage::RequestOpened { epoch, seq: self.last_seq + 1, request };
                    self.conn.send(serde_json::to_value(&frame).unwrap()).await;
                    self.conn.close(1000).await;
                }
            }
            Fault::Gap => {
                let frame = ServerMessage::AgentRemoved { epoch, seq: self.last_seq + 2, agent_id: "zz".into() };
                self.conn.send(serde_json::to_value(&frame).unwrap()).await;
            }
        }
        if reconnects {
            self.reconnect().await?;
        }
        Ok(())
    }
}

fn daemon_view(core: &Core) -> HashMap<String, Option<String>> {
    match core.attach(None).0.remove(0) {
        ServerMessage::Snapshot { agents, .. } => agents
            .into_iter()
            .map(|a| (format!("m1/{}", a.id), a.pending.map(|p| p.id)))
            .collect(),
        _ => unreachable!(),
    }
}

async fn run_case(ops: Vec<Op>) -> Result<(), TestCaseError> {
    // At most four requests open; with the sentinel that is five, the
    // notification rate limit (5 per window), so every first announcement is
    // delivered and only the dedupe stops a replayed one.
    let opens = ops.iter().filter(|o| matches!(o, Op::Seen { .. })).count();
    prop_assume!(opens <= 4);

    let f = FakeServer::start(TOKEN, Identity::Paired).await;
    let core = Core::new(test_machine(), vec![FakeAdapter::new()]);
    f.set_epoch(&core.epoch());
    let secrets = Arc::new(MemorySecrets::default());
    secrets
        .set("m1", &MachineSecret { token: TOKEN.into(), pin: f.pin() }.to_json())
        .unwrap();
    let settings = Settings {
        client_id: "00000000-0000-4000-8000-000000000002".into(),
        hotkey: DEFAULT_HOTKEY.into(),
        hotkey_migrated: true,
        show_idle: true,
        show_subagents: true,
        show_hook_only: true,
        machines: vec![MachineEntry {
            key: "m1".into(),
            name: "Box".into(),
            endpoint: EndpointConfig::Tcp { host: "127.0.0.1".into(), port: f.port() },
        }],
    };
    let ib = Inbox::new(
        settings,
        secrets,
        InboxOptions { timing: timing(), settings_path: None, client_name: "proptest".into(), client_version: "0".into() },
    );
    let mut events = ib.events();
    ib.start();
    let conn = tokio::time::timeout(Duration::from_secs(5), f.next_connection())
        .await
        .map_err(|_| TestCaseError::fail("the inbox never connected"))?;
    let (_, rx) = core.attach(None);
    let mut h = Harness { f, core: core.clone(), conn, rx, last_seq: 0, last_frame: None, shown: HashSet::new(), opened: HashSet::new(), last_opened: None };
    h.serve_connection().await;

    for op in ops {
        match op {
            Op::Upsert(a) => core.observe(Observation::AgentUpsert(agent_obs(&format!("a{a}")))),
            Op::Seen { a, k, var } => {
                let mut o = permission_obs(&format!("a{a}"), &format!("k{k}"));
                o.detail = format!("cmd{var}");
                core.observe(Observation::RequestSeen(o));
            }
            Op::End { a, k, by } => core.observe(Observation::RequestEnded {
                agent_id: format!("a{a}"),
                key: CorrelationKey(format!("k{k}")),
                by,
            }),
            Op::Gone(a) => core.observe(Observation::AgentGone { agent_id: format!("a{a}") }),
            Op::Fault(fault) => h.fault(fault).await?,
        }
        h.forward().await;
    }
    h.forward().await;

    // The sentinel: a last request on its own agent, opened over a healthy
    // connection. Its notification proves every earlier event was handled.
    core.observe(Observation::AgentUpsert(agent_obs("sentinel")));
    core.observe(Observation::RequestSeen(permission_obs("sentinel", "ks")));
    let sentinel = core.agent("sentinel").and_then(|a| a.pending).expect("the sentinel request").id;
    h.forward().await;
    prop_assert!(h.opened.contains(&sentinel), "the sentinel open was not sent as an event");
    let mut notified = Vec::new();
    let waited = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match events.recv().await {
                Ok(InboxEvent::RequestOpened { request_id, machine_key, .. }) => {
                    let last = request_id == sentinel;
                    notified.push((machine_key, request_id));
                    if last {
                        return;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => return,
            }
        }
    })
    .await;
    prop_assert!(waited.is_ok(), "no notification for the sentinel; got {notified:?}, expected {:?}", h.opened);
    let mut once = HashSet::new();
    for (machine_key, request_id) in &notified {
        prop_assert_eq!(machine_key, "m1");
        prop_assert!(h.shown.contains(request_id), "notified for unknown request {request_id}");
        prop_assert!(once.insert(request_id.clone()), "notified twice for {request_id}");
    }
    prop_assert_eq!(&once, &h.opened, "the notified requests differ from the request.opened events sent");

    // The inbox converges on the daemon's state: the same agents with the same
    // pending request ids, and online.
    let want = daemon_view(&core);
    let mut view = ib.view();
    let settled = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            {
                let v = view.borrow();
                let got: HashMap<String, Option<String>> = v
                    .cards
                    .iter()
                    .map(|c| (c.key.clone(), c.agent.pending.as_ref().map(|p| p.id.clone())))
                    .collect();
                if got == want && v.machines.iter().all(|m| m.status == acd_client::conn::MachineStatus::Online) {
                    return;
                }
            }
            if view.changed().await.is_err() {
                return;
            }
        }
    })
    .await;
    let v = view.borrow().clone();
    prop_assert!(
        settled.is_ok(),
        "the inbox did not converge: wanted {want:?}, got {:?} / {:?}",
        v.cards.iter().map(|c| (c.key.clone(), c.agent.pending.as_ref().map(|p| p.id.clone()))).collect::<Vec<_>>(),
        v.machines.iter().map(|m| m.status.clone()).collect::<Vec<_>>()
    );
    // No answerable card for a request that is not open on the daemon.
    for card in v.cards.iter().filter(|c| c.answerable) {
        let p = card.agent.pending.as_ref().expect("answerable card has a request");
        let live = core.request(&p.id);
        prop_assert!(
            live.is_some_and(|r| r.status == RequestStatus::Open),
            "answerable card for closed request {}",
            p.id
        );
    }
    ib.remove_machine("m1").ok();
    Ok(())
}

#[test]
fn the_inbox_survives_faults_and_never_notifies_twice() {
    check("inbox_over_socket", 12, prop::collection::vec(op(), 1..14), |ops| {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(run_case(ops))
    });
}

/// The replayed `request.opened` path, always exercised (the random program
/// reaches it only sometimes): the second announcement is not notified.
#[test]
fn a_replayed_open_is_notified_once() {
    let ops = vec![
        Op::Upsert(0),
        Op::Seen { a: 0, k: 0, var: 0 },
        Op::Fault(Fault::ReplayOpened),
        Op::Seen { a: 1, k: 0, var: 0 },
        Op::Fault(Fault::ReplayOpened),
    ];
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(run_case(ops)).unwrap();
}

/// The notified history holds the last `NOTIFIED_HISTORY_MAX` announced
/// requests and evicts the oldest. History insertion and eviction happen
/// before the rate limit, so machine `m1` can push more than 256 requests
/// through it even though only five notify. To see the history from outside,
/// a second machine `m2` (same machine id `fake`, so the same history keys)
/// replays requests of `m1`: a retained key is not notified again, an evicted
/// key is, and `m2` has its own rate budget.
async fn history_case(n: usize) -> Result<(), TestCaseError> {
    let (fa, fb) = (FakeServer::start(TOKEN, Identity::Paired).await, FakeServer::start(TOKEN, Identity::Paired).await);
    let core = Core::new(test_machine(), vec![FakeAdapter::new()]);
    fa.set_epoch(&core.epoch());
    fb.set_epoch(&core.epoch());
    let secrets = Arc::new(MemorySecrets::default());
    for (key, f) in [("m1", &fa), ("m2", &fb)] {
        secrets.set(key, &MachineSecret { token: TOKEN.into(), pin: f.pin() }.to_json()).unwrap();
    }
    let entry = |key: &str, name: &str, f: &FakeServer| MachineEntry {
        key: key.into(),
        name: name.into(),
        endpoint: EndpointConfig::Tcp { host: "127.0.0.1".into(), port: f.port() },
    };
    let settings = Settings {
        client_id: "00000000-0000-4000-8000-000000000002".into(),
        hotkey: DEFAULT_HOTKEY.into(),
        hotkey_migrated: true,
        show_idle: true,
        show_subagents: true,
        show_hook_only: true,
        machines: vec![entry("m1", "A", &fa), entry("m2", "B", &fb)],
    };
    let ib = Inbox::new(
        settings,
        secrets,
        InboxOptions { timing: timing(), settings_path: None, client_name: "proptest".into(), client_version: "0".into() },
    );
    let mut events = ib.events();
    ib.start();
    let wait = |what: &'static str| TestCaseError::fail(format!("the inbox never connected to {what}"));
    let conn_a = tokio::time::timeout(Duration::from_secs(5), fa.next_connection()).await.map_err(|_| wait("m1"))?;
    let conn_b = tokio::time::timeout(Duration::from_secs(5), fb.next_connection()).await.map_err(|_| wait("m2"))?;
    core.observe(Observation::AgentUpsert(agent_obs("a0")));
    core.observe(Observation::AgentUpsert(agent_obs("s")));
    let (initial_a, mut rx) = core.attach(None);
    for msg in &initial_a {
        conn_a.send(serde_json::to_value(msg).unwrap()).await;
    }
    let (initial_b, _) = core.attach(None);
    let mut seq_b = initial_b[0].seq().expect("a snapshot has a seq");
    conn_b.send(serde_json::to_value(&initial_b[0]).unwrap()).await;

    // `m1`: n requests on one agent (each supersedes the last), then one on
    // another agent; every one is a `request.opened` event, in this order.
    let mut order: Vec<Request> = Vec::new();
    for i in 0..=n {
        let (agent, key) = if i < n { ("a0", format!("k{i}")) } else { ("s", "ks".to_owned()) };
        core.observe(Observation::RequestSeen(permission_obs(agent, &key)));
        order.push(core.agent(agent).and_then(|a| a.pending).expect("an open request"));
        while let Ok(msg) = rx.try_recv() {
            conn_a.send(serde_json::to_value(&msg).unwrap()).await;
        }
    }
    prop_assert_eq!(order.iter().map(|r| &r.id).collect::<HashSet<_>>().len(), order.len(), "request ids repeat");
    // Wait until `m1` handled all of them: the sentinel agent's request ends
    // after its open, and the notice it leaves is handled after that open.
    core.observe(Observation::RequestEnded { agent_id: "s".into(), key: CorrelationKey("ks".into()), by: ResolvedBy::Terminal });
    while let Ok(msg) = rx.try_recv() {
        conn_a.send(serde_json::to_value(&msg).unwrap()).await;
    }
    let mut view = ib.view();
    let handled = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if view.borrow().cards.iter().any(|c| c.key == "m1/s" && c.notice.is_some()) {
                return;
            }
            if view.changed().await.is_err() {
                return;
            }
        }
    })
    .await;
    prop_assert!(handled.is_ok(), "m1 did not handle its {} requests", order.len());

    // The model: the history is the last `NOTIFIED_HISTORY_MAX` opens.
    let cut = order.len().saturating_sub(NOTIFIED_HISTORY_MAX);
    let (evicted, retained) = order.split_at(cut);
    let mut replays: Vec<Request> = vec![retained[0].clone(), retained[retained.len() / 2].clone()];
    let mut expect: Vec<String> = Vec::new();
    if let (Some(first), Some(last)) = (evicted.first(), evicted.last()) {
        replays.push(first.clone());
        expect.push(first.id.clone());
        if last.id != first.id {
            replays.push(last.clone());
            expect.push(last.id.clone());
        }
    }
    let mut fresh = order[0].clone();
    fresh.id = "fresh".into();
    replays.push(fresh);
    expect.push("fresh".into());
    for request in replays {
        seq_b += 1;
        let frame = ServerMessage::RequestOpened { epoch: core.epoch(), seq: seq_b, request };
        conn_b.send(serde_json::to_value(&frame).unwrap()).await;
    }
    // Everything `m2` handles comes before "fresh": the retained replays make
    // no event, the evicted ones do.
    let mut got = Vec::new();
    let waited = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match events.recv().await {
                Ok(InboxEvent::RequestOpened { request_id, machine_key, .. }) if machine_key == "m2" => {
                    let last = request_id == "fresh";
                    got.push(request_id);
                    if last {
                        return;
                    }
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => return,
            }
        }
    })
    .await;
    prop_assert!(waited.is_ok(), "m2 never notified the fresh request; got {got:?}");
    prop_assert_eq!(&got, &expect, "m2 notification order for {} opens, history {}", order.len(), NOTIFIED_HISTORY_MAX);
    ib.remove_machine("m1").ok();
    ib.remove_machine("m2").ok();
    Ok(())
}

#[test]
fn the_notified_history_evicts_the_oldest_and_keeps_the_rest() {
    // Around the bound: 251..=263 distinct opens with the 256 limit, so both
    // sides of it (nothing evicted, one evicted, several evicted) are drawn.
    check("inbox_notified_history", 6, 250usize..=262, |n| {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(history_case(n))
    });
}
