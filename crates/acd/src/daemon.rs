use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use acd_core::{router, Auth, Core, ServerOptions};
use acd_protocol::{Machine, SourceStatus, Sources};
use anyhow::{bail, Context, Result};

use crate::{bind, config::Config, identity, secrets};

/// Exit code that asks the supervisor (launchd or systemd) to restart acd
/// after the Tailscale addresses change.
pub const EXIT_ADDRESSES_CHANGED: i32 = 75;

/// The Unix socket in the config dir that local tools such as `acd status` use.
pub fn socket_path(dir: &Path) -> PathBuf { dir.join("acd.sock") }

pub async fn run(dir: &Path) -> Result<()> {
    crate::preflight(dir)?;
    let cfg = Config::load(dir)?;
    let token = secrets::read_secret(&dir.join("client.token"))
        .and_then(|t| secrets::parse_token(&t))
        .map_err(|e| anyhow::anyhow!("client.token is invalid ({e}); run `acd token rotate client`"))?;
    let machine = Machine {
        id: identity::machine_id(dir)?,
        name: cfg.machine_name.clone(),
        acd_version: env!("CARGO_PKG_VERSION").into(),
        sources: Sources { hooks: SourceStatus::unconfigured(), herdr: SourceStatus::unconfigured(), t3: SourceStatus::unconfigured() },
    };
    let core = Core::new(machine, vec![]);
    let auth = Arc::new(Auth::new(token.clone()));
    let app = router(core, auth.clone(), ServerOptions { allowed_origins: cfg.allowed_origins.clone(), ..Default::default() });

    let probe = bind::tailscale_reported().await;
    let listen_includes_tailscale = cfg.listen.iter().any(|e| e == "tailscale");
    let reported = probe.clone().unwrap_or_default();
    let ips = bind::resolve_listen(&cfg.listen, &reported)?;
    if listen_includes_tailscale {
        if probe.is_none() {
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
            Err(e) if e.kind() == std::io::ErrorKind::AddrNotAvailable && *ip == IpAddr::V6(Ipv6Addr::LOCALHOST) => {
                tracing::warn!(%addr, error = %e, "IPv6 loopback unavailable; skipping");
            }
            Err(e) => bail!("cannot bind {addr}: {e}"),
        }
    }
    if listeners.is_empty() { bail!("no listen address to bind ({ips:?})"); }
    for (addr, listener) in listeners {
        tracing::info!(%addr, "listening");
        let app = app.clone();
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await { tracing::error!(%addr, error = %e, "server stopped"); }
        });
    }
    // The same router (and so the same limits) on the Unix socket. Hello and
    // token auth still apply; the Origin check passes trivially because no
    // browser can reach a Unix socket, and there is no address to allowlist.
    let sock = socket_path(dir);
    let local = bind_local_socket(&sock)?;
    tracing::info!(path = %sock.display(), "listening");
    tokio::spawn(async move {
        if let Err(e) = axum::serve(local, app).await { tracing::error!(error = %e, "local socket server stopped"); }
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
                Ok(t) if t != current => { tracing::info!("client token rotated"); rot_auth.rotate(t.clone()); current = t; invalid_logged = false; }
                Ok(_) => invalid_logged = false,
                Err(e) => {
                    if !invalid_logged { tracing::error!(error = %e, "client token unreadable or invalid; keeping the current token"); }
                    invalid_logged = true;
                }
            }
        }
    });

    // Tailscale address changes: exit so the supervisor restarts acd with
    // the new bind set.
    let listen = cfg.listen.clone();
    let watch = tokio::spawn(async move {
        let mut pending: Option<Vec<IpAddr>> = None;
        let mut failures = 0u32;
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            // A failed probe says nothing about the addresses, so it never
            // counts as a change; a long failure streak exits on its own rule.
            let Some(reported) = bind::tailscale_reported().await else {
                failures = bind::next_failure_count(failures, false);
                if bind::should_exit_on_probe_failures(failures, &ips) {
                    tracing::error!(failures, "`tailscale ip` keeps failing while bound to Tailscale addresses; exiting for restart");
                    return;
                }
                continue;
            };
            failures = bind::next_failure_count(failures, true);
            let now = bind::resolve_listen(&listen, &reported).unwrap_or_default();
            let (exit, next) = bind::should_exit(pending.as_ref(), &now, &ips);
            if exit { tracing::warn!(?now, "listen addresses changed; exiting for restart"); return; }
            pending = next;
        }
    });

    let exit_code = tokio::select! {
        _ = tokio::signal::ctrl_c() => None,
        _ = watch => Some(EXIT_ADDRESSES_CHANGED),
    };
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
            std::fs::remove_file(path).with_context(|| format!("removing stale {}", path.display()))?
        }
        Ok(_) => bail!("{} exists and is not a socket; remove it", path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    }
    let listener = tokio::net::UnixListener::bind(path).with_context(|| format!("cannot bind {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}
