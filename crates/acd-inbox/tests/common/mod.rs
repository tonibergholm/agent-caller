#![allow(dead_code)]
use std::sync::Arc;
use std::time::Duration;

use acd_client::{
    Timing,
    conn::MachineStatus,
    fake::{FakeConn, FakeServer, Identity},
    inbox::{Inbox, InboxOptions},
    settings::{EndpointConfig, MachineEntry, MachineSecret, MemorySecrets, SecretStore, Settings},
    view::InboxView,
};
use serde_json::{Value, json};

pub const TOKEN: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8";

pub fn fast() -> Timing {
    let ms = Duration::from_millis;
    Timing {
        backoff_base: ms(20),
        backoff_max: ms(100),
        identity_retry: ms(200),
        idle_ping: ms(500),
        ping_timeout: ms(300),
        undo: ms(200),
        not_ready_interval: ms(50),
        connect_total: ms(3000),
        ..Timing::default()
    }
}

/// Countdowns that never end on their own; tests end them with
/// `expire_countdowns_for_test`.
pub fn held() -> Timing {
    Timing { undo: Duration::from_secs(3600), ..fast() }
}

/// An agent with an open permission request that also takes a typed answer.
pub fn pending_agent(id: &str, request: &str) -> Value {
    json!({"id":id,"machine_id":"m","source":"herdr","kind":"claude","title":"agent","cwd":null,"project":null,"state":"needs_you","state_since":"2026-10-08T00:00:00.000Z","failed":false,"last_message":null,"can_send":true,"pending":{"id":request,"revision":1,"agent_id":id,"kind":"permission","status":"open","answer_path":"hook","title":"Bash","detail":"npm test","options":[{"id":"allow","label":"Yes","description":null,"effect":"allow"},{"id":"deny","label":"No","description":null,"effect":"deny"}],"questions":null,"free_text":true,"raw_screen":null,"opened_at":"2026-10-08T00:00:00.000Z"}})
}

pub fn box_secret(f: &FakeServer) -> String {
    MachineSecret { token: TOKEN.into(), pin: f.pin() }.to_json()
}

pub fn box_settings(f: &FakeServer) -> Settings {
    Settings {
        machines: vec![MachineEntry {
            key: "m1".into(),
            name: "Box".into(),
            endpoint: EndpointConfig::Tcp { host: "127.0.0.1".into(), port: f.port() },
        }],
        ..Settings::fresh()
    }
}

pub type StoreMaker = Box<dyn FnOnce(&FakeServer) -> Arc<dyn SecretStore> + Send>;

/// A fake daemon and a started inbox over `store` (default: memory with
/// m1's secret). With `agents`, waits for the snapshot to show.
pub async fn inbox_rig(
    agents: Option<Vec<Value>>,
    timing: Timing,
    store: Option<StoreMaker>,
) -> (FakeServer, Option<FakeConn>, Arc<Inbox>) {
    let f = FakeServer::start(TOKEN, Identity::Paired).await;
    let store = match store {
        Some(make) => make(&f),
        None => {
            let s = Arc::new(MemorySecrets::default());
            s.set("m1", &box_secret(&f)).unwrap();
            s as Arc<dyn SecretStore>
        }
    };
    let inbox = Inbox::new(
        box_settings(&f),
        store,
        InboxOptions { timing, settings_path: None, client_name: "t".into(), client_version: "0".into() },
    );
    inbox.start();
    let Some(agents) = agents else { return (f, None, inbox) };
    let c = f.next_connection().await;
    c.send(json!({"type":"snapshot","epoch":"e1","seq":1,"agents":agents})).await;
    let want = agents.len();
    view_until(&inbox, move |v| v.cards.len() >= want && v.machines.iter().any(|m| m.status == MachineStatus::Online)).await;
    (f, Some(c), inbox)
}

pub async fn view_until(ib: &Inbox, pred: impl Fn(&InboxView) -> bool) {
    let mut rx = ib.view();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if pred(&rx.borrow()) {
                break;
            }
            rx.changed().await.unwrap();
        }
    })
    .await
    .expect("the view never matched");
}

/// Polls `cond` (no fixed sleep) for up to 5 s.
pub async fn until(what: &str, cond: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !cond() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

/// The first frame of this type the fake daemon received.
pub async fn frame_of_type(f: &FakeServer, ty: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(v) = f.received().into_iter().find(|v| v["type"] == ty) {
                return v;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no {ty} frame"))
}

pub fn answers(f: &FakeServer) -> usize {
    f.received().iter().filter(|v| v["type"] == "request.answer").count()
}

use std::path::{Path, PathBuf};

use acd_inbox::{
    prefs::Prefs,
    server::{NoKeyring, SecretControl, ServerOptions, Service},
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

pub type MakeSecrets = Box<dyn FnOnce(&FakeServer) -> (Arc<dyn SecretStore>, Arc<dyn SecretControl>) + Send>;

pub struct RigOptions {
    pub agents: Vec<Value>,
    pub timing: Timing,
    pub server: ServerOptions,
    pub control: Arc<dyn SecretControl>,
    /// Builds the inbox's store and the service's keyring control; default:
    /// a `MemorySecrets` holding m1's secret, and `control`.
    pub secrets: Option<MakeSecrets>,
    /// Wait for the fake daemon connection and send `agents`.
    pub connect: bool,
}

impl Default for RigOptions {
    fn default() -> Self {
        RigOptions {
            agents: vec![pending_agent("a1", "r1")],
            timing: fast(),
            server: ServerOptions::default(),
            control: Arc::new(NoKeyring),
            secrets: None,
            connect: true,
        }
    }
}

pub struct Rig {
    pub f: FakeServer,
    pub c: Option<FakeConn>,
    pub inbox: Arc<Inbox>,
    pub svc: Arc<Service>,
    pub socket: PathBuf,
    pub server: tokio::task::JoinHandle<()>,
    _dir: tempfile::TempDir,
}

pub async fn rig(agents: Vec<Value>, timing: Timing) -> Rig {
    rig_with(RigOptions { agents, timing, ..Default::default() }).await
}

pub async fn rig_with(o: RigOptions) -> Rig {
    type MakeStore = Box<dyn FnOnce(&FakeServer) -> Arc<dyn SecretStore> + Send>;
    let slot: Arc<std::sync::Mutex<Option<Arc<dyn SecretControl>>>> = Arc::new(std::sync::Mutex::new(None));
    let make: Option<MakeStore> = o.secrets.map(|make| {
        let out = slot.clone();
        Box::new(move |f: &FakeServer| {
            let (store, control) = make(f);
            *out.lock().unwrap() = Some(control);
            store
        }) as MakeStore
    });
    let (f, c, inbox) = inbox_rig(o.connect.then_some(o.agents), o.timing, make).await;
    let control = slot.lock().unwrap().take().unwrap_or(o.control);
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("s.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let svc = Service::new(inbox.clone(), Prefs::default(), None, control, o.server);
    let server = svc.spawn(listener);
    Rig { f, c, inbox, svc, socket, server, _dir: dir }
}

/// A view: a socket client that speaks the JSON-lines protocol.
pub struct View {
    r: BufReader<OwnedReadHalf>,
    w: OwnedWriteHalf,
    n: u64,
    /// The latest `settings` that `cmd` read past on its way to a reply. A
    /// reply and the settings it caused are queued apart, so either may be
    /// written first; `recv_until` looks here before it reads.
    passed: Option<Value>,
}

impl View {
    pub async fn connect(path: &Path) -> View {
        let (r, w) = UnixStream::connect(path).await.unwrap().into_split();
        View { r: BufReader::new(r), w, n: 0, passed: None }
    }

    /// The next message; `None` once the service closed the connection.
    pub async fn recv(&mut self) -> Option<Value> {
        let mut line = String::new();
        let n = tokio::time::timeout(Duration::from_secs(5), self.r.read_line(&mut line))
            .await
            .expect("no message within 5 s")
            .ok()?;
        if n == 0 {
            return None;
        }
        Some(serde_json::from_str(&line).expect("one JSON object per line"))
    }

    pub async fn greeting(&mut self) -> (Value, Value, Value) {
        let h = self.recv().await.unwrap();
        let s = self.recv().await.unwrap();
        let v = self.recv().await.unwrap();
        assert_eq!(
            (h["type"].as_str(), s["type"].as_str(), v["type"].as_str()),
            (Some("hello"), Some("settings"), Some("view"))
        );
        (h, s, v)
    }

    pub async fn recv_until(&mut self, pred: impl Fn(&Value) -> bool) -> Value {
        if let Some(m) = self.passed.take() {
            if pred(&m) {
                return m;
            }
        }
        loop {
            let m = self.recv().await.expect("the service closed the connection");
            if pred(&m) {
                return m;
            }
        }
    }

    pub async fn send(&mut self, v: &Value) {
        let mut bytes = serde_json::to_vec(v).unwrap();
        bytes.push(b'\n');
        self.w.write_all(&bytes).await.unwrap();
    }

    pub async fn send_raw(&mut self, bytes: &[u8]) {
        let _ = self.w.write_all(bytes).await;
    }

    /// Sends `v` with a fresh id; returns the `ok` or `error` reply to it.
    pub async fn cmd(&mut self, mut v: Value) -> Value {
        self.n += 1;
        let id = format!("c{}", self.n);
        v["id"] = id.clone().into();
        self.send(&v).await;
        loop {
            let m = self.recv().await.expect("the service closed the connection");
            if m["id"] == id.as_str() && (m["type"] == "ok" || m["type"] == "error") {
                return m;
            }
            if m["type"] == "settings" {
                self.passed = Some(m);
            }
        }
    }

    /// Reads until the service closes the connection.
    pub async fn closed(&mut self) {
        while self.recv().await.is_some() {}
    }
}
