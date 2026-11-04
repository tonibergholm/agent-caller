use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use acd_protocol::*;
use axum::Router;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Extension, State};
use axum::http::{HeaderMap, StatusCode, header::ORIGIN};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures_util::{SinkExt, StreamExt};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::sync::{Semaphore, broadcast, mpsc, oneshot, watch};
use tokio::task::JoinSet;

use crate::idem::Origin;
use crate::engine::{Core, MAX_ID_BYTES, RING_CAPACITY};

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
        Self {
            token: RwLock::new(token),
            generation: watch::channel(0).0,
        }
    }
    pub fn rotate(&self, token: String) {
        let mut g = self.token.write().unwrap();
        *g = token;
        self.generation.send_modify(|n| *n += 1);
    }
    pub fn generation(&self) -> u64 {
        *self.generation.borrow()
    }
    fn subscribe(&self) -> watch::Receiver<u64> {
        self.generation.subscribe()
    }
    fn check(&self, presented: &str) -> Option<u64> {
        let g = self.token.read().unwrap();
        // An empty stored token must never authenticate (ct_eq("", "") is true).
        if g.is_empty() {
            return None;
        }
        bool::from(g.as_bytes().ct_eq(presented.as_bytes())).then(|| self.generation())
    }
}

/// Lets at most `MAX` auth log lines through per `WINDOW`, so a flood of
/// connections can fill neither the log nor the disk. Lines held back are
/// counted and reported on the next line that passes.
struct LogLimiter {
    state: Mutex<LimiterState>,
}
struct LimiterState {
    window_start: Instant,
    in_window: u32,
    suppressed: u64,
    total: u64,
}
impl LogLimiter {
    const MAX: u32 = 10;
    const WINDOW: Duration = Duration::from_secs(10);
    fn new() -> Self {
        Self {
            state: Mutex::new(LimiterState {
                window_start: Instant::now(),
                in_window: 0,
                suppressed: 0,
                total: 0,
            }),
        }
    }
    /// `Some((suppressed_since_last_line, total_events))` when the line may
    /// be written, `None` when it is held back.
    fn pass(&self) -> Option<(u64, u64)> {
        self.pass_at(Instant::now())
    }
    /// `pass` with the clock passed in, so a test controls the window.
    fn pass_at(&self, now: Instant) -> Option<(u64, u64)> {
        let mut s = self.state.lock().unwrap();
        s.total += 1;
        if now.saturating_duration_since(s.window_start) >= Self::WINDOW {
            s.window_start = now;
            s.in_window = 0;
        }
        if s.in_window >= Self::MAX {
            s.suppressed += 1;
            return None;
        }
        s.in_window += 1;
        Some((std::mem::take(&mut s.suppressed), s.total))
    }
}

/// A peer-chosen string is never logged, because it may be the token itself.
/// Its log form is the byte length and the first 8 hex characters of its
/// SHA-256, enough to tell clients apart and nothing to read back.
fn log_tag(s: &str) -> (usize, String) {
    use std::fmt::Write;
    let digest = Sha256::digest(s.as_bytes());
    let mut hex = String::with_capacity(8);
    for b in &digest[..4] {
        let _ = write!(hex, "{b:02x}");
    }
    (s.len(), hex)
}

#[derive(Clone)]
pub struct ServerOptions {
    pub allowed_origins: Vec<String>,
    pub hello_timeout: Duration,
    pub max_frame: usize,
    pub max_preauth: usize,
    /// Commands in flight per session.
    pub max_inflight: usize,
    /// Commands in flight across all sessions, including detached ones.
    pub max_inflight_total: usize,
    /// Test seam. When set, the hello timeout of each connection starts
    /// only once the watched value is `true`. `None` (the default) starts
    /// it at upgrade, as always.
    #[doc(hidden)]
    pub hello_gate: Option<watch::Receiver<bool>>,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            allowed_origins: vec![],
            hello_timeout: Duration::from_secs(5),
            max_frame: 1 << 20,
            max_preauth: 8,
            max_inflight: 32,
            max_inflight_total: 256,
            hello_gate: None,
        }
    }
}

#[derive(Clone)]
struct AppState {
    core: Arc<Core>,
    auth: Arc<Auth>,
    opts: ServerOptions,
    /// Pre-auth slots for network peers. Each source address is also capped in
    /// `tls::Admission`, so one peer cannot hold more than a few of them.
    preauth: Arc<SourcePool>,
    /// Pre-auth slots for the Unix socket, apart from the network pool so
    /// that no remote peer can starve the local client.
    local_preauth: Arc<Semaphore>,
    /// Daemon-wide in-flight commands; shared by every clone of the router.
    inflight_total: Arc<Semaphore>,
    /// Bounded logs for rejected and accepted hellos. Never log a token.
    reject_log: Arc<LogLimiter>,
    accept_log: Arc<LogLimiter>,
}

/// The acd/1 WebSocket server. Serve clones of one router on every listener
/// so the limits apply daemon-wide.
pub fn router(core: Arc<Core>, auth: Arc<Auth>, opts: ServerOptions) -> Router {
    let preauth = Arc::new(SourcePool::new(opts.max_preauth));
    let local_preauth = Arc::new(Semaphore::new(opts.max_preauth));
    let inflight_total = Arc::new(Semaphore::new(opts.max_inflight_total));
    Router::new().route("/", get(upgrade)).with_state(AppState {
        core,
        auth,
        opts,
        preauth,
        local_preauth,
        inflight_total,
        reject_log: Arc::new(LogLimiter::new()),
        accept_log: Arc::new(LogLimiter::new()),
    })
}

async fn upgrade(
    State(st): State<AppState>,
    admit: Option<Extension<ConnectInfo<crate::tls::Admit>>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let admit = admit.map(|Extension(info)| info);
    if let Some(origin) = headers.get(ORIGIN) {
        let allowed = origin
            .to_str()
            .is_ok_and(|o| st.opts.allowed_origins.iter().any(|a| a == o));
        if !allowed {
            return StatusCode::FORBIDDEN.into_response();
        }
    }
    // A connection without admission info did not come through the TLS
    // listener: it is on the Unix socket.
    let permit: Box<dyn Send> = match &admit {
        Some(a) => {
            let Some(source) = a.peer().map(|p| p.ip()) else {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            };
            match st.preauth.try_acquire(source) {
                Some(slot) => Box::new(slot),
                None => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            }
        }
        None => match st.local_preauth.clone().try_acquire_owned() {
            Ok(permit) => Box::new(permit),
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        },
    };
    let max = st.opts.max_frame;
    ws.max_message_size(max)
        .max_frame_size(max)
        .on_upgrade(move |socket| session(st, socket, permit, admit.map(|ConnectInfo(a)| a)))
}

/// Slots of the post-upgrade pre-auth pool that only a source with no
/// connection waiting for its hello may take. Without them, a few sources
/// holding their per-source share each could take the whole pool.
const PREAUTH_RESERVE: usize = 2;

/// The post-upgrade pre-auth pool for network peers. Like `tls::Admission`
/// it remembers each source: a source holds at most
/// `tls::MAX_UNAUTHENTICATED_PER_SOURCE` slots, and the last
/// `PREAUTH_RESERVE` slots go only to sources holding none.
struct SourcePool {
    capacity: usize,
    held: Mutex<std::collections::HashMap<std::net::IpAddr, usize>>,
}

struct SourceSlot {
    pool: Arc<SourcePool>,
    source: std::net::IpAddr,
}

impl SourcePool {
    fn new(capacity: usize) -> Self {
        Self { capacity, held: Mutex::default() }
    }

    fn try_acquire(self: &Arc<Self>, source: std::net::IpAddr) -> Option<SourceSlot> {
        let source = source.to_canonical();
        let mut held = self.held.lock().unwrap();
        let total: usize = held.values().sum();
        let mine = held.get(&source).copied().unwrap_or(0);
        let reserve = PREAUTH_RESERVE.min(self.capacity.saturating_sub(1));
        if mine >= crate::tls::MAX_UNAUTHENTICATED_PER_SOURCE
            || total >= self.capacity
            || (mine > 0 && total >= self.capacity - reserve)
        {
            return None;
        }
        *held.entry(source).or_insert(0) += 1;
        Some(SourceSlot { pool: self.clone(), source })
    }
}

impl Drop for SourceSlot {
    fn drop(&mut self) {
        let mut held = self.pool.held.lock().unwrap();
        if let Some(n) = held.get_mut(&self.source) {
            *n -= 1;
            if *n == 0 {
                held.remove(&self.source);
            }
        }
    }
}

/// The idempotency origin of a session: its TLS source address, or the local
/// socket when there is none.
fn origin_of(source: Option<std::net::IpAddr>) -> Origin {
    source.map_or(Origin::Local, Origin::net)
}

fn text(msg: &ServerMessage) -> Message {
    Message::Text(serde_json::to_string(msg).expect("serializable").into())
}

fn close_msg(code: u16, reason: &str) -> Message {
    Message::Close(Some(CloseFrame {
        code,
        reason: reason.to_string().into(),
    }))
}

fn error(code: ErrorCode, message: &str) -> ServerMessage {
    ServerMessage::Error {
        code,
        message: message.into(),
        supported: None,
    }
}

const PREAUTH_SEND_TIMEOUT: Duration = Duration::from_secs(2);
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);
const WRITER_GRACE: Duration = Duration::from_secs(1);

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

async fn session(
    st: AppState,
    mut socket: WebSocket,
    permit: Box<dyn Send>,
    admit: Option<crate::tls::Admit>,
) {
    if let Some(mut gate) = st.opts.hello_gate.clone() {
        let _ = gate.wait_for(|open| *open).await;
    }
    let source = admit.as_ref().and_then(|a| a.peer()).map(|p| p.ip());
    let peer = match &admit {
        Some(a) => a.peer().map_or_else(|| "tcp".to_string(), |p| p.to_string()),
        None => "local".to_string(),
    };
    let reject = |why: &str| {
        if let Some((suppressed, total)) = st.reject_log.pass() {
            tracing::warn!(target: "acd::auth", %peer, why, rejected_total = total, suppressed, "hello rejected");
        }
    };
    let hello = match tokio::time::timeout(st.opts.hello_timeout, socket.recv()).await {
        Ok(Some(Ok(Message::Text(t)))) => serde_json::from_str::<ClientMessage>(t.as_str()).ok(),
        _ => None,
    };
    let Some(ClientMessage::Hello {
        protocol,
        token,
        client,
        resume,
    }) = hello
    else {
        reject("hello required");
        preauth_send(&mut socket, close_msg(CLOSE_UNAUTHORIZED, "hello required")).await;
        return;
    };
    if protocol != PROTOCOL {
        reject("unsupported protocol");
        let msg = ServerMessage::Error {
            code: ErrorCode::UnsupportedProtocol,
            message: format!("unsupported protocol {protocol}"),
            supported: Some(vec![PROTOCOL.to_string()]),
        };
        preauth_send(&mut socket, text(&msg)).await;
        preauth_send(
            &mut socket,
            close_msg(CLOSE_BAD_PROTOCOL, "unsupported protocol"),
        )
        .await;
        return;
    }
    let mut gen_rx = st.auth.subscribe();
    let Some(generation) = st.auth.check(&token) else {
        reject("invalid token");
        preauth_send(
            &mut socket,
            text(&error(ErrorCode::Unauthorized, "invalid token")),
        )
        .await;
        preauth_send(&mut socket, close_msg(CLOSE_UNAUTHORIZED, "unauthorized")).await;
        return;
    };
    if client.id.len() > MAX_ID_BYTES {
        reject("client.id too long");
        preauth_send(
            &mut socket,
            text(&error(
                ErrorCode::BadRequest,
                "client.id longer than 128 bytes",
            )),
        )
        .await;
        preauth_send(&mut socket, close_msg(CLOSE_UNAUTHORIZED, "invalid hello")).await;
        return;
    }
    drop(permit);
    if let Some(admit) = admit {
        admit.authenticated();
    }
    if let Some((suppressed, total)) = st.accept_log.pass() {
        let (id_len, id_sha) = log_tag(&client.id);
        let (name_len, name_sha) = log_tag(&client.name);
        tracing::info!(target: "acd::auth", %peer, protocol = %protocol,
            client_id_len = id_len, client_id_sha = %id_sha,
            client_name_len = name_len, client_name_sha = %name_sha,
            client_version_len = client.version.len(),
            accepted_total = total, suppressed, "hello accepted");
    }

    let (mut sink, mut stream) = socket.split();
    let origin = origin_of(source);
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

    let inflight = Arc::new(Semaphore::new(st.opts.max_inflight));
    let mut tasks: JoinSet<()> = JoinSet::new();
    let closing = run(
        &st,
        &client.id,
        origin,
        resume,
        generation,
        &mut gen_rx,
        &mut stream,
        &out,
        &inflight,
        &mut tasks,
    )
    .await;

    // Detach, not abort: an in-flight delivery must finish so a retry sees its
    // cached result. Each task cancels itself if the token is rotated.
    tasks.detach_all();
    drop(out);
    if let Err(closing) = closing {
        let _ = close_tx.send(closing);
    }
    if tokio::time::timeout(WRITER_GRACE, &mut writer)
        .await
        .is_err()
    {
        writer.abort();
    }
}

#[allow(clippy::too_many_arguments)]
async fn run(
    st: &AppState,
    client_id: &str,
    origin: Origin,
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
                        Some(Ok(Message::Text(t))) => on_frame(st, client_id, origin, generation, t.as_str(), out, inflight, tasks).await.map(|()| true),
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

#[allow(clippy::too_many_arguments)]
async fn on_frame(
    st: &AppState,
    client_id: &str,
    origin: Origin,
    generation: u64,
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
                Some(id) => ServerMessage::Result(CommandResult::err(
                    &id,
                    ErrorCode::BadRequest,
                    &e.to_string(),
                )),
                None => error(ErrorCode::BadRequest, &e.to_string()),
            };
            return reply(out, &r).await;
        }
    };
    match msg {
        ClientMessage::Ping { id } => reply(out, &ServerMessage::Pong { id }).await,
        ClientMessage::Hello { .. } => {
            reply(out, &error(ErrorCode::BadRequest, "already authenticated")).await
        }
        command => {
            let permits = inflight.clone().try_acquire_owned().and_then(|session| {
                st.inflight_total
                    .clone()
                    .try_acquire_owned()
                    .map(|total| (session, total))
            });
            let Ok(permits) = permits else {
                let r = match command.command_id() {
                    Some(id) => ServerMessage::Result(CommandResult::err(
                        id,
                        ErrorCode::BadRequest,
                        "too many in-flight commands",
                    )),
                    None => error(ErrorCode::BadRequest, "too many in-flight commands"),
                };
                return reply(out, &r).await;
            };
            let core = st.core.clone();
            let out = out.clone();
            let client = client_id.to_owned();
            let gen_rx = st.auth.subscribe();
            tasks.spawn(run_command(
                core, client, origin, command, generation, gen_rx, out, permits,
            ));
            Ok(())
        }
    }
}

/// Runs one command and replies with its result, unless the token is rotated
/// away from `generation` first. A rotation revokes every command authorized
/// under an older token, detached ones included. Dropping `handle` runs the
/// core's abandon guard: an undelivered claim is released and a delivery in
/// flight resolves `uncertain`. Nothing is re-sent.
#[allow(clippy::too_many_arguments)]
async fn run_command<P: Send>(
    core: Arc<Core>,
    client: String,
    origin: Origin,
    command: ClientMessage,
    generation: u64,
    mut gen_rx: watch::Receiver<u64>,
    out: mpsc::Sender<Message>,
    permits: P,
) {
    // Revoked before it ever ran: do not touch the core at all.
    if *gen_rx.borrow_and_update() != generation {
        return;
    }
    tokio::select! {
        // Revocation wins whenever both are ready, so a delivery that becomes
        // ready in the same instant as a rotation is dropped, not completed.
        biased;
        () = revoked(&mut gen_rx, generation) => {}
        result = core.handle_from(&client, origin, command) => {
            drop(permits);
            let _ = reply(&out, &ServerMessage::Result(result)).await;
        }
    }
}

/// Completes once the token generation moves away from `generation`. Never
/// completes if the `Auth` is gone, since nothing can rotate it then.
async fn revoked(gen_rx: &mut watch::Receiver<u64>, generation: u64) {
    if gen_rx.wait_for(|g| *g != generation).await.is_err() {
        std::future::pending::<()>().await;
    }
}

#[cfg(test)]
mod source_pool_tests {
    use super::*;
    use std::net::IpAddr;

    fn ip(n: u8) -> IpAddr {
        IpAddr::from([10, 0, 0, n])
    }

    /// Each source takes two slots until the reserve is reached, so with a
    /// pool of 8 the sources fill it as 2 + 2 + 2 + 1 + 1: five sources, not
    /// four. The reserve closes only the four-source case.
    #[test]
    fn five_sources_still_fill_the_pool_but_four_cannot() {
        let pool = Arc::new(SourcePool::new(8));
        let mut held = Vec::new();
        for n in 1..=4 {
            held.extend(pool.try_acquire(ip(n)));
            held.extend(pool.try_acquire(ip(n)));
        }
        assert_eq!(held.len(), 7, "the fourth source cannot take a reserved slot for its second connection");
        let fifth = pool.try_acquire(ip(5)).expect("a source holding none takes a reserved slot");
        assert!(pool.try_acquire(ip(6)).is_none(), "five sources fill the pool: the residual");
        drop(fifth);
        assert!(pool.try_acquire(ip(6)).is_some(), "a freed slot is usable at once");
    }

    /// Fails with `PREAUTH_RESERVE = 0`: four sources would hold all 8 slots.
    #[test]
    fn a_source_holding_one_slot_is_refused_a_second_once_six_are_taken() {
        let pool = Arc::new(SourcePool::new(8));
        let mut held: Vec<_> = (1..=6).filter_map(|n| pool.try_acquire(ip(n))).collect();
        assert_eq!(held.len(), 6);
        assert!(pool.try_acquire(ip(1)).is_none(), "holds one, pool at 6/8: refused despite spare capacity");
        assert!(pool.try_acquire(ip(7)).is_some(), "a source holding none is admitted");
        held.clear();
        assert!(pool.try_acquire(ip(1)).is_some());
    }

    #[test]
    fn a_source_holds_at_most_its_cap_and_mapped_addresses_count_as_ipv4() {
        let pool = Arc::new(SourcePool::new(8));
        let v4: IpAddr = "192.0.2.1".parse().unwrap();
        let mapped: IpAddr = "::ffff:192.0.2.1".parse().unwrap();
        let _a = pool.try_acquire(v4).unwrap();
        let _b = pool.try_acquire(mapped).unwrap();
        assert!(pool.try_acquire(v4).is_none(), "two slots per source, mapped or not");
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

#[cfg(test)]
mod revocation_tests {
    use super::*;
    use crate::registry::Observation;
    use crate::testing::*;

    /// A core with one open request, plus a command answering it.
    fn setup(fake: Arc<FakeAdapter>) -> (Arc<Core>, ClientMessage) {
        let core = Core::new(test_machine(), vec![fake]);
        core.observe(Observation::AgentUpsert(agent_obs("a1")));
        core.observe(Observation::RequestSeen(permission_obs("a1", "k1")));
        let rid = core
            .attach(None)
            .0
            .iter()
            .find_map(|m| match m {
                ServerMessage::Snapshot { agents, .. } => {
                    agents[0].pending.as_ref().map(|p| p.id.clone())
                }
                _ => None,
            })
            .unwrap();
        let cmd = ClientMessage::RequestAnswer {
            id: "c".into(),
            request_id: rid,
            revision: None,
            option_id: Some("allow".into()),
            text: None,
        };
        (core, cmd)
    }

    fn pending(core: &Core) -> Option<RequestStatus> {
        let (initial, _) = core.attach(None);
        let ServerMessage::Snapshot { agents, .. } = &initial[0] else {
            panic!("expected snapshot")
        };
        agents[0].pending.as_ref().map(|p| p.status)
    }

    // tokio::select! picks a random branch when several are ready, so each
    // case runs many times; one delivery after revocation fails the test.
    const ROUNDS: usize = 64;

    #[tokio::test]
    async fn rotation_before_the_first_poll_never_delivers() {
        for _ in 0..ROUNDS {
            let fake = FakeAdapter::new();
            let (core, cmd) = setup(fake.clone());
            let auth = Auth::new("old".into());
            let generation = auth.generation();
            let gen_rx = auth.subscribe();
            let (out, mut out_rx) = mpsc::channel(8);
            let task = run_command(core.clone(), "cl".into(), Origin::Local, cmd, generation, gen_rx, out, ());
            auth.rotate("new".into()); // spawned but not yet polled
            task.await;
            assert!(fake.delivered().is_empty(), "delivered after revocation");
            assert!(
                out_rx.try_recv().is_err(),
                "no result for a revoked command"
            );
            assert_eq!(pending(&core), Some(RequestStatus::Open), "never claimed");
        }
    }

    #[tokio::test]
    async fn rotation_together_with_delivery_never_delivers() {
        for _ in 0..ROUNDS {
            let fake = FakeAdapter::gated();
            let (core, cmd) = setup(fake.clone());
            let auth = Arc::new(Auth::new("old".into()));
            let generation = auth.generation();
            let gen_rx = auth.subscribe();
            let (out, _out_rx) = mpsc::channel(8);
            let task = tokio::spawn(run_command(
                core.clone(),
                "cl".into(),
                Origin::Local,
                cmd,
                generation,
                gen_rx,
                out,
                (),
            ));
            while pending(&core) != Some(RequestStatus::Answering) {
                tokio::task::yield_now().await;
            }
            // Both wake-ups land before the task runs again (current-thread runtime).
            fake.open_gate();
            auth.rotate("new".into());
            task.await.unwrap();
            assert!(fake.delivered().is_empty(), "delivered after revocation");
            assert_eq!(
                pending(&core),
                None,
                "resolved uncertain by the abandon guard"
            );
        }
    }
}

#[cfg(test)]
mod log_limit_tests {
    use super::*;

    #[test]
    fn a_flood_passes_a_bounded_number_of_lines_and_counts_the_rest() {
        let l = LogLimiter::new();
        let t0 = l.state.lock().unwrap().window_start;
        // The clock stands still inside the window: no real time involved.
        let passed = (0..1000).filter(|_| l.pass_at(t0).is_some()).count();
        assert_eq!(passed, LogLimiter::MAX as usize);
        // Just short of the window: still held back.
        let almost = t0 + LogLimiter::WINDOW - Duration::from_millis(1);
        assert_eq!(l.pass_at(almost), None);
        // A new window lets lines through again and reports what was held back.
        assert_eq!(l.pass_at(t0 + LogLimiter::WINDOW), Some((991, 1002)));
    }
}

#[cfg(test)]
#[path = "mutants_server.rs"]
mod mutants_server;

#[cfg(test)]
#[path = "mutants_pool.rs"]
mod mutants_pool;
