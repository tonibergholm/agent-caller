//! In-process stand-in for a T3 Code server (orchestration protocol 2), for
//! tests. Built from the gate-3 fixtures and faithful where acd depends on
//! T3: the upgrade checks, one Chunk item at a time with an Ack before the
//! next, resume from `afterSequence`, one global sequence, receipts for a
//! reused `commandId`, and the recorded failure shapes.
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use axum::Router;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State as AxumState};
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use super::wire::{DISPATCH, SUBSCRIBE_SHELL, SUBSCRIBE_THREAD};

pub const FAKE_TOKEN: &str = "fake-t3-token";
const NOW: &str = "2026-10-08T07:00:00.000Z";
const PONG: &str = r#"{"_tag":"Pong"}"#;

/// Reads `tests/fixtures/t3/<name>`.
pub fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/../../tests/fixtures/t3/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("fixture {name}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("fixture {name}: {e}"))
}

fn exit_of(name: &str) -> Value {
    fixture(name)["exit"].clone()
}

/// Replaces every `from` with `to` anywhere in `v`, keys and values alike.
fn replace_text(v: &Value, pairs: &[(&str, &str)]) -> Value {
    let escape = |s: &str| {
        let q = serde_json::to_string(s).expect("string");
        q[1..q.len() - 1].to_owned()
    };
    let mut s = v.to_string();
    for (from, to) in pairs {
        s = s.replace(&escape(from), &escape(to));
    }
    serde_json::from_str(&s).expect("json")
}

/// The recorded running Claude shell entry with `id`, `status` and
/// `pendingRuntimeRequest` replaced. `pending` is `(request id, kind)`.
pub fn shell_thread(id: &str, status: &str, pending: Option<(&str, &str)>) -> Value {
    let base = fixture("shell_chunk_thread_updated_running_pending_approval_claude.json")["values"]
        [0]["thread"]
        .clone();
    let old = base["id"].as_str().expect("thread id").to_owned();
    let created = base["pendingRuntimeRequest"]["createdAt"]
        .as_str()
        .unwrap_or(NOW)
        .to_owned();
    let mut t = replace_text(&base, &[(&old, id)]);
    t["status"] = json!(status);
    t["activityRunStatus"] = if status == "running" {
        json!("running")
    } else {
        Value::Null
    };
    t["pendingRuntimeRequest"] = match pending {
        Some((rid, kind)) => json!({"id": rid, "kind": kind, "createdAt": created}),
        None => Value::Null,
    };
    t
}

fn rekey_projection(file: &str, thread_id: &str, request_id: &str, native: Option<&str>) -> Value {
    let base = fixture(file)["values"][0]["projection"].clone();
    let old_thread = base["thread"]["id"].as_str().expect("thread id").to_owned();
    let old_request = base["runtimeRequests"]
        .as_array()
        .and_then(|rs| rs.iter().find(|r| r["status"] == "pending"))
        .and_then(|r| r["id"].as_str())
        .expect("pending runtime request")
        .to_owned();
    // The request id first: it embeds the thread id.
    let mut p = replace_text(
        &base,
        &[(&old_request, request_id), (&old_thread, thread_id)],
    );
    let active = p["thread"]["activeProviderThreadId"].clone();
    if let Some(pts) = p["providerThreads"].as_array_mut() {
        for pt in pts.iter_mut().filter(|pt| pt["id"] == active) {
            match native {
                Some(n) => pt["nativeThreadRef"]["nativeId"] = json!(n),
                None => {
                    pt["nativeThreadRef"]["nativeId"] = Value::Null;
                    pt["status"] = json!("not_loaded");
                }
            }
        }
    }
    p
}

/// The recorded Claude approval projection, re-keyed to `thread_id` and
/// `request_id`. `native` sets the provider thread's native id.
pub fn approval_projection(thread_id: &str, request_id: &str, native: Option<&str>) -> Value {
    rekey_projection(
        "thread_chunk_snapshot_waiting_approval_claude.json",
        thread_id,
        request_id,
        native,
    )
}

/// `projection` with exactly one provider thread, `provider_thread_id`, whose
/// native id is `native`, and `thread.activeProviderThreadId` set to it.
pub fn with_provider(
    mut projection: Value,
    provider_thread_id: &str,
    native: Option<&str>,
) -> Value {
    let mut pt = projection["providerThreads"]
        .as_array()
        .and_then(|a| a.first())
        .cloned()
        .unwrap_or_else(|| json!({}));
    pt["id"] = json!(provider_thread_id);
    pt["nativeThreadRef"] =
        json!({"driver": pt["driver"].clone(), "nativeId": native, "strength": "strong"});
    pt["status"] = json!(if native.is_some() {
        "active"
    } else {
        "not_loaded"
    });
    projection["providerThreads"] = json!([pt]);
    projection["thread"]["activeProviderThreadId"] = json!(provider_thread_id);
    projection
}

/// The recorded Claude question projection, re-keyed the same way.
pub fn question_projection(thread_id: &str, request_id: &str) -> Value {
    rekey_projection(
        "thread_chunk_snapshot_waiting_question_claude.json",
        thread_id,
        request_id,
        Some("d67b7431-38fa-47ca-b40d-11d54f568263"),
    )
}

/// The recorded project (`/tmp/acp4/proj`).
pub fn project() -> Value {
    fixture("shell_chunk_project_updated.json")["values"][0]["project"].clone()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reject {
    Token,
    Protocol,
}

/// Releases a dispatch reply held by `FakeT3::gate_next_dispatch`.
pub struct DispatchGate(oneshot::Sender<()>);

impl DispatchGate {
    pub fn release(self) {
        let _ = self.0.send(());
    }
}

enum Target {
    Shell,
    Thread(String),
}

struct Stream {
    target: Target,
    outbox: VecDeque<Vec<Value>>,
    /// Chunks sent and not yet acked.
    outstanding: usize,
}

enum Cmd {
    Raw(String),
    Drop,
}

struct ConnEntry {
    wake: Arc<Notify>,
    cmd: mpsc::UnboundedSender<Cmd>,
    streams: HashMap<String, Stream>,
}

enum Outcome {
    Reply(Value),
    Hang,
    Held(Value, oneshot::Receiver<()>),
}

struct State {
    reject: Option<Reject>,
    head: u64,
    threads: Vec<Value>,
    shell_log: Vec<(u64, Value)>,
    thread_log: HashMap<String, Vec<(u64, Value)>>,
    projections: HashMap<String, Value>,
    project: Value,
    conns: HashMap<u64, ConnEntry>,
    next_conn: u64,
    receipts: HashMap<String, Value>,
    dispatched: Vec<Value>,
    subscriptions: Vec<(String, Value)>,
    auth_headers: Vec<String>,
    connections: usize,
    hang_next: bool,
    fail_next: Option<Value>,
    gate_next: Option<oneshot::Receiver<()>>,
    deny_operate: bool,
    muted: bool,
    /// A shell resume below this sequence gets a full snapshot.
    compacted_at: u64,
    /// Thread ids whose streams never get `synchronized`.
    withheld: HashSet<String>,
}

struct Shared {
    state: Mutex<State>,
    stop_reading: watch::Sender<bool>,
    shutdown: watch::Sender<bool>,
    unacked_peak: AtomicUsize,
    chunks_sent: AtomicUsize,
    acks_received: AtomicUsize,
    thread_stream_peak: AtomicUsize,
}

fn exit_frame(id: &str, exit: &Value) -> String {
    json!({"_tag": "Exit", "requestId": id, "exit": exit}).to_string()
}

fn success_exit(sequence: u64) -> Value {
    json!({"_tag": "Success", "value": {"sequence": sequence}})
}

/// Rewrites `Runtime request <id> is|was ...` to the actual request id, as
/// the real server would phrase it, and the `commandId` fields.
fn rewrite_failure(v: &mut Value, request_id: &str, command_id: &str) {
    const LEAD: &str = "Runtime request ";
    match v {
        Value::String(s) => {
            if let Some(i) = s.find(LEAD) {
                let start = i + LEAD.len();
                let end = [" is ", " was "]
                    .iter()
                    .filter_map(|m| s[start..].find(m))
                    .min();
                if let Some(e) = end {
                    s.replace_range(start..start + e, request_id);
                }
            }
        }
        Value::Array(a) => a
            .iter_mut()
            .for_each(|x| rewrite_failure(x, request_id, command_id)),
        Value::Object(o) => {
            for (k, x) in o.iter_mut() {
                if k == "commandId" {
                    *x = json!(command_id);
                } else {
                    rewrite_failure(x, request_id, command_id);
                }
            }
        }
        _ => {}
    }
}

fn upsert(list: &mut Value, item: Value) {
    let Some(arr) = list.as_array_mut() else {
        *list = json!([item]);
        return;
    };
    match arr.iter_mut().find(|x| x["id"] == item["id"]) {
        Some(slot) => *slot = item,
        None => arr.push(item),
    }
}

impl State {
    fn wake_all(&self) {
        self.conns.values().for_each(|c| c.wake.notify_one());
    }

    fn emit(&mut self, to: impl Fn(&Target) -> bool, item: &Value) {
        for c in self.conns.values_mut() {
            for s in c.streams.values_mut().filter(|s| to(&s.target)) {
                s.outbox.push_back(vec![item.clone()]);
            }
        }
        self.wake_all();
    }

    fn project_only_snapshot(&self) -> Value {
        let root = self.project["workspaceRoot"].clone();
        json!({"kind": "snapshot", "snapshot": {"schemaVersion": 2, "snapshotSequence": self.head,
            "threads": [], "archivedThreads": [], "projects": [self.project]},
            "resolvedRepositoryIdentityRoots": [root]})
    }

    fn full_snapshot(&self) -> Value {
        json!({"kind": "snapshot", "snapshot": {"schemaVersion": 2, "snapshotSequence": self.head,
            "threads": self.threads, "archivedThreads": [], "projects": [self.project]}})
    }

    fn put_thread(&mut self, entry: Value) -> u64 {
        self.head += 1;
        let seq = self.head;
        match self.threads.iter_mut().find(|t| t["id"] == entry["id"]) {
            Some(slot) => *slot = entry.clone(),
            None => self.threads.push(entry.clone()),
        }
        let item = json!({"kind": "thread.updated", "sequence": seq, "location": "active", "thread": entry});
        self.shell_log.push((seq, item.clone()));
        self.emit(|t| matches!(t, Target::Shell), &item);
        seq
    }

    fn remove_thread(&mut self, id: &str) -> u64 {
        self.head += 1;
        let seq = self.head;
        self.threads.retain(|t| t["id"] != id);
        let item = json!({"kind": "thread.removed", "sequence": seq, "location": "active", "threadId": id});
        self.shell_log.push((seq, item.clone()));
        self.emit(|t| matches!(t, Target::Shell), &item);
        seq
    }

    fn push_thread_event(&mut self, thread_id: &str, kind: &str, payload: Value) -> u64 {
        self.head += 1;
        let seq = self.head;
        if let Some(p) = self.projections.get_mut(thread_id) {
            let key = match kind {
                "runtime-request.updated" => Some("runtimeRequests"),
                "turn-item.updated" => Some("turnItems"),
                "node.updated" => Some("nodes"),
                "run.updated" => Some("runs"),
                "provider-thread.updated" => Some("providerThreads"),
                _ => None,
            };
            if let Some(key) = key {
                upsert(&mut p[key], payload.clone());
            }
        }
        let item = json!({"kind": "event", "sequence": seq, "event": {
            "id": format!("event:fake:{seq}"), "threadId": thread_id, "occurredAt": NOW,
            "type": kind, "payload": payload}});
        self.thread_log
            .entry(thread_id.to_owned())
            .or_default()
            .push((seq, item.clone()));
        self.emit(
            |t| matches!(t, Target::Thread(id) if id == thread_id),
            &item,
        );
        seq
    }

    fn drop_connections(&mut self) {
        for c in self.conns.values_mut() {
            c.streams.clear();
            let _ = c.cmd.send(Cmd::Drop);
        }
    }

    /// The `afterSequence` of a subscription: `Err` for a negative value.
    fn after(&self, payload: &Value) -> Result<Option<u64>, Value> {
        match payload.get("afterSequence").and_then(Value::as_i64) {
            Some(n) if n < 0 => Err(exit_of("shell_resume_negative_afterSequence_exit.json")),
            Some(n) => Ok(Some(n as u64)),
            None => Ok(None),
        }
    }

    /// Returns the number of thread streams now open on `conn`.
    fn open_stream(
        &mut self,
        conn: u64,
        id: &str,
        target: Target,
        items: VecDeque<Vec<Value>>,
    ) -> usize {
        let mut threads = 0;
        if let Some(c) = self.conns.get_mut(&conn) {
            c.streams.insert(
                id.to_owned(),
                Stream {
                    target,
                    outbox: items,
                    outstanding: 0,
                },
            );
            threads = c
                .streams
                .values()
                .filter(|s| matches!(s.target, Target::Thread(_)))
                .count();
        }
        self.wake_all();
        threads
    }

    /// `Some(exit)` when the subscription is refused.
    fn subscribe_shell(&mut self, conn: u64, id: &str, payload: &Value) -> Option<Value> {
        let after = match self.after(payload) {
            Ok(a) => a,
            Err(exit) => return Some(exit),
        };
        let mut items = VecDeque::new();
        match after {
            Some(n) if n < self.head && n >= self.compacted_at => {
                items.push_back(vec![self.project_only_snapshot()]);
                items.extend(
                    self.shell_log
                        .iter()
                        .filter(|(s, _)| *s > n)
                        .map(|(_, i)| vec![i.clone()]),
                );
            }
            _ => {
                // One Chunk with two values, as recorded.
                items.push_back(vec![self.full_snapshot(), self.project_only_snapshot()]);
            }
        }
        if payload["requestCompletionMarker"] == true {
            items.push_back(vec![json!({"kind": "synchronized"})]);
        }
        self.open_stream(conn, id, Target::Shell, items);
        None
    }

    /// `Ok(n)`: the stream opened and `conn` now has `n` thread streams.
    fn subscribe_thread(&mut self, conn: u64, id: &str, payload: &Value) -> Result<usize, Value> {
        let thread_id = payload["threadId"].as_str().unwrap_or("").to_owned();
        let after = self.after(payload)?;
        let Some(projection) = self.projections.get(&thread_id) else {
            let mut exit = exit_of("thread_subscribe_unknown_thread_exit.json");
            exit = replace_text(&exit, &[("nope", &thread_id)]);
            return Err(exit);
        };
        let mut items = VecDeque::new();
        match after {
            Some(n) if n < self.head => items.extend(
                self.thread_log
                    .get(&thread_id)
                    .into_iter()
                    .flatten()
                    .filter(|(s, _)| *s > n)
                    .map(|(_, i)| vec![i.clone()]),
            ),
            _ => items.push_back(vec![
                json!({"kind": "snapshot", "snapshotSequence": self.head,
                "projection": projection}),
            ]),
        }
        if payload["requestCompletionMarker"] == true && !self.withheld.contains(&thread_id) {
            items.push_back(vec![json!({"kind": "synchronized"})]);
        }
        Ok(self.open_stream(conn, id, Target::Thread(thread_id), items))
    }

    fn dispatch(&mut self, payload: &Value) -> Outcome {
        if self.deny_operate {
            return Outcome::Reply(exit_of("rpc_exit_fail_missing_scope_operate.json"));
        }
        self.dispatched.push(payload.clone());
        let command_id = payload["commandId"].as_str().map(str::to_owned);
        if let Some(receipt) = command_id.as_ref().and_then(|c| self.receipts.get(c)) {
            return Outcome::Reply(receipt.clone());
        }
        if self.hang_next {
            self.hang_next = false;
            return Outcome::Hang;
        }
        let exit = match self.fail_next.take() {
            Some(mut exit) => {
                if let Some(req) = payload["requestId"].as_str() {
                    rewrite_failure(&mut exit, req, command_id.as_deref().unwrap_or(""));
                }
                exit
            }
            None => self.execute(payload),
        };
        if let Some(c) = command_id {
            self.receipts.insert(c, exit.clone());
        }
        match self.gate_next.take() {
            Some(rx) => Outcome::Held(exit, rx),
            None => Outcome::Reply(exit),
        }
    }

    fn execute(&mut self, payload: &Value) -> Value {
        match payload["type"].as_str() {
            Some("runtime-request.respond") => self.respond(payload),
            Some("message.dispatch") => {
                let id = payload["threadId"].as_str().unwrap_or("");
                let seq = match self.threads.iter().find(|t| t["id"] == id).cloned() {
                    Some(mut entry) => {
                        entry["status"] = json!("running");
                        self.put_thread(entry)
                    }
                    None => self.head,
                };
                success_exit(seq)
            }
            _ => success_exit(self.head),
        }
    }

    fn respond(&mut self, payload: &Value) -> Value {
        let request_id = payload["requestId"].as_str().unwrap_or("").to_owned();
        let command_id = payload["commandId"].as_str().unwrap_or("").to_owned();
        let found = self.projections.iter().find_map(|(tid, p)| {
            p["runtimeRequests"]
                .as_array()?
                .iter()
                .find(|r| r["id"] == request_id.as_str())
                .map(|r| (tid.clone(), r.clone()))
        });
        let refusal = match &found {
            None => Some("cmd_exit_respond_fail_not_found.json"),
            Some((_, r)) => match r["status"].as_str() {
                Some("pending") => None,
                Some("cancelled") => Some("cmd_exit_respond_fail_cancelled_after_restart.json"),
                _ => Some("cmd_exit_respond_fail_already_resolved.json"),
            },
        };
        if let Some(file) = refusal {
            let mut exit = exit_of(file);
            rewrite_failure(&mut exit, &request_id, &command_id);
            return exit;
        }
        let (thread_id, mut request) = found.expect("pending request");
        request["status"] = json!("resolved");
        request["resolvedAt"] = json!(NOW);
        for key in ["decision", "answers"] {
            match payload.get(key) {
                Some(v) => request[key] = v.clone(),
                None => {
                    if let Some(o) = request.as_object_mut() {
                        o.remove(key);
                    }
                }
            }
        }
        self.push_thread_event(&thread_id, "runtime-request.updated", request);
        if let Some(mut entry) = self
            .threads
            .iter()
            .find(|t| t["id"] == thread_id.as_str())
            .cloned()
        {
            entry["pendingRuntimeRequest"] = Value::Null;
            entry["status"] = json!("running");
            self.put_thread(entry);
        }
        success_exit(self.head)
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("fake state")
    }

    /// One Chunk per stream whose previous Chunk was acked.
    fn take_ready(&self, conn: u64) -> Vec<String> {
        let mut st = self.lock();
        if st.muted {
            return vec![];
        }
        let Some(c) = st.conns.get_mut(&conn) else {
            return vec![];
        };
        let mut frames = vec![];
        for (id, s) in c.streams.iter_mut() {
            if s.outstanding == 0
                && let Some(item) = s.outbox.pop_front()
            {
                s.outstanding += 1;
                self.chunks_sent.fetch_add(1, Ordering::SeqCst);
                self.unacked_peak.fetch_max(s.outstanding, Ordering::SeqCst);
                frames.push(json!({"_tag": "Chunk", "requestId": id, "values": item}).to_string());
            }
        }
        frames
    }

    fn handle_text(
        &self,
        conn: u64,
        text: &str,
        late: &mpsc::UnboundedSender<String>,
    ) -> Vec<String> {
        let Ok(v) = serde_json::from_str::<Value>(text) else {
            return vec![];
        };
        let frames = match v {
            Value::Array(a) => a,
            o => vec![o],
        };
        let mut out = vec![];
        for f in frames {
            match f["_tag"].as_str() {
                Some("Request") => out.extend(self.request(conn, &f, late)),
                Some("Ack") => {
                    let mut st = self.lock();
                    let id = f["requestId"].as_str().unwrap_or("");
                    if let Some(s) = st.conns.get_mut(&conn).and_then(|c| c.streams.get_mut(id)) {
                        s.outstanding = s.outstanding.saturating_sub(1);
                        self.acks_received.fetch_add(1, Ordering::SeqCst);
                    }
                }
                Some("Interrupt") => {
                    let mut st = self.lock();
                    let id = f["requestId"].as_str().unwrap_or("");
                    if st
                        .conns
                        .get_mut(&conn)
                        .and_then(|c| c.streams.remove(id))
                        .is_some()
                    {
                        out.push(exit_frame(id, &exit_of("rpc_exit_interrupted_stream.json")));
                    }
                }
                Some("Ping") => out.push(PONG.to_owned()),
                _ => {}
            }
        }
        if self.lock().muted { vec![] } else { out }
    }

    fn request(&self, conn: u64, f: &Value, late: &mpsc::UnboundedSender<String>) -> Vec<String> {
        let id = f["id"].as_str().unwrap_or("").to_owned();
        let tag = f["tag"].as_str().unwrap_or("");
        let payload = &f["payload"];
        let mut st = self.lock();
        match tag {
            SUBSCRIBE_SHELL | SUBSCRIBE_THREAD => {
                st.subscriptions.push((tag.to_owned(), payload.clone()));
                let refused = if tag == SUBSCRIBE_SHELL {
                    st.subscribe_shell(conn, &id, payload)
                } else {
                    match st.subscribe_thread(conn, &id, payload) {
                        Ok(open) => {
                            self.thread_stream_peak.fetch_max(open, Ordering::SeqCst);
                            None
                        }
                        Err(exit) => Some(exit),
                    }
                };
                refused
                    .map(|exit| exit_frame(&id, &exit))
                    .into_iter()
                    .collect()
            }
            DISPATCH => match st.dispatch(payload) {
                Outcome::Reply(exit) => vec![exit_frame(&id, &exit)],
                Outcome::Hang => vec![],
                Outcome::Held(exit, release) => {
                    let frame = exit_frame(&id, &exit);
                    let late = late.clone();
                    tokio::spawn(async move {
                        let _ = release.await;
                        let _ = late.send(frame);
                    });
                    vec![]
                }
            },
            other => {
                let mut exit = exit_of("rpc_exit_die_unknown_tag.json");
                exit = replace_text(&exit, &[("orchestration.nope", other)]);
                vec![exit_frame(&id, &exit)]
            }
        }
    }
}

async fn ws_handler(
    AxumState(sh): AxumState<Arc<Shared>>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let refuse = |status: StatusCode, body: Value| {
        (
            status,
            [(CONTENT_TYPE, "application/json")],
            body.to_string(),
        )
            .into_response()
    };
    let (reject, auth_ok) = {
        let mut st = sh.lock();
        let auth = headers.get(AUTHORIZATION).and_then(|h| h.to_str().ok());
        if let Some(a) = auth {
            st.auth_headers.push(a.to_owned());
        }
        (
            st.reject,
            auth == Some(format!("Bearer {FAKE_TOKEN}").as_str()),
        )
    };
    let protocol_ok = q.get("orchestrationProtocol").map(String::as_str) == Some("2");
    if !protocol_ok || reject == Some(Reject::Protocol) {
        return refuse(
            StatusCode::UPGRADE_REQUIRED,
            fixture("http_ws_upgrade_426_protocol_incompatible.json"),
        );
    }
    if !auth_ok || reject == Some(Reject::Token) {
        return refuse(
            StatusCode::UNAUTHORIZED,
            fixture("http_ws_upgrade_401_invalid_credential_expired_ticket.json"),
        );
    }
    sh.lock().connections += 1;
    ws.on_upgrade(move |socket| serve(sh, socket))
}

async fn serve(shared: Arc<Shared>, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();
    let wake = Arc::new(Notify::new());
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
    let (late_tx, mut late_rx) = mpsc::unbounded_channel::<String>();
    let conn = {
        let mut st = shared.lock();
        let id = st.next_conn;
        st.next_conn += 1;
        st.conns.insert(
            id,
            ConnEntry {
                wake: wake.clone(),
                cmd: cmd_tx,
                streams: HashMap::new(),
            },
        );
        id
    };
    let mut stop_rx = shared.stop_reading.subscribe();
    let mut down = shared.shutdown.subscribe();
    'run: loop {
        for frame in shared.take_ready(conn) {
            if sink.send(Message::text(frame)).await.is_err() {
                break 'run;
            }
        }
        let stopped = *stop_rx.borrow_and_update();
        let mut replies = vec![];
        tokio::select! {
            _ = down.changed() => break,
            cmd = cmd_rx.recv() => match cmd {
                Some(Cmd::Raw(text)) => if !shared.lock().muted { replies.push(text) },
                Some(Cmd::Drop) | None => {
                    let close = CloseFrame { code: 1001, reason: "going away".into() };
                    let _ = sink.send(Message::Close(Some(close))).await;
                    break;
                }
            },
            late = late_rx.recv() => replies.extend(late),
            _ = wake.notified() => {}
            _ = stop_rx.changed() => {}
            msg = stream.next(), if !stopped => match msg {
                Some(Ok(Message::Text(t))) => replies = shared.handle_text(conn, t.as_str(), &late_tx),
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
        }
        for reply in replies {
            if sink.send(Message::text(reply)).await.is_err() {
                break 'run;
            }
        }
    }
    shared.lock().conns.remove(&conn);
}

/// A handle; clones share one server, which stops when the last one drops.
#[derive(Clone)]
pub struct FakeT3 {
    shared: Arc<Shared>,
    addr: SocketAddr,
    _server: Arc<Server>,
}

struct Server {
    shared: Arc<Shared>,
    task: JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shared.shutdown.send_replace(true);
        self.task.abort();
    }
}

impl FakeT3 {
    pub async fn start() -> FakeT3 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake T3");
        let addr = listener.local_addr().expect("addr");
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                reject: None,
                head: 1,
                threads: vec![],
                shell_log: vec![],
                thread_log: HashMap::new(),
                projections: HashMap::new(),
                project: project(),
                conns: HashMap::new(),
                next_conn: 1,
                receipts: HashMap::new(),
                dispatched: vec![],
                subscriptions: vec![],
                auth_headers: vec![],
                connections: 0,
                hang_next: false,
                fail_next: None,
                gate_next: None,
                deny_operate: false,
                muted: false,
                compacted_at: 0,
                withheld: HashSet::new(),
            }),
            stop_reading: watch::channel(false).0,
            shutdown: watch::channel(false).0,
            unacked_peak: AtomicUsize::new(0),
            chunks_sent: AtomicUsize::new(0),
            acks_received: AtomicUsize::new(0),
            thread_stream_peak: AtomicUsize::new(0),
        });
        let app = Router::new()
            .route("/ws", get(ws_handler))
            .with_state(shared.clone());
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        FakeT3 {
            _server: Arc::new(Server {
                shared: shared.clone(),
                task: server,
            }),
            shared,
            addr,
        }
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn reject(&self, r: Option<Reject>) {
        self.shared.lock().reject = r;
    }

    /// Shell `thread.updated`.
    pub fn put_thread(&self, entry: Value) -> u64 {
        self.shared.lock().put_thread(entry)
    }

    /// Shell `thread.removed`.
    pub fn remove_thread(&self, id: &str) -> u64 {
        self.shared.lock().remove_thread(id)
    }

    /// Sets the projection new subscriptions get; no event.
    pub fn set_projection(&self, thread_id: &str, p: Value) {
        self.shared
            .lock()
            .projections
            .insert(thread_id.to_owned(), p);
    }

    /// Applies `payload` to the stored projection and pushes the event to
    /// thread subscribers. `kind` is the event `type`.
    pub fn push_thread_event(&self, thread_id: &str, kind: &str, payload: Value) -> u64 {
        self.shared
            .lock()
            .push_thread_event(thread_id, kind, payload)
    }

    /// To live shell subscribers.
    pub fn push_project_only_snapshot(&self) {
        let mut st = self.shared.lock();
        let item = st.project_only_snapshot();
        st.emit(|t| matches!(t, Target::Shell), &item);
    }

    /// To every connection.
    pub fn send_raw(&self, text: &str) {
        let st = self.shared.lock();
        for c in st.conns.values() {
            let _ = c.cmd.send(Cmd::Raw(text.to_owned()));
        }
    }

    /// Records the next dispatch and never replies.
    pub fn hang_next_dispatch(&self) {
        self.shared.lock().hang_next = true;
    }

    /// Replies to the next dispatch with that fixture's Exit. Any
    /// `Runtime request <id>` in its messages is rewritten to the
    /// dispatched `requestId`, as the real server would phrase it.
    pub fn fail_next_dispatch(&self, exit_fixture: &str) {
        self.fail_next_dispatch_value(exit_of(exit_fixture));
    }

    /// Replies to the next dispatch with exactly this Exit value
    /// (`{"_tag":"Failure","cause":[...]}`), `requestId` filled in.
    pub fn fail_next_dispatch_value(&self, exit: Value) {
        self.shared.lock().fail_next = Some(exit);
    }

    /// Records the next dispatch and applies it, then holds the reply until
    /// the gate is released.
    pub fn gate_next_dispatch(&self) -> DispatchGate {
        let (tx, rx) = oneshot::channel();
        self.shared.lock().gate_next = Some(rx);
        DispatchGate(tx)
    }

    /// A dispatch gets the missing-scope `Fail` and is not recorded.
    pub fn deny_operate(&self, yes: bool) {
        self.shared.lock().deny_operate = yes;
    }

    /// Reads frames, sends nothing (not even a Pong).
    pub fn mute(&self, yes: bool) {
        let mut st = self.shared.lock();
        st.muted = yes;
        st.wake_all();
    }

    /// Stops reading from every socket, so the client's TCP send buffer
    /// fills and its writes block.
    pub fn stop_reading(&self, yes: bool) {
        self.shared.stop_reading.send_replace(yes);
    }

    /// Closes every socket with 1001.
    pub fn drop_connections(&self) {
        self.shared.lock().drop_connections();
    }

    /// Simulates a server restart: drops connections and marks every pending
    /// runtime request `cancelled` with `responseCapability: {type:
    /// "not_resumable"}`, logging the events and shell updates
    /// (`pendingRuntimeRequest: null`, `status: "cancelled"`) so that a
    /// resuming client sees them.
    pub fn restart(&self) {
        let mut st = self.shared.lock();
        let pending: Vec<(String, Value)> = st
            .projections
            .iter()
            .flat_map(|(tid, p)| {
                p["runtimeRequests"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|r| r["status"] == "pending")
                    .map(|r| (tid.clone(), r.clone()))
            })
            .collect();
        for (tid, mut r) in pending {
            r["status"] = json!("cancelled");
            r["responseCapability"] = json!({"type": "not_resumable",
                "reason": "Provider session was closed before this runtime request was resolved."});
            st.push_thread_event(&tid, "runtime-request.updated", r);
        }
        let waiting: Vec<Value> = st
            .threads
            .iter()
            .filter(|t| !t["pendingRuntimeRequest"].is_null())
            .cloned()
            .collect();
        for mut t in waiting {
            t["pendingRuntimeRequest"] = Value::Null;
            t["status"] = json!("cancelled");
            st.put_thread(t);
        }
        st.drop_connections();
    }

    /// `dispatchCommand` payloads, in order.
    pub fn dispatched(&self) -> Vec<Value> {
        self.shared.lock().dispatched.clone()
    }

    /// `(tag, payload)` of every subscription request.
    pub fn subscriptions(&self) -> Vec<(String, Value)> {
        self.shared.lock().subscriptions.clone()
    }

    /// Thread ids with an open stream.
    pub fn live_thread_streams(&self) -> Vec<String> {
        let st = self.shared.lock();
        let mut ids: Vec<String> = st
            .conns
            .values()
            .flat_map(|c| c.streams.values())
            .filter_map(|s| match &s.target {
                Target::Thread(id) => Some(id.clone()),
                Target::Shell => None,
            })
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }

    pub fn auth_headers(&self) -> Vec<String> {
        self.shared.lock().auth_headers.clone()
    }

    /// Accepted upgrades so far.
    pub fn connections(&self) -> usize {
        self.shared.lock().connections
    }

    /// Most Chunks ever outstanding on one stream.
    pub fn unacked_peak(&self) -> usize {
        self.shared.unacked_peak.load(Ordering::SeqCst)
    }

    /// Chunks sent so far, over all streams.
    pub fn chunks_sent(&self) -> usize {
        self.shared.chunks_sent.load(Ordering::SeqCst)
    }

    /// Acks received for a stream that was waiting for one.
    pub fn acks_received(&self) -> usize {
        self.shared.acks_received.load(Ordering::SeqCst)
    }

    pub fn head(&self) -> u64 {
        self.shared.lock().head
    }

    /// Like T3's fallback for a gap over 1000: any later shell resume below
    /// the current head gets a full snapshot instead of the log.
    pub fn compact(&self) {
        let mut st = self.shared.lock();
        st.compacted_at = st.head;
    }

    /// Most thread streams open at once on one connection.
    pub fn thread_stream_peak(&self) -> usize {
        self.shared.thread_stream_peak.load(Ordering::SeqCst)
    }

    /// That thread's streams get their snapshot and events but never
    /// `synchronized`; everything else is served normally.
    pub fn withhold_sync(&self, thread_id: &str) {
        self.shared.lock().withheld.insert(thread_id.to_owned());
    }
}
