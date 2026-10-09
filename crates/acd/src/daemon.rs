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

/// Claude registry roots: the roots of recorded Claude installs, then the
/// default root, deduplicated (D24). Read again before every scan.
pub fn claude_roots(
    config_dir: &Path,
    home: &Path,
    claude_config_dir: Option<&Path>,
) -> Vec<PathBuf> {
    let mut roots = crate::hooks_install::recorded_claude_roots(config_dir);
    let default = acd_adapters::hooks::claude_sessions::sessions_dir(home, claude_config_dir);
    let default = std::fs::canonicalize(default.parent().unwrap_or(&default))
        .map(|p| p.join("sessions"))
        .unwrap_or(default);
    if !roots.contains(&default) {
        roots.push(default);
    }
    roots
}

pub fn hook_credentials(s: &crate::hook_token::Store) -> (Credential, Option<Credential>) {
    (
        Credential {
            token: s.current.clone(),
            generation: s.generation,
        },
        s.previous.as_ref().map(|p| Credential {
            token: p.token.clone(),
            generation: p.generation,
        }),
    )
}

/// Debug builds only: `ACD_TEST_STOP_SERVER=<name>` ends that server task
/// shortly after it starts, to test supervision.
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
    // Task 31 drives token retirement from the scan interval.
    let _scan_interval = hooks_cfg.scan_interval;
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
    // D13: the hook endpoint binds first, so a taken hook port fails startup
    // before any socket file exists. One router serves both hook listeners,
    // so they share the hold cap and hold ids.
    let (cur, prev) = hook_credentials(&store);
    let hook_auth = Arc::new(HookAuth::new(cur.clone()));
    hook_auth.set(cur, prev);
    let hook_addr = SocketAddr::from(([127, 0, 0, 1], cfg.hook_port));
    let hook_listener = tokio::net::TcpListener::bind(hook_addr)
        .await
        .map_err(|e| anyhow::anyhow!("cannot bind hook endpoint {hook_addr}: {e}"))?;
    let hook_sock = hook_socket_path(dir);
    let (hook_unix, hook_sock_owned) = local_socket::bind(&lock, &hook_sock)?;
    let hook_unix = tokio::net::UnixListener::from_std(hook_unix)?;
    let hook_app = hooks.router(hook_auth.clone());

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
            tokio::time::sleep(Duration::from_millis(100)).await;
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
            tokio::time::sleep(Duration::from_millis(100)).await;
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
    // Task 31 refreshes the credentials from the store.
    let _hook_auth = hook_auth;
    for (addr, listener) in listeners {
        tracing::info!(%addr, "listening");
        let app = app.clone();
        let tls_listener = core_tls::TlsListener::new(
            listener,
            tls_rx.clone(),
            admission.clone(),
            core_tls::Limits::default(),
        );
        servers.spawn(async move {
            if let Err(e) = axum::serve(
                tls_listener,
                app.into_make_service_with_connect_info::<core_tls::Admit>(),
            )
            .await
            {
                tracing::error!(%addr, error = %e, "server stopped");
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
            tokio::time::sleep(Duration::from_millis(100)).await;
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

    #[test]
    fn hook_credentials_follow_the_store_generations() {
        let d = tempfile::tempdir().unwrap();
        crate::hook_token::ensure(d.path(), false).unwrap();
        let held = crate::writer_lock::acquire(d.path()).unwrap();
        let s = crate::hook_token::rotate_store_locked(d.path(), &held, 1_000).unwrap();
        let (cur, prev) = hook_credentials(&s);
        assert_eq!(
            (cur.generation, prev.as_ref().map(|p| p.generation)),
            (2, Some(1))
        );
        assert_eq!(cur.token, s.current);
        assert_eq!(prev.unwrap().token, s.previous.unwrap().token);
    }
}
