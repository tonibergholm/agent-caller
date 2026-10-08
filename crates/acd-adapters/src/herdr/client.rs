//! One-shot herdr socket client: every request opens a fresh connection,
//! writes one line, reads one line and drops the stream.
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::time::{Instant, timeout_at};

use super::wire::{AgentInfo, Event, Pong, parse_event};

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
pub const CALL_TIMEOUT: Duration = Duration::from_secs(3);

/// Error codes herdr returns before it acts. Each is a check that runs ahead
/// of any side effect:
/// - `invalid_request`, `invalid_params` and `invalid_key` are request
///   validation errors;
/// - `agent_not_found` is a failed target lookup;
/// - `agent_blocked` is a precondition check.
///
/// Any other code after a complete write may have acted, so it counts as
/// reached. The Task 8 and 9 smoke tests confirm the two that matter most:
/// `agent_blocked` leaves no prompt text and `invalid_key` sends no key.
pub const KNOWN_REFUSALS: &[&str] = &[
    "agent_blocked",
    "agent_not_found",
    "invalid_request",
    "invalid_key",
    "invalid_params",
];

#[derive(Debug, thiserror::Error)]
pub enum CallError {
    /// Could not connect; nothing was sent.
    #[error("connect failed: {0}")]
    Connect(io::Error),
    /// The request line was not completely written; nothing was dispatched.
    #[error("write failed: {0}")]
    Write(io::Error),
    /// Written, then timeout, EOF, a malformed reply, a reply with the wrong
    /// `id`, or an unexpected `result.type`. The outcome is unknown.
    #[error("no usable reply: {0}")]
    NoReply(String),
    #[error("herdr error {code}: {message}")]
    Server { code: String, message: String },
}

impl CallError {
    /// Whether the request may have reached the agent.
    pub fn reached_agent(&self) -> bool {
        match self {
            CallError::NoReply(_) => true,
            CallError::Server { code, .. } => !KNOWN_REFUSALS.contains(&code.as_str()),
            CallError::Connect(_) | CallError::Write(_) => false,
        }
    }
}

static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(1);

fn next_request_id() -> String {
    format!("acd-{}", REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed))
}

#[derive(Clone, Debug)]
pub struct Herdr {
    path: PathBuf,
}

/// An open connection; nothing has been sent yet.
pub struct Connected {
    r: BufReader<OwnedReadHalf>,
    w: OwnedWriteHalf,
    deadline: Instant,
}

/// A request that was completely written and flushed.
pub struct Written {
    r: BufReader<OwnedReadHalf>,
    _w: OwnedWriteHalf,
    id: String,
    deadline: Instant,
    at: Instant,
}

impl Herdr {
    pub fn new(path: PathBuf) -> Herdr {
        Herdr { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `CALL_TIMEOUT` is one absolute deadline for connect, write and reply.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, CallError> {
        self.call_until(method, params, Instant::now() + CALL_TIMEOUT).await
    }

    /// Like `call`, but with the caller's absolute deadline when that is earlier.
    pub async fn call_until(&self, method: &str, params: Value, deadline: Instant) -> Result<Value, CallError> {
        let connected = self.connect_until(deadline).await?;
        connected.write(method, params).await?.reply().await
    }

    /// Stage 1 of a staged call: open the connection. Connect may use at most
    /// `min(CONNECT_TIMEOUT, remaining)`.
    pub async fn connect_until(&self, deadline: Instant) -> Result<Connected, CallError> {
        let now = Instant::now();
        let deadline = deadline.min(now + CALL_TIMEOUT);
        let connect_by = deadline.min(now + CONNECT_TIMEOUT);
        let stream = timeout_at(connect_by, UnixStream::connect(&self.path))
            .await
            .map_err(|_| CallError::Connect(io::Error::new(io::ErrorKind::TimedOut, "connect timed out")))?
            .map_err(CallError::Connect)?;
        let (r, w) = stream.into_split();
        Ok(Connected { r: BufReader::new(r), w, deadline })
    }

    pub async fn ping(&self) -> Result<Pong, CallError> {
        let v = self.call("ping", json!({})).await?;
        serde_json::from_value(v).map_err(|e| CallError::NoReply(format!("bad pong: {e}")))
    }

    pub async fn list(&self) -> Result<Vec<AgentInfo>, CallError> {
        let v = self.call("agent.list", json!({})).await?;
        serde_json::from_value(v["agents"].clone()).map_err(|e| CallError::NoReply(format!("bad agent list: {e}")))
    }

    /// `Ok(None)` on `agent_not_found`.
    pub async fn get(&self, pane_id: &str) -> Result<Option<AgentInfo>, CallError> {
        match self.call("agent.get", json!({"target": pane_id})).await {
            Ok(v) => serde_json::from_value(v["agent"].clone())
                .map(Some)
                .map_err(|e| CallError::NoReply(format!("bad agent: {e}"))),
            Err(CallError::Server { code, .. }) if code == "agent_not_found" => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// `agent.read` with `strip_ansi: true`; returns `read.text`.
    pub async fn read(&self, pane_id: &str, source: &str, lines: Option<u32>) -> Result<String, CallError> {
        let params = json!({"target": pane_id, "source": source, "lines": lines, "strip_ansi": true});
        let v = self.call("agent.read", params).await?;
        v["read"]["text"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| CallError::NoReply("read reply has no text".into()))
    }

    pub async fn prompt(&self, pane_id: &str, text: &str) -> Result<(), CallError> {
        tracing::debug!(pane_id, len = text.len(), "herdr agent.prompt");
        let v = self.call("agent.prompt", json!({"target": pane_id, "text": text})).await?;
        expect_type(&v, "agent_prompted")
    }

    /// Only the keys engine (Task 9) may call this.
    pub async fn send_keys(&self, pane_id: &str, keys: &[&str]) -> Result<(), CallError> {
        tracing::debug!(pane_id, count = keys.len(), "herdr agent.send_keys");
        let v = self.call("agent.send_keys", json!({"target": pane_id, "keys": keys})).await?;
        expect_type(&v, "ok")
    }

    /// Subscribes to `pane.created`, `pane.closed`, `pane.agent_detected` and
    /// `pane.updated`, plus `pane.agent_status_changed` for each pane.
    pub async fn subscribe(&self, panes: &[String]) -> Result<Subscription, CallError> {
        let mut subs = vec![
            json!({"type": "pane.created"}),
            json!({"type": "pane.closed"}),
            json!({"type": "pane.agent_detected"}),
            json!({"type": "pane.updated"}),
        ];
        subs.extend(panes.iter().map(|p| json!({"type": "pane.agent_status_changed", "pane_id": p})));
        let deadline = Instant::now() + CALL_TIMEOUT;
        let mut written = self.connect_until(deadline).await?.write("events.subscribe", json!({"subscriptions": subs})).await?;
        let v = written.read_reply().await?;
        expect_type(&v, "subscription_started")?;
        Ok(Subscription { r: written.r, _w: written._w, buf: String::new() })
    }
}

fn expect_type(v: &Value, want: &str) -> Result<(), CallError> {
    if v["type"].as_str() == Some(want) {
        Ok(())
    } else {
        Err(CallError::NoReply(format!("unexpected result type, wanted {want}")))
    }
}

impl Connected {
    /// Returns only after the whole line, newline included, is written and
    /// flushed. A failure is `Write`: herdr frames requests by newline, so a
    /// partial line is never dispatched.
    pub async fn write(mut self, method: &str, params: Value) -> Result<Written, CallError> {
        let id = next_request_id();
        let mut line = serde_json::to_vec(&json!({"id": id, "method": method, "params": params})).expect("json");
        line.push(b'\n');
        timeout_at(self.deadline, async {
            self.w.write_all(&line).await?;
            self.w.flush().await
        })
        .await
        .map_err(|_| CallError::Write(io::Error::new(io::ErrorKind::TimedOut, "write timed out")))?
        .map_err(CallError::Write)?;
        Ok(Written { r: self.r, _w: self.w, id, deadline: self.deadline, at: Instant::now() })
    }
}

impl Written {
    /// When the write completed.
    pub fn at(&self) -> Instant {
        self.at
    }

    /// Any failure here is `NoReply` or `Server`.
    pub async fn reply(mut self) -> Result<Value, CallError> {
        self.read_reply().await
    }

    async fn read_reply(&mut self) -> Result<Value, CallError> {
        let mut reply = String::new();
        let n = timeout_at(self.deadline, self.r.read_line(&mut reply))
            .await
            .map_err(|_| CallError::NoReply("timed out".into()))?
            .map_err(|e| CallError::NoReply(e.to_string()))?;
        if n == 0 {
            return Err(CallError::NoReply("connection closed".into()));
        }
        let v: Value = serde_json::from_str(&reply).map_err(|e| CallError::NoReply(format!("bad reply: {e}")))?;
        if v["id"].as_str() != Some(self.id.as_str()) {
            return Err(CallError::NoReply("reply id does not match".into()));
        }
        if let Some(err) = v.get("error") {
            return Err(CallError::Server {
                code: err["code"].as_str().unwrap_or("unknown").into(),
                message: err["message"].as_str().unwrap_or("").into(),
            });
        }
        v.get("result").cloned().ok_or_else(|| CallError::NoReply("reply has no result".into()))
    }
}

/// A live event stream. Ends (`None`) on EOF or an I/O error.
pub struct Subscription {
    r: BufReader<OwnedReadHalf>,
    _w: OwnedWriteHalf,
    // Kept across calls so a cancelled `next` loses no partial line.
    buf: String,
}

impl Subscription {
    pub async fn next(&mut self) -> Option<Event> {
        loop {
            match self.r.read_line(&mut self.buf).await {
                Ok(0) | Err(_) => return None,
                Ok(_) if !self.buf.ends_with('\n') => continue,
                Ok(_) => {
                    let event = parse_event(self.buf.trim_end());
                    self.buf.clear();
                    return Some(event);
                }
            }
        }
    }
}
