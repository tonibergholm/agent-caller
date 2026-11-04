#![allow(dead_code)]
//! Shared test harness: a fake push sender, a fake FCM + OAuth endpoint,
//! and helpers to run the relay on a loopback port.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use acd_relay::fcm::{Address, FcmClient, PushSender, SendError, ServiceAccount};
use acd_relay::handle::Keys;
use acd_relay::http::HttpClient;
use acd_relay::server::{AppState, Options, Rates, ServeOptions, serve_with};
use acd_relay::wire::Platform;
use acd_relay::{Clock, ManualClock};
use async_trait::async_trait;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use serde_json::{Value, json};

pub const APP: &str = "com.example.agentcaller";
pub const TEST_KEY_PEM: &str = include_str!("../fixtures/test_rsa_pkcs8.pem");

#[derive(Default)]
pub struct FakeSender {
    pub wakes: Mutex<Vec<String>>,
    pub challenges: Mutex<Vec<(String, String)>>,
    pub wake_result: Mutex<Option<SendError>>,
    pub gate: Mutex<Option<Arc<tokio::sync::Semaphore>>>,
}

impl FakeSender {
    pub fn wake_count(&self) -> usize {
        self.wakes.lock().unwrap().len()
    }
    pub fn last_code(&self) -> String {
        self.challenges.lock().unwrap().last().unwrap().1.clone()
    }
}

#[async_trait]
impl PushSender for FakeSender {
    fn supports(&self, p: Platform) -> bool {
        p == Platform::Android
    }
    async fn wake(&self, to: &Address) -> Result<(), SendError> {
        let gate = self.gate.lock().unwrap().clone();
        if let Some(g) = gate {
            let _p = g.acquire().await.unwrap();
        }
        self.wakes.lock().unwrap().push(to.value.clone());
        match *self.wake_result.lock().unwrap() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
    async fn challenge(&self, to: &Address, code: &str) -> Result<(), SendError> {
        self.challenges.lock().unwrap().push((to.value.clone(), code.to_string()));
        Ok(())
    }
}

pub struct Relay {
    pub addr: SocketAddr,
    pub state: Arc<AppState>,
    pub clock: Arc<ManualClock>,
    pub http: HttpClient,
    n: AtomicUsize,
}

pub fn keys() -> Keys {
    Keys::new(vec![(1, [42u8; 32])]).unwrap()
}

pub const T0: u64 = 1_800_000_000;

pub struct Setup {
    pub rates: Rates,
    pub trusted_proxy: Option<std::net::IpAddr>,
}

impl Default for Setup {
    fn default() -> Setup {
        Setup { rates: Rates::default(), trusted_proxy: Some("127.0.0.1".parse().unwrap()) }
    }
}

pub async fn start(setup: Setup, sender: Arc<dyn PushSender>) -> Relay {
    start_clock(setup, sender, Arc::new(ManualClock::new(T0))).await
}

pub async fn start_clock(setup: Setup, sender: Arc<dyn PushSender>, clock: Arc<ManualClock>) -> Relay {
    start_serving(setup, sender, clock, ServeOptions::default()).await
}

pub async fn start_serving(
    setup: Setup,
    sender: Arc<dyn PushSender>,
    clock: Arc<ManualClock>,
    opts: ServeOptions,
) -> Relay {
    let state = AppState::new(
        Options {
            keys: keys(),
            allowed_apps: vec![APP.into()],
            trusted_proxy: setup.trusted_proxy,
            rates: setup.rates,
        },
        sender,
        clock.clone(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve_with(listener, state.clone(), opts, std::future::pending()));
    Relay {
        addr,
        state,
        clock,
        http: HttpClient::plain_for_tests(Duration::from_secs(10), 64 * 1024),
        n: AtomicUsize::new(0),
    }
}

impl Relay {
    /// POST JSON; `ip` becomes the X-Forwarded-For (the harness trusts it).
    pub async fn post(&self, path: &str, body: &Value, ip: &str) -> (u16, Value) {
        self.post_raw(path, body.to_string().into_bytes(), ip).await
    }

    pub async fn post_raw(&self, path: &str, body: Vec<u8>, ip: &str) -> (u16, Value) {
        let url = format!("http://{}{path}", self.addr);
        let r = self.http.post(&url, "application/json", &[("x-forwarded-for", ip)], body).await.unwrap();
        (r.status, serde_json::from_slice(&r.body).unwrap_or(Value::Null))
    }

    /// A fresh test IP.
    pub fn ip(&self) -> String {
        let n = self.n.fetch_add(1, Ordering::SeqCst) + 1;
        format!("198.51.{}.{}", (n / 250) % 250, n % 250 + 1)
    }

    pub fn reg_body(address: &str) -> Value {
        json!({"platform":"android","app":APP,"address_kind":"fcm_token","address":address})
    }

    pub async fn register(&self, address: &str) -> (u16, Value) {
        let ip = self.ip();
        self.post("/v1/handles", &Relay::reg_body(address), &ip).await
    }

    pub fn confirm_body(id: &str, code: &str, address: &str) -> Value {
        json!({"challenge_id":id,"code":code,"platform":"android","app":APP,
               "address_kind":"fcm_token","address":address})
    }

    /// Full registration with a fake sender; returns the handle.
    pub async fn make_handle(&self, sender: &FakeSender, address: &str) -> String {
        let (s, b) = self.register(address).await;
        assert_eq!(s, 202, "{b}");
        let id = b["challenge_id"].as_str().unwrap().to_string();
        let ip = self.ip();
        let (s, b) = self
            .post("/v1/handles/confirm", &Relay::confirm_body(&id, &sender.last_code(), address), &ip)
            .await;
        assert_eq!(s, 200, "{b}");
        b["handle"].as_str().unwrap().to_string()
    }

    pub async fn wake(&self, handle: &str) -> u16 {
        let ip = self.ip();
        self.post("/v1/wake", &json!({"v":1,"handle":handle}), &ip).await.0
    }

    pub fn now(&self) -> u64 {
        self.clock.now_secs()
    }
}

// ---- fake FCM + OAuth ----

pub struct FcmState {
    pub token_requests: AtomicUsize,
    pub assertions: Mutex<Vec<String>>,
    pub sends: Mutex<Vec<(String, Value)>>,
    pub send_reply: Mutex<(u16, String)>,
    pub token_reply: Mutex<(u16, String)>,
}

pub struct FakeFcm {
    pub addr: SocketAddr,
    pub st: Arc<FcmState>,
}

pub const ACCESS_TOKEN: &str = "ya29.fake-access-token-for-tests";

pub async fn fake_fcm() -> FakeFcm {
    let st = Arc::new(FcmState {
        token_requests: AtomicUsize::new(0),
        assertions: Mutex::new(vec![]),
        sends: Mutex::new(vec![]),
        send_reply: Mutex::new((200, r#"{"name":"projects/p-1/messages/1"}"#.into())),
        token_reply: Mutex::new((200, format!(r#"{{"access_token":"{ACCESS_TOKEN}","expires_in":3600,"token_type":"Bearer"}}"#))),
    });
    async fn token(State(st): State<Arc<FcmState>>, body: String) -> (StatusCode, String) {
        assert!(body.starts_with("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion="));
        st.token_requests.fetch_add(1, Ordering::SeqCst);
        let jwt = body.split("assertion=").nth(1).unwrap().to_string();
        st.assertions.lock().unwrap().push(jwt);
        let (c, b) = st.token_reply.lock().unwrap().clone();
        (StatusCode::from_u16(c).unwrap(), b)
    }
    async fn send(State(st): State<Arc<FcmState>>, h: HeaderMap, body: String) -> (StatusCode, String) {
        let auth = h.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        st.sends.lock().unwrap().push((auth, serde_json::from_str(&body).unwrap()));
        let (c, b) = st.send_reply.lock().unwrap().clone();
        (StatusCode::from_u16(c).unwrap(), b)
    }
    let app = axum::Router::new()
        .route("/token", post(token))
        .route("/v1/projects/p-1/messages:send", post(send))
        .with_state(st.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await });
    FakeFcm { addr, st }
}

pub fn credentials_json(fake: &FakeFcm) -> String {
    json!({
        "type": "service_account",
        "project_id": "p-1",
        "client_email": "relay@p-1.iam.example",
        "private_key": TEST_KEY_PEM,
        "token_uri": format!("http://{}/token", fake.addr),
    })
    .to_string()
}

pub fn fcm_client(fake: &FakeFcm, clock: Arc<dyn Clock>) -> Arc<FcmClient> {
    let account = ServiceAccount::from_json_for_tests(credentials_json(fake).as_bytes()).unwrap();
    Arc::new(FcmClient::new(
        HttpClient::plain_for_tests(Duration::from_secs(10), 64 * 1024),
        account,
        format!("http://{}", fake.addr),
        "p-1".into(),
        clock,
    ))
}

/// Like [`fcm_client`] with the per-exchange HTTP timeout chosen by the test.
pub fn fcm_client_with_http_timeout(fake: &FakeFcm, clock: Arc<dyn Clock>, http: Duration) -> Arc<FcmClient> {
    let account = ServiceAccount::from_json_for_tests(credentials_json(fake).as_bytes()).unwrap();
    Arc::new(FcmClient::new(
        HttpClient::plain_for_tests(http, 64 * 1024),
        account,
        format!("http://{}", fake.addr),
        "p-1".into(),
        clock,
    ))
}

/// An FCM client whose OAuth endpoint is in memory and never answers. Every
/// connection is held open (the returned list keeps the far ends alive), so
/// the only thing that can end a refresh is a deadline. No sockets: nothing
/// outside the runtime can wake it, so paused time is deterministic.
pub fn client_with_stalled_oauth(
    fake: &FakeFcm,
    clock: Arc<dyn Clock>,
    http: Duration,
) -> (Arc<FcmClient>, Arc<Mutex<Vec<tokio::io::DuplexStream>>>) {
    let held = Arc::new(Mutex::new(vec![]));
    let sink = held.clone();
    let connect: acd_relay::http::Connector = Arc::new(move || {
        let (client, server) = tokio::io::duplex(64 * 1024);
        sink.lock().unwrap().push(server);
        client
    });
    let account = ServiceAccount::from_json_for_tests(credentials_json(fake).as_bytes()).unwrap();
    let c = Arc::new(FcmClient::new(
        HttpClient::in_memory_for_tests(http, 64 * 1024, connect),
        account,
        format!("http://{}", fake.addr),
        "p-1".into(),
        clock,
    ));
    (c, held)
}

/// A clock that follows tokio time (paused or not): `start` plus the time
/// since this clock was made. The relay's own clocks and its tokio timers
/// then move together, as they do in production.
pub struct TokioClock {
    start: u64,
    since: tokio::time::Instant,
}

impl TokioClock {
    pub fn new(start: u64) -> TokioClock {
        TokioClock { start, since: tokio::time::Instant::now() }
    }
}

impl Clock for TokioClock {
    fn now_secs(&self) -> u64 {
        self.start + self.since.elapsed().as_secs()
    }
}
