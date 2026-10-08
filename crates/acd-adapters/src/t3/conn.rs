//! T3 effect/rpc connection over one WebSocket (orchestration protocol 2).
//!
//! Three tasks share one close signal: a reader, a writer and a keepalive.
//! Nothing the reader does waits for the writer: acks and pongs go on a
//! bounded control lane with `try_send`, and a full lane closes the
//! connection. Calls use their own lane, which the writer serves only when
//! the control lane is empty. Every socket write runs under a deadline.
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::net::TcpStream;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{Instant, sleep, timeout, timeout_at};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::http::{HeaderValue, Uri};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async_with_config};

use super::wire::{
    ExitValue, Failure, PING_FRAME, ServerFrame, ack_frame, failure, interrupt_frame, parse_frames,
    request_frame, string_fingerprint,
};
use crate::text::redact_log;

pub const CONTROL_CAPACITY: usize = 1024;
pub const CALL_CAPACITY: usize = 64;
const EVENTS_CAPACITY: usize = 256;
const MAX_MESSAGE: usize = 16 * 1024 * 1024;
const PONG_FRAME: &str = r#"{"_tag":"Pong"}"#;

const QUEUED: u8 = 0;
const WRITING: u8 = 1;
const CANCELLED: u8 = 2;
const FAILED: u8 = 3;
const WRITTEN: u8 = 4;

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Clone, Debug)]
pub struct ConnTiming {
    pub connect: Duration,
    pub ping_every: Duration,
    pub pong_timeout: Duration,
    pub write_timeout: Duration,
}

impl Default for ConnTiming {
    fn default() -> Self {
        ConnTiming {
            connect: Duration::from_secs(3),
            ping_every: Duration::from_secs(20),
            pong_timeout: Duration::from_secs(10),
            write_timeout: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("T3 rejected the token")]
    Unauthorized,
    #[error("T3 does not speak orchestration protocol 2")]
    Protocol,
    #[error("cannot reach T3")]
    Unreachable,
}

#[derive(Debug, thiserror::Error)]
pub enum CallError {
    /// Nothing was queued; the connection is closed or closing.
    #[error("not connected to T3")]
    NotConnected,
    /// The writer never took the frame, so T3 cannot have seen it.
    #[error("not sent to T3: {0}")]
    NotWritten(&'static str),
    /// The writer started the frame, or it was sent: T3 may have acted.
    #[error("no reply from T3: {0}")]
    NoReply(&'static str),
    #[error("T3 refused the call")]
    Failed(Failure),
}

#[derive(Debug)]
pub enum StreamMsg {
    Chunk {
        stream: String,
        values: Vec<Value>,
    },
    End {
        stream: String,
        failure: Option<Failure>,
    },
}

/// Checks `t3.url` and returns the WebSocket URL. Only loopback and Tailscale
/// hosts are accepted: the token goes over plain `ws`.
pub fn ws_url(base: &str) -> Result<String, String> {
    let uri: Uri = base.parse().map_err(|_| "t3.url is not a valid URL")?;
    if !matches!(uri.scheme_str(), Some("http" | "ws")) {
        return Err("t3.url must start with http:// or ws://".into());
    }
    let authority = uri.authority().ok_or("t3.url has no host")?;
    if authority.as_str().contains('@') {
        return Err("t3.url must not carry user info".into());
    }
    let port = uri.port_u16().ok_or("t3.url needs a port")?;
    if uri.path() != "/" || uri.query().is_some() {
        return Err("t3.url must be only scheme, host and port".into());
    }
    let host = uri.host().ok_or("t3.url has no host")?;
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let local = bare.eq_ignore_ascii_case("localhost")
        || bare.parse::<IpAddr>().is_ok_and(|ip| match ip {
            IpAddr::V4(v4) => {
                v4.is_loopback() || (v4.octets()[0] == 100 && v4.octets()[1] & 0xc0 == 0x40)
            }
            IpAddr::V6(v6) => v6.is_loopback() || v6.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
        });
    if !local {
        return Err("t3.url must be localhost, a loopback address or a Tailscale address".into());
    }
    Ok(format!("ws://{host}:{port}/ws?orchestrationProtocol=2"))
}

struct Ticket {
    state: Arc<AtomicU8>,
    done: oneshot::Sender<()>,
}

struct Outgoing {
    text: String,
    /// Set for calls: the verdict on whether the frame went out.
    ticket: Option<Ticket>,
    /// Set for pings: told when the write started, which is when the pong
    /// clock starts.
    written: Option<oneshot::Sender<Instant>>,
}

impl Outgoing {
    fn control(text: String) -> Outgoing {
        Outgoing {
            text,
            ticket: None,
            written: None,
        }
    }
}

/// Test hooks. Always compiled, so the writer has one code path; only the
/// public methods that drive them are gated.
struct Hooks {
    paused: watch::Sender<bool>,
    fail_after_write: AtomicBool,
}

struct Core {
    control_tx: mpsc::Sender<Outgoing>,
    close_tx: watch::Sender<Option<&'static str>>,
    calls: Mutex<HashMap<String, oneshot::Sender<ExitValue>>>,
    streams: Mutex<HashSet<String>>,
    next: AtomicU64,
    last_rx: Mutex<Instant>,
    hooks: Hooks,
}

struct ClosedSignal(watch::Receiver<Option<&'static str>>);

impl ClosedSignal {
    async fn wait(&mut self) {
        let _ = self.0.wait_for(|v| v.is_some()).await;
    }
}

impl Core {
    fn close(&self, why: &'static str) {
        self.close_tx.send_if_modified(|v| {
            if v.is_none() {
                *v = Some(why);
                true
            } else {
                false
            }
        });
    }

    fn is_closed(&self) -> bool {
        self.close_tx.borrow().is_some()
    }

    fn closed_signal(&self) -> ClosedSignal {
        ClosedSignal(self.close_tx.subscribe())
    }

    fn next_id(&self) -> String {
        self.next.fetch_add(1, SeqCst).to_string()
    }

    /// Queues a control frame without waiting. A full lane means the writer
    /// is not making progress, so the connection closes.
    fn queue(&self, out: Outgoing) -> Result<(), CallError> {
        if self.is_closed() {
            return Err(CallError::NotConnected);
        }
        match self.control_tx.try_send(out) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                self.close("T3 write queue full");
                Err(CallError::NotConnected)
            }
            Err(TrySendError::Closed(_)) => Err(CallError::NotConnected),
        }
    }

    fn control(&self, text: String) -> Result<(), CallError> {
        self.queue(Outgoing::control(text))
    }

    fn touch(&self) {
        *self.last_rx.lock().expect("last_rx") = Instant::now();
    }

    fn last_rx(&self) -> Instant {
        *self.last_rx.lock().expect("last_rx")
    }
}

struct ConnInner {
    core: Arc<Core>,
    calls_tx: mpsc::Sender<Outgoing>,
}

impl Drop for ConnInner {
    fn drop(&mut self) {
        self.core.close("connection dropped");
    }
}

#[derive(Clone)]
pub struct Conn {
    inner: Arc<ConnInner>,
}

/// Holds the writer back while it lives (test hook).
#[cfg(any(test, feature = "testing"))]
pub struct WriterGate(Arc<Core>);

#[cfg(any(test, feature = "testing"))]
impl WriterGate {
    pub fn release(self) {}
}

#[cfg(any(test, feature = "testing"))]
impl Drop for WriterGate {
    fn drop(&mut self) {
        self.0.hooks.paused.send_replace(false);
    }
}

/// Removes a call's reply slot when the caller's future ends.
struct ForgetCall(Arc<Core>, String);
impl Drop for ForgetCall {
    fn drop(&mut self) {
        self.0.calls.lock().expect("calls").remove(&self.1);
    }
}

/// Cancels a queued frame when the caller's future is dropped. A frame the
/// writer already took (`WRITING`) is not affected: it may still take effect.
struct CancelOnDrop(Arc<AtomicU8>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        let _ = self.0.compare_exchange(QUEUED, CANCELLED, SeqCst, SeqCst);
    }
}

pub async fn connect(
    ws_url: &str,
    token: &str,
    timing: &ConnTiming,
) -> Result<(Conn, mpsc::Receiver<StreamMsg>), ConnectError> {
    if token.is_empty() || !token.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        return Err(ConnectError::Unauthorized);
    }
    let unreachable = |e: &dyn std::fmt::Display| {
        tracing::debug!(error = %redact_log(&string_fingerprint(&e.to_string()).to_string()), "T3 connect failed");
        ConnectError::Unreachable
    };
    let mut request = ws_url.into_client_request().map_err(|e| unreachable(&e))?;
    let mut bearer = HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|_| ConnectError::Unauthorized)?;
    bearer.set_sensitive(true);
    request.headers_mut().insert(AUTHORIZATION, bearer);
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE))
        .max_frame_size(Some(MAX_MESSAGE));
    let ws = match timeout(
        timing.connect,
        connect_async_with_config(request, Some(config), true),
    )
    .await
    {
        Err(_) => return Err(unreachable(&"connect timed out")),
        Ok(Ok((ws, _response))) => ws,
        Ok(Err(tungstenite::Error::Http(resp))) => {
            return Err(match resp.status().as_u16() {
                401 => ConnectError::Unauthorized,
                426 => ConnectError::Protocol,
                s => unreachable(&format!("HTTP status {s}")),
            });
        }
        Ok(Err(e)) => return Err(unreachable(&e)),
    };
    Ok(start(ws, timing))
}

fn start(ws: Ws, timing: &ConnTiming) -> (Conn, mpsc::Receiver<StreamMsg>) {
    let (sink, stream) = ws.split();
    let (control_tx, control_rx) = mpsc::channel(CONTROL_CAPACITY);
    let (calls_tx, calls_rx) = mpsc::channel(CALL_CAPACITY);
    let (events_tx, events_rx) = mpsc::channel(EVENTS_CAPACITY);
    let core = Arc::new(Core {
        control_tx,
        close_tx: watch::channel(None).0,
        calls: Mutex::new(HashMap::new()),
        streams: Mutex::new(HashSet::new()),
        next: AtomicU64::new(1),
        last_rx: Mutex::new(Instant::now()),
        hooks: Hooks {
            paused: watch::channel(false).0,
            fail_after_write: AtomicBool::new(false),
        },
    });
    tokio::spawn(reader(stream, core.clone(), events_tx));
    tokio::spawn(writer(
        sink,
        control_rx,
        calls_rx,
        core.clone(),
        timing.write_timeout,
    ));
    tokio::spawn(keepalive(core.clone(), timing.clone()));
    let conn = Conn {
        inner: Arc::new(ConnInner { core, calls_tx }),
    };
    (conn, events_rx)
}

async fn reader(mut stream: SplitStream<Ws>, core: Arc<Core>, events: mpsc::Sender<StreamMsg>) {
    let mut closed = core.closed_signal();
    loop {
        let msg = tokio::select! {
            _ = closed.wait() => break,
            m = stream.next() => m,
        };
        let text = match msg {
            Some(Ok(Message::Text(t))) => t,
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {
                core.touch();
                continue;
            }
            Some(Ok(Message::Binary(_) | Message::Frame(_))) => {
                core.close("T3 sent a frame acd cannot read");
                break;
            }
            Some(Ok(Message::Close(_))) | Some(Err(_)) | None => {
                core.close("connection to T3 lost");
                break;
            }
        };
        core.touch();
        let frames = match parse_frames(text.as_str()) {
            Ok(f) => f,
            Err(_) => {
                tracing::debug!(frame = %redact_log(&string_fingerprint(text.as_str()).to_string()), "T3 frame cannot be read");
                core.close("T3 sent a frame acd cannot read");
                break;
            }
        };
        for frame in frames {
            if !handle_frame(&core, &events, &mut closed, frame).await {
                break;
            }
        }
    }
    // Dropping the reply slots and the events sender tells everyone waiting.
    core.calls.lock().expect("calls").clear();
}

/// Returns false when the reader should stop.
async fn handle_frame(
    core: &Core,
    events: &mpsc::Sender<StreamMsg>,
    closed: &mut ClosedSignal,
    frame: ServerFrame,
) -> bool {
    match frame {
        ServerFrame::Chunk { request_id, values } => {
            let known = core.streams.lock().expect("streams").contains(&request_id);
            if known {
                let msg = StreamMsg::Chunk {
                    stream: request_id.clone(),
                    values,
                };
                tokio::select! {
                    r = events.send(msg) => if r.is_err() {
                        core.close("event consumer gone");
                        return false;
                    },
                    _ = closed.wait() => return false,
                }
            }
            // An unknown stream was interrupted; ack it so T3 stops waiting.
            core.control(ack_frame(&request_id)).is_ok()
        }
        ServerFrame::Exit { request_id, exit } => {
            let slot = core.calls.lock().expect("calls").remove(&request_id);
            if let Some(slot) = slot {
                let _ = slot.send(exit);
                return true;
            }
            let was_stream = core.streams.lock().expect("streams").remove(&request_id);
            if was_stream {
                let failure = match exit {
                    ExitValue::Success { .. } => None,
                    ExitValue::Failure { cause } => Some(failure(&cause)),
                    ExitValue::Other => Some(Failure::Unknown),
                };
                let msg = StreamMsg::End {
                    stream: request_id,
                    failure,
                };
                tokio::select! {
                    r = events.send(msg) => if r.is_err() {
                        core.close("event consumer gone");
                        return false;
                    },
                    _ = closed.wait() => return false,
                }
            }
            true
        }
        ServerFrame::Ping {} => core.control(PONG_FRAME.to_owned()).is_ok(),
        ServerFrame::Pong {} | ServerFrame::Other => true,
    }
}

async fn writer(
    mut sink: SplitSink<Ws, Message>,
    mut control_rx: mpsc::Receiver<Outgoing>,
    mut calls_rx: mpsc::Receiver<Outgoing>,
    core: Arc<Core>,
    write_timeout: Duration,
) {
    let mut closed = core.closed_signal();
    let mut paused = core.hooks.paused.subscribe();
    loop {
        if core.is_closed() {
            break;
        }
        let hold = *paused.borrow_and_update();
        let out = tokio::select! {
            biased;
            Some(o) = control_rx.recv(), if !hold => o,
            Some(o) = calls_rx.recv(), if !hold => o,
            _ = paused.changed() => continue,
            _ = closed.wait() => break,
        };
        let Outgoing {
            text,
            ticket,
            written,
        } = out;
        if let Some(t) = &ticket {
            // A call whose caller gave up is skipped.
            if t.state
                .compare_exchange(QUEUED, WRITING, SeqCst, SeqCst)
                .is_err()
            {
                continue;
            }
        }
        let started = Instant::now();
        let sent = tokio::select! {
            r = timeout(write_timeout, sink.send(Message::text(text))) => r,
            _ = closed.wait() => {
                if let Some(t) = ticket {
                    let _ = t.done.send(());
                }
                break;
            }
        };
        match sent {
            Ok(Ok(())) => {
                let mut fail_now = false;
                if let Some(t) = &ticket {
                    fail_now = core.hooks.fail_after_write.swap(false, SeqCst);
                    t.state
                        .store(if fail_now { FAILED } else { WRITTEN }, SeqCst);
                }
                if let Some(w) = written {
                    let _ = w.send(started);
                }
                if let Some(t) = ticket {
                    let _ = t.done.send(());
                }
                if fail_now {
                    core.close("connection to T3 lost");
                    break;
                }
            }
            Ok(Err(e)) => {
                tracing::debug!(error = %redact_log(&string_fingerprint(&e.to_string()).to_string()), "T3 write failed");
                fail_ticket(ticket);
                core.close("connection to T3 lost");
                break;
            }
            Err(_) => {
                fail_ticket(ticket);
                core.close("T3 write stalled");
                break;
            }
        }
    }
    // Dropping the sink and the receivers ends the socket and wakes callers.
}

fn fail_ticket(ticket: Option<Ticket>) {
    if let Some(t) = ticket {
        t.state.store(FAILED, SeqCst);
        let _ = t.done.send(());
    }
}

async fn keepalive(core: Arc<Core>, timing: ConnTiming) {
    let mut closed = core.closed_signal();
    loop {
        tokio::select! {
            _ = sleep(timing.ping_every) => {}
            _ = closed.wait() => return,
        }
        let (tx, rx) = oneshot::channel();
        let ping = Outgoing {
            text: PING_FRAME.to_owned(),
            ticket: None,
            written: Some(tx),
        };
        if core.queue(ping).is_err() {
            return;
        }
        // The pong clock starts when the ping is written, not when it is
        // queued: a stuck writer is the write timeout's business.
        let sent_at = tokio::select! {
            r = rx => match r { Ok(at) => at, Err(_) => return },
            _ = closed.wait() => return,
        };
        tokio::select! {
            _ = sleep(timing.pong_timeout) => {}
            _ = closed.wait() => return,
        }
        if core.last_rx() < sent_at {
            core.close("T3 did not answer a ping");
            return;
        }
    }
}

impl Conn {
    fn core(&self) -> &Arc<Core> {
        &self.inner.core
    }

    /// Registers the stream and queues the request on the control lane. It
    /// never waits for the writer.
    pub fn subscribe(&self, tag: &str, payload: Value) -> Result<String, CallError> {
        let core = self.core();
        if core.is_closed() {
            return Err(CallError::NotConnected);
        }
        let id = core.next_id();
        core.streams.lock().expect("streams").insert(id.clone());
        if let Err(e) = core.control(request_frame(&id, tag, &payload)) {
            core.streams.lock().expect("streams").remove(&id);
            return Err(e);
        }
        Ok(id)
    }

    /// Stops a stream. Its later frames are acked and dropped, and no `End`
    /// is delivered for it.
    pub fn interrupt(&self, stream: &str) {
        let core = self.core();
        let known = core.streams.lock().expect("streams").remove(stream);
        if known {
            let _ = core.control(interrupt_frame(stream));
        }
    }

    pub async fn call(
        &self,
        tag: &str,
        payload: &Value,
        deadline: Instant,
    ) -> Result<Value, CallError> {
        let core = self.core();
        if core.is_closed() {
            return Err(CallError::NotConnected);
        }
        let id = core.next_id();
        let (exit_tx, exit_rx) = oneshot::channel();
        core.calls
            .lock()
            .expect("calls")
            .insert(id.clone(), exit_tx);
        let _forget = ForgetCall(core.clone(), id.clone());
        let state = Arc::new(AtomicU8::new(QUEUED));
        let _cancel = CancelOnDrop(state.clone());
        let (done_tx, done_rx) = oneshot::channel();
        let out = Outgoing {
            text: request_frame(&id, tag, payload),
            ticket: Some(Ticket {
                state: state.clone(),
                done: done_tx,
            }),
            written: None,
        };
        let mut closed = core.closed_signal();
        tokio::select! {
            r = timeout_at(deadline, self.inner.calls_tx.send(out)) => if !matches!(r, Ok(Ok(()))) {
                return Err(CallError::NotConnected); // never queued
            },
            _ = closed.wait() => return Err(CallError::NotConnected),
        }
        tokio::select! {
            _ = timeout_at(deadline, done_rx) => {}
            _ = closed.wait() => {}
        }
        // The ticket state is the only verdict on whether the frame went out.
        // Cancel first: if the writer has not taken it, it never will.
        if state
            .compare_exchange(QUEUED, CANCELLED, SeqCst, SeqCst)
            .is_ok()
        {
            return Err(CallError::NotWritten(
                "not written before the deadline or close",
            ));
        }
        match state.load(SeqCst) {
            CANCELLED => {
                return Err(CallError::NotWritten(
                    "not written before the deadline or close",
                ));
            }
            WRITTEN => {}
            // WRITING or FAILED: the writer started this frame. A send error
            // does not prove T3 received nothing.
            _ => {
                return Err(CallError::NoReply(
                    "the write started but was not confirmed",
                ));
            }
        }
        tokio::select! {
            biased;
            r = timeout_at(deadline, exit_rx) => match r {
                Ok(Ok(ExitValue::Success { value })) => Ok(value),
                Ok(Ok(ExitValue::Failure { cause })) => Err(CallError::Failed(failure(&cause))),
                Ok(Ok(ExitValue::Other)) => Err(CallError::NoReply("reply not understood")),
                Ok(Err(_)) => Err(CallError::NoReply("connection closed before the reply")),
                Err(_) => Err(CallError::NoReply("no reply before the deadline")),
            },
            _ = closed.wait() => Err(CallError::NoReply("connection closed before the reply")),
        }
    }

    /// The first reason wins.
    pub fn close(&self, why: &'static str) {
        self.core().close(why);
    }

    pub fn is_closed(&self) -> bool {
        self.core().is_closed()
    }

    /// Resolves with the reason once the connection is closed.
    pub async fn closed(&self) -> &'static str {
        let mut rx = self.core().close_tx.subscribe();
        let reason = rx.wait_for(|v| v.is_some()).await.map(|v| *v);
        reason.ok().flatten().unwrap_or("connection closed")
    }

    /// The writer takes no frame until the gate is released or dropped.
    #[cfg(any(test, feature = "testing"))]
    pub fn pause_writer(&self) -> WriterGate {
        self.core().hooks.paused.send_replace(true);
        WriterGate(self.core().clone())
    }

    /// The writer sends the next call frame completely, then reports a send
    /// error and closes the connection.
    #[cfg(any(test, feature = "testing"))]
    pub fn fail_after_write_next(&self) {
        self.core().hooks.fail_after_write.store(true, SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unreadable_frame_fingerprint_omits_frame_text() {
        let frame = "not json: rm -rf /tmp/zz; token=top-secret";
        let detail = string_fingerprint(frame).to_string();
        assert!(!detail.contains("rm -rf /tmp/zz"), "{detail}");
        assert!(!detail.contains("top-secret"), "{detail}");
        assert!(detail.contains("\"bytes\""), "{detail}");
        assert!(detail.contains("\"sha256\""), "{detail}");
    }

    #[tokio::test]
    async fn server_json_ping_queues_client_pong() {
        let (tx, mut rx) = mpsc::channel(1);
        let core = Core {
            control_tx: tx,
            close_tx: watch::channel(None).0,
            calls: Mutex::new(HashMap::new()),
            streams: Mutex::new(HashSet::new()),
            next: AtomicU64::new(0),
            last_rx: Mutex::new(Instant::now()),
            hooks: Hooks {
                paused: watch::channel(false).0,
                fail_after_write: AtomicBool::new(false),
            },
        };
        let (event_tx, _events) = mpsc::channel(1);
        let (closed_tx, closed_rx) = watch::channel(None);
        let mut closed = ClosedSignal(closed_rx);
        assert!(handle_frame(&core, &event_tx, &mut closed, ServerFrame::Ping {}).await);
        let queued = rx.recv().await.unwrap();
        assert_eq!(queued.text, PONG_FRAME);
        drop(closed_tx);
    }
}
