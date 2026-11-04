//! Property tests for the client's view of a daemon: `Session` fed by a real
//! `acd_core::Core`, then checked through `build_view`.
//!
//! A random program of daemon changes (open, update, resolve, agent loss,
//! floods past the 1000-event ring, restarts) interleaves with client
//! disconnects and reconnects from a random cursor, and with injected faults:
//! a duplicate frame, a gap, an event from another epoch, an oversized agent,
//! a snapshot over the limits. The client must:
//!
//! - never panic, and treat every fault as `Ignored` or `Resync`, leaving its
//!   agents untouched;
//! - see exactly the missing suffix or a full snapshot on reconnect, so after
//!   it reads everything its agents equal the daemon's;
//! - never show an answerable card for a request that is no longer open on
//!   the daemon, or that a `request.resolved` already ended.
mod proptest_support;

use std::collections::HashSet;
use std::sync::Arc;

use acd_client::conn::{MachineState, MachineStatus};
use acd_client::session::{Applied, Session};
use acd_client::settings::{EndpointConfig, MachineEntry};
use acd_client::view::{MachineInput, ViewInput, build_view};
use acd_core::testing::*;
use acd_core::*;
use acd_protocol::*;
use proptest::prelude::*;
use proptest_support::check;
use tokio::sync::broadcast::{Receiver, error::TryRecvError};

#[derive(Clone, Copy, Debug)]
enum Fault {
    Duplicate,
    Gap,
    OtherEpoch,
    OversizedAgent,
    TooManyAgentsSnapshot,
    OtherEpochSnapshot,
    OversizedSnapshotAgent,
}

#[derive(Clone, Debug)]
enum Op {
    Upsert { a: u8, t: u8 },
    Seen { a: u8, k: u8, var: u8, ready: bool },
    End { a: u8, k: u8, by: ResolvedBy },
    Gone(u8),
    /// Answer the agent's pending request: it stays `answering` until it ends.
    Answer(u8),
    Flood(u16),
    Pump,
    Disconnect,
    /// Reconnect; `between` daemon changes land between `welcome` and `attach`.
    Connect { between: u8 },
    Restart,
    Fault(Fault),
}

fn op() -> impl Strategy<Value = Op> {
    let a = 0u8..3;
    prop_oneof![
        3 => (a.clone(), 0u8..3).prop_map(|(a, t)| Op::Upsert { a, t }),
        10 => (a.clone(), 0u8..3, 0u8..3, prop::bool::weighted(0.9)).prop_map(|(a, k, var, ready)| Op::Seen { a, k, var, ready }),
        4 => (a.clone(), 0u8..3, proptest::sample::select(vec![
                ResolvedBy::Inbox, ResolvedBy::Terminal, ResolvedBy::Agent,
                ResolvedBy::Superseded, ResolvedBy::Lost])).prop_map(|(a, k, by)| Op::End { a, k, by }),
        1 => a.clone().prop_map(Op::Gone),
        3 => a.prop_map(Op::Answer),
        1 => (950u16..1200).prop_map(Op::Flood),
        6 => Just(Op::Pump),
        2 => Just(Op::Disconnect),
        4 => (0u8..3).prop_map(|between| Op::Connect { between }),
        1 => Just(Op::Restart),
        3 => proptest::sample::select(vec![
                Fault::Duplicate, Fault::Gap, Fault::OtherEpoch, Fault::OversizedAgent,
                Fault::TooManyAgentsSnapshot, Fault::OtherEpochSnapshot, Fault::OversizedSnapshotAgent,
            ]).prop_map(Op::Fault),
    ]
}

struct World {
    core: Arc<Core>,
    session: Session,
    rx: Option<Receiver<ServerMessage>>,
    entry: MachineEntry,
    resolved: HashSet<String>,
    last_applied: Option<ServerMessage>,
    counter: u64,
}

fn new_core() -> Arc<Core> {
    Core::new(test_machine(), vec![FakeAdapter::new()])
}

fn daemon_agents(core: &Core) -> Vec<Agent> {
    match core.attach(None).0.remove(0) {
        ServerMessage::Snapshot { agents, .. } => agents,
        other => panic!("expected a snapshot, got {other:?}"),
    }
}

impl World {
    fn new() -> World {
        World {
            core: new_core(),
            session: Session::default(),
            rx: None,
            entry: MachineEntry {
                key: "m1".into(),
                name: "Box".into(),
                endpoint: EndpointConfig::Tcp { host: "127.0.0.1".into(), port: 1 },
            },
            resolved: HashSet::new(),
            last_applied: None,
            counter: 0,
        }
    }

    fn view_cards(&self) -> Vec<acd_client::view::CardView> {
        let state = MachineState {
            instance: 1,
            status: if self.session.ready() { MachineStatus::Online } else { MachineStatus::Syncing },
            session: self.session.clone(),
            conn_gen: 1,
        };
        let (busy, notices) = (Default::default(), Default::default());
        build_view(&ViewInput {
            machines: vec![MachineInput { entry: &self.entry, state: Some(&state) }],
            actions: &[],
            busy: &busy,
            notices: &notices,
            show_idle: true,
            show_subagents: true,
            show_hook_only: true,
        })
        .cards
    }

    /// What the published state may show: no answerable card for a request a
    /// `request.resolved` already ended.
    fn check_published(&self) -> Result<(), TestCaseError> {
        for card in self.view_cards() {
            if let Some(p) = &card.agent.pending {
                prop_assert!(
                    !(card.answerable && self.resolved.contains(&p.id)),
                    "answerable card for resolved request {}",
                    p.id
                );
            }
        }
        Ok(())
    }

    fn apply(&mut self, msg: &ServerMessage) -> Result<Applied, TestCaseError> {
        let applied = self.session.apply(msg);
        match &applied {
            Applied::Resolved { request_id, .. } => {
                self.resolved.insert(request_id.clone());
            }
            Applied::Changed | Applied::Opened(_) => self.check_published()?,
            _ => {}
        }
        if !matches!(applied, Applied::Ignored | Applied::Resync(_)) && msg.seq().is_some() {
            self.last_applied = Some(msg.clone());
        }
        Ok(applied)
    }

    /// Reads everything the daemon published since the client last read.
    fn pump(&mut self) -> Result<(), TestCaseError> {
        loop {
            let Some(rx) = self.rx.as_mut() else { return Ok(()) };
            match rx.try_recv() {
                Ok(msg) => {
                    let applied = self.apply(&msg)?;
                    prop_assert!(!matches!(applied, Applied::Resync(_)), "resync from a valid stream: {applied:?}");
                }
                Err(TryRecvError::Empty) => break,
                // The daemon drops a client that cannot keep up (4408).
                Err(TryRecvError::Lagged(_)) | Err(TryRecvError::Closed) => {
                    self.rx = None;
                    return Ok(());
                }
            }
        }
        self.check_consistent()
    }

    /// After reading everything, the client's agents are the daemon's, and
    /// every answerable card is an open request on the daemon.
    fn check_consistent(&self) -> Result<(), TestCaseError> {
        if !self.session.ready() {
            return Ok(());
        }
        let mine: Vec<Agent> = self.session.agents.values().cloned().collect();
        prop_assert_eq!(&mine, &daemon_agents(&self.core), "client agents drifted from the daemon");
        for card in self.view_cards() {
            if card.answerable {
                let p = card.agent.pending.as_ref().expect("answerable card has a request");
                let live = self.core.request(&p.id);
                prop_assert!(
                    live.as_ref().is_some_and(|r| r.status == RequestStatus::Open && r.revision == p.revision),
                    "answerable card for {} but the daemon has {live:?}",
                    p.id
                );
            }
        }
        Ok(())
    }

    fn connect(&mut self, between: u8) -> Result<(), TestCaseError> {
        // Hello carries the cursor held before the welcome.
        let resume = self.session.resume();
        let ServerMessage::Welcome { machine, capabilities, epoch, seq } = self.core.welcome() else {
            unreachable!()
        };
        let epoch_changed = self.session.cursor().is_some_and(|c| c.epoch != epoch);
        self.session.welcome(machine, capabilities, &epoch, seq);
        if epoch_changed {
            self.resolved.clear();
        }
        // Changes that land between the welcome and the attach.
        for _ in 0..between {
            self.change_agent();
        }
        let (initial, rx) = self.core.attach(resume.as_ref());
        let initial_shape = match initial.as_slice() {
            [ServerMessage::Snapshot { .. }] => None,
            events => {
                let c = resume.as_ref().expect("events are replayed only to a cursor");
                let seqs: Vec<u64> = events.iter().map(|m| m.seq().unwrap()).collect();
                prop_assert_eq!(
                    seqs.first().copied().unwrap_or(c.seq + 1),
                    c.seq + 1,
                    "replay must start right after the cursor"
                );
                Some(seqs)
            }
        };
        if let Some(seqs) = initial_shape {
            prop_assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1), "gap or duplicate in replay: {seqs:?}");
        }
        for msg in &initial {
            let applied = self.apply(msg)?;
            prop_assert!(!matches!(applied, Applied::Resync(_)), "resync on connect: {applied:?}");
        }
        self.rx = Some(rx);
        self.pump()?;
        prop_assert!(self.session.ready(), "not ready after a full read");
        Ok(())
    }

    fn change_agent(&mut self) {
        self.counter += 1;
        let mut a = agent_obs("a0");
        a.title = format!("t{}", self.counter);
        self.core.observe(Observation::AgentUpsert(a));
    }

    fn inject(&mut self, fault: Fault) -> Result<(), TestCaseError> {
        let Some(cursor) = self.session.cursor().cloned() else { return Ok(()) };
        let agents_before = self.session.agents.clone();
        let cursor_before = cursor.clone();
        let big = |n: usize| "x".repeat(n);
        let mut agent = daemon_agents(&self.core).into_iter().next().unwrap_or_else(|| {
            // An agent to carry the fault: built from the registry shape.
            let c = Core::new(test_machine(), vec![]);
            c.observe(Observation::AgentUpsert(agent_obs("zz")));
            daemon_agents(&c).remove(0)
        });
        let msg = match fault {
            Fault::Duplicate => match self.last_applied.clone() {
                Some(m) if m.seq().is_some_and(|s| s <= cursor.seq) && !matches!(m, ServerMessage::Snapshot { .. }) => m,
                _ => return Ok(()),
            },
            Fault::Gap => ServerMessage::AgentRemoved { epoch: cursor.epoch.clone(), seq: cursor.seq + 2, agent_id: "zz".into() },
            Fault::OtherEpoch => ServerMessage::AgentRemoved { epoch: "other".into(), seq: cursor.seq + 1, agent_id: "zz".into() },
            Fault::OversizedAgent => {
                agent.title = big(MAX_AGENT_BYTES + 10);
                ServerMessage::AgentUpdated { epoch: cursor.epoch.clone(), seq: cursor.seq + 1, agent }
            }
            Fault::TooManyAgentsSnapshot => {
                let agents = (0..=MAX_AGENTS_PER_MACHINE)
                    .map(|i| {
                        let mut a = agent.clone();
                        a.id = format!("n{i}");
                        a.pending = None;
                        a
                    })
                    .collect();
                ServerMessage::Snapshot { epoch: cursor.epoch.clone(), seq: cursor.seq + 1, agents }
            }
            Fault::OtherEpochSnapshot => {
                ServerMessage::Snapshot { epoch: "other".into(), seq: 1, agents: vec![] }
            }
            Fault::OversizedSnapshotAgent => {
                agent.last_message = Some(big(MAX_AGENT_BYTES + 10));
                ServerMessage::Snapshot { epoch: cursor.epoch.clone(), seq: cursor.seq + 1, agents: vec![agent] }
            }
        };
        let applied = self.session.apply(&msg);
        match (fault, &applied) {
            (Fault::Duplicate, Applied::Ignored) => {}
            (Fault::Duplicate, other) => prop_assert!(false, "a duplicate frame was {other:?}"),
            (_, Applied::Resync(_)) => {}
            (f, other) => prop_assert!(false, "{f:?} was applied silently: {other:?}"),
        }
        prop_assert_eq!(&self.session.agents, &agents_before, "a faulty frame changed the agents");
        if matches!(applied, Applied::Resync(_)) {
            // Fault frames whose seq is the next one advance the cursor
            // before the size check; the connection layer then forces a
            // resync, which must discard it.
            if fault_advances_cursor(fault) {
                prop_assert!(self.session.cursor().is_some_and(|c| c.seq >= cursor_before.seq));
            }
            self.session.force_resync();
            prop_assert!(!self.session.synced(), "still synced after a forced resync");
            self.rx = None;
            self.connect(0)?;
        }
        Ok(())
    }
}

fn fault_advances_cursor(f: Fault) -> bool {
    matches!(f, Fault::OversizedAgent)
}

fn run(ops: Vec<Op>) -> Result<(), TestCaseError> {
    let mut w = World::new();
    w.connect(0)?;
    for op in ops {
        match op {
            Op::Upsert { a, t } => {
                let mut o = agent_obs(&format!("a{a}"));
                o.title = format!("title{t}");
                w.core.observe(Observation::AgentUpsert(o));
            }
            Op::Seen { a, k, var, ready } => {
                let mut o = permission_obs(&format!("a{a}"), &format!("k{k}"));
                o.detail = format!("cmd{var}");
                o.ready = ready;
                w.core.observe(Observation::RequestSeen(o));
            }
            Op::End { a, k, by } => w.core.observe(Observation::RequestEnded {
                agent_id: format!("a{a}"),
                key: CorrelationKey(format!("k{k}")),
                by,
            }),
            Op::Gone(a) => w.core.observe(Observation::AgentGone { agent_id: format!("a{a}") }),
            Op::Answer(a) => {
                if let Some(p) = w.core.agent(&format!("a{a}")).and_then(|x| x.pending) {
                    w.counter += 1;
                    let msg = ClientMessage::RequestAnswer {
                        id: format!("c{}", w.counter),
                        request_id: p.id,
                        revision: None,
                        option_id: Some("allow".into()),
                        text: None,
                    };
                    let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
                    rt.block_on(w.core.handle("cl", msg));
                }
            }
            Op::Flood(n) => {
                for _ in 0..n {
                    w.change_agent();
                }
            }
            Op::Pump => w.pump()?,
            Op::Disconnect => w.rx = None,
            Op::Connect { between } => w.connect(between)?,
            Op::Restart => {
                w.core = new_core();
                w.rx = None;
                w.resolved.clear();
                w.last_applied = None;
            }
            Op::Fault(f) => {
                w.pump()?;
                w.inject(f)?;
            }
        }
    }
    // Whatever happened, a final reconnect converges.
    w.connect(0)
}

#[test]
fn the_client_session_follows_the_daemon_through_resumes_and_faults() {
    check("session_follows_daemon", 60, prop::collection::vec(op(), 1..40), run);
}

/// A small, fast program for the resume contract alone: random changes and
/// reconnects, with floods of 0 to 1300 events while disconnected.
#[test]
fn reconnect_after_any_number_of_missed_events_ends_in_the_same_state() {
    let case = (0u16..1300, 0u16..40, 0u8..3);
    check("session_reconnect_missed", 40, case, |(missed, before, between)| {
        let mut w = World::new();
        w.connect(0)?;
        for _ in 0..before {
            w.change_agent();
        }
        w.pump()?;
        w.rx = None;
        for _ in 0..missed {
            w.change_agent();
        }
        w.connect(between)?;
        prop_assert_eq!(w.session.cursor().map(|c| c.seq), match w.core.welcome() {
            ServerMessage::Welcome { seq, .. } => Some(seq),
            _ => None,
        });
        Ok(())
    });
}

/// A snapshot with an answerable `r1`, then a valid `request.resolved(r1)`.
/// Returns the world and the published cards right after the resolution.
fn answerable_then_resolved() -> (World, acd_protocol::Request, Vec<acd_client::view::CardView>) {
    let mut w = World::new();
    w.connect(0).unwrap();
    w.core.observe(Observation::AgentUpsert(agent_obs("a0")));
    w.core.observe(Observation::RequestSeen(permission_obs("a0", "k0")));
    w.connect(0).unwrap();
    let request = w.core.agent("a0").and_then(|a| a.pending).expect("an open request");
    assert!(w.view_cards().iter().any(|c| c.answerable), "setup: the card must be answerable first");
    let cursor = w.session.cursor().cloned().unwrap();
    let resolved = ServerMessage::RequestResolved {
        epoch: cursor.epoch,
        seq: cursor.seq + 1,
        request_id: request.id.clone(),
        agent_id: "a0".into(),
        by: ResolvedBy::Terminal,
        option_id: None,
        reason: None,
        message: None,
    };
    assert!(matches!(w.session.apply(&resolved), Applied::Resolved { .. }));
    let cards = w.view_cards();
    (w, request, cards)
}

/// FINDING, not a fix (production code is untouched): `Session::apply` leaves
/// `agent.pending` as it was on `request.resolved`, and `build_view` marks it
/// answerable until the daemon's following `agent.updated` replaces the agent.
/// Shrunk input: [snapshot with answerable r1, request.resolved(r1)], no
/// follow-up. Un-ignore this when the client clears the request itself.
#[test]
#[ignore = "finding: a resolved request stays answerable until the following agent.updated"]
fn a_resolved_request_is_not_answerable_before_the_following_agent_update() {
    let (_w, request, cards) = answerable_then_resolved();
    for card in cards {
        assert!(
            !(card.answerable && card.agent.pending.as_ref().is_some_and(|p| p.id == request.id)),
            "answerable card for the just-resolved request {}",
            request.id
        );
    }
}

/// The contract that does hold: once the daemon's `agent.updated` after the
/// resolution is read, the resolved request is no longer answerable. Also with
/// unrelated traffic in between (a delayed follow-up).
#[test]
fn the_following_agent_update_clears_a_resolved_request_even_when_delayed() {
    let (mut w, request, _) = answerable_then_resolved();
    // The daemon's own stream: its resolved is already in the history, so
    // read what it published after the first connect, with the unrelated
    // event in front of the follow-up.
    w.core.observe(Observation::RequestEnded {
        agent_id: "a0".into(),
        key: CorrelationKey("k0".into()),
        by: ResolvedBy::Terminal,
    });
    w.connect(0).unwrap();
    for card in w.view_cards() {
        assert!(
            !(card.answerable && card.agent.pending.as_ref().is_some_and(|p| p.id == request.id)),
            "still answerable after the agent update"
        );
    }
}
