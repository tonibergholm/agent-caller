//! Scripted TLS/WebSocket server for client transport fault tests.
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use acd_core::tls;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::{
    net::{TcpListener, UnixListener},
    sync::mpsc,
    time::timeout,
};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Message, protocol::CloseFrame, protocol::frame::coding::CloseCode},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Identity {
    Paired,
    Impostor,
}

#[derive(Clone)]
struct Shared(Arc<Mutex<State>>);

struct State {
    token: String,
    epoch: String,
    welcome_seq: u64,
    caps: Vec<String>,
    handshake_error: Option<(Option<Value>, Option<u16>)>,
    answer_pings: bool,
    accept_delay: Duration,
    welcome_delay: Duration,
    tls_accepts: usize,
    hellos: Vec<Value>,
    received: Vec<Value>,
    connections: usize,
}

enum Out {
    Text(String),
    Close(u16),
}

pub struct FakeConn {
    tx: mpsc::UnboundedSender<Out>,
}

impl FakeConn {
    pub async fn send(&self, v: Value) {
        self.tx
            .send(Out::Text(v.to_string()))
            .expect("fake connection receiver dropped while sending text");
    }

    pub async fn send_raw(&self, s: &str) {
        self.tx
            .send(Out::Text(s.to_owned()))
            .expect("fake connection receiver dropped while sending raw text");
    }

    pub async fn close(&self, code: u16) {
        self.tx
            .send(Out::Close(code))
            .expect("fake connection receiver dropped while sending close");
    }
}

pub struct FakeServer {
    shared: Shared,
    port: u16,
    pin: String,
    ready_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<FakeConn>>,
    ready_tx: mpsc::UnboundedSender<FakeConn>,
    listener_tasks: Vec<tokio::task::JoinHandle<()>>,
    _unix_path: Option<PathBuf>,
}

impl FakeServer {
    pub async fn start(token: &str, identity: Identity) -> FakeServer {
        let paired = tls::generate("acd-client fake paired");
        let served = match identity {
            Identity::Paired => tls::KeyFiles {
                key_pem: paired.key_pem.clone(),
                cert_pem: paired.cert_pem.clone(),
            },
            Identity::Impostor => tls::generate("acd-client fake impostor"),
        };
        let pin = tls::check_pair(&paired).expect("generated key pair is valid");
        let acceptor = tokio_rustls::TlsAcceptor::from(
            tls::server_config(&served).expect("generated server config is valid"),
        );
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake server");
        let port = listener.local_addr().expect("listener address").port();
        let mut server = Self::new(token, port, pin, None);
        server.listener_tasks.push(tokio::spawn(accept_tcp(
            listener,
            acceptor,
            server.shared.clone(),
            server.ready_tx.clone(),
        )));
        server
    }

    pub async fn start_unix(dir: &Path, token: &str) -> FakeServer {
        let path = dir.join("acd.sock");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).expect("bind fake unix socket");
        let mut server = Self::new(token, 0, String::new(), Some(path));
        server.listener_tasks.push(tokio::spawn(accept_unix(
            listener,
            server.shared.clone(),
            server.ready_tx.clone(),
        )));
        server
    }

    fn new(token: &str, port: u16, pin: String, path: Option<PathBuf>) -> FakeServer {
        let (tx, rx) = mpsc::unbounded_channel();
        let state = State {
            token: token.to_owned(),
            epoch: "e1".into(),
            welcome_seq: 0,
            caps: ["answer", "send", "mark_seen"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            handshake_error: None,
            answer_pings: true,
            accept_delay: Duration::ZERO,
            welcome_delay: Duration::ZERO,
            tls_accepts: 0,
            hellos: Vec::new(),
            received: Vec::new(),
            connections: 0,
        };
        FakeServer {
            shared: Shared(Arc::new(Mutex::new(state))),
            port,
            pin,
            ready_rx: tokio::sync::Mutex::new(rx),
            ready_tx: tx,
            listener_tasks: Vec::new(),
            _unix_path: path,
        }
    }

    pub fn set_token(&self, token: &str) {
        self.shared.0.lock().unwrap().token = token.to_owned();
    }
    pub fn port(&self) -> u16 {
        self.port
    }
    pub fn pin(&self) -> String {
        self.pin.clone()
    }
    pub fn set_epoch(&self, epoch: &str) {
        self.shared.0.lock().unwrap().epoch = epoch.to_owned();
    }
    pub fn set_welcome_seq(&self, seq: u64) {
        self.shared.0.lock().unwrap().welcome_seq = seq;
    }
    pub fn set_capabilities(&self, caps: &[&str]) {
        self.shared.0.lock().unwrap().caps = caps.iter().map(|s| (*s).to_owned()).collect();
    }
    pub fn set_handshake_error(&self, frame: Option<Value>, close: Option<u16>) {
        self.shared.0.lock().unwrap().handshake_error = Some((frame, close));
    }
    pub fn set_answer_pings(&self, on: bool) {
        self.shared.0.lock().unwrap().answer_pings = on;
    }
    pub fn set_accept_delay(&self, d: Duration) {
        self.shared.0.lock().unwrap().accept_delay = d;
    }
    pub fn set_welcome_delay(&self, d: Duration) {
        self.shared.0.lock().unwrap().welcome_delay = d;
    }
    pub async fn next_connection(&self) -> FakeConn {
        timeout(Duration::from_secs(5), self.ready_rx.lock().await.recv())
            .await
            .expect("timed out waiting for fake connection")
            .expect("fake server stopped")
    }
    pub fn tls_accepts(&self) -> usize {
        self.shared.0.lock().unwrap().tls_accepts
    }
    pub fn hellos(&self) -> Vec<Value> {
        self.shared.0.lock().unwrap().hellos.clone()
    }
    pub fn received(&self) -> Vec<Value> {
        self.shared.0.lock().unwrap().received.clone()
    }
    pub fn connections(&self) -> usize {
        self.shared.0.lock().unwrap().connections
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        for task in &self.listener_tasks {
            task.abort();
        }
    }
}

async fn accept_tcp(
    listener: TcpListener,
    acceptor: tokio_rustls::TlsAcceptor,
    shared: Shared,
    ready: mpsc::UnboundedSender<FakeConn>,
) {
    while let Ok((stream, _)) = listener.accept().await {
        let acceptor = acceptor.clone();
        let shared = shared.clone();
        let ready = ready.clone();
        tokio::spawn(async move {
            let Ok(stream) = acceptor.accept(stream).await else {
                return;
            };
            shared.0.lock().unwrap().tls_accepts += 1;
            serve_ws(tokio_tungstenite::accept_async(stream).await, shared, ready).await;
        });
    }
}

async fn accept_unix(
    listener: UnixListener,
    shared: Shared,
    ready: mpsc::UnboundedSender<FakeConn>,
) {
    while let Ok((stream, _)) = listener.accept().await {
        let shared = shared.clone();
        let ready = ready.clone();
        tokio::spawn(async move {
            serve_ws(tokio_tungstenite::accept_async(stream).await, shared, ready).await;
        });
    }
}

async fn serve_ws<S>(
    ws: Result<WebSocketStream<S>, tokio_tungstenite::tungstenite::Error>,
    shared: Shared,
    ready: mpsc::UnboundedSender<FakeConn>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let Ok(mut ws) = ws else {
        return;
    };
    let delay = shared.0.lock().unwrap().accept_delay;
    tokio::time::sleep(delay).await;
    let Some(Ok(Message::Text(text))) = ws.next().await else {
        return;
    };
    let hello: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let state_snapshot = {
        let mut s = shared.0.lock().unwrap();
        s.hellos.push(hello.clone());
        s.connections += 1;
        (
            s.token.clone(),
            s.epoch.clone(),
            s.welcome_seq,
            s.caps.clone(),
            s.handshake_error.clone(),
            s.welcome_delay,
        )
    };
    let (tx, mut out) = mpsc::unbounded_channel();
    let (token, epoch, seq, caps, failure, welcome_delay) = state_snapshot;
    let response = if let Some((frame, close)) = failure {
        if let Some(frame) = frame {
            let _ = ws.send(Message::Text(frame.to_string().into())).await;
        }
        if let Some(code) = close {
            send_close(&mut ws, code).await;
        }
        let _ = ready.send(FakeConn { tx });
        return;
    } else if hello["token"] == token {
        json!({"type":"welcome","machine":{"id":"fake","name":"Fake","acd_version":"0.1.0","sources":{"hooks":{"state":"unconfigured"},"herdr":{"state":"unconfigured"},"t3":{"state":"unconfigured"}}},"capabilities":caps,"epoch":epoch,"seq":seq})
    } else {
        json!({"type":"error","code":"unauthorized","message":"unauthorized"})
    };
    tokio::time::sleep(welcome_delay).await;
    let _ = ws.send(Message::Text(response.to_string().into())).await;
    let _ = ready.send(FakeConn { tx });
    let unauthorized = hello["token"] != token;
    if unauthorized {
        send_close(&mut ws, 4401).await;
        return;
    }
    loop {
        tokio::select! {
            Some(msg) = out.recv() => match msg {
                Out::Text(s) => if ws.send(Message::Text(s.into())).await.is_err() { break; },
                Out::Close(code) => { send_close(&mut ws, code).await; break; }
            },
            incoming = ws.next() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    let parsed = serde_json::from_str(&text).unwrap_or(Value::Null);
                    let (answer, tx) = { let mut s = shared.0.lock().unwrap(); s.received.push(parsed.clone()); (s.answer_pings, parsed["id"].clone()) };
                    if answer && parsed["type"] == "ping" { let _ = ws.send(Message::Text(json!({"type":"pong","id":tx}).to_string().into())).await; }
                }
                Some(Ok(Message::Ping(v))) => { let _ = ws.send(Message::Pong(v)).await; }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            }
        }
    }
}

async fn send_close<S>(ws: &mut WebSocketStream<S>, code: u16)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let _ = ws
        .send(Message::Close(Some(CloseFrame {
            code: CloseCode::from(code),
            reason: "".into(),
        })))
        .await;
}

pub fn cfg_for(f: &FakeServer, token: &str, timing: crate::Timing) -> crate::conn::MachineConfig {
    use crate::{
        conn::{Credential, MachineConfig},
        endpoint::Endpoint,
    };
    MachineConfig {
        key: "fake".into(),
        instance: 1,
        endpoint: Endpoint::Tcp {
            host: "127.0.0.1".into(),
            port: f.port(),
            pin: f.pin(),
        },
        credential: Credential::Token(token.into()),
        client: acd_protocol::ClientInfo {
            id: "11111111-1111-4111-8111-111111111111".into(),
            name: "test".into(),
            version: "0".into(),
        },
        timing,
    }
}

pub fn local_cfg_for(dir: &Path, timing: crate::Timing) -> crate::conn::MachineConfig {
    use crate::{
        conn::{Credential, MachineConfig},
        endpoint::Endpoint,
    };
    MachineConfig {
        key: "local".into(),
        instance: 1,
        endpoint: Endpoint::Local {
            config_dir: dir.to_owned(),
        },
        credential: Credential::LocalFile(dir.to_owned()),
        client: acd_protocol::ClientInfo {
            id: "11111111-1111-4111-8111-111111111111".into(),
            name: "test".into(),
            version: "0".into(),
        },
        timing,
    }
}
