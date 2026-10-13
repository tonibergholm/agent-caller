//! What a user sees: the daemon's event stream for an external producer,
//! replayed through acd-client's session and card view the way `Inbox` does.
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use acd_adapters::external::{ExternalAdapter, ExternalConfig};
use acd_adapters::hooks::endpoint::{Credential, HookAuth};
use acd_client::conn::{MachineState, MachineStatus};
use acd_client::session::{Applied, Session};
use acd_client::settings::{EndpointConfig, MachineEntry};
use acd_client::view::{self, CardView, Notice, ViewInput};
use acd_core::{Core, testing::test_machine};
use acd_protocol::*;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

const TOKEN: &str = "tok-external-view-0123456789";

struct Rig {
    core: Arc<Core>,
    adapter: Arc<ExternalAdapter>,
    clock: Arc<AtomicI64>,
    sock: std::path::PathBuf,
    session: Session,
    rx: tokio::sync::broadcast::Receiver<ServerMessage>,
    notices: HashMap<String, Notice>,
    _dir: tempfile::TempDir,
}

async fn rig() -> Rig {
    let clock = Arc::new(AtomicI64::new(1_000_000));
    let (c, w) = (clock.clone(), clock.clone());
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
    let mut session = Session::default();
    let ServerMessage::Welcome {
        machine,
        capabilities,
        epoch,
        seq,
    } = core.welcome()
    else {
        panic!("welcome")
    };
    session.welcome(machine, capabilities, &epoch, seq);
    let (initial, rx) = core.attach(None);
    for message in initial {
        session.apply(&message);
    }
    Rig {
        core,
        adapter,
        clock,
        sock,
        session,
        rx,
        notices: HashMap::new(),
        _dir: dir,
    }
}

async fn put(sock: &std::path::Path, body: Value) -> u16 {
    let body = body.to_string();
    let mut stream = UnixStream::connect(sock).await.unwrap();
    let request = format!(
        "PUT /external/v1/producers/sup/state HTTP/1.1\r\nHost: acd\r\nContent-Length: {}\r\nConnection: close\r\nX-Acd-Token: {TOKEN}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).await.unwrap();
    String::from_utf8_lossy(&bytes)
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap()
}

impl Rig {
    /// Applies what the daemon published since the last call, and keeps the
    /// resolution notices as `Inbox` does.
    fn pump(&mut self) {
        while let Ok(message) = self.rx.try_recv() {
            if let Applied::Resolved {
                agent_id,
                by,
                reason,
                message,
                ..
            } = self.session.apply(&message)
                && let Some(n) = view::resolution_notice(by, reason, message.as_deref())
            {
                self.notices.insert(view::card_key("local", &agent_id), n);
            }
        }
    }

    fn cards(&mut self) -> Vec<CardView> {
        self.pump();
        let entry = MachineEntry {
            key: "local".into(),
            name: "Mac".into(),
            endpoint: EndpointConfig::Local,
        };
        let state = MachineState {
            instance: 1,
            status: MachineStatus::Online,
            session: self.session.clone(),
            conn_gen: 1,
        };
        let busy = HashSet::new();
        view::build_view(&ViewInput {
            machines: vec![view::MachineInput {
                entry: &entry,
                state: Some(&state),
            }],
            actions: &[],
            busy: &busy,
            notices: &self.notices,
            show_idle: true,
            show_subagents: true,
            show_hook_only: true,
        })
        .cards
    }
}

fn item(key: &str) -> Value {
    json!({"key": key, "agent_key": "parked:ENG-12", "agent": {"title": "autopilot"},
           "title": "ENG-12 parked", "options": [{"id": "abandon", "label": "Abandon", "confirm": true}]})
}

#[tokio::test]
async fn a_failed_ack_dropped_in_the_same_call_still_shows_its_message_on_a_card() {
    let mut rig = rig().await;
    assert_eq!(
        put(&rig.sock, json!({"lease_ms": 60000, "items": [item("k1")]})).await,
        200
    );
    let cards = rig.cards();
    assert_eq!(cards.len(), 1);
    let request = cards[0].agent.pending.clone().unwrap();
    assert!(cards[0].answerable);
    let result = rig
        .core
        .handle(
            "client",
            ClientMessage::RequestAnswer {
                id: "c1".into(),
                request_id: request.id.clone(),
                revision: Some(request.revision),
                option_id: Some("abandon".into()),
                text: None,
            },
        )
        .await;
    assert_eq!(result.outcome, Some(Outcome::Delivered));
    let answers = http_answers(&rig.sock).await;
    let answer_id = answers["answers"][0]["answer_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let status = put(
        &rig.sock,
        json!({"lease_ms": 60000, "items": [],
               "acks": [{"answer_id": answer_id, "outcome": "failed", "message": "crashed mid-action"}]}),
    )
    .await;
    assert_eq!(status, 200);
    let cards = rig.cards();
    assert_eq!(
        cards.len(),
        1,
        "the card is still there to carry the notice"
    );
    assert_eq!(
        cards[0].notice.as_ref().map(|n| n.text.as_str()),
        Some("crashed mid-action")
    );
    assert!(!cards[0].answerable);

    rig.clock.fetch_add(15_000, Ordering::SeqCst);
    rig.adapter.sweep();
    assert!(rig.cards().is_empty(), "then the card goes");
}

async fn http_answers(sock: &std::path::Path) -> Value {
    let mut stream = UnixStream::connect(sock).await.unwrap();
    let request = format!(
        "GET /external/v1/producers/sup/answers HTTP/1.1\r\nHost: acd\r\nConnection: close\r\nX-Acd-Token: {TOKEN}\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).await.unwrap();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    serde_json::from_str(text.split_once("\r\n\r\n").unwrap().1).unwrap()
}
