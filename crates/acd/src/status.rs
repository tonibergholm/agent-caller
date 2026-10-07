use std::path::Path;

use acd_protocol::{Agent, ClientInfo, ClientMessage, ServerMessage, PROTOCOL};
use anyhow::{bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::{config::Config, secrets};

pub async fn status(dir: &Path) -> Result<()> {
    let cfg = Config::load(dir)?;
    let token = secrets::read_secret(&dir.join("client.token"))?;
    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{}/", cfg.port))
        .await
        .context("cannot connect to the local acd; is `acd run` running?")?;
    let hello = ClientMessage::Hello {
        protocol: PROTOCOL.into(),
        token,
        client: ClientInfo { id: "acd-status".into(), name: "acd status".into(), version: env!("CARGO_PKG_VERSION").into() },
        resume: None,
    };
    ws.send(Message::text(serde_json::to_string(&hello)?)).await?;
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
