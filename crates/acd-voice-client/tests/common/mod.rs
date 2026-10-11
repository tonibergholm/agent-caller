#![allow(dead_code)]
use std::sync::Arc;
use std::time::Duration;

use acd_client::Timing;
use acd_client::fake::{FakeConn, FakeServer, Identity};
use acd_client::inbox::{Inbox, InboxOptions};
use acd_client::settings::{
    DEFAULT_HOTKEY, EndpointConfig, MachineEntry, MachineSecret, MemorySecrets, SecretStore, Settings,
};
use acd_client::view::InboxView;
use acd_voice::snapshot::Presentation;
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

/// Machine "Box" (key m1), Claude agent a1 with request r1: Yes / No / Yes, always.
pub fn pending_agent() -> Value {
    json!({"id":"a1","machine_id":"m","source":"herdr","kind":"claude","title":"agent","cwd":null,"project":null,"state":"needs_you","state_since":"2026-10-08T00:00:00.000Z","failed":false,"last_message":null,"can_send":true,"pending":{"id":"r1","revision":1,"agent_id":"a1","kind":"permission","status":"open","answer_path":"hook","title":"Bash","detail":"npm test","options":[{"id":"allow","label":"Yes","description":null,"effect":"allow"},{"id":"deny","label":"No","description":null,"effect":"deny"},{"id":"always","label":"Yes, always","description":"Bash(npm test:*)","effect":"allow_always"}],"questions":null,"free_text":false,"raw_screen":null,"opened_at":"2026-10-08T00:00:00.000Z"}})
}

/// A working Claude agent a2 that takes a reply.
pub fn working_agent() -> Value {
    let mut a = pending_agent();
    a["id"] = "a2".into();
    a["state"] = "working".into();
    a["pending"] = Value::Null;
    a
}

/// An idle Codex agent a3; the panel hides it ("Show idle" is off).
pub fn idle_agent() -> Value {
    let mut a = working_agent();
    a["id"] = "a3".into();
    a["kind"] = "codex".into();
    a["state"] = "idle".into();
    a
}

pub struct InboxRig {
    pub f: FakeServer,
    pub c: FakeConn,
    pub inbox: Arc<Inbox>,
}

pub async fn inbox_rig(agents: Vec<Value>) -> InboxRig {
    let f = FakeServer::start(TOKEN, Identity::Paired).await;
    let secrets = Arc::new(MemorySecrets::default());
    secrets
        .set("m1", &MachineSecret { token: TOKEN.into(), pin: f.pin() }.to_json())
        .unwrap();
    let settings = Settings {
        client_id: "00000000-0000-4000-8000-000000000001".into(),
        hotkey: DEFAULT_HOTKEY.into(),
        show_idle: false,
        show_subagents: false,
        machines: vec![MachineEntry {
            key: "m1".into(),
            name: "Box".into(),
            endpoint: EndpointConfig::Tcp { host: "127.0.0.1".into(), port: f.port() },
        }],
    };
    let inbox = Inbox::new(
        settings,
        secrets,
        InboxOptions { timing: fast(), settings_path: None, client_name: "t".into(), client_version: "0".into() },
    );
    inbox.start();
    let c = f.next_connection().await;
    let n = agents.len();
    c.send(json!({"type":"snapshot","epoch":"e1","seq":1,"agents":agents})).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let s = inbox.stamped_view();
            if s.view.cards.len() + s.hidden_idle_cards.len() == n && !s.stamps.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the inbox shows the agents");
    InboxRig { f, c, inbox }
}

pub async fn view_until(inbox: &Inbox, what: &str, pred: impl Fn(&InboxView) -> bool) {
    let mut rx = inbox.view();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if pred(&rx.borrow()) {
                break;
            }
            rx.changed().await.unwrap();
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

pub fn shown(selected: Option<&str>, visible: &[&str]) -> Presentation {
    Presentation {
        epoch: "e".into(),
        version: 1,
        selected: selected.map(str::to_owned),
        visible: visible.iter().map(|s| (*s).to_owned()).collect(),
        machine_filter: None,
        done_collapsed: true,
    }
}

pub fn frames(f: &FakeServer, kind: &str) -> Vec<Value> {
    f.received().into_iter().filter(|v| v["type"] == kind).collect()
}

pub async fn frame(f: &FakeServer, kind: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(v) = frames(f, kind).into_iter().next() {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no {kind} frame"))
}
