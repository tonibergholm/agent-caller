//! Authenticated, bounded hook endpoint shared by loopback TCP and Unix sockets.
use std::collections::HashMap;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Router;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, Request, State};
use axum::http::{Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::serve::Listener;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, UnixListener};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use tokio::time::Instant;

use super::mapping::log_payload;
use super::wire::{Hints, HookAgent, HookEvent, HookName, Payload};

const MAX_REJECTED: usize = 256;

#[derive(Clone, PartialEq, Eq)]
pub struct Credential {
    pub token: String,
    pub generation: u64,
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credential")
            .field("token", &"<redacted>")
            .field("generation", &self.generation)
            .finish()
    }
}

struct Creds {
    current: Credential,
    previous: Option<Credential>,
}

pub struct HookAuth {
    creds: RwLock<Creds>,
    rejected: Mutex<HashMap<String, Instant>>,
}

pub fn remote_id(hint: Option<&str>) -> String {
    match hint {
        None => "none".into(),
        Some(h) => Sha256::digest(h.as_bytes())
            .iter()
            .take(4)
            .map(|b| format!("{b:02x}"))
            .collect(),
    }
}

impl HookAuth {
    pub fn new(current: Credential) -> Self {
        Self {
            creds: RwLock::new(Creds {
                current,
                previous: None,
            }),
            rejected: Mutex::default(),
        }
    }

    pub fn set(&self, current: Credential, previous: Option<Credential>) {
        *self.creds.write().expect("hook auth") = Creds { current, previous };
    }

    pub fn check(&self, presented: &str) -> Option<u64> {
        let creds = self.creds.read().expect("hook auth");
        let matches = |stored: &Credential| {
            !stored.token.is_empty()
                && bool::from(stored.token.as_bytes().ct_eq(presented.as_bytes()))
        };
        if matches(&creds.current) {
            Some(creds.current.generation)
        } else {
            creds
                .previous
                .as_ref()
                .filter(|p| matches(p))
                .map(|p| p.generation)
        }
    }

    pub fn note_rejected(&self, hint: Option<&str>) {
        let id = remote_id(hint);
        let now = Instant::now();
        let mut rejected = self.rejected.lock().expect("hook auth");
        let quiet = rejected
            .get(&id)
            .is_some_and(|at| now.duration_since(*at) < Duration::from_secs(60));
        rejected.insert(id.clone(), now);
        while rejected.len() > MAX_REJECTED {
            let oldest = rejected
                .iter()
                .min_by_key(|(_, at)| **at)
                .map(|(key, _)| key.clone())
                .expect("non-empty");
            rejected.remove(&oldest);
        }
        drop(rejected);
        if !quiet {
            tracing::warn!(remote = %id, "hook request with a missing or retired token (restart that Claude session or run /clear in it)");
        }
    }

    pub fn rejected_since(&self, since: Instant) -> usize {
        self.rejected
            .lock()
            .expect("hook auth")
            .values()
            .filter(|at| **at >= since)
            .count()
    }

    pub fn rejected_entries(&self) -> usize {
        self.rejected.lock().expect("hook auth").len()
    }
}

pub type HoldId = u64;

#[derive(Debug)]
pub struct HoldPermit(#[allow(dead_code)] OwnedSemaphorePermit);

pub type HoldIn = (HoldId, oneshot::Sender<String>, HoldPermit);

#[derive(Debug)]
pub enum Inbound {
    Event {
        event: HookEvent,
        generation: u64,
        bytes: usize,
    },
    Held {
        event: HookEvent,
        hold: HoldId,
        reply: oneshot::Sender<String>,
        generation: u64,
        permit: HoldPermit,
    },
    HoldClosed {
        hold: HoldId,
    },
    HoldExpired {
        hold: HoldId,
    },
}

#[derive(Clone, Debug)]
pub struct EndpointOptions {
    pub permission_hold: Duration,
    pub max_holds: usize,
    pub body_timeout: Duration,
    pub max_body: usize,
    pub max_events: usize,
    pub max_event_bytes: usize,
    pub max_connections: usize,
}

impl Default for EndpointOptions {
    fn default() -> Self {
        Self {
            permission_hold: Duration::from_secs(3600),
            max_holds: 64,
            body_timeout: Duration::from_secs(5),
            max_body: 1 << 20,
            max_events: 512,
            max_event_bytes: 16 << 20,
            max_connections: 128,
        }
    }
}

struct Budget {
    bytes: AtomicUsize,
    max: usize,
    dropped: AtomicU64,
}

#[derive(Clone)]
pub struct MailboxTx {
    events: mpsc::Sender<Inbound>,
    /// Holds, closes and expiries (A2), capacity `2 × max_holds`. A hold
    /// reserves both of its slots at admission, so a `Held` and its later
    /// `HoldClosed` or `HoldExpired` are never dropped.
    control: mpsc::Sender<Inbound>,
    budget: Arc<Budget>,
}

pub struct Mailbox {
    pub events: mpsc::Receiver<Inbound>,
    pub control: mpsc::Receiver<Inbound>,
    budget: Arc<Budget>,
}

impl Mailbox {
    pub fn release(&self, bytes: usize) {
        self.budget.bytes.fetch_sub(bytes, Ordering::SeqCst);
    }

    pub fn dropped(&self) -> u64 {
        self.budget.dropped.load(Ordering::SeqCst)
    }
}

pub fn mailbox(opts: &EndpointOptions) -> (MailboxTx, Mailbox) {
    let (events_tx, events_rx) = mpsc::channel(opts.max_events);
    let (control_tx, control_rx) = mpsc::channel(opts.max_holds.saturating_mul(2));
    let budget = Arc::new(Budget {
        bytes: AtomicUsize::new(0),
        max: opts.max_event_bytes,
        dropped: AtomicU64::new(0),
    });
    (
        MailboxTx {
            events: events_tx,
            control: control_tx,
            budget: budget.clone(),
        },
        Mailbox {
            events: events_rx,
            control: control_rx,
            budget,
        },
    )
}

impl MailboxTx {
    /// Queued (and, for control, reserved) messages: `(control, events)`.
    /// Read from the senders, so it is current while the reconciler is
    /// blocked. An open hold keeps one control slot reserved.
    pub fn queued(&self) -> (usize, usize) {
        (
            self.control.max_capacity() - self.control.capacity(),
            self.events.max_capacity() - self.events.capacity(),
        )
    }

    fn event(&self, event: HookEvent, generation: u64, bytes: usize) {
        let budget = &self.budget;
        let previous = budget.bytes.fetch_add(bytes, Ordering::SeqCst);
        if previous.saturating_add(bytes) > budget.max {
            budget.bytes.fetch_sub(bytes, Ordering::SeqCst);
            budget.dropped.fetch_add(1, Ordering::SeqCst);
            return;
        }
        if self
            .events
            .try_send(Inbound::Event {
                event,
                generation,
                bytes,
            })
            .is_err()
        {
            budget.bytes.fetch_sub(bytes, Ordering::SeqCst);
            budget.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Reserves the two control slots a hold may use (A2). `None` when the
    /// channel is full, for example while handlers whose permits the
    /// adapter already freed still own reserved slots.
    fn reserve_hold(&self) -> Option<(ControlSlot, ControlSlot)> {
        let held = self.control.clone().try_reserve_owned().ok()?;
        let end = self.control.clone().try_reserve_owned().ok()?;
        Some((held, end))
    }
}

type ControlSlot = mpsc::OwnedPermit<Inbound>;

#[derive(Clone)]
struct Ctx {
    auth: Arc<HookAuth>,
    tx: MailboxTx,
    opts: EndpointOptions,
    holds: Arc<Semaphore>,
    next: Arc<AtomicU64>,
}

impl Ctx {
    fn new(auth: Arc<HookAuth>, tx: MailboxTx, opts: EndpointOptions) -> Self {
        Ctx {
            auth,
            tx,
            holds: Arc::new(Semaphore::new(opts.max_holds)),
            opts,
            next: Arc::default(),
        }
    }
}

pub fn router(auth: Arc<HookAuth>, tx: MailboxTx, opts: EndpointOptions) -> Router {
    let ctx = Ctx::new(auth, tx, opts);
    Router::new()
        .route("/hook/{agent}/{event}", any(handle))
        .fallback(|| async { StatusCode::UNAUTHORIZED })
        .with_state(ctx)
}

fn empty() -> Response {
    StatusCode::OK.into_response()
}

async fn handle(
    State(ctx): State<Ctx>,
    path: Result<Path<(String, String)>, PathRejection>,
    req: Request,
) -> Response {
    // A malformed segment (`/hook/%FF/Stop`) is unauthorized like any
    // other bad path, not a 400 from the extractor.
    let Ok(Path((agent, event))) = path else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    serve_hook(ctx, &agent, &event, req).await
}

async fn serve_hook(ctx: Ctx, agent: &str, event: &str, req: Request) -> Response {
    if req.method() != Method::POST {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(agent) = HookAgent::parse(agent) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let hints = Hints::from_headers(req.headers());
    let token = req
        .headers()
        .get("x-acd-token")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let Some(generation) = ctx.auth.check(token) else {
        ctx.auth.note_rejected(hints.herdr_pane.as_deref());
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let body = match tokio::time::timeout(
        ctx.opts.body_timeout,
        axum::body::to_bytes(req.into_body(), ctx.opts.max_body),
    )
    .await
    {
        Ok(Ok(body)) => body,
        Ok(Err(_)) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
        Err(_) => return StatusCode::REQUEST_TIMEOUT.into_response(),
    };
    let name = HookName::parse(event);
    log_payload(agent, &name, &body);
    let Some(payload) = serde_json::from_slice(&body)
        .ok()
        .and_then(|value| Payload::from_value(&value))
    else {
        return empty();
    };
    let holdable = agent == HookAgent::Claude
        && name == HookName::PermissionRequest
        && payload.tool_name.as_deref() != Some("AskUserQuestion");
    let event = HookEvent {
        agent,
        name,
        payload,
        hints,
        at: Instant::now(),
    };
    if !holdable {
        ctx.tx.event(event, generation, body.len());
        return empty();
    }
    // A1: no hold permit, or no room for its two control messages (A2),
    // fails open at once on the unheld path.
    let admitted = ctx
        .holds
        .clone()
        .try_acquire_owned()
        .ok()
        .and_then(|permit| Some((HoldPermit(permit), ctx.tx.reserve_hold()?)));
    let Some((permit, (held, end))) = admitted else {
        tracing::warn!("too many held permission prompts; this one is answered in the terminal");
        ctx.tx.event(event, generation, body.len());
        return empty();
    };
    hold(ctx, event, generation, permit, held, end).await
}

/// Calls the hook handler directly, without the router or a connection, so
/// a test can poll a handler future by hand and drop it at a chosen point
/// (the way hyper drops it when the client closes).
#[cfg(any(test, feature = "testing"))]
pub struct DirectHooks(Ctx);

#[cfg(any(test, feature = "testing"))]
impl DirectHooks {
    pub fn new(auth: Arc<HookAuth>, tx: MailboxTx, opts: EndpointOptions) -> Self {
        DirectHooks(Ctx::new(auth, tx, opts))
    }

    /// The handler for `POST /hook/{agent}/{event}` with `req`.
    pub fn call(
        &self,
        agent: &str,
        event: &str,
        req: Request,
    ) -> impl std::future::Future<Output = Response> + Send + 'static {
        let (ctx, agent, event) = (self.0.clone(), agent.to_owned(), event.to_owned());
        async move { serve_hook(ctx, &agent, &event, req).await }
    }
}

/// Owns the hold's reserved second control slot. Dropped with the slot
/// still in it (hyper drops the handler when the client closes the
/// connection), it reports `HoldClosed`.
struct HoldGuard {
    hold: HoldId,
    end: Option<ControlSlot>,
}

impl Drop for HoldGuard {
    fn drop(&mut self) {
        if let Some(slot) = self.end.take() {
            slot.send(Inbound::HoldClosed { hold: self.hold });
        }
    }
}

async fn hold(
    ctx: Ctx,
    event: HookEvent,
    generation: u64,
    permit: HoldPermit,
    held: ControlSlot,
    end: ControlSlot,
) -> Response {
    let id = ctx.next.fetch_add(1, Ordering::Relaxed);
    let (reply, receive) = oneshot::channel();
    let mut guard = HoldGuard {
        hold: id,
        end: Some(end),
    };
    // The permit now travels inside the queued `Held` (A1).
    held.send(Inbound::Held {
        event,
        hold: id,
        reply,
        generation,
        permit,
    });
    let result = tokio::time::timeout(ctx.opts.permission_hold, receive).await;
    let end = guard.end.take();
    match result {
        Ok(Ok(body)) if !body.is_empty() => {
            ([(header::CONTENT_TYPE, "application/json")], body).into_response()
        }
        // Replied empty or released: the slot is not needed.
        Ok(_) => empty(),
        Err(_) => {
            if let Some(slot) = end {
                slot.send(Inbound::HoldExpired { hold: id });
            }
            empty()
        }
    }
}

/// A listener that keeps at most `max` connections open; `accept` waits
/// for a slot. The slot lives in the connection's IO and is returned when
/// hyper drops it. One policy for TCP and Unix: the inner listener's own
/// `accept` (axum's) retries transient errors.
pub struct Capped<L> {
    inner: L,
    slots: Arc<Semaphore>,
}

/// The loopback TCP hook listener.
pub type CappedListener = Capped<TcpListener>;
/// The `hook.sock` listener (Amendment A1).
pub type CappedUnixListener = Capped<UnixListener>;

fn capped_with<L>(inner: L, max: usize) -> Capped<L> {
    Capped {
        inner,
        slots: Arc::new(Semaphore::new(max)),
    }
}

pub fn capped(listener: TcpListener, max: usize) -> CappedListener {
    capped_with(listener, max)
}

pub fn capped_unix(listener: UnixListener, max: usize) -> CappedUnixListener {
    capped_with(listener, max)
}

pub struct CappedIo<S> {
    io: S,
    _slot: OwnedSemaphorePermit,
}

impl<S: AsyncRead + Unpin> AsyncRead for CappedIo<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for CappedIo<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.io).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_shutdown(cx)
    }
}

impl<L: Listener> Listener for Capped<L> {
    type Io = CappedIo<L::Io>;
    type Addr = L::Addr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        let slot = self
            .slots
            .clone()
            .acquire_owned()
            .await
            .expect("semaphore is never closed");
        let (io, addr) = self.inner.accept().await;
        (CappedIo { io, _slot: slot }, addr)
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.inner.local_addr()
    }
}

/// Serve the shared endpoint on a Unix listener, applying the configured
/// connection cap. The caller owns socket creation, permissions and cleanup.
pub async fn serve_unix(
    listener: UnixListener,
    app: Router,
    max_connections: usize,
) -> io::Result<()> {
    axum::serve(capped_unix(listener, max_connections), app).await
}

#[cfg(test)]
mod tests {
    use super::Credential;

    #[test]
    fn credential_debug_redacts_the_token() {
        let token = "secret-hook-token";
        let credential = Credential {
            token: token.into(),
            generation: 42,
        };
        let debug = format!("{credential:?}");
        assert!(!debug.contains(token), "{debug}");
        assert!(debug.contains("<redacted>"), "{debug}");
        assert!(debug.contains("42"), "{debug}");
    }
}
