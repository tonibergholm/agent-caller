use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use acd_core::{router, Auth, Core, ServerOptions};
use acd_protocol::{Machine, SourceStatus, Sources};
use anyhow::{bail, Result};

use crate::{bind, config::Config, identity, secrets};

/// Exit code that asks the supervisor (launchd or systemd) to restart acd
/// after the Tailscale addresses change.
pub const EXIT_ADDRESSES_CHANGED: i32 = 75;

pub async fn run(dir: &Path) -> Result<()> {
    crate::preflight(dir)?;
    let cfg = Config::load(dir)?;
    let token = secrets::read_secret(&dir.join("client.token"))?;
    let machine = Machine {
        id: identity::machine_id(dir)?,
        name: cfg.machine_name.clone(),
        acd_version: env!("CARGO_PKG_VERSION").into(),
        sources: Sources { hooks: SourceStatus::unconfigured(), herdr: SourceStatus::unconfigured(), t3: SourceStatus::unconfigured() },
    };
    let core = Core::new(machine, vec![]);
    let auth = Arc::new(Auth::new(token.clone()));
    let app = router(core, auth.clone(), ServerOptions { allowed_origins: cfg.allowed_origins.clone(), ..Default::default() });

    let reported = bind::tailscale_reported();
    let ips = bind::resolve_listen(&cfg.listen, &reported)?;
    let mut bound = 0;
    for ip in &ips {
        let addr = SocketAddr::new(*ip, cfg.port);
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                tracing::info!(%addr, "listening");
                let app = app.clone();
                tokio::spawn(async move {
                    if let Err(e) = axum::serve(listener, app).await { tracing::error!(%addr, error = %e, "server stopped"); }
                });
                bound += 1;
            }
            Err(e) => tracing::warn!(%addr, error = %e, "could not bind"),
        }
    }
    if bound == 0 { bail!("could not bind any listen address ({ips:?})"); }

    // Token rotation: re-read client.token every 2 s.
    let token_path = dir.join("client.token");
    let rot_auth = auth.clone();
    tokio::spawn(async move {
        let mut current = token;
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            match secrets::read_secret(&token_path) {
                Ok(t) if t != current => { tracing::info!("client token rotated"); rot_auth.rotate(t.clone()); current = t; }
                Ok(_) => {}
                Err(e) => tracing::error!(error = %e, "cannot read client token"),
            }
        }
    });

    // Tailscale address changes: exit so the supervisor restarts acd with
    // the new bind set.
    let listen = cfg.listen.clone();
    let watch = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            let now: Vec<IpAddr> = bind::resolve_listen(&listen, &bind::tailscale_reported()).unwrap_or_default();
            if now != ips { tracing::warn!(?now, "listen addresses changed; exiting for restart"); return; }
        }
    });

    tokio::select! {
        _ = tokio::signal::ctrl_c() => Ok(()),
        _ = watch => std::process::exit(EXIT_ADDRESSES_CHANGED),
    }
}
