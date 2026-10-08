//! herdr adapter (spec §6.4).
pub mod client;
pub mod discovery;
#[cfg(any(test, feature = "testing"))]
pub mod fake;
pub mod fallback;
pub mod keys;
pub mod map;
pub mod reconciler;
pub mod screen;
pub mod wire;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use acd_core::{Adapter, Answer, Core, DeliverError};
use acd_protocol::{Agent, AnswerPath, ErrorCode, Request, Source, SourceState, SourceStatus};
use async_trait::async_trait;

use self::client::CallError;
use self::discovery::Discovery;
use self::keys::{Engine, Evidence, KeysTargets, KeysTiming};
use self::reconciler::{Hint, PaneMap, Timing};

const SEND_DEADLINE: Duration = Duration::from_secs(10);
const SCAN_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
pub struct HerdrConfig {
    pub enabled: bool,
    pub home: PathBuf,
    pub env_socket: Option<PathBuf>,
    pub extra_sockets: Vec<PathBuf>,
    pub scan_interval: Duration,
    pub timing: Timing,
    pub keys: KeysTiming,
}

impl HerdrConfig {
    /// Reads `$HOME` and `$HERDR_SOCKET_PATH`; default timing, 30 s scan.
    pub fn from_env(enabled: bool, extra_sockets: Vec<PathBuf>) -> HerdrConfig {
        let home = std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                tracing::warn!(
                    "HOME is not set; the default herdr socket and sessions are not discovered"
                );
                PathBuf::new()
            });
        HerdrConfig {
            enabled,
            home,
            env_socket: std::env::var_os("HERDR_SOCKET_PATH")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            extra_sockets,
            scan_interval: SCAN_INTERVAL,
            timing: Timing::default(),
            keys: KeysTiming::default(),
        }
    }
}

pub struct HerdrAdapter {
    cfg: HerdrConfig,
    panes: Arc<PaneMap>,
    keys: Engine,
    /// Held weakly: the core owns the adapter.
    core: Mutex<Weak<Core>>,
}

impl HerdrAdapter {
    pub fn new(cfg: HerdrConfig) -> Arc<HerdrAdapter> {
        Arc::new(HerdrAdapter {
            keys: Engine::new(cfg.keys.clone()),
            cfg,
            panes: Arc::default(),
            core: Mutex::new(Weak::new()),
        })
    }

    /// Spawns socket discovery. A disabled config only reports `disabled`.
    pub fn start(self: &Arc<Self>, core: Arc<Core>) {
        if !self.cfg.enabled {
            core.set_source_status(
                Source::Herdr,
                SourceStatus {
                    state: SourceState::Disabled,
                    message: None,
                },
            );
            return;
        }
        *self.core.lock().expect("core slot") = Arc::downgrade(&core);
        discovery::spawn(
            Discovery {
                env_socket: self.cfg.env_socket.clone(),
                home: self.cfg.home.clone(),
                extra: self.cfg.extra_sockets.clone(),
                interval: self.cfg.scan_interval,
                timing: self.cfg.timing.clone(),
                keys: self.keys.targets(),
            },
            core,
            self.panes.clone(),
        );
    }

    pub fn panes(&self) -> Arc<PaneMap> {
        self.panes.clone()
    }

    /// Where P3 registers keys-answerable occurrences.
    pub fn keys_targets(&self) -> Arc<KeysTargets> {
        self.keys.targets()
    }

    /// Replaces the default `NoEvidence`.
    pub fn set_evidence(&self, e: Arc<dyn Evidence>) {
        self.keys.set_evidence(e);
    }
}

fn map_call_error(e: &CallError) -> DeliverError {
    if e.reached_agent() {
        return DeliverError::Uncertain(match e {
            CallError::Server { code, .. } => format!("herdr error {code}"),
            _ => "herdr did not confirm the prompt".into(),
        });
    }
    DeliverError::NotDelivered(match e {
        CallError::Server { code, .. } if code == "agent_blocked" => ErrorCode::AgentBusy,
        CallError::Server { code, .. } if code == "agent_not_found" => ErrorCode::AgentGone,
        _ => ErrorCode::SourceUnavailable,
    })
}

#[async_trait]
impl Adapter for HerdrAdapter {
    fn name(&self) -> &'static str {
        "herdr"
    }

    fn answer_paths(&self) -> &[AnswerPath] {
        &[AnswerPath::Keys]
    }

    fn sources(&self) -> &[Source] {
        &[Source::Herdr]
    }

    async fn deliver(&self, request: &Request, answer: &Answer) -> Result<(), DeliverError> {
        let core = self.core.lock().expect("core slot").clone();
        self.keys.deliver(core, &self.panes, request, answer).await
    }

    async fn send(&self, agent: &Agent, text: &str) -> Result<(), DeliverError> {
        if crate::text::check_prompt_text(text).is_err() {
            return Err(DeliverError::NotDelivered(ErrorCode::BadRequest));
        }
        // Clone out of the map so no lock is held across an await.
        let pane = self.panes.read().expect("pane map").get(&agent.id).cloned();
        let Some(pane) = pane else {
            return Err(DeliverError::NotDelivered(ErrorCode::AgentGone));
        };
        let result =
            match tokio::time::timeout(SEND_DEADLINE, pane.herdr.prompt(&pane.pane_id, text)).await
            {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(map_call_error(&e)),
                Err(_) => Err(DeliverError::Uncertain(
                    "herdr did not confirm the prompt in time".into(),
                )),
            };
        if matches!(result, Ok(()) | Err(DeliverError::Uncertain(_))) {
            // The reconciler may be reconnecting and not draining: never block.
            let _ = pane.hints.try_send(Hint::Pane(pane.pane_id.clone()));
        }
        result
    }
}
