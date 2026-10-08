//! End to end over acd/1: a real WebSocket server on a `Core` with a `HerdrAdapter` over a `FakeHerdr`.

use acd_adapters::herdr::{
    HerdrAdapter, HerdrConfig,
    fake::{FakeHerdr, fake_agent},
    keys::KeysTiming,
    reconciler::Timing,
};
use acd_core::{Auth, Core, ServerOptions, router, testing::test_machine};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::net::TcpStream;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Five seconds covers the 2 s production fallback delay plus polling (ruling R2).
const MARGIN: Duration = Duration::from_secs(5);

struct Rig {
    fake: FakeHerdr,
    url: String,
    _home: tempfile::TempDir,
}

/// Panes and detection screens are installed before the adapter starts, so nothing races the
/// first scan.
async fn start(agents: Vec<Value>, screens: &[(&str, &str)]) -> Rig {
    let home = tempfile::Builder::new()
        .prefix("acd")
        .tempdir_in("/tmp")
        .unwrap(); // short: macOS socket paths max 104 bytes
    let sock = home.path().join(".config/herdr/herdr.sock");
    std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
    let fake = FakeHerdr::start_at(&sock).await;
    for a in agents {
        fake.set_agent(a);
    }
    for (pane, text) in screens {
        fake.set_screen(pane, "detection", text);
    }
    let cfg = HerdrConfig {
        enabled: true,
        home: home.path().into(),
        env_socket: None,
        extra_sockets: vec![],
        scan_interval: Duration::from_millis(100),
        timing: Timing {
            list_idle: Duration::from_millis(300),
            list_blocked: Duration::from_millis(100),
            backoff_max: Duration::from_millis(200),
            fallback_after: Duration::from_millis(200),
            poll: Duration::from_millis(50),
        },
        keys: KeysTiming {
            evidence_timeout: Duration::from_millis(500),
            ..KeysTiming::default()
        },
    };
    let adapter = HerdrAdapter::new(cfg);
    let core = Core::new(test_machine(), vec![adapter.clone()]);
    adapter.start(core.clone());
    let auth = Arc::new(Auth::new("secret".into()));
    let app = router(
        core,
        auth,
        ServerOptions {
            hello_timeout: Duration::from_millis(300),
            ..Default::default()
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Rig {
        fake,
        url,
        _home: home,
    }
}

async fn connect(url: &str) -> Ws {
    connect_async(url).await.unwrap().0
}

async fn send(ws: &mut Ws, v: Value) {
    ws.send(Message::text(v.to_string())).await.unwrap();
}

/// The next JSON frame, or the close code as `{"closed": code}`.
async fn next(ws: &mut Ws) -> Value {
    next_within(ws, Duration::from_secs(2)).await
}

async fn next_within(ws: &mut Ws, d: Duration) -> Value {
    loop {
        match tokio::time::timeout(d, ws.next())
            .await
            .expect("timed out waiting for the next websocket frame or close")
        {
            Some(Ok(Message::Text(t))) => return serde_json::from_str(t.as_str()).unwrap(),
            Some(Ok(Message::Close(frame))) => {
                return json!({"closed": frame.map(|f| u16::from(f.code))});
            }
            Some(Ok(_)) => continue,
            Some(Err(_)) | None => return json!({"closed": null}),
        }
    }
}

/// Reads frames until one satisfies `pred`, within `d` overall.
async fn next_matching(ws: &mut Ws, d: Duration, mut pred: impl FnMut(&Value) -> bool) -> Value {
    let deadline = tokio::time::Instant::now() + d;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        let v = next_within(ws, left.max(Duration::from_millis(1))).await;
        if pred(&v) {
            return v;
        }
    }
}

fn hello(token: &str, resume: Option<Value>) -> Value {
    let mut v = json!({"type":"hello","protocol":"acd/1","token":token,"client":{"id":"c1","name":"test","version":"0"}});
    if let Some(r) = resume {
        v["resume"] = r;
    }
    v
}

async fn authed(url: &str) -> (Ws, Value, Value) {
    let mut ws = connect(url).await;
    send(&mut ws, hello("secret", None)).await;
    let welcome = next(&mut ws).await;
    let initial = next(&mut ws).await;
    (ws, welcome, initial)
}

/// A snapshot with exactly `n` agents, reconnecting until the adapter has produced them.
async fn snapshot_with(url: &str, n: usize) -> (Ws, Value, Value) {
    for _ in 0..100 {
        let (ws, welcome, snap) = authed(url).await;
        if snap["agents"].as_array().is_some_and(|a| a.len() == n) {
            return (ws, welcome, snap);
        }
        drop(ws);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("no snapshot with {n} agents");
}

/// Herdr methods acd must never call in these tests (spec invariants).
fn assert_no_forbidden_calls(fake: &FakeHerdr) {
    const FORBIDDEN: [&str; 5] = [
        "agent.focus",
        "pane.send_keys",
        "pane.send_text",
        "pane.send_input",
        "agent.send_keys",
    ];
    let bad: Vec<_> = fake
        .calls()
        .into_iter()
        .map(|(m, _)| m)
        .filter(|m| FORBIDDEN.contains(&m.as_str()))
        .collect();
    assert!(bad.is_empty(), "forbidden herdr calls: {bad:?}");
}

#[tokio::test]
async fn agent_appears_over_the_protocol() {
    let mut shell = fake_agent("w1:p2", "x", "unknown");
    shell["agent"] = Value::Null; // a plain shell pane
    let r = start(vec![fake_agent("w1:p1", "claude", "idle"), shell], &[]).await;
    let (mut ws, _, snap) = snapshot_with(&r.url, 1).await;
    let a = &snap["agents"][0];
    assert_eq!(a["source"], "herdr");
    assert_eq!(a["kind"], "claude");
    assert_eq!(a["can_send"], true);

    r.fake.set_status("w1:p1", "blocked");
    let ev = next_matching(&mut ws, Duration::from_secs(2), |v| {
        v["type"] == "agent.updated" && v["agent"]["state"] == "needs_you"
    })
    .await;
    assert_eq!(ev["agent"]["id"], a["id"]);
    assert_no_forbidden_calls(&r.fake);
}

#[tokio::test]
async fn follow_up_retry_sends_once() {
    let r = start(vec![fake_agent("w1:p1", "claude", "idle")], &[]).await;
    let (mut ws, welcome, snap) = snapshot_with(&r.url, 1).await;
    let agent_id = snap["agents"][0]["id"].clone();
    let resume = json!({"epoch": welcome["epoch"], "seq": snap["seq"]});
    let cmd = json!({"type":"agent.send","id":"f1","agent_id":agent_id,"text":"run the tests"});

    // Send, then drop the socket without reading the result.
    send(&mut ws, cmd.clone()).await;
    drop(ws);
    let mut sent = false;
    for _ in 0..100 {
        if r.fake.prompts().len() == 1 {
            sent = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(sent, "the first send never reached herdr");

    let mut ws = connect(&r.url).await;
    send(&mut ws, hello("secret", Some(resume))).await;
    assert_eq!(next(&mut ws).await["type"], "welcome");
    send(&mut ws, cmd).await;
    let res = next_matching(&mut ws, Duration::from_secs(2), |v| v["type"] == "result").await;
    assert_eq!(res["id"], "f1");
    assert_eq!(res["ok"], true);
    assert_eq!(res["outcome"], "delivered");
    assert_eq!(
        r.fake.prompts(),
        vec![("w1:p1".to_string(), "run the tests".to_string())]
    );
    assert_no_forbidden_calls(&r.fake);
}

#[tokio::test]
async fn blocked_pane_is_read_only_over_the_protocol() {
    let trust = std::fs::read_to_string(format!(
        "{}/../../tests/fixtures/herdr/codex-trust-dialog-detection.txt",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    // Pad past 40 lines so the raw_screen cap is exercised.
    let transcript: String = (1..=60).map(|i| format!("transcript line {i}\n")).collect();
    let dialog = format!("{transcript}{trust}");
    let r = start(
        vec![fake_agent("w1:p1", "aider", "blocked")],
        &[("w1:p1", &dialog)],
    )
    .await;
    let (mut ws, _, initial) = authed(&r.url).await;

    // The request may already be in the initial snapshot, or arrive as an update.
    let pending = if initial["agents"][0]["pending"].is_object() {
        initial["agents"][0]["pending"].clone()
    } else {
        next_matching(&mut ws, MARGIN, |v| {
            v["type"] == "agent.updated" && v["agent"]["pending"].is_object()
        })
        .await["agent"]["pending"]
            .clone()
    };
    let req = &pending;
    assert_eq!(req["kind"], "blocked_unparsed");
    assert_eq!(req["answer_path"], "none");
    let lines = req["raw_screen"].as_str().unwrap().lines().count();
    assert!((1..=40).contains(&lines), "raw_screen has {lines} lines");

    send(
        &mut ws,
        json!({"type":"request.answer","id":"a1","request_id":req["id"],"option_id":"1"}),
    )
    .await;
    let res = next_matching(&mut ws, Duration::from_secs(2), |v| v["type"] == "result").await;
    assert_eq!(res["ok"], false);
    assert_eq!(res["error"]["code"], "not_answerable");
    assert_no_forbidden_calls(&r.fake);
}
