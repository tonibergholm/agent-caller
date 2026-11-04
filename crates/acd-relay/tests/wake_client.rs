//! The wake sender against a fake relay (spec §5.4).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use acd_relay::ManualClock;
use acd_relay::client::*;
use acd_relay::http::HttpClient;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::routing::post;

const T0: u64 = 1_800_000_000;

/// status, body, optional Location header
type Reply = (u16, Vec<u8>, Option<String>);

struct Fake {
    hits: AtomicUsize,
    bodies: Mutex<Vec<String>>,
    replies: Mutex<VecDeque<Reply>>,
    default: Mutex<(u16, Vec<u8>)>,
}

struct Rig {
    url: RelayUrl,
    fake: Arc<Fake>,
    clock: Arc<ManualClock>,
    sender: WakeSender,
}

async fn rig() -> Rig {
    let fake = Arc::new(Fake {
        hits: AtomicUsize::new(0),
        bodies: Mutex::new(vec![]),
        replies: Mutex::new(VecDeque::new()),
        default: Mutex::new((204, vec![])),
    });
    async fn wake(State(f): State<Arc<Fake>>, body: String) -> (StatusCode, HeaderMap, Vec<u8>) {
        f.hits.fetch_add(1, Ordering::SeqCst);
        f.bodies.lock().unwrap().push(body);
        let (s, b, loc) = f.replies.lock().unwrap().pop_front().unwrap_or_else(|| {
            let d = f.default.lock().unwrap().clone();
            (d.0, d.1, None)
        });
        let mut h = HeaderMap::new();
        if let Some(l) = loc {
            h.insert(header::LOCATION, l.parse().unwrap());
        }
        (StatusCode::from_u16(s).unwrap(), h, b)
    }
    let app = axum::Router::new().route("/v1/wake", post(wake)).with_state(fake.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = RelayUrl::for_tests(&format!("http://{}", l.local_addr().unwrap()));
    tokio::spawn(async move { axum::serve(l, app).await });
    let clock = Arc::new(ManualClock::new(T0));
    let sender = WakeSender::new(
        HttpClient::plain_for_tests(Duration::from_secs(5), MAX_RESPONSE),
        url.clone(),
        clock.clone(),
    );
    Rig { url, fake, clock, sender }
}

impl Rig {
    fn reply(&self, status: u16, body: &str) {
        self.fake.replies.lock().unwrap().push_back((status, body.as_bytes().to_vec(), None));
    }
    fn set_default(&self, status: u16, body: &str) {
        *self.fake.default.lock().unwrap() = (status, body.as_bytes().to_vec());
    }
    fn hits(&self) -> usize {
        self.fake.hits.load(Ordering::SeqCst)
    }
}

#[tokio::test]
async fn success_marks_ok() {
    let r = rig().await;
    let mut st = WakeState { status: Status::RelayError, failures: 3, retry_after: 0, last_wake_at: None };
    assert_eq!(r.sender.wake("HANDLE", &mut st).await, Outcome::Sent);
    assert_eq!(st, WakeState { status: Status::Ok, failures: 0, retry_after: 0, last_wake_at: Some(T0) });
    // The request body is exactly {v, handle}.
    assert_eq!(r.fake.bodies.lock().unwrap()[0], r#"{"v":1,"handle":"HANDLE"}"#);
}

#[tokio::test]
async fn gone_disables_until_reregistered() {
    let r = rig().await;
    let mut st = WakeState::default();
    r.reply(404, r#"{"error":"gone"}"#);
    assert_eq!(r.sender.wake("H", &mut st).await, Outcome::Invalidated);
    assert_eq!(st.status, Status::Expired);
    assert!(st.disabled());
    let hits = r.hits();
    for _ in 0..3 {
        r.clock.advance(10_000);
        assert_eq!(r.sender.wake("H", &mut st).await, Outcome::Disabled);
    }
    assert_eq!(r.hits(), hits, "a disabled registration sends nothing");
    st.reregistered();
    assert_eq!(r.sender.wake("H2", &mut st).await, Outcome::Sent);
    assert_eq!(st.status, Status::Ok);
}

#[tokio::test]
async fn invalid_handle_disables() {
    let r = rig().await;
    let mut st = WakeState::default();
    r.reply(400, r#"{"error":"invalid_handle"}"#);
    assert_eq!(r.sender.wake("H", &mut st).await, Outcome::Invalidated);
    assert_eq!(st.status, Status::Invalid);
    assert_eq!(r.sender.wake("H", &mut st).await, Outcome::Disabled);
}

#[tokio::test]
async fn rate_limited_drops_without_state_change() {
    let r = rig().await;
    let mut st = WakeState { last_wake_at: Some(5), ..WakeState::default() };
    let before = st.clone();
    r.reply(429, r#"{"error":"rate_limited"}"#);
    assert_eq!(r.sender.wake("H", &mut st).await, Outcome::RateLimited);
    assert_eq!(st, before);
    assert_eq!(r.hits(), 1, "no retry");
}

#[tokio::test]
async fn transient_outage_then_recovery() {
    let r = rig().await;
    let mut st = WakeState::default();
    r.set_default(503, r#"{"error":"unavailable"}"#);
    assert_eq!(r.sender.wake("H", &mut st).await, Outcome::Failed);
    assert_eq!(st.status, Status::RelayError);
    assert!(!st.disabled());
    // Inside the window: skipped, nothing sent.
    let hits = r.hits();
    assert_eq!(r.sender.wake("H", &mut st).await, Outcome::Skipped);
    assert_eq!(r.hits(), hits);
    // The relay comes back; the first wake after the window succeeds with
    // the same handle (no re-registration).
    r.set_default(204, "");
    r.clock.advance(11);
    assert_eq!(r.sender.wake("H", &mut st).await, Outcome::Sent);
    assert_eq!(st.status, Status::Ok);
    assert_eq!(st.failures, 0);
}

#[tokio::test]
async fn backoff_doubles_and_caps() {
    assert_eq!(
        (1..=9).map(backoff_secs).collect::<Vec<_>>(),
        [10, 20, 40, 80, 160, 320, 640, 900, 900]
    );
    assert_eq!(backoff_secs(u32::MAX), 900);
    let r = rig().await;
    r.set_default(500, "");
    let mut st = WakeState::default();
    let mut windows = vec![];
    for _ in 0..9 {
        let t = acd_relay::Clock::now_secs(&*r.clock);
        assert_eq!(r.sender.wake("H", &mut st).await, Outcome::Failed);
        windows.push(st.retry_after - t);
        r.clock.advance(st.retry_after - t);
    }
    assert_eq!(windows, [10, 20, 40, 80, 160, 320, 640, 900, 900]);
    // The 10th failure (window 900 s); then a wake one second early is skipped.
    let hits = r.hits();
    assert_eq!(r.sender.wake("H", &mut st).await, Outcome::Failed);
    assert_eq!(r.hits(), hits + 1);
    r.clock.advance(899);
    assert_eq!(r.sender.wake("H", &mut st).await, Outcome::Skipped);
    assert_eq!(r.hits(), hits + 1);
}

#[tokio::test]
async fn outage_never_disables_but_invalid_registration_does() {
    let r = rig().await;
    r.set_default(503, "");
    let mut st = WakeState::default();
    for _ in 0..40 {
        r.clock.advance(1000);
        assert_eq!(r.sender.wake("H", &mut st).await, Outcome::Failed);
        assert!(!st.disabled());
    }
    // Connection errors and timeouts are the same class.
    let dead = {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let a = l.local_addr().unwrap();
        drop(l);
        a
    };
    let s2 = WakeSender::new(
        HttpClient::plain_for_tests(Duration::from_secs(2), MAX_RESPONSE),
        RelayUrl::for_tests(&format!("http://{dead}")),
        r.clock.clone(),
    );
    r.clock.advance(1000);
    assert_eq!(s2.wake("H", &mut st).await, Outcome::Failed);
    assert!(!st.disabled());
    // The relay recovers, then says the registration is invalid.
    r.set_default(404, r#"{"error":"gone"}"#);
    r.clock.advance(1000);
    assert_eq!(r.sender.wake("H", &mut st).await, Outcome::Invalidated);
    assert!(st.disabled());
}

#[tokio::test]
async fn hostile_bodies_change_nothing_but_the_status() {
    let r = rig().await;
    let huge = "x".repeat(5 * 1024 * 1024);
    let cases: Vec<(u16, Vec<u8>, Outcome)> = vec![
        (204, huge.clone().into_bytes(), Outcome::Sent),
        (204, vec![0xff, 0xfe, 0x00, 0xc3, 0x28], Outcome::Sent),
        (204, br#"{"answer":"allow","approve":true}"#.to_vec(), Outcome::Sent),
        (200, br#"{"answer":"allow","approve":true}"#.to_vec(), Outcome::Failed),
        (500, huge.into_bytes(), Outcome::Failed),
        (500, vec![0xff, 0xfe, 0xff], Outcome::Failed),
        (400, br#"{"answer":"allow","approve":true}"#.to_vec(), Outcome::Failed),
        (400, vec![0xff; 100], Outcome::Failed),
        (404, br#"{"approve":true,"error":"nope"}"#.to_vec(), Outcome::Failed),
        (429, br#"{"approve":true}"#.to_vec(), Outcome::RateLimited),
        (302, vec![], Outcome::Failed),
    ];
    for (status, body, want) in cases {
        let mut st = WakeState::default();
        r.fake.replies.lock().unwrap().push_back((status, body, None));
        assert_eq!(r.sender.wake("H", &mut st).await, want, "status {status}");
        // Only the declared fields can change, and never into a disabled state.
        assert!(!st.disabled(), "status {status}");
        r.clock.advance(1000);
    }
    // Hostile bodies that do carry the right error code only do what that
    // code means, whatever else the JSON holds.
    let mut st = WakeState::default();
    r.reply(404, r#"{"error":"gone","answer":"allow","approve":true}"#);
    assert_eq!(r.sender.wake("H", &mut st).await, Outcome::Invalidated);
    assert_eq!(st.status, Status::Expired);
}

#[tokio::test]
async fn redirects_are_not_followed() {
    let r = rig().await;
    let other = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let other_addr = other.local_addr().unwrap();
    let conns = Arc::new(AtomicUsize::new(0));
    let c2 = conns.clone();
    tokio::spawn(async move {
        loop {
            if other.accept().await.is_ok() {
                c2.fetch_add(1, Ordering::SeqCst);
            }
        }
    });
    r.fake.replies.lock().unwrap().push_back((307, vec![], Some(format!("http://{other_addr}/v1/wake"))));
    let mut st = WakeState::default();
    assert_eq!(r.sender.wake("H", &mut st).await, Outcome::Failed);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(conns.load(Ordering::SeqCst), 0, "the redirect target was contacted");
    assert_eq!(r.hits(), 1);
}

#[tokio::test]
async fn disabled_config_opens_no_connection() {
    let r = rig().await;
    let url = r.url.as_str().to_string();
    let parse = |s: &str| -> toml::Value { toml::from_str::<toml::Value>(s).unwrap()["push"].clone() };
    let bad = [
        None,
        Some(parse(&format!("[push]\nrelai = \"{url}\"\n"))),
        Some(parse(&format!("[push]\nrelay = \"{url}\"\n"))), // http://
        Some(parse(&format!("[push]\nrelay = \"{url}\"\nrelay_pin = \"sha256:aa\"\n"))),
        Some(parse("[push]\nrelay = 5\n")),
        Some(toml::Value::String("https://relay.example".into())),
        Some(parse("[push]\n")),
        Some(parse("[push]\nrelay = \"https://user:pw@relay.example\"\n")),
        Some(parse("[push]\nrelay = \"https://relay.example/?k=v\"\n")),
    ];
    for v in &bad {
        let setting = resolve(v.as_ref());
        assert!(matches!(setting, PushSetting::Off(_)), "{v:?} -> {setting:?}");
        // Off means there is no sender to call: nothing can reach the relay.
    }
    assert_eq!(r.hits(), 0);
    assert!(matches!(resolve(Some(&parse("[push]\nrelay = \"https://relay.example\"\n"))), PushSetting::On(_)));
}
