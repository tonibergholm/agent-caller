use std::sync::{Arc, RwLock};
use std::time::Duration;

use acd_protocol::*;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{header::ORIGIN, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use subtle::ConstantTimeEq;
use tokio::sync::{broadcast, mpsc, OwnedSemaphorePermit, Semaphore};

use crate::engine::Core;

pub const CLOSE_BAD_PROTOCOL: u16 = 4400;
pub const CLOSE_UNAUTHORIZED: u16 = 4401;
pub const CLOSE_TOO_SLOW: u16 = 4408;

/// The client token. `generation` increments on rotation, and sessions
/// authenticated under an older generation are closed.
pub struct Auth {
    token: RwLock<(u64, String)>,
}

impl Auth {
    pub fn new(token: String) -> Self { Self { token: RwLock::new((0, token)) } }
    pub fn rotate(&self, token: String) {
        let mut g = self.token.write().unwrap();
        g.0 += 1;
        g.1 = token;
    }
    pub fn generation(&self) -> u64 { self.token.read().unwrap().0 }
    fn check(&self, presented: &str) -> Option<u64> {
        let g = self.token.read().unwrap();
        bool::from(g.1.as_bytes().ct_eq(presented.as_bytes())).then_some(g.0)
    }
}

#[derive(Clone)]
pub struct ServerOptions {
    pub allowed_origins: Vec<String>,
    pub hello_timeout: Duration,
    pub max_frame: usize,
    pub max_preauth: usize,
    pub rotation_check: Duration,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            allowed_origins: vec![],
            hello_timeout: Duration::from_secs(5),
            max_frame: 1 << 20,
            max_preauth: 8,
            rotation_check: Duration::from_secs(1),
        }
    }
}

#[derive(Clone)]
struct AppState {
    core: Arc<Core>,
    auth: Arc<Auth>,
    opts: ServerOptions,
    preauth: Arc<Semaphore>,
}

pub fn router(core: Arc<Core>, auth: Arc<Auth>, opts: ServerOptions) -> Router {
    let preauth = Arc::new(Semaphore::new(opts.max_preauth));
    Router::new().route("/", get(upgrade)).with_state(AppState { core, auth, opts, preauth })
}

async fn upgrade(State(st): State<AppState>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    if let Some(origin) = headers.get(ORIGIN) {
        let allowed = origin.to_str().is_ok_and(|o| st.opts.allowed_origins.iter().any(|a| a == o));
        if !allowed { return StatusCode::FORBIDDEN.into_response(); }
    }
    let Ok(permit) = st.preauth.clone().try_acquire_owned() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let max = st.opts.max_frame;
    ws.max_message_size(max).max_frame_size(max).on_upgrade(move |socket| session(st, socket, permit))
}

fn text(msg: &ServerMessage) -> Message {
    Message::Text(serde_json::to_string(msg).expect("serializable").into())
}

fn close_msg(code: u16, reason: &str) -> Message {
    Message::Close(Some(CloseFrame { code, reason: reason.to_string().into() }))
}

fn error(code: ErrorCode, message: &str) -> ServerMessage {
    ServerMessage::Error { code, message: message.into(), supported: None }
}

async fn session(st: AppState, mut socket: WebSocket, permit: OwnedSemaphorePermit) {
    let hello = match tokio::time::timeout(st.opts.hello_timeout, socket.recv()).await {
        Ok(Some(Ok(Message::Text(t)))) => serde_json::from_str::<ClientMessage>(t.as_str()).ok(),
        _ => None,
    };
    let Some(ClientMessage::Hello { protocol, token, client, resume }) = hello else {
        let _ = socket.send(close_msg(CLOSE_UNAUTHORIZED, "hello required")).await;
        return;
    };
    if protocol != PROTOCOL {
        let msg = ServerMessage::Error {
            code: ErrorCode::UnsupportedProtocol,
            message: format!("unsupported protocol {protocol}"),
            supported: Some(vec![PROTOCOL.to_string()]),
        };
        let _ = socket.send(text(&msg)).await;
        let _ = socket.send(close_msg(CLOSE_BAD_PROTOCOL, "unsupported protocol")).await;
        return;
    }
    let Some(generation) = st.auth.check(&token) else {
        let _ = socket.send(text(&error(ErrorCode::Unauthorized, "invalid token"))).await;
        let _ = socket.send(close_msg(CLOSE_UNAUTHORIZED, "unauthorized")).await;
        return;
    };
    drop(permit);

    let (mut sink, mut stream) = socket.split();
    let (out, mut out_rx) = mpsc::channel::<Message>(64);
    let writer = tokio::spawn(async move {
        while let Some(m) = out_rx.recv().await {
            let closing = matches!(m, Message::Close(_));
            if sink.send(m).await.is_err() || closing { break; }
        }
    });

    let _ = out.send(text(&st.core.welcome())).await;
    let (initial, mut events) = st.core.attach(resume.as_ref());
    for m in &initial { let _ = out.send(text(m)).await; }

    let mut tick = tokio::time::interval(st.opts.rotation_check);
    loop {
        tokio::select! {
            frame = stream.next() => match frame {
                Some(Ok(Message::Text(t))) => on_frame(&st, &client.id, t.as_str(), &out),
                Some(Ok(Message::Binary(_))) => {
                    let _ = out.send(text(&error(ErrorCode::BadRequest, "binary frames are not supported"))).await;
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
            ev = events.recv() => match ev {
                Ok(m) => { if out.send(text(&m)).await.is_err() { break; } }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let _ = out.send(close_msg(CLOSE_TOO_SLOW, "client too slow")).await;
                    break;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            _ = tick.tick() => {
                if st.auth.generation() != generation {
                    let _ = out.send(close_msg(CLOSE_UNAUTHORIZED, "token rotated")).await;
                    break;
                }
            }
        }
    }
    drop(out);
    let _ = writer.await;
}

fn on_frame(st: &AppState, client_id: &str, raw: &str, out: &mpsc::Sender<Message>) {
    let value: serde_json::Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => { let _ = out.try_send(text(&error(ErrorCode::BadRequest, "invalid JSON"))); return; }
    };
    let id = value.get("id").and_then(|v| v.as_str()).map(str::to_owned);
    let msg: ClientMessage = match serde_json::from_value(value) {
        Ok(m) => m,
        Err(e) => {
            let reply = match id {
                Some(id) => ServerMessage::Result(CommandResult::err(&id, ErrorCode::BadRequest, &e.to_string())),
                None => error(ErrorCode::BadRequest, &e.to_string()),
            };
            let _ = out.try_send(text(&reply));
            return;
        }
    };
    match msg {
        ClientMessage::Ping { id } => { let _ = out.try_send(text(&ServerMessage::Pong { id })); }
        ClientMessage::Hello { .. } => { let _ = out.try_send(text(&error(ErrorCode::BadRequest, "already authenticated"))); }
        command => {
            let core = st.core.clone();
            let out = out.clone();
            let client = client_id.to_owned();
            tokio::spawn(async move {
                let result = core.handle(&client, command).await;
                let _ = out.send(text(&ServerMessage::Result(result))).await;
            });
        }
    }
}
