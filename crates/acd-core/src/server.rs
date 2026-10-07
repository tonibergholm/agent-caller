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
use tokio::sync::{broadcast, mpsc, oneshot, watch, OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinSet;

use crate::engine::{Core, RING_CAPACITY};

pub const CLOSE_BAD_PROTOCOL: u16 = 4400;
pub const CLOSE_UNAUTHORIZED: u16 = 4401;
pub const CLOSE_TOO_SLOW: u16 = 4408;

/// The client token. The generation increments on rotation, and sessions
/// authenticated under an older generation are closed immediately.
pub struct Auth {
    token: RwLock<String>,
    generation: watch::Sender<u64>,
}

impl Auth {
    pub fn new(token: String) -> Self {
        Self { token: RwLock::new(token), generation: watch::channel(0).0 }
    }
    pub fn rotate(&self, token: String) {
        let mut g = self.token.write().unwrap();
        *g = token;
        self.generation.send_modify(|n| *n += 1);
    }
    pub fn generation(&self) -> u64 { *self.generation.borrow() }
    fn subscribe(&self) -> watch::Receiver<u64> { self.generation.subscribe() }
    fn check(&self, presented: &str) -> Option<u64> {
        let g = self.token.read().unwrap();
        // An empty stored token must never authenticate (ct_eq("", "") is true).
        if g.is_empty() { return None; }
        bool::from(g.as_bytes().ct_eq(presented.as_bytes())).then(|| self.generation())
    }
}

#[derive(Clone)]
pub struct ServerOptions {
    pub allowed_origins: Vec<String>,
    pub hello_timeout: Duration,
    pub max_frame: usize,
    pub max_preauth: usize,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            allowed_origins: vec![],
            hello_timeout: Duration::from_secs(5),
            max_frame: 1 << 20,
            max_preauth: 8,
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

const PREAUTH_SEND_TIMEOUT: Duration = Duration::from_secs(2);
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);
const WRITER_GRACE: Duration = Duration::from_secs(1);
const MAX_INFLIGHT: usize = 32;

async fn preauth_send(socket: &mut WebSocket, m: Message) {
    let _ = tokio::time::timeout(PREAUTH_SEND_TIMEOUT, socket.send(m)).await;
}

/// Why a session ends; the code and reason go out in the final close frame.
type Closing = (u16, &'static str);
const ROTATED: Closing = (CLOSE_UNAUTHORIZED, "token rotated");

/// Queue a reply, giving up if the client does not drain its queue.
async fn reply(out: &mpsc::Sender<Message>, msg: &ServerMessage) -> Result<(), Closing> {
    match tokio::time::timeout(REPLY_TIMEOUT, out.send(text(msg))).await {
        Ok(Ok(())) => Ok(()),
        _ => Err((CLOSE_TOO_SLOW, "client too slow")),
    }
}

async fn session(st: AppState, mut socket: WebSocket, permit: OwnedSemaphorePermit) {
    let hello = match tokio::time::timeout(st.opts.hello_timeout, socket.recv()).await {
        Ok(Some(Ok(Message::Text(t)))) => serde_json::from_str::<ClientMessage>(t.as_str()).ok(),
        _ => None,
    };
    let Some(ClientMessage::Hello { protocol, token, client, resume }) = hello else {
        preauth_send(&mut socket, close_msg(CLOSE_UNAUTHORIZED, "hello required")).await;
        return;
    };
    if protocol != PROTOCOL {
        let msg = ServerMessage::Error {
            code: ErrorCode::UnsupportedProtocol,
            message: format!("unsupported protocol {protocol}"),
            supported: Some(vec![PROTOCOL.to_string()]),
        };
        preauth_send(&mut socket, text(&msg)).await;
        preauth_send(&mut socket, close_msg(CLOSE_BAD_PROTOCOL, "unsupported protocol")).await;
        return;
    }
    let mut gen_rx = st.auth.subscribe();
    let Some(generation) = st.auth.check(&token) else {
        preauth_send(&mut socket, text(&error(ErrorCode::Unauthorized, "invalid token"))).await;
        preauth_send(&mut socket, close_msg(CLOSE_UNAUTHORIZED, "unauthorized")).await;
        return;
    };
    drop(permit);

    let (mut sink, mut stream) = socket.split();
    let (out, mut out_rx) = mpsc::channel::<Message>(RING_CAPACITY);
    let (close_tx, close_rx) = oneshot::channel::<Closing>();
    let mut writer = tokio::spawn(async move {
        while let Some(m) = out_rx.recv().await {
            match tokio::time::timeout(WRITE_TIMEOUT, sink.send(m)).await {
                Ok(Ok(())) => {}
                _ => return,
            }
        }
        if let Ok((code, reason)) = close_rx.await {
            let _ = tokio::time::timeout(WRITE_TIMEOUT, sink.send(close_msg(code, reason))).await;
        }
    });

    let inflight = Arc::new(Semaphore::new(MAX_INFLIGHT));
    let mut tasks: JoinSet<()> = JoinSet::new();
    let closing = run(&st, &client.id, resume, generation, &mut gen_rx, &mut stream, &out, &inflight, &mut tasks).await;

    if closing == Err(ROTATED) {
        // Revoked: nothing this session started may take effect.
        tasks.abort_all();
    } else {
        // Detach, not abort: an in-flight delivery must finish so a retry sees its cached result.
        tasks.detach_all();
    }
    drop(out);
    if let Err(closing) = closing {
        let _ = close_tx.send(closing);
    }
    if tokio::time::timeout(WRITER_GRACE, &mut writer).await.is_err() {
        writer.abort();
    }
}

#[allow(clippy::too_many_arguments)]
async fn run(
    st: &AppState,
    client_id: &str,
    resume: Option<Cursor>,
    generation: u64,
    gen_rx: &mut watch::Receiver<u64>,
    stream: &mut futures_util::stream::SplitStream<WebSocket>,
    out: &mpsc::Sender<Message>,
    inflight: &Arc<Semaphore>,
    tasks: &mut JoinSet<()>,
) -> Result<(), Closing> {
    if *gen_rx.borrow_and_update() != generation {
        return Err(ROTATED);
    }
    reply(out, &st.core.welcome()).await?;
    let (initial, mut events) = st.core.attach(resume.as_ref());
    for m in &initial {
        reply(out, m).await?;
    }
    loop {
        tokio::select! {
            frame = stream.next() => {
                if *gen_rx.borrow() != generation { return Err(ROTATED); }
                let handled = async {
                    match frame {
                        Some(Ok(Message::Text(t))) => on_frame(st, client_id, t.as_str(), out, inflight, tasks).await.map(|()| true),
                        Some(Ok(Message::Binary(_))) => reply(out, &error(ErrorCode::BadRequest, "binary frames are not supported")).await.map(|()| true),
                        Some(Ok(Message::Close(_))) | Some(Err(_)) | None => Ok(false),
                        Some(Ok(_)) => Ok(true),
                    }
                };
                tokio::select! {
                    r = handled => if !r? { return Ok(()) },
                    _ = gen_rx.changed() => return Err(ROTATED),
                }
            },
            ev = events.recv() => match ev {
                Ok(m) => {
                    if out.try_send(text(&m)).is_err() { return Err((CLOSE_TOO_SLOW, "client too slow")); }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => return Err((CLOSE_TOO_SLOW, "client too slow")),
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            },
            _ = gen_rx.changed() => {
                if *gen_rx.borrow() != generation { return Err(ROTATED); }
            },
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
        }
    }
}

async fn on_frame(
    st: &AppState,
    client_id: &str,
    raw: &str,
    out: &mpsc::Sender<Message>,
    inflight: &Arc<Semaphore>,
    tasks: &mut JoinSet<()>,
) -> Result<(), Closing> {
    let value: serde_json::Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => return reply(out, &error(ErrorCode::BadRequest, "invalid JSON")).await,
    };
    let id = value.get("id").and_then(|v| v.as_str()).map(str::to_owned);
    let msg: ClientMessage = match serde_json::from_value(value) {
        Ok(m) => m,
        Err(e) => {
            let r = match id {
                Some(id) => ServerMessage::Result(CommandResult::err(&id, ErrorCode::BadRequest, &e.to_string())),
                None => error(ErrorCode::BadRequest, &e.to_string()),
            };
            return reply(out, &r).await;
        }
    };
    match msg {
        ClientMessage::Ping { id } => reply(out, &ServerMessage::Pong { id }).await,
        ClientMessage::Hello { .. } => reply(out, &error(ErrorCode::BadRequest, "already authenticated")).await,
        command => {
            let id = command.command_id().map(str::to_owned);
            let Ok(permit) = inflight.clone().try_acquire_owned() else {
                let r = match id {
                    Some(id) => ServerMessage::Result(CommandResult::err(&id, ErrorCode::BadRequest, "too many in-flight commands")),
                    None => error(ErrorCode::BadRequest, "too many in-flight commands"),
                };
                return reply(out, &r).await;
            };
            let core = st.core.clone();
            let out = out.clone();
            let client = client_id.to_owned();
            tasks.spawn(async move {
                let result = core.handle(&client, command).await;
                drop(permit);
                let _ = reply(&out, &ServerMessage::Result(result)).await;
            });
            Ok(())
        }
    }
}

#[cfg(test)]
mod auth_tests {
    use super::*;

    #[test]
    fn empty_stored_token_never_authenticates() {
        let auth = Auth::new(String::new());
        assert!(auth.check("").is_none());
        assert!(auth.check("anything").is_none());
        auth.rotate("secret".into());
        assert!(auth.check("secret").is_some());
        assert!(auth.check("").is_none());
        auth.rotate(String::new());
        assert!(auth.check("").is_none());
    }
}
