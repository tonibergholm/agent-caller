use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::time::Duration;

use acd_protocol::{Agent, ClientInfo, ClientMessage, ServerMessage, PROTOCOL};
use anyhow::{bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::{config::Config, secrets};

pub async fn status(dir: &Path) -> Result<()> {
    let cfg = Config::load(dir)?;
    let token = secrets::read_secret(&dir.join("client.token"))?;
    let host = loopback_addr(&cfg.listen)?;
    let url = format!("ws://{}/", SocketAddr::new(host, cfg.port));
    let (mut ws, _) = tokio::time::timeout(Duration::from_secs(3), connect_async(&url))
        .await
        .context("timed out connecting to the local acd")?
        .context("cannot connect to the local acd; is `acd run` running?")?;
    let hello = ClientMessage::Hello {
        protocol: PROTOCOL.into(),
        token,
        client: ClientInfo { id: "acd-status".into(), name: "acd status".into(), version: env!("CARGO_PKG_VERSION").into() },
        resume: None,
    };
    ws.send(Message::text(serde_json::to_string(&hello)?)).await?;
    tokio::time::timeout(Duration::from_secs(5), read_status(&mut ws))
        .await
        .context("timed out waiting for the daemon's snapshot")?
}

/// First loopback address the config listens on. The token is only ever sent
/// to an address acd itself binds.
fn loopback_addr(listen: &[String]) -> Result<IpAddr> {
    let loopback: Vec<String> = listen
        .iter()
        .filter(|e| e.as_str() == "loopback" || e.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback()))
        .cloned()
        .collect();
    crate::bind::resolve_listen(&loopback, &[])?
        .into_iter()
        .next()
        .context("acd status needs a loopback listen address")
}

async fn read_status<S>(ws: &mut S) -> Result<()>
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let mut welcome = None;
    while let Some(frame) = ws.next().await {
        let Message::Text(t) = frame? else { continue };
        match serde_json::from_str::<ServerMessage>(t.as_str())? {
            ServerMessage::Welcome { machine, epoch, .. } => welcome = Some((machine, epoch)),
            ServerMessage::Snapshot { agents, .. } => {
                let Some((machine, epoch)) = welcome else { bail!("snapshot before welcome") };
                println!("machine: {} ({})  epoch {}", machine.name, machine.id, epoch);
                println!("sources: hooks={:?} herdr={:?} t3={:?}", machine.sources.hooks.state, machine.sources.herdr.state, machine.sources.t3.state);
                println!("agents: {}", agents.len());
                for a in &agents { println!("  {}", line(a)); }
                return Ok(());
            }
            ServerMessage::Error { message, .. } => bail!("daemon refused: {message}"),
            _ => {}
        }
    }
    bail!("connection closed before snapshot")
}

fn line(a: &Agent) -> String {
    let pending = a.pending.as_ref().map(|r| format!("  [{}: {} {}]", r.id, r.title, r.detail)).unwrap_or_default();
    format!("{:?} {:?} {} {}{}", a.state, a.kind, a.title, a.cwd.as_deref().unwrap_or(""), pending)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_requires_a_loopback_entry() {
        assert_eq!(loopback_addr(&["loopback".into(), "tailscale".into()]).unwrap().to_string(), "127.0.0.1");
        assert_eq!(loopback_addr(&["::1".into()]).unwrap().to_string(), "::1");
        let err = loopback_addr(&["tailscale".into()]).unwrap_err().to_string();
        assert!(err.contains("acd status needs a loopback listen address"), "{err}");
    }
}
