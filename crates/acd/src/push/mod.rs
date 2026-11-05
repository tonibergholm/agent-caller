//! Push: wakes for registered devices (relay spec 2026-11-03-relay-push-design
//! §2.3, §5). Off unless `[push]` names an https relay; then `acd` sends one
//! content-free wake per registered device when a request opens, through
//! `acd_relay::client::WakeSender`.
//!
//! What may reach the relay is a handle and the constant `v`. Nothing about a
//! request, agent or machine is passed to anything in this module:
//! [`Push::request_opened`] takes no argument, and request ids (which restart
//! at `r1` on every start) are never used as identifiers here.

mod gate;
mod store;
#[cfg(test)]
mod e2e;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use acd_core::{PushControl, PushFailure, PushRegistration};
use acd_protocol::{ErrorCode, PushInfo};
use acd_relay::client::{Outcome, RelayUrl, WakeSender};
use acd_relay::{Clock, SystemClock};
use tokio::sync::{Notify, Semaphore, watch};
use tokio::task::JoinSet;
use tokio::time::Instant;

use gate::Gate;
use store::{Device, Store, StoreError};

/// A relay name for the CA bundles of the usual hosts, tried in order when
/// `[push] ca_file` is not set (Debian and Ubuntu, then macOS and Alpine).
pub const DEFAULT_CA_FILES: [&str; 2] = ["/etc/ssl/certs/ca-certificates.crt", "/etc/ssl/cert.pem"];

/// At most one wake per device per interval (relay spec §2.3).
pub const MIN_INTERVAL: Duration = Duration::from_secs(30);
/// Sends in flight at once. There are at most [`MAX_DEVICES`] devices.
pub const MAX_CONCURRENT: usize = 4;

#[derive(Debug, Clone, Copy)]
pub struct Settings {
    pub min_interval: Duration,
    pub max_concurrent: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { min_interval: MIN_INTERVAL, max_concurrent: MAX_CONCURRENT }
    }
}

/// What `[push]` means. Fail closed: anything but a clean https relay with a
/// usable CA file is `Off`, with a reason for `acd status` (never a value).
pub enum Resolved {
    Off(Option<String>),
    On { relay: RelayUrl, sender: WakeSender },
}

/// Interprets the `[push]` section: `relay` (https only), optional `ca_file`
/// (PEM; the relay's certificate must chain to a certificate in it, so a CA
/// bundle and a pinned self-signed certificate both work). Without `ca_file`
/// the first existing file of `defaults` is used. Unknown keys, a bad URL,
/// a missing or empty CA file and `relay_pin` all turn push off.
pub fn resolve(
    section: Option<&toml::Value>,
    defaults: &[&str],
    clock: Arc<dyn Clock>,
) -> Resolved {
    let off = |why: &str| Resolved::Off(Some(why.to_string()));
    let mut ca = None;
    let mut rest = section.cloned();
    if let Some(toml::Value::Table(t)) = rest.as_mut()
        && let Some(v) = t.remove("ca_file")
    {
        match v.as_str() {
            Some(s) if !s.is_empty() => ca = Some(crate::config::expand_home(s)),
            _ => return off("[push] ca_file is not a non-empty string"),
        }
    }
    let relay = match acd_relay::client::resolve(rest.as_ref()) {
        acd_relay::client::PushSetting::Off(None) if ca.is_some() => {
            return off("[push] ca_file is set without relay");
        }
        acd_relay::client::PushSetting::Off(why) => return Resolved::Off(why),
        acd_relay::client::PushSetting::On(url) => url,
    };
    let path = match ca {
        Some(p) => p,
        None => match defaults.iter().map(std::path::PathBuf::from).find(|p| p.is_file()) {
            Some(p) => p,
            None => return off("no CA bundle found; set [push] ca_file"),
        },
    };
    let Ok(pem) = std::fs::read(&path) else {
        return off("[push] ca_file cannot be read");
    };
    match WakeSender::https(relay.clone(), &pem, clock) {
        Ok(sender) => Resolved::On { relay, sender },
        Err(_) => off("[push] ca_file has no usable certificate"),
    }
}

/// A client id is peer-chosen and may be the token itself, so a log line
/// carries only its byte length and a SHA-256 prefix (as the server does).
const MAX_DEVICES_LOGGED: usize = 8;

fn client_tag(id: &str) -> String {
    let (len, sha) = acd_core::server::log_tag(id);
    format!("len={len} sha={sha}")
}

/// The push engine: the registrations, the debounce and the sender.
pub struct Push {
    relay: String,
    sender: WakeSender,
    settings: Settings,
    store: Arc<Mutex<Store>>,
    /// Requests opened so far. A count: the sender is told nothing else.
    opened: AtomicU64,
    wake: Notify,
    /// The `opened` count the runner has handled (tests wait on it).
    handled: watch::Sender<u64>,
}

impl Push {
    pub fn new(relay: &RelayUrl, sender: WakeSender, store: Store, settings: Settings) -> Arc<Push> {
        Arc::new(Push {
            relay: relay.as_str().to_string(),
            sender,
            settings,
            store: Arc::new(Mutex::new(store)),
            opened: AtomicU64::new(0),
            wake: Notify::new(),
            handled: watch::channel(0).0,
        })
    }

    /// Starts the task that sends wakes. It ends with the runtime.
    pub fn spawn(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(self.clone().run())
    }

    /// Resolves once the runner has handled `n` opened requests.
    #[cfg(test)]
    pub async fn handled(&self, n: u64) {
        let mut rx = self.handled.subscribe();
        let _ = rx.wait_for(|h| *h >= n).await;
    }

    fn store(&self) -> MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// File writes are small but synchronous: keep them off the runtime.
    async fn blocking<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Store) -> T + Send + 'static,
    ) -> Option<T> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || f(&mut store.lock().unwrap_or_else(|e| e.into_inner())))
            .await
            .ok()
    }

    async fn run(self: Arc<Self>) {
        let min = self.settings.min_interval;
        let mut gates: HashMap<String, Gate> = HashMap::new();
        let mut seen = 0;
        loop {
            let due = gates.values().filter_map(|g| g.deadline(min)).min();
            tokio::select! {
                () = self.wake.notified() => {}
                () = async {
                    match due {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                } => {}
            }
            let opened = self.opened.load(Ordering::SeqCst);
            self.cycle(&mut gates, opened > seen).await;
            seen = opened;
            self.handled.send_replace(opened);
        }
    }

    /// One pass: wakes the devices whose gate allows it. Sends run together,
    /// at most `max_concurrent` at a time, and the pass ends when they have.
    /// A device has at most one send in flight, because passes do not overlap.
    async fn cycle(self: &Arc<Self>, gates: &mut HashMap<String, Gate>, any_open: bool) {
        let now = Instant::now();
        let min = self.settings.min_interval;
        let targets: Vec<_> = self
            .store()
            .iter()
            .map(|(id, e)| (id.clone(), e.generation, e.device.handle().to_string(), e.device.wake_state()))
            .collect();
        gates.retain(|id, _| targets.iter().any(|t| &t.0 == id));
        let permits = Arc::new(Semaphore::new(self.settings.max_concurrent.max(1)));
        let mut sends = JoinSet::new();
        for (id, generation, handle, mut state) in targets {
            let gate = gates.entry(id.clone()).or_default();
            let go = (any_open && gate.opened(now, min)) | gate.trailing_due(now, min);
            if !go {
                continue;
            }
            let (this, permits) = (self.clone(), permits.clone());
            sends.spawn(async move {
                let _permit = permits.acquire_owned().await;
                // The debounce window starts when the send does, not when the
                // pass did: a send queued behind slow ones must not wake the
                // device again less than `min` after it.
                let started = Instant::now();
                let outcome = this.sender.wake(&handle, &mut state).await;
                (id, generation, state, outcome, started)
            });
        }
        while let Some(done) = sends.join_next().await {
            let Ok((id, generation, state, outcome, started)) = done else {
                tracing::error!("a wake task failed");
                continue;
            };
            let gate = gates.entry(id.clone()).or_default();
            let tag = client_tag(&id);
            match outcome {
                Outcome::Skipped | Outcome::Disabled => gate.dropped(),
                _ => gate.attempted(started),
            }
            match outcome {
                Outcome::Sent => tracing::debug!(client = %tag, "wake sent"),
                Outcome::RateLimited => tracing::info!(client = %tag, "wake dropped: relay rate limit"),
                Outcome::Skipped | Outcome::Disabled => {}
                Outcome::Failed => tracing::warn!(
                    client = %tag, failures = state.failures, "wake failed; retrying with backoff"
                ),
                Outcome::Invalidated => {
                    tracing::warn!(client = %tag, status = ?state.status, "wake registration disabled");
                }
            }
            self.blocking(move |s| s.apply_wake(&id, generation, &state)).await;
        }
    }
}

#[async_trait::async_trait]
impl PushControl for Push {
    fn request_opened(&self) {
        self.opened.fetch_add(1, Ordering::SeqCst);
        self.wake.notify_one();
    }

    async fn register(
        &self,
        client_id: &str,
        reg: PushRegistration,
    ) -> Result<(), PushFailure> {
        let bad = |message| Err(PushFailure { code: ErrorCode::BadRequest, message });
        // F2: the configured relay, byte for byte.
        if reg.relay != self.relay {
            return bad("relay mismatch");
        }
        if !store::handle_ok(&reg.handle) {
            return bad("invalid handle");
        }
        let device = Device::new(
            reg.platform,
            reg.handle,
            reg.expires_at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        );
        let id = client_id.to_string();
        let stored = self.blocking(move |s| s.register(&id, device)).await;
        match stored {
            Some(Ok(())) => {
                tracing::info!(client = %client_tag(client_id), "push device registered");
                Ok(())
            }
            Some(Err(StoreError::TooMany)) => bad("too many push devices"),
            Some(Err(StoreError::BadId)) => bad("invalid client id"),
            Some(Err(StoreError::Corrupt)) => Err(PushFailure {
                code: ErrorCode::SourceUnavailable,
                message: "push.json is corrupt; repair or remove it",
            }),
            Some(Err(StoreError::Write)) | None => Err(PushFailure {
                code: ErrorCode::SourceUnavailable,
                message: "could not store the registration",
            }),
        }
    }

    async fn unregister(&self, client_id: &str) -> Result<(), PushFailure> {
        let id = client_id.to_string();
        match self.blocking(move |s| s.remove(&id)).await {
            Some(Ok(())) => Ok(()),
            _ => Err(PushFailure {
                code: ErrorCode::SourceUnavailable,
                message: "could not store the registration",
            }),
        }
    }

    async fn status(&self, client_id: &str) -> PushInfo {
        let store = self.store();
        match store.get(client_id) {
            None => PushInfo { registered: false, status: None, last_wake_at: None },
            Some(e) => PushInfo {
                registered: true,
                status: Some(e.device.status()),
                last_wake_at: e
                    .device
                    .last_wake_at()
                    .and_then(|s| chrono::DateTime::from_timestamp(i64::try_from(s).ok()?, 0)),
            },
        }
    }
}

/// Turns push on for a starting daemon: the engine, if `[push]` resolves.
/// Called inside the runtime, before the daemon serves.
pub fn enable(dir: &Path, cfg: &crate::config::Config, core: &acd_core::Core) -> Option<Arc<Push>> {
    match resolve(cfg.push.as_ref(), &DEFAULT_CA_FILES, Arc::new(SystemClock)) {
        Resolved::Off(None) => None,
        Resolved::Off(Some(why)) => {
            tracing::warn!(reason = %why, "push is off");
            None
        }
        Resolved::On { relay, sender } => {
            let store = Store::load(dir);
            if let Some(why) = store.degraded() {
                tracing::warn!(reason = %why, "push is degraded: starting with no registrations; push.json is left untouched and registering fails until it is repaired or removed");
            }
            let skipped = store.skipped_ids().count();
            if skipped > 0 {
                let tags: Vec<String> = store.skipped_ids().take(MAX_DEVICES_LOGGED).map(client_tag).collect();
                tracing::warn!(skipped, ids = ?tags, "push.json: skipped invalid entries; kept in push.json.quarantine at the next write");
            }
            let push = Push::new(&relay, sender, store, Settings::default());
            core.set_push(push.clone());
            push.spawn();
            tracing::info!(devices = push.store().len(), "push is on");
            Some(push)
        }
    }
}

/// The `push:` line of `acd status`, from the config and `push.json` (not
/// from the running daemon).
pub fn report(dir: &Path, cfg: &crate::config::Config) -> String {
    match resolve(cfg.push.as_ref(), &DEFAULT_CA_FILES, Arc::new(SystemClock)) {
        Resolved::Off(None) => "push: off".into(),
        Resolved::Off(Some(why)) => format!("push: off ({why})"),
        Resolved::On { .. } => {
            let store = Store::load(dir);
            match store.degraded() {
                Some(why) => format!("push: degraded ({why})"),
                None => format!("push: on, {} device(s)", store.len()),
            }
        }
    }
}

/// `acd push list`: one line per registered device, read from `push.json`.
/// Handles are never printed.
pub fn list(dir: &Path, cfg: &crate::config::Config, out: &mut impl std::io::Write) -> std::io::Result<()> {
    writeln!(out, "{}", report(dir, cfg))?;
    let store = Store::load(dir);
    for (id, e) in store.iter() {
        let last = e
            .device
            .last_wake_at()
            .and_then(|s| chrono::DateTime::from_timestamp(i64::try_from(s).ok()?, 0))
            .map_or("never".to_string(), |t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
        writeln!(
            out,
            "{}  {:?}  {:?}  last wake {last}",
            id.escape_debug(),
            e.device.platform,
            e.device.status()
        )?;
    }
    Ok(())
}
