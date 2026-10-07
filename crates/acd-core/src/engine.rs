use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use acd_protocol::*;
use sha2::{Digest, Sha256};
use tokio::sync::broadcast;

use crate::adapter::{Adapter, DeliverError};
use crate::idem::{Begin, IdemCache};
use crate::log::EventLog;
use crate::registry::{Answer, Change, Observation, Registry};

pub const RING_CAPACITY: usize = 1000;
pub const IDEM_TTL: Duration = Duration::from_secs(900);

struct Inner {
    machine: Machine,
    registry: Registry,
    log: EventLog,
    idem: IdemCache,
}

pub struct Core {
    inner: Mutex<Inner>,
    events: broadcast::Sender<ServerMessage>,
    adapters: Vec<Arc<dyn Adapter>>,
}

impl Core {
    pub fn new(machine: Machine, adapters: Vec<Arc<dyn Adapter>>) -> Arc<Core> {
        let epoch = uuid::Uuid::new_v4().simple().to_string();
        let (events, _) = broadcast::channel(RING_CAPACITY);
        Arc::new(Core {
            inner: Mutex::new(Inner {
                registry: Registry::new(machine.id.clone()),
                machine,
                log: EventLog::new(epoch, RING_CAPACITY),
                idem: IdemCache::new(IDEM_TTL),
            }),
            events,
            adapters,
        })
    }

    pub fn epoch(&self) -> String { self.inner.lock().unwrap().log.epoch().to_string() }

    pub fn observe(&self, obs: Observation) {
        let mut st = self.inner.lock().unwrap();
        let changes = st.registry.apply(obs);
        self.publish(&mut st, changes);
    }

    pub fn set_source_status(&self, source: Source, status: SourceStatus) {
        let mut st = self.inner.lock().unwrap();
        let slot = match source {
            Source::Hooks => &mut st.machine.sources.hooks,
            Source::Herdr => &mut st.machine.sources.herdr,
            Source::T3 => &mut st.machine.sources.t3,
        };
        if *slot == status { return; }
        *slot = status;
        let machine = st.machine.clone();
        self.publish(&mut st, vec![Change::MachineUpdated(machine)]);
    }

    pub fn welcome(&self) -> ServerMessage {
        let st = self.inner.lock().unwrap();
        ServerMessage::Welcome {
            machine: st.machine.clone(),
            capabilities: CAPABILITIES.iter().map(|c| c.to_string()).collect(),
            epoch: st.log.epoch().to_string(),
            seq: st.log.seq(),
        }
    }

    /// The initial messages for a new session, plus a live receiver. Both
    /// are taken under one lock, so every live event has a seq greater
    /// than the initial messages (spec §7.4).
    pub fn attach(&self, resume: Option<&Cursor>) -> (Vec<ServerMessage>, broadcast::Receiver<ServerMessage>) {
        let st = self.inner.lock().unwrap();
        let rx = self.events.subscribe();
        let initial = match resume.and_then(|c| st.log.replay_after(c)) {
            Some(events) => events,
            None => vec![ServerMessage::Snapshot {
                epoch: st.log.epoch().to_string(),
                seq: st.log.seq(),
                agents: st.registry.agents(),
            }],
        };
        (initial, rx)
    }

    pub async fn handle(&self, client_id: &str, msg: ClientMessage) -> CommandResult {
        if let ClientMessage::Hello { .. } | ClientMessage::Ping { .. } = msg {
            let id = msg.command_id().unwrap_or("");
            return CommandResult::err(id, ErrorCode::BadRequest, "not a command");
        }
        let Some(id) = msg.command_id().map(str::to_owned) else {
            return CommandResult::err("", ErrorCode::BadRequest, "not a command");
        };
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&Sha256::digest(serde_json::to_vec(&msg).expect("serializable")));
        let begin = self.inner.lock().unwrap().idem.begin(client_id, &id, hash, Instant::now());
        match begin {
            Begin::Done(result) => return result,
            Begin::Mismatch => return CommandResult::err(&id, ErrorCode::DuplicateMismatch, ErrorCode::DuplicateMismatch.describe()),
            Begin::Wait(mut rx) => loop {
                if let Some(result) = rx.borrow().clone() { return result; }
                if rx.changed().await.is_err() {
                    return CommandResult::err(&id, ErrorCode::SourceUnavailable, "command abandoned");
                }
            },
            Begin::Run => {}
        }
        let mut guard = AbandonGuard { core: self, client_id, id: &id, phase: Mutex::new(Phase::Start), armed: true };
        let result = self.execute(&id, msg, &guard.phase).await;
        guard.armed = false;
        let cacheable = !matches!(
            result.error_code(),
            Some(ErrorCode::RequestNotReady | ErrorCode::RequestBusy | ErrorCode::SourceUnavailable)
        );
        self.inner.lock().unwrap().idem.finish(client_id, &id, result.clone(), cacheable, Instant::now());
        result
    }

    async fn execute(&self, id: &str, msg: ClientMessage, phase: &Mutex<Phase>) -> CommandResult {
        match msg {
            ClientMessage::RequestAnswer { request_id, option_id, text, .. } => {
                let answer = match (option_id, text) {
                    (Some(o), None) => Answer::Option(o),
                    (None, Some(t)) => Answer::Text(t),
                    _ => return CommandResult::err(id, ErrorCode::BadRequest, "exactly one of option_id or text is required"),
                };
                let request = {
                    let mut st = self.inner.lock().unwrap();
                    match st.registry.claim(&request_id, answer.clone()) {
                        Ok((req, changes)) => {
                            *phase.lock().unwrap() = Phase::Claimed(request_id.clone());
                            self.publish(&mut st, changes);
                            req
                        }
                        Err(code) => return CommandResult::err(id, code, code.describe()),
                    }
                };
                let Some(adapter) = self.adapters.iter().find(|a| a.answer_paths().contains(&request.answer_path)).cloned() else {
                    self.release(&request_id);
                    return CommandResult::err(id, ErrorCode::SourceUnavailable, ErrorCode::SourceUnavailable.describe());
                };
                *phase.lock().unwrap() = Phase::Delivering(Some(request_id.clone()));
                match adapter.deliver(&request, &answer).await {
                    Ok(()) => CommandResult::delivered(id),
                    Err(DeliverError::NotDelivered(code)) => {
                        self.release(&request_id);
                        CommandResult::err(id, code, code.describe())
                    }
                    Err(DeliverError::Uncertain(why)) => {
                        tracing::warn!(request_id, adapter = adapter.name(), %why, "answer delivery uncertain");
                        let mut st = self.inner.lock().unwrap();
                        let changes = st.registry.resolve_request(&request_id, ResolvedBy::Uncertain);
                        self.publish(&mut st, changes);
                        CommandResult::delivered(id)
                    }
                }
            }
            ClientMessage::AgentSend { agent_id, text, .. } => {
                let agent = match self.inner.lock().unwrap().registry.check_send(&agent_id, &text) {
                    Ok(a) => a,
                    Err(code) => return CommandResult::err(id, code, code.describe()),
                };
                let Some(adapter) = self.adapter_for_source(agent.source) else {
                    return CommandResult::err(id, ErrorCode::SourceUnavailable, ErrorCode::SourceUnavailable.describe());
                };
                *phase.lock().unwrap() = Phase::Delivering(None);
                match adapter.send(&agent, &text).await {
                    Ok(()) => CommandResult::delivered(id),
                    Err(DeliverError::NotDelivered(code)) => CommandResult::err(id, code, code.describe()),
                    Err(DeliverError::Uncertain(why)) => {
                        tracing::warn!(agent_id, %why, "follow-up delivery uncertain");
                        CommandResult::delivered(id)
                    }
                }
            }
            ClientMessage::AgentMarkSeen { agent_id, .. } => {
                let agent = {
                    let mut st = self.inner.lock().unwrap();
                    match st.registry.mark_seen(&agent_id) {
                        Ok(changes) => self.publish(&mut st, changes),
                        Err(code) => return CommandResult::err(id, code, code.describe()),
                    }
                    st.registry.agent(&agent_id).cloned()
                };
                if let (Some(agent), Some(adapter)) = (agent.as_ref(), agent.as_ref().and_then(|a| self.adapter_for_source(a.source))) {
                    if let Err(e) = adapter.mark_seen(agent).await {
                        tracing::debug!(agent_id, error = %e, "adapter mark_seen failed; acd keeps its own flag");
                    }
                }
                CommandResult::ok(id)
            }
            ClientMessage::Hello { .. } | ClientMessage::Ping { .. } => {
                CommandResult::err(id, ErrorCode::BadRequest, "not a command")
            }
        }
    }

    fn adapter_for_source(&self, source: Source) -> Option<Arc<dyn Adapter>> {
        self.adapters.iter().find(|a| a.sources().contains(&source)).cloned()
    }

    fn release(&self, request_id: &str) {
        let mut st = self.inner.lock().unwrap();
        let changes = st.registry.release(request_id);
        self.publish(&mut st, changes);
    }

    fn publish(&self, st: &mut Inner, changes: Vec<Change>) {
        for change in changes {
            let msg = st.log.record(change);
            let _ = self.events.send(msg);
        }
    }
}

/// How far a command got. Decides what an abandoned command may do (spec §5.1:
/// a request returns to open only if nothing reached the agent).
enum Phase {
    Start,
    /// Claimed, adapter not yet called.
    Claimed(String),
    /// Adapter call in flight; the request id is set for `request.answer`.
    Delivering(Option<String>),
}

/// Runs if `execute` never completes (panic, or the `handle` future dropped).
/// Before delivery it releases the claim and finishes the id as an uncached
/// error. During delivery something may have reached the agent, so it resolves
/// the request `uncertain` and caches `delivered`, so a retry never re-sends.
struct AbandonGuard<'a> {
    core: &'a Core,
    client_id: &'a str,
    id: &'a str,
    phase: Mutex<Phase>,
    armed: bool,
}

impl Drop for AbandonGuard<'_> {
    fn drop(&mut self) {
        if !self.armed { return; }
        let phase = match self.phase.get_mut() {
            Ok(p) => std::mem::replace(p, Phase::Start),
            Err(e) => std::mem::replace(e.into_inner(), Phase::Start),
        };
        let mut st = match self.core.inner.lock() {
            Ok(st) => st,
            Err(e) => e.into_inner(),
        };
        let (result, cacheable) = match phase {
            Phase::Start => (CommandResult::err(self.id, ErrorCode::SourceUnavailable, "command abandoned"), false),
            Phase::Claimed(rid) => {
                let changes = st.registry.release(&rid);
                self.core.publish(&mut st, changes);
                (CommandResult::err(self.id, ErrorCode::SourceUnavailable, "command abandoned"), false)
            }
            Phase::Delivering(rid) => {
                if let Some(rid) = rid {
                    let changes = st.registry.resolve_request(&rid, ResolvedBy::Uncertain);
                    self.core.publish(&mut st, changes);
                }
                (CommandResult::delivered(self.id), true)
            }
        };
        st.idem.finish(self.client_id, self.id, result, cacheable, Instant::now());
    }
}
