//! Producer source: a local program publishes questions and acts on the
//! answers itself (docs/superpowers/specs/2026-10-13-external-source-design.md).
//!
//! acd stays a mailbox. A producer holds a lease with `PUT .../state`, which
//! also upserts its items, acknowledges answers and fetches the queued
//! ones. The API is served on `hook.sock` only.
mod http;
mod state;
pub mod wire;

use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use acd_core::{Adapter, Answer, Core, DeliverError, Txn};
use acd_protocol::{Agent, AnswerPath, ErrorCode, Request, Source};
use async_trait::async_trait;
use tokio::sync::Notify;

use self::state::{QueueError, State};
use self::wire::{AnswerOut, Invalid, PutBody, PutResponse};
use super::hooks::endpoint::HookAuth;

/// Milliseconds on a clock that only moves forward.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

#[derive(Clone)]
pub struct ExternalConfig {
    /// Monotonic: leases and answer deadlines run on it.
    pub clock: Clock,
    /// Wall clock: only the `expires_at_ms` a producer sees is stamped from it.
    pub wall: Clock,
    /// How often leases and answer deadlines are checked between calls.
    pub tick: Duration,
}

impl Default for ExternalConfig {
    fn default() -> Self {
        let start = Instant::now();
        let base = wall_ms();
        Self {
            clock: Arc::new(move || base + start.elapsed().as_millis() as i64),
            wall: Arc::new(wall_ms),
            tick: Duration::from_secs(1),
        }
    }
}

fn wall_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

pub struct ExternalAdapter {
    cfg: ExternalConfig,
    state: Mutex<State>,
    core: Mutex<Weak<Core>>,
    arrived: Notify,
}

impl ExternalAdapter {
    pub fn new(cfg: ExternalConfig) -> Arc<ExternalAdapter> {
        Arc::new(ExternalAdapter {
            cfg,
            state: Mutex::new(State::default()),
            core: Mutex::new(Weak::new()),
            arrived: Notify::new(),
        })
    }

    pub fn start(self: &Arc<Self>, core: Arc<Core>) {
        *self.core.lock().expect("core slot") = Arc::downgrade(&core);
        let adapter = Arc::downgrade(self);
        let tick = self.cfg.tick;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tick).await;
                let Some(adapter) = adapter.upgrade() else {
                    return;
                };
                adapter.sweep();
            }
        });
    }

    /// The producer API. Serve it on the Unix socket only: merge it into the
    /// hook router there, and give the TCP listener the hook router alone.
    pub fn router(self: &Arc<Self>, auth: Arc<HookAuth>) -> axum::Router {
        http::router(self.clone(), auth)
    }

    pub fn now_ms(&self) -> i64 {
        (self.cfg.clock)()
    }

    fn core(&self) -> Option<Arc<Core>> {
        self.core.lock().expect("core slot").upgrade()
    }

    /// Runs `f` on the producer state inside one registry transaction.
    fn run<R>(&self, f: impl FnOnce(&mut State, &mut Txn<'_>, i64, i64) -> R) -> Option<R> {
        let core = self.core()?;
        let (now, wall) = (self.now_ms(), (self.cfg.wall)());
        let mut state = self.state.lock().expect("external state");
        Some(core.transact(|txn| f(&mut state, txn, now, wall)))
    }

    /// Ends expired leases and answers now, rather than at the next call.
    pub fn sweep(&self) {
        self.run(|state, txn, now, _| state.sweep(now, txn));
    }

    pub(crate) fn put(&self, pid: &str, body: PutBody) -> Result<PutResponse, PutError> {
        let outcome = self
            .run(|state, txn, now, wall| state.put(pid, body, now, wall, txn))
            .ok_or(PutError::Stopped)?;
        self.arrived.notify_waiters();
        outcome.map_err(PutError::Invalid)
    }

    pub(crate) fn delete(&self, pid: &str) -> bool {
        self.run(|state, txn, now, _| state.delete(pid, now, txn))
            .is_some()
    }

    pub(crate) async fn answers(&self, pid: &str, wait: Duration) -> Option<Vec<AnswerOut>> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let notified = self.arrived.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let answers = self.run(|state, txn, now, wall| state.answers(pid, now, wall, txn))?;
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if !answers.is_empty() || left.is_zero() {
                return Some(answers);
            }
            tokio::select! {
                () = &mut notified => {}
                () = tokio::time::sleep(left.min(self.cfg.tick)) => {}
            }
        }
    }
}

pub(crate) enum PutError {
    Invalid(Invalid),
    Stopped,
}

#[async_trait]
impl Adapter for ExternalAdapter {
    fn name(&self) -> &'static str {
        "external"
    }
    fn answer_paths(&self) -> &[AnswerPath] {
        &[AnswerPath::External]
    }
    fn sources(&self) -> &[Source] {
        &[Source::External]
    }

    async fn deliver(&self, request: &Request, answer: &Answer) -> Result<(), DeliverError> {
        let now = self.now_ms();
        let queued = self.state.lock().expect("external state").queue(
            &request.id,
            request.revision,
            answer,
            now,
        );
        match queued {
            Ok(()) => {
                self.arrived.notify_waiters();
                Ok(())
            }
            Err(QueueError::Gone) => Err(DeliverError::NotDelivered(ErrorCode::RequestGone)),
            Err(QueueError::Unavailable) => {
                Err(DeliverError::NotDelivered(ErrorCode::SourceUnavailable))
            }
        }
    }

    async fn send(&self, _agent: &Agent, _text: &str) -> Result<(), DeliverError> {
        Err(DeliverError::NotDelivered(ErrorCode::NotSupported))
    }
}
