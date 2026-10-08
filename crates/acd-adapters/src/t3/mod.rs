//! T3 Code adapter (spec §6.5, gate 3 evidence in docs/research/2026-10-08-t3-gate3.md).
pub mod conn;
mod deferred_ends;
pub mod detail;
#[cfg(any(test, feature = "testing"))]
pub mod fake;
pub mod map;
pub mod outcome;
pub mod reconciler;
pub mod sessions;
pub mod targets;
pub mod wire;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use acd_core::{Adapter, Answer, Core, DeliverError, Observation};
use acd_protocol::{
    Agent, AnswerPath, ErrorCode, Request, ResolvedBy, Source, SourceState, SourceStatus,
};
use async_trait::async_trait;
use tokio::time::Instant;

#[cfg(any(test, feature = "testing"))]
use self::conn::Conn;
use self::outcome::Outcome as DispatchOutcome;
use self::reconciler::{Shared, SyncConfig, SyncHandle, T3Timing};
use self::sessions::SessionMap;

#[derive(Clone, Debug)]
pub struct T3Config {
    pub enabled: bool,
    pub url: Option<String>,
    pub token_path: PathBuf,
    pub timing: T3Timing,
    pub dispatch_budget: Duration,
}

impl T3Config {
    pub fn new(enabled: bool, url: Option<String>, token_path: PathBuf) -> T3Config {
        T3Config {
            enabled,
            url,
            token_path,
            timing: T3Timing::default(),
            dispatch_budget: Duration::from_secs(8),
        }
    }
}

pub struct T3Adapter {
    cfg: T3Config,
    shared: Arc<Shared>,
    sync: Mutex<Option<SyncHandle>>,
    core: Mutex<Weak<Core>>,
}

impl T3Adapter {
    pub fn new(cfg: T3Config) -> Arc<T3Adapter> {
        Arc::new(T3Adapter {
            cfg,
            shared: Arc::default(),
            sync: Mutex::new(None),
            core: Mutex::new(Weak::new()),
        })
    }

    fn core(&self) -> Option<Arc<Core>> {
        self.core.lock().expect("core slot").upgrade()
    }

    fn degrade(&self, msg: Option<&'static str>) {
        if let Some(msg) = msg {
            self.shared.mailbox.degraded(msg);
        }
    }

    pub fn start(self: &Arc<Self>, core: Arc<Core>) {
        if !self.cfg.enabled {
            core.set_source_status(
                Source::T3,
                SourceStatus {
                    state: SourceState::Disabled,
                    message: None,
                },
            );
            return;
        }
        let Some(url) = self.cfg.url.as_deref() else {
            core.set_source_status(
                Source::T3,
                SourceStatus {
                    state: SourceState::Unconfigured,
                    message: Some("set t3.url in config.toml".into()),
                },
            );
            return;
        };
        *self.core.lock().expect("core slot") = Arc::downgrade(&core);
        let handle = reconciler::spawn(
            SyncConfig {
                ws_url: conn::ws_url(url),
                token_path: self.cfg.token_path.clone(),
                timing: self.cfg.timing.clone(),
            },
            core,
            self.shared.clone(),
        );
        *self.sync.lock().expect("sync slot") = Some(handle);
    }

    pub fn sessions(&self) -> Arc<SessionMap> {
        self.shared.sessions.clone()
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn current_conn_for_tests(&self) -> Option<Conn> {
        self.shared.current_conn()
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn shared_for_tests(&self) -> Arc<Shared> {
        self.shared.clone()
    }
}

#[async_trait]
impl Adapter for T3Adapter {
    fn name(&self) -> &'static str {
        "t3"
    }
    fn answer_paths(&self) -> &[AnswerPath] {
        &[AnswerPath::T3]
    }
    fn sources(&self) -> &[Source] {
        &[Source::T3]
    }

    async fn deliver(&self, request: &Request, answer: &Answer) -> Result<(), DeliverError> {
        let deadline = Instant::now() + self.cfg.dispatch_budget;
        let Some(core) = self.core() else {
            return Err(DeliverError::NotDelivered(ErrorCode::SourceUnavailable));
        };
        let Some(target) = self.shared.targets.find(&core, request) else {
            return Err(DeliverError::NotDelivered(ErrorCode::SourceUnavailable));
        };
        let command_id = uuid::Uuid::new_v4().to_string();
        let payload = match map::respond_payload(&command_id, &target, request, answer) {
            Ok(payload) => payload,
            Err(why) => {
                tracing::warn!(request_id = %request.id, why, "T3 answer not sent");
                return Err(DeliverError::NotDelivered(ErrorCode::BadRequest));
            }
        };
        let Some(conn) = self.shared.current_conn() else {
            return Err(DeliverError::NotDelivered(ErrorCode::SourceUnavailable));
        };
        let result = conn.call(wire::DISPATCH, &payload, deadline).await;
        match outcome::classify(&result, Some(&target.t3_request_id)) {
            DispatchOutcome::Delivered => {
                core.observe(Observation::RequestEndedById {
                    request_id: request.id.clone(),
                    by: ResolvedBy::Inbox,
                });
                self.shared
                    .mailbox
                    .answered(&target.thread_id, &target.t3_request_id);
                Ok(())
            }
            DispatchOutcome::NotDelivered { code, end, degrade } => {
                if let Some(by) = end {
                    // The core releases this claim after deliver returns. The reconciler
                    // defers the conditional end until the request is open again.
                    self.shared.mailbox.request_ended(request.id.clone(), by);
                }
                self.degrade(degrade);
                Err(DeliverError::NotDelivered(code))
            }
            DispatchOutcome::Uncertain(why) => Err(DeliverError::Uncertain(why.to_string())),
        }
    }

    async fn send(&self, agent: &Agent, text: &str) -> Result<(), DeliverError> {
        let deadline = Instant::now() + self.cfg.dispatch_budget;
        if crate::text::check_prompt_text(text).is_err() {
            return Err(DeliverError::NotDelivered(ErrorCode::BadRequest));
        }
        let thread_id = self
            .shared
            .threads
            .read()
            .expect("threads")
            .get(&agent.id)
            .cloned();
        let Some(thread_id) = thread_id else {
            return Err(DeliverError::NotDelivered(ErrorCode::AgentGone));
        };
        let Some(conn) = self.shared.current_conn() else {
            return Err(DeliverError::NotDelivered(ErrorCode::SourceUnavailable));
        };
        let payload = map::dispatch_payload(
            &uuid::Uuid::new_v4().to_string(),
            &uuid::Uuid::new_v4().to_string(),
            &thread_id,
            text,
        );
        match outcome::classify(&conn.call(wire::DISPATCH, &payload, deadline).await, None) {
            DispatchOutcome::Delivered => Ok(()),
            DispatchOutcome::NotDelivered { code, degrade, .. } => {
                self.degrade(degrade);
                Err(DeliverError::NotDelivered(code))
            }
            DispatchOutcome::Uncertain(why) => Err(DeliverError::Uncertain(why.to_string())),
        }
    }
}
