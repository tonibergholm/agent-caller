//! In-process stand-in for herdr 0.9.3 on a Unix socket, for tests.
//!
//! Faithful where it matters: one request per connection, closed after the
//! reply (a second line on the same connection is counted, not served).
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::sync::{oneshot, watch};
use tokio::task::{JoinHandle, JoinSet};

type KeysHook = Arc<dyn Fn(&FakeHerdr, &str, &[String]) + Send + Sync>;

enum Hook {
    /// Apply the request, record it, never reply, hold the connection.
    Hang,
    /// Reply with an error (code, message) and apply nothing.
    Fail(String, String),
}

/// Server side of a `Gate`.
struct Held {
    arrived: watch::Sender<bool>,
    release: oneshot::Receiver<()>,
}

/// Holds one call after its request line is read, before any side effect
/// and before the reply (see `FakeHerdr::block_next`).
pub struct Gate {
    arrived: watch::Receiver<bool>,
    release: oneshot::Sender<()>,
}

impl Gate {
    /// Resolves once the call is held.
    pub async fn arrived(&self) {
        let mut rx = self.arrived.clone();
        let _ = rx.wait_for(|held| *held).await;
    }

    /// Lets the held call proceed. Dropping the gate does the same.
    pub fn release(self) {
        let _ = self.release.send(());
    }
}

struct Subscriber {
    /// Panes whose status changes this subscription asked for.
    panes: HashSet<String>,
    tx: UnboundedSender<String>,
}

#[derive(Default)]
struct State {
    agents: Vec<Value>,
    screens: HashMap<(String, String), String>,
    hooks: HashMap<String, VecDeque<Hook>>,
    gates: HashMap<String, VecDeque<Held>>,
    delays: HashMap<String, VecDeque<Duration>>,
    truncated: HashSet<(String, String)>,
    /// `(pane, source)` whose every `agent.read` fails.
    failing: HashSet<(String, String)>,
    /// `agent.read` source -> queued error codes.
    read_fails: HashMap<String, VecDeque<String>>,
    on_keys: Option<KeysHook>,
    subscribers: Vec<Subscriber>,
    calls: Vec<(tokio::time::Instant, String, Value)>,
    in_flight: usize,
    max_in_flight: usize,
    prompts: Vec<(String, String)>,
    keys_sent: Vec<(String, Vec<String>)>,
    subscriptions: Vec<Vec<String>>,
    reused: usize,
}

struct Shared {
    path: PathBuf,
    version: String,
    protocol: u32,
    state: Mutex<State>,
}

/// Owns the socket directory and the accept loop; dropping it stops both.
struct Owner {
    _dir: Option<tempfile::TempDir>,
    /// Set by `start_at`, which reuses a directory it does not own.
    cleanup: Option<PathBuf>,
    task: JoinHandle<()>,
}

impl Drop for Owner {
    fn drop(&mut self) {
        self.task.abort();
        if let Some(path) = &self.cleanup {
            let _ = std::fs::remove_file(path);
        }
    }
}

pub struct FakeHerdr {
    shared: Arc<Shared>,
    owner: Option<Arc<Owner>>,
}

pub fn fake_agent(pane_id: &str, agent: &str, status: &str) -> Value {
    json!({
        "terminal_id": format!("term_{pane_id}"),
        "agent": agent,
        "agent_status": status,
        "workspace_id": "w1",
        "tab_id": "w1:t1",
        "pane_id": pane_id,
        "focused": false,
        "state_change_seq": 1,
        "cwd": "/tmp/proj",
        "revision": 1,
    })
}

impl FakeHerdr {
    pub async fn start() -> FakeHerdr {
        FakeHerdr::start_with_version("0.9.3", 22).await
    }

    pub async fn start_with_version(version: &str, protocol: u32) -> FakeHerdr {
        // macOS limits Unix socket paths to 104 bytes; its temp dir is long.
        let dir = tempfile::Builder::new()
            .prefix("acd")
            .tempdir_in("/tmp")
            .expect("tempdir");
        let path = dir.path().join("h.sock");
        let listener = UnixListener::bind(&path).expect("bind fake herdr socket");
        let shared = Arc::new(Shared {
            path,
            version: version.to_owned(),
            protocol,
            state: Mutex::new(State::default()),
        });
        let task = tokio::spawn(accept_loop(listener, shared.clone()));
        FakeHerdr {
            shared,
            owner: Some(Arc::new(Owner {
                _dir: Some(dir),
                cleanup: None,
                task,
            })),
        }
    }

    /// A fresh server on an existing socket path, for a herdr restart. The
    /// previous server must be stopped; its state is not carried over.
    pub async fn start_at(path: &Path) -> FakeHerdr {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).expect("socket dir");
        }
        let _ = std::fs::remove_file(path);
        let listener = UnixListener::bind(path).expect("rebind fake herdr socket");
        let shared = Arc::new(Shared {
            path: path.to_path_buf(),
            version: "0.9.3".to_owned(),
            protocol: 22,
            state: Mutex::new(State::default()),
        });
        let task = tokio::spawn(accept_loop(listener, shared.clone()));
        FakeHerdr {
            shared,
            owner: Some(Arc::new(Owner {
                _dir: None,
                cleanup: Some(path.to_path_buf()),
                task,
            })),
        }
    }

    /// A second handle to the same server state, for helper tasks. It does
    /// not keep the server alive.
    pub fn clone_handle(&self) -> FakeHerdr {
        FakeHerdr {
            shared: self.shared.clone(),
            owner: None,
        }
    }

    /// The most `agent.list`, `agent.get` and `agent.read` requests that were
    /// being served at once.
    pub fn max_in_flight(&self) -> usize {
        self.st().max_in_flight
    }

    fn st(&self) -> MutexGuard<'_, State> {
        self.shared.state.lock().expect("fake state")
    }

    pub fn path(&self) -> &Path {
        &self.shared.path
    }

    /// Insert or replace by `pane_id`.
    pub fn set_agent(&self, info: Value) {
        let id = info["pane_id"].as_str().expect("pane_id").to_owned();
        let mut st = self.st();
        match st.agents.iter_mut().find(|a| a["pane_id"] == id) {
            Some(slot) => *slot = info,
            None => st.agents.push(info),
        }
    }

    /// Also pushes a `pane.agent_status_changed` frame to subscribers of that pane.
    pub fn set_status(&self, pane_id: &str, status: &str) {
        let mut st = self.st();
        set_status_locked(&mut st, pane_id, status);
    }

    /// `agent.get` then replies `agent_not_found`.
    pub fn remove_agent(&self, pane_id: &str) {
        self.st().agents.retain(|a| a["pane_id"] != pane_id);
    }

    /// `source` is `"detection"` or `"visible"` (any string works).
    pub fn set_screen(&self, pane_id: &str, source: &str, text: &str) {
        self.st()
            .screens
            .insert((pane_id.to_owned(), source.to_owned()), text.to_owned());
    }

    /// `agent.read` of this pane and source reports `truncated: value`.
    pub fn set_truncated(&self, pane_id: &str, source: &str, value: bool) {
        let key = (pane_id.to_owned(), source.to_owned());
        let mut st = self.st();
        if value {
            st.truncated.insert(key);
        } else {
            st.truncated.remove(&key);
        }
    }

    /// Every `agent.read` of this pane and source fails with
    /// `platform_error` while `value` is true.
    pub fn set_read_failing(&self, pane_id: &str, source: &str, value: bool) {
        let key = (pane_id.to_owned(), source.to_owned());
        let mut st = self.st();
        if value {
            st.failing.insert(key);
        } else {
            st.failing.remove(&key);
        }
    }

    /// The next `agent.read` of `source` (any pane) gets an error with
    /// `code`; reads of other sources are unaffected.
    pub fn fail_next_read(&self, source: &str, code: &str) {
        self.st()
            .read_fails
            .entry(source.to_owned())
            .or_default()
            .push_back(code.to_owned());
    }

    /// The next `method` request is held after its line is read, before any
    /// side effect and before the reply, until the gate is released. A
    /// `fail_next` for the same method still applies on release. If the
    /// client drops the connection while the call is held, it is discarded.
    /// `"<method>@<pane id>"` holds only a request whose `target` is that pane.
    pub fn block_next(&self, method: &str) -> Gate {
        let (arrived_tx, arrived_rx) = watch::channel(false);
        let (release_tx, release_rx) = oneshot::channel();
        self.st()
            .gates
            .entry(method.to_owned())
            .or_default()
            .push_back(Held {
                arrived: arrived_tx,
                release: release_rx,
            });
        Gate {
            arrived: arrived_rx,
            release: release_tx,
        }
    }

    /// The next `method` request takes effect at once (keys are recorded,
    /// `on_keys` runs), but its reply is sent only after `d`.
    pub fn delay_next(&self, method: &str, d: Duration) {
        self.st()
            .delays
            .entry(method.to_owned())
            .or_default()
            .push_back(d);
    }

    /// Runs on `agent.send_keys` (after recording), outside the state lock.
    pub fn on_keys(&self, f: impl Fn(&FakeHerdr, &str, &[String]) + Send + Sync + 'static) {
        self.st().on_keys = Some(Arc::new(f));
    }

    /// The next `method` request is applied and recorded, but never answered.
    pub fn hang_next(&self, method: &str) {
        self.st()
            .hooks
            .entry(method.to_owned())
            .or_default()
            .push_back(Hook::Hang);
    }

    /// The next `method` request gets `{"error":{"code":code,...}}` and has no effect.
    pub fn fail_next(&self, method: &str, code: &str) {
        self.st()
            .hooks
            .entry(method.to_owned())
            .or_default()
            .push_back(Hook::Fail(code.to_owned(), "injected failure".to_owned()));
    }

    /// `fail_next` with this error message, e.g. one that echoes input.
    pub fn fail_next_with_message(&self, method: &str, code: &str, message: &str) {
        self.st()
            .hooks
            .entry(method.to_owned())
            .or_default()
            .push_back(Hook::Fail(code.to_owned(), message.to_owned()));
    }

    pub fn push_event(&self, frame: Value) {
        self.push_raw(&frame.to_string());
    }

    pub fn push_raw(&self, line: &str) {
        self.push_chunk(&format!("{line}\n"));
    }

    /// Pushes bytes as given, with no newline added (for split-line tests).
    pub fn push_chunk(&self, chunk: &str) {
        for s in &self.st().subscribers {
            let _ = s.tx.send(chunk.to_owned());
        }
    }

    /// Closes every subscription connection.
    pub fn drop_subscribers(&self) {
        self.st().subscribers.clear();
    }

    /// Closes the listener and removes the socket file.
    pub async fn stop(self) {
        if let Some(owner) = &self.owner {
            owner.task.abort();
        }
        let _ = std::fs::remove_file(&self.shared.path);
    }

    /// Every request: `(method, params)`.
    pub fn calls(&self) -> Vec<(String, Value)> {
        self.st()
            .calls
            .iter()
            .map(|(_, m, p)| (m.clone(), p.clone()))
            .collect()
    }

    /// Like `calls`, with the arrival time of each request.
    pub fn calls_timed(&self) -> Vec<(tokio::time::Instant, String, Value)> {
        self.st().calls.clone()
    }

    pub fn prompts(&self) -> Vec<(String, String)> {
        self.st().prompts.clone()
    }

    pub fn keys_sent(&self) -> Vec<(String, Vec<String>)> {
        self.st().keys_sent.clone()
    }

    /// Per subscribe call; status entries are `"pane.agent_status_changed:<pane>"`.
    pub fn subscriptions(&self) -> Vec<Vec<String>> {
        self.st().subscriptions.clone()
    }

    pub fn connections_reused(&self) -> usize {
        self.st().reused
    }
}

fn set_status_locked(st: &mut State, pane_id: &str, status: &str) {
    let Some(agent) = st.agents.iter_mut().find(|a| a["pane_id"] == pane_id) else {
        return;
    };
    agent["agent_status"] = json!(status);
    let seq = agent["state_change_seq"].as_u64().unwrap_or(0) + 1;
    agent["state_change_seq"] = json!(seq);
    let frame = json!({
        "data": {
            "agent": agent["agent"],
            "agent_status": status,
            "pane_id": pane_id,
            "workspace_id": agent["workspace_id"],
        },
        "event": "pane.agent_status_changed",
    })
    .to_string()
        + "\n";
    for s in st.subscribers.iter().filter(|s| s.panes.contains(pane_id)) {
        let _ = s.tx.send(frame.clone());
    }
}

async fn accept_loop(listener: UnixListener, shared: Arc<Shared>) {
    let mut conns = JoinSet::new();
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        while conns.try_join_next().is_some() {}
        conns.spawn(serve(stream, shared.clone()));
    }
}

fn tracked(method: &str) -> bool {
    matches!(method, "agent.list" | "agent.get" | "agent.read")
}

enum Outcome {
    Reply(Value),
    Error(String, String),
    /// `events.subscribe` accepted: keep the connection and stream frames.
    Subscribed(Value, tokio::sync::mpsc::UnboundedReceiver<String>),
}

fn err(code: &str, message: &str) -> Outcome {
    Outcome::Error(code.into(), message.into())
}

async fn serve(stream: UnixStream, shared: Arc<Shared>) {
    let (r, mut w) = stream.into_split();
    let mut r = BufReader::new(r);
    let mut line = String::new();
    if !matches!(r.read_line(&mut line).await, Ok(n) if n > 0) {
        return;
    }
    let req: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
    let id = req["id"].clone();
    let method = req["method"].as_str().unwrap_or("").to_owned();
    let params = req["params"].clone();

    let gate = {
        let mut st = shared.state.lock().expect("fake state");
        st.calls
            .push((tokio::time::Instant::now(), method.clone(), params.clone()));
        if tracked(&method) {
            st.in_flight += 1;
            st.max_in_flight = st.max_in_flight.max(st.in_flight);
        }
        let pane_gate = params["target"]
            .as_str()
            .and_then(|t| st.gates.get_mut(&format!("{method}@{t}")))
            .and_then(VecDeque::pop_front);
        pane_gate.or_else(|| st.gates.get_mut(&method).and_then(VecDeque::pop_front))
    };
    if let Some(held) = gate {
        let _ = held.arrived.send(true);
        let mut sink = String::new();
        let released = tokio::select! {
            _ = held.release => true,
            // EOF (or anything else) from the client while held: discard.
            _ = r.read_line(&mut sink) => false,
        };
        if !released {
            if tracked(&method) {
                shared.state.lock().expect("fake state").in_flight -= 1;
            }
            return;
        }
    }
    let (hook, delay) = {
        let mut st = shared.state.lock().expect("fake state");
        (
            st.hooks.get_mut(&method).and_then(VecDeque::pop_front),
            st.delays.get_mut(&method).and_then(VecDeque::pop_front),
        )
    };
    let outcome = match hook {
        Some(Hook::Fail(code, message)) => Outcome::Error(code, message),
        hook => {
            let outcome = handle(&shared, &method, &params);
            if matches!(hook, Some(Hook::Hang)) {
                // Hold the connection until the client goes away.
                let mut sink = String::new();
                while matches!(r.read_line(&mut sink).await, Ok(n) if n > 0) {
                    sink.clear();
                }
                if tracked(&method) {
                    shared.state.lock().expect("fake state").in_flight -= 1;
                }
                return;
            }
            outcome
        }
    };

    let (frame, stream_rx) = match outcome {
        Outcome::Reply(result) => (json!({"id": id, "result": result}), None),
        Outcome::Error(code, message) => (
            json!({"id": id, "error": {"code": code, "message": message}}),
            None,
        ),
        Outcome::Subscribed(result, rx) => (json!({"id": id, "result": result}), Some(rx)),
    };
    if let Some(d) = delay {
        tokio::time::sleep(d).await;
    }
    let mut bytes = frame.to_string().into_bytes();
    bytes.push(b'\n');
    let written = w.write_all(&bytes).await;
    if tracked(&method) {
        shared.state.lock().expect("fake state").in_flight -= 1;
    }
    if written.is_err() {
        return;
    }

    if let Some(mut rx) = stream_rx {
        while let Some(frame) = rx.recv().await {
            if w.write_all(frame.as_bytes()).await.is_err() {
                return;
            }
        }
        let _ = w.shutdown().await;
        return;
    }

    // One request per connection: close, and count a second line.
    let _ = w.shutdown().await;
    line.clear();
    if let Ok(Ok(n)) =
        tokio::time::timeout(Duration::from_millis(200), r.read_line(&mut line)).await
        && n > 0
    {
        shared.state.lock().expect("fake state").reused += 1;
    }
}

fn handle(shared: &Arc<Shared>, method: &str, params: &Value) -> Outcome {
    let target = params["target"].as_str();
    let mut keys_call: Option<(KeysHook, String, Vec<String>)> = None;
    let outcome = {
        let mut st = shared.state.lock().expect("fake state");
        let find = |st: &State, t: &str| st.agents.iter().find(|a| a["pane_id"] == t).cloned();
        match method {
            "ping" => Outcome::Reply(json!({
                "type": "pong",
                "version": shared.version,
                "protocol": shared.protocol,
                "capabilities": {},
            })),
            "agent.list" => Outcome::Reply(json!({"type": "agent_list", "agents": st.agents})),
            "agent.get" | "agent.read" | "agent.prompt" | "agent.send_keys" | "agent.focus" => {
                let Some(t) = target else {
                    return err("invalid_request", "missing field `target`");
                };
                let Some(agent) = find(&st, t) else {
                    return err("agent_not_found", &format!("agent target {t} not found"));
                };
                match method {
                    "agent.get" => Outcome::Reply(json!({"type": "agent_info", "agent": agent})),
                    "agent.read" => {
                        let Some(source) = params["source"].as_str() else {
                            return err("invalid_request", "missing field `source`");
                        };
                        if let Some(code) =
                            st.read_fails.get_mut(source).and_then(VecDeque::pop_front)
                        {
                            return err(&code, "injected read failure");
                        }
                        let key = (t.to_owned(), source.to_owned());
                        if st.failing.contains(&key) {
                            return err("platform_error", "injected read failure");
                        }
                        let text = st.screens.get(&key).cloned().unwrap_or_default();
                        let truncated = st.truncated.contains(&key);
                        Outcome::Reply(json!({"type": "pane_read", "read": {
                            "pane_id": t, "workspace_id": "w1", "tab_id": "w1:t1", "source": source,
                            "format": "text", "text": text, "revision": 0, "truncated": truncated,
                        }}))
                    }
                    "agent.prompt" => {
                        let Some(text) = params["text"].as_str() else {
                            return err("invalid_request", "missing field `text`");
                        };
                        if agent["agent_status"] == "blocked" {
                            return err(
                                "agent_blocked",
                                &format!("agent {t} is blocked and requires interactive input"),
                            );
                        }
                        st.prompts.push((t.to_owned(), text.to_owned()));
                        set_status_locked(&mut st, t, "working");
                        let agent = find(&st, t).expect("agent still present");
                        Outcome::Reply(json!({"type": "agent_prompted", "agent": agent}))
                    }
                    "agent.send_keys" => {
                        let Some(keys) = params["keys"].as_array() else {
                            return err("invalid_request", "missing field `keys`");
                        };
                        let keys: Vec<String> = keys
                            .iter()
                            .filter_map(|k| k.as_str().map(str::to_owned))
                            .collect();
                        st.keys_sent.push((t.to_owned(), keys.clone()));
                        if let Some(f) = st.on_keys.clone() {
                            keys_call = Some((f, t.to_owned(), keys));
                        }
                        Outcome::Reply(json!({"type": "ok"}))
                    }
                    _ => Outcome::Reply(json!({"type": "ok"})), // agent.focus
                }
            }
            "events.subscribe" => {
                let Some(subs) = params["subscriptions"].as_array() else {
                    return err("invalid_request", "missing field `subscriptions`");
                };
                let mut names = Vec::new();
                let mut panes = HashSet::new();
                for s in subs {
                    let ty = s["type"].as_str().unwrap_or("");
                    match s["pane_id"].as_str() {
                        Some(p) => {
                            names.push(format!("{ty}:{p}"));
                            if ty == "pane.agent_status_changed" {
                                panes.insert(p.to_owned());
                            }
                        }
                        None => names.push(ty.to_owned()),
                    }
                }
                st.subscriptions.push(names);
                let (tx, rx) = unbounded_channel();
                st.subscribers.push(Subscriber { panes, tx });
                Outcome::Subscribed(json!({"type": "subscription_started"}), rx)
            }
            other => err(
                "invalid_request",
                &format!("invalid request: unknown variant `{other}`"),
            ),
        }
    };
    if let Some((f, pane, keys)) = keys_call {
        let view = FakeHerdr {
            shared: shared.clone(),
            owner: None,
        };
        f(&view, &pane, &keys);
    }
    outcome
}
