use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap, HashSet};

use acd_protocol::*;
use chrono::Utc;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CorrelationKey(pub String);

#[derive(Clone, Debug, PartialEq)]
pub struct AgentObs {
    pub id: String,
    pub source: Source,
    pub kind: AgentKind,
    pub title: String,
    pub cwd: Option<String>,
    pub project: Option<String>,
    pub state: AgentState,
    pub failed: bool,
    pub last_message: Option<String>,
    pub can_send: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RequestObs {
    pub agent_id: String,
    pub key: CorrelationKey,
    pub upgrade_from: Option<CorrelationKey>,
    pub kind: RequestKind,
    pub answer_path: AnswerPath,
    pub title: String,
    pub detail: String,
    pub options: Vec<RequestOption>,
    pub questions: Option<Vec<Question>>,
    pub free_text: bool,
    pub raw_screen: Option<String>,
    pub ready: bool,
    /// Whether the key can identify a later occurrence of the same dialog
    /// (Codex and herdr fingerprint keys). Claude `prompt_id` and T3 request
    /// ids are unique per occurrence, so once resolved they never reopen.
    pub reusable_key: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Observation {
    AgentUpsert(AgentObs),
    AgentGone {
        agent_id: String,
    },
    RequestSeen(RequestObs),
    RequestReady {
        agent_id: String,
        key: CorrelationKey,
        ready: bool,
    },
    RequestPath {
        agent_id: String,
        key: CorrelationKey,
        answer_path: AnswerPath,
    },
    RequestEnded {
        agent_id: String,
        key: CorrelationKey,
        by: ResolvedBy,
    },
    DialogGone {
        agent_id: String,
        key: CorrelationKey,
    },
    /// Ends exactly this request if it is still unresolved; otherwise a no-op.
    RequestEndedById {
        request_id: String,
        by: ResolvedBy,
    },
    /// `RequestEndedById` with a reason for clients.
    RequestEndedWithReason {
        request_id: String,
        by: ResolvedBy,
        reason: ResolveReason,
    },
    /// Like `RequestEndedById`, but only while the request is `open`: a no-op
    /// once it is claimed (`answering`) or resolved.
    RequestEndedIfOpen {
        request_id: String,
        by: ResolvedBy,
    },
    /// Marks every key of this occurrence confirmed gone, resolved or not.
    DialogGoneById {
        request_id: String,
    },
    /// Like `RequestSeen`, but a no-op while a structured request is pending.
    FallbackSeen(RequestObs),
    /// Ends a fallback request and confirms only the fallback key gone.
    FallbackEnded {
        agent_id: String,
        key: CorrelationKey,
        request_id: String,
    },
    /// Moves `from`'s pending request (same id, status and revision) and
    /// its keys to `to`, then removes `from` (spec §6.2 Binding, D6).
    AgentMigrated {
        from: String,
        to: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    AgentUpdated(Agent),
    AgentRemoved(String),
    RequestOpened(Request),
    RequestResolved {
        request_id: String,
        agent_id: String,
        by: ResolvedBy,
        option_id: Option<String>,
        reason: Option<ResolveReason>,
    },
    MachineUpdated(Machine),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    Option(String),
    Text(String),
}

struct ReqEntry {
    req: Request,
    key: CorrelationKey,
    ready: bool,
    claimed: Option<Answer>,
}

#[derive(Default)]
struct KeyState {
    agent_id: String,
    generation: u64,
    open_request: Option<String>,
    confirmed_gone: bool,
    /// The last request this key claimed; kept after the request resolves so
    /// late observations can be matched to their occurrence.
    last_request: Option<String>,
    /// A non-reusable key is tombstoned once its request resolves.
    reusable: bool,
}

pub struct Registry {
    machine_id: String,
    agents: BTreeMap<String, Agent>,
    requests: HashMap<String, ReqEntry>,
    keys: HashMap<(String, CorrelationKey), KeyState>,
    /// Agents whose current `done` was marked seen; cleared when the agent leaves `done`.
    seen: HashSet<String>,
    next_id: u64,
}

impl Registry {
    pub fn new(machine_id: String) -> Self {
        Self {
            machine_id,
            agents: BTreeMap::new(),
            requests: HashMap::new(),
            keys: HashMap::new(),
            seen: HashSet::new(),
            next_id: 0,
        }
    }

    pub fn agents(&self) -> Vec<Agent> {
        self.agents.values().cloned().collect()
    }
    pub fn agent(&self, id: &str) -> Option<&Agent> {
        self.agents.get(id)
    }
    pub fn request(&self, id: &str) -> Option<&Request> {
        self.requests.get(id).map(|e| &e.req)
    }
    pub fn is_ready(&self, id: &str) -> bool {
        self.requests.get(id).is_some_and(|e| e.ready)
    }

    pub fn apply(&mut self, obs: Observation) -> Vec<Change> {
        match obs {
            Observation::AgentUpsert(o) => self.upsert_agent(o),
            Observation::AgentGone { agent_id } => self.agent_gone(&agent_id),
            Observation::AgentMigrated { from, to } => self.migrate(&from, &to),
            Observation::RequestSeen(o) => self.request_seen(o),
            Observation::RequestReady {
                agent_id,
                key,
                ready,
            } => {
                if let Some(rid) = self.request_id_for(&agent_id, &key)
                    && let Some(e) = self.requests.get_mut(&rid)
                {
                    e.ready = ready;
                }
                vec![]
            }
            Observation::RequestPath {
                agent_id,
                key,
                answer_path,
            } => {
                let Some(rid) = self.request_id_for(&agent_id, &key) else {
                    return vec![];
                };
                let Some(e) = self.requests.get_mut(&rid) else {
                    return vec![];
                };
                if e.req.status != RequestStatus::Open || e.req.answer_path == answer_path {
                    return vec![];
                }
                e.req.revision = e
                    .req
                    .revision
                    .checked_add(1)
                    .expect("request revision overflow");
                e.req.answer_path = answer_path;
                self.sync_pending(&rid).into_iter().collect()
            }
            Observation::RequestEnded { agent_id, key, by } => {
                let confirming = matches!(
                    by,
                    ResolvedBy::Inbox | ResolvedBy::Terminal | ResolvedBy::Agent
                );
                if confirming && let Some(st) = self.keys.get_mut(&(agent_id.clone(), key.clone()))
                {
                    st.confirmed_gone = true;
                }
                let rid = if confirming {
                    self.open_request_for(&agent_id, &key)
                } else {
                    self.request_id_for(&agent_id, &key)
                };
                rid.map(|rid| self.resolve_request(&rid, by))
                    .unwrap_or_default()
            }
            Observation::DialogGone { agent_id, key } => {
                if let Some(st) = self.keys.get_mut(&(agent_id, key)) {
                    st.confirmed_gone = true;
                }
                vec![]
            }
            Observation::RequestEndedById { request_id, by } => {
                self.end_by_id(&request_id, by, None)
            }
            Observation::RequestEndedWithReason {
                request_id,
                by,
                reason,
            } => self.end_by_id(&request_id, by, Some(reason)),
            Observation::RequestEndedIfOpen { request_id, by } => {
                let open = self
                    .requests
                    .get(&request_id)
                    .is_some_and(|e| e.req.status == RequestStatus::Open);
                if open {
                    self.apply(Observation::RequestEndedById { request_id, by })
                } else {
                    vec![]
                }
            }
            Observation::DialogGoneById { request_id } => {
                self.mark_gone_by_id(&request_id);
                vec![]
            }
            Observation::FallbackSeen(o) => {
                let structured = self
                    .agents
                    .get(&o.agent_id)
                    .and_then(|a| a.pending.as_ref())
                    .is_some_and(|p| p.kind != RequestKind::BlockedUnparsed);
                if structured {
                    vec![]
                } else {
                    self.request_seen(o)
                }
            }
            Observation::FallbackEnded {
                agent_id,
                key,
                request_id,
            } => {
                let still_fallback = self.requests.get(&request_id).is_some_and(|e| {
                    e.req.kind == RequestKind::BlockedUnparsed
                        && e.req.answer_path == AnswerPath::None
                });
                if let Some(k) = self.keys.get_mut(&(agent_id, key))
                    && k.last_request.as_deref() == Some(request_id.as_str())
                {
                    k.confirmed_gone = true;
                }
                if still_fallback {
                    self.resolve_request(&request_id, ResolvedBy::Terminal)
                } else {
                    vec![]
                }
            }
        }
    }

    /// Ends a request by id. A resolution that confirms the dialog is gone
    /// (`Inbox`, `Terminal`, `Agent`) marks its key gone first.
    fn end_by_id(
        &mut self,
        request_id: &str,
        by: ResolvedBy,
        reason: Option<ResolveReason>,
    ) -> Vec<Change> {
        if !self.requests.contains_key(request_id) {
            return vec![];
        }
        if matches!(
            by,
            ResolvedBy::Inbox | ResolvedBy::Terminal | ResolvedBy::Agent
        ) {
            self.mark_gone_by_id(request_id);
        }
        self.resolve_request_with(request_id, by, reason)
    }

    /// Resolves an unresolved request without a reason. See
    /// `resolve_request_with`.
    pub fn resolve_request(&mut self, request_id: &str, by: ResolvedBy) -> Vec<Change> {
        self.resolve_request_with(request_id, by, None)
    }

    /// Resolves an unresolved request. `by` is taken as reported: the adapter
    /// decides between `Inbox` and `Terminal`. An `Inbox` resolution of a
    /// claimed request carries the claimed option. `reason` refines `by`.
    pub fn resolve_request_with(
        &mut self,
        request_id: &str,
        by: ResolvedBy,
        reason: Option<ResolveReason>,
    ) -> Vec<Change> {
        let Some(e) = self.requests.remove(request_id) else {
            return vec![];
        };
        let option_id = match (by, &e.claimed) {
            (ResolvedBy::Inbox, Some(Answer::Option(o))) => Some(o.clone()),
            _ => None,
        };
        for k in self.keys.values_mut() {
            if k.open_request.as_deref() == Some(request_id) {
                k.open_request = None;
            }
        }
        let mut out = vec![Change::RequestResolved {
            request_id: request_id.to_string(),
            agent_id: e.req.agent_id.clone(),
            by,
            option_id,
            reason,
        }];
        if let Some(a) = self.agents.get_mut(&e.req.agent_id)
            && a.pending.as_ref().map(|p| p.id.as_str()) == Some(request_id)
        {
            a.pending = None;
            out.push(Change::AgentUpdated(a.clone()));
        }
        out
    }

    fn mark_gone_by_id(&mut self, request_id: &str) {
        for k in self.keys.values_mut() {
            if k.last_request.as_deref() == Some(request_id) {
                k.confirmed_gone = true;
            }
        }
    }

    fn open_request_for(&self, agent_id: &str, key: &CorrelationKey) -> Option<String> {
        self.keys
            .get(&(agent_id.to_string(), key.clone()))
            .and_then(|k| k.open_request.clone())
    }

    /// The agent's open request whose primary key is `key`. An alias left
    /// behind by an upgrade is skipped.
    pub fn request_id_for(&self, agent_id: &str, key: &CorrelationKey) -> Option<String> {
        self.open_request_for(agent_id, key)
            .filter(|rid| self.requests.get(rid).is_some_and(|e| e.key == *key))
    }

    pub fn pending_key(&self, agent_id: &str) -> Option<(String, CorrelationKey, RequestKind)> {
        let p = self.agents.get(agent_id)?.pending.as_ref()?;
        let e = self.requests.get(&p.id)?;
        Some((p.id.clone(), e.key.clone(), e.req.kind))
    }

    fn migrate(&mut self, from: &str, to: &str) -> Vec<Change> {
        if from == to || !self.agents.contains_key(from) || !self.agents.contains_key(to) {
            return vec![];
        }
        let mut out = Vec::new();
        let moving = self.agents.get(from).and_then(|a| a.pending.clone());
        if let Some(p) = moving {
            match self.agents.get(to).and_then(|a| a.pending.clone()) {
                Some(q)
                    if q.kind == RequestKind::BlockedUnparsed
                        && q.status == RequestStatus::Open =>
                {
                    out.extend(
                        self.resolve_request(&q.id, ResolvedBy::Superseded)
                            .into_iter()
                            .filter(|c| !matches!(c, Change::AgentUpdated(_))),
                    );
                }
                Some(_) => {
                    let by = if p.status == RequestStatus::Answering {
                        ResolvedBy::Uncertain
                    } else {
                        ResolvedBy::Lost
                    };
                    out.extend(
                        self.resolve_request(&p.id, by)
                            .into_iter()
                            .filter(|c| !matches!(c, Change::AgentUpdated(_))),
                    );
                    out.extend(self.agent_gone(from));
                    return out;
                }
                None => {}
            }
            if let Some(e) = self.requests.get_mut(&p.id) {
                e.req.agent_id = to.to_string();
            }
            let moved: Vec<(String, CorrelationKey)> = self
                .keys
                .keys()
                .filter(|(a, _)| a == from)
                .cloned()
                .collect();
            for (a, k) in moved {
                let mut st = self.keys.remove(&(a, k.clone())).expect("listed above");
                st.agent_id = to.to_string();
                // A leftover state on the target (an earlier, resolved
                // occurrence) must not hide the live request or a newer
                // tombstone: the source's whole state wins when it holds an
                // open request or has a higher generation.
                match self.keys.entry((to.to_string(), k)) {
                    Entry::Vacant(v) => {
                        v.insert(st);
                    }
                    Entry::Occupied(mut o) => {
                        let generation = o.get().generation.max(st.generation);
                        if st.open_request.is_some() || st.generation > o.get().generation {
                            o.insert(st);
                        }
                        o.get_mut().generation = generation;
                    }
                }
            }
            let req = self
                .requests
                .get(&p.id)
                .map(|e| e.req.clone())
                .expect("moved request");
            self.seen.remove(to);
            let a = self.agents.get_mut(to).expect("checked above");
            a.pending = Some(req);
            if a.state != AgentState::NeedsYou {
                a.state = AgentState::NeedsYou;
                a.state_since = Utc::now();
            }
            out.push(Change::AgentUpdated(a.clone()));
        }
        // The request moved: nothing is resolved here.
        self.agents.remove(from);
        self.keys.retain(|_, k| k.agent_id != from);
        self.seen.remove(from);
        out.push(Change::AgentRemoved(from.to_string()));
        out
    }

    fn upsert_agent(&mut self, mut o: AgentObs) -> Vec<Change> {
        let now = Utc::now();
        // `Unknown` is a source outage, not a state change: the seen marker
        // survives it.
        if o.state != AgentState::Done && o.state != AgentState::Unknown {
            self.seen.remove(&o.id);
        } else if o.state == AgentState::Done && self.seen.contains(&o.id) {
            o.state = AgentState::Idle;
        }
        let (pending, state_since) = match self.agents.get(&o.id) {
            Some(a) => (
                a.pending.clone(),
                if a.state == o.state {
                    a.state_since
                } else {
                    now
                },
            ),
            None => (None, now),
        };
        let agent = Agent {
            id: o.id.clone(),
            machine_id: self.machine_id.clone(),
            source: o.source,
            kind: o.kind,
            title: o.title,
            cwd: o.cwd,
            project: o.project,
            state: o.state,
            state_since,
            failed: o.failed,
            last_message: o.last_message,
            pending,
            can_send: o.can_send,
        };
        if self.agents.get(&o.id) == Some(&agent) {
            return vec![];
        }
        self.agents.insert(o.id, agent.clone());
        vec![Change::AgentUpdated(agent)]
    }

    fn agent_gone(&mut self, agent_id: &str) -> Vec<Change> {
        let Some(agent) = self.agents.get(agent_id) else {
            return vec![];
        };
        let mut out = Vec::new();
        if let Some(p) = agent.pending.clone() {
            let by = if p.status == RequestStatus::Answering {
                ResolvedBy::Uncertain
            } else {
                ResolvedBy::Lost
            };
            out.extend(
                self.resolve_request(&p.id, by)
                    .into_iter()
                    .filter(|c| !matches!(c, Change::AgentUpdated(_))),
            );
        }
        self.agents.remove(agent_id);
        self.keys.retain(|_, k| k.agent_id != agent_id);
        self.seen.remove(agent_id);
        out.push(Change::AgentRemoved(agent_id.to_string()));
        out
    }

    fn request_seen(&mut self, o: RequestObs) -> Vec<Change> {
        let Some(agent) = self.agents.get(&o.agent_id) else {
            return vec![];
        };
        let pending = agent.pending.clone();

        // 1. Same key with an open request: update it in place.
        if let Some(rid) = self.open_request_for(&o.agent_id, &o.key) {
            // An alias left by an upgrade must not overwrite structured content.
            if self.requests.get(&rid).is_some_and(|e| e.key != o.key) {
                return vec![];
            }
            return self.update_request(&rid, &o);
        }
        // 2. Replay of a resolved occurrence: its dialog was never confirmed
        //    gone, or its key is unique per occurrence and so never reopens.
        if let Some(k) = self.keys.get(&(o.agent_id.clone(), o.key.clone()))
            && k.generation > 0
            && (!k.confirmed_gone || !k.reusable)
        {
            return vec![];
        }
        // 3. Upgrade a fallback request in place.
        if let (Some(p), Some(from)) = (&pending, &o.upgrade_from)
            && p.status == RequestStatus::Open
            && self.open_request_for(&o.agent_id, from).as_deref() == Some(p.id.as_str())
        {
            let rid = p.id.clone();
            // Keep `from` aliased to the request so replays of it are ignored.
            self.claim_key(&o.key, &o.agent_id, &rid, o.reusable_key);
            if let Some(e) = self.requests.get_mut(&rid) {
                e.key = o.key.clone();
            }
            return self.update_request(&rid, &o);
        }
        let mut out = Vec::new();
        // 4. A different request on the same agent supersedes the pending one.
        if let Some(p) = pending {
            let by = if p.status == RequestStatus::Answering {
                ResolvedBy::Uncertain
            } else {
                ResolvedBy::Superseded
            };
            out.extend(self.resolve_request(&p.id, by));
        }
        // 5. Open a new occurrence. A new request means the agent's next
        //    `done` is a new completion, so the seen marker no longer applies.
        self.seen.remove(&o.agent_id);
        self.next_id += 1;
        let rid = format!("r{}", self.next_id);
        let req = Request {
            id: rid.clone(),
            revision: 1,
            agent_id: o.agent_id.clone(),
            kind: o.kind,
            status: RequestStatus::Open,
            answer_path: o.answer_path,
            title: o.title.clone(),
            detail: o.detail.clone(),
            options: o.options.clone(),
            questions: o.questions.clone(),
            free_text: o.free_text,
            raw_screen: o.raw_screen.clone(),
            opened_at: Utc::now(),
        };
        self.claim_key(&o.key, &o.agent_id, &rid, o.reusable_key);
        self.requests.insert(
            rid.clone(),
            ReqEntry {
                req: req.clone(),
                key: o.key,
                ready: o.ready,
                claimed: None,
            },
        );
        let a = self
            .agents
            .get_mut(&o.agent_id)
            .expect("agent checked above");
        if a.state != AgentState::NeedsYou {
            a.state = AgentState::NeedsYou;
            a.state_since = Utc::now();
        }
        a.pending = Some(req.clone());
        out.push(Change::RequestOpened(req));
        out.push(Change::AgentUpdated(a.clone()));
        out
    }

    fn claim_key(&mut self, key: &CorrelationKey, agent_id: &str, rid: &str, reusable: bool) {
        let k = self
            .keys
            .entry((agent_id.to_string(), key.clone()))
            .or_default();
        k.agent_id = agent_id.to_string();
        k.reusable = reusable;
        k.generation += 1;
        k.open_request = Some(rid.to_string());
        k.last_request = Some(rid.to_string());
        k.confirmed_gone = false;
    }

    fn update_request(&mut self, rid: &str, o: &RequestObs) -> Vec<Change> {
        let Some(e) = self.requests.get_mut(rid) else {
            return vec![];
        };
        // Content is frozen while a claimed answer is in flight, so what the
        // user approved cannot change before delivery.
        if e.req.status == RequestStatus::Answering {
            e.ready = o.ready;
            return vec![];
        }
        let before = e.req.clone();
        let semantic_changed = e.req.kind != o.kind
            || e.req.answer_path != o.answer_path
            || e.req.title != o.title
            || e.req.detail != o.detail
            || e.req.options != o.options
            || e.req.questions != o.questions
            || e.req.free_text != o.free_text;
        if semantic_changed {
            e.req.revision = e
                .req
                .revision
                .checked_add(1)
                .expect("request revision overflow");
        }
        e.req.kind = o.kind;
        if e.req.status == RequestStatus::Open {
            e.req.answer_path = o.answer_path;
        }
        e.req.title = o.title.clone();
        e.req.detail = o.detail.clone();
        e.req.options = o.options.clone();
        e.req.questions = o.questions.clone();
        e.req.free_text = o.free_text;
        e.req.raw_screen = o.raw_screen.clone();
        e.ready = o.ready;
        if e.req == before {
            return vec![];
        }
        self.sync_pending(rid).into_iter().collect()
    }

    /// Copies the current request into its agent's `pending` field.
    pub(crate) fn sync_pending(&mut self, rid: &str) -> Option<Change> {
        let req = self.requests.get(rid)?.req.clone();
        let a = self.agents.get_mut(&req.agent_id)?;
        a.pending = Some(req);
        Some(Change::AgentUpdated(a.clone()))
    }
}

/// Control characters other than newline and tab are never forwarded to an agent.
pub fn has_forbidden_control(text: &str) -> bool {
    text.chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
}

impl Registry {
    pub fn claim(
        &mut self,
        request_id: &str,
        answer: Answer,
        revision: Option<u64>,
    ) -> Result<(Request, Vec<Change>), ErrorCode> {
        let e = self
            .requests
            .get_mut(request_id)
            .ok_or(ErrorCode::RequestGone)?;
        match e.req.status {
            RequestStatus::Answering => return Err(ErrorCode::RequestBusy),
            RequestStatus::Resolved => return Err(ErrorCode::RequestGone),
            RequestStatus::Open => {}
            RequestStatus::Unknown => return Err(ErrorCode::NotAnswerable),
        }
        if !e.req.is_answerable() {
            return Err(ErrorCode::NotAnswerable);
        }
        if !e.ready {
            return Err(ErrorCode::RequestNotReady);
        }
        if revision.is_some_and(|revision| revision != e.req.revision) {
            return Err(ErrorCode::RequestChanged);
        }
        match &answer {
            Answer::Option(id) => {
                let valid = e
                    .req
                    .options
                    .iter()
                    .any(|o| &o.id == id && o.effect != Effect::Unknown);
                if !valid {
                    return Err(ErrorCode::BadRequest);
                }
            }
            Answer::Text(t) => {
                if !e.req.free_text || t.trim().is_empty() || has_forbidden_control(t) {
                    return Err(ErrorCode::BadRequest);
                }
            }
        }
        e.req.status = RequestStatus::Answering;
        e.claimed = Some(answer);
        let req = e.req.clone();
        Ok((req, self.sync_pending(request_id).into_iter().collect()))
    }

    pub fn release(&mut self, request_id: &str) -> Vec<Change> {
        let Some(e) = self.requests.get_mut(request_id) else {
            return vec![];
        };
        if e.req.status != RequestStatus::Answering {
            return vec![];
        }
        e.req.status = RequestStatus::Open;
        e.claimed = None;
        self.sync_pending(request_id).into_iter().collect()
    }

    pub fn check_send(&self, agent_id: &str, text: &str) -> Result<Agent, ErrorCode> {
        let a = self.agents.get(agent_id).ok_or(ErrorCode::AgentGone)?;
        if !a.can_send {
            return Err(ErrorCode::NotSupported);
        }
        if text.trim().is_empty() || has_forbidden_control(text) {
            return Err(ErrorCode::BadRequest);
        }
        if a.pending.is_some() || a.state == AgentState::NeedsYou {
            return Err(ErrorCode::AgentBusy);
        }
        Ok(a.clone())
    }

    pub fn mark_seen(&mut self, agent_id: &str) -> Result<Vec<Change>, ErrorCode> {
        let a = self.agents.get_mut(agent_id).ok_or(ErrorCode::AgentGone)?;
        if a.state != AgentState::Done {
            return Ok(vec![]);
        }
        a.state = AgentState::Idle;
        a.state_since = Utc::now();
        self.seen.insert(agent_id.to_string());
        Ok(vec![Change::AgentUpdated(a.clone())])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_of_unknown_status_is_not_answerable_and_remains_unclaimed() {
        let mut registry = Registry::new("machine".into());
        registry.requests.insert(
            "request".into(),
            ReqEntry {
                req: Request {
                    id: "request".into(),
                    revision: 1,
                    agent_id: "agent".into(),
                    kind: RequestKind::Permission,
                    status: RequestStatus::Unknown,
                    answer_path: AnswerPath::Hook,
                    title: "title".into(),
                    detail: "detail".into(),
                    options: vec![],
                    questions: None,
                    free_text: false,
                    raw_screen: None,
                    opened_at: Utc::now(),
                },
                key: CorrelationKey("key".into()),
                ready: true,
                claimed: None,
            },
        );

        assert_eq!(
            registry.claim("request", Answer::Option("allow".into()), None),
            Err(ErrorCode::NotAnswerable)
        );
        assert_eq!(
            registry.request("request").unwrap().status,
            RequestStatus::Unknown
        );
        assert!(registry.requests["request"].claimed.is_none());
    }

    #[test]
    #[should_panic(expected = "request revision overflow")]
    fn semantic_revision_overflow_panics_instead_of_wrapping() {
        let mut registry = Registry::new("machine".into());
        registry.apply(Observation::AgentUpsert(AgentObs {
            id: "agent".into(),
            source: Source::Herdr,
            kind: AgentKind::Claude,
            title: "agent".into(),
            cwd: None,
            project: None,
            state: AgentState::Working,
            failed: false,
            last_message: None,
            can_send: true,
        }));
        let mut request = RequestObs {
            agent_id: "agent".into(),
            key: CorrelationKey("key".into()),
            upgrade_from: None,
            kind: RequestKind::Permission,
            answer_path: AnswerPath::Hook,
            title: "title".into(),
            detail: "first".into(),
            options: vec![],
            questions: None,
            free_text: false,
            raw_screen: None,
            ready: true,
            reusable_key: true,
        };
        registry.apply(Observation::RequestSeen(request.clone()));
        let id = registry.agents()[0].pending.as_ref().unwrap().id.clone();
        registry.requests.get_mut(&id).unwrap().req.revision = u64::MAX;
        request.detail = "second".into();
        registry.apply(Observation::RequestSeen(request));
    }
}
