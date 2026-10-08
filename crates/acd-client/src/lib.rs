//! acd/1 client for the agent-caller Mac app (spec §8). No UI code lives
//! here, so Linux CI tests all of it.
use std::time::Duration;

pub mod backoff;
pub mod endpoint;
pub mod local;
pub mod session;
pub mod settings;

/// Every timing rule in one place; tests shrink these.
#[derive(Clone, Debug)]
pub struct Timing {
    pub connect_total: Duration,
    pub idle_ping: Duration,
    pub ping_timeout: Duration,
    pub undo: Duration,
    pub not_ready_window: Duration,
    pub not_ready_interval: Duration,
    pub notice_ttl: Duration,
    pub stale_inflight: Duration,
    pub backoff_base: Duration,
    pub backoff_max: Duration,
    pub identity_retry: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        let s = Duration::from_secs;
        Self {
            connect_total: s(8),
            idle_ping: s(15),
            ping_timeout: s(10),
            undo: s(2),
            not_ready_window: s(3),
            not_ready_interval: Duration::from_millis(250),
            notice_ttl: s(10),
            stale_inflight: s(14 * 60),
            backoff_base: s(1),
            backoff_max: s(30),
            identity_retry: s(30),
        }
    }
}
