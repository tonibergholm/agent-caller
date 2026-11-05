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

/// Targets that can log peer addresses or credentials. Above `warn` they
/// are dropped, whatever the user's directives say.
#[cfg(feature = "server")]
const CAPPED_TARGETS: [&str; 6] = ["axum", "hyper", "hyper_util", "rustls", "tokio_rustls", "h2"];

/// True when an event of `level` from `target` may be logged.
#[cfg(feature = "server")]
fn ceiling_allows(target: &str, level: &tracing::Level) -> bool {
    let capped = CAPPED_TARGETS
        .iter()
        .any(|p| target == *p || target.strip_prefix(p).is_some_and(|rest| rest.starts_with("::")));
    !capped || *level <= tracing::Level::WARN
}

/// The relay's log layer: the user's directives (`RUST_LOG`) decide what is
/// logged, then a ceiling that no directive can raise drops events above
/// `warn` from the third-party targets in `CAPPED_TARGETS` (and their
/// submodules). At `trace`, `axum::serve` logs every peer address and
/// rustls can log the address of an OAuth endpoint; spec §8 says IPs never
/// reach a log.
#[cfg(feature = "server")]
pub fn log_layer<S, W>(user: &str, writer: W) -> impl tracing_subscriber::Layer<S>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    W: for<'a> tracing_subscriber::fmt::MakeWriter<'a> + 'static,
{
    use tracing_subscriber::Layer;
    use tracing_subscriber::filter::{FilterExt, filter_fn};
    let ceiling = filter_fn(|m| ceiling_allows(m.target(), m.level()));
    tracing_subscriber::fmt::layer()
        .with_writer(writer)
        .with_ansi(false)
        .with_filter(tracing_subscriber::EnvFilter::new(user).and(ceiling))
}

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
