//! Probes of the documented limits (PROTOCOL.md §1) and of what happens
//! beyond them. Each probe prints `key=value` lines.
use std::path::{Path, PathBuf};
use std::time::Duration;

use acd_client::conn::{Credential, MachineConfig, MachineStatus, spawn_machine};
use acd_client::endpoint::Endpoint;
use acd_protocol::{ClientInfo, ClientMessage, PROTOCOL};
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpSocket, UnixStream};
use tokio_tungstenite::tungstenite::Message;

use crate::args::Args;
use crate::daemon::{Proc, Target};

fn out(k: &str, v: impl std::fmt::Display) {
    println!("{k}={v}");
}

async fn raw_upgrade(sock: &Path) -> Result<(UnixStream, String), String> {
    let mut s = UnixStream::connect(sock).await.map_err(|e| e.to_string())?;
    s.write_all(b"GET / HTTP/1.1\r\nHost: x\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n")
        .await
        .map_err(|e| e.to_string())?;
    let mut buf = [0u8; 64];
    let n = tokio::time::timeout(Duration::from_secs(2), s.read(&mut buf))
        .await
        .map_err(|_| "no reply".to_string())?
        .map_err(|e| e.to_string())?;
    Ok((s, String::from_utf8_lossy(&buf[..n.min(12)]).into_owned()))
}

/// Pre-auth pool of the Unix socket: 8 upgrades without a hello.
async fn unix_preauth(p: &Proc) {
    let mut held = Vec::new();
    let (mut ok, mut refused) = (0, 0);
    for _ in 0..12 {
        match raw_upgrade(&p.acd_sock()).await {
            Ok((s, line)) if line.contains("101") => {
                ok += 1;
                held.push(s);
            }
            Ok((_, line)) => {
                refused += 1;
                if refused == 1 {
                    out("unix_preauth_first_refusal", line.trim());
                }
            }
            Err(e) => out("unix_preauth_error", e),
        }
    }
    out("unix_preauth_upgrades_of_12", format!("accepted={ok} refused={refused}"));
    tokio::time::sleep(Duration::from_secs(6)).await; // hello timeout is 5 s
    let again = raw_upgrade(&p.acd_sock()).await.map(|(_, l)| l.contains("101")).unwrap_or(false);
    out("unix_preauth_slot_free_after_hello_timeout", again);
    drop(held);
}

/// Is this TCP connection still open `wait` after connect?
async fn held_after(src: &str, port: u16, wait: Duration) -> (Option<tokio::net::TcpStream>, bool) {
    let sock = TcpSocket::new_v4().unwrap();
    sock.bind(format!("{src}:0").parse().unwrap()).unwrap();
    let Ok(mut s) = sock.connect(format!("127.0.0.1:{port}").parse().unwrap()).await else {
        return (None, false);
    };
    let mut b = [0u8; 1];
    match tokio::time::timeout(wait, s.read(&mut b)).await {
        Err(_) => (Some(s), true),
        Ok(_) => (None, false),
    }
}

async fn tcp_preauth(p: &Proc) {
    let port = p.ports.0;
    let mut keep = Vec::new();
    let mut held = 0;
    for _ in 0..5 {
        let (s, h) = held_after("127.0.0.1", port, Duration::from_millis(300)).await;
        held += h as usize;
        keep.extend(s);
    }
    out("tcp_one_ip_5_connections", format!("held={held} closed={}", 5 - held));
    drop(keep);
    tokio::time::sleep(Duration::from_secs(1)).await;
    let mut keep = Vec::new();
    let mut held = 0;
    for ip in 2..26 {
        for _ in 0..2 {
            let (s, h) = held_after(&format!("127.0.0.{ip}"), port, Duration::from_millis(300)).await;
            held += h as usize;
            keep.extend(s);
        }
    }
    out("tcp_24_ips_x2_connections", format!("held={held} closed={}", 48 - held));
    tokio::time::sleep(Duration::from_secs(11)).await;
    let mut still = 0;
    for s in &mut keep {
        let mut b = [0u8; 1];
        if tokio::time::timeout(Duration::from_millis(50), s.read(&mut b)).await.is_err() {
            still += 1;
        }
    }
    out("tcp_still_open_after_11s", still);
    drop(keep);
    let (_, h) = held_after("127.0.0.2", port, Duration::from_millis(300)).await;
    out("tcp_new_connection_after_release_is_held", h);
}

/// The hook socket's connection cap (128).
async fn hook_cap(p: &Proc) {
    let token = p.hook_token().unwrap_or_default();
    let mut idle = Vec::new();
    for _ in 0..140 {
        match UnixStream::connect(p.hook_sock()).await {
            Ok(s) => idle.push(s),
            Err(e) => {
                out("hook_connect_error", e);
                break;
            }
        }
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let probe = crate::http::request(&p.hook_sock(), "POST", "/hook/claude/UserPromptSubmit", &token, "{}", Duration::from_secs(3)).await;
    out("hook_141st_connection_with_128_idle", match &probe {
        Ok((c, _)) => format!("answered {c}"),
        Err(e) => format!("no answer ({e})"),
    });
    // How long does the daemon keep an idle connection?
    let t = std::time::Instant::now();
    let mut closed_after = None;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let mut b = [0u8; 1];
        if let Some(s) = idle.first_mut()
            && matches!(tokio::time::timeout(Duration::from_millis(10), s.read(&mut b)).await, Ok(_))
        {
            closed_after = Some(t.elapsed().as_secs_f64());
            break;
        }
    }
    out("hook_idle_connection_closed_after_s", closed_after.map_or("not within 20 s".into(), |s| format!("{s:.1}")));
    drop(idle);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let after = crate::http::request(&p.hook_sock(), "POST", "/hook/claude/UserPromptSubmit", &token, "{}", Duration::from_secs(3)).await;
    out("hook_request_after_release", match &after {
        Ok((c, _)) => format!("answered {c}"),
        Err(e) => format!("no answer ({e})"),
    });
}

type Ws = tokio_tungstenite::WebSocketStream<UnixStream>;

async fn ws_hello(sock: &Path, token: &str, client_id: &str) -> Result<Ws, String> {
    let s = UnixStream::connect(sock).await.map_err(|e| e.to_string())?;
    let cfg = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(None)
        .max_frame_size(None);
    let (mut ws, _) = tokio_tungstenite::client_async_with_config("ws://localhost/", s, Some(cfg))
        .await
        .map_err(|e| e.to_string())?;
    let hello = ClientMessage::Hello {
        protocol: PROTOCOL.into(),
        token: token.into(),
        client: ClientInfo { id: client_id.into(), name: "loadtest".into(), version: "0".into() },
        resume: None,
    };
    ws.send(Message::text(serde_json::to_string(&hello).unwrap())).await.map_err(|e| e.to_string())?;
    Ok(ws)
}

/// Reads until a message of `kind` or the end; returns (its length, the close, what ended it).
async fn read_until(ws: &mut Ws, want: impl Fn(&serde_json::Value) -> bool, limit: Duration) -> (Option<(usize, serde_json::Value)>, String) {
    let end = tokio::time::Instant::now() + limit;
    loop {
        match tokio::time::timeout_at(end, ws.next()).await {
            Err(_) => return (None, "timeout".into()),
            Ok(None) => return (None, "stream ended".into()),
            Ok(Some(Err(e))) => return (None, format!("error: {e}")),
            Ok(Some(Ok(Message::Close(c)))) => {
                return (None, format!("close {:?}", c.map(|c| (u16::from(c.code), c.reason.to_string()))));
            }
            Ok(Some(Ok(Message::Text(t)))) => {
                let n = t.len();
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(t.as_str())
                    && want(&v)
                {
                    return (Some((n, v)), String::new());
                }
            }
            Ok(Some(Ok(_))) => {}
        }
    }
}

async fn frames(p: &Proc, dir: &Path) {
    let token = std::fs::read_to_string(dir.join("client.token")).unwrap_or_default().trim().to_owned();
    // Inbound frames: exactly at the limit and one byte over.
    for (label, total) in [("1MiB", 1usize << 20), ("1MiB+1", (1usize << 20) + 1)] {
        let Ok(mut ws) = ws_hello(&p.acd_sock(), &token, "lt-frames").await else { continue };
        let _ = read_until(&mut ws, |v| v["type"] == "snapshot", Duration::from_secs(3)).await;
        let pad = total - r#"{"type":"ping","id":""}"#.len();
        let ping = format!(r#"{{"type":"ping","id":"{}"}}"#, "p".repeat(pad));
        let _ = ws.send(Message::text(ping)).await;
        let (got, end) = read_until(&mut ws, |v| v["type"] == "pong", Duration::from_secs(3)).await;
        out(&format!("frame_in_{label}_text"), match got {
            Some((n, _)) => format!("pong returned ({n} bytes)"),
            None => format!("no pong: {end}"),
        });
    }
    // Text limits on commands.
    for (label, n) in [("64KiB", 64 * 1024), ("64KiB+1", 64 * 1024 + 1)] {
        let Ok(mut ws) = ws_hello(&p.acd_sock(), &token, "lt-text").await else { continue };
        let _ = read_until(&mut ws, |v| v["type"] == "snapshot", Duration::from_secs(3)).await;
        let msg = ClientMessage::AgentSend { id: format!("c-{label}"), agent_id: "nobody".into(), text: "a".repeat(n) };
        let _ = ws.send(Message::text(serde_json::to_string(&msg).unwrap())).await;
        let (got, end) = read_until(&mut ws, |v| v["type"] == "result", Duration::from_secs(3)).await;
        out(&format!("agent_send_text_{label}"), match got {
            Some((_, v)) => format!("result code={} ", v["error"]["code"].as_str().or(v["code"].as_str()).unwrap_or("?")),
            None => format!("no result: {end}"),
        });
    }
    // client.id over 128 bytes.
    if let Ok(mut ws) = ws_hello(&p.acd_sock(), &token, &"i".repeat(129)).await {
        let (_, end) = read_until(&mut ws, |v| v["type"] == "snapshot", Duration::from_secs(3)).await;
        out("hello_client_id_129_bytes", end);
    }
}

async fn snapshot_bytes(dir: &Path) -> Option<usize> {
    let token = std::fs::read_to_string(dir.join("client.token")).ok()?.trim().to_owned();
    let mut ws = ws_hello(&dir.join("acd.sock"), &token, "lt-snap").await.ok()?;
    read_until(&mut ws, |v| v["type"] == "snapshot", Duration::from_secs(10)).await.0.map(|(n, _)| n)
}

/// A real client against the host with N idle agents: does it sync?
async fn agents_probe(label: &str, n: usize, detail: usize, work: &Path) -> Result<(), String> {
    let dir: PathBuf = work.join(format!("h{n}-{detail}"));
    let mut p = Proc::new(
        Target::Host {
            args: vec![
                "--agents".into(), n.to_string(), "--rate".into(), "0.001".into(),
                "--withdraw-after".into(), "100000".into(), "--turnover".into(), "0".into(),
                "--open-at-start".into(), "1".into(), "--detail-bytes".into(), detail.to_string(),
            ],
        },
        dir.clone(),
    );
    p.init()?;
    p.start().await?;
    tokio::time::sleep(Duration::from_secs(3)).await; // agents register
    let h = spawn_machine(MachineConfig {
        key: "lt".into(),
        instance: 1,
        endpoint: Endpoint::Local { config_dir: dir.clone() },
        credential: Credential::LocalFile(dir.clone()),
        client: ClientInfo { id: "lt-limit".into(), name: "loadtest".into(), version: "0".into() },
        timing: acd_client::Timing::default(),
    });
    let mut ends = h.events();
    let mut resyncs = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut online = false;
    while tokio::time::Instant::now() < deadline {
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(250)) => {}
            Ok((_, acd_client::conn::MachineEvent::ConnectionEnded { .. })) = ends.recv() => resyncs += 1,
        }
        if matches!(h.state().borrow().status, MachineStatus::Online) {
            online = true;
            break;
        }
    }
    let st = h.state().borrow().clone();
    let snap = snapshot_bytes(&dir).await;
    out(&format!("agents_{label}"), format!(
        "n={n} detail_bytes={detail} snapshot_bytes={} client_online={online} client_agents={} connection_ends_in_15s={resyncs} status={:?}",
        snap.map_or("?".into(), |b| b.to_string()), st.session.agents.len(),
        match st.status { MachineStatus::Online => "Online".to_string(), s => format!("{s:?}") }
    ));
    h.cancel();
    p.stop(true);
    Ok(())
}

pub async fn run(a: Args) -> Result<(), String> {
    let which = a.str("only", "all");
    let work = PathBuf::from(a.str("work", "/tmp/lt-limits"));
    let _ = std::fs::remove_dir_all(&work);
    if which == "all" || which == "daemon" {
        let mut p = Proc::new(Target::Acd { bin: a.str("acd-bin", "../../target/release/acd").into() }, work.join("d"));
        p.init()?;
        p.start().await?;
        let dir = work.join("d");
        frames(&p, &dir).await;
        unix_preauth(&p).await;
        tcp_preauth(&p).await;
        hook_cap(&p).await;
        p.stop(true);
    }
    if which == "all" || which == "agents" {
        for (label, n, detail) in [("1000", 1000, 0), ("1001", 1001, 0), ("1100", 1100, 0)] {
            agents_probe(label, n, detail, &work).await?;
        }
        for n in [100, 200, 300, 400, 600, 1000] {
            agents_probe(&format!("big_{n}"), n, 4000, &work).await?;
        }
    }
    Ok(())
}
