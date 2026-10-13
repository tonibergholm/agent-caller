use std::future::IntoFuture;
use std::io::Write;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use acd_adapters::hooks::endpoint::{self, Credential, HookAuth};
use acd_core::{Auth, Core, ServerOptions, router, tls as core_tls};
use acd_protocol::{Machine, Source, SourceState, SourceStatus, Sources};
use anyhow::{Result, bail};

use crate::{bind, config::Config, identity, local_socket, secrets};

/// How long a clean stop waits for the hook servers to send the replies of
/// released holds and close their connections.
const HOOK_DRAIN: Duration = Duration::from_secs(3);

/// Exit code that asks the supervisor (launchd or systemd) to restart acd
/// after the Tailscale addresses change.
pub const EXIT_ADDRESSES_CHANGED: i32 = 75;
/// A server task stopped: the supervisor restarts acd.
pub const EXIT_SERVER_STOPPED: i32 = 1;
/// Any panic, including a poisoned mutex (D14).
pub const EXIT_PANIC: i32 = 70;

fn exit_on_panic() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default(info);
        let _ = writeln!(
            std::io::stderr(),
            "acd: panic; exiting so the supervisor restarts acd"
        );
        std::process::exit(EXIT_PANIC);
    }));
}

/// The Unix socket in the config dir that local tools such as `acd status` use.
pub fn socket_path(dir: &Path) -> PathBuf {
    dir.join("acd.sock")
}

/// The hook endpoint's Unix socket, used by the Codex shim (amendment A1).
pub fn hook_socket_path(dir: &Path) -> PathBuf {
    dir.join("hook.sock")
}

/// Stands in for the roots that could not be discovered. It can never be
/// opened (the NUL byte), so the scan that lists it is incomplete (F4).
pub const UNDISCOVERED_ROOT: &str = "<Claude registry roots unavailable>\0";

/// Claude registry roots: the roots of recorded Claude installs and of
/// uninstalled ones the previous token may still be in use under, then the
/// default root, deduplicated (D24). Read again before every scan. When the
/// install records cannot be read, `UNDISCOVERED_ROOT` makes the scan
/// incomplete, so it retires no token.
pub fn claude_roots(
    config_dir: &Path,
    home: &Path,
    claude_config_dir: Option<&Path>,
) -> Vec<PathBuf> {
    use std::sync::atomic::{AtomicBool, Ordering};
    static FAILING: AtomicBool = AtomicBool::new(false);
    let mut roots = match crate::hooks_install::recorded_claude_roots(config_dir) {
        Ok(roots) => {
            FAILING.store(false, Ordering::Relaxed);
            roots
        }
        Err(e) => {
            if !FAILING.swap(true, Ordering::Relaxed) {
                tracing::error!(error = %format!("{e:#}"), "cannot read the hook install records; Claude registry scans stay incomplete");
            }
            vec![PathBuf::from(UNDISCOVERED_ROOT)]
        }
    };
    let default = acd_adapters::hooks::claude_sessions::sessions_dir(home, claude_config_dir);
    let default = std::fs::canonicalize(default.parent().unwrap_or(&default))
        .map(|p| p.join("sessions"))
        .unwrap_or(default);
    if !roots.contains(&default) {
        roots.push(default);
    }
    roots
}

/// Daemon-local credential tags (T5). Each distinct token gets its own tag,
/// which the hook endpoint attaches to every request it authenticates, so
/// the sessions recorded under a tag used exactly that token. A store
/// generation cannot scope them: a forced reset of an unreadable store
/// starts again at generation 1, and events already queued keep the tag
/// they were authenticated with. Only the store's current and previous
/// tokens keep a tag; a token that comes back later gets a new one.
#[derive(Default)]
pub struct CredentialTags {
    last: u64,
    tags: Vec<(String, u64)>,
}

impl CredentialTags {
    fn tag_into(&mut self, kept: &mut Vec<(String, u64)>, token: &str) -> u64 {
        let tag = match self.tags.iter().find(|(t, _)| t == token) {
            Some((_, tag)) => *tag,
            None => {
                self.last += 1;
                self.last
            }
        };
        kept.push((token.to_owned(), tag));
        tag
    }

    /// The credentials of `s`. The previous one carries its expiry, which
    /// the endpoint checks on every request (T4).
    pub fn credentials(
        &mut self,
        s: &crate::hook_token::Store,
    ) -> (Credential, Option<Credential>) {
        let mut kept = Vec::new();
        // The previous token first, so on first sight it gets the older tag.
        let previous = s.previous.as_ref().map(|p| Credential {
            token: p.token.clone(),
            generation: self.tag_into(&mut kept, &p.token),
            expires_at_ms: Some(p.expires_at_ms),
        });
        let current = Credential {
            token: s.current.clone(),
            generation: self.tag_into(&mut kept, &s.current),
            expires_at_ms: None,
        };
        self.tags = kept;
        (current, previous)
    }
}

/// The hook endpoint's credentials, loaded from the token store. Every
/// update loads the store and sets the auth under one lock, so a slower
/// reload never replaces the result of a newer one (T5).
pub struct HookCredentials {
    dir: PathBuf,
    auth: Arc<HookAuth>,
    applied: std::sync::Mutex<Applied>,
    /// Debug builds only: every reload fails once a retirement committed
    /// (`ACD_TEST_FAIL_AT=reload-after-retire`).
    #[cfg(debug_assertions)]
    fail_reloads: std::sync::atomic::AtomicBool,
}

struct Applied {
    tags: CredentialTags,
    /// The accepted current credential, the tags of the accepted
    /// credentials, and when those tags last changed.
    current: Credential,
    accepted: (u64, Option<u64>),
    changed_at: tokio::time::Instant,
}

impl HookCredentials {
    pub fn new(dir: &Path, store: &crate::hook_token::Store) -> Self {
        let mut tags = CredentialTags::default();
        let (cur, prev) = tags.credentials(store);
        let accepted = (cur.generation, prev.as_ref().map(|p| p.generation));
        let auth = Arc::new(HookAuth::new(cur.clone()));
        auth.set(cur.clone(), prev);
        Self {
            dir: dir.to_path_buf(),
            auth,
            applied: std::sync::Mutex::new(Applied {
                tags,
                current: cur,
                accepted,
                changed_at: tokio::time::Instant::now(),
            }),
            #[cfg(debug_assertions)]
            fail_reloads: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// When the accepted credentials last changed. A request rejected
    /// before that may only have come before the reload (a session that
    /// read new headers first), so the status does not count it; a session
    /// that really holds a retired token is rejected again.
    pub fn changed_at(&self) -> tokio::time::Instant {
        self.applied.lock().expect("hook credentials").changed_at
    }

    pub fn auth(&self) -> Arc<HookAuth> {
        self.auth.clone()
    }

    /// Loads the store and applies it. Returns the store and the tags of
    /// its current and previous tokens. On an error the credentials stay as
    /// they were.
    pub fn reload(&self) -> Result<(crate::hook_token::Store, (u64, Option<u64>))> {
        let mut applied = self.applied.lock().expect("hook credentials");
        #[cfg(debug_assertions)]
        if self.fail_reloads.load(std::sync::atomic::Ordering::SeqCst) {
            bail!("injected failure at reload-after-retire");
        }
        let store = crate::hook_token::load(&self.dir)?;
        let (cur, prev) = applied.tags.credentials(&store);
        let accepted = (cur.generation, prev.as_ref().map(|p| p.generation));
        if accepted != applied.accepted {
            applied.accepted = accepted;
            applied.changed_at = tokio::time::Instant::now();
        }
        applied.current = cur.clone();
        self.auth.set(cur, prev);
        Ok((store, accepted))
    }

    /// T5: a committed retirement of the previous token tagged `previous`
    /// takes effect at once, with no further store read. It applies only
    /// while that token is still the accepted previous one, so a newer
    /// credential set (a refresh that already saw a later store) is never
    /// replaced.
    pub fn retired(&self, previous: u64) {
        let mut applied = self.applied.lock().expect("hook credentials");
        if applied.accepted.1 == Some(previous) {
            applied.accepted.1 = None;
            applied.changed_at = tokio::time::Instant::now();
            applied.tags.tags.retain(|(_, tag)| *tag != previous);
            self.auth.set(applied.current.clone(), None);
        }
        #[cfg(debug_assertions)]
        if std::env::var("ACD_TEST_FAIL_AT").is_ok_and(|s| s == "reload-after-retire") {
            self.fail_reloads
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// Rejected hook requests for the status: those since `window_start`
    /// and since the accepted credentials last changed. A request rejected
    /// before the change may only have come before the reload (a session
    /// that read new headers first); a session that really holds a retired
    /// token is rejected again and counted.
    pub fn rejected_for_status(&self, window_start: tokio::time::Instant) -> usize {
        self.auth
            .rejected_since(window_start.max(self.changed_at()))
    }
}

fn degraded(message: String) -> SourceStatus {
    SourceStatus {
        state: SourceState::Degraded,
        message: Some(message),
    }
}

/// T4 caller contract: built only from a complete scan, with every live
/// registry session. None is left out (not T3-owned, ignored or refused
/// ones): any live Claude session may still hold the previous token.
fn scan_view(c: acd_adapters::hooks::claude_sessions::CompleteScan) -> crate::hook_token::ScanView {
    crate::hook_token::ScanView {
        started_at_ms: c.started_at_ms,
        finished_at_ms: c.finished_at_ms,
        live: c
            .live
            .into_iter()
            .map(|e| crate::hook_token::LiveSession {
                session_id: e.session_id,
                started_at_ms: e.started_at_ms,
            })
            .collect(),
    }
}

/// One retirement attempt (T4, T5, T6) and the hooks source status it
/// leads to. `rejected_since` bounds the count of rejected hook requests.
async fn retire_once(
    creds: &HookCredentials,
    hooks: &acd_adapters::hooks::HooksAdapter,
    fresh_within: Duration,
    rejected_since: tokio::time::Instant,
) -> SourceStatus {
    use crate::hook_token::{Retire, retire_if_done, status_message};
    let (store, (tag, previous_tag)) = match creds.reload() {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "hook token store unreadable; keeping the current credentials");
            return degraded(format!("hook token store unreadable: {e}"));
        }
    };
    let scan = hooks.last_complete_scan().map(scan_view);
    // T5: the sessions seen with this store's current token, by its tag.
    let on_current = hooks.sessions_on_generation(tag);
    let (dir, expected) = (creds.dir.clone(), store.clone());
    // T6: `retire_if_done` takes the blocking writer lock and reads the
    // clock after holding it.
    let result = tokio::task::spawn_blocking(move || {
        retire_if_done(
            &dir,
            &expected,
            scan.as_ref(),
            fresh_within,
            &on_current,
            &|| chrono::Utc::now().timestamp_millis(),
        )
    })
    .await
    .unwrap_or_else(|e| Err(anyhow::anyhow!("retirement task failed: {e}")));
    let (kept, retire_error) = match result {
        Ok(Retire::Kept(k)) => (Some(k), None),
        Ok(Retire::Retired) => {
            tracing::info!("previous hook token retired");
            // T5: revoke it in memory now; the reload below may fail.
            if let Some(previous) = previous_tag {
                creds.retired(previous);
            }
            (None, None)
        }
        Ok(Retire::Nothing | Retire::Moved) => (None, None),
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "could not retire the previous hook token");
            (None, Some(e.to_string()))
        }
    };
    // The store as it is now, for the status.
    let store = match creds.reload() {
        Ok((s, _)) => s,
        Err(e) => {
            tracing::error!(error = %e, "hook token store unreadable; keeping the current credentials");
            return degraded(format!("hook token store unreadable: {e}"));
        }
    };
    let rejected = creds.rejected_for_status(rejected_since);
    match status_message(&store, kept.as_ref(), rejected, retire_error.as_deref()) {
        Some((state, m)) => SourceStatus {
            state,
            message: Some(m),
        },
        None => SourceStatus {
            state: SourceState::Ok,
            message: None,
        },
    }
}

/// Debug builds only: `ACD_TEST_STOP_SERVER=<name>` ends that server task
/// about a second after it starts (after every listener is up, even on a
/// slow machine), to test supervision.
fn stop_server_for_tests(name: &str) -> bool {
    #[cfg(debug_assertions)]
    {
        std::env::var("ACD_TEST_STOP_SERVER").is_ok_and(|v| v == name)
    }
    #[cfg(not(debug_assertions))]
    {
        let _ = name;
        false
    }
}

/// A listener that ends while the daemon runs is a failure; one that ends
/// during a clean stop is the teardown.
fn listener_end_is_failure(stopping: &std::sync::atomic::AtomicBool) -> bool {
    !stopping.load(std::sync::atomic::Ordering::SeqCst)
}

/// The "a stop has begun" flag. It is also set when `run` returns early with
/// an error, because the listener tasks are torn down on that path too.
#[derive(Default)]
struct StopFlag(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl StopFlag {
    fn flag(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        self.0.clone()
    }
    fn set(&self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Drop for StopFlag {
    fn drop(&mut self) {
        self.set();
    }
}

async fn released(mut rx: tokio::sync::watch::Receiver<bool>) {
    let _ = rx.wait_for(|stop| *stop).await;
}

pub async fn run(dir: &Path) -> Result<()> {
    exit_on_panic();
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    #[cfg(debug_assertions)]
    if let Some(ms) = std::env::var("ACD_TEST_PANIC_AFTER_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
    {
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            panic!("test panic");
        });
    }
    crate::preflight(dir)?;
    let cfg = Config::load(dir)?;
    // A1: one daemon per config dir, from before migration and any bind
    // until the process exits.
    let lock = local_socket::lock_daemon(dir)?;
    #[cfg(debug_assertions)]
    if let Some(ms) = std::env::var("ACD_TEST_BLOCKING_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
    {
        // A blocking task that outlives a clean stop.
        tokio::task::spawn_blocking(move || std::thread::sleep(Duration::from_millis(ms)));
    }
    let (key_files, installed_pin) = crate::tls::load(dir)?;
    let server_config = core_tls::server_config(&key_files)?;
    let (tls_tx, tls_rx) = tokio::sync::watch::channel(server_config);
    let admission_limit = {
        #[cfg(debug_assertions)]
        {
            std::env::var("ACD_TEST_MAX_UNAUTH")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(core_tls::MAX_UNAUTHENTICATED_TCP)
        }
        #[cfg(not(debug_assertions))]
        {
            core_tls::MAX_UNAUTHENTICATED_TCP
        }
    };
    let admission = core_tls::Admission::new(admission_limit);
    let token = secrets::read_secret(&dir.join("client.token"))
        .and_then(|t| secrets::parse_token(&t))
        .map_err(|e| {
            anyhow::anyhow!("client.token is invalid ({e}); run `acd token rotate client`")
        })?;
    let machine = Machine {
        id: identity::machine_id(dir)?,
        name: cfg.machine_name.clone(),
        acd_version: env!("CARGO_PKG_VERSION").into(),
        sources: Sources {
            hooks: SourceStatus::unconfigured(),
            herdr: SourceStatus::unconfigured(),
            t3: SourceStatus::unconfigured(),
        },
    };
    let extra = cfg
        .herdr
        .extra_sockets
        .iter()
        .map(|s| crate::config::expand_home(s))
        .collect();
    let herdr = acd_adapters::herdr::HerdrAdapter::new(acd_adapters::herdr::HerdrConfig::from_env(
        cfg.herdr.enabled,
        extra,
    ));
    let t3 = acd_adapters::t3::T3Adapter::new(acd_adapters::t3::T3Config::new(
        cfg.t3.enabled,
        cfg.t3.url.clone(),
        dir.join("t3.token"),
    ));
    // T6: migration takes the blocking writer lock (a CLI may hold it).
    let store = {
        let d = dir.to_path_buf();
        tokio::task::spawn_blocking(move || crate::hook_token::migrate(&d)).await??
    };
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let claude_dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let roots_dir = dir.to_path_buf();
    let roots = acd_adapters::hooks::SessionRoots::dynamic(move || {
        claude_roots(&roots_dir, &home, claude_dir.as_deref())
    });
    #[allow(unused_mut)]
    let mut hooks_cfg = acd_adapters::hooks::HooksConfig::new(
        Duration::from_secs(cfg.claude.permission_hold),
        cfg.ignored_entrypoints.clone(),
        roots,
    );
    #[cfg(debug_assertions)]
    if std::env::var_os("ACD_TEST_HOOKS_FAST").is_some() {
        hooks_cfg.scan_interval = Duration::from_millis(200);
    }
    // T4: a scan counts for retirement while it is at most two intervals old.
    let fresh_within = hooks_cfg.scan_interval * 2;
    let hooks = acd_adapters::hooks::HooksAdapter::new(
        hooks_cfg,
        Some(herdr.clone()),
        Some(t3.sessions()),
        Arc::new(acd_adapters::hooks::claude_sessions::SystemProc),
    );
    let core = Core::new(machine, vec![herdr.clone(), t3.clone(), hooks.clone()]);
    herdr.set_evidence(hooks.evidence());
    herdr.start(core.clone());
    t3.start(core.clone());
    hooks.start(core.clone());
    let auth = Arc::new(Auth::new(token.clone()));
    let app = router(
        core.clone(),
        auth.clone(),
        ServerOptions {
            allowed_origins: cfg.allowed_origins.clone(),
            ..Default::default()
        },
    );

    let listen_includes_tailscale = cfg.listen.iter().any(|e| e == "tailscale");
    // Without a Tailscale entry nothing depends on the probe: never run the
    // external CLI.
    let needs_probe = bind::needs_tailscale_probe(&cfg.listen);
    let probe = if needs_probe {
        bind::tailscale_reported().await
    } else {
        None
    };
    let reported = probe.clone().unwrap_or_default();
    let ips = bind::resolve_listen(&cfg.listen, &reported)?;
    if listen_includes_tailscale {
        if bind::tailscale_cli().is_none() {
            tracing::warn!(
                searched = %bind::tailscale_cli_searched(),
                "Tailscale CLI not found; Tailscale addresses unavailable"
            );
        } else if probe.is_none() {
            tracing::warn!("`tailscale ip` failed or timed out; Tailscale addresses unavailable");
        } else if !reported.iter().any(bind::in_tailscale_range) {
            tracing::warn!("listen contains \"tailscale\" but no Tailscale addresses are reported");
        }
    }
    // A socket path that `bind` would refuse fails startup here, before any
    // listener is bound (#16).
    local_socket::check_path(&socket_path(dir))?;
    local_socket::check_path(&hook_socket_path(dir))?;
    // D13: the hook endpoint binds first, so a taken hook port fails startup
    // before any socket file exists. One router serves both hook listeners,
    // so they share the hold cap and hold ids.
    let hook_creds = Arc::new(HookCredentials::new(dir, &store));
    let hook_addr = SocketAddr::from(([127, 0, 0, 1], cfg.hook_port));
    let hook_listener = tokio::net::TcpListener::bind(hook_addr)
        .await
        .map_err(|e| anyhow::anyhow!("cannot bind hook endpoint {hook_addr}: {e}"))?;
    let hook_sock = hook_socket_path(dir);
    let (hook_unix, hook_sock_owned) = local_socket::bind(&lock, &hook_sock)?;
    let hook_unix = tokio::net::UnixListener::from_std(hook_unix)?;
    let hook_app = hooks.router(hook_creds.auth());

    // Bind everything before serving; any failure is fatal except ::1 being
    // unavailable (IPv6 disabled).
    let mut listeners = Vec::new();
    for ip in &ips {
        let addr = SocketAddr::new(*ip, cfg.port);
        match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => listeners.push((addr, l)),
            Err(e)
                if e.kind() == std::io::ErrorKind::AddrNotAvailable
                    && *ip == IpAddr::V6(Ipv6Addr::LOCALHOST) =>
            {
                tracing::warn!(%addr, error = %e, "IPv6 loopback unavailable; skipping");
            }
            Err(e) => bail!("cannot bind {addr}: {e}"),
        }
    }
    if listeners.is_empty() {
        bail!("no listen address to bind ({ips:?})");
    }
    let mut servers = tokio::task::JoinSet::<String>::new();
    // Hook listeners are servers like the others: losing one exits 1. A
    // clean stop releases the holds first and then ends them gracefully.
    let (stop_hooks, hooks_stopped) = tokio::sync::watch::channel(false);
    let max_connections = endpoint::EndpointOptions::default().max_connections;
    tracing::info!(addr = %hook_addr, "hook endpoint listening");
    let (app_tcp, stopped) = (hook_app.clone(), hooks_stopped.clone());
    let stop_now = stop_server_for_tests("hook");
    servers.spawn(async move {
        if stop_now {
            tokio::time::sleep(Duration::from_millis(1000)).await;
        } else {
            let listener = endpoint::capped(hook_listener, max_connections);
            if let Err(e) = axum::serve(listener, app_tcp)
                .with_graceful_shutdown(released(stopped))
                .await
            {
                tracing::error!(error = %e, "hook endpoint stopped");
            }
        }
        "hook endpoint".to_string()
    });
    tracing::info!(path = %hook_sock.display(), "hook socket listening");
    let stop_now = stop_server_for_tests("hook-sock");
    servers.spawn(async move {
        if stop_now {
            tokio::time::sleep(Duration::from_millis(1000)).await;
        } else {
            let listener = endpoint::capped_unix(hook_unix, max_connections);
            if let Err(e) = axum::serve(listener, hook_app)
                .with_graceful_shutdown(released(hooks_stopped))
                .await
            {
                tracing::error!(error = %e, "hook socket stopped");
            }
        }
        "hook socket".to_string()
    });
    let mut hook_servers_left = 2;
    core.set_source_status(
        Source::Hooks,
        SourceStatus {
            state: SourceState::Ok,
            message: None,
        },
    );
    // T5: the credentials follow the store (rotation, retirement, reset)
    // without a restart; retirement runs in its own task, so a CLI holding
    // the writer lock never delays a refresh.
    #[allow(unused_mut)]
    let (mut refresh_every, mut retire_every) = (Duration::from_secs(2), Duration::from_secs(30));
    #[cfg(debug_assertions)]
    if std::env::var_os("ACD_TEST_HOOKS_FAST").is_some() {
        (refresh_every, retire_every) = (Duration::from_millis(200), Duration::from_secs(1));
    }
    let refresh = hook_creds.clone();
    tokio::spawn(async move {
        let mut failing = false;
        loop {
            tokio::time::sleep(refresh_every).await;
            match refresh.reload() {
                Ok(_) => failing = false,
                Err(e) => {
                    if !failing {
                        tracing::error!(error = %e, "hook token store unreadable; keeping the current credentials");
                    }
                    failing = true;
                }
            }
        }
    });
    let (retire_creds, retire_hooks, retire_core) =
        (hook_creds.clone(), hooks.clone(), core.clone());
    tokio::spawn(async move {
        let started = tokio::time::Instant::now();
        loop {
            tokio::time::sleep(retire_every).await;
            // Rejections of the last 10 minutes.
            let now = tokio::time::Instant::now();
            let since = now
                .checked_sub(Duration::from_secs(600))
                .map_or(started, |t| t.max(started));
            let status = retire_once(&retire_creds, &retire_hooks, fresh_within, since).await;
            retire_core.set_source_status(Source::Hooks, status);
        }
    });
    // Set once a stop has begun, or `run` returns on any path, so the teardown
    // of a listener is not logged as a failure.
    let stop_guard = StopFlag::default();
    let stopping = stop_guard.flag();
    for (addr, listener) in listeners {
        tracing::info!(%addr, "listening");
        let app = app.clone();
        let stopping = stopping.clone();
        let mut tls_listener = core_tls::TlsListener::new(
            listener,
            tls_rx.clone(),
            admission.clone(),
            core_tls::Limits::default(),
        );
        let ended = tls_listener.ended();
        servers.spawn(async move {
            let serve = axum::serve(
                tls_listener,
                app.into_make_service_with_connect_info::<core_tls::Admit>(),
            )
            .into_future();
            tokio::select! {
                result = serve => {
                    if let Err(e) = result {
                        tracing::error!(%addr, error = %e, "server stopped");
                    }
                }
                () = ended => {
                    if listener_end_is_failure(&stopping) {
                        tracing::error!(%addr, "TLS listener task ended");
                    }
                }
            }
            format!("listener {addr}")
        });
    }
    let tls_watch = tokio::spawn(crate::tls::watch(dir.to_path_buf(), installed_pin, tls_tx));
    // The same router (and so the same limits) on the Unix socket. Hello and
    // token auth still apply; the Origin check passes trivially because no
    // browser can reach a Unix socket, and there is no address to allowlist.
    let sock = socket_path(dir);
    let (local, local_owned) = local_socket::bind(&lock, &sock)?;
    let local = tokio::net::UnixListener::from_std(local)?;
    tracing::info!(path = %sock.display(), "listening");
    let stop_local = stop_server_for_tests("local");
    servers.spawn(async move {
        if stop_local {
            tokio::time::sleep(Duration::from_millis(1000)).await;
            return "local socket".to_string();
        }
        if let Err(e) = axum::serve(local, app).await {
            tracing::error!(error = %e, "local socket server stopped");
        }
        "local socket".to_string()
    });

    // Token rotation: re-read client.token every 2 s.
    let token_path = dir.join("client.token");
    let rot_auth = auth.clone();
    tokio::spawn(async move {
        let mut current = token;
        let mut invalid_logged = false;
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            match secrets::read_secret(&token_path).and_then(|t| secrets::parse_token(&t)) {
                Ok(t) if t != current => {
                    tracing::info!("client token rotated");
                    rot_auth.rotate(t.clone());
                    current = t;
                    invalid_logged = false;
                }
                Ok(_) => invalid_logged = false,
                Err(e) => {
                    if !invalid_logged {
                        tracing::error!(error = %e, "client token unreadable or invalid; keeping the current token");
                    }
                    invalid_logged = true;
                }
            }
        }
    });

    // Tailscale address changes: exit so the supervisor restarts acd with
    // the new bind set.
    let listen = cfg.listen.clone();
    let watch = tokio::spawn(async move {
        if !needs_probe {
            return std::future::pending().await;
        }
        let mut pending: Option<Vec<IpAddr>> = None;
        let mut failures = 0u32;
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            // A failed probe says nothing about the addresses, so it never
            // counts as a change; a long failure streak exits on its own rule.
            let Some(reported) = bind::tailscale_reported().await else {
                failures = bind::next_failure_count(failures, false);
                if bind::should_exit_on_probe_failures(failures, &ips) {
                    tracing::error!(
                        failures,
                        "`tailscale ip` keeps failing while bound to Tailscale addresses; exiting for restart"
                    );
                    return;
                }
                continue;
            };
            failures = bind::next_failure_count(failures, true);
            let now = bind::resolve_listen(&listen, &reported).unwrap_or_default();
            let (exit, next) = bind::should_exit(pending.as_ref(), &now, &ips);
            if exit {
                tracing::warn!(?now, "listen addresses changed; exiting for restart");
                return;
            }
            pending = next;
        }
    });

    let exit_code = tokio::select! {
        _ = tokio::signal::ctrl_c() => None,
        _ = term.recv() => None,
        _ = watch => Some(EXIT_ADDRESSES_CHANGED),
        Some(done) = servers.join_next() => {
            let done = done.unwrap_or_else(|e| e.to_string());
            if done.starts_with("hook ") {
                hook_servers_left -= 1;
            }
            tracing::error!(server = %done, "a server task stopped; exiting");
            Some(EXIT_SERVER_STOPPED)
        }
    };
    stop_guard.set();
    tls_watch.abort();
    // Fail open: every held hook request gets an empty reply, and the hook
    // servers finish those replies and close before acd exits.
    hooks.release_holds();
    let _ = stop_hooks.send(true);
    let deadline = tokio::time::Instant::now() + HOOK_DRAIN;
    while hook_servers_left > 0 {
        match tokio::time::timeout_at(deadline, servers.join_next()).await {
            Ok(Some(Ok(name))) if name.starts_with("hook ") => hook_servers_left -= 1,
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    // Removes each socket only if it is still ours, then lets a successor in.
    drop((local_owned, hook_sock_owned));
    drop(lock);
    match exit_code {
        Some(code) => std::process::exit(code),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_listener_ending_is_a_failure_only_while_the_daemon_runs() {
        let guard = StopFlag::default();
        let flag = guard.flag();
        assert!(listener_end_is_failure(&flag));
        guard.set();
        assert!(!listener_end_is_failure(&flag));
    }

    #[test]
    fn leaving_run_on_an_error_path_counts_as_a_stop() {
        let guard = StopFlag::default();
        let flag = guard.flag();
        drop(guard);
        assert!(!listener_end_is_failure(&flag));
    }

    #[test]
    fn claude_roots_are_the_recorded_roots_then_the_default() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path().join("home");
        let custom = d.path().join("custom-claude");
        std::fs::create_dir_all(&custom).unwrap();
        let custom = std::fs::canonicalize(&custom).unwrap();
        let mut rec = crate::hooks_install::Record::default();
        rec.files.insert(
            custom.join("settings.json"),
            crate::hooks_install::FileRecord {
                target: crate::hooks_install::Target::Claude,
                created_file: true,
                created_hooks_key: true,
                created_events: vec![],
            },
        );
        rec.save(d.path()).unwrap();
        let roots = claude_roots(d.path(), &home, None);
        assert_eq!(
            roots,
            vec![custom.join("sessions"), home.join(".claude/sessions")]
        );
        let roots = claude_roots(d.path(), &home, Some(&custom));
        assert_eq!(roots, vec![custom.join("sessions")], "deduplicated");
    }

    fn claude_record(rec: &mut crate::hooks_install::Record, dir: &Path, dest: &Path) {
        rec.set(
            dir,
            dest,
            Some(crate::hooks_install::FileRecord {
                target: crate::hooks_install::Target::Claude,
                created_file: true,
                created_hooks_key: true,
                created_events: vec![],
            }),
        );
    }

    #[test]
    fn an_uninstalled_root_is_scanned_while_the_previous_token_may_be_in_use_there() {
        let d = tempfile::tempdir().unwrap();
        crate::secrets::ensure_private_dir(d.path()).unwrap();
        crate::hook_token::ensure(d.path(), false).unwrap();
        let home = d.path().join("home");
        let default = home.join(".claude/sessions");
        let custom = d.path().join("custom/settings.json");
        let mut rec = crate::hooks_install::Record::load(d.path()).unwrap();
        claude_record(&mut rec, d.path(), &custom);
        rec.set(d.path(), &custom, None);
        rec.save(d.path()).unwrap();
        assert_eq!(rec.uninstalled_claude.get(&custom), Some(&1));
        // No previous token: nothing is pending anywhere.
        assert_eq!(claude_roots(d.path(), &home, None), vec![default.clone()]);
        // Generation 1, which the uninstalled root's sessions hold, became previous.
        rotated_store(d.path());
        let with_custom = vec![d.path().join("custom/sessions"), default.clone()];
        assert_eq!(claude_roots(d.path(), &home, None), with_custom);
        // Retiring it is not enough while generation 1 is the oldest accepted;
        // a second rotation makes generation 2 previous and generation 1 gone.
        let held = crate::writer_lock::acquire(d.path()).unwrap();
        let mut s = crate::hook_token::load(d.path()).unwrap();
        s.previous = None;
        s.headers.state = crate::hook_token::HeaderState::Done;
        crate::hook_token::save_locked(d.path(), &held, &s).unwrap();
        crate::hook_token::rotate_store_locked(d.path(), &held, 2_000).unwrap();
        drop(held);
        assert_eq!(claude_roots(d.path(), &home, None), vec![default.clone()]);
        // A later record change prunes it.
        let mut rec = crate::hooks_install::Record::load(d.path()).unwrap();
        claude_record(&mut rec, d.path(), &d.path().join("other/settings.json"));
        assert!(rec.uninstalled_claude.is_empty(), "{rec:?}");
    }

    #[test]
    fn a_reinstalled_root_is_no_longer_uninstalled() {
        let d = tempfile::tempdir().unwrap();
        crate::secrets::ensure_private_dir(d.path()).unwrap();
        crate::hook_token::ensure(d.path(), false).unwrap();
        let custom = d.path().join("custom/settings.json");
        let mut rec = crate::hooks_install::Record::load(d.path()).unwrap();
        claude_record(&mut rec, d.path(), &custom);
        rec.set(d.path(), &custom, None);
        claude_record(&mut rec, d.path(), &custom);
        assert!(rec.uninstalled_claude.is_empty());
        assert_eq!(rec.claude_destinations(), vec![custom]);
    }

    #[test]
    fn unreadable_install_records_make_every_scan_incomplete() {
        let d = tempfile::tempdir().unwrap();
        crate::secrets::ensure_private_dir(d.path()).unwrap();
        let home = d.path().join("home");
        let default = home.join(".claude/sessions");
        std::fs::create_dir_all(&default).unwrap();
        let scan = |roots: Vec<PathBuf>| {
            acd_adapters::hooks::claude_sessions::Scanner::new(
                roots,
                Arc::new(acd_adapters::hooks::claude_sessions::SystemProc),
            )
            .scan()
            .complete
        };
        assert!(
            scan(claude_roots(d.path(), &home, None)),
            "a readable empty root"
        );
        let records = d.path().join(crate::hooks_install::RECORD_FILE);
        crate::secrets::write_structured(&records, "not json").unwrap();
        let roots = claude_roots(d.path(), &home, None);
        let default = std::fs::canonicalize(default).unwrap();
        assert_eq!(roots, vec![PathBuf::from(UNDISCOVERED_ROOT), default]);
        assert!(!scan(roots), "the undiscovered root counts as unreadable");
    }

    fn rotated_store(d: &Path) -> crate::hook_token::Store {
        let held = crate::writer_lock::acquire(d).unwrap();
        crate::hook_token::rotate_store_locked(d, &held, 1_000).unwrap()
    }

    #[test]
    fn credentials_carry_the_previous_expiry_and_a_tag_per_token() {
        let d = tempfile::tempdir().unwrap();
        crate::hook_token::ensure(d.path(), false).unwrap();
        let first = crate::hook_token::load(d.path()).unwrap();
        let mut tags = CredentialTags::default();
        let (cur1, prev1) = tags.credentials(&first);
        assert!(prev1.is_none());
        assert_eq!(cur1.expires_at_ms, None);
        let s = rotated_store(d.path());
        let (cur, prev) = tags.credentials(&s);
        let prev = prev.unwrap();
        assert_eq!(cur.token, s.current);
        assert_eq!(prev.token, s.previous.as_ref().unwrap().token);
        assert_eq!(
            prev.expires_at_ms,
            Some(1_000 + crate::hook_token::MAX_RETENTION_MS)
        );
        assert_eq!(
            prev.generation, cur1.generation,
            "a rotation keeps the old token's tag"
        );
        assert!(cur.generation > prev.generation);
        assert_eq!(tags.credentials(&s).0.generation, cur.generation, "stable");
    }

    #[test]
    fn the_status_counts_only_rejections_after_a_credential_change() {
        let d = tempfile::tempdir().unwrap();
        crate::hook_token::ensure(d.path(), false).unwrap();
        let store = crate::hook_token::load(d.path()).unwrap();
        let creds = HookCredentials::new(d.path(), &store);
        let window = tokio::time::Instant::now();
        creds.auth().note_rejected(Some("pane"));
        creds.reload().unwrap();
        assert_eq!(
            creds.rejected_for_status(window),
            1,
            "an unchanged reload keeps the count"
        );
        rotated_store(d.path());
        creds.reload().unwrap();
        assert_eq!(
            creds.rejected_for_status(window),
            0,
            "a rejection from before the rotation is not counted"
        );
        creds.auth().note_rejected(Some("other pane"));
        assert_eq!(creds.rejected_for_status(window), 1, "one after it is");
        creds.reload().unwrap();
        assert_eq!(creds.rejected_for_status(window), 1, "and stays counted");
    }

    /// A rotation now, so the previous token has not expired.
    fn rotated_now(d: &Path) -> crate::hook_token::Store {
        let held = crate::writer_lock::acquire(d).unwrap();
        let now = chrono::Utc::now().timestamp_millis();
        crate::hook_token::rotate_store_locked(d, &held, now).unwrap()
    }

    fn rotated_creds(d: &Path) -> (HookCredentials, String, String, Option<u64>) {
        crate::hook_token::ensure(d, false).unwrap();
        let s = rotated_now(d);
        let creds = HookCredentials::new(d, &s);
        let (_, (_, previous)) = creds.reload().unwrap();
        let old = s.previous.unwrap().token;
        (creds, old, s.current, previous)
    }

    #[test]
    fn a_retirement_revokes_the_previous_token_without_a_store_read() {
        let d = tempfile::tempdir().unwrap();
        let (creds, old, new, previous) = rotated_creds(d.path());
        assert!(creds.auth().check(&old).is_some());
        // The store becomes unreadable: only the in-memory revocation acts.
        std::fs::remove_file(d.path().join(crate::hook_token::STORE_FILE)).unwrap();
        creds.retired(previous.unwrap());
        assert_eq!(creds.auth().check(&old), None, "revoked at once");
        assert!(creds.auth().check(&new).is_some());
        assert!(creds.reload().is_err());
        assert_eq!(
            creds.auth().check(&old),
            None,
            "a failed reload keeps it revoked"
        );
    }

    #[test]
    fn a_retirement_never_replaces_newer_credentials() {
        let d = tempfile::tempdir().unwrap();
        let (creds, _, middle, previous) = rotated_creds(d.path());
        // A refresh already applied a later rotation: `middle` is now the
        // previous token and must stay accepted.
        let later = rotated_now(d.path());
        creds.reload().unwrap();
        creds.retired(previous.unwrap());
        assert!(creds.auth().check(&middle).is_some());
        assert!(creds.auth().check(&later.current).is_some());
    }

    #[test]
    fn a_reset_store_that_reuses_a_generation_gets_a_new_tag() {
        let d = tempfile::tempdir().unwrap();
        crate::hook_token::ensure(d.path(), false).unwrap();
        let mut tags = CredentialTags::default();
        let s = rotated_store(d.path());
        let (cur, _) = tags.credentials(&s);
        // The same generation with another token (a forced reset).
        let mut reset = s.clone();
        reset.current = crate::secrets::new_token();
        reset.previous = None;
        let (after, _) = tags.credentials(&reset);
        assert_eq!(reset.generation, s.generation);
        assert_ne!(after.generation, cur.generation);
        // A token that left the store and came back is new as well.
        let (back, _) = tags.credentials(&s);
        assert!(back.generation > after.generation);
    }
}
