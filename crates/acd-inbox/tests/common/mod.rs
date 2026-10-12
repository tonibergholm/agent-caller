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
