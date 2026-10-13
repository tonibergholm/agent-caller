//! Producer leases, tracked requests, tombstones and the answer queue.
//!
//! Every entry point takes a `Txn`, so a call reads and changes the
//! registry under one lock and its events publish together. Callers lock
//! this state before the core, never the other way round.
use std::collections::{HashMap, HashSet, VecDeque};

use acd_core::{AgentObs, Answer, CorrelationKey, Observation, RequestObs, Txn};
use acd_protocol::{
    AgentKind, AgentRole, AgentState, AnswerPath, Effect, Question, RequestKind, RequestOption,
    ResolveReason, ResolvedBy, Source,
};

use super::wire::{
    Ack, AckRefused, AnswerOut, Invalid, Item, Outcome, PutBody, PutResponse, Refused,
};
use crate::ids;

const MAX_LIVE_PRODUCERS: usize = 8;
const TOMBSTONE_CAP: usize = 4096;
/// Producers that lost their lease and still hold tombstones. Past this the
/// longest idle one is forgotten.
const MAX_DORMANT: usize = 64;

pub fn agent_id(pid: &str, agent_key: &str) -> String {
    ids::bounded("external:", &format!("{pid}:{agent_key}"))
}

/// The most recent `cap` strings, oldest forgotten first.
struct Bounded {
    order: VecDeque<String>,
    set: HashSet<String>,
    cap: usize,
}

impl Bounded {
    fn new(cap: usize) -> Self {
        Self {
            order: VecDeque::new(),
            set: HashSet::new(),
            cap,
        }
    }

    fn insert(&mut self, value: &str) {
        if !self.set.insert(value.to_owned()) {
            return;
        }
        self.order.push_back(value.to_owned());
        if self.order.len() > self.cap
            && let Some(oldest) = self.order.pop_front()
        {
            self.set.remove(&oldest);
        }
    }

    fn contains(&self, value: &str) -> bool {
        self.set.contains(value)
    }
}

struct Tracked {
    request_id: String,
    agent_key: String,
}

struct Queued {
    answer_id: String,
    key: String,
    request_id: String,
    revision: u64,
    option_id: Option<String>,
    text: Option<String>,
    expires_at_ms: i64,
}

impl Queued {
    fn out(&self) -> AnswerOut {
        AnswerOut {
            answer_id: self.answer_id.clone(),
            key: self.key.clone(),
            revision: self.revision,
            option_id: self.option_id.clone(),
            text: self.text.clone(),
            expires_at_ms: self.expires_at_ms,
        }
    }
}

struct Producer {
    live: bool,
    lease_expires_at_ms: i64,
    answer_timeout_ms: u64,
    last_active_ms: i64,
    agents: HashSet<String>,
    tracked: HashMap<String, Tracked>,
    tombstones: Bounded,
    queued: Vec<Queued>,
    finished: Bounded,
}

impl Producer {
    fn new() -> Self {
        Self {
            live: false,
            lease_expires_at_ms: 0,
            answer_timeout_ms: 0,
            last_active_ms: 0,
            agents: HashSet::new(),
            tracked: HashMap::new(),
            tombstones: Bounded::new(TOMBSTONE_CAP),
            queued: Vec::new(),
            finished: Bounded::new(TOMBSTONE_CAP),
        }
    }

    /// The request under `key` is over: its key is tombstoned for good and
    /// its queued answer is dropped.
    fn finish(&mut self, key: &str) {
        let Some(tracked) = self.tracked.remove(key) else {
            return;
        };
        self.tombstones.insert(key);
        let (gone, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.queued)
            .into_iter()
            .partition(|q| q.request_id == tracked.request_id);
        self.queued = kept;
        for q in gone {
            self.finished.insert(&q.answer_id);
        }
    }

    /// Requests the registry resolved on its own (an abandoned delivery
    /// resolves `uncertain`) are tombstoned here, so a producer that still
    /// lists the key cannot open a second occurrence.
    fn reconcile(&mut self, txn: &Txn<'_>) {
        let resolved: Vec<String> = self
            .tracked
            .iter()
            .filter(|(_, t)| txn.request(&t.request_id).is_none())
            .map(|(k, _)| k.clone())
            .collect();
        for key in resolved {
            self.finish(&key);
        }
    }

    fn expire_answers(&mut self, pid: &str, now: i64, txn: &mut Txn<'_>) {
        let expired: Vec<(String, String)> = self
            .queued
            .iter()
            .filter(|q| q.expires_at_ms <= now)
            .map(|q| (q.key.clone(), q.request_id.clone()))
            .collect();
        for (key, request_id) in expired {
            if txn.request(&request_id).is_some() {
                txn.apply(Observation::RequestEndedById {
                    request_id,
                    by: ResolvedBy::Uncertain,
                });
            }
            tracing::info!(producer = pid, key = %key, "external answer expired; request uncertain");
            self.finish(&key);
        }
    }

    fn expire_lease(&mut self, pid: &str, txn: &mut Txn<'_>) {
        for agent_key in self.agents.drain() {
            txn.apply(Observation::AgentGone {
                agent_id: agent_id(pid, &agent_key),
            });
        }
        let keys: Vec<String> = self.tracked.keys().cloned().collect();
        for key in keys {
            self.finish(&key);
        }
        for q in self.queued.drain(..) {
            self.finished.insert(&q.answer_id);
        }
        self.live = false;
        tracing::info!(producer = pid, "external producer lease ended");
    }

    fn pending_answers(&self, now: i64) -> Vec<AnswerOut> {
        self.queued
            .iter()
            .filter(|q| q.expires_at_ms > now)
            .map(Queued::out)
            .collect()
    }
}

#[derive(Default)]
pub struct State {
    producers: HashMap<String, Producer>,
}

fn request_obs(item: &Item, agent_id: &str) -> RequestObs {
    let options: Vec<RequestOption> = item
        .options
        .iter()
        .map(|o| RequestOption {
            id: o.id.clone(),
            label: o.label.clone(),
            description: o.description.clone(),
            effect: Effect::Choose,
            confirm: o.confirm,
        })
        .collect();
    let (header, text) = match &item.question {
        Some(q) => (q.header.clone(), q.text.clone()),
        None => (None, item.title.clone()),
    };
    let answer_path = if options.is_empty() && !item.free_text {
        AnswerPath::None
    } else {
        AnswerPath::External
    };
    RequestObs {
        agent_id: agent_id.to_owned(),
        key: CorrelationKey(item.key.clone()),
        upgrade_from: None,
        kind: RequestKind::Question,
        answer_path,
        title: item.title.clone(),
        detail: item.detail.clone(),
        options: options.clone(),
        questions: Some(vec![Question {
            id: "q1".into(),
            header,
            text,
            multi_select: false,
            options,
        }]),
        free_text: item.free_text,
        raw_screen: None,
        ready: true,
        reusable_key: false,
    }
}

fn agent_obs(pid: &str, item: &Item, agent_id: &str) -> AgentObs {
    AgentObs {
        id: agent_id.to_owned(),
        source: Source::External,
        kind: AgentKind::Other,
        title: item.agent.title.clone().unwrap_or_else(|| pid.to_owned()),
        cwd: item.agent.cwd.clone(),
        project: item.agent.project.clone(),
        state: AgentState::NeedsYou,
        failed: false,
        last_message: None,
        can_send: false,
        role: AgentRole::Main,
        parent_title: None,
    }
}

impl State {
    /// Lease and answer expiry for every producer, and requests the registry
    /// resolved without us.
    pub fn sweep(&mut self, now: i64, txn: &mut Txn<'_>) {
        for (pid, p) in self.producers.iter_mut() {
            p.reconcile(txn);
            p.expire_answers(pid, now, txn);
            if p.live && p.lease_expires_at_ms <= now {
                p.expire_lease(pid, txn);
            }
        }
        let dormant = self.producers.values().filter(|p| !p.live).count();
        for _ in MAX_DORMANT..dormant {
            let oldest = self
                .producers
                .iter()
                .filter(|(_, p)| !p.live)
                .min_by_key(|(_, p)| p.last_active_ms)
                .map(|(pid, _)| pid.clone());
            if let Some(pid) = oldest {
                self.producers.remove(&pid);
            }
        }
    }

    pub fn delete(&mut self, pid: &str, now: i64, txn: &mut Txn<'_>) {
        self.sweep(now, txn);
        if let Some(p) = self.producers.get_mut(pid)
            && p.live
        {
            p.expire_lease(pid, txn);
        }
    }

    pub fn answers(&mut self, pid: &str, now: i64, txn: &mut Txn<'_>) -> Vec<AnswerOut> {
        self.sweep(now, txn);
        self.producers
            .get(pid)
            .filter(|p| p.live)
            .map(|p| p.pending_answers(now))
            .unwrap_or_default()
    }

    /// The whole body was validated; what remains are the checks that need
    /// the current state. Nothing changes before they pass.
    fn check(&self, pid: &str, body: &PutBody) -> Result<(), Invalid> {
        let live_elsewhere = self
            .producers
            .iter()
            .filter(|(id, p)| p.live && id.as_str() != pid)
            .count();
        if !self.producers.get(pid).is_some_and(|p| p.live) && live_elsewhere >= MAX_LIVE_PRODUCERS
        {
            return Err(Invalid("producers: more than 8 live".into()));
        }
        if let Some(p) = self.producers.get(pid) {
            for (i, item) in body.items.iter().enumerate() {
                if p.tracked
                    .get(&item.key)
                    .is_some_and(|t| t.agent_key != item.agent_key)
                {
                    return Err(Invalid(format!(
                        "items[{i}].key: open under another agent_key"
                    )));
                }
            }
        }
        Ok(())
    }

    pub fn put(
        &mut self,
        pid: &str,
        body: PutBody,
        now: i64,
        txn: &mut Txn<'_>,
    ) -> Result<PutResponse, Invalid> {
        self.sweep(now, txn);
        self.check(pid, &body)?;
        let p = self
            .producers
            .entry(pid.to_owned())
            .or_insert_with(Producer::new);
        if !p.live {
            tracing::info!(producer = pid, "external producer lease started");
        }
        p.live = true;
        p.lease_expires_at_ms = now.saturating_add(body.lease_ms as i64);
        p.answer_timeout_ms = body.answer_timeout_ms();
        p.last_active_ms = now;

        let ack_refused = apply_acks(pid, p, body.acks, txn);
        let refused = upsert_items(pid, p, &body.items, txn);
        let answers = p.pending_answers(now);
        Ok(PutResponse {
            answers,
            refused,
            ack_refused,
        })
    }

    /// Queues an answer a client sent. `None` when no live request of a
    /// live producer matches.
    pub fn queue(
        &mut self,
        request_id: &str,
        revision: u64,
        answer: &Answer,
        now: i64,
    ) -> Result<(), QueueError> {
        let Some((pid, p, key)) = self.producers.iter_mut().find_map(|(pid, p)| {
            let key = p
                .tracked
                .iter()
                .find(|(_, t)| t.request_id == request_id)
                .map(|(k, _)| k.clone())?;
            Some((pid.clone(), p, key))
        }) else {
            return Err(QueueError::Gone);
        };
        if !p.live || p.lease_expires_at_ms <= now {
            return Err(QueueError::Unavailable);
        }
        let (option_id, text) = match answer {
            Answer::Option(id) => (Some(id.clone()), None),
            Answer::Text(t) => (None, Some(t.clone())),
        };
        let answer_id = uuid::Uuid::new_v4().to_string();
        tracing::info!(producer = %pid, key = %key, "external answer queued");
        p.queued.push(Queued {
            answer_id,
            key,
            request_id: request_id.to_owned(),
            revision,
            option_id,
            text,
            expires_at_ms: now.saturating_add(p.answer_timeout_ms as i64),
        });
        Ok(())
    }
}

pub enum QueueError {
    Gone,
    Unavailable,
}

fn apply_acks(pid: &str, p: &mut Producer, acks: Vec<Ack>, txn: &mut Txn<'_>) -> Vec<AckRefused> {
    let mut refused = Vec::new();
    for ack in acks {
        let Some(pos) = p.queued.iter().position(|q| q.answer_id == ack.answer_id) else {
            let reason = if p.finished.contains(&ack.answer_id) {
                "resolved"
            } else {
                "unknown"
            };
            refused.push(AckRefused {
                answer_id: ack.answer_id,
                reason,
            });
            continue;
        };
        let q = p.queued.remove(pos);
        p.finished.insert(&q.answer_id);
        if txn.request(&q.request_id).is_none() {
            p.finish(&q.key);
            refused.push(AckRefused {
                answer_id: q.answer_id,
                reason: "resolved",
            });
            continue;
        }
        let request_id = q.request_id.clone();
        let observation = match ack.outcome {
            Outcome::Applied => Observation::RequestEndedById {
                request_id,
                by: ResolvedBy::Inbox,
            },
            Outcome::Rejected => Observation::RequestEndedWithReason {
                request_id,
                by: ResolvedBy::Agent,
                reason: Some(ResolveReason::Rejected),
                message: ack.message,
            },
            Outcome::Failed => Observation::RequestEndedWithReason {
                request_id,
                by: ResolvedBy::Uncertain,
                reason: None,
                message: ack.message,
            },
        };
        txn.apply(observation);
        p.finish(&q.key);
        tracing::info!(producer = pid, key = %q.key, "external ack applied");
    }
    refused
}

fn upsert_items(pid: &str, p: &mut Producer, items: &[Item], txn: &mut Txn<'_>) -> Vec<Refused> {
    let mut refused = Vec::new();
    let mut present: HashSet<&str> = HashSet::new();
    let mut accepted: HashSet<&str> = HashSet::new();
    for item in items {
        present.insert(&item.key);
        if p.tombstones.contains(&item.key) {
            refused.push(Refused {
                key: item.key.clone(),
                reason: "resolved",
            });
            continue;
        }
        let id = agent_id(pid, &item.agent_key);
        let previous = p
            .tracked
            .iter()
            .find(|(k, t)| t.agent_key == item.agent_key && **k != item.key)
            .map(|(k, t)| (k.clone(), t.request_id.clone()));
        if let Some((old_key, request_id)) = previous {
            // Open: the producer replaced it, `by: agent`. Answering: the
            // registry resolves it `uncertain` when the new request opens.
            txn.apply(Observation::RequestEndedIfOpen {
                request_id,
                by: ResolvedBy::Agent,
            });
            p.finish(&old_key);
        }
        p.agents.insert(item.agent_key.clone());
        txn.apply(Observation::AgentUpsert(agent_obs(pid, item, &id)));
        txn.apply(Observation::RequestSeen(request_obs(item, &id)));
        match txn.request_id_for(&id, &CorrelationKey(item.key.clone())) {
            Some(request_id) => {
                p.tracked.insert(
                    item.key.clone(),
                    Tracked {
                        request_id,
                        agent_key: item.agent_key.clone(),
                    },
                );
                accepted.insert(&item.agent_key);
            }
            None => {
                p.tombstones.insert(&item.key);
                refused.push(Refused {
                    key: item.key.clone(),
                    reason: "resolved",
                });
            }
        }
    }
    let gone: Vec<(String, String)> = p
        .tracked
        .iter()
        .filter(|(k, _)| !present.contains(k.as_str()))
        .map(|(k, t)| (k.clone(), t.request_id.clone()))
        .collect();
    for (key, request_id) in gone {
        if txn.request(&request_id).is_some() {
            txn.apply(Observation::RequestEndedIfOpen {
                request_id: request_id.clone(),
                by: ResolvedBy::Agent,
            });
        }
        if txn.request(&request_id).is_some() {
            txn.apply(Observation::RequestEndedById {
                request_id,
                by: ResolvedBy::Uncertain,
            });
        }
        p.finish(&key);
    }
    let stale: Vec<String> = p
        .agents
        .iter()
        .filter(|a| !accepted.contains(a.as_str()))
        .cloned()
        .collect();
    for agent_key in stale {
        txn.apply(Observation::AgentGone {
            agent_id: agent_id(pid, &agent_key),
        });
        p.agents.remove(&agent_key);
    }
    refused
}
