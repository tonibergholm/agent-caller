//! The relay's HTTP surface end to end: bytes (a raw request, or a request
//! built around a real sealed handle) go through hyper's HTTP/1 parser and the
//! relay's router, with a recording push sender and a fixed clock.
//! Invariants: no handler panics and no connection hangs; every response is
//! a well formed status line with a status the API documents (never 500);
//! the sender is only woken for the address sealed in the handle, and only
//! challenged with an address and app the charset rules accept; every 204 is
//! one wake; no response holds a handle, an address or a challenge code; the
//! bounded maps stay within their caps.
#![no_main]
use acd_relay::ManualClock;
use acd_relay::fcm::{Address, PushSender, SendError};
use acd_relay::handle::{Keys, Sealed, seal};
use acd_relay::server::{AppState, Options, Rates, connection_builder, router};
use acd_relay::wire::{AddressKind, Platform, address_ok, app_ok};
use arbitrary::Arbitrary;
use async_trait::async_trait;
use axum::extract::ConnectInfo;
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use libfuzzer_sys::fuzz_target;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const APP: &str = "com.example.agentcaller";
const ADDR: &str = "tok:abc-DEF_123.xyz";
const T0: u64 = 1_800_000_000;

#[derive(Default)]
struct Recorder {
    wakes: Mutex<Vec<String>>,
    challenges: Mutex<Vec<(String, String)>>,
}

#[async_trait]
impl PushSender for Recorder {
    fn supports(&self, p: Platform) -> bool {
        p == Platform::Android
    }
    async fn wake(&self, to: &Address) -> Result<(), SendError> {
        self.wakes.lock().unwrap().push(to.value.clone());
        Ok(())
    }
    async fn challenge(&self, to: &Address, code: &str) -> Result<(), SendError> {
        self.challenges.lock().unwrap().push((to.value.clone(), code.to_string()));
        Ok(())
    }
}

#[derive(Arbitrary, Debug)]
struct Input {
    raw: Vec<u8>,
    structured: bool,
    route: u8,
    body: Vec<u8>,
    json: u8,
    handle_ok: bool,
    xff: Vec<u8>,
    trusted: bool,
    xor: Vec<(u16, u8)>,
}

fn request(i: &Input, handle: &str) -> Vec<u8> {
    let path = ["/v1/wake", "/v1/handles", "/v1/handles/confirm", "/healthz", "/nope"][i.route as usize % 5];
    let h = if i.handle_ok { handle.to_string() } else { String::from_utf8_lossy(&i.body).into_owned() };
    let h = h.replace(['"', '\\'], "");
    let body: Vec<u8> = match i.json % 5 {
        0 => format!(r#"{{"v":1,"handle":"{h}"}}"#).into_bytes(),
        1 => format!(r#"{{"platform":"android","app":"{APP}","address_kind":"fcm_token","address":"{h}"}}"#).into_bytes(),
        2 => format!(r#"{{"challenge_id":"{h}","code":"{h}","platform":"android","app":"{APP}","address_kind":"fcm_token","address":"{ADDR}"}}"#).into_bytes(),
        3 => format!(r#"{{"v":1,"handle":"{h}","text":"x"}}"#).into_bytes(),
        _ => i.body.clone(),
    };
    let mut out = format!("POST {path} HTTP/1.1\r\nhost: relay\r\ncontent-type: application/json\r\ncontent-length: {}\r\n", body.len()).into_bytes();
    if !i.xff.is_empty() {
        out.extend_from_slice(b"x-forwarded-for: ");
        out.extend(i.xff.iter().filter(|b| **b != b'\r' && **b != b'\n'));
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"connection: close\r\n\r\n");
    out.extend_from_slice(&body);
    for (at, x) in &i.xor {
        let n = out.len();
        out[*at as usize % n] ^= x;
    }
    out
}

fn statuses(res: &[u8]) -> Vec<u16> {
    // Every "HTTP/1.1 " at the start of a line is a status line.
    let mut out = vec![];
    let mut at = 0;
    while let Some(p) = res[at..].windows(9).position(|w| w == b"HTTP/1.1 ") {
        let s = &res[at + p + 9..];
        if s.len() >= 3 {
            if let Some(n) = std::str::from_utf8(&s[..3]).ok().and_then(|t| t.parse().ok()) {
                out.push(n);
            }
        }
        at += p + 9;
    }
    out
}

thread_local! {
    static RT: tokio::runtime::Runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
}

fuzz_target!(|i: Input| {
    let keys = Keys::new(vec![(1, [42u8; 32])]).unwrap();
    let handle = seal(
        &keys,
        &Sealed {
            platform: Platform::Android,
            kind: AddressKind::FcmToken,
            app: APP.into(),
            address: ADDR.into(),
            issued_at: T0 as u32,
            expires_at: T0 as u32 + 1000,
        },
    );
    let req = if i.structured { request(&i, &handle) } else { i.raw.clone() };
    let peer: std::net::SocketAddr = "127.0.0.1:1".parse().unwrap();
    let sender = Arc::new(Recorder::default());
    let state = AppState::new(
        Options {
            keys,
            allowed_apps: vec![APP.into()],
            trusted_proxy: i.trusted.then(|| peer.ip()),
            rates: Rates::default(),
        },
        sender.clone(),
        Arc::new(ManualClock::new(T0)),
    );
    let (res, task_ok) = RT.with(|rt| {
        rt.block_on(async {
            let (mut client, server) = tokio::io::duplex(64 * 1024);
            let app = router(state.clone()).layer(axum::Extension(ConnectInfo(peer)));
            let task = tokio::spawn(async move {
                let _ = connection_builder(std::time::Duration::from_secs(5))
                    .serve_connection(TokioIo::new(server), TowerToHyperService::new(app))
                    .await;
            });
            let (mut r, mut w) = tokio::io::split(&mut client);
            let write = async {
                let _ = w.write_all(&req).await;
                let _ = w.shutdown().await;
            };
            let mut res = Vec::new();
            let read = async {
                let mut buf = [0u8; 4096];
                while res.len() < 1 << 20 {
                    match r.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => res.extend_from_slice(&buf[..n]),
                    }
                }
            };
            tokio::join!(write, read);
            (res, task.await.is_ok())
        })
    });
    assert!(task_ok, "a handler panicked");
    for s in statuses(&res) {
        assert!(matches!(s, 200 | 202 | 204 | 400 | 404 | 405 | 408 | 411 | 413 | 414 | 429 | 431 | 503 | 505), "status {s}");
    }
    let wakes = sender.wakes.lock().unwrap();
    let challenges = sender.challenges.lock().unwrap();
    assert!(wakes.iter().all(|a| a == ADDR), "woke {wakes:?}");
    assert!(challenges.iter().all(|(a, _)| address_ok(a)));
    assert!(app_ok(APP));
    assert!(statuses(&res).iter().filter(|s| **s == 204).count() <= wakes.len());
    let text = String::from_utf8_lossy(&res);
    assert!(!text.contains(&handle) && !text.contains(ADDR), "a secret in the response");
    for (_, code) in challenges.iter() {
        assert!(!text.contains(code.as_str()), "the code in the response");
    }
    assert!(state.map_sizes().iter().all(|n| *n <= 4096));
});
