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
use tokio::task::{JoinHandle, JoinSet};

type KeysHook = Arc<dyn Fn(&FakeHerdr, &str, &[String]) + Send + Sync>;

enum Hook {
    /// Apply the request, record it, never reply, hold the connection.
    Hang,
    /// Reply with an error and apply nothing.
    Fail(String),
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
    on_keys: Option<KeysHook>,
    subscribers: Vec<Subscriber>,
    calls: Vec<(String, Value)>,
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
    _dir: tempfile::TempDir,
    task: JoinHandle<()>,
}

impl Drop for Owner {
    fn drop(&mut self) {
        self.task.abort();
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
        let dir = tempfile::Builder::new().prefix("acd").tempdir_in("/tmp").expect("tempdir");
        let path = dir.path().join("h.sock");
        let listener = UnixListener::bind(&path).expect("bind fake herdr socket");
        let shared = Arc::new(Shared {
            path,
            version: version.to_owned(),
            protocol,
            state: Mutex::new(State::default()),
        });
        let task = tokio::spawn(accept_loop(listener, shared.clone()));
        FakeHerdr { shared, owner: Some(Arc::new(Owner { _dir: dir, task })) }
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
        self.st().screens.insert((pane_id.to_owned(), source.to_owned()), text.to_owned());
    }

    /// Runs on `agent.send_keys` (after recording), outside the state lock.
    pub fn on_keys(&self, f: impl Fn(&FakeHerdr, &str, &[String]) + Send + Sync + 'static) {
        self.st().on_keys = Some(Arc::new(f));
    }

    /// The next `method` request is applied and recorded, but never answered.
    pub fn hang_next(&self, method: &str) {
        self.st().hooks.entry(method.to_owned()).or_default().push_back(Hook::Hang);
    }

    /// The next `method` request gets `{"error":{"code":code,...}}` and has no effect.
    pub fn fail_next(&self, method: &str, code: &str) {
        self.st().hooks.entry(method.to_owned()).or_default().push_back(Hook::Fail(code.to_owned()));
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
    let Some(agent) = st.agents.iter_mut().find(|a| a["pane_id"] == pane_id) else { return };
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
        let Ok((stream, _)) = listener.accept().await else { return };
        while conns.try_join_next().is_some() {}
        conns.spawn(serve(stream, shared.clone()));
    }
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

    let hook = {
        let mut st = shared.state.lock().expect("fake state");
        st.calls.push((method.clone(), params.clone()));
        st.hooks.get_mut(&method).and_then(VecDeque::pop_front)
    };
    let outcome = match hook {
        Some(Hook::Fail(code)) => Outcome::Error(code, "injected failure".into()),
        hook => {
            let outcome = handle(&shared, &method, &params);
            if matches!(hook, Some(Hook::Hang)) {
                // Hold the connection until the client goes away.
                let mut sink = String::new();
                while matches!(r.read_line(&mut sink).await, Ok(n) if n > 0) {
                    sink.clear();
                }
                return;
            }
            outcome
        }
    };

    let (frame, stream_rx) = match outcome {
        Outcome::Reply(result) => (json!({"id": id, "result": result}), None),
        Outcome::Error(code, message) => (json!({"id": id, "error": {"code": code, "message": message}}), None),
        Outcome::Subscribed(result, rx) => (json!({"id": id, "result": result}), Some(rx)),
    };
    let mut bytes = frame.to_string().into_bytes();
    bytes.push(b'\n');
    if w.write_all(&bytes).await.is_err() {
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
    if let Ok(Ok(n)) = tokio::time::timeout(Duration::from_millis(200), r.read_line(&mut line)).await
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
                let Some(t) = target else { return err("invalid_request", "missing field `target`") };
                let Some(agent) = find(&st, t) else {
                    return err("agent_not_found", &format!("agent target {t} not found"));
                };
                match method {
                    "agent.get" => Outcome::Reply(json!({"type": "agent_info", "agent": agent})),
                    "agent.read" => {
                        let Some(source) = params["source"].as_str() else {
                            return err("invalid_request", "missing field `source`");
                        };
                        let text = st.screens.get(&(t.to_owned(), source.to_owned())).cloned().unwrap_or_default();
                        Outcome::Reply(json!({"type": "pane_read", "read": {
                            "pane_id": t, "workspace_id": "w1", "tab_id": "w1:t1", "source": source,
                            "format": "text", "text": text, "revision": 0, "truncated": false,
                        }}))
                    }
                    "agent.prompt" => {
                        let Some(text) = params["text"].as_str() else {
                            return err("invalid_request", "missing field `text`");
                        };
                        if agent["agent_status"] == "blocked" {
                            return err("agent_blocked", &format!("agent {t} is blocked and requires interactive input"));
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
                        let keys: Vec<String> = keys.iter().filter_map(|k| k.as_str().map(str::to_owned)).collect();
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
            other => err("invalid_request", &format!("invalid request: unknown variant `{other}`")),
        }
    };
    if let Some((f, pane, keys)) = keys_call {
        let view = FakeHerdr { shared: shared.clone(), owner: None };
        f(&view, &pane, &keys);
    }
    outcome
}
