use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use acd_core::{Auth, Core, ServerOptions, router, tls as core_tls};
use acd_protocol::{Machine, SourceStatus, Sources};
use anyhow::{Context, Result, bail};

use crate::{bind, config::Config, identity, secrets};

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
        eprintln!("acd: panic; exiting so the supervisor restarts acd");
        std::process::exit(EXIT_PANIC);
    }));
}

/// The Unix socket in the config dir that local tools such as `acd status` use.
pub fn socket_path(dir: &Path) -> PathBuf {
    dir.join("acd.sock")
}

pub async fn run(dir: &Path) -> Result<()> {
    exit_on_panic();
    #[cfg(debug_assertions)]
    if let Some(ms) = std::env::var("ACD_TEST_PANIC_AFTER_MS").ok().and_then(|s| s.parse::<u64>().ok()) {
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            panic!("test panic");
        });
    }
    crate::preflight(dir)?;
    let cfg = Config::load(dir)?;
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
    let core = Core::new(machine, vec![herdr.clone(), t3.clone()]);
    herdr.start(core.clone());
    t3.start(core.clone());
    let auth = Arc::new(Auth::new(token.clone()));
    let app = router(
        core,
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
    let local = bind_local_socket(&sock)?;
    tracing::info!(path = %sock.display(), "listening");
    #[cfg(debug_assertions)]
    let stop_local = std::env::var("ACD_TEST_STOP_SERVER").is_ok_and(|v| v == "local");
    servers.spawn(async move {
        #[cfg(debug_assertions)]
        if stop_local {
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

    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let exit_code = tokio::select! {
        _ = tokio::signal::ctrl_c() => None,
        _ = term.recv() => None,
        _ = watch => Some(EXIT_ADDRESSES_CHANGED),
        Some(done) = servers.join_next() => {
            tracing::error!(server = %done.unwrap_or_else(|e| e.to_string()), "a server task stopped; exiting");
            Some(EXIT_SERVER_STOPPED)
        }
    };
    tls_watch.abort();
    let _ = std::fs::remove_file(&sock);
    match exit_code {
        Some(code) => std::process::exit(code),
        None => Ok(()),
    }
}

/// Binds `path` with mode 0600, replacing a socket left by an earlier run.
/// Called only after every TCP listener is bound, and refuses a socket that
/// still accepts connections, so a second `acd run` never takes over a live
/// daemon's socket.
fn bind_local_socket(path: &Path) -> Result<tokio::net::UnixListener> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_socket() => {
            if std::os::unix::net::UnixStream::connect(path).is_ok() {
                bail!("another acd is running on {}", path.display());
            }
            std::fs::remove_file(path)
                .with_context(|| format!("removing stale {}", path.display()))?
        }
        Ok(_) => bail!("{} exists and is not a socket; remove it", path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    }
    let listener = tokio::net::UnixListener::bind(path)
        .with_context(|| format!("cannot bind {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}
