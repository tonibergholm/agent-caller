//! Shared rig for the producer API tests: a manual clock, a Unix socket and the event stream.
#![allow(dead_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use acd_adapters::external::{ExternalAdapter, ExternalConfig};
use acd_adapters::hooks::endpoint::{Credential, HookAuth};
use acd_core::{Core, testing::test_machine};
use acd_protocol::*;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast;

pub const TOKEN: &str = "tok-external-0123456789";
pub const LEASE: u64 = 60_000;
pub const START_MS: i64 = 1_760_000_000_000;
pub const WALL_START_MS: i64 = 1_790_000_000_000;

pub struct Rig {
    pub core: Arc<Core>,
    pub adapter: Arc<ExternalAdapter>,
    pub clock: Arc<AtomicI64>,
    pub wall: Arc<AtomicI64>,
    pub rx: broadcast::Receiver<ServerMessage>,
    pub sock: std::path::PathBuf,
    pub next_command: u32,
    pub _dir: tempfile::TempDir,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub request_id: String,
    pub by: ResolvedBy,
    pub option_id: Option<String>,
    pub reason: Option<ResolveReason>,
    pub message: Option<String>,
}

pub async fn rig() -> Rig {
    let clock = Arc::new(AtomicI64::new(START_MS));
    let c = clock.clone();
    let wall = Arc::new(AtomicI64::new(WALL_START_MS));
    let w = wall.clone();
    let adapter = ExternalAdapter::new(ExternalConfig {
        clock: Arc::new(move || c.load(Ordering::SeqCst)),
        wall: Arc::new(move || w.load(Ordering::SeqCst)),
        tick: Duration::from_secs(3600),
    });
    let core = Core::new(test_machine(), vec![adapter.clone()]);
    adapter.start(core.clone());
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("hook.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    let auth = Arc::new(HookAuth::new(Credential {
        token: TOKEN.into(),
        generation: 1,
        expires_at_ms: None,
    }));
    let app = adapter.router(auth);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (_, rx) = core.attach(None);
    Rig {
        core,
        adapter,
        clock,
        wall,
        rx,
        sock,
        next_command: 0,
        _dir: dir,
    }
}

pub async fn http(
    sock: &std::path::Path,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: &str,
) -> (u16, Value) {
    let mut stream = UnixStream::connect(sock).await.unwrap();
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: acd\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(token) = token {
        request.push_str(&format!("X-Acd-Token: {token}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(body);
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).await.unwrap();
    let response = String::from_utf8_lossy(&bytes).into_owned();
    let status = response.split_whitespace().nth(1).unwrap().parse().unwrap();
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, b)| b)
        .unwrap_or("");
    (status, serde_json::from_str(body).unwrap_or(Value::Null))
}

impl Rig {
    pub fn advance(&self, ms: i64) {
        self.clock.fetch_add(ms, Ordering::SeqCst);
        self.wall.fetch_add(ms, Ordering::SeqCst);
    }

    pub fn wall_now(&self) -> i64 {
        self.wall.load(Ordering::SeqCst)
    }

    /// The system slept: the wall clock moved, the monotonic one did not.
    pub fn sleep_through(&self, ms: i64) {
        self.wall.fetch_add(ms, Ordering::SeqCst);
    }

    pub async fn put(&self, pid: &str, body: Value) -> (u16, Value) {
        http(
            &self.sock,
            "PUT",
            &format!("/external/v1/producers/{pid}/state"),
            Some(TOKEN),
            &body.to_string(),
        )
        .await
    }

    pub async fn ok(&self, pid: &str, body: Value) -> Value {
        let (status, body) = self.put(pid, body).await;
        assert_eq!(status, 200, "{body}");
        body
    }

    pub async fn get_answers(&self, pid: &str, wait_ms: u64) -> (u16, Value) {
        http(
            &self.sock,
            "GET",
            &format!("/external/v1/producers/{pid}/answers?wait_ms={wait_ms}"),
            Some(TOKEN),
            "",
        )
        .await
    }

    pub async fn stop(&self, pid: &str) -> u16 {
        http(
            &self.sock,
            "DELETE",
            &format!("/external/v1/producers/{pid}"),
            Some(TOKEN),
            "",
        )
        .await
        .0
    }

    pub fn agents(&self) -> Vec<Agent> {
        match self.core.attach(None).0.into_iter().next() {
            Some(ServerMessage::Snapshot { agents, .. }) => agents
                .into_iter()
                .filter(|a| a.source == Source::External)
                .collect(),
            other => panic!("no snapshot: {other:?}"),
        }
    }

    pub fn agent(&self, title: &str) -> Option<Agent> {
        self.agents().into_iter().find(|a| a.title == title)
    }

    pub fn request(&self, title: &str) -> Request {
        self.agent(title)
            .and_then(|a| a.pending)
            .unwrap_or_else(|| panic!("no pending request on {title}"))
    }

    pub async fn answer(&mut self, request: &Request, option: &str) -> CommandResult {
        self.next_command += 1;
        self.core
            .handle(
                "client",
                ClientMessage::RequestAnswer {
                    id: format!("c{}", self.next_command),
                    request_id: request.id.clone(),
                    revision: Some(request.revision),
                    option_id: Some(option.into()),
                    text: None,
                },
            )
            .await
    }

    pub async fn answer_text(&mut self, request: &Request, text: &str) -> CommandResult {
        self.next_command += 1;
        self.core
            .handle(
                "client",
                ClientMessage::RequestAnswer {
                    id: format!("c{}", self.next_command),
                    request_id: request.id.clone(),
                    revision: Some(request.revision),
                    option_id: None,
                    text: Some(text.into()),
                },
            )
            .await
    }

    pub fn resolved(&mut self) -> Vec<Resolved> {
        let mut out = Vec::new();
        while let Ok(message) = self.rx.try_recv() {
            if let ServerMessage::RequestResolved {
                request_id,
                by,
                option_id,
                reason,
                message,
                ..
            } = message
            {
                out.push(Resolved {
                    request_id,
                    by,
                    option_id,
                    reason,
                    message,
                });
            }
        }
        out
    }

    pub fn snapshot(&self) -> Value {
        serde_json::to_value(self.agents()).unwrap()
    }
}

pub fn item(key: &str, agent_key: &str, agent_title: &str) -> Value {
    json!({
        "key": key,
        "agent_key": agent_key,
        "agent": {"title": agent_title, "project": "engine", "cwd": "/work/engine"},
        "title": format!("{agent_title} needs you"),
        "detail": "details",
        "question": {"header": "Parked", "text": "What now?"},
        "options": [
            {"id": "approve", "label": "Approve", "description": "go on"},
            {"id": "abandon", "label": "Abandon", "confirm": true}
        ],
        "free_text": false
    })
}

pub fn state(items: Vec<Value>) -> Value {
    json!({"lease_ms": LEASE, "items": items})
}

pub fn with_acks(items: Vec<Value>, acks: Vec<Value>) -> Value {
    json!({"lease_ms": LEASE, "items": items, "acks": acks})
}

pub fn ack(answer_id: &str, outcome: &str, message: Option<&str>) -> Value {
    match message {
        Some(m) => json!({"answer_id": answer_id, "outcome": outcome, "message": m}),
        None => json!({"answer_id": answer_id, "outcome": outcome}),
    }
}

pub fn only_answer(body: &Value) -> Value {
    let answers = body["answers"].as_array().unwrap();
    assert_eq!(answers.len(), 1, "{body}");
    answers[0].clone()
}
