use std::path::Path;
use std::time::Duration;

use acd_protocol::{Agent, ClientInfo, ClientMessage, PROTOCOL, ServerMessage, decode_server};
use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{client_async, tungstenite::Message};

use crate::{daemon::socket_path, secrets};

/// Connects through `<config_dir>/acd.sock` only. The token never goes over
/// TCP, where any local process holding the port could receive it.
pub async fn status(dir: &Path) -> Result<()> {
    crate::preflight(dir)?;
    let token = secrets::read_secret(&dir.join("client.token"))?;
    let sock = socket_path(dir);
    let connected = tokio::time::timeout(
        Duration::from_secs(3),
        tokio::net::UnixStream::connect(&sock),
    )
    .await;
    if !matches!(connected, Ok(Ok(_)))
        && let Ok(cfg) = crate::config::Config::load(dir)
        && let Some(w) = hook_port_warning(dir, cfg.hook_port)
    {
        eprintln!("warning: {w}");
    }
    let stream = connected
        .context("timed out connecting to the local acd")?
        .with_context(|| {
            format!(
                "cannot connect to {}; is `acd run` running?",
                sock.display()
            )
        })?;
    let (mut ws, _) = tokio::time::timeout(
        Duration::from_secs(3),
        client_async("ws://localhost/", stream),
    )
    .await
    .context("timed out connecting to the local acd")?
    .context("WebSocket handshake with the local acd failed")?;
    let hello = ClientMessage::Hello {
        protocol: PROTOCOL.into(),
        token,
        client: ClientInfo {
            id: "acd-status".into(),
            name: "acd status".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
        resume: None,
    };
    ws.send(Message::text(serde_json::to_string(&hello)?))
        .await?;
    tokio::time::timeout(Duration::from_secs(5), read_status(&mut ws))
        .await
        .context("timed out waiting for the daemon's snapshot")?
}

/// Best effort (amendment A1): when acd is not running but something holds
/// the hook port, Claude would send that process its hooks and the token.
/// Connects and test-binds the port; never sends a byte. Every probe is
/// bounded, so a saturated `acd.sock` cannot hang the caller.
pub fn hook_port_warning(dir: &Path, hook_port: u16) -> Option<String> {
    // acd counts as down only when nothing listens on acd.sock. A connect
    // that succeeds or times out (a listener with a full backlog) means
    // something is serving there.
    match crate::local_socket::connect_within(&socket_path(dir), Duration::from_millis(500)) {
        Ok(()) => return None,
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => return None,
        Err(_) => {}
    }
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], hook_port));
    let held = std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok()
        || std::net::TcpListener::bind(addr)
            .err()
            .is_some_and(|e| e.kind() == std::io::ErrorKind::AddrInUse);
    held.then(|| {
        format!(
            "{addr} (hook_port) is held by another process while acd is not running; Claude hooks would send it the hook token and payloads, and it could answer Claude permission prompts. Stop that process, then start acd."
        )
    })
}

async fn read_status<S>(ws: &mut S) -> Result<()>
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let mut welcome = None;
    while let Some(frame) = ws.next().await {
        let Message::Text(t) = frame? else { continue };
        // A newer daemon may send message types this version does not know.
        let Some(msg) = decode_server(t.as_str())? else {
            continue;
        };
        match msg {
            ServerMessage::Welcome { machine, epoch, .. } => welcome = Some((machine, epoch)),
            ServerMessage::Snapshot { agents, .. } => {
                let Some((machine, epoch)) = welcome else {
                    bail!("snapshot before welcome")
                };
                println!(
                    "machine: {} ({})  epoch {}",
                    machine.name, machine.id, epoch
                );
                println!(
                    "sources: hooks={:?} herdr={:?} t3={:?}",
                    machine.sources.hooks.state,
                    machine.sources.herdr.state,
                    machine.sources.t3.state
                );
                for (name, source) in [
                    ("hooks", &machine.sources.hooks),
                    ("herdr", &machine.sources.herdr),
                    ("t3", &machine.sources.t3),
                ] {
                    if let Some(message) = &source.message {
                        println!("  {name}: {message}");
                    }
                }
                println!("agents: {}", agents.len());
                for a in &agents {
                    println!("  {}", line(a));
                }
                return Ok(());
            }
            ServerMessage::Error { message, .. } => bail!("daemon refused: {message}"),
            _ => {}
        }
    }
    bail!("connection closed before snapshot")
}

fn line(a: &Agent) -> String {
    let pending = a
        .pending
        .as_ref()
        .map(|r| format!("  [{}: {} {}]", r.id, r.title, r.detail))
        .unwrap_or_default();
    format!(
        "{:?} {:?} {} {}{}",
        a.state,
        a.kind,
        a.title,
        a.cwd.as_deref().unwrap_or(""),
        pending
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unknown_message_types_are_skipped() {
        let welcome = include_str!("../../acd-protocol/examples/server/welcome.json");
        let snapshot = r#"{"type":"snapshot","epoch":"e1","seq":42,"agents":[]}"#;
        let raw = [welcome, r#"{"type":"agent.launched","x":1}"#, snapshot];
        let mut ws = futures_util::stream::iter(raw.map(|t| Ok(Message::text(t))));
        read_status(&mut ws).await.unwrap();
    }
}
