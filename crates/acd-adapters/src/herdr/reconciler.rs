//! Per-socket reconciler (spec §6.4 Sync): keeps the registry in step with
//! one herdr socket. One task makes every `agent.list` / `agent.get` call,
//! one at a time, so results apply in the order they were issued.
//!
//! Its screen poll also runs the keys target watch (spec §5.1): every
//! registered keys target on this socket's panes is observed until its
//! dialog is confirmed gone, sharing one detection read per pane per poll.
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use acd_core::{AgentObs, Core, CorrelationKey, Observation, RequestObs};
use acd_protocol::{AgentKind, AgentState, AnswerPath, RequestKind, RequestStatus, ResolvedBy};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep, sleep_until};

use super::client::{CALL_TIMEOUT, CallError, Herdr, ScreenRead, Subscription};
use super::fallback::{self, Fallback, FbOut};
use super::keys::{ABSENT_READS, DialogIdentity, KeysTargets};
use super::map;
use super::screen::find_dialog;
use super::wire::{AgentInfo, Event};

const HINT_CAPACITY: usize = 256;
/// Hint messages moved into the dirty queue per turn.
const DRAIN_PER_TURN: usize = 64;
/// Distinct dirty panes before the queue collapses into one full list.
const MAX_DIRTY: usize = 1024;
const BACKOFF_START: Duration = Duration::from_millis(500);

#[derive(Clone, Debug)]
pub struct Timing {
    pub list_idle: Duration,
    pub list_blocked: Duration,
    pub backoff_max: Duration,
    pub fallback_after: Duration,
    pub poll: Duration,
}

impl Default for Timing {
    fn default() -> Timing {
        Timing {
            list_idle: Duration::from_secs(30),
            list_blocked: Duration::from_secs(5),
            backoff_max: Duration::from_secs(30),
            fallback_after: Duration::from_secs(2),
            poll: Duration::from_secs(1),
        }
    }
}

#[derive(Clone)]
pub struct PaneRef {
    pub herdr: Herdr,
    pub pane_id: String,
    pub kind: AgentKind,
    pub hints: mpsc::Sender<Hint>,
}

/// `agent_id -> PaneRef`. The reconciler inserts and removes; adapters read.
pub type PaneMap = RwLock<HashMap<String, PaneRef>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hint {
    /// Fetch this pane afresh with `agent.get`.
    Pane(String),
    /// The pane or its agent went away; `agent.get` confirms.
    Closed(String),
    /// Something unexpected happened: run a full `agent.list`.
    Resync,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Health {
    Connecting,
    Connected,
    Down(String),
    Incompatible(String),
}

pub struct ReconcilerHandle {
    hints: mpsc::Sender<Hint>,
    health: watch::Receiver<Health>,
    stop: oneshot::Sender<()>,
    task: JoinHandle<HashMap<String, AgentKind>>,
    core: Arc<Core>,
    panes: Arc<PaneMap>,
    keys: Arc<KeysTargets>,
    hash: String,
}

impl ReconcilerHandle {
    pub fn health(&self) -> watch::Receiver<Health> {
        self.health.clone()
    }

    pub fn hints(&self) -> mpsc::Sender<Hint> {
        self.hints.clone()
    }

    /// Stops the task. With `remove_agents`, also emits `AgentGone` for every
    /// agent of this socket and drops them from the pane map.
    pub async fn shutdown(self, remove_agents: bool) {
        let _ = self.stop.send(());
        let known = match self.task.await {
            Ok(known) => Some(known),
            Err(e) => {
                tracing::error!(error = %e, "herdr reconciler task failed");
                None
            }
        };
        if !remove_agents {
            return;
        }
        // Never hold the pane map across a core call.
        let ids: Vec<String> = {
            let mut panes = self.panes.write().expect("pane map");
            match known {
                Some(known) => known
                    .keys()
                    .map(|p| map::agent_id(&self.hash, p))
                    .inspect(|id| {
                        panes.remove(id);
                    })
                    .collect(),
                None => {
                    // The task is gone: fall back to this socket's id prefix.
                    let prefix = format!("herdr:{}:", self.hash);
                    let ids: Vec<String> = panes
                        .keys()
                        .filter(|id| id.starts_with(&prefix))
                        .cloned()
                        .collect();
                    for id in &ids {
                        panes.remove(id);
                    }
                    ids
                }
            }
        };
        for agent_id in ids {
            self.keys.remove_agent(&agent_id);
            self.core.observe(Observation::AgentGone { agent_id });
        }
    }
}

pub fn spawn(
    herdr: Herdr,
    core: Arc<Core>,
    panes: Arc<PaneMap>,
    timing: Timing,
    keys: Arc<KeysTargets>,
) -> ReconcilerHandle {
    let (hints_tx, hint_rx) = mpsc::channel(HINT_CAPACITY);
    let (down_tx, down_rx) = mpsc::unbounded_channel();
    let (health_tx, health_rx) = watch::channel(Health::Connecting);
    let (stop_tx, stop_rx) = oneshot::channel();
    let hash = map::socket_hash(herdr.path());
    let timing_after = timing.fallback_after;
    let reconciler = Reconciler {
        herdr,
        core: core.clone(),
        panes: panes.clone(),
        timing,
        hash: hash.clone(),
        hints_tx: hints_tx.clone(),
        hint_rx,
        down_tx,
        down_rx,
        stop_rx,
        health: health_tx,
        known: HashMap::new(),
        blocked: HashSet::new(),
        sub: None,
        subscribed: HashSet::new(),
        generation: 0,
        subs_changed: false,
        resync: Arc::new(AtomicBool::new(false)),
        dirty: VecDeque::new(),
        dirty_set: HashSet::new(),
        poll_queue: VecDeque::new(),
        next_poll: Instant::now(),
        fallback: Fallback::new(timing_after),
        fb_open: HashMap::new(),
        keys: keys.clone(),
        watch_absent: HashMap::new(),
        next_class: Class::Hint,
        last_list: Instant::now(),
    };
    ReconcilerHandle {
        hints: hints_tx,
        health: health_rx,
        stop: stop_tx,
        task: tokio::spawn(reconciler.run()),
        core,
        panes,
        keys,
        hash,
    }
}

/// How a phase of the task ended.
enum Flow {
    Stop,
    Lost(String),
}

#[derive(Clone, Copy)]
enum Class {
    Hint,
    List,
    Poll,
}

impl Class {
    fn next(self) -> Class {
        match self {
            Class::Hint => Class::List,
            Class::List => Class::Poll,
            Class::Poll => Class::Hint,
        }
    }
}

/// A subscription's reader task; dropping it aborts the reader, so a
/// replaced subscription never reports a loss.
struct SubReader(JoinHandle<()>);

impl Drop for SubReader {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct Reconciler {
    herdr: Herdr,
    core: Arc<Core>,
    panes: Arc<PaneMap>,
    timing: Timing,
    hash: String,
    hints_tx: mpsc::Sender<Hint>,
    hint_rx: mpsc::Receiver<Hint>,
    /// Subscription generation that ended.
    down_tx: mpsc::UnboundedSender<u64>,
    down_rx: mpsc::UnboundedReceiver<u64>,
    stop_rx: oneshot::Receiver<()>,
    health: watch::Sender<Health>,
    /// Panes holding a detected agent.
    known: HashMap<String, AgentKind>,
    /// Known panes whose agent is `NeedsYou`.
    blocked: HashSet<String>,
    sub: Option<SubReader>,
    /// Panes in the current subscription.
    subscribed: HashSet<String>,
    generation: u64,
    subs_changed: bool,
    /// Sticky: set by a full hint channel, an overflowing dirty queue or
    /// `Hint::Resync`; cleared when a full list starts.
    resync: Arc<AtomicBool>,
    dirty: VecDeque<String>,
    dirty_set: HashSet<String>,
    /// Round-robin of due screen reads, filled from `fallback.due` every poll.
    poll_queue: VecDeque<String>,
    next_poll: Instant,
    fallback: Fallback,
    /// Pane -> (key, request id) of the fallback request this task opened.
    fb_open: HashMap<String, (CorrelationKey, String)>,
    keys: Arc<KeysTargets>,
    /// Request id -> consecutive detection reads without its dialog.
    watch_absent: HashMap<String, u8>,
    next_class: Class,
    last_list: Instant,
}

fn is_server(e: &CallError) -> bool {
    matches!(e, CallError::Server { .. })
}

/// `>= 0.9.3, < 0.10`, protocol 22.
fn supported(version: &str, protocol: u32) -> bool {
    let parts: Option<Vec<u32>> = version.split('.').map(|p| p.parse().ok()).collect();
    matches!(parts.as_deref(), Some([0, 9, patch]) if *patch >= 3) && protocol == 22
}

impl Reconciler {
    async fn run(mut self) -> HashMap<String, AgentKind> {
        let mut backoff = BACKOFF_START;
        loop {
            let flow = match self.connect().await {
                Ok(()) => {
                    backoff = BACKOFF_START;
                    self.serve().await
                }
                Err(flow) => flow,
            };
            match flow {
                Flow::Stop => return self.known,
                Flow::Lost(reason) => {
                    self.lost(reason);
                    if self
                        .sleep_or_stop(backoff.min(self.timing.backoff_max))
                        .await
                    {
                        return self.known;
                    }
                    backoff = (backoff * 2).min(self.timing.backoff_max);
                }
            }
        }
    }

    /// Returns true if stop arrived first.
    async fn sleep_or_stop(&mut self, d: Duration) -> bool {
        tokio::select! {
            biased;
            _ = &mut self.stop_rx => true,
            _ = sleep(d) => false,
        }
    }

    fn set_health(&self, h: Health) {
        self.health.send_replace(h);
    }

    /// Ping, full list, subscribe. Retries a version mismatch every
    /// `backoff_max`.
    async fn connect(&mut self) -> Result<(), Flow> {
        loop {
            let pong = tokio::select! {
                biased;
                _ = &mut self.stop_rx => return Err(Flow::Stop),
                r = self.herdr.ping() => r,
            };
            match pong {
                Ok(p) if supported(&p.version, p.protocol) => break,
                Ok(p) => {
                    self.set_health(Health::Incompatible(format!(
                        "herdr {} protocol {} is not supported (need 0.9.x, protocol 22)",
                        p.version, p.protocol
                    )));
                    if self.sleep_or_stop(self.timing.backoff_max).await {
                        return Err(Flow::Stop);
                    }
                }
                Err(e) => return Err(Flow::Lost(e.to_string())),
            }
        }
        self.dirty.clear();
        self.dirty_set.clear();
        self.list_unit(false).await?;
        self.resubscribe(true).await?;
        self.set_health(Health::Connected);
        Ok(())
    }

    /// Connection loss (spec §12): every known agent becomes `unknown` until
    /// the next full resync restores it.
    fn lost(&mut self, reason: String) {
        self.sub = None;
        self.subscribed.clear();
        self.dirty.clear();
        self.dirty_set.clear();
        self.resync.store(false, Ordering::Relaxed);
        self.set_health(Health::Down(reason));
        for pane_id in self.known.keys() {
            let Some(a) = self.core.agent(&map::agent_id(&self.hash, pane_id)) else {
                continue;
            };
            self.core.observe(Observation::AgentUpsert(AgentObs {
                id: a.id,
                source: a.source,
                kind: a.kind,
                title: a.title,
                cwd: a.cwd,
                project: a.project,
                state: AgentState::Unknown,
                failed: a.failed,
                last_message: a.last_message,
                can_send: a.can_send,
            }));
        }
    }

    async fn serve(&mut self) -> Flow {
        loop {
            if let Some(flow) = self.control() {
                return flow;
            }
            self.drain_hints();
            self.enqueue_due();
            let result = match self.pick() {
                Some(Class::Hint) => self.hint_unit().await,
                Some(Class::List) => self.list_unit(true).await,
                Some(Class::Poll) => self.poll_unit().await,
                None => self.wait().await,
            };
            let result = match result {
                Ok(()) if self.subs_changed => self.resubscribe(false).await,
                other => other,
            };
            if let Err(flow) = result {
                return flow;
            }
        }
    }

    /// Non-blocking control check, run every turn: stop, then a loss of the
    /// current subscription. A stale generation is ignored.
    fn control(&mut self) -> Option<Flow> {
        match self.stop_rx.try_recv() {
            Err(oneshot::error::TryRecvError::Empty) => {}
            _ => return Some(Flow::Stop),
        }
        while let Ok(generation) = self.down_rx.try_recv() {
            if generation == self.generation {
                return Some(Flow::Lost("event stream closed".into()));
            }
        }
        None
    }

    fn drain_hints(&mut self) {
        for _ in 0..DRAIN_PER_TURN {
            let Ok(hint) = self.hint_rx.try_recv() else {
                break;
            };
            self.ingest(hint);
        }
    }

    fn ingest(&mut self, hint: Hint) {
        match hint {
            Hint::Pane(p) | Hint::Closed(p) => self.mark_dirty(p),
            Hint::Resync => self.resync.store(true, Ordering::Relaxed),
        }
    }

    fn mark_dirty(&mut self, pane_id: String) {
        if self.dirty_set.contains(&pane_id) {
            return;
        }
        if self.dirty_set.len() >= MAX_DIRTY {
            self.dirty.clear();
            self.dirty_set.clear();
            self.resync.store(true, Ordering::Relaxed);
            return;
        }
        self.dirty_set.insert(pane_id.clone());
        self.dirty.push_back(pane_id);
    }

    fn list_interval(&self) -> Duration {
        if self.blocked.is_empty() {
            self.timing.list_idle
        } else {
            self.timing.list_blocked
        }
    }

    fn list_due(&self) -> bool {
        self.resync.load(Ordering::Relaxed)
            || Instant::now() >= self.last_list + self.list_interval()
    }

    /// The next non-empty class in the fixed rotation hint, list, poll.
    fn pick(&mut self) -> Option<Class> {
        let mut class = self.next_class;
        for _ in 0..3 {
            let ready = match class {
                Class::Hint => !self.dirty.is_empty(),
                Class::List => self.list_due(),
                Class::Poll => !self.poll_queue.is_empty(),
            };
            if ready {
                self.next_class = class.next();
                return Some(class);
            }
            class = class.next();
        }
        None
    }

    /// Every class is empty: wait for control, a hint or the list timer.
    async fn wait(&mut self) -> Result<(), Flow> {
        let due = self.last_list + self.list_interval();
        let poll_due = self.next_poll;
        enum Woke {
            Stop,
            Down(u64),
            Hint(Hint),
            Idle,
        }
        let woke = tokio::select! {
            biased;
            _ = &mut self.stop_rx => Woke::Stop,
            Some(g) = self.down_rx.recv() => Woke::Down(g),
            Some(h) = self.hint_rx.recv() => Woke::Hint(h),
            _ = sleep_until(due) => Woke::Idle,
            _ = sleep_until(poll_due) => Woke::Idle,
        };
        match woke {
            Woke::Stop => Err(Flow::Stop),
            Woke::Down(g) if g == self.generation => Err(Flow::Lost("event stream closed".into())),
            Woke::Hint(h) => {
                self.ingest(h);
                Ok(())
            }
            Woke::Down(_) | Woke::Idle => Ok(()),
        }
    }

    /// One `agent.get` for the front of the dirty queue.
    async fn hint_unit(&mut self) -> Result<(), Flow> {
        let Some(pane_id) = self.dirty.pop_front() else {
            return Ok(());
        };
        self.dirty_set.remove(&pane_id);
        match self.herdr.get(&pane_id).await {
            Ok(info) => {
                self.apply(&pane_id, info);
                Ok(())
            }
            Err(e) if is_server(&e) => {
                tracing::debug!(error = %e, "herdr agent.get failed; resyncing");
                self.resync.store(true, Ordering::Relaxed);
                Ok(())
            }
            Err(e) => Err(Flow::Lost(e.to_string())),
        }
    }

    /// Every `poll`, queues the panes the fallback wants read and the panes
    /// with keys targets, skipping any already queued.
    fn enqueue_due(&mut self) {
        let now = Instant::now();
        if now < self.next_poll {
            return;
        }
        self.next_poll = now + self.timing.poll;
        let mut due = self.fallback.due(now.into_std());
        due.extend(self.target_panes());
        for pane_id in due {
            if self.known.contains_key(&pane_id) && !self.poll_queue.contains(&pane_id) {
                self.poll_queue.push_back(pane_id);
            }
        }
    }

    /// Known panes of this socket with keys targets. Drops targets of this
    /// socket whose pane is gone, and watch counts of removed targets.
    fn target_panes(&mut self) -> Vec<String> {
        let prefix = format!("herdr:{}:", self.hash);
        let by_agent: HashMap<String, &String> = self
            .known
            .keys()
            .map(|p| (map::agent_id(&self.hash, p), p))
            .collect();
        let mut panes = Vec::new();
        let mut live = HashSet::new();
        for t in self.keys.all() {
            if !t.agent_id.starts_with(&prefix) {
                continue;
            }
            match by_agent.get(&t.agent_id) {
                Some(p) => {
                    live.insert(t.request_id);
                    if !panes.contains(*p) {
                        panes.push((*p).clone());
                    }
                }
                None => {
                    self.keys.remove(&t.request_id);
                }
            }
        }
        self.watch_absent.retain(|rid, _| live.contains(rid));
        panes
    }

    /// One `agent.read source=detection` for the front of the poll queue.
    async fn poll_unit(&mut self) -> Result<(), Flow> {
        let Some(pane_id) = self.poll_queue.pop_front() else {
            return Ok(());
        };
        if !self.known.contains_key(&pane_id) {
            return Ok(());
        }
        let read = self
            .herdr
            .read_screen(&pane_id, "detection", None, Instant::now() + CALL_TIMEOUT)
            .await;
        match read {
            Ok(read) => {
                self.apply_read(&pane_id, &read.text);
                self.watch_targets(&pane_id, &read);
                Ok(())
            }
            Err(e) if is_server(&e) => {
                tracing::debug!(error = %e, "herdr agent.read failed");
                Ok(())
            }
            Err(e) => Err(Flow::Lost(e.to_string())),
        }
    }

    fn apply_read(&mut self, pane_id: &str, text: &str) {
        let agent_id = map::agent_id(&self.hash, pane_id);
        let other_pending = self
            .core
            .agent(&agent_id)
            .and_then(|a| a.pending)
            .is_some_and(|p| p.kind != RequestKind::BlockedUnparsed);
        let outs = self
            .fallback
            .on_read(pane_id, text, other_pending, Instant::now().into_std());
        for out in outs {
            match out {
                FbOut::Open {
                    fingerprint,
                    raw_screen,
                } => {
                    let key = fallback::key(&self.hash, pane_id, &fingerprint);
                    self.core.observe(Observation::FallbackSeen(RequestObs {
                        agent_id: agent_id.clone(),
                        key: key.clone(),
                        upgrade_from: None,
                        kind: RequestKind::BlockedUnparsed,
                        answer_path: AnswerPath::None,
                        title: "Waiting for input in the terminal".into(),
                        detail: String::new(),
                        options: vec![],
                        questions: None,
                        free_text: false,
                        raw_screen: Some(raw_screen),
                        ready: false,
                        reusable_key: true,
                    }));
                    match self.core.request_id_for(&agent_id, &key) {
                        Some(request_id) => {
                            self.fb_open.insert(pane_id.to_owned(), (key, request_id));
                        }
                        // A structured request won the race.
                        None => self.fallback.discard_open(pane_id),
                    }
                }
                FbOut::End { .. } => {
                    if let Some((key, request_id)) = self.fb_open.remove(pane_id) {
                        self.core.observe(Observation::FallbackEnded {
                            agent_id: agent_id.clone(),
                            key,
                            request_id,
                        });
                    }
                }
            }
        }
    }

    /// The keys target watch (spec §5.1), on this pane's shared read. A
    /// target is handled by its request's state: `Open` is terminal-first,
    /// `Answering` belongs to the confirmation task, and a resolved request
    /// is monitored. Two consecutive absent reads confirm the dialog gone.
    fn watch_targets(&mut self, pane_id: &str, read: &ScreenRead) {
        let agent_id = map::agent_id(&self.hash, pane_id);
        let targets = self.keys.for_agent(&agent_id);
        if targets.is_empty() {
            return;
        }
        let shown = find_dialog(&read.text).map(|d| DialogIdentity::of(&d));
        for t in targets {
            let status = self.core.request(&t.request_id).map(|r| r.status);
            if status == Some(RequestStatus::Answering) {
                self.watch_absent.remove(&t.request_id);
                continue;
            }
            // A truncated read is neither present nor absent.
            if read.truncated {
                continue;
            }
            let absent = self.watch_absent.entry(t.request_id.clone()).or_default();
            // By identity: a redraw around a visible dialog is not a close.
            if shown.as_ref() == Some(&t.identity) {
                *absent = 0;
                continue;
            }
            *absent += 1;
            if *absent < ABSENT_READS {
                continue;
            }
            self.watch_absent.remove(&t.request_id);
            // Removed before the resolution is published.
            self.keys.remove(&t.request_id);
            self.core.observe(Observation::DialogGoneById {
                request_id: t.request_id.clone(),
            });
            if status == Some(RequestStatus::Open) {
                // Ended without any acd delivery.
                self.core.observe(Observation::RequestEndedById {
                    request_id: t.request_id,
                    by: ResolvedBy::Terminal,
                });
            }
            self.mark_dirty(pane_id.to_owned());
        }
    }

    /// One full `agent.list`. `periodic` lists log changes the events missed.
    async fn list_unit(&mut self, periodic: bool) -> Result<(), Flow> {
        self.resync.store(false, Ordering::Relaxed);
        let result = self.herdr.list().await;
        self.last_list = Instant::now();
        match result {
            Ok(infos) => {
                self.apply_list(infos, periodic);
                Ok(())
            }
            Err(e) if is_server(&e) => {
                tracing::debug!(error = %e, "herdr agent.list failed");
                if !periodic {
                    // Connect-time: agents are still Unknown; retry promptly.
                    self.resync.store(true, Ordering::Relaxed);
                }
                Ok(())
            }
            Err(e) => Err(Flow::Lost(e.to_string())),
        }
    }

    fn apply_list(&mut self, infos: Vec<AgentInfo>, periodic: bool) {
        let mut seen = HashSet::new();
        for info in infos {
            if periodic
                && let Some(obs) = map::agent_obs(&self.hash, &info)
                && self.core.agent(&obs.id).map(|a| a.state) != Some(obs.state)
            {
                tracing::debug!("herdr list found a change the events missed");
            }
            seen.insert(info.pane_id.clone());
            let pane_id = info.pane_id.clone();
            self.apply(&pane_id, Some(info));
        }
        let absent: Vec<String> = self
            .known
            .keys()
            .filter(|p| !seen.contains(*p))
            .cloned()
            .collect();
        for pane_id in absent {
            if periodic {
                tracing::debug!("herdr list found a change the events missed");
            }
            self.apply(&pane_id, None);
        }
    }

    fn apply(&mut self, pane_id: &str, info: Option<AgentInfo>) {
        let agent_id = map::agent_id(&self.hash, pane_id);
        match info.and_then(|i| map::agent_obs(&self.hash, &i)) {
            Some(obs) => {
                if self.known.insert(pane_id.to_owned(), obs.kind) != Some(obs.kind) {
                    self.subs_changed = true;
                }
                if obs.state == AgentState::NeedsYou {
                    self.blocked.insert(pane_id.to_owned());
                } else {
                    self.blocked.remove(pane_id);
                }
                self.panes.write().expect("pane map").insert(
                    agent_id,
                    PaneRef {
                        herdr: self.herdr.clone(),
                        pane_id: pane_id.to_owned(),
                        kind: obs.kind,
                        hints: self.hints_tx.clone(),
                    },
                );
                self.fallback.on_state(
                    pane_id,
                    obs.state == AgentState::NeedsYou,
                    Instant::now().into_std(),
                );
                self.core.observe(Observation::AgentUpsert(obs));
            }
            None => {
                self.fallback.forget(pane_id);
                self.fb_open.remove(pane_id);
                self.keys.remove_agent(&agent_id);
                if self.known.remove(pane_id).is_some() {
                    self.subs_changed = true;
                    self.blocked.remove(pane_id);
                    self.panes.write().expect("pane map").remove(&agent_id);
                    self.core.observe(Observation::AgentGone { agent_id });
                }
            }
        }
    }

    /// Opens a subscription for every known pane, then drops the old one.
    /// Events in the overlap only cause extra `agent.get` calls.
    async fn resubscribe(&mut self, connect: bool) -> Result<(), Flow> {
        self.subs_changed = false;
        let panes: Vec<String> = self.known.keys().cloned().collect();
        let sub = self
            .herdr
            .subscribe(&panes)
            .await
            .map_err(|e| Flow::Lost(e.to_string()))?;
        let added: Vec<String> = panes
            .iter()
            .filter(|p| !self.subscribed.contains(*p))
            .cloned()
            .collect();
        self.subscribed = panes.into_iter().collect();
        if connect {
            // Changes before the stream went live are only visible to a list.
            self.resync.store(true, Ordering::Relaxed);
        } else {
            for pane_id in added {
                self.mark_dirty(pane_id);
            }
        }
        self.generation += 1;
        let reader = tokio::spawn(read_events(
            sub,
            self.generation,
            self.hints_tx.clone(),
            self.resync.clone(),
            self.down_tx.clone(),
        ));
        self.sub = Some(SubReader(reader));
        Ok(())
    }
}

/// Turns events into hints. A full channel drops the event and sets the
/// sticky resync flag. EOF reports `generation` as lost.
async fn read_events(
    mut sub: Subscription,
    generation: u64,
    hints: mpsc::Sender<Hint>,
    resync: Arc<AtomicBool>,
    down: mpsc::UnboundedSender<u64>,
) {
    while let Some(event) = sub.next().await {
        let hint = match event {
            Event::StatusChanged { pane_id, .. }
            | Event::AgentDetected { pane_id }
            | Event::Updated { pane_id }
            | Event::Created { pane_id } => Hint::Pane(pane_id),
            Event::AgentReleased { pane_id } | Event::Closed { pane_id } => Hint::Closed(pane_id),
            Event::Other => Hint::Resync,
        };
        if hints.try_send(hint).is_err() {
            resync.store(true, Ordering::Relaxed);
        }
    }
    let _ = down.send(generation);
}
