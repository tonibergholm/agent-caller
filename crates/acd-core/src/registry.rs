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
}

#[derive(Clone, Debug, PartialEq)]
pub enum Observation {
    AgentUpsert(AgentObs),
    AgentGone { agent_id: String },
    RequestSeen(RequestObs),
    RequestReady { key: CorrelationKey, ready: bool },
    RequestPath { key: CorrelationKey, answer_path: AnswerPath },
    RequestEnded { key: CorrelationKey, by: ResolvedBy },
    DialogGone { key: CorrelationKey },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    AgentUpdated(Agent),
    AgentRemoved(String),
    RequestOpened(Request),
    RequestResolved { request_id: String, agent_id: String, by: ResolvedBy, option_id: Option<String> },
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
        Self { machine_id, agents: BTreeMap::new(), requests: HashMap::new(), keys: HashMap::new(), seen: HashSet::new(), next_id: 0 }
    }

    pub fn agents(&self) -> Vec<Agent> { self.agents.values().cloned().collect() }
    pub fn agent(&self, id: &str) -> Option<&Agent> { self.agents.get(id) }
    pub fn request(&self, id: &str) -> Option<&Request> { self.requests.get(id).map(|e| &e.req) }
    pub fn is_ready(&self, id: &str) -> bool { self.requests.get(id).is_some_and(|e| e.ready) }

    pub fn apply(&mut self, obs: Observation) -> Vec<Change> {
        match obs {
            Observation::AgentUpsert(o) => self.upsert_agent(o),
            Observation::AgentGone { agent_id } => self.agent_gone(&agent_id),
            Observation::RequestSeen(o) => self.request_seen(o),
            Observation::RequestReady { key, ready } => {
                for rid in self.primary_requests_for(&key) {
                    if let Some(e) = self.requests.get_mut(&rid) { e.ready = ready; }
                }
                vec![]
            }
            Observation::RequestPath { key, answer_path } => {
                let mut out = Vec::new();
                for rid in self.primary_requests_for(&key) {
                    let Some(e) = self.requests.get_mut(&rid) else { continue };
                    if e.req.status != RequestStatus::Open || e.req.answer_path == answer_path { continue; }
                    e.req.answer_path = answer_path;
                    out.extend(self.sync_pending(&rid));
                }
                out
            }
            Observation::RequestEnded { key, by } => {
                if matches!(by, ResolvedBy::Inbox | ResolvedBy::Terminal | ResolvedBy::Agent) {
                    for ((_, k), st) in self.keys.iter_mut() {
                        if *k == key { st.confirmed_gone = true; }
                    }
                }
                let confirming = matches!(by, ResolvedBy::Inbox | ResolvedBy::Terminal | ResolvedBy::Agent);
                let rids = if confirming { self.any_requests_for(&key) } else { self.primary_requests_for(&key) };
                let mut out = Vec::new();
                for rid in rids {
                    out.extend(self.resolve_request(&rid, by));
                }
                out
            }
            Observation::DialogGone { key } => {
                for ((_, k), st) in self.keys.iter_mut() {
                    if *k == key { st.confirmed_gone = true; }
                }
                vec![]
            }
        }
    }

    /// Resolves an unresolved request. A `Terminal` resolution of a claimed
    /// request is attributed to the inbox, with the claimed option.
    pub fn resolve_request(&mut self, request_id: &str, by: ResolvedBy) -> Vec<Change> {
        let Some(e) = self.requests.remove(request_id) else { return vec![] };
        let by = if e.req.status == RequestStatus::Answering && by == ResolvedBy::Terminal { ResolvedBy::Inbox } else { by };
        let option_id = match (by, &e.claimed) {
            (ResolvedBy::Inbox, Some(Answer::Option(o))) => Some(o.clone()),
            _ => None,
        };
        for k in self.keys.values_mut() {
            if k.open_request.as_deref() == Some(request_id) { k.open_request = None; }
        }
        let mut out = vec![Change::RequestResolved {
            request_id: request_id.to_string(), agent_id: e.req.agent_id.clone(), by, option_id,
        }];
        if let Some(a) = self.agents.get_mut(&e.req.agent_id)
            && a.pending.as_ref().map(|p| p.id.as_str()) == Some(request_id) {
                a.pending = None;
                out.push(Change::AgentUpdated(a.clone()));
            }
        out
    }

    fn open_request_for(&self, agent_id: &str, key: &CorrelationKey) -> Option<String> {
        self.keys.get(&(agent_id.to_string(), key.clone())).and_then(|k| k.open_request.clone())
    }

    /// Open requests reached through `key`, primary or alias, across agents.
    fn any_requests_for(&self, key: &CorrelationKey) -> Vec<String> {
        let mut v: Vec<String> = self.keys.iter()
            .filter(|((_, k), _)| k == key)
            .filter_map(|(_, st)| st.open_request.clone())
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// Open requests whose primary key is `key`, across agents. Aliases
    /// left behind by an upgrade are skipped.
    fn primary_requests_for(&self, key: &CorrelationKey) -> Vec<String> {
        self.keys.iter()
            .filter(|((_, k), _)| k == key)
            .filter_map(|(_, st)| st.open_request.clone())
            .filter(|rid| self.requests.get(rid).is_some_and(|e| e.key == *key))
            .collect()
    }

    fn upsert_agent(&mut self, mut o: AgentObs) -> Vec<Change> {
        let now = Utc::now();
        if o.state != AgentState::Done {
            self.seen.remove(&o.id);
        } else if self.seen.contains(&o.id) {
            o.state = AgentState::Idle;
        }
        let (pending, state_since) = match self.agents.get(&o.id) {
            Some(a) => (a.pending.clone(), if a.state == o.state { a.state_since } else { now }),
            None => (None, now),
        };
        let agent = Agent {
            id: o.id.clone(), machine_id: self.machine_id.clone(), source: o.source, kind: o.kind,
            title: o.title, cwd: o.cwd, project: o.project, state: o.state, state_since,
            failed: o.failed, last_message: o.last_message, pending, can_send: o.can_send,
        };
        if self.agents.get(&o.id) == Some(&agent) { return vec![]; }
        self.agents.insert(o.id, agent.clone());
        vec![Change::AgentUpdated(agent)]
    }

    fn agent_gone(&mut self, agent_id: &str) -> Vec<Change> {
        let Some(agent) = self.agents.get(agent_id) else { return vec![] };
        let mut out = Vec::new();
        if let Some(p) = agent.pending.clone() {
            out.extend(self.resolve_request(&p.id, ResolvedBy::Lost).into_iter()
                .filter(|c| !matches!(c, Change::AgentUpdated(_))));
        }
        self.agents.remove(agent_id);
        self.keys.retain(|_, k| k.agent_id != agent_id);
        self.seen.remove(agent_id);
        out.push(Change::AgentRemoved(agent_id.to_string()));
        out
    }

    fn request_seen(&mut self, o: RequestObs) -> Vec<Change> {
        let Some(agent) = self.agents.get(&o.agent_id) else { return vec![] };
        let pending = agent.pending.clone();

        // 1. Same key with an open request: update it in place.
        if let Some(rid) = self.open_request_for(&o.agent_id, &o.key) {
            // An alias left by an upgrade must not overwrite structured content.
            if self.requests.get(&rid).is_some_and(|e| e.key != o.key) { return vec![]; }
            return self.update_request(&rid, &o);
        }
        // 2. Replay of a resolved occurrence whose dialog was never confirmed gone.
        if let Some(k) = self.keys.get(&(o.agent_id.clone(), o.key.clone()))
            && k.generation > 0 && !k.confirmed_gone { return vec![]; }
        // 3. Upgrade a fallback request in place.
        if let (Some(p), Some(from)) = (&pending, &o.upgrade_from)
            && p.status == RequestStatus::Open
                && self.open_request_for(&o.agent_id, from).as_deref() == Some(p.id.as_str())
            {
                let rid = p.id.clone();
                // Keep `from` aliased to the request so replays of it are ignored.
                self.claim_key(&o.key, &o.agent_id, &rid);
                if let Some(e) = self.requests.get_mut(&rid) { e.key = o.key.clone(); }
                return self.update_request(&rid, &o);
            }
        let mut out = Vec::new();
        // 4. A different request on the same agent supersedes the pending one.
        if let Some(p) = pending {
            out.extend(self.resolve_request(&p.id, ResolvedBy::Superseded));
        }
        // 5. Open a new occurrence.
        self.next_id += 1;
        let rid = format!("r{}", self.next_id);
        let req = Request {
            id: rid.clone(), agent_id: o.agent_id.clone(), kind: o.kind, status: RequestStatus::Open,
            answer_path: o.answer_path, title: o.title.clone(), detail: o.detail.clone(),
            options: o.options.clone(), questions: o.questions.clone(), free_text: o.free_text,
            raw_screen: o.raw_screen.clone(), opened_at: Utc::now(),
        };
        self.claim_key(&o.key, &o.agent_id, &rid);
        self.requests.insert(rid.clone(), ReqEntry { req: req.clone(), key: o.key, ready: o.ready, claimed: None });
        let a = self.agents.get_mut(&o.agent_id).expect("agent checked above");
        if a.state != AgentState::NeedsYou {
            a.state = AgentState::NeedsYou;
            a.state_since = Utc::now();
        }
        a.pending = Some(req.clone());
        out.push(Change::RequestOpened(req));
        out.push(Change::AgentUpdated(a.clone()));
        out
    }

    fn claim_key(&mut self, key: &CorrelationKey, agent_id: &str, rid: &str) {
        let k = self.keys.entry((agent_id.to_string(), key.clone())).or_default();
        k.agent_id = agent_id.to_string();
        k.generation += 1;
        k.open_request = Some(rid.to_string());
        k.confirmed_gone = false;
    }

    fn update_request(&mut self, rid: &str, o: &RequestObs) -> Vec<Change> {
        let Some(e) = self.requests.get_mut(rid) else { return vec![] };
        // Content is frozen while a claimed answer is in flight, so what the
        // user approved cannot change before delivery.
        if e.req.status == RequestStatus::Answering {
            e.ready = o.ready;
            return vec![];
        }
        let before = e.req.clone();
        e.req.kind = o.kind;
        if e.req.status == RequestStatus::Open { e.req.answer_path = o.answer_path; }
        e.req.title = o.title.clone();
        e.req.detail = o.detail.clone();
        e.req.options = o.options.clone();
        e.req.questions = o.questions.clone();
        e.req.free_text = o.free_text;
        e.req.raw_screen = o.raw_screen.clone();
        e.ready = o.ready;
        if e.req == before { return vec![]; }
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
    text.chars().any(|c| c.is_control() && c != '\n' && c != '\t')
}

impl Registry {
    pub fn claim(&mut self, request_id: &str, answer: Answer) -> Result<(Request, Vec<Change>), ErrorCode> {
        let e = self.requests.get_mut(request_id).ok_or(ErrorCode::RequestGone)?;
        match e.req.status {
            RequestStatus::Answering => return Err(ErrorCode::RequestBusy),
            RequestStatus::Resolved => return Err(ErrorCode::RequestGone),
            RequestStatus::Open => {}
        }
        if !e.req.is_answerable() { return Err(ErrorCode::NotAnswerable); }
        if !e.ready { return Err(ErrorCode::RequestNotReady); }
        match &answer {
            Answer::Option(id) => {
                let valid = e.req.options.iter().any(|o| &o.id == id && o.effect != Effect::Unknown);
                if !valid { return Err(ErrorCode::BadRequest); }
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
        let Some(e) = self.requests.get_mut(request_id) else { return vec![] };
        if e.req.status != RequestStatus::Answering { return vec![]; }
        e.req.status = RequestStatus::Open;
        e.claimed = None;
        self.sync_pending(request_id).into_iter().collect()
    }

    pub fn check_send(&self, agent_id: &str, text: &str) -> Result<Agent, ErrorCode> {
        let a = self.agents.get(agent_id).ok_or(ErrorCode::AgentGone)?;
        if !a.can_send { return Err(ErrorCode::NotSupported); }
        if text.trim().is_empty() || has_forbidden_control(text) { return Err(ErrorCode::BadRequest); }
        if a.pending.is_some() || a.state == AgentState::NeedsYou { return Err(ErrorCode::AgentBusy); }
        Ok(a.clone())
    }

    pub fn mark_seen(&mut self, agent_id: &str) -> Result<Vec<Change>, ErrorCode> {
        let a = self.agents.get_mut(agent_id).ok_or(ErrorCode::AgentGone)?;
        if a.state != AgentState::Done { return Ok(vec![]); }
        a.state = AgentState::Idle;
        a.state_since = Utc::now();
        self.seen.insert(agent_id.to_string());
        Ok(vec![Change::AgentUpdated(a.clone())])
    }
}
