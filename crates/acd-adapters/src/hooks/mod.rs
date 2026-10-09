//! Claude and Codex hooks (spec §6.1 to §6.3).
pub mod claude_sessions;
pub mod dialogs;
pub mod endpoint;
pub mod evidence;
pub mod mapping;
pub mod owner;
mod state;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod wire;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use acd_core::{Adapter, Answer, Core, DeliverError};
use acd_protocol::{Agent, AnswerPath, ErrorCode, Request, Source};
use async_trait::async_trait;

use self::claude_sessions::{CompleteScan, ProcCheck};
use self::endpoint::{EndpointOptions, HookAuth, Mailbox, MailboxTx};
use crate::herdr::HerdrAdapter;
use crate::t3::sessions::SessionMap;

#[derive(Clone, Debug)]
pub struct HooksConfig {
    /// `claude.permission_hold` (spec §9); holds end here with an empty reply.
    pub permission_hold: Duration,
    pub ignored_entrypoints: Vec<String>,
    /// Registry roots (`<claude config dir>/sessions`), asked before every
    /// scan; empty disables the scan.
    pub sessions_dirs: SessionRoots,
    pub scan_interval: Duration,
    pub tick: Duration,
    /// Correlated outcome evidence window (spec §5.1, §6.2: 30 s).
    pub evidence_window: Duration,
    /// Fingerprint capture after a request opens (spec §6.2 step 6: 2 s).
    pub capture_window: Duration,
    pub capture_poll: Duration,
    /// Codex dialog wait (spec §6.3: 1 s).
    pub codex_window: Duration,
    /// Presence checks of a held request's captured dialog.
    pub held_dialog_poll: Duration,
    /// Removal after `SessionEnd` or the registry entry disappearing (10 min).
    pub end_grace: Duration,
    /// Removal after no event at all (24 h).
    pub idle_expiry: Duration,
    pub max_holds: usize,
    /// A3: screen reads running at once.
    pub max_reads: usize,
    /// A5: open tracks.
    pub max_tracks: usize,
    /// A7: the longest the reconciler waits for a delivery lock to apply a read result.
    pub lock_wait: Duration,
    /// A6: items per queue per loop turn.
    pub batch: usize,
    /// O7: how long an unconsumed identity can shadow a new occurrence.
    pub unconsumed_ttl: Duration,
    /// A8: Claude sessions, Codex sessions and sessions with unconsumed
    /// identities, each.
    pub max_sessions: usize,
    /// A8: session ids per credential generation.
    pub max_generation_ids: usize,
}

#[derive(Clone)]
pub struct SessionRoots(Arc<dyn Fn() -> Vec<PathBuf> + Send + Sync>);

impl SessionRoots {
    pub fn fixed(dirs: Vec<PathBuf>) -> Self {
        Self(Arc::new(move || dirs.clone()))
    }
    pub fn dynamic(f: impl Fn() -> Vec<PathBuf> + Send + Sync + 'static) -> Self {
        Self(Arc::new(f))
    }
    pub fn none() -> Self {
        Self::fixed(vec![])
    }
    pub fn get(&self) -> Vec<PathBuf> {
        (self.0)()
    }
}

impl std::fmt::Debug for SessionRoots {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionRoots")
    }
}

impl HooksConfig {
    pub fn new(
        permission_hold: Duration,
        ignored_entrypoints: Vec<String>,
        sessions_dirs: SessionRoots,
    ) -> Self {
        Self {
            permission_hold,
            ignored_entrypoints,
            sessions_dirs,
            scan_interval: Duration::from_secs(30),
            tick: Duration::from_millis(250),
            evidence_window: Duration::from_secs(30),
            capture_window: Duration::from_secs(2),
            capture_poll: Duration::from_millis(200),
            codex_window: Duration::from_secs(1),
            held_dialog_poll: Duration::from_secs(1),
            end_grace: Duration::from_secs(600),
            idle_expiry: Duration::from_secs(24 * 3600),
            max_holds: 64,
            max_reads: 8,
            max_tracks: 256,
            lock_wait: Duration::from_secs(2),
            batch: 32,
            unconsumed_ttl: Duration::from_secs(600),
            max_sessions: 4096,
            max_generation_ids: 4096,
        }
    }
}

pub struct HooksAdapter {
    cfg: HooksConfig,
    herdr: Option<Arc<HerdrAdapter>>,
    t3: Option<Arc<SessionMap>>,
    proc: Arc<dyn ProcCheck>,
    shared: Arc<state::Shared>,
    opts: EndpointOptions,
    tx: MailboxTx,
    rx: Mutex<Option<Mailbox>>,
}

impl HooksAdapter {
    pub fn new(
        cfg: HooksConfig,
        herdr: Option<Arc<HerdrAdapter>>,
        t3: Option<Arc<SessionMap>>,
        proc: Arc<dyn ProcCheck>,
    ) -> Arc<Self> {
        let opts = EndpointOptions {
            permission_hold: cfg.permission_hold,
            max_holds: cfg.max_holds,
            ..Default::default()
        };
        let (tx, rx) = endpoint::mailbox(&opts);
        Arc::new(Self {
            cfg,
            herdr,
            t3,
            proc,
            shared: Arc::default(),
            opts,
            tx,
            rx: Mutex::new(Some(rx)),
        })
    }
    pub fn router(&self, auth: Arc<HookAuth>) -> axum::Router {
        endpoint::router(auth, self.tx.clone(), self.opts.clone())
    }
    pub fn start(self: &Arc<Self>, core: Arc<Core>) {
        let rx = self
            .rx
            .lock()
            .expect("rx")
            .take()
            .expect("HooksAdapter::start called twice");
        let r = state::Reconciler::new(
            self.cfg.clone(),
            core,
            self.herdr.clone(),
            self.t3.clone(),
            self.proc.clone(),
            self.shared.clone(),
            rx,
        );
        tokio::spawn(r.run());
    }
    pub fn last_complete_scan(&self) -> Option<CompleteScan> {
        self.shared.last_scan.lock().expect("shared").clone()
    }
    /// Session ids that were not recorded under a generation because it
    /// already held `max_generation_ids` (diagnostics).
    pub fn dropped_generation_ids(&self) -> u64 {
        self.shared
            .dropped_generation_ids
            .load(std::sync::atomic::Ordering::Relaxed)
    }
    pub fn sessions_on_generation(&self, generation: u64) -> HashSet<String> {
        self.shared
            .by_generation
            .lock()
            .expect("shared")
            .get(&generation)
            .cloned()
            .unwrap_or_default()
    }
}

#[async_trait]
impl Adapter for HooksAdapter {
    fn name(&self) -> &'static str {
        "hooks"
    }
    fn answer_paths(&self) -> &[AnswerPath] {
        &[AnswerPath::Hook]
    }
    fn sources(&self) -> &[Source] {
        &[Source::Hooks]
    }
    async fn deliver(&self, _request: &Request, _answer: &Answer) -> Result<(), DeliverError> {
        Err(DeliverError::NotDelivered(ErrorCode::RequestGone))
    }
    async fn send(&self, _agent: &Agent, _text: &str) -> Result<(), DeliverError> {
        Err(DeliverError::NotDelivered(ErrorCode::NotSupported))
    }
}
