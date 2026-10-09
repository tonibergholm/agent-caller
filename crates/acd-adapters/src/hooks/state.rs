//! The hooks reconciler: the only code that changes hook-derived state.
//! It drains the endpoint mailbox and its internal channel; it never awaits a herdr call itself.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};

use acd_core::{AgentObs, Core, Observation};
use acd_protocol::{AgentKind, AgentState, Effect, Source};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;

use super::HooksConfig;
use super::claude_sessions::{CompleteScan, ProcCheck, Scan, Scanner, map_status};
use super::endpoint::{HoldId, HoldPermit, Inbound, Mailbox};
use super::owner::{Owner, PaneFact, claude_owner, pane_facts};
use super::wire::{AlwaysRule, HookAgent, HookEvent, HookName};
use crate::herdr::HerdrAdapter;
use crate::t3::sessions::SessionMap;
use crate::text::{LAST_MESSAGE_CHARS, TITLE_CHARS, cap_chars, clean_display};

#[derive(Default)]
pub(crate) struct Shared {
    #[allow(dead_code)] // Populated and consumed by the permission hold path in Task 14.
    pub held: Mutex<HashMap<String, HeldReq>>,
    pub last_scan: Mutex<Option<CompleteScan>>,
    pub by_generation: Mutex<BTreeMap<u64, HashSet<String>>>,
}

#[allow(dead_code)] // The Task 14 delivery path consumes these fields.
pub(crate) struct HeldReq {
    pub hold: HoldId,
    pub reply: Option<oneshot::Sender<String>>,
    pub rule: Option<AlwaysRule>,
    pub delivered: Option<(Effect, Instant)>,
    pub permit: Option<HoldPermit>,
}

pub(crate) enum Internal {
    Scanned(Scan),
}

struct Session {
    owner: Owner,
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
    internal_tx: mpsc::UnboundedSender<Internal>,
    internal_rx: mpsc::UnboundedReceiver<Internal>,
    scanner: Option<Arc<Mutex<Scanner>>>,
    sessions: HashMap<String, Session>,
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
        let (internal_tx, internal_rx) = mpsc::unbounded_channel();
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
            scanner,
            sessions: HashMap::new(),
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

    fn spawn_scan(&self) {
        let Some(scanner) = self.scanner.clone() else {
            return;
        };
        let dirs = self.cfg.sessions_dirs.get();
        if dirs.is_empty() {
            return;
        }
        let tx = self.internal_tx.clone();
        tokio::task::spawn_blocking(move || {
            let mut scanner = scanner.lock().expect("scanner");
            scanner.set_dirs(dirs);
            let scan = scanner.scan();
            let _ = tx.send(Internal::Scanned(scan));
        });
    }

    fn panes(&self) -> Vec<PaneFact> {
        self.herdr
            .as_ref()
            .map(|h| pane_facts(&h.panes()))
            .unwrap_or_default()
    }

    fn owner_of(
        &self,
        session_id: &str,
        hints: &super::wire::Hints,
        entrypoint: Option<&str>,
    ) -> Owner {
        let t3 = self
            .t3
            .as_deref()
            .map(|m| m as &dyn super::owner::SessionOwners);
        claude_owner(
            session_id,
            hints,
            entrypoint,
            t3,
            &self.panes(),
            &self.cfg.ignored_entrypoints,
        )
    }

    async fn on_inbound(&mut self, message: Inbound) {
        match message {
            Inbound::Event {
                event, generation, ..
            } => self.on_event(event, generation).await,
            Inbound::Held {
                event, generation, ..
            } => self.on_event(event, generation).await,
            Inbound::HoldClosed { .. } | Inbound::HoldExpired { .. } => {}
        }
    }

    async fn on_event(&mut self, event: HookEvent, generation: u64) {
        {
            let mut generations = self.shared.by_generation.lock().expect("shared");
            let ids = generations.entry(generation).or_default();
            if ids.len() < self.cfg.max_generation_ids {
                ids.insert(event.payload.session_id.clone());
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
            HookAgent::Claude => self.on_claude(event).await,
            HookAgent::Codex => self.on_codex(event).await,
        }
    }

    async fn on_claude(&mut self, event: HookEvent) {
        let sid = event.payload.session_id.clone();
        let entrypoint = event.hints.entrypoint.clone();
        let owner = self.owner_of(&sid, &event.hints, entrypoint.as_deref());
        let now = Instant::now();
        let session = self.sessions.entry(sid.clone()).or_insert_with(|| Session {
            owner: Owner::Unknown,
            bind_gen: 0,
            agent_id: None,
            hooks_only: false,
            cwd: None,
            name: None,
            entrypoint: None,
            state: AgentState::Idle,
            last_message: None,
            last_hook: None,
            last_event: now,
            in_registry: false,
            ended: None,
        });
        session.last_event = now;
        session.last_hook = Some(now);
        if event.payload.cwd.is_some() {
            session.cwd = event.payload.cwd.clone();
        }
        if entrypoint.is_some() {
            session.entrypoint = entrypoint;
        }
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
            HookName::SessionEnd => session.ended = Some(now),
            _ => {}
        }
        self.apply_owner(&sid, owner).await;
    }

    async fn on_codex(&mut self, _event: HookEvent) {}

    async fn on_internal(&mut self, message: Internal) {
        match message {
            Internal::Scanned(scan) => self.on_scan(scan).await,
        }
    }

    async fn apply_owner(&mut self, sid: &str, owner: Owner) {
        let hooks_id = format!("claude:{sid}");
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
                    self.core
                        .observe(Observation::AgentGone { agent_id: hooks_id });
                }
            }
            Owner::Herdr(agent) => {
                session.agent_id = Some(agent.clone());
                session.hooks_only = false;
                if was_hooks_only && self.core.agent(&agent).is_some() {
                    let locks = self.herdr.as_ref().map(|h| h.agent_locks());
                    let _guard = match &locks {
                        Some(locks) => Some(locks.lock(&agent).await),
                        None => None,
                    };
                    self.core.observe(Observation::AgentMigrated {
                        from: hooks_id,
                        to: agent,
                    });
                }
            }
            Owner::Hooks => {
                session.agent_id = Some(hooks_id.clone());
                session.hooks_only = true;
                self.core
                    .observe(Observation::AgentUpsert(hooks_only_obs(&hooks_id, session)));
            }
        }
    }

    async fn reevaluate_all(&mut self) {
        let ids: Vec<(String, Option<String>)> = self
            .sessions
            .iter()
            .map(|(id, s)| (id.clone(), s.entrypoint.clone()))
            .collect();
        for (sid, entrypoint) in ids {
            let owner = self.owner_of(&sid, &Default::default(), entrypoint.as_deref());
            if self.sessions.get(&sid).is_some_and(|s| s.owner != owner) {
                self.apply_owner(&sid, owner).await;
            }
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
            let session = self
                .sessions
                .entry(entry.session_id.clone())
                .or_insert_with(|| Session {
                    owner: Owner::Unknown,
                    bind_gen: 0,
                    agent_id: None,
                    hooks_only: false,
                    cwd: None,
                    name: None,
                    entrypoint: None,
                    state: AgentState::Unknown,
                    last_message: None,
                    last_hook: None,
                    last_event: now,
                    in_registry: true,
                    ended: None,
                });
            session.in_registry = true;
            session.last_event = now;
            session.cwd = session.cwd.clone().or_else(|| entry.cwd.clone());
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
            let owner = self.owner_of(
                &entry.session_id,
                &Default::default(),
                entry.entrypoint.as_deref(),
            );
            self.apply_owner(&entry.session_id, owner).await;
        }
    }

    async fn on_tick(&mut self) {
        let now = Instant::now();
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
    if let Some(receiver) = rx {
        let _ = receiver.changed().await;
    } else {
        std::future::pending::<()>().await;
    }
}
