//! Connection limits, head deadline and the periodic sweep (spec §4.6, §8).
//!
//! No test here waits on the wall clock. The head-deadline tests drive one
//! connection over an in-memory pipe (`tokio::io::duplex`) with tokio's
//! clock paused and moved by `advance`, using the same hyper settings as
//! `serve_with` (`connection_builder`). The connection-cap test uses real
//! sockets and waits for events only: a head deadline too long to matter is
//! configured and a slot is freed by closing a client. The sweep tests pause
//! the clock only while no socket is waiting, and move the relay's
//! `ManualClock` by hand.

mod common;

use std::sync::Arc;
use std::time::Duration;

use acd_relay::ManualClock;
use acd_relay::limits::Limit;
use acd_relay::server::{
    AppState, Options, Rates, SWEEP_INTERVAL, ServeOptions, connection_builder, router, serve_with,
};
use axum::extract::ConnectInfo;
use common::*;
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::net::TcpStream;

const CAP: usize = 4;
const HEAD: Duration = Duration::from_millis(250);

/// A relay state with no sweeper, for the in-memory connection tests.
fn state() -> Arc<AppState> {
    AppState::new(
        Options { keys: keys(), allowed_apps: vec![APP.into()], trusted_proxy: None, rates: Rates::default() },
        Arc::new(FakeSender::default()),
        Arc::new(ManualClock::new(T0)),
    )
}

/// Serve one connection over a pipe with the relay's own connection
/// settings and a head deadline of `HEAD`. Returns the client end and the
/// server task. The task has been polled once when this returns, so its
/// deadline timer is armed at the current (paused) instant.
async fn pipe_connection() -> (DuplexStream, tokio::task::JoinHandle<()>) {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let peer: std::net::SocketAddr = "127.0.0.1:1".parse().unwrap();
    let app = router(state()).layer(axum::Extension(ConnectInfo(peer)));
    let task = tokio::spawn(async move {
        let _ = connection_builder(HEAD)
            .serve_connection(TokioIo::new(server), TowerToHyperService::new(app))
            .await;
    });
    tokio::task::yield_now().await;
    (client, task)
}

/// The server must still hold the connection one tick before the deadline
/// and must have closed it at the deadline.
async fn assert_closed_exactly_at_the_deadline(
    mut client: DuplexStream,
    task: tokio::task::JoinHandle<()>,
) {
    tokio::time::advance(HEAD - Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    assert!(!task.is_finished(), "closed before the deadline");
    tokio::time::advance(Duration::from_millis(1)).await;
    // Not `task.await`: with the clock paused an idle runtime would advance
    // it to the next timer, hiding a deadline that is too long.
    tokio::task::yield_now().await;
    assert!(task.is_finished(), "still open at the deadline");
    task.await.unwrap();
    let mut rest = vec![];
    client.read_to_end(&mut rest).await.unwrap();
    assert!(!String::from_utf8_lossy(&rest).contains("200 OK"), "no answer to a silent peer");
}

/// Move virtual time forward with the clock paused, then let it run again.
/// Requests over real sockets run with the clock running: a paused clock
/// can jump past a client timeout while a loopback reply is still in the
/// kernel.
async fn virtual_sleep(d: Duration) {
    tokio::time::pause();
    tokio::time::sleep(d).await;
    tokio::time::resume();
}

#[tokio::test(start_paused = true)]
async fn an_idle_connection_is_closed_at_the_head_deadline() {
    let (client, task) = pipe_connection().await;
    assert_closed_exactly_at_the_deadline(client, task).await;
}

#[tokio::test(start_paused = true)]
async fn an_incomplete_request_head_is_closed_at_the_head_deadline() {
    let (mut client, task) = pipe_connection().await;
    // No blank line ends this head.
    client.write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\n").await.unwrap();
    assert_closed_exactly_at_the_deadline(client, task).await;
}

#[tokio::test(start_paused = true)]
async fn a_keep_alive_connection_that_goes_idle_after_a_response_is_closed() {
    let (mut client, task) = pipe_connection().await;
    client.write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\n\r\n").await.unwrap();
    let mut got = vec![];
    let mut buf = [0u8; 512];
    while !String::from_utf8_lossy(&got).contains("\"ok\":true") {
        let n = client.read(&mut buf).await.unwrap();
        assert!(n > 0, "closed before the answer");
        got.extend_from_slice(&buf[..n]);
    }
    assert!(String::from_utf8_lossy(&got).starts_with("HTTP/1.1 200"));
    // The deadline restarts after the response: it is measured from here.
    tokio::task::yield_now().await;
    tokio::time::advance(HEAD - Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    assert!(!task.is_finished(), "idle keep-alive closed before the deadline");
    tokio::time::advance(Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    assert!(task.is_finished(), "idle keep-alive still open at the deadline");
    task.await.unwrap();
}

/// One request/response on a keep-alive connection.
async fn healthz(c: &mut TcpStream) {
    c.write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\n\r\n").await.unwrap();
    let mut got = vec![];
    let mut buf = [0u8; 512];
    while !String::from_utf8_lossy(&got).contains("\"ok\":true") {
        let n = c.read(&mut buf).await.unwrap();
        assert!(n > 0, "closed before the answer");
        got.extend_from_slice(&buf[..n]);
    }
}

/// Real sockets, no timers: the head deadline is an hour, so it cannot be
/// what frees a slot. All `CAP` slots are shown to be held (each connection
/// has been answered), an extra connection's request is written, and a
/// round trip on another held connection is awaited so the server has had
/// every chance to answer the extra one. It must not have. Closing one held
/// client frees a slot and only then is the extra connection answered.
#[tokio::test]
async fn a_connection_over_the_cap_is_served_only_after_a_slot_frees() {
    let s = Arc::new(FakeSender::default());
    let opts = ServeOptions {
        max_connections: CAP,
        head_timeout: Duration::from_secs(3600),
        sweep_interval: None,
    };
    let r = start_serving(Setup::default(), s, Arc::new(ManualClock::new(T0)), opts).await;
    let mut held = vec![];
    for _ in 0..CAP {
        let mut c = TcpStream::connect(r.addr).await.unwrap();
        healthz(&mut c).await; // accepted and answered: its slot is taken
        held.push(c);
    }
    let mut extra = TcpStream::connect(r.addr).await.unwrap();
    extra.write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").await.unwrap();
    healthz(&mut held[1]).await;
    let mut buf = [0u8; 8];
    assert!(
        matches!(extra.try_read(&mut buf), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
        "the extra connection was answered while the cap was full"
    );
    drop(held.swap_remove(0)); // a slot frees
    let mut got = vec![];
    extra.read_to_end(&mut got).await.unwrap();
    assert!(String::from_utf8_lossy(&got).starts_with("HTTP/1.1 200"), "served once a slot freed");
}

/// Small helper: how many entries do the bounded maps hold?
fn total(r: &Relay) -> usize {
    r.state.map_sizes().iter().sum()
}

#[tokio::test]
async fn expired_entries_are_swept_while_the_maps_are_below_capacity() {
    let s = Arc::new(FakeSender::default());
    let r = start(Setup::default(), s.clone()).await;
    let h = r.make_handle(&s, "tok:sweep").await;
    assert_eq!(r.wake(&h).await, 204);
    let (st, _) = r.register("tok:pending").await;
    assert_eq!(st, 202);
    let before = r.state.map_sizes();
    assert!(before.iter().all(|n| *n > 0), "{before:?}");
    assert!(before.iter().all(|n| *n < 4096), "below capacity: {before:?}");
    // Still inside every window: a sweep removes nothing.
    r.clock.advance(10);
    virtual_sleep(SWEEP_INTERVAL + Duration::from_secs(1)).await;
    assert_eq!(r.state.map_sizes(), before, "live entries survive a sweep");
    // Past the hour window and the challenge lifetime: the next sweep empties them.
    r.clock.advance(3600);
    virtual_sleep(SWEEP_INTERVAL + Duration::from_secs(1)).await;
    assert_eq!(total(&r), 0, "{:?}", r.state.map_sizes());
}

#[tokio::test]
async fn a_sweep_never_resets_a_live_counter() {
    let s = Arc::new(FakeSender::default());
    let rates = Rates { wake_handle: Limit { per_minute: 2, per_hour: 100 }, ..Rates::default() };
    let r = start(Setup { rates, ..Setup::default() }, s.clone()).await;
    let h = r.make_handle(&s, "tok:live").await;
    assert_eq!(r.wake(&h).await, 204);
    assert_eq!(r.wake(&h).await, 204);
    assert_eq!(r.wake(&h).await, 429);
    r.clock.advance(10); // inside the one-minute window
    virtual_sleep(SWEEP_INTERVAL + Duration::from_secs(1)).await;
    assert_eq!(r.wake(&h).await, 429, "the sweep must not give the budget back");
}

#[tokio::test(start_paused = true)]
async fn the_sweeper_ends_with_serve() {
    let s = Arc::new(FakeSender::default());
    let state = AppState::new(
        Options { keys: keys(), allowed_apps: vec![APP.into()], trusted_proxy: None, rates: Rates::default() },
        s,
        Arc::new(ManualClock::new(T0)),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let serving = tokio::spawn(serve_with(listener, state.clone(), ServeOptions::default(), async {
        let _ = stopped.await;
    }));
    // The sweeper holds a reference while serving.
    tokio::time::sleep(SWEEP_INTERVAL + Duration::from_secs(1)).await;
    assert!(Arc::strong_count(&state) > 1);
    stop.send(()).unwrap();
    serving.await.unwrap().unwrap();
    assert_eq!(Arc::strong_count(&state), 1, "sweeper and connections are gone");
}
