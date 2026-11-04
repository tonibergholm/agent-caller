//! Tests added from a cargo-mutants run (docs/research/2026-11-04-mutation-testing.md).
//! A child module of `conn`, for its private functions.
use super::*;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Role, frame::coding::CloseCode};

/// `now_ms -> 1.0`, `*` -> `+` / `/`: milliseconds since the Unix epoch.
#[test]
fn now_ms_is_unix_milliseconds() {
    let t = now_ms();
    // After 2020-01-01 and before 2100-01-01, in milliseconds.
    assert!((1.577e12..4.102e12).contains(&t), "{t}");
}

/// `close_code -> None`: only a close frame that carries a code has one.
#[test]
fn close_code_reads_the_frame() {
    let frame = |c: u16| Message::Close(Some(CloseFrame { code: CloseCode::from(c), reason: "".into() }));
    assert_eq!(close_code(&frame(4401)), Some(4401));
    assert_eq!(close_code(&frame(1001)), Some(1001));
    assert_eq!(close_code(&Message::Close(None)), None);
    assert_eq!(close_code(&Message::Text("x".into())), None);
}

/// `end_for_close`: delete match arm 4400 / 4401 / 4408. 4401 must stop the
/// retry loop (token rejected), 4400 is an incompatible peer, 4408 a slow client.
#[test]
fn close_codes_map_to_their_ends() {
    assert!(matches!(end_for_close(4400), End::Incompatible(_)));
    assert!(matches!(end_for_close(4401), End::Unauthorized));
    assert!(matches!(end_for_close(4408), End::TooSlow));
    assert!(matches!(end_for_close(1001), End::Offline(_)));
    assert!(matches!(end_for_close(4402), End::Offline(_)));
}

/// `retryable_status`: `-> true / false` and `delete !`. A machine that was
/// refused (auth, identity, protocol, local) is not retried on its own.
#[test]
fn only_transient_states_are_retried() {
    let m = || "m".to_string();
    for s in [
        MachineStatus::Connecting,
        MachineStatus::Syncing,
        MachineStatus::Online,
        MachineStatus::Offline { since_ms: 1., error: m() },
    ] {
        assert!(retryable_status(&s), "{s:?}");
    }
    for s in [
        MachineStatus::AuthFailed { message: m() },
        MachineStatus::IdentityChanged { message: m() },
        MachineStatus::Incompatible { message: m() },
        MachineStatus::LocalUnavailable { message: m() },
    ] {
        assert!(!retryable_status(&s), "{s:?}");
    }
}

fn client_info() -> ClientInfo {
    ClientInfo { id: "c".into(), name: "t".into(), version: "0".into() }
}

/// A connected in-memory pair: the client side as `conn` uses it, and a raw server side.
async fn pair() -> (Ws, WebSocketStream<tokio::io::DuplexStream>) {
    let (a, b) = tokio::io::duplex(1 << 16);
    let client = WebSocketStream::from_raw_socket(Box::new(a) as Box<dyn Io>, Role::Client, None).await;
    let server = WebSocketStream::from_raw_socket(b, Role::Server, None).await;
    (client, server)
}

fn far() -> Instant {
    // Only reached on failure: a hung handshake ends the test instead of hanging it.
    Instant::now() + Duration::from_secs(60)
}

async fn handshake_against(frames: Vec<Message>) -> Result<(Machine, Vec<String>, String, u64), End> {
    let (mut client, mut server) = pair().await;
    let peer = tokio::spawn(async move {
        let hello = server.next().await.unwrap().unwrap();
        let Message::Text(t) = hello else { panic!("hello is text") };
        let v: serde_json::Value = serde_json::from_str(&t).unwrap();
        assert_eq!((v["type"].clone(), v["token"].clone()), ("hello".into(), "tok".into()));
        for f in frames {
            server.send(f).await.unwrap();
        }
        server
    });
    let out = handshake(&mut client, "tok", &client_info(), None, far()).await;
    let _server = peer.await.unwrap();
    out
}

fn err_frame(code: &str, message: &str) -> Message {
    Message::Text(serde_json::json!({"type":"error","code":code,"message":message}).to_string().into())
}

/// `handshake`: delete match arm `Error { message, .. }`. A refusal with any
/// other code ends the attempt with the daemon's message, at once.
#[tokio::test]
async fn handshake_ends_on_each_kind_of_error() {
    assert!(matches!(
        handshake_against(vec![err_frame("bad_request", "boom")]).await,
        Err(End::Offline(m)) if m == "boom"
    ));
    assert!(matches!(
        handshake_against(vec![err_frame("unauthorized", "no")]).await,
        Err(End::Unauthorized)
    ));
    assert!(matches!(
        handshake_against(vec![err_frame("unsupported_protocol", "old")]).await,
        Err(End::Incompatible(m)) if m == "old"
    ));
    let close = |c: u16| Message::Close(Some(CloseFrame { code: CloseCode::from(c), reason: "".into() }));
    assert!(matches!(handshake_against(vec![close(4401)]).await, Err(End::Unauthorized)));
    assert!(matches!(handshake_against(vec![close(4400)]).await, Err(End::Incompatible(_))));
}

/// `wait_retry -> true`: it waits for `RetryNow`, answers commands meanwhile
/// with "offline", and gives up when cancelled or when the handle is gone.
#[tokio::test]
async fn wait_retry_waits_for_retry_now() {
    let (ctl_tx, mut ctl_rx) = mpsc::channel(4);
    let (cancel_tx, mut cancel_rx) = watch::channel(false);
    let mut waiting = Box::pin(wait_retry(&mut ctl_rx, &mut cancel_rx));
    tokio::select! {
        biased;
        _ = &mut waiting => panic!("returned with nothing to wait for"),
        () = std::future::ready(()) => {}
    }
    let (tx, rx) = oneshot::channel();
    ctl_tx
        .send(Control::Submit(Command::MarkSeen { agent_id: "a".into() }, None, tx))
        .await
        .unwrap();
    tokio::select! {
        biased;
        r = &mut waiting => panic!("a command ended the wait: {r}"),
        outcome = rx => assert_eq!(outcome.unwrap(), Outcome::NotSent(NotSent::Offline)),
    }
    ctl_tx.send(Control::DropConnection).await.unwrap();
    ctl_tx.send(Control::RetryNow).await.unwrap();
    assert!(waiting.await);

    let (ctl_tx, mut ctl_rx) = mpsc::channel::<Control>(1);
    let (cancel_tx2, mut cancel_rx2) = watch::channel(false);
    cancel_tx2.send(true).unwrap();
    assert!(!wait_retry(&mut ctl_rx, &mut cancel_rx2).await, "cancelled");
    drop(cancel_tx);
    let (_c, mut never) = watch::channel(false);
    drop(ctl_tx);
    assert!(!wait_retry(&mut ctl_rx, &mut never).await, "handle dropped");
}
