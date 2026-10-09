//! The hooks reconciler: the only code that changes hook-derived state.
//! It drains the endpoint mailbox and its internal channel; it never awaits a herdr call itself.
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use acd_core::{AgentObs, Core, CorrelationKey, Observation};
use acd_protocol::{
    AgentKind, AgentState, AnswerPath, Effect, RequestKind, RequestStatus, ResolvedBy, Source,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot, watch};
use tokio::time::Instant;

use super::HooksConfig;
use super::claude_sessions::{CompleteScan, ProcCheck, Scan, Scanner, map_status};
use super::dialogs::{self, DialogMatch};
use super::endpoint::{HoldId, HoldPermit, Inbound, Mailbox};
use super::evidence::{self, Expect, Verdict};
use super::mapping;
use super::owner::{Owner, PaneFact, claude_owner, header_pane, pane_facts};
use super::tickets::{Now, ReadKind, ReadSlots, Ticket, still_valid};
use super::wire::{
    AlwaysRule, Hints, HookAgent, HookEvent, HookName, Payload, always_rule, claude_key,
    command_of, tool_digest,
};
use crate::herdr::HerdrAdapter;
use crate::herdr::client::CALL_TIMEOUT;
use crate::herdr::keys::KeysTarget;
use crate::herdr::reconciler::PaneRef;
use crate::t3::sessions::SessionMap;
use crate::text::{LAST_MESSAGE_CHARS, TITLE_CHARS, cap_chars, clean_display};

#[derive(Default)]
pub(crate) struct Shared {
    #[allow(dead_code)] // Populated and consumed by the permission hold path in Task 14.
    pub held: Mutex<HashMap<String, HeldReq>>,
    pub last_scan: Mutex<Option<CompleteScan>>,
    pub by_generation: Mutex<BTreeMap<u64, HashSet<String>>>,
    /// Session ids not recorded under a generation because it was full.
    pub dropped_generation_ids: AtomicU64,
    occurrence_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    #[cfg(any(test, feature = "testing"))]
    pub gates: Gates,
}

#[cfg(any(test, feature = "testing"))]
#[derive(Default)]
pub struct Gates {
    pub deliver: Gate,
    pub retire: Gate,
}

#[cfg(any(test, feature = "testing"))]
#[derive(Default)]
pub struct Gate(Mutex<Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>>);

#[cfg(any(test, feature = "testing"))]
impl Gate {
    pub fn arm(&self) -> (oneshot::Receiver<()>, oneshot::Sender<()>) {
        let (reached_tx, reached_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        *self.0.lock().expect("gate") = Some((reached_tx, release_rx));
        (reached_rx, release_tx)
    }

    pub(crate) async fn pass(&self) {
        let armed = self.0.lock().expect("gate").take();
        if let Some((reached, release)) = armed {
            let _ = reached.send(());
            let _ = release.await;
        }
    }
}

impl Shared {
    pub(crate) async fn occurrence_lock(&self, agent: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let mutex = {
            let mut locks = self.occurrence_locks.lock().expect("occurrence locks");
            locks.retain(|_, lock| Arc::strong_count(lock) > 1);
            locks.entry(agent.to_owned()).or_default().clone()
        };
        mutex.lock_owned().await
    }
}

#[allow(dead_code)] // The Task 14 delivery path consumes these fields.
pub(crate) struct HeldReq {
    pub hold: HoldId,
    pub reply: Option<oneshot::Sender<String>>,
    pub rule: Option<AlwaysRule>,
    pub delivered: Option<(Effect, Instant)>,
    pub permit: Option<HoldPermit>,
    pub bound_captured: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Purpose {
    Capture,
    Presence,
    HoldEnded { expired: bool },
}

impl Purpose {
    pub(crate) fn kind(self) -> ReadKind {
        match self {
            Self::Capture => ReadKind::Capture,
            Self::Presence => ReadKind::Presence,
            Self::HoldEnded { .. } => ReadKind::HoldEnded,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ReadResult {
    Matched(String),
    NoMatch,
    Unavailable,
}

pub(crate) enum Internal {
    Scanned(Scan),
    Screen {
        ticket: Ticket,
        purpose: Purpose,
        result: ReadResult,
        permit: OwnedSemaphorePermit,
    },
}

/// The last valid header binding (G1): the pane the hook headers named,
/// kept while that pane's incarnation lives. The headers only arrive with
/// hooks, so scans and T3 changes re-evaluate with this.
struct HeaderBind {
    hints: Hints,
    agent_id: String,
    incarnation: u64,
}

/// Largest `cwd` kept per session.
const CWD_MAX_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Held,
    Delivered,
    HoldEnded { since: Instant },
    Keys,
    ReadOnly,
}

struct Track {
    agent_id: String,
    session_id: String,
    key: CorrelationKey,
    op: u64,
    reads: ReadSlots,
    wanted_purpose: HashMap<ReadKind, Purpose>,
    hold_end_tries: u8,
    payload: Payload,
    expect: Expect,
    matcher: Option<DialogMatch>,
    pane: Option<PaneFact>,
    phase: Phase,
    opened_at: Instant,
    capture: Option<String>,
    absent: u8,
    next_check: Instant,
    shadowed: bool,
    identity_seen: bool,
}

#[derive(Default)]
struct Unconsumed {
    entries: VecDeque<(Expect, Instant)>,
    consumed: VecDeque<(Expect, Instant)>,
    shadow_all: Option<Instant>,
}

const MAX_UNCONSUMED: usize = 8;

fn cap_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_owned()
}

struct Session {
    owner: Owner,
    binding: Option<HeaderBind>,
    bind_gen: u64,
    agent_id: Option<String>,
    hooks_only: bool,
    cwd: Option<String>,
    name: Option<String>,
    entrypoint: Option<String>,
    state: AgentState,
    last_message: Option<String>,
    last_hook: Option<Instant>,
    last_event: Instant,
    in_registry: bool,
    ended: Option<Instant>,
}

pub(crate) struct Reconciler {
    cfg: HooksConfig,
    core: Arc<Core>,
    herdr: Option<Arc<HerdrAdapter>>,
    t3: Option<Arc<SessionMap>>,
    shared: Arc<Shared>,
    mailbox: Mailbox,
    internal_tx: mpsc::Sender<Internal>,
    internal_rx: mpsc::Receiver<Internal>,
    reads: Arc<Semaphore>,
    scanner: Option<Arc<Mutex<Scanner>>>,
    scan_in_flight: Arc<AtomicBool>,
    scanning: bool,
    // TODO(A8, Task 19): bound this map to `max_sessions`.
    sessions: HashMap<String, Session>,
    tracks: HashMap<String, Track>,
    holds: HashMap<HoldId, String>,
    next_op: u64,
    unconsumed: HashMap<String, Unconsumed>,
}

impl Reconciler {
    pub(crate) fn new(
        cfg: HooksConfig,
        core: Arc<Core>,
        herdr: Option<Arc<HerdrAdapter>>,
        t3: Option<Arc<SessionMap>>,
        proc: Arc<dyn ProcCheck>,
        shared: Arc<Shared>,
        mailbox: Mailbox,
    ) -> Self {
        let (internal_tx, internal_rx) = mpsc::channel(cfg.max_reads + 2);
        let reads = Arc::new(Semaphore::new(cfg.max_reads));
        let scanner = Some(Arc::new(Mutex::new(Scanner::new(
            cfg.sessions_dirs.get(),
            proc,
        ))));
        Self {
            cfg,
            core,
            herdr,
            t3,
            shared,
            mailbox,
            internal_tx,
            internal_rx,
            reads,
            scanner,
            scan_in_flight: Arc::new(AtomicBool::new(false)),
            scanning: false,
            sessions: HashMap::new(),
            tracks: HashMap::new(),
            holds: HashMap::new(),
            next_op: 0,
            unconsumed: HashMap::new(),
        }
    }

    pub(crate) async fn run(mut self) {
        let mut tick = tokio::time::interval(self.cfg.tick);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut t3_changes = self.t3.as_ref().map(|m| m.changes());
        let mut next_scan = Instant::now();
        loop {
            tokio::select! {
                biased;
                Some(m) = self.mailbox.control.recv() => self.on_inbound(m).await,
                Some(m) = self.internal_rx.recv() => self.on_internal(m).await,
                _ = tick.tick() => {
                    let now = Instant::now();
                    if now >= next_scan { next_scan = now + self.cfg.scan_interval; self.spawn_scan(); }
                    self.on_tick().await;
                }
                _ = changed(&mut t3_changes) => self.reevaluate_all().await,
                m = self.mailbox.events.recv() => match m {
                    Some(m) => {
                        if let Inbound::Event { bytes, .. } = &m { self.mailbox.release(*bytes); }
                        self.on_inbound(m).await
                    }
                    None => return,
                },
            }
        }
    }

    fn spawn_scan(&mut self) {
        let Some(scanner) = self.scanner.clone() else {
            return;
        };
        let dirs = self.cfg.sessions_dirs.get();
        if dirs.is_empty() {
            return;
        }
        if self.scanning || self.scan_in_flight.swap(true, Ordering::AcqRel) {
            return;
        }
        self.scanning = true;
        let tx = self.internal_tx.clone();
        let in_flight = self.scan_in_flight.clone();
        tokio::task::spawn_blocking(move || {
            struct Done(Arc<AtomicBool>);
            impl Drop for Done {
                fn drop(&mut self) {
                    self.0.store(false, Ordering::Release);
                }
            }
            let _done = Done(in_flight);
            let mut scanner = scanner.lock().expect("scanner");
            scanner.set_dirs(dirs);
            let scan = scanner.scan();
            let _ = tx.blocking_send(Internal::Scanned(scan));
        });
    }

    fn panes(&self) -> Vec<PaneFact> {
        self.herdr
            .as_ref()
            .map(|h| pane_facts(&h.panes()))
            .unwrap_or_default()
    }

    /// The session's owner. `event` is the header hints of the hook being
    /// handled; a valid header binding is remembered on the session and
    /// reused by every later re-evaluation until its pane's incarnation
    /// changes or the pane disappears.
    fn owner_of(
        &mut self,
        session_id: &str,
        event: Option<&Hints>,
        entrypoint: Option<&str>,
    ) -> Owner {
        let panes = self.panes();
        let fresh = event.and_then(|h| {
            header_pane(h, session_id, &panes).map(|p| HeaderBind {
                hints: h.clone(),
                agent_id: p.agent_id.clone(),
                incarnation: p.incarnation,
            })
        });
        let mut hints = event.cloned().unwrap_or_default();
        if let Some(session) = self.sessions.get_mut(session_id) {
            if fresh.is_some() {
                session.binding = fresh;
            } else if session.binding.as_ref().is_some_and(|b| {
                !panes
                    .iter()
                    .any(|p| p.agent_id == b.agent_id && p.incarnation == b.incarnation)
            }) {
                session.binding = None;
            }
            if let Some(b) = &session.binding {
                hints = b.hints.clone();
            }
        }
        let t3 = self
            .t3
            .as_deref()
            .map(|m| m as &dyn super::owner::SessionOwners);
        claude_owner(
            session_id,
            &hints,
            entrypoint,
            t3,
            &panes,
            &self.cfg.ignored_entrypoints,
        )
    }

    fn new_session(&mut self, sid: &str, state: AgentState, in_registry: bool, now: Instant) {
        // TODO(A8, Task 19): refuse or evict at `max_sessions`.
        self.sessions
            .entry(sid.to_owned())
            .or_insert_with(|| Session {
                owner: Owner::Unknown,
                binding: None,
                bind_gen: 0,
                agent_id: None,
                hooks_only: false,
                cwd: None,
                name: None,
                entrypoint: None,
                state,
                last_message: None,
                last_hook: None,
                last_event: now,
                in_registry,
                ended: None,
            });
    }

    async fn on_inbound(&mut self, message: Inbound) {
        match message {
            Inbound::Event {
                event, generation, ..
            } => self.on_event(event, generation, None).await,
            Inbound::Held {
                event,
                hold,
                reply,
                generation,
                permit,
            } => {
                self.on_event(event, generation, Some((hold, reply, permit)))
                    .await
            }
            Inbound::HoldClosed { hold } => self.on_hold_ended(hold, false).await,
            Inbound::HoldExpired { hold } => self.on_hold_ended(hold, true).await,
        }
    }

    async fn on_event(
        &mut self,
        event: HookEvent,
        generation: u64,
        hold: Option<super::endpoint::HoldIn>,
    ) {
        {
            let mut generations = self.shared.by_generation.lock().expect("shared");
            let ids = generations.entry(generation).or_default();
            if ids.contains(&event.payload.session_id) {
            } else if ids.len() < self.cfg.max_generation_ids {
                ids.insert(event.payload.session_id.clone());
            } else {
                self.shared
                    .dropped_generation_ids
                    .fetch_add(1, Ordering::Relaxed);
            }
            while generations.len() > 4 {
                let oldest = *generations.keys().next().expect("non-empty");
                generations.remove(&oldest);
            }
        }
        if event.payload.is_subagent() {
            return;
        }
        match event.agent {
            HookAgent::Claude => self.on_claude(event, hold).await,
            HookAgent::Codex => self.on_codex(event).await,
        }
    }

    async fn on_claude(&mut self, event: HookEvent, hold: Option<super::endpoint::HoldIn>) {
        let sid = event.payload.session_id.clone();
        let entrypoint = event.hints.entrypoint.clone();
        let now = Instant::now();
        self.new_session(&sid, AgentState::Idle, false, now);
        let owner = self.owner_of(&sid, Some(&event.hints), entrypoint.as_deref());
        let t3_owned = matches!(owner, Owner::T3(_));
        let session = self.sessions.get_mut(&sid).expect("inserted above");
        session.last_event = now;
        session.last_hook = Some(now);
        if let Some(cwd) = &event.payload.cwd {
            session.cwd = Some(cap_bytes(cwd, CWD_MAX_BYTES));
        }
        if entrypoint.is_some() {
            session.entrypoint = entrypoint;
        }
        if event.name == HookName::SessionEnd {
            session.ended = Some(now);
        } else {
            session.ended = None;
        }
        // A T3-owned session is shown by T3; hook-side state stays untouched.
        if !t3_owned {
            match event.name {
                HookName::UserPromptSubmit => session.state = AgentState::Working,
                HookName::Stop => {
                    session.state = AgentState::Done;
                    session.last_message = event
                        .payload
                        .last_assistant_message
                        .as_deref()
                        .map(|m| cap_chars(&clean_display(m, usize::MAX), LAST_MESSAGE_CHARS));
                }
                _ => {}
            }
        }
        self.apply_owner(&sid, owner).await;
        if !t3_owned {
            self.apply_evidence(&event).await;
            if event.name == HookName::PermissionRequest
                && event.payload.tool_name.as_deref() != Some("AskUserQuestion")
            {
                self.open_claude_permission(event, hold).await;
            }
        }
    }

    fn fallback_key(&self, agent_id: &str) -> Option<CorrelationKey> {
        self.core
            .pending_key(agent_id)
            .filter(|(_, _, kind)| *kind == RequestKind::BlockedUnparsed)
            .map(|(_, key, _)| key)
    }

    async fn observe_locked(&self, agent_id: &str, herdr_agent: bool, obs: Observation) {
        let locks = self
            .herdr
            .as_ref()
            .filter(|_| herdr_agent)
            .map(|h| h.agent_locks());
        let _guard = match &locks {
            Some(l) => Some(l.lock(agent_id).await),
            None => None,
        };
        self.core.observe(obs);
    }

    fn bound_pane(&self, sid: &str) -> Option<PaneFact> {
        let Owner::Herdr(agent) = &self.sessions.get(sid)?.owner else {
            return None;
        };
        self.panes().into_iter().find(|p| p.agent_id == *agent)
    }

    async fn open_claude_permission(
        &mut self,
        event: HookEvent,
        hold: Option<super::endpoint::HoldIn>,
    ) {
        let sid = event.payload.session_id.clone();
        let Some(agent_id) = self.sessions.get(&sid).and_then(|s| s.agent_id.clone()) else {
            return;
        };
        let pane = self.bound_pane(&sid);
        let Some(key) = claude_key(&event.payload) else {
            return;
        };
        self.end_session_occurrences(&sid).await;
        let mut obs = mapping::claude_permission(
            &agent_id,
            key.clone(),
            self.fallback_key(&agent_id),
            &event.payload,
        );
        if hold.is_none() {
            if pane.is_none() {
                return;
            }
            obs.answer_path = AnswerPath::Keys;
            obs.options = mapping::claude_keys_options();
            obs.ready = false;
        }
        self.observe_locked(&agent_id, pane.is_some(), Observation::RequestSeen(obs))
            .await;
        let Some(rid) = self.core.request_id_for(&agent_id, &key) else {
            return;
        };
        let tool_name = event.payload.tool_name.clone().unwrap_or_default();
        let matcher = command_of(&event.payload.tool_input)
            .filter(|c| tool_name == "Bash" && dialogs::single_row(c))
            .map(|c| DialogMatch::ClaudeBash {
                command: c.to_owned(),
            });
        let rule = always_rule(&event.payload)
            .filter(|r| mapping::rule_displayable(&mapping::rule_text(r)));
        let phase = match hold {
            Some((hold_id, reply, permit)) => {
                self.shared.held.lock().expect("held").insert(
                    rid.clone(),
                    HeldReq {
                        hold: hold_id,
                        reply: Some(reply),
                        rule,
                        delivered: None,
                        permit: Some(permit),
                        bound_captured: false,
                    },
                );
                self.holds.insert(hold_id, rid.clone());
                Phase::Held
            }
            None => Phase::ReadOnly,
        };
        let now = Instant::now();
        let has_pane = pane.is_some();
        self.next_op += 1;
        self.tracks.insert(
            rid.clone(),
            Track {
                agent_id,
                session_id: sid.clone(),
                key,
                op: self.next_op,
                reads: ReadSlots::default(),
                wanted_purpose: HashMap::new(),
                hold_end_tries: 0,
                expect: Expect::ClaudeTool {
                    session_id: sid,
                    digest: tool_digest(&tool_name, &event.payload.tool_input),
                    tool_name,
                },
                matcher,
                pane,
                phase,
                opened_at: now,
                payload: event.payload,
                capture: None,
                absent: 0,
                next_check: now + self.cfg.held_dialog_poll,
                shadowed: false,
                identity_seen: false,
            },
        );
        self.mark_shadow(&rid);
        if has_pane {
            self.spawn_read(&rid, Purpose::Capture);
        }
        self.sweep();
    }

    fn read_target(&self, rid: &str) -> Option<(PaneRef, DialogMatch)> {
        let herdr = self.herdr.clone()?;
        let track = self.tracks.get(rid)?;
        let pane = track.pane.as_ref()?;
        let matcher = track.matcher.clone()?;
        let pref = herdr
            .panes()
            .read()
            .expect("pane map")
            .get(&pane.agent_id)
            .cloned()?;
        (pref.incarnation == pane.incarnation).then_some((pref, matcher))
    }

    fn bind_gen_of(&self, sid: &str) -> Option<u64> {
        self.sessions.get(sid).map(|session| session.bind_gen)
    }

    fn ticket_for(&mut self, rid: &str, kind: ReadKind) -> Option<Ticket> {
        let track = self.tracks.get_mut(rid)?;
        let read_seq = track.reads.admit(kind, true)?;
        let ticket = Ticket {
            agent_id: track.agent_id.clone(),
            incarnation: track.pane.as_ref()?.incarnation,
            session_id: track.session_id.clone(),
            bind_gen: 0,
            request_id: Some(rid.to_owned()),
            op: track.op,
            kind,
            read_seq,
        };
        let bind_gen = self.bind_gen_of(&ticket.session_id)?;
        Some(Ticket { bind_gen, ..ticket })
    }

    fn spawn_read(&mut self, rid: &str, purpose: Purpose) {
        let kind = purpose.kind();
        let Some((pref, matcher)) = self.read_target(rid) else {
            return;
        };
        let Ok(permit) = self.reads.clone().try_acquire_owned() else {
            if let Some(track) = self.tracks.get_mut(rid) {
                track.reads.admit(kind, false);
                track.wanted_purpose.insert(kind, purpose);
            }
            return;
        };
        let Some(ticket) = self.ticket_for(rid, kind) else {
            return;
        };
        if let Some(track) = self.tracks.get_mut(rid) {
            track.wanted_purpose.remove(&kind);
        }
        let tx = self.internal_tx.clone();
        let window = self.cfg.capture_window;
        let poll = self.cfg.capture_poll;
        let task = tokio::spawn(async move {
            let deadline = Instant::now() + window;
            loop {
                let read = pref
                    .herdr
                    .read_screen(
                        &pref.pane_id,
                        "detection",
                        None,
                        Instant::now() + CALL_TIMEOUT,
                    )
                    .await
                    .ok()
                    .filter(|result| !result.truncated);
                let result = match read {
                    None => ReadResult::Unavailable,
                    Some(screen) if matcher.find(&screen.text).is_some() => {
                        ReadResult::Matched(screen.text)
                    }
                    Some(_) => ReadResult::NoMatch,
                };
                let retry = purpose == Purpose::Capture
                    && !matches!(result, ReadResult::Matched(_))
                    && Instant::now() < deadline;
                if !retry {
                    let _ = tx
                        .send(Internal::Screen {
                            ticket,
                            purpose,
                            result,
                            permit,
                        })
                        .await;
                    return;
                }
                tokio::time::sleep(poll).await;
            }
        });
        if let Some(track) = self.tracks.get_mut(rid) {
            track.reads.started(kind, task.abort_handle());
        }
    }

    fn retry_wanted(&mut self) {
        let mut todo = Vec::new();
        for (rid, track) in &self.tracks {
            for kind in track.reads.wanted() {
                if let Some(purpose) = track.wanted_purpose.get(&kind) {
                    todo.push((rid.clone(), *purpose));
                }
            }
        }
        for (rid, purpose) in todo.into_iter().take(self.cfg.batch) {
            if self.reads.available_permits() == 0 {
                break;
            }
            if let Some(track) = self.tracks.get_mut(&rid) {
                track.reads.unwant(purpose.kind());
            }
            self.spawn_read(&rid, purpose);
        }
    }

    fn now_for(&self, ticket: &Ticket) -> Now {
        let incarnation = self.herdr.as_ref().and_then(|herdr| {
            herdr
                .panes()
                .read()
                .expect("pane map")
                .get(&ticket.agent_id)
                .map(|pane| pane.incarnation)
        });
        let track = ticket
            .request_id
            .as_ref()
            .and_then(|rid| self.tracks.get(rid))
            .map(|track| (track.op, track.reads.latest(ticket.kind)));
        let request = ticket
            .request_id
            .as_ref()
            .and_then(|rid| self.core.request(rid))
            .map(|request| (request.status, request.answer_path));
        Now {
            incarnation,
            bind_gen: self.bind_gen_of(&ticket.session_id),
            t3_owned: self
                .t3
                .as_ref()
                .is_some_and(|map| map.owner(&ticket.session_id).is_some()),
            track,
            request,
        }
    }

    fn mark_shadow(&mut self, rid: &str) {
        let shadowed = self.tracks.get(rid).is_some_and(|t| {
            self.unconsumed.get(&t.session_id).is_some_and(|u| {
                u.shadow_all.is_some()
                    || u.entries
                        .iter()
                        .chain(u.consumed.iter())
                        .any(|(e, _)| evidence::same_identity(e, &t.expect))
            })
        });
        if let Some(t) = self.tracks.get_mut(rid) {
            t.shadowed = shadowed;
        }
    }

    fn note_unconsumed(&mut self, t: &Track) {
        let u = self.unconsumed.entry(t.session_id.clone()).or_default();
        u.entries.push_back((t.expect.clone(), Instant::now()));
        Self::cap_identities(u);
    }

    fn note_consumed(&mut self, t: &Track) {
        let u = self.unconsumed.entry(t.session_id.clone()).or_default();
        u.consumed.push_back((t.expect.clone(), Instant::now()));
        Self::cap_identities(u);
    }

    fn cap_identities(u: &mut Unconsumed) {
        while u.entries.len() + u.consumed.len() > MAX_UNCONSUMED {
            match (u.entries.front(), u.consumed.front()) {
                (Some((_, entry_at)), Some((_, consumed_at))) if entry_at <= consumed_at => {
                    u.entries.pop_front();
                }
                (Some(_), Some(_)) | (None, Some(_)) => {
                    u.consumed.pop_front();
                }
                (Some(_), None) => {
                    u.entries.pop_front();
                }
                (None, None) => break,
            }
            u.shadow_all = Some(Instant::now());
        }
    }

    fn delivered(&self, rid: &str) -> Option<(Effect, Instant)> {
        self.shared
            .held
            .lock()
            .expect("held")
            .get(rid)
            .and_then(|h| h.delivered)
    }

    fn phase(&self, rid: &str) -> Option<Phase> {
        let t = self.tracks.get(rid)?;
        Some(match t.phase {
            Phase::Held if self.delivered(rid).is_some() => Phase::Delivered,
            p => p,
        })
    }

    async fn end_session_occurrences(&mut self, sid: &str) {
        let ids: Vec<String> = self
            .tracks
            .iter()
            .filter(|(_, t)| t.session_id == sid)
            .map(|(id, _)| id.clone())
            .collect();
        for rid in ids {
            let Some(agent) = self.tracks.get(&rid).map(|t| t.agent_id.clone()) else {
                continue;
            };
            let _guard = self.shared.occurrence_lock(&agent).await;
            let Some(track) = self.tracks.get(&rid) else {
                continue;
            };
            if track.session_id != sid {
                continue;
            }
            let by = if self.phase(&rid) == Some(Phase::Delivered) {
                ResolvedBy::Uncertain
            } else {
                ResolvedBy::Terminal
            };
            if let Some(h) = &self.herdr {
                h.keys_targets().remove(&rid);
            }
            #[cfg(any(test, feature = "testing"))]
            self.shared.gates.retire.pass().await;
            self.end(&rid, by);
        }
    }

    fn end(&mut self, rid: &str, by: ResolvedBy) {
        self.core.observe(Observation::RequestEndedById {
            request_id: rid.to_owned(),
            by,
        });
        self.forget(rid);
    }

    fn forget(&mut self, rid: &str) {
        if let Some(h) = self.shared.held.lock().expect("held").remove(rid) {
            self.holds.remove(&h.hold);
        }
        self.core.observe(Observation::DialogGoneById {
            request_id: rid.to_owned(),
        });
        if let Some(t) = self.tracks.remove(rid) {
            if t.identity_seen {
                self.note_consumed(&t);
            } else {
                self.note_unconsumed(&t);
            }
        }
    }

    fn sweep(&mut self) {
        let dead: Vec<String> = self
            .tracks
            .keys()
            .filter(|rid| self.core.request(rid).is_none())
            .cloned()
            .collect();
        for rid in dead {
            self.forget(&rid);
        }
    }

    async fn apply_evidence(&mut self, ev: &HookEvent) {
        let sid = ev.payload.session_id.clone();
        let mut evidence_eligible = true;
        let ids: Vec<String> = self
            .tracks
            .iter()
            .filter(|(_, t)| t.session_id == sid)
            .map(|(id, _)| id.clone())
            .collect();
        for rid in &ids {
            let Some(agent) = self.tracks.get(rid).map(|t| t.agent_id.clone()) else {
                continue;
            };
            let _guard = self.shared.occurrence_lock(&agent).await;
            let Some((opened_at, phase)) = self.tracks.get(rid).map(|t| (t.opened_at, t.phase))
            else {
                continue;
            };
            if phase == Phase::Keys {
                continue;
            }
            if ev.at <= opened_at {
                evidence_eligible = false;
                continue;
            }
            let delivered_at = self.delivered(rid).map(|(_, at)| at);
            if delivered_at.is_some_and(|at| ev.at <= at) {
                evidence_eligible = false;
                continue;
            }
            let Some(seen) = self
                .tracks
                .get(rid)
                .and_then(|t| evidence::observe_shadowed(&t.expect, ev, t.shadowed))
            else {
                continue;
            };
            if evidence::is_identity(&seen)
                && let Some(t) = self.tracks.get_mut(rid)
            {
                t.identity_seen = true;
            }
            let delivered = self.delivered(rid);
            let by = match delivered {
                Some(_) if self.tracks.get(rid).is_some_and(|t| t.shadowed) => {
                    ResolvedBy::Uncertain
                }
                Some((effect, _)) => {
                    match evidence::sent_for_effect(effect).map(|s| evidence::verdict(&s, &seen)) {
                        Some(Verdict::Consistent) => ResolvedBy::Inbox,
                        _ => ResolvedBy::Terminal,
                    }
                }
                None => ResolvedBy::Terminal,
            };
            self.end(rid, by);
        }
        if evidence_eligible && matches!(ev.name, HookName::Stop | HookName::UserPromptSubmit) {
            self.unconsumed.remove(&sid);
        } else if evidence_eligible
            && let Some(u) = self.unconsumed.get_mut(&sid)
            && let Some(i) = u.entries.iter().position(|(e, sent_at)| {
                ev.at > *sent_at
                    && evidence::observe(e, ev).is_some_and(|s| evidence::is_identity(&s))
            })
        {
            u.entries.remove(i);
        }
        if self.unconsumed.get(&sid).is_some_and(|u| {
            u.entries.is_empty() && u.consumed.is_empty() && u.shadow_all.is_none()
        }) {
            self.unconsumed.remove(&sid);
        }
    }

    async fn on_codex(&mut self, _event: HookEvent) {}

    async fn on_internal(&mut self, message: Internal) {
        match message {
            Internal::Scanned(scan) => {
                self.scanning = false;
                self.on_scan(scan).await;
            }
            Internal::Screen {
                ticket,
                purpose,
                result,
                permit,
            } => {
                self.on_screen(ticket, purpose, result).await;
                drop(permit);
            }
        }
    }

    async fn on_screen(&mut self, ticket: Ticket, purpose: Purpose, result: ReadResult) {
        if self.herdr.is_none() {
            return;
        }
        let Ok(_guard) = tokio::time::timeout(
            self.cfg.lock_wait,
            self.shared.occurrence_lock(&ticket.agent_id),
        )
        .await
        else {
            if let Some(rid) = &ticket.request_id
                && let Some(track) = self.tracks.get_mut(rid)
            {
                track.reads.finished(ticket.kind, ticket.read_seq);
                track.reads.admit(ticket.kind, false);
                track.wanted_purpose.insert(ticket.kind, purpose);
            }
            return;
        };
        if let Some(rid) = &ticket.request_id
            && let Some(track) = self.tracks.get_mut(rid)
        {
            track.reads.finished(ticket.kind, ticket.read_seq);
        }
        let paths: &[AnswerPath] = match purpose {
            Purpose::Capture => &[AnswerPath::Hook, AnswerPath::Keys, AnswerPath::None],
            _ => &[AnswerPath::Hook, AnswerPath::None],
        };
        if !still_valid(&ticket, &self.now_for(&ticket), paths) {
            return;
        }
        let Some(rid) = ticket.request_id.clone() else {
            return;
        };
        match (purpose, result) {
            (Purpose::Capture, ReadResult::Matched(screen)) => {
                if let Some(held) = self.shared.held.lock().expect("held").get_mut(&rid) {
                    held.bound_captured = true;
                }
                let unheld = self
                    .tracks
                    .get(&rid)
                    .is_some_and(|track| track.phase == Phase::ReadOnly);
                if let Some(track) = self.tracks.get_mut(&rid) {
                    track.capture = Some(screen.clone());
                }
                if unheld {
                    self.switch_to_keys_locked(&rid, screen, ticket.incarnation);
                }
            }
            (Purpose::Capture, _) => {}
            (Purpose::Presence, ReadResult::Unavailable) => {}
            (Purpose::Presence, result) => {
                let gone = if let Some(track) = self.tracks.get_mut(&rid) {
                    track.absent = if matches!(result, ReadResult::Matched(_)) {
                        0
                    } else {
                        track.absent.saturating_add(1)
                    };
                    track.absent >= 2
                } else {
                    false
                };
                if gone && self.delivered(&rid).is_none() {
                    self.end(&rid, ResolvedBy::Terminal);
                }
            }
            (Purpose::HoldEnded { .. }, ReadResult::Matched(screen)) => {
                self.switch_to_keys_locked(&rid, screen, ticket.incarnation)
            }
            (Purpose::HoldEnded { .. }, ReadResult::NoMatch) => {
                self.end(&rid, ResolvedBy::Terminal)
            }
            (Purpose::HoldEnded { expired }, ReadResult::Unavailable) => {
                let tries = self.tracks.get_mut(&rid).map(|track| {
                    track.hold_end_tries += 1;
                    track.hold_end_tries
                });
                if tries.is_some_and(|count| count < 3) {
                    self.spawn_read(&rid, Purpose::HoldEnded { expired });
                } else {
                    self.end(&rid, ResolvedBy::Uncertain);
                }
            }
        }
    }

    async fn on_hold_ended(&mut self, hold: HoldId, expired: bool) {
        let Some(rid) = self.holds.get(&hold).cloned() else {
            return;
        };
        let Some(agent) = self.tracks.get(&rid).map(|track| track.agent_id.clone()) else {
            return;
        };
        let _guard = self.shared.occurrence_lock(&agent).await;
        if self.holds.remove(&hold).is_none() {
            return;
        }
        if let Some(held) = self.shared.held.lock().expect("held").get_mut(&rid) {
            held.reply = None;
            held.permit = None;
        }
        if self.delivered(&rid).is_some() {
            return;
        }
        let Some(track) = self.tracks.get_mut(&rid) else {
            return;
        };
        track.phase = Phase::HoldEnded {
            since: Instant::now(),
        };
        let captured = track.pane.is_some() && track.capture.is_some();
        let has_pane = track.pane.is_some();
        if captured {
            self.spawn_read(&rid, Purpose::HoldEnded { expired });
        } else if expired {
            self.end(&rid, ResolvedBy::Lost);
        } else if !has_pane {
            // A hooks-only close has no screen to inspect. Keep it open for
            // lifecycle evidence; the evidence window retires it uncertain.
        } else {
            self.end(&rid, ResolvedBy::Terminal);
        }
    }

    fn switch_to_keys_locked(&mut self, rid: &str, screen: String, incarnation: u64) {
        let Some(herdr) = self.herdr.clone() else {
            return;
        };
        let Some(track) = self.tracks.get_mut(rid) else {
            return;
        };
        let Some(dialog) = crate::herdr::screen::find_dialog(&screen) else {
            return;
        };
        let mut observation =
            mapping::claude_permission(&track.agent_id, track.key.clone(), None, &track.payload);
        observation.answer_path = AnswerPath::Keys;
        observation.options = mapping::claude_keys_options();
        observation.ready = false;
        self.core.observe(Observation::RequestSeen(observation));
        let target = KeysTarget::from_screen(
            rid.to_owned(),
            track.agent_id.clone(),
            track.key.clone(),
            &screen,
            mapping::claude_permission_keys(&dialog),
        )
        .map(|target| target.for_incarnation(incarnation));
        let open_on_keys = self.core.request(rid).is_some_and(|request| {
            request.status == RequestStatus::Open && request.answer_path == AnswerPath::Keys
        });
        match target {
            Ok(target) => {
                if open_on_keys
                    && self.core.agent(&track.agent_id).is_some()
                    && herdr.keys_targets().set(target).is_ok()
                {
                    self.core.observe(Observation::RequestReady {
                        agent_id: track.agent_id.clone(),
                        key: track.key.clone(),
                        ready: true,
                    });
                    track.phase = Phase::Keys;
                } else {
                    self.core.observe(Observation::RequestPath {
                        agent_id: track.agent_id.clone(),
                        key: track.key.clone(),
                        answer_path: AnswerPath::None,
                    });
                    track.phase = Phase::ReadOnly;
                }
            }
            _ => {
                self.core.observe(Observation::RequestPath {
                    agent_id: track.agent_id.clone(),
                    key: track.key.clone(),
                    answer_path: AnswerPath::None,
                });
                track.phase = Phase::ReadOnly;
            }
        }
    }

    async fn apply_owner(&mut self, sid: &str, owner: Owner) {
        let hooks_id = format!("claude:{sid}");
        let mut migrated_to = None;
        let Some(session) = self.sessions.get_mut(sid) else {
            return;
        };
        let was_hooks_only = session.hooks_only;
        if session.owner != owner {
            session.bind_gen += 1;
        }
        session.owner = owner.clone();
        match owner {
            Owner::T3(_) | Owner::Unknown => {
                session.agent_id = None;
                session.hooks_only = false;
                if was_hooks_only {
                    self.core.observe(Observation::AgentGone {
                        agent_id: hooks_id.clone(),
                    });
                }
            }
            Owner::Herdr(agent) => {
                session.agent_id = Some(agent.clone());
                session.hooks_only = false;
                if was_hooks_only {
                    let locks = self.herdr.as_ref().map(|h| h.agent_locks());
                    let _guard = match &locks {
                        Some(locks) => Some(locks.lock(&agent).await),
                        None => None,
                    };
                    // Checked under the delivery lock. Without the target in
                    // core the hooks-only agent stays, and a later event or
                    // tick retries (the owner is unchanged, so only
                    // `retry_migrations` revisits it).
                    if self.core.agent(&agent).is_some() {
                        self.core.observe(Observation::AgentMigrated {
                            from: hooks_id.clone(),
                            to: agent.clone(),
                        });
                        migrated_to = Some(agent.clone());
                    } else {
                        session.agent_id = Some(hooks_id.clone());
                        session.hooks_only = true;
                    }
                }
            }
            Owner::Hooks => {
                session.agent_id = Some(hooks_id.clone());
                session.hooks_only = true;
                self.core
                    .observe(Observation::AgentUpsert(hooks_only_obs(&hooks_id, session)));
            }
        }
        if let Some(agent) = migrated_to {
            let pane = self.panes().into_iter().find(|fact| fact.agent_id == agent);
            let moved: Vec<String> = self
                .tracks
                .iter()
                .filter(|(_, track)| track.session_id == sid && track.agent_id == hooks_id)
                .map(|(rid, _)| rid.clone())
                .collect();
            for rid in moved {
                if let Some(track) = self.tracks.get_mut(&rid) {
                    track.agent_id = agent.clone();
                    track.pane = pane.clone();
                }
                if pane.is_some() {
                    self.spawn_read(&rid, Purpose::Capture);
                }
            }
        }
        self.sweep();
    }

    async fn reevaluate_all(&mut self) {
        let ids: Vec<(String, Option<String>)> = self
            .sessions
            .iter()
            .map(|(id, s)| (id.clone(), s.entrypoint.clone()))
            .collect();
        for (sid, entrypoint) in ids {
            let owner = self.owner_of(&sid, None, entrypoint.as_deref());
            if self.sessions.get(&sid).is_some_and(|s| s.owner != owner) {
                self.apply_owner(&sid, owner).await;
            }
        }
    }

    /// Sessions bound to a herdr agent that core did not have yet when the
    /// binding was found still show their hooks-only agent; try again.
    async fn retry_migrations(&mut self) {
        let pending: Vec<(String, Owner)> = self
            .sessions
            .iter()
            .filter(|(_, s)| s.hooks_only && matches!(s.owner, Owner::Herdr(_)))
            .map(|(id, s)| (id.clone(), s.owner.clone()))
            .collect();
        for (sid, owner) in pending {
            self.apply_owner(&sid, owner).await;
        }
    }

    async fn on_scan(&mut self, scan: Scan) {
        let now = Instant::now();
        let live: HashSet<String> = scan
            .live
            .iter()
            .map(|entry| entry.session_id.clone())
            .collect();
        for entry in &scan.live {
            self.new_session(&entry.session_id, AgentState::Unknown, true, now);
            let session = self
                .sessions
                .get_mut(&entry.session_id)
                .expect("inserted above");
            session.in_registry = true;
            session.last_event = now;
            if session.cwd.is_none() {
                session.cwd = entry.cwd.as_deref().map(|c| cap_bytes(c, CWD_MAX_BYTES));
            }
            if scan.complete {
                session.ended = None;
            }
            session.name = entry.name.clone().or(session.name.take());
            if session.entrypoint.is_none() {
                session.entrypoint = entry.entrypoint.clone();
            }
            let fresh_hook = session
                .last_hook
                .is_some_and(|t| now.duration_since(t) < self.cfg.scan_interval);
            let scanned = map_status(entry.status.as_deref());
            if !fresh_hook && !(session.state == AgentState::Done && scanned == AgentState::Idle) {
                session.state = scanned;
            }
        }
        if scan.complete {
            for (sid, session) in &mut self.sessions {
                if session.in_registry && !live.contains(sid) && session.ended.is_none() {
                    session.ended = Some(now);
                }
            }
            *self.shared.last_scan.lock().expect("shared") = Some(CompleteScan {
                started_at_ms: scan.started_at_ms,
                finished_at_ms: scan.finished_at_ms,
                live: scan.live.clone(),
            });
        }
        for entry in &scan.live {
            let owner = self.owner_of(&entry.session_id, None, entry.entrypoint.as_deref());
            self.apply_owner(&entry.session_id, owner).await;
        }
    }

    async fn on_tick(&mut self) {
        self.retry_migrations().await;
        let now = Instant::now();
        self.sweep();
        let window = self.cfg.evidence_window;
        let mut ends = Vec::new();
        ends.extend(self.tracks.keys().cloned());
        for rid in ends {
            let Some(agent) = self.tracks.get(&rid).map(|track| track.agent_id.clone()) else {
                continue;
            };
            let _guard = self.shared.occurrence_lock(&agent).await;
            let now = Instant::now();
            let Some(track) = self.tracks.get(&rid) else {
                continue;
            };
            let delivered = self.delivered(&rid);
            let retirement = match (track.phase, delivered) {
                (Phase::Keys, _) => None,
                (_, Some((_, at))) if now.duration_since(at) >= window => {
                    Some(ResolvedBy::Uncertain)
                }
                (Phase::HoldEnded { since }, None) if now.duration_since(since) >= window => {
                    Some(ResolvedBy::Uncertain)
                }
                _ => None,
            };
            if let Some(by) = retirement {
                #[cfg(any(test, feature = "testing"))]
                self.shared.gates.retire.pass().await;
                self.end(&rid, by);
            }
        }
        let ids: Vec<String> = self.tracks.keys().cloned().collect();
        for rid in ids {
            let Some(agent) = self.tracks.get(&rid).map(|track| track.agent_id.clone()) else {
                continue;
            };
            let _guard = self.shared.occurrence_lock(&agent).await;
            let should_read = self.tracks.get(&rid).is_some_and(|track| {
                matches!(track.phase, Phase::Held | Phase::ReadOnly)
                    && track.capture.is_some()
                    && now >= track.next_check
                    && self.delivered(&rid).is_none()
            });
            if should_read {
                if let Some(track) = self.tracks.get_mut(&rid) {
                    track.next_check = now + self.cfg.held_dialog_poll;
                }
                self.spawn_read(&rid, Purpose::Presence);
            }
        }
        let ttl = self.cfg.unconsumed_ttl;
        self.unconsumed.retain(|_, u| {
            u.entries.retain(|(_, at)| now.duration_since(*at) < ttl);
            u.consumed.retain(|(_, at)| now.duration_since(*at) < ttl);
            if u.shadow_all.is_some_and(|at| now.duration_since(at) >= ttl) {
                u.shadow_all = None;
            }
            !u.entries.is_empty() || !u.consumed.is_empty() || u.shadow_all.is_some()
        });
        let gone: Vec<String> = self
            .sessions
            .iter()
            .filter(|(_, s)| {
                s.ended
                    .is_some_and(|t| now.duration_since(t) >= self.cfg.end_grace)
                    || now.duration_since(s.last_event) >= self.cfg.idle_expiry
            })
            .map(|(id, _)| id.clone())
            .collect();
        for sid in gone {
            if let Some(session) = self.sessions.remove(&sid)
                && session.hooks_only
            {
                self.core.observe(Observation::AgentGone {
                    agent_id: format!("claude:{sid}"),
                });
            }
        }
        self.retry_wanted();
    }
}

fn hooks_only_obs(id: &str, session: &Session) -> AgentObs {
    let project = session
        .cwd
        .as_deref()
        .and_then(|cwd| Path::new(cwd).file_name())
        .map(|name| clean_display(&name.to_string_lossy(), TITLE_CHARS))
        .filter(|p| !p.is_empty());
    let title = session
        .name
        .as_deref()
        .map(|name| clean_display(name, TITLE_CHARS))
        .filter(|t| !t.is_empty())
        .or_else(|| project.clone())
        .unwrap_or_else(|| "Claude".into());
    AgentObs {
        id: id.into(),
        source: Source::Hooks,
        kind: AgentKind::Claude,
        title,
        cwd: session
            .cwd
            .as_deref()
            .map(|cwd| clean_display(cwd, usize::MAX)),
        project,
        state: session.state,
        failed: false,
        last_message: session.last_message.clone(),
        can_send: false,
    }
}

async fn changed(rx: &mut Option<watch::Receiver<u64>>) {
    if let Some(r) = rx
        && r.changed().await.is_ok()
    {
        return;
    }
    // No watcher, or its sender is closed: never fire again.
    std::future::pending::<()>().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::claude_sessions::{ProcVerdict, RegistryEntry};
    use crate::hooks::endpoint::{self, EndpointOptions};
    use crate::hooks::wire::Payload;
    use acd_core::CorrelationKey;
    use acd_core::testing::{permission_obs, test_machine};

    struct Live;
    impl ProcCheck for Live {
        fn check(&self, _: i32, _: &str) -> ProcVerdict {
            ProcVerdict::Live
        }
    }

    fn reconciler(max_generation_ids: usize) -> Reconciler {
        let mut cfg = HooksConfig::new(
            std::time::Duration::from_secs(3600),
            vec![],
            super::super::SessionRoots::none(),
        );
        cfg.max_generation_ids = max_generation_ids;
        let (_tx, mailbox) = endpoint::mailbox(&EndpointOptions::default());
        Reconciler::new(
            cfg,
            Core::new(test_machine(), vec![]),
            None,
            None,
            Arc::new(Live),
            Arc::default(),
            mailbox,
        )
    }

    fn hook(sid: &str) -> HookEvent {
        HookEvent {
            agent: HookAgent::Claude,
            name: HookName::UserPromptSubmit,
            payload: Payload {
                session_id: sid.into(),
                ..Default::default()
            },
            hints: Hints::default(),
            at: Instant::now(),
        }
    }

    fn track(sid: &str, rid: &str, opened_at: Instant) -> Track {
        let payload = Payload {
            session_id: sid.into(),
            tool_name: Some("Bash".into()),
            tool_input: serde_json::json!({"command":"echo hi"}),
            ..Default::default()
        };
        Track {
            agent_id: format!("claude:{sid}"),
            session_id: sid.into(),
            key: CorrelationKey(rid.into()),
            op: 1,
            reads: ReadSlots::default(),
            wanted_purpose: HashMap::new(),
            hold_end_tries: 0,
            payload: payload.clone(),
            expect: Expect::ClaudeTool {
                session_id: sid.into(),
                digest: tool_digest("Bash", &payload.tool_input),
                tool_name: "Bash".into(),
            },
            matcher: None,
            pane: None,
            phase: Phase::Held,
            opened_at,
            capture: None,
            absent: 0,
            next_check: opened_at,
            shadowed: false,
            identity_seen: false,
        }
    }

    fn herdr_obs(id: &str) -> AgentObs {
        AgentObs {
            id: id.into(),
            source: Source::Herdr,
            kind: AgentKind::Claude,
            title: "pane".into(),
            cwd: None,
            project: None,
            state: AgentState::Idle,
            failed: false,
            last_message: None,
            can_send: true,
        }
    }

    #[tokio::test]
    async fn pre_delivery_identity_does_not_mutate_evidence_bookkeeping() {
        let mut r = reconciler(8);
        let opened_at = Instant::now() - std::time::Duration::from_secs(2);
        let sent_at = Instant::now() - std::time::Duration::from_secs(1);
        let older = track("s1", "old", opened_at);
        r.unconsumed
            .entry("s1".into())
            .or_default()
            .entries
            .push_back((older.expect, sent_at));
        r.tracks.insert("r1".into(), track("s1", "r1", opened_at));
        r.shared.held.lock().unwrap().insert(
            "r1".into(),
            HeldReq {
                hold: 1,
                reply: None,
                rule: None,
                delivered: Some((Effect::Allow, sent_at)),
                permit: None,
                bound_captured: false,
            },
        );
        let mut ev = hook("s1");
        ev.name = HookName::PostToolUse;
        ev.payload.tool_name = Some("Bash".into());
        ev.payload.tool_input = serde_json::json!({"command":"echo hi"});
        ev.at = sent_at - std::time::Duration::from_millis(1);
        r.apply_evidence(&ev).await;
        assert!(!r.tracks["r1"].identity_seen);
        assert_eq!(r.unconsumed["s1"].entries.len(), 1);
    }

    #[tokio::test]
    async fn pre_open_event_does_not_remove_matching_shadow_history() {
        let mut r = reconciler(8);
        r.on_event(hook("s1"), 1, None).await;
        let now = Instant::now();
        let older = track("s1", "r1", now - std::time::Duration::from_secs(2));
        r.note_unconsumed(&older);
        let opened_at = now - std::time::Duration::from_secs(1);
        r.tracks.insert("r2".into(), track("s1", "r2", opened_at));

        let mut ev = hook("s1");
        ev.name = HookName::PostToolUse;
        ev.payload.tool_name = Some("Bash".into());
        ev.payload.tool_input = serde_json::json!({"command":"echo hi"});
        ev.at = opened_at - std::time::Duration::from_millis(1);
        r.apply_evidence(&ev).await;
        assert_eq!(r.unconsumed["s1"].entries.len(), 1);

        r.end("r2", ResolvedBy::Terminal);
        let mut later = ev;
        later.at = Instant::now();
        r.tracks
            .insert("r3".into(), track("s1", "r3", Instant::now()));
        r.mark_shadow("r3");
        assert!(r.tracks["r3"].shadowed);
    }

    #[tokio::test]
    async fn timeout_retirement_keeps_occurrence_lock_through_sender_removal() {
        use acd_protocol::Effect;

        let mut r = reconciler(8);
        r.on_event(hook("s1"), 1, None).await;
        let now = Instant::now();
        let mut t = track("s1", "r1", now - std::time::Duration::from_secs(10));
        t.phase = Phase::Held;
        r.tracks.insert("r1".into(), t);
        r.core
            .observe(Observation::RequestSeen(permission_obs("claude:s1", "r1")));
        let rid = r
            .core
            .request_id_for("claude:s1", &CorrelationKey("r1".into()))
            .unwrap();
        let track = r.tracks.remove("r1").unwrap();
        r.tracks.insert(rid.clone(), track);
        let (reply_tx, mut reply_rx) = oneshot::channel();
        r.shared.held.lock().unwrap().insert(
            rid,
            HeldReq {
                hold: 1,
                reply: Some(reply_tx),
                rule: None,
                delivered: Some((Effect::Allow, now - std::time::Duration::from_secs(10))),
                permit: None,
                bound_captured: false,
            },
        );
        r.cfg.evidence_window = std::time::Duration::from_millis(1);
        let (retire_reached, retire_release) = r.shared.gates.retire.arm();
        let shared = r.shared.clone();
        let retiring = tokio::spawn(async move {
            let mut r = r;
            r.on_tick().await;
            r
        });
        retire_reached.await.unwrap();
        let contender = tokio::spawn(async move { shared.occurrence_lock("claude:s1").await });
        tokio::task::yield_now().await;
        assert!(
            !contender.is_finished(),
            "timeout retains the lock until removal"
        );
        retire_release.send(()).unwrap();
        let _ = retiring.await.unwrap();
        drop(contender.await.unwrap());
        assert!(reply_rx.try_recv().is_err());
    }

    #[test]
    fn consumed_identity_tombstone_shadows_identical_successor() {
        let mut r = reconciler(8);
        let t1 = track("s1", "r1", Instant::now());
        r.note_consumed(&t1);
        r.tracks
            .insert("r2".into(), track("s1", "r2", Instant::now()));
        r.mark_shadow("r2");
        assert!(r.tracks["r2"].shadowed);
        assert_eq!(r.unconsumed["s1"].consumed.len(), 1);
    }

    fn scan(complete: bool, sessions: &[&str]) -> Scan {
        Scan {
            live: sessions
                .iter()
                .map(|s| RegistryEntry {
                    pid: 1,
                    session_id: (*s).into(),
                    cwd: None,
                    started_at_ms: None,
                    proc_start: None,
                    entrypoint: Some("cli".into()),
                    name: None,
                    status: Some("busy".into()),
                    waiting_for: None,
                })
                .collect(),
            complete,
            scanned_at: Instant::now(),
            started_at_ms: 10,
            finished_at_ms: 20,
        }
    }

    #[tokio::test]
    async fn a_binding_to_an_agent_core_lacks_keeps_the_hooks_agent_until_it_appears() {
        let mut r = reconciler(8);
        r.on_event(hook("s1"), 1, None).await;
        let hooks_id = "claude:s1".to_string();
        assert!(r.core.agent(&hooks_id).is_some());
        let key = CorrelationKey("k".into());
        r.core
            .observe(Observation::RequestSeen(permission_obs(&hooks_id, "k")));
        let rid = r.core.request_id_for(&hooks_id, &key).unwrap();

        r.apply_owner("s1", Owner::Herdr("herdr:abc:w1:p1".into()))
            .await;
        assert!(r.core.agent(&hooks_id).is_some(), "not orphaned");
        assert!(r.sessions["s1"].hooks_only);

        r.retry_migrations().await;
        assert!(
            r.core.agent(&hooks_id).is_some(),
            "still waiting for the target"
        );

        r.core
            .observe(Observation::AgentUpsert(herdr_obs("herdr:abc:w1:p1")));
        r.retry_migrations().await;
        assert!(r.core.agent(&hooks_id).is_none());
        assert!(!r.sessions["s1"].hooks_only);
        assert_eq!(r.core.request_id_for("herdr:abc:w1:p1", &key), Some(rid));
    }

    #[tokio::test]
    async fn bind_gen_changes_only_with_the_owner() {
        let mut r = reconciler(8);
        r.on_event(hook("s1"), 1, None).await;
        let g = r.sessions["s1"].bind_gen;
        assert_eq!(g, 1, "Unknown to Hooks");
        r.on_event(hook("s1"), 1, None).await;
        assert_eq!(r.sessions["s1"].bind_gen, g, "same owner");
        r.apply_owner("s1", Owner::T3("t3:x".into())).await;
        assert_eq!(r.sessions["s1"].bind_gen, g + 1);
        r.apply_owner("s1", Owner::T3("t3:x".into())).await;
        assert_eq!(r.sessions["s1"].bind_gen, g + 1);
        r.apply_owner("s1", Owner::Hooks).await;
        assert_eq!(r.sessions["s1"].bind_gen, g + 2);
    }

    #[tokio::test]
    async fn only_complete_scans_are_recorded() {
        let mut r = reconciler(8);
        r.on_scan(scan(false, &["a"])).await;
        assert!(r.shared.last_scan.lock().unwrap().is_none());
        r.on_scan(scan(true, &["a", "b"])).await;
        let last = r.shared.last_scan.lock().unwrap().clone().unwrap();
        assert_eq!(
            (last.started_at_ms, last.finished_at_ms, last.live.len()),
            (10, 20, 2)
        );
        r.on_scan(scan(false, &[])).await;
        assert_eq!(
            r.shared
                .last_scan
                .lock()
                .unwrap()
                .clone()
                .unwrap()
                .live
                .len(),
            2,
            "an incomplete scan leaves the last complete one"
        );
        assert!(r.sessions["a"].ended.is_none(), "and never ends a session");
    }

    #[tokio::test]
    async fn activity_and_complete_scans_clear_the_end_mark() {
        let mut r = reconciler(8);
        let mut end = hook("s1");
        end.name = HookName::SessionEnd;
        r.on_event(end.clone(), 1, None).await;
        assert!(r.sessions["s1"].ended.is_some());
        r.on_event(hook("s1"), 1, None).await;
        assert!(r.sessions["s1"].ended.is_none(), "any other hook");
        r.on_event(end, 1, None).await;
        r.on_scan(scan(false, &["s1"])).await;
        assert!(
            r.sessions["s1"].ended.is_some(),
            "an incomplete scan proves nothing"
        );
        r.on_scan(scan(true, &["s1"])).await;
        assert!(
            r.sessions["s1"].ended.is_none(),
            "a complete scan lists it live"
        );
    }

    #[tokio::test]
    async fn generations_keep_four_and_count_dropped_ids() {
        let mut r = reconciler(2);
        for (gen_, sid) in [(1, "a"), (1, "b"), (1, "c"), (1, "a")] {
            r.on_event(hook(sid), gen_, None).await;
        }
        let ids = r.shared.by_generation.lock().unwrap()[&1].clone();
        assert_eq!(ids.len(), 2);
        assert_eq!(r.shared.dropped_generation_ids.load(Ordering::Relaxed), 1);
        for g in 2..=6 {
            r.on_event(hook("a"), g, None).await;
        }
        let keys: Vec<u64> = r
            .shared
            .by_generation
            .lock()
            .unwrap()
            .keys()
            .copied()
            .collect();
        assert_eq!(keys, vec![3, 4, 5, 6]);
    }

    #[tokio::test]
    async fn t3_owned_sessions_keep_hook_state_untouched() {
        let mut r = reconciler(8);
        r.on_event(hook("s1"), 1, None).await;
        assert_eq!(r.sessions["s1"].state, AgentState::Working);
        r.apply_owner("s1", Owner::T3("t3:x".into())).await;
        let t3 = Arc::new(crate::t3::sessions::SessionMap::default());
        t3.set_for_tests("t3:x", Some("s1"));
        r.t3 = Some(t3);
        let mut stop = hook("s1");
        stop.name = HookName::Stop;
        stop.payload.last_assistant_message = Some("hello".into());
        r.on_event(stop, 1, None).await;
        assert_eq!(r.sessions["s1"].state, AgentState::Working);
        assert!(r.sessions["s1"].last_message.is_none());
    }

    #[test]
    fn cwd_is_capped_on_a_char_boundary() {
        let s = "é".repeat(3000);
        let c = cap_bytes(&s, CWD_MAX_BYTES);
        assert!(c.len() <= CWD_MAX_BYTES && c.len() >= CWD_MAX_BYTES - 1);
        assert_eq!(cap_bytes("abc", 10), "abc");
    }
}
