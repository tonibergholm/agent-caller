//! Push: registration, storage, the wake sender and its failure rules, against
//! a fake relay. The same chain through the real relay is in `e2e.rs`.
//!
//! Handles are secrets: no assertion here prints one.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use acd_core::testing::{agent_obs, permission_obs, test_machine};
use acd_core::{Core, Observation};
use acd_protocol::*;
use acd_relay::ManualClock;
use acd_relay::client::{MAX_RESPONSE, Status, WakeState};
use acd_relay::http::HttpClient;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::routing::post;
use tokio::sync::{Semaphore, watch};

use super::*;

pub const T0: u64 = 1_800_000_000;
const NO_WINDOW: Settings = Settings { min_interval: Duration::ZERO, max_concurrent: MAX_CONCURRENT };

/// A handle that looks like one and is unique per `name`.
pub fn handle_for(name: &str) -> String {
    format!("H{name}-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
}

#[derive(Clone)]
pub struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
    pub location: Option<String>,
}

impl Reply {
    pub fn status(status: u16) -> Reply {
        Reply { status, body: vec![], location: None }
    }
    pub fn json(status: u16, body: &str) -> Reply {
        Reply { status, body: body.as_bytes().to_vec(), location: None }
    }
}

/// A relay that records what it is sent and answers as scripted.
pub struct FakeRelay {
    pub url: RelayUrl,
    bodies: Mutex<Vec<String>>,
    replies: Mutex<VecDeque<Reply>>,
    default: Mutex<Reply>,
    hits: watch::Sender<usize>,
    in_flight: watch::Sender<usize>,
    max_in_flight: AtomicUsize,
    gate: Mutex<Option<Arc<Semaphore>>>,
    hang: AtomicBool,
}

impl FakeRelay {
    pub async fn start() -> Arc<FakeRelay> {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = RelayUrl::for_tests(&format!("http://{}", l.local_addr().unwrap()));
        let relay = Arc::new(FakeRelay {
            url,
            bodies: Mutex::default(),
            replies: Mutex::default(),
            default: Mutex::new(Reply::status(204)),
            hits: watch::channel(0).0,
            in_flight: watch::channel(0).0,
            max_in_flight: AtomicUsize::new(0),
            gate: Mutex::default(),
            hang: AtomicBool::new(false),
        });
        async fn wake(State(f): State<Arc<FakeRelay>>, body: String) -> (StatusCode, HeaderMap, Vec<u8>) {
            f.bodies.lock().unwrap().push(body);
            f.hits.send_modify(|n| *n += 1);
            f.in_flight.send_modify(|n| {
                *n += 1;
                f.max_in_flight.fetch_max(*n, Ordering::SeqCst);
            });
            struct Leave<'a>(&'a FakeRelay);
            impl Drop for Leave<'_> {
                fn drop(&mut self) {
                    self.0.in_flight.send_modify(|n| *n -= 1);
                }
            }
            let _leave = Leave(&f);
            let gate = f.gate.lock().unwrap().clone();
            if let Some(g) = gate {
                drop(g.acquire().await.unwrap());
            }
            if f.hang.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            let r = f.replies.lock().unwrap().pop_front().unwrap_or_else(|| f.default.lock().unwrap().clone());
            let mut h = HeaderMap::new();
            if let Some(l) = &r.location {
                h.insert(header::LOCATION, l.parse().unwrap());
            }
            (StatusCode::from_u16(r.status).unwrap(), h, r.body)
        }
        let app = axum::Router::new().route("/v1/wake", post(wake)).with_state(relay.clone());
        tokio::spawn(async move { axum::serve(l, app).await });
        relay
    }

    pub fn reply(&self, r: Reply) {
        self.replies.lock().unwrap().push_back(r);
    }
    pub fn set_default(&self, r: Reply) {
        *self.default.lock().unwrap() = r;
    }
    pub fn hits(&self) -> usize {
        *self.hits.borrow()
    }
    pub async fn wait_hits(&self, n: usize) {
        let _ = self.hits.subscribe().wait_for(|h| *h >= n).await;
    }
    pub async fn wait_in_flight(&self, n: usize) {
        let _ = self.in_flight.subscribe().wait_for(|h| *h >= n).await;
    }
    pub fn max_in_flight(&self) -> usize {
        self.max_in_flight.load(Ordering::SeqCst)
    }
    pub fn bodies(&self) -> Vec<String> {
        self.bodies.lock().unwrap().clone()
    }
    /// Holds every request until `release` adds permits.
    pub fn hold(&self) -> Arc<Semaphore> {
        let g = Arc::new(Semaphore::new(0));
        *self.gate.lock().unwrap() = Some(g.clone());
        g
    }
    pub fn hang(&self) {
        self.hang.store(true, Ordering::SeqCst);
    }
}

pub struct Rig {
    pub dir: tempfile::TempDir,
    pub relay: Arc<FakeRelay>,
    pub clock: Arc<ManualClock>,
    pub push: Arc<Push>,
    pub core: Arc<Core>,
    next_id: AtomicUsize,
}

pub fn private_dir() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let d = tempfile::tempdir().unwrap();
    std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    d
}

impl Rig {
    pub async fn new(settings: Settings) -> Rig {
        Rig::with_timeout(settings, Duration::from_secs(5)).await
    }

    pub async fn with_timeout(settings: Settings, timeout: Duration) -> Rig {
        Rig::in_dir(private_dir(), FakeRelay::start().await, settings, timeout)
    }

    /// A daemon "start": loads `push.json` from `dir` and runs against `relay`.
    pub fn in_dir(dir: tempfile::TempDir, relay: Arc<FakeRelay>, settings: Settings, timeout: Duration) -> Rig {
        let clock = Arc::new(ManualClock::new(T0));
        let sender = WakeSender::new(
            HttpClient::plain_for_tests(timeout, MAX_RESPONSE),
            relay.url.clone(),
            clock.clone(),
        );
        let push = Push::new(&relay.url, sender, Store::load(dir.path()), settings);
        let core = Core::new(test_machine(), vec![]);
        assert!(core.set_push(push.clone()));
        push.spawn();
        Rig { dir, relay, clock, push, core, next_id: AtomicUsize::new(0) }
    }

    pub fn id(&self) -> String {
        format!("c{}", self.next_id.fetch_add(1, Ordering::SeqCst))
    }

    pub fn register_msg(&self, handle: &str) -> ClientMessage {
        ClientMessage::PushRegister {
            id: self.id(),
            relay: self.relay.url.as_str().to_string(),
            handle: handle.to_string(),
            platform: PushPlatform::Android,
            expires_at: "2026-12-04T10:00:00Z".parse().unwrap(),
        }
    }

    pub async fn register(&self, client: &str, handle: &str) -> CommandResult {
        self.core.handle(client, self.register_msg(handle)).await
    }

    /// Registers `client` and checks it was accepted.
    pub async fn enroll(&self, client: &str) {
        let r = self.register(client, &handle_for(client)).await;
        assert!(r.ok, "registration refused: {:?}", r.error_code());
    }

    /// Opens one more request; each is a different agent.
    pub fn open(&self, n: usize) {
        let agent = format!("agent-{n}");
        self.core.observe(Observation::AgentUpsert(agent_obs(&agent)));
        self.core.observe(Observation::RequestSeen(permission_obs(&agent, &format!("key-{n}"))));
    }

    pub fn state(&self, client: &str) -> WakeState {
        self.push.store().get(client).expect("registered").device.wake_state()
    }

    pub async fn status(&self, client: &str) -> PushInfo {
        let r = self.core.handle(client, ClientMessage::PushStatus { id: self.id() }).await;
        r.push.expect("push.status carries push")
    }
}

// ---- the wake carries no content ----

#[test]
fn the_wake_wire_type_has_no_free_text_field() {
    use acd_relay::wire::WakeRequest;
    let schema = serde_json::to_value(schemars::schema_for!(WakeRequest)).unwrap();
    let mut props: Vec<&str> = schema["properties"].as_object().unwrap().keys().map(String::as_str).collect();
    props.sort();
    assert_eq!(props, ["handle", "v"]);
    assert_eq!(schema["properties"]["v"]["type"], "integer");
    assert_eq!(schema["properties"]["handle"]["type"], "string");
    assert_eq!(schema["additionalProperties"], serde_json::json!(false));
}

#[test]
fn the_opened_signal_takes_no_argument() {
    // If `request_opened` ever takes a request, an id or a name, this stops
    // compiling: nothing about a request can reach the sender through it.
    let _: fn(&Push) = <Push as PushControl>::request_opened;
}

#[tokio::test]
async fn the_wake_body_is_exactly_the_version_and_the_registered_handle() {
    let rig = Rig::new(NO_WINDOW).await;
    rig.enroll("phone").await;
    rig.open(1);
    rig.relay.wait_hits(1).await;
    let bodies = rig.relay.bodies();
    assert_eq!(bodies.len(), 1);
    let v: serde_json::Value = serde_json::from_str(&bodies[0]).unwrap();
    let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(keys, ["handle", "v"]);
    assert_eq!(v["v"], 1);
    assert!(v["handle"].as_str() == Some(handle_for("phone").as_str()), "the handle is not the registered one");
}

#[tokio::test]
async fn nothing_known_about_the_request_reaches_the_relay() {
    const PLANTED: [&str; 6] = [
        "PLANTED-AGENT-TITLE-7f3a",
        "PLANTED-TITLE-9c1d",
        "PLANTED-DETAIL-4b8e",
        "PLANTED-OPTION-LABEL-2e6f",
        "PLANTED-PROJECT-5a0c",
        "PLANTED-CWD-/home/planted-3d7b",
    ];
    let rig = Rig::new(NO_WINDOW).await;
    rig.enroll("phone").await;
    let mut agent = agent_obs("agent-planted");
    agent.title = PLANTED[0].into();
    agent.project = Some(PLANTED[4].into());
    agent.cwd = Some(PLANTED[5].into());
    rig.core.observe(Observation::AgentUpsert(agent));
    let mut req = permission_obs("agent-planted", "key-planted");
    req.title = PLANTED[1].into();
    req.detail = PLANTED[2].into();
    req.options[0].label = PLANTED[3].into();
    rig.core.observe(Observation::RequestSeen(req));
    rig.relay.wait_hits(1).await;
    rig.push.handled(1).await;
    // What the relay was sent, and what the request's id was: the id restarts
    // at r1 on every start and must not be used as an identifier.
    let ServerMessage::Snapshot { agents, .. } = rig.core.attach(None).0.remove(0) else { panic!("snapshot") };
    let request_id = agents[0].pending.as_ref().unwrap().id.clone();
    let wire = rig.relay.bodies().join("\n");
    for text in PLANTED.into_iter().chain(["Test", "test-machine", "agent-planted", "key-planted"]) {
        assert!(!wire.contains(text), "request or machine text reached the relay");
    }
    let v: serde_json::Value = serde_json::from_str(&rig.relay.bodies()[0]).unwrap();
    assert_ne!(v["handle"].as_str(), Some(request_id.as_str()));
    assert!(v.get("request_id").is_none() && v.get("id").is_none());
}

// ---- when a wake is sent ----

#[tokio::test]
async fn an_opened_request_wakes_each_registered_device_once() {
    let rig = Rig::new(NO_WINDOW).await;
    for c in ["phone-a", "phone-b", "phone-c"] {
        rig.enroll(c).await;
    }
    rig.open(1);
    rig.push.handled(1).await;
    assert_eq!(rig.relay.hits(), 3);
    let mut seen = rig.relay.bodies();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 3, "three different handles");
}

#[tokio::test]
async fn no_registered_device_means_no_wake() {
    let rig = Rig::new(NO_WINDOW).await;
    rig.open(1);
    rig.push.handled(1).await;
    assert_eq!(rig.relay.hits(), 0);
}

#[tokio::test]
async fn a_flood_of_requests_wakes_a_device_once_per_window() {
    let rig = Rig::new(Settings { min_interval: Duration::from_secs(3600), max_concurrent: MAX_CONCURRENT }).await;
    rig.enroll("phone").await;
    // Each request is handled before the next opens, so the debounce, not the
    // coalescing of a burst, is what holds the wakes back.
    for n in 0..50 {
        rig.open(n);
        rig.push.handled(n as u64 + 1).await;
    }
    // One immediate wake; the trailing one is owed for the window's end.
    assert_eq!(rig.relay.hits(), 1);
}

#[tokio::test]
async fn a_request_inside_the_window_gets_its_trailing_wake() {
    // The window is short enough to wait out, long enough that the second
    // request normally lands inside it. Either way the device must end up
    // woken twice, and never more.
    let rig = Rig::new(Settings { min_interval: Duration::from_millis(100), max_concurrent: MAX_CONCURRENT }).await;
    rig.enroll("phone").await;
    rig.open(1);
    rig.relay.wait_hits(1).await;
    rig.open(2);
    rig.relay.wait_hits(2).await;
    rig.push.handled(2).await;
    assert_eq!(rig.relay.hits(), 2);
}

#[tokio::test]
async fn sends_run_together_but_never_more_than_the_bound() {
    let rig = Rig::new(Settings { min_interval: Duration::ZERO, max_concurrent: 4 }).await;
    let gate = rig.relay.hold();
    for n in 0..store::MAX_DEVICES {
        rig.enroll(&format!("phone-{n}")).await;
    }
    rig.open(1);
    rig.relay.wait_in_flight(4).await;
    gate.add_permits(store::MAX_DEVICES);
    rig.push.handled(1).await;
    assert_eq!(rig.relay.hits(), store::MAX_DEVICES, "every device was woken");
    assert_eq!(rig.relay.max_in_flight(), 4, "no more than the bound at once");
}

// ---- failure and recovery ----

#[tokio::test]
async fn a_relay_that_is_down_backs_off_and_recovers_by_itself() {
    let rig = Rig::new(NO_WINDOW).await;
    rig.enroll("phone").await;
    rig.relay.set_default(Reply::json(503, r#"{"error":"unavailable"}"#));
    rig.open(1);
    rig.push.handled(1).await;
    assert_eq!(rig.relay.hits(), 1);
    assert_eq!(rig.state("phone").status, Status::RelayError);
    assert_eq!(rig.status("phone").await.status, Some(PushDeviceStatus::RelayError));

    // Inside the backoff window the wake is dropped without a request.
    rig.open(2);
    rig.push.handled(2).await;
    assert_eq!(rig.relay.hits(), 1, "skipped while backing off");

    // The relay is back and the window has passed: the next wake goes out and
    // clears the failure. Nothing was queued for the wakes that were dropped.
    rig.relay.set_default(Reply::status(204));
    rig.clock.advance(acd_relay::client::backoff_secs(1));
    rig.open(3);
    rig.push.handled(3).await;
    assert_eq!(rig.relay.hits(), 2);
    let st = rig.status("phone").await;
    assert_eq!(st.status, Some(PushDeviceStatus::Ok));
    assert!(st.last_wake_at.is_some());
}

#[tokio::test]
async fn a_refused_connection_is_transient_not_fatal() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = RelayUrl::for_tests(&format!("http://{}", l.local_addr().unwrap()));
    drop(l);
    let dir = private_dir();
    let clock = Arc::new(ManualClock::new(T0));
    let sender = WakeSender::new(HttpClient::plain_for_tests(Duration::from_secs(5), MAX_RESPONSE), url.clone(), clock);
    let push = Push::new(&url, sender, Store::load(dir.path()), NO_WINDOW);
    let core = Core::new(test_machine(), vec![]);
    core.set_push(push.clone());
    push.spawn();
    let r = core
        .handle("phone", ClientMessage::PushRegister {
            id: "c1".into(),
            relay: url.as_str().into(),
            handle: handle_for("phone"),
            platform: PushPlatform::Android,
            expires_at: "2026-12-04T10:00:00Z".parse().unwrap(),
        })
        .await;
    assert!(r.ok);
    core.observe(Observation::AgentUpsert(agent_obs("a")));
    core.observe(Observation::RequestSeen(permission_obs("a", "k")));
    push.handled(1).await;
    let e = push.store().get("phone").unwrap().device.wake_state();
    assert_eq!(e.status, Status::RelayError);
    assert_eq!(e.failures, 1);
}

#[tokio::test]
async fn an_invalid_registration_is_disabled_until_the_device_registers_again() {
    for (reply, expect) in [
        (Reply::json(404, r#"{"error":"gone"}"#), PushDeviceStatus::Expired),
        (Reply::json(400, r#"{"error":"invalid_handle"}"#), PushDeviceStatus::Invalid),
    ] {
        let rig = Rig::new(NO_WINDOW).await;
        rig.enroll("phone").await;
        rig.relay.set_default(reply);
        rig.open(1);
        rig.push.handled(1).await;
        assert_eq!(rig.status("phone").await.status, Some(expect));

        // Disabled: later requests, even after a long time, send nothing.
        rig.clock.advance(100_000);
        rig.open(2);
        rig.push.handled(2).await;
        assert_eq!(rig.relay.hits(), 1);

        // The state survives a restart.
        let reloaded = Store::load(rig.dir.path());
        assert!(reloaded.degraded().is_none());
        assert_eq!(reloaded.get("phone").unwrap().device.status(), expect);

        // Registering again enables it.
        rig.relay.set_default(Reply::status(204));
        assert!(rig.register("phone", &handle_for("phone")).await.ok);
        assert_eq!(rig.status("phone").await.status, Some(PushDeviceStatus::Ok));
        rig.open(3);
        rig.push.handled(3).await;
        assert_eq!(rig.relay.hits(), 2);
    }
}

#[tokio::test]
async fn only_an_invalid_registration_disables_other_answers_do_not() {
    // 429 is dropped without a state change; every other answer outside the
    // table is transient. None of them disables.
    for (reply, expect) in [
        (Reply::json(429, r#"{"error":"rate_limited"}"#), Status::Ok),
        (Reply::json(400, r#"{"error":"bad_request"}"#), Status::RelayError),
        (Reply::json(401, "{}"), Status::RelayError),
        (Reply::json(403, "{}"), Status::RelayError),
        (Reply::json(413, r#"{"error":"too_large"}"#), Status::RelayError),
        (Reply::json(404, r#"{"error":"not_found"}"#), Status::RelayError),
        (Reply::json(500, "boom"), Status::RelayError),
        (Reply::json(200, "ok"), Status::RelayError),
    ] {
        let rig = Rig::new(NO_WINDOW).await;
        rig.enroll("phone").await;
        rig.relay.set_default(reply.clone());
        rig.open(1);
        rig.push.handled(1).await;
        assert_eq!(rig.state("phone").status, expect, "status {}", reply.status);
        assert!(!rig.state("phone").disabled());
    }
}

#[tokio::test]
async fn a_redirect_is_not_followed() {
    let elsewhere = FakeRelay::start().await;
    let rig = Rig::new(NO_WINDOW).await;
    rig.enroll("phone").await;
    let mut r = Reply::status(307);
    r.location = Some(format!("{}/v1/wake", elsewhere.url.as_str()));
    rig.relay.set_default(r);
    rig.open(1);
    rig.push.handled(1).await;
    assert_eq!(rig.relay.hits(), 1);
    assert_eq!(elsewhere.hits(), 0, "the redirect target was contacted");
    assert_eq!(rig.state("phone").status, Status::RelayError);
}

#[tokio::test]
async fn a_huge_response_body_is_cut_off_and_treated_as_a_failure() {
    let rig = Rig::new(NO_WINDOW).await;
    rig.enroll("phone").await;
    let mut huge = Reply::status(200);
    huge.body = vec![b'x'; 16 << 20];
    rig.relay.set_default(huge);
    rig.open(1);
    rig.push.handled(1).await;
    assert_eq!(rig.state("phone").status, Status::RelayError);
    // Not even a plausible success is believed when it comes with a big body
    // of a status outside the table.
    assert_eq!(rig.state("phone").failures, 1);
}

#[tokio::test]
async fn a_relay_that_never_answers_hits_the_deadline() {
    // The sender's deadline is the only thing that can end this send.
    let rig = Rig::with_timeout(NO_WINDOW, Duration::from_millis(150)).await;
    rig.enroll("phone").await;
    rig.relay.hang();
    rig.open(1);
    rig.push.handled(1).await;
    assert_eq!(rig.relay.hits(), 1);
    assert_eq!(rig.state("phone").status, Status::RelayError);
    assert_eq!(rig.state("phone").failures, 1);
}

#[tokio::test]
async fn one_slow_device_does_not_hold_up_the_state_of_the_others() {
    // Both devices share the relay; the first answer is a failure, the second
    // a success. Each registration keeps its own state.
    let rig = Rig::new(NO_WINDOW).await;
    rig.enroll("phone-a").await;
    rig.enroll("phone-b").await;
    rig.relay.reply(Reply::json(404, r#"{"error":"gone"}"#));
    rig.open(1);
    rig.push.handled(1).await;
    let states = [rig.state("phone-a").status, rig.state("phone-b").status];
    assert_eq!(states.iter().filter(|s| **s == Status::Expired).count(), 1);
    assert_eq!(states.iter().filter(|s| **s == Status::Ok).count(), 1);
}

#[tokio::test]
async fn a_late_outcome_never_overwrites_a_newer_registration() {
    let rig = Rig::new(NO_WINDOW).await;
    rig.enroll("phone").await;
    let gate = rig.relay.hold();
    rig.relay.set_default(Reply::json(404, r#"{"error":"gone"}"#));
    rig.open(1);
    rig.relay.wait_in_flight(1).await;
    // The device registers a new handle while the old wake is in flight.
    assert!(rig.register("phone", &handle_for("phone-new")).await.ok);
    gate.add_permits(1);
    rig.push.handled(1).await;
    assert_eq!(rig.status("phone").await.status, Some(PushDeviceStatus::Ok), "the old handle's 'gone' was applied");
    assert!(!rig.state("phone").disabled());
}

#[tokio::test]
async fn registrations_and_their_state_survive_a_restart() {
    let rig = Rig::new(NO_WINDOW).await;
    rig.enroll("phone").await;
    rig.relay.set_default(Reply::json(503, "{}"));
    rig.open(1);
    rig.push.handled(1).await;
    let Rig { dir, relay, .. } = rig;
    relay.set_default(Reply::status(204));
    let again = Rig::in_dir(dir, relay, NO_WINDOW, Duration::from_secs(5));
    let st = again.state("phone");
    assert_eq!((st.status, st.failures), (Status::RelayError, 1));
    assert!(again.status("phone").await.registered);
}

// ---- registration over acd/1 ----

#[tokio::test]
async fn registration_checks_the_relay_and_the_handle() {
    let rig = Rig::new(NO_WINDOW).await;
    let mut other = rig.register_msg(&handle_for("phone"));
    if let ClientMessage::PushRegister { relay, .. } = &mut other {
        *relay = "https://other.example".into();
    }
    let r = rig.core.handle("phone", other).await;
    assert_eq!((r.error_code(), r.error.unwrap().message.as_str()), (Some(ErrorCode::BadRequest), "relay mismatch"));
    for bad in ["", "has space", "plus+sign", "slash/sign", "pad=", "ünïcode"] {
        let r = rig.register("phone", bad).await;
        assert_eq!(r.error_code(), Some(ErrorCode::BadRequest), "handle {bad:?}");
    }
    assert!(!rig.status("phone").await.registered);
    assert!(!rig.dir.path().join(store::FILE).exists(), "a refused registration wrote nothing");
}

#[tokio::test]
async fn at_most_eight_devices_and_a_known_one_may_replace_itself() {
    let rig = Rig::new(NO_WINDOW).await;
    for n in 0..store::MAX_DEVICES {
        rig.enroll(&format!("phone-{n}")).await;
    }
    let r = rig.register("phone-new", &handle_for("phone-new")).await;
    assert_eq!((r.error_code(), r.error.unwrap().message.as_str()), (Some(ErrorCode::BadRequest), "too many push devices"));
    assert!(rig.register("phone-3", &handle_for("phone-3-again")).await.ok);
    assert_eq!(rig.push.store().len(), store::MAX_DEVICES);
}

#[tokio::test]
async fn a_session_sees_and_changes_only_its_own_registration() {
    let rig = Rig::new(NO_WINDOW).await;
    rig.enroll("phone-a").await;
    assert!(rig.status("phone-a").await.registered);
    assert!(!rig.status("phone-b").await.registered);
    // B unregistering does not touch A.
    let r = rig.core.handle("phone-b", ClientMessage::PushUnregister { id: rig.id() }).await;
    assert!(r.ok);
    assert!(rig.status("phone-a").await.registered);
    // A unregistering removes it; asking again is still ok.
    for _ in 0..2 {
        let r = rig.core.handle("phone-a", ClientMessage::PushUnregister { id: rig.id() }).await;
        assert!(r.ok);
    }
    assert!(!rig.status("phone-a").await.registered);
    // The result never carries a handle.
    let r = rig.core.handle("phone-a", ClientMessage::PushStatus { id: rig.id() }).await;
    assert!(!serde_json::to_string(&r).unwrap().contains(&handle_for("phone-a")), "status carries the handle");
}

// ---- storage ----

#[test]
fn the_store_is_owner_only_and_holds_handles_but_no_push_address() {
    use std::os::unix::fs::PermissionsExt;
    let dir = private_dir();
    let mut s = Store::load(dir.path());
    s.register("phone", Device::new(PushPlatform::Android, handle_for("phone"), "2026-12-04T10:00:00.000Z".into())).unwrap();
    let path = dir.path().join(store::FILE);
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777, 0o600);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains(&handle_for("phone")));
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    let mut keys: Vec<&str> = v["devices"]["phone"].as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(keys, ["expires_at", "failures", "handle", "last_wake_at", "platform", "retry_after", "status"]);
    // A reload gives the same devices.
    let again = Store::load(dir.path());
    assert!(again.degraded().is_none());
    assert_eq!(again.get("phone").unwrap().device, s.get("phone").unwrap().device);
}

#[test]
fn a_device_never_shows_its_handle_in_debug() {
    let d = Device::new(PushPlatform::Ios, handle_for("phone"), "2026-12-04T10:00:00.000Z".into());
    assert!(!format!("{d:?}").contains(&handle_for("phone")));
    let reg = PushRegistration {
        relay: "https://r.example".into(),
        handle: handle_for("phone"),
        platform: PushPlatform::Android,
        expires_at: "2026-12-04T10:00:00Z".parse().unwrap(),
    };
    assert!(!format!("{reg:?}").contains(&handle_for("phone")));
}

fn write_raw(dir: &std::path::Path, text: &str, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join(store::FILE);
    std::fs::write(&p, text).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn valid_file(handle: &str) -> String {
    format!(
        r#"{{"version":1,"devices":{{"phone":{{"platform":"android","handle":"{handle}","expires_at":"2026-12-04T10:00:00.000Z","status":"ok","failures":0,"retry_after":0,"last_wake_at":null}}}}}}"#
    )
}

#[test]
fn an_unsafe_or_invalid_file_starts_empty_with_a_reason_and_is_left_alone() {
    let secret = "SECRET-HANDLE-MUST-NOT-APPEAR";
    let cases: Vec<(&str, String, u32)> = vec![
        ("loose mode", valid_file(&handle_for("x")), 0o644),
        ("not json", format!("{{ {secret}"), 0o600),
        ("unknown version", valid_file(&handle_for("x")).replace("\"version\":1", "\"version\":2"), 0o600),
        ("unknown field", valid_file(&handle_for("x")).replace("\"failures\":0", &format!("\"failures\":0,\"{secret}\":1")), 0o600),
        ("bad handle", valid_file("has space"), 0o600),
        ("wrong type", valid_file(&handle_for("x")).replace("\"failures\":0", &format!("\"failures\":\"{secret}\"")), 0o600),
    ];
    for (name, text, mode) in cases {
        let dir = private_dir();
        write_raw(dir.path(), &text, mode);
        let mut s = Store::load(dir.path());
        let why = s.degraded().unwrap_or_else(|| panic!("{name}: not degraded")).to_string();
        assert!(!why.contains(secret) && !why.contains("SECRET"), "{name}: reason quotes the file");
        assert_eq!(s.len(), 0, "{name}");
        // Nothing rewrites it: not a state update, not an unregister.
        assert!(s.remove("phone").is_ok());
        assert!(!s.apply_wake("phone", 1, &WakeState::default()));
        assert_eq!(std::fs::read_to_string(dir.path().join(store::FILE)).unwrap(), text, "{name}: rewritten");
    }
}

#[test]
fn a_symlinked_file_is_not_followed() {
    let dir = private_dir();
    let real = dir.path().join("real.json");
    std::fs::write(&real, valid_file(&handle_for("x"))).unwrap();
    std::os::unix::fs::symlink(&real, dir.path().join(store::FILE)).unwrap();
    assert!(Store::load(dir.path()).degraded().is_some());
}

#[test]
fn too_many_devices_in_the_file_is_degraded() {
    let dir = private_dir();
    let dev = |n: usize| {
        format!(
            r#""p{n}":{{"platform":"ios","handle":"{}","expires_at":"x","status":"ok","failures":0,"retry_after":0,"last_wake_at":null}}"#,
            handle_for(&n.to_string())
        )
    };
    let all: Vec<String> = (0..=store::MAX_DEVICES).map(dev).collect();
    write_raw(dir.path(), &format!(r#"{{"version":1,"devices":{{{}}}}}"#, all.join(",")), 0o600);
    assert!(Store::load(dir.path()).degraded().is_some());
}

#[test]
fn the_next_valid_registration_replaces_a_degraded_file() {
    use std::os::unix::fs::PermissionsExt;
    let dir = private_dir();
    write_raw(dir.path(), "not json", 0o644);
    let mut s = Store::load(dir.path());
    assert!(s.degraded().is_some());
    s.register("phone", Device::new(PushPlatform::Android, handle_for("phone"), "x".into())).unwrap();
    assert!(s.degraded().is_none());
    let p = dir.path().join(store::FILE);
    assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o7777, 0o600);
    assert!(Store::load(dir.path()).get("phone").is_some());
}

#[test]
fn a_failed_write_changes_nothing_in_memory() {
    let dir = private_dir();
    let mut s = Store::load(dir.path());
    s.register("a", Device::new(PushPlatform::Android, handle_for("a"), "x".into())).unwrap();
    // The directory is gone: the write fails and the registration is refused.
    let path = dir.path().to_path_buf();
    drop(dir);
    assert!(!path.exists());
    assert_eq!(
        s.register("b", Device::new(PushPlatform::Android, handle_for("b"), "x".into())),
        Err(store::StoreError::Write)
    );
    assert!(s.get("b").is_none() && s.get("a").is_some());
}

#[test]
fn a_wake_outcome_for_an_old_generation_is_ignored() {
    let dir = private_dir();
    let mut s = Store::load(dir.path());
    s.register("p", Device::new(PushPlatform::Android, handle_for("old"), "x".into())).unwrap();
    let old = s.get("p").unwrap().generation;
    s.register("p", Device::new(PushPlatform::Android, handle_for("new"), "x".into())).unwrap();
    let gone = WakeState { status: Status::Expired, ..WakeState::default() };
    assert!(!s.apply_wake("p", old, &gone));
    assert_eq!(s.get("p").unwrap().device.status(), PushDeviceStatus::Ok);
    assert!(s.apply_wake("p", s.get("p").unwrap().generation, &gone));
    assert_eq!(s.get("p").unwrap().device.status(), PushDeviceStatus::Expired);
}

// ---- configuration ----

fn section(src: &str) -> toml::Value {
    toml::from_str::<toml::Value>(src).unwrap()["push"].clone()
}

fn ca_pem() -> String {
    let key = rcgen::KeyPair::generate().unwrap();
    let mut p = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    p.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    p.self_signed(&key).unwrap().pem()
}

fn off_reason(r: Resolved) -> Option<String> {
    match r {
        Resolved::Off(why) => why,
        Resolved::On { .. } => panic!("push resolved to on"),
    }
}

fn clock() -> Arc<dyn acd_relay::Clock> {
    Arc::new(ManualClock::new(T0))
}

#[test]
fn push_is_off_without_a_section_or_a_relay() {
    assert_eq!(off_reason(resolve(None, &[], clock())), None);
    let empty = section("[push]\n");
    assert_eq!(off_reason(resolve(Some(&empty), &[], clock())), None);
}

#[test]
fn a_good_relay_with_a_usable_ca_file_turns_push_on() {
    let d = tempfile::tempdir().unwrap();
    let ca = d.path().join("ca.pem");
    std::fs::write(&ca, ca_pem()).unwrap();
    let s = section(&format!("[push]\nrelay = \"https://relay.example.net/\"\nca_file = \"{}\"\n", ca.display()));
    match resolve(Some(&s), &[], clock()) {
        Resolved::On { relay, .. } => assert_eq!(relay.as_str(), "https://relay.example.net"),
        Resolved::Off(why) => panic!("off: {why:?}"),
    }
}

#[test]
fn without_ca_file_the_first_existing_default_is_used() {
    let d = tempfile::tempdir().unwrap();
    let (missing, present) = (d.path().join("missing.pem"), d.path().join("bundle.pem"));
    std::fs::write(&present, ca_pem()).unwrap();
    let s = section("[push]\nrelay = \"https://relay.example.net\"\n");
    let defaults = [missing.to_str().unwrap(), present.to_str().unwrap()];
    assert!(matches!(resolve(Some(&s), &defaults, clock()), Resolved::On { .. }));
    // None of them exists: off, with a reason, and no silent fallback.
    let why = off_reason(resolve(Some(&s), &[missing.to_str().unwrap()], clock()));
    assert!(why.unwrap().contains("ca_file"));
}

#[test]
fn every_bad_push_section_fails_closed_with_a_reason() {
    let d = tempfile::tempdir().unwrap();
    let ca = d.path().join("ca.pem");
    std::fs::write(&ca, ca_pem()).unwrap();
    let empty = d.path().join("empty.pem");
    std::fs::write(&empty, "no certificates here").unwrap();
    let c = ca.display();
    let cases = [
        format!("[push]\nrelay = \"http://relay.example.net\"\nca_file = \"{c}\"\n"),
        format!("[push]\nrelay = \"https://u:p@relay.example.net\"\nca_file = \"{c}\"\n"),
        format!("[push]\nrelay = \"https://relay.example.net/?x=1\"\nca_file = \"{c}\"\n"),
        format!("[push]\nrelay = 5\nca_file = \"{c}\"\n"),
        format!("[push]\nrelay = \"https://relay.example.net\"\nca_flie = \"{c}\"\n"),
        format!("[push]\nrelay = \"https://relay.example.net\"\nrelay_pin = \"sha256:00\"\nca_file = \"{c}\"\n"),
        "[push]\nrelay = \"https://relay.example.net\"\nca_file = 7\n".to_string(),
        "[push]\nrelay = \"https://relay.example.net\"\nca_file = \"\"\n".to_string(),
        format!("[push]\nca_file = \"{c}\"\n"),
        "[push]\nrelay = \"https://relay.example.net\"\nca_file = \"/nonexistent/ca.pem\"\n".to_string(),
        format!("[push]\nrelay = \"https://relay.example.net\"\nca_file = \"{}\"\n", empty.display()),
    ];
    for src in cases {
        let s = section(&src);
        assert!(off_reason(resolve(Some(&s), &[], clock())).is_some(), "no reason for: {src}");
    }
    // Not a table at all.
    assert!(off_reason(resolve(Some(&toml::Value::Integer(5)), &[], clock())).is_some());
}
