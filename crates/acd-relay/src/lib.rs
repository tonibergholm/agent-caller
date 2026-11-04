//! The acd relay: turns a content-free wake from `acd` into a push.
//!
//! Spec: `docs/superpowers/specs/2026-11-03-relay-push-design.md`.
//! No request content ever reaches this crate: the wake request type has
//! two fields, a version and a handle.
//!
//! Feature `client` is what `acd` uses (wake sender, minimal HTTP client).
//! Feature `server` (default) adds the relay itself and the FCM code, which
//! must stay out of the daemon.

pub mod provisional;
pub mod wire;

#[cfg(feature = "client")]
pub mod client;
#[cfg(feature = "client")]
pub mod http;

#[cfg(feature = "server")]
pub mod challenge;
#[cfg(feature = "server")]
pub mod config;
#[cfg(feature = "server")]
pub mod fcm;
#[cfg(feature = "server")]
pub mod handle;
#[cfg(feature = "server")]
pub mod limits;
#[cfg(feature = "server")]
pub mod server;

use std::sync::atomic::{AtomicU64, Ordering};

/// Seconds since the Unix epoch. A trait so tests can move time.
pub trait Clock: Send + Sync {
    fn now_secs(&self) -> u64;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now_secs(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
}

/// A clock a test moves by hand.
#[derive(Default)]
pub struct ManualClock(AtomicU64);

impl ManualClock {
    pub fn new(start: u64) -> Self {
        ManualClock(AtomicU64::new(start))
    }
    pub fn advance(&self, secs: u64) {
        self.0.fetch_add(secs, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_secs(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}
