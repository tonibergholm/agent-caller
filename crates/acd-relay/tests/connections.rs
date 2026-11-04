//! Connection limits, head deadline and the periodic sweep (spec §4.6, §8).
//! Time is tokio's paused clock plus the harness `ManualClock`: nothing here
//! waits on the wall clock.

mod common;

use std::sync::Arc;
use std::time::Duration;

use acd_relay::ManualClock;
use acd_relay::limits::Limit;
use acd_relay::server::{AppState, Options, Rates, SWEEP_INTERVAL, ServeOptions, serve_with};
use common::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::Instant;

const CAP: usize = 4;
const HEAD: Duration = Duration::from_secs(2);
/// Upper bound on a wait for a close, in virtual time, so a missing
/// deadline fails the test instead of hanging it.
const GIVE_UP: Duration = Duration::from_secs(120);

async fn relay() -> Relay {
    let s = Arc::new(FakeSender::default());
    let opts = ServeOptions { max_connections: CAP, head_timeout: HEAD, ..ServeOptions::default() };
    start_serving(Setup::default(), s, Arc::new(ManualClock::new(T0)), opts).await
}

/// Read until the peer closes; returns what arrived.
async fn until_closed(mut s: TcpStream) -> Vec<u8> {
    let mut got = vec![];
    let mut buf = [0u8; 512];
    loop {
        match tokio::time::timeout(GIVE_UP, s.read(&mut buf)).await {
            Err(_) => panic!("connection still open after {GIVE_UP:?} (virtual)"),
            Ok(Ok(0)) | Ok(Err(_)) => return got,
            Ok(Ok(n)) => got.extend_from_slice(&buf[..n]),
        }
    }
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

async fn later_request_succeeds(r: &Relay) {
    tokio::time::resume();
    let ip = r.ip();
    let (st, b) = r.post("/v1/wake", &serde_json::json!({"v":1,"handle":"x"}), &ip).await;
    assert_eq!((st, b["error"].as_str()), (400, Some("invalid_handle")));
}

#[tokio::test(start_paused = true)]
async fn idle_connections_beyond_the_cap_are_closed_after_the_deadline() {
    let r = relay().await;
    let start = Instant::now();
    let mut conns = vec![];
    for _ in 0..CAP + 3 {
        conns.push(TcpStream::connect(r.addr).await.unwrap());
    }
    let closers: Vec<_> = conns.into_iter().map(|c| tokio::spawn(until_closed(c))).collect();
    for c in closers {
        assert!(c.await.unwrap().iter().all(|b| *b != b'2'), "no 200 for a silent connection");
    }
    // Two rounds of slots at most; hyper's own default (30 s) would fail this.
    assert!(start.elapsed() >= HEAD && start.elapsed() <= 3 * HEAD, "{:?}", start.elapsed());
    later_request_succeeds(&r).await;
}

#[tokio::test(start_paused = true)]
async fn incomplete_request_heads_are_closed_after_the_deadline() {
    let r = relay().await;
    let start = Instant::now();
    let mut closers = vec![];
    for _ in 0..CAP + 3 {
        let mut c = TcpStream::connect(r.addr).await.unwrap();
        // No blank line ends this head.
        c.write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\n").await.unwrap();
        closers.push(tokio::spawn(until_closed(c)));
    }
    for c in closers {
        let got = c.await.unwrap();
        assert!(!String::from_utf8_lossy(&got).contains("200 OK"), "no answer to half a head");
    }
    assert!(start.elapsed() >= HEAD && start.elapsed() <= 3 * HEAD, "{:?}", start.elapsed());
    later_request_succeeds(&r).await;
}

#[tokio::test(start_paused = true)]
async fn a_keep_alive_connection_that_goes_idle_after_a_response_is_closed() {
    let r = relay().await;
    let mut c = TcpStream::connect(r.addr).await.unwrap();
    c.write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\n\r\n").await.unwrap();
    let mut got = vec![];
    let mut buf = [0u8; 512];
    while !String::from_utf8_lossy(&got).contains("\"ok\":true") {
        let n = tokio::time::timeout(GIVE_UP, c.read(&mut buf)).await.unwrap().unwrap();
        assert!(n > 0, "closed before the answer");
        got.extend_from_slice(&buf[..n]);
    }
    assert!(String::from_utf8_lossy(&got).starts_with("HTTP/1.1 200"));
    let idle_from = Instant::now();
    assert!(until_closed(c).await.is_empty());
    assert!(idle_from.elapsed() >= HEAD && idle_from.elapsed() <= 2 * HEAD, "{:?}", idle_from.elapsed());
}

/// Real clock: the only wait is for the server's own head deadline. The
/// check is an ordering, not a duration: a response to the extra connection
/// can exist only after the server closed an idle one, so by then at least
/// one idle socket must already read as closed.
#[tokio::test]
async fn a_connection_over_the_cap_is_served_only_after_a_slot_frees() {
    let s = Arc::new(FakeSender::default());
    let opts = ServeOptions { max_connections: CAP, head_timeout: Duration::from_millis(300), ..ServeOptions::default() };
    let r = start_serving(Setup::default(), s, Arc::new(ManualClock::new(T0)), opts).await;
    let mut idle = vec![];
    for _ in 0..CAP {
        idle.push(TcpStream::connect(r.addr).await.unwrap());
    }
    let mut extra = TcpStream::connect(r.addr).await.unwrap();
    extra.write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").await.unwrap();
    let got = until_closed(extra).await;
    assert!(String::from_utf8_lossy(&got).starts_with("HTTP/1.1 200"), "served once a slot freed");
    let mut buf = [0u8; 8];
    let closed = idle.iter().filter(|c| matches!(c.try_read(&mut buf), Ok(0))).count();
    assert!(closed >= 1, "the extra connection was served while the cap was full");
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
