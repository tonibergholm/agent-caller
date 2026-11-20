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
use acd_protocol::{Agent, AgentState, AnswerPath, ErrorCode, Request, Source, SourceState, SourceStatus};
use async_trait::async_trait;
use tokio::time::{Instant, timeout_at};

use self::client::CallError;
use self::discovery::Discovery;
use self::keys::{AgentLocks, Engine, Evidence, KeysTargets, KeysTiming};
use self::reconciler::{Hint, PaneMap, Timing, pane_replaced};
use self::screen::find_dialog;

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

    /// Shared with P3 (D7).
    pub fn agent_locks(&self) -> Arc<AgentLocks> {
        self.keys.locks()
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
        let deadline = Instant::now() + SEND_DEADLINE;
        let unavailable = || DeliverError::NotDelivered(ErrorCode::SourceUnavailable);
        // Held to the end: the pane cannot change owner between the checks and the prompt.
        let _lock = timeout_at(deadline, self.keys.locks().lock(&agent.id))
            .await
            .map_err(|_| unavailable())?;
        // Clone out of the map so no lock is held across an await.
        let pane = self.panes.read().expect("pane map").get(&agent.id).cloned();
        let Some(pane) = pane else {
            return Err(DeliverError::NotDelivered(ErrorCode::AgentGone));
        };
        // The agent id is a pane id and outlives the agent in it. The core
        // checked the client's incarnation against `agent`; `agent` must
        // still be this pane's agent now, else the reply was written for one
        // that has left (or the registry has not caught up).
        let marker = map::incarnation_id(pane.incarnation, pane.terminal_id.as_deref());
        if agent.incarnation.as_deref() != Some(marker.as_str()) {
            return Err(DeliverError::NotDelivered(ErrorCode::AgentGone));
        }
        // A dialog may have opened since the last poll: text starting with "1"
        // would pick "Yes". A read that fails or is cut short proves nothing.
        let read = match pane
            .herdr
            .read_screen(&pane.pane_id, "detection", None, deadline)
            .await
        {
            Ok(read) => read,
            Err(_) => {
                // A pane whose agent has left cannot be read: say so, not "unavailable".
                let left = matches!(
                    timeout_at(deadline, pane.herdr.get(&pane.pane_id)).await,
                    Ok(Ok(live)) if live.as_ref().is_none_or(|i| pane_replaced(&pane, i))
                );
                if left {
                    let _ = pane.hints.try_send(Hint::Pane(pane.pane_id.clone()));
                    return Err(DeliverError::NotDelivered(ErrorCode::AgentGone));
                }
                return Err(unavailable());
            }
        };
        if read.truncated {
            return Err(unavailable());
        }
        // Decided after the identity check: a dialog of a replacement agent
        // means this agent is gone, not busy.
        let dialog = find_dialog(&read.text).is_some();
        // The pane may have changed hands since the reconciler last looked,
        // including during the screen read above. Ask herdr now, after the
        // read, fail closed.
        let live = timeout_at(deadline, pane.herdr.get(&pane.pane_id))
            .await
            .map_err(|_| unavailable())?
            .map_err(|_| unavailable())?;
        let Some(live) = live.filter(|i| !pane_replaced(&pane, i)) else {
            let _ = pane.hints.try_send(Hint::Pane(pane.pane_id.clone()));
            return Err(DeliverError::NotDelivered(ErrorCode::AgentGone));
        };
        // herdr reports a dialog this parser cannot read as `blocked`.
        if dialog || map::state(live.agent_status) == AgentState::NeedsYou {
            return Err(DeliverError::NotDelivered(ErrorCode::AgentBusy));
        }
        // The reconciler does not take the agent lock, so it can have replaced
        // the pane while the screen read ran. Re-read the map entry right
        // before the prompt. A live name follows the occupant, so herdr itself
        // refuses a replaced agent; a pane id has no such guard and the gap
        // between this check and herdr stays for unnamed agents (p1 follow-ups).
        let unchanged = self
            .panes
            .read()
            .expect("pane map")
            .get(&agent.id)
            .is_some_and(|p| p.incarnation == pane.incarnation && p.terminal_id == pane.terminal_id);
        if !unchanged {
            return Err(DeliverError::NotDelivered(ErrorCode::AgentGone));
        }
        let target = live.name.as_deref().filter(|n| !n.is_empty()).unwrap_or(&pane.pane_id);
        let result = match timeout_at(deadline, pane.herdr.prompt(target, text)).await {
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
