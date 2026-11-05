//! The whole chain in one process: acd core opens a request, the wake sender
//! calls the real `acd-relay` router, which calls a fake FCM. Includes a run
//! over real TLS with `ca_file`, which is the only test of the CA path.
//!
//! Handles and push addresses are secrets: no assertion prints one.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use acd_core::testing::{agent_obs, permission_obs, test_machine};
use acd_core::{Core, Observation};
use acd_protocol::*;
use acd_relay::client::{MAX_RESPONSE, Status};
use acd_relay::fcm::{Address, FcmClient, ServiceAccount, wake_body};
use acd_relay::handle::Keys;
use acd_relay::http::HttpClient;
use acd_relay::provisional::{WAKE_BODY, WAKE_TITLE};
use acd_relay::server::{AppState, Options, Rates, ServeOptions, serve_with};
use acd_relay::wire::AddressKind;
use acd_relay::ManualClock;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use serde_json::{Value, json};
use tokio::sync::watch;

use super::tests::{T0, private_dir};
use super::*;

const APP: &str = "com.example.agentcaller";
const KEY_PEM: &str = include_str!("../../../acd-relay/tests/fixtures/test_rsa_pkcs8.pem");
/// Valid push-address characters, and unmistakable in any output.
const ADDRESS: &str = "fcm-token-PLANTED-ADDRESS:7e1c9a";

struct FakeFcm {
    addr: SocketAddr,
    sends: Mutex<Vec<Value>>,
    send_reply: Mutex<(u16, String)>,
    count: watch::Sender<usize>,
    token_requests: AtomicUsize,
}

impl FakeFcm {
    async fn start() -> Arc<FakeFcm> {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let fcm = Arc::new(FakeFcm {
            addr: l.local_addr().unwrap(),
            sends: Mutex::default(),
            send_reply: Mutex::new((200, r#"{"name":"projects/p-1/messages/1"}"#.into())),
            count: watch::channel(0).0,
            token_requests: AtomicUsize::new(0),
        });
        async fn token(State(f): State<Arc<FakeFcm>>) -> (StatusCode, String) {
            f.token_requests.fetch_add(1, Ordering::SeqCst);
            (StatusCode::OK, r#"{"access_token":"ya29.fake","expires_in":3600,"token_type":"Bearer"}"#.into())
        }
        async fn send(State(f): State<Arc<FakeFcm>>, _h: HeaderMap, body: String) -> (StatusCode, String) {
            f.sends.lock().unwrap().push(serde_json::from_str(&body).unwrap());
            f.count.send_modify(|n| *n += 1);
            let (c, b) = f.send_reply.lock().unwrap().clone();
            (StatusCode::from_u16(c).unwrap(), b)
        }
        let app = axum::Router::new()
            .route("/token", post(token))
            .route("/v1/projects/p-1/messages:send", post(send))
            .with_state(fcm.clone());
        tokio::spawn(async move { axum::serve(l, app).await });
        fcm
    }

    async fn wait_sends(&self, n: usize) {
        let _ = self.count.subscribe().wait_for(|c| *c >= n).await;
    }

    fn sends(&self) -> Vec<Value> {
        self.sends.lock().unwrap().clone()
    }

    fn wakes(&self) -> Vec<Value> {
        self.sends().into_iter().filter(|m| m["message"]["data"]["t"] != "challenge").collect()
    }

    fn last_challenge_code(&self) -> String {
        let sends = self.sends();
        let m = sends.iter().rev().find(|m| m["message"]["data"]["t"] == "challenge").expect("a challenge push");
        m["message"]["data"]["c"].as_str().unwrap().to_string()
    }
}

/// The real relay router in this process, sending through [`FakeFcm`].
struct RealRelay {
    addr: SocketAddr,
    fcm: Arc<FakeFcm>,
    http: HttpClient,
}

impl RealRelay {
    async fn start() -> RealRelay {
        let fcm = FakeFcm::start().await;
        let clock = Arc::new(ManualClock::new(T0));
        let creds = json!({
            "type": "service_account", "project_id": "p-1", "client_email": "relay@p-1.iam.example",
            "private_key": KEY_PEM, "token_uri": format!("http://{}/token", fcm.addr),
        });
        let account = ServiceAccount::from_json_for_tests(creds.to_string().as_bytes()).unwrap();
        let client = Arc::new(FcmClient::new(
            HttpClient::plain_for_tests(Duration::from_secs(10), 64 * 1024),
            account,
            format!("http://{}", fcm.addr),
            "p-1".into(),
            clock.clone(),
        ));
        let state = AppState::new(
            Options {
                keys: Keys::new(vec![(1, [42u8; 32])]).unwrap(),
                allowed_apps: vec![APP.into()],
                trusted_proxy: None,
                rates: Rates::default(),
            },
            client,
            clock,
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(serve_with(l, state, ServeOptions::default(), std::future::pending()));
        RealRelay { addr, fcm, http: HttpClient::plain_for_tests(Duration::from_secs(10), 64 * 1024) }
    }

    async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        let url = format!("http://{}{path}", self.addr);
        let r = self.http.post(&url, "application/json", &[], body.to_string().into_bytes()).await.unwrap();
        (r.status, serde_json::from_slice(&r.body).unwrap_or(Value::Null))
    }

    /// What the app does (relay spec §2.2 steps 2-3): returns the handle and its expiry.
    async fn issue_handle(&self, address: &str) -> (String, String) {
        let reg = json!({"platform":"android","app":APP,"address_kind":"fcm_token","address":address});
        let (status, b) = self.post("/v1/handles", reg).await;
        assert_eq!(status, 202);
        let id = b["challenge_id"].as_str().unwrap().to_string();
        let code = self.fcm.last_challenge_code();
        let confirm = json!({"challenge_id":id,"code":code,"platform":"android","app":APP,
                             "address_kind":"fcm_token","address":address});
        let (status, b) = self.post("/v1/handles/confirm", confirm).await;
        assert_eq!(status, 200);
        (b["handle"].as_str().unwrap().to_string(), b["expires_at"].as_str().unwrap().to_string())
    }
}

struct Chain {
    relay: RealRelay,
    core: Arc<Core>,
    push: Arc<Push>,
    _dir: tempfile::TempDir,
}

impl Chain {
    /// acd talks to the relay over plain HTTP on loopback (test sender).
    async fn plain() -> Chain {
        let relay = RealRelay::start().await;
        let url = RelayUrl::for_tests(&format!("http://{}", relay.addr));
        let sender = WakeSender::new(
            HttpClient::plain_for_tests(Duration::from_secs(5), MAX_RESPONSE),
            url.clone(),
            Arc::new(ManualClock::new(T0)),
        );
        Chain::with(relay, url, sender)
    }

    fn with(relay: RealRelay, url: RelayUrl, sender: WakeSender) -> Chain {
        let dir = private_dir();
        let push = Push::new(&url, sender, Store::load(dir.path()), Settings { min_interval: Duration::ZERO, ..Settings::default() });
        let core = Core::new(test_machine(), vec![]);
        assert!(core.set_push(push.clone()));
        push.spawn();
        Chain { relay, core, push, _dir: dir }
    }

    /// The device pairs for push: relay registration, then `push.register`.
    async fn enroll(&self, client: &str) {
        let (handle, expires_at) = self.relay.issue_handle(ADDRESS).await;
        let r = self
            .core
            .handle(client, ClientMessage::PushRegister {
                id: format!("reg-{client}"),
                relay: self.push.relay.clone(),
                handle,
                platform: PushPlatform::Android,
                expires_at: expires_at.parse().unwrap(),
            })
            .await;
        assert!(r.ok, "push.register refused: {:?}", r.error_code());
    }

    fn open_request(&self, n: usize) {
        let agent = format!("agent-{n}");
        self.core.observe(Observation::AgentUpsert(agent_obs(&agent)));
        self.core.observe(Observation::RequestSeen(permission_obs(&agent, &format!("key-{n}"))));
    }

    fn state(&self, client: &str) -> acd_relay::client::WakeState {
        self.push.store().get(client).unwrap().device.wake_state()
    }
}

fn address() -> Address {
    Address { kind: AddressKind::FcmToken, value: ADDRESS.into() }
}

#[tokio::test]
async fn an_opened_request_reaches_the_phone_as_one_fixed_push() {
    const PLANTED: [&str; 5] = [
        "PLANTED-TITLE-1b2c",
        "PLANTED-DETAIL-3d4e",
        "PLANTED-AGENT-5f6a",
        "PLANTED-PROJECT-7b8c",
        "PLANTED-OPTION-9d0e",
    ];
    let chain = Chain::plain().await;
    chain.enroll("phone").await;
    // Registration sent the challenge, nothing more.
    assert_eq!(chain.relay.fcm.sends().len(), 1);

    let mut agent = agent_obs("agent-planted");
    agent.title = PLANTED[2].into();
    agent.project = Some(PLANTED[3].into());
    chain.core.observe(Observation::AgentUpsert(agent));
    let mut req = permission_obs("agent-planted", "key-planted");
    req.title = PLANTED[0].into();
    req.detail = PLANTED[1].into();
    req.options[0].label = PLANTED[4].into();
    chain.core.observe(Observation::RequestSeen(req));
    chain.relay.fcm.wait_sends(2).await;
    chain.push.handled(1).await;

    let wakes = chain.relay.fcm.wakes();
    assert_eq!(wakes.len(), 1, "exactly one push");
    // The fixed content, and nothing else: the whole message is the relay's
    // constant wake for this address.
    assert!(wakes[0] == wake_body(&address()), "the push is not the fixed wake");
    let m = &wakes[0]["message"];
    assert_eq!(m["data"], json!({"v": "1"}));
    let note = &m["androidV2"]["remoteNotification"]["notification"];
    assert_eq!((note["title"].as_str(), note["body"].as_str()), (Some(WAKE_TITLE), Some(WAKE_BODY)));
    // Nothing derived from the request, the agent or the machine.
    let wire = serde_json::to_string(&wakes).unwrap();
    for text in PLANTED.into_iter().chain(["Test", "test-machine", "agent-planted", "key-planted"]) {
        assert!(!wire.contains(text), "request or machine text reached the push");
    }
    assert_eq!(chain.state("phone").status, Status::Ok);
    assert_eq!(chain.core.handle("phone", ClientMessage::PushStatus { id: "s".into() }).await.push.unwrap().status, Some(PushDeviceStatus::Ok));
}

#[tokio::test]
async fn a_second_request_sends_a_second_push_and_a_resolution_sends_none() {
    let chain = Chain::plain().await;
    chain.enroll("phone").await;
    chain.open_request(1);
    chain.push.handled(1).await;
    chain.core.observe(Observation::RequestEnded {
        agent_id: "agent-1".into(),
        key: acd_core::CorrelationKey("key-1".into()),
        by: ResolvedBy::Inbox,
    });
    chain.open_request(2);
    chain.push.handled(2).await;
    chain.relay.fcm.wait_sends(3).await;
    assert_eq!(chain.relay.fcm.wakes().len(), 2);
}

#[tokio::test]
async fn a_dead_push_address_disables_the_registration_through_the_real_relay() {
    let chain = Chain::plain().await;
    chain.enroll("phone").await;
    *chain.relay.fcm.send_reply.lock().unwrap() = (404, r#"{"error":{"status":"UNREGISTERED"}}"#.into());
    chain.open_request(1);
    chain.push.handled(1).await;
    assert_eq!(chain.state("phone").status, Status::Expired);
    chain.open_request(2);
    chain.push.handled(2).await;
    assert_eq!(chain.relay.fcm.wakes().len(), 1, "no wake after the registration was disabled");
}

#[tokio::test]
async fn a_push_service_outage_is_transient_and_recovers() {
    let chain = Chain::plain().await;
    chain.enroll("phone").await;
    *chain.relay.fcm.send_reply.lock().unwrap() = (503, r#"{"error":{"status":"UNAVAILABLE"}}"#.into());
    chain.open_request(1);
    chain.push.handled(1).await;
    assert_eq!(chain.state("phone").status, Status::RelayError);
    assert!(!chain.state("phone").disabled());
}

// ---- over real TLS ----

struct Pki {
    ca_pem: String,
    leaf_cert: rustls::pki_types::CertificateDer<'static>,
    leaf_key: rustls::pki_types::PrivateKeyDer<'static>,
}

fn pki() -> Pki {
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params.distinguished_name.push(rcgen::DnType::CommonName, "acd test CA");
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let issuer = rcgen::Issuer::new(ca_params, ca_key);
    let leaf_key = rcgen::KeyPair::generate().unwrap();
    let mut p = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    p.subject_alt_names = vec![rcgen::SanType::IpAddress("127.0.0.1".parse().unwrap())];
    let leaf = p.signed_by(&leaf_key, &issuer).unwrap();
    Pki {
        ca_pem: ca.pem(),
        leaf_cert: leaf.der().clone(),
        leaf_key: rustls::pki_types::PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(leaf_key.serialize_der())),
    }
}

/// A TLS front for the relay: terminates TLS and copies bytes to it, as the
/// reverse proxy of the supported deployment does. Returns its address.
async fn tls_front(pki: Pki, to: SocketAddr) -> SocketAddr {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let cfg = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![pki.leaf_cert], pki.leaf_key)
        .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(cfg));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = l.accept().await else { return };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(tcp).await else { return };
                let Ok(mut back) = tokio::net::TcpStream::connect(to).await else { return };
                let _ = tokio::io::copy_bidirectional(&mut tls, &mut back).await;
            });
        }
    });
    addr
}

/// Resolves `[push]` from TOML as the daemon does and builds the chain on it.
async fn tls_chain(trusted_ca: Option<&str>) -> (Chain, tempfile::TempDir) {
    let relay = RealRelay::start().await;
    let pki = pki();
    let real_ca = pki.ca_pem.clone();
    let front = tls_front(pki, relay.addr).await;
    let files = tempfile::tempdir().unwrap();
    let ca_path = files.path().join("relay-ca.pem");
    std::fs::write(&ca_path, trusted_ca.unwrap_or(&real_ca)).unwrap();
    let section: toml::Value = toml::from_str(&format!(
        "[push]\nrelay = \"https://{front}\"\nca_file = \"{}\"\n",
        ca_path.display()
    ))
    .unwrap();
    let Resolved::On { relay: url, sender } = resolve(section.get("push"), &[], Arc::new(ManualClock::new(T0))) else {
        panic!("push did not resolve to on");
    };
    (Chain::with(relay, url, sender), files)
}

#[tokio::test]
async fn over_tls_with_the_configured_ca_a_wake_reaches_the_push_service() {
    let (chain, _files) = tls_chain(None).await;
    chain.enroll("phone").await;
    chain.open_request(1);
    chain.relay.fcm.wait_sends(2).await;
    chain.push.handled(1).await;
    assert_eq!(chain.relay.fcm.wakes().len(), 1);
    assert_eq!(chain.state("phone").status, Status::Ok);
}

#[tokio::test]
async fn a_relay_certificate_that_does_not_chain_to_the_ca_file_gets_no_wake() {
    // F4: the sender trusts only `ca_file`. A relay presenting a certificate
    // from another CA is not talked to.
    let other = pki().ca_pem;
    let (chain, _files) = tls_chain(Some(&other)).await;
    // Register straight in the store: the app's own registration path is not
    // what is under test, and it would talk to the relay without TLS.
    let (handle, expires_at) = chain.relay.issue_handle(ADDRESS).await;
    let r = chain
        .core
        .handle("phone", ClientMessage::PushRegister {
            id: "reg".into(),
            relay: chain.push.relay.clone(),
            handle,
            platform: PushPlatform::Android,
            expires_at: expires_at.parse().unwrap(),
        })
        .await;
    assert!(r.ok);
    chain.open_request(1);
    chain.push.handled(1).await;
    assert_eq!(chain.state("phone").status, Status::RelayError);
    assert!(chain.relay.fcm.wakes().is_empty(), "a wake was sent over an untrusted connection");
}
