//! Model-based property test of the request engine.
//!
//! A random sequence of observations (open, update, revision bump, end with
//! each reason, expiry, agent loss, dialog-gone marks), answer commands
//! (with stale revisions, bad options, retries under the same command id,
//! adapter failures) and daemon restarts runs against a real `Core` and
//! against a small reference model of the spec. After every step the engine
//! must agree with the model, and:
//!
//! - a request is delivered at most once, and resolved at most once;
//! - an answer to a stale revision or a resolved request is refused;
//! - request ids are unique within an epoch, and a reused id after a restart
//!   has a different `opened_at` (at the wire's millisecond precision);
//! - the same command id with the same payload replays the same result, and
//!   with a different payload is refused (`duplicate_mismatch`);
//! - the event stream is gap-free.
mod proptest_support;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use acd_core::testing::*;
use acd_core::*;
use acd_protocol::*;
use proptest::prelude::*;
use proptest_support::check;

const AGENTS: usize = 3;
const KEYS: usize = 3;
const CLIENTS: usize = 2;
const CMD_IDS: usize = 4;

#[derive(Clone, Copy, Debug)]
enum Fail {
    No,
    NotDelivered,
    Uncertain,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RevSel {
    Omit,
    Current,
    Stale,
}

#[derive(Clone, Copy, Debug)]
enum What {
    Allow,
    Deny,
    Bogus,
    Text,
}

#[derive(Clone, Debug)]
enum Op {
    Upsert(usize),
    AgentGone(usize),
    Seen { a: usize, k: usize, var: u8, none_path: bool, ready: bool, reusable: bool },
    Ready { a: usize, k: usize, ready: bool },
    End { a: usize, k: usize, by: ResolvedBy },
    EndById { t: usize, by: ResolvedBy, reason: Option<ResolveReason>, message: bool },
    /// What the external adapter does when an answer is never confirmed.
    Expire { t: usize },
    EndIfOpen { t: usize, by: ResolvedBy },
    DialogGone { a: usize, k: usize },
    GoneById { t: usize },
    Answer { client: usize, cmd: usize, t: usize, rev: RevSel, what: What, fail: Fail },
    OversizedId { t: usize },
    Restart,
}

fn by() -> impl Strategy<Value = ResolvedBy> {
    proptest::sample::select(vec![
        ResolvedBy::Inbox,
        ResolvedBy::Terminal,
        ResolvedBy::Agent,
        ResolvedBy::Superseded,
        ResolvedBy::Uncertain,
        ResolvedBy::Lost,
    ])
}

fn op() -> impl Strategy<Value = Op> {
    let a = 0..AGENTS;
    let k = 0..KEYS;
    let t = 0..32usize;
    prop_oneof![
        2 => a.clone().prop_map(Op::Upsert),
        1 => a.clone().prop_map(Op::AgentGone),
        12 => (a.clone(), k.clone(), 0u8..3, prop::bool::weighted(0.15), prop::bool::weighted(0.85), any::<bool>())
            .prop_map(|(a, k, var, none_path, ready, reusable)| Op::Seen { a, k, var, none_path, ready, reusable }),
        2 => (a.clone(), k.clone(), any::<bool>()).prop_map(|(a, k, ready)| Op::Ready { a, k, ready }),
        4 => (a.clone(), k.clone(), by()).prop_map(|(a, k, by)| Op::End { a, k, by }),
        3 => (t.clone(), by(), proptest::option::of(proptest::sample::select(vec![ResolveReason::StrayKeystroke, ResolveReason::Rejected])), any::<bool>())
            .prop_map(|(t, by, reason, message)| Op::EndById { t, by, reason, message }),
        1 => t.clone().prop_map(|t| Op::Expire { t }),
        2 => (t.clone(), by()).prop_map(|(t, by)| Op::EndIfOpen { t, by }),
        2 => (a.clone(), k.clone()).prop_map(|(a, k)| Op::DialogGone { a, k }),
        1 => t.clone().prop_map(|t| Op::GoneById { t }),
        16 => (0..CLIENTS, 0..CMD_IDS, t.clone(),
               proptest::sample::select(vec![RevSel::Omit, RevSel::Current, RevSel::Stale]),
               proptest::sample::select(vec![What::Allow, What::Allow, What::Deny, What::Bogus, What::Text]),
               prop_oneof![8 => Just(Fail::No), 1 => Just(Fail::NotDelivered), 1 => Just(Fail::Uncertain)])
            .prop_map(|(client, cmd, t, rev, what, fail)| Op::Answer { client, cmd, t, rev, what, fail }),
        1 => t.prop_map(|t| Op::OversizedId { t }),
        1 => Just(Op::Restart),
    ]
}

// ---- the reference model --------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Ev {
    Opened(String),
    Resolved { rid: String, agent: String, by: ResolvedBy, option_id: Option<String>, reason: Option<ResolveReason>, message: Option<String> },
}

#[derive(Clone, Debug)]
struct MReq {
    agent: usize,
    revision: u64,
    answering: bool,
    content: (u8, bool),
    ready: bool,
    claimed: Option<Answer>,
}

#[derive(Clone, Debug, Default)]
struct MKey {
    generation: u32,
    open: Option<String>,
    last: Option<String>,
    gone: bool,
    reusable: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct Payload {
    request_id: String,
    revision: Option<u64>,
    answer: Answer,
}

#[derive(Default)]
struct Model {
    exists: [bool; AGENTS],
    reqs: HashMap<String, MReq>,
    pending: [Option<String>; AGENTS],
    keys: HashMap<(usize, usize), MKey>,
    issued: Vec<String>,
    next_id: u64,
    cache: HashMap<(usize, usize), (Payload, CommandResult)>,
    delivered: Vec<(String, Answer)>,
    resolved_once: HashSet<String>,
}

fn gone_by(by: ResolvedBy) -> bool {
    matches!(by, ResolvedBy::Inbox | ResolvedBy::Terminal | ResolvedBy::Agent)
}

impl Model {
    fn target(&self, t: usize) -> String {
        let n = self.issued.len() + 1;
        self.issued.get(t % n).cloned().unwrap_or_else(|| "r999".to_string())
    }

    fn resolve(&mut self, rid: &str, by: ResolvedBy, reason: Option<ResolveReason>, message: Option<String>, ev: &mut Vec<Ev>) {
        let Some(r) = self.reqs.remove(rid) else { return };
        let option_id = match (by, &r.claimed) {
            (ResolvedBy::Inbox, Some(Answer::Option(o))) => Some(o.clone()),
            _ => None,
        };
        for k in self.keys.values_mut() {
            if k.open.as_deref() == Some(rid) {
                k.open = None;
            }
        }
        if self.pending[r.agent].as_deref() == Some(rid) {
            self.pending[r.agent] = None;
        }
        self.resolved_once.insert(rid.to_string());
        ev.push(Ev::Resolved { rid: rid.to_string(), agent: format!("a{}", r.agent), by, option_id, reason, message });
    }

    fn mark_gone_by_id(&mut self, rid: &str) {
        for k in self.keys.values_mut() {
            if k.last.as_deref() == Some(rid) {
                k.gone = true;
            }
        }
    }

    fn apply(&mut self, op: &Op) -> Vec<Ev> {
        let mut ev = Vec::new();
        match op {
            Op::Upsert(a) => self.exists[*a] = true,
            Op::AgentGone(a) => {
                if self.exists[*a] {
                    if let Some(p) = self.pending[*a].clone() {
                        let by = if self.reqs[&p].answering { ResolvedBy::Uncertain } else { ResolvedBy::Lost };
                        self.resolve(&p, by, None, None, &mut ev);
                    }
                    self.exists[*a] = false;
                    self.keys.retain(|(ka, _), _| ka != a);
                }
            }
            Op::Seen { a, k, var, none_path, ready, reusable } => {
                if !self.exists[*a] {
                    return ev;
                }
                let content = (*var, *none_path);
                if let Some(rid) = self.keys.get(&(*a, *k)).and_then(|s| s.open.clone()) {
                    let r = self.reqs.get_mut(&rid).expect("open key has a request");
                    r.ready = *ready;
                    if !r.answering {
                        if r.content != content {
                            r.revision += 1;
                        }
                        r.content = content;
                    }
                    return ev;
                }
                if let Some(s) = self.keys.get(&(*a, *k))
                    && s.generation > 0
                    && (!s.gone || !s.reusable)
                {
                    return ev;
                }
                if let Some(p) = self.pending[*a].clone() {
                    let by = if self.reqs[&p].answering { ResolvedBy::Uncertain } else { ResolvedBy::Superseded };
                    self.resolve(&p, by, None, None, &mut ev);
                }
                self.next_id += 1;
                let rid = format!("r{}", self.next_id);
                let s = self.keys.entry((*a, *k)).or_default();
                s.reusable = *reusable;
                s.generation += 1;
                s.open = Some(rid.clone());
                s.last = Some(rid.clone());
                s.gone = false;
                self.reqs.insert(
                    rid.clone(),
                    MReq { agent: *a, revision: 1, answering: false, content, ready: *ready, claimed: None },
                );
                self.pending[*a] = Some(rid.clone());
                self.issued.push(rid.clone());
                ev.push(Ev::Opened(rid));
            }
            Op::Ready { a, k, ready } => {
                if let Some(rid) = self.keys.get(&(*a, *k)).and_then(|s| s.open.clone()) {
                    self.reqs.get_mut(&rid).unwrap().ready = *ready;
                }
            }
            Op::End { a, k, by } => {
                if gone_by(*by)
                    && let Some(s) = self.keys.get_mut(&(*a, *k))
                {
                    s.gone = true;
                }
                if let Some(rid) = self.keys.get(&(*a, *k)).and_then(|s| s.open.clone()) {
                    self.resolve(&rid, *by, None, None, &mut ev);
                }
            }
            Op::EndById { t, by, reason, message } => {
                let rid = self.target(*t);
                if self.reqs.contains_key(&rid) {
                    if gone_by(*by) {
                        self.mark_gone_by_id(&rid);
                    }
                    let message = message.then(|| "reported".to_string());
                    self.resolve(&rid, *by, *reason, message, &mut ev);
                }
            }
            Op::Expire { t } => {
                let rid = self.target(*t);
                if self.reqs.contains_key(&rid) {
                    self.resolve(&rid, ResolvedBy::Uncertain, None, Some("expired".into()), &mut ev);
                }
            }
            Op::EndIfOpen { t, by } => {
                let rid = self.target(*t);
                if self.reqs.get(&rid).is_some_and(|r| !r.answering) {
                    if gone_by(*by) {
                        self.mark_gone_by_id(&rid);
                    }
                    self.resolve(&rid, *by, None, None, &mut ev);
                }
            }
            Op::DialogGone { a, k } => {
                if let Some(s) = self.keys.get_mut(&(*a, *k)) {
                    s.gone = true;
                }
            }
            Op::GoneById { t } => {
                let rid = self.target(*t);
                self.mark_gone_by_id(&rid);
            }
            Op::Answer { .. } | Op::OversizedId { .. } | Op::Restart => unreachable!("handled by the runner"),
        }
        ev
    }
}

/// The expected outcome of one claim attempt.
enum Expect {
    Refused(ErrorCode),
    Delivered,
    DeliveredUncertain,
    Released(ErrorCode),
}

impl Model {
    fn judge(&self, p: &Payload, fail: Fail) -> Expect {
        let Some(r) = self.reqs.get(&p.request_id) else { return Expect::Refused(ErrorCode::RequestGone) };
        if r.answering {
            return Expect::Refused(ErrorCode::RequestBusy);
        }
        if r.content.1 {
            return Expect::Refused(ErrorCode::NotAnswerable);
        }
        if !r.ready {
            return Expect::Refused(ErrorCode::RequestNotReady);
        }
        if p.revision.is_some_and(|rev| rev != r.revision) {
            return Expect::Refused(ErrorCode::RequestChanged);
        }
        match &p.answer {
            Answer::Option(o) if o == "allow" || o == "deny" => {}
            _ => return Expect::Refused(ErrorCode::BadRequest),
        }
        match fail {
            Fail::No => Expect::Delivered,
            Fail::NotDelivered => Expect::Released(ErrorCode::ScreenMismatch),
            Fail::Uncertain => Expect::DeliveredUncertain,
        }
    }
}

fn cacheable(e: &Expect) -> bool {
    match e {
        Expect::Delivered | Expect::DeliveredUncertain => true,
        Expect::Released(_) => false,
        Expect::Refused(c) => !matches!(
            c,
            ErrorCode::RequestNotReady | ErrorCode::RequestBusy | ErrorCode::SourceUnavailable | ErrorCode::RequestChanged
        ),
    }
}

// ---- the runner -------------------------------------------------------------

fn req_obs(a: usize, k: usize, var: u8, none_path: bool, ready: bool, reusable: bool) -> RequestObs {
    let mut o = permission_obs(&format!("a{a}"), &format!("k{k}"));
    o.detail = format!("cmd{var}");
    o.answer_path = if none_path { AnswerPath::None } else { AnswerPath::Hook };
    o.ready = ready;
    o.reusable_key = reusable;
    o
}

fn drain(rx: &mut tokio::sync::broadcast::Receiver<ServerMessage>, last_seq: &mut u64, opened_at: &mut HashMap<String, Timestamp>) -> Result<Vec<Ev>, TestCaseError> {
    let mut out = Vec::new();
    loop {
        let msg = match rx.try_recv() {
            Ok(m) => m,
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
            Err(e) => return Err(TestCaseError::fail(format!("event stream broke: {e:?}"))),
        };
        let seq = msg.seq().expect("events carry seq");
        prop_assert_eq!(seq, *last_seq + 1, "gap or duplicate in the event stream");
        *last_seq = seq;
        match msg {
            ServerMessage::RequestOpened { request, .. } => {
                prop_assert_eq!(request.status, RequestStatus::Open);
                prop_assert_eq!(request.revision, 1);
                opened_at.insert(request.id.clone(), request.opened_at);
                out.push(Ev::Opened(request.id));
            }
            ServerMessage::RequestResolved { request_id, agent_id, by, option_id, reason, message, .. } => {
                out.push(Ev::Resolved { rid: request_id, agent: agent_id, by, option_id, reason, message });
            }
            _ => {}
        }
    }
    Ok(out)
}

fn run_case(ops: Vec<Op>) -> Result<(), TestCaseError> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(run_ops(ops))
}

struct Env {
    core: Arc<Core>,
    fake: Arc<FakeAdapter>,
    rx: tokio::sync::broadcast::Receiver<ServerMessage>,
    last_seq: u64,
}

fn boot() -> Env {
    let fake = FakeAdapter::new();
    let core = Core::new(test_machine(), vec![fake.clone()]);
    let (initial, rx) = core.attach(None);
    let last_seq = match &initial[0] {
        ServerMessage::Snapshot { seq, .. } => *seq,
        other => panic!("expected a snapshot, got {other:?}"),
    };
    Env { core, fake, rx, last_seq }
}

async fn run_ops(ops: Vec<Op>) -> Result<(), TestCaseError> {
    let mut env = boot();
    let mut model = Model::default();
    let mut epochs = vec![env.core.epoch()];
    // (epoch index, request id) -> opened_at; ids repeat across epochs.
    let mut opened_at: HashMap<(usize, String), Timestamp> = HashMap::new();
    let mut epoch_ix = 0usize;
    let mut live_opened: HashMap<String, Timestamp> = HashMap::new();

    for (step, op) in ops.iter().enumerate() {
        let ctx = format!("step {step}: {op:?}");
        let expected: Vec<Ev> = match op {
            Op::Restart => {
                // A new daemon: new epoch, ids start again at r1. Wait out
                // the wire's millisecond precision so opened_at can differ.
                std::thread::sleep(Duration::from_millis(2));
                env = boot();
                model = Model::default();
                live_opened.clear();
                epoch_ix += 1;
                prop_assert!(!epochs.contains(&env.core.epoch()), "epoch reused after restart");
                epochs.push(env.core.epoch());
                // Agents are gone with the old process; the sources re-upsert.
                vec![]
            }
            Op::OversizedId { t } => {
                let rid = model.target(*t);
                let msg = ClientMessage::RequestAnswer {
                    id: "x".repeat(MAX_ID_BYTES + 1),
                    request_id: rid,
                    revision: None,
                    option_id: Some("allow".into()),
                    text: None,
                };
                let res = env.core.handle("cl0", msg).await;
                prop_assert_eq!(res.error_code(), Some(ErrorCode::BadRequest), "{}", ctx);
                vec![]
            }
            Op::Answer { client, cmd, t, rev, what, fail } => {
                let rid = model.target(*t);
                let revision = match rev {
                    RevSel::Omit => None,
                    RevSel::Current => Some(model.reqs.get(&rid).map_or(1, |r| r.revision)),
                    RevSel::Stale => Some(model.reqs.get(&rid).map_or(1, |r| r.revision) + 7),
                };
                let (option_id, text, answer) = match what {
                    What::Allow => (Some("allow".to_string()), None, Answer::Option("allow".into())),
                    What::Deny => (Some("deny".to_string()), None, Answer::Option("deny".into())),
                    What::Bogus => (Some("nope".to_string()), None, Answer::Option("nope".into())),
                    What::Text => (None, Some("hello".to_string()), Answer::Text("hello".into())),
                };
                let cmd_id = format!("c{cmd}");
                let client_id = format!("cl{client}");
                let payload = Payload { request_id: rid.clone(), revision, answer: answer.clone() };
                let msg = ClientMessage::RequestAnswer { id: cmd_id.clone(), request_id: rid.clone(), revision, option_id, text };

                let mut ev = Vec::new();
                let want: CommandResult = match model.cache.get(&(*client, *cmd)) {
                    Some((p, res)) if *p == payload => res.clone(),
                    Some(_) => CommandResult::err(&cmd_id, ErrorCode::DuplicateMismatch, ErrorCode::DuplicateMismatch.describe()),
                    None => {
                        let verdict = model.judge(&payload, *fail);
                        let result = match &verdict {
                            Expect::Refused(c) | Expect::Released(c) => CommandResult::err(&cmd_id, *c, c.describe()),
                            Expect::Delivered | Expect::DeliveredUncertain => CommandResult::delivered(&cmd_id),
                        };
                        match &verdict {
                            Expect::Delivered | Expect::DeliveredUncertain | Expect::Released(_) => match fail {
                                Fail::NotDelivered => env.fake.fail_next(DeliverError::NotDelivered(ErrorCode::ScreenMismatch)),
                                Fail::Uncertain => env.fake.fail_next(DeliverError::Uncertain("test".into())),
                                Fail::No => {}
                            },
                            Expect::Refused(_) => {}
                        }
                        match &verdict {
                            Expect::Delivered => {
                                let r = model.reqs.get_mut(&rid).unwrap();
                                r.answering = true;
                                r.claimed = Some(answer.clone());
                                model.delivered.push((rid.clone(), answer.clone()));
                            }
                            Expect::DeliveredUncertain => {
                                // Uncertain: the adapter did not record it, the request resolves.
                                model.reqs.get_mut(&rid).unwrap().claimed = Some(answer.clone());
                                model.resolve(&rid, ResolvedBy::Uncertain, None, None, &mut ev);
                            }
                            Expect::Released(_) | Expect::Refused(_) => {}
                        }
                        if cacheable(&verdict) {
                            model.cache.insert((*client, *cmd), (payload.clone(), result.clone()));
                        }
                        result
                    }
                };
                let got = env.core.handle(&client_id, msg).await;
                prop_assert_eq!(&got, &want, "{}", ctx);
                ev
            }
            other => {
                // `issued` only grows on a `Seen`, so the target is the same
                // before and after the model applies the op.
                let rid_of = |t: usize| model.target(t);
                let request_ids: Vec<String> = match other {
                    Op::EndById { t, .. } | Op::Expire { t } | Op::EndIfOpen { t, .. } | Op::GoneById { t } => vec![rid_of(*t)],
                    _ => vec![],
                };
                let ev = model.apply(other);
                let rid_of = |_t: usize| request_ids[0].clone();
                match other {
                    Op::Upsert(a) => env.core.observe(Observation::AgentUpsert(agent_obs(&format!("a{a}")))),
                    Op::AgentGone(a) => env.core.observe(Observation::AgentGone { agent_id: format!("a{a}") }),
                    Op::Seen { a, k, var, none_path, ready, reusable } => {
                        env.core.observe(Observation::RequestSeen(req_obs(*a, *k, *var, *none_path, *ready, *reusable)))
                    }
                    Op::Ready { a, k, ready } => env.core.observe(Observation::RequestReady {
                        agent_id: format!("a{a}"),
                        key: CorrelationKey(format!("k{k}")),
                        ready: *ready,
                    }),
                    Op::End { a, k, by } => env.core.observe(Observation::RequestEnded {
                        agent_id: format!("a{a}"),
                        key: CorrelationKey(format!("k{k}")),
                        by: *by,
                    }),
                    Op::EndById { t, by, reason, message } => {
                        let request_id = rid_of(*t);
                        env.core.observe(Observation::RequestEndedWithReason {
                            request_id,
                            by: *by,
                            reason: *reason,
                            message: message.then(|| "reported".to_string()),
                        })
                    }
                    Op::Expire { t } => {
                        let request_id = rid_of(*t);
                        env.core.observe(Observation::RequestEndedWithReason {
                            request_id,
                            by: ResolvedBy::Uncertain,
                            reason: None,
                            message: Some("expired".into()),
                        })
                    }
                    Op::EndIfOpen { t, by } => {
                        let request_id = rid_of(*t);
                        env.core.observe(Observation::RequestEndedIfOpen { request_id, by: *by })
                    }
                    Op::DialogGone { a, k } => env.core.observe(Observation::DialogGone {
                        agent_id: format!("a{a}"),
                        key: CorrelationKey(format!("k{k}")),
                    }),
                    Op::GoneById { t } => {
                        let request_id = rid_of(*t);
                        env.core.observe(Observation::DialogGoneById { request_id })
                    }
                    _ => unreachable!(),
                }
                ev
            }
        };

        // Events: exactly the opens and resolutions the model expects, once each.
        let got = drain(&mut env.rx, &mut env.last_seq, &mut live_opened)?;
        let got: Vec<Ev> = got;
        prop_assert_eq!(&got, &expected, "events differ at {}", ctx);
        for e in &got {
            if let Ev::Opened(rid) = e {
                let ts = live_opened[rid];
                let key = (epoch_ix, rid.clone());
                prop_assert!(!opened_at.contains_key(&key), "id {rid} opened twice in one epoch");
                // The same id in an earlier epoch has a different opened_at.
                for ((ep, id), prev) in &opened_at {
                    if id == rid && *ep != epoch_ix {
                        prop_assert!(prev != &ts, "id {rid} reused with the same opened_at");
                    }
                }
                opened_at.insert(key, ts);
            }
        }

        // State: the engine agrees with the model.
        for a in 0..AGENTS {
            let agent = env.core.agent(&format!("a{a}"));
            // Agents are only present after an upsert (and not gone since).
            prop_assert_eq!(agent.is_some(), model.exists[a], "agent a{} presence at {}", a, ctx);
            if let Some(agent) = agent {
                match (&agent.pending, &model.pending[a]) {
                    (None, None) => {}
                    (Some(p), Some(rid)) => {
                        prop_assert_eq!(&p.id, rid);
                        let live = env.core.request(rid);
                        prop_assert_eq!(live.as_ref(), Some(p), "agent.pending drifted from the request at {}", ctx);
                    }
                    (got, want) => prop_assert!(false, "pending mismatch {got:?} vs {want:?} at {ctx}"),
                }
            }
        }
        for rid in &model.issued {
            match (model.reqs.get(rid), env.core.request(rid)) {
                (None, None) => {}
                (Some(m), Some(r)) => {
                    prop_assert_eq!(r.revision, m.revision, "revision of {} at {}", rid, ctx);
                    prop_assert_eq!(
                        r.status,
                        if m.answering { RequestStatus::Answering } else { RequestStatus::Open }
                    );
                }
                (m, r) => prop_assert!(false, "request {rid}: model {m:?}, engine {r:?} at {ctx}"),
            }
        }
        // Delivered at most once per request, exactly as modelled.
        let delivered = env.fake.delivered();
        prop_assert_eq!(&delivered, &model.delivered, "deliveries at {}", ctx);
        let mut seen = HashSet::new();
        for (rid, _) in &delivered {
            prop_assert!(seen.insert(rid.clone()), "request {rid} delivered twice");
        }
    }
    Ok(())
}

#[test]
fn the_engine_agrees_with_the_reference_model() {
    check("engine_model", 150, prop::collection::vec(op(), 1..60), run_case);
}

// ---- ids and idempotency, focused -----------------------------------------------

#[test]
fn ids_are_unique_per_epoch_and_reused_ids_carry_a_new_opened_at() {
    // Open and resolve many requests on one agent, restart, and again.
    check("ids_per_epoch", 40, (1usize..12, 1usize..12, any::<bool>()), |(n1, n2, reusable)| {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let mut firsts = Vec::new();
            for n in [n1, n2] {
                let env = boot();
                env.core.observe(Observation::AgentUpsert(agent_obs("a0")));
                let mut ids = HashSet::new();
                let mut rx = env.rx;
                for i in 0..n {
                    let mut o = req_obs(0, i % KEYS, 0, false, true, reusable);
                    o.key = CorrelationKey(format!("key-{i}"));
                    env.core.observe(Observation::RequestSeen(o));
                }
                let mut opened = Vec::new();
                while let Ok(m) = rx.try_recv() {
                    if let ServerMessage::RequestOpened { request, .. } = m {
                        prop_assert!(ids.insert(request.id.clone()), "duplicate id {}", request.id);
                        opened.push((request.id, request.opened_at));
                    }
                }
                prop_assert_eq!(opened.len(), n);
                prop_assert_eq!(&opened[0].0, "r1");
                firsts.push(opened[0].1);
                std::thread::sleep(Duration::from_millis(2));
            }
            prop_assert!(firsts[0] != firsts[1], "r1 of two epochs has the same opened_at");
            Ok(())
        })
    });
}
