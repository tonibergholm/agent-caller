//! The push seam in the engine (relay spec §5.1): the `push` capability,
//! `push.*` routing and the opened-request signal.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use acd_core::testing::*;
use acd_core::*;
use acd_protocol::*;
use async_trait::async_trait;

#[derive(Default)]
struct Spy {
    opened: AtomicUsize,
    calls: Mutex<Vec<String>>,
}

#[async_trait]
impl PushControl for Spy {
    fn request_opened(&self) {
        self.opened.fetch_add(1, Ordering::SeqCst);
    }
    async fn register(&self, client_id: &str, reg: PushRegistration) -> Result<(), PushFailure> {
        self.calls.lock().unwrap().push(format!("register {client_id} {}", reg.relay));
        if reg.relay == "https://other.example" {
            return Err(PushFailure { code: ErrorCode::BadRequest, message: "relay mismatch" });
        }
        Ok(())
    }
    async fn unregister(&self, client_id: &str) -> Result<(), PushFailure> {
        self.calls.lock().unwrap().push(format!("unregister {client_id}"));
        Ok(())
    }
    async fn status(&self, client_id: &str) -> PushInfo {
        self.calls.lock().unwrap().push(format!("status {client_id}"));
        PushInfo { registered: true, status: Some(PushDeviceStatus::Ok), last_wake_at: None }
    }
}

fn register(id: &str, relay: &str) -> ClientMessage {
    ClientMessage::PushRegister {
        id: id.into(),
        relay: relay.into(),
        handle: "AQIDBA".into(),
        platform: PushPlatform::Android,
        expires_at: "2026-12-04T10:00:00Z".parse().unwrap(),
    }
}

fn capabilities(core: &Core) -> Vec<String> {
    let ServerMessage::Welcome { capabilities, .. } = core.welcome() else { panic!("not a welcome") };
    capabilities
}

#[tokio::test]
async fn without_push_the_capability_is_absent_and_the_commands_are_not_supported() {
    let core = Core::new(test_machine(), vec![]);
    assert_eq!(capabilities(&core), ["answer", "send", "mark_seen", "answer.revision"]);
    for msg in [
        register("c1", "https://relay.example"),
        ClientMessage::PushUnregister { id: "c2".into() },
        ClientMessage::PushStatus { id: "c3".into() },
    ] {
        let r = core.handle("cl", msg).await;
        assert_eq!(r.error_code(), Some(ErrorCode::NotSupported), "{r:?}");
    }
}

#[tokio::test]
async fn with_push_the_capability_is_advertised_and_commands_reach_the_handler() {
    let core = Core::new(test_machine(), vec![]);
    let spy = std::sync::Arc::new(Spy::default());
    assert!(core.set_push(spy.clone()));
    assert!(!core.set_push(spy.clone()), "only the first call counts");
    assert_eq!(capabilities(&core), ["answer", "send", "mark_seen", "answer.revision", "push"]);

    assert_eq!(core.handle("phone", register("c1", "https://relay.example")).await, CommandResult::ok("c1"));
    let r = core.handle("phone", register("c2", "https://other.example")).await;
    assert_eq!(r.error_code(), Some(ErrorCode::BadRequest));
    assert_eq!(r.error.unwrap().message, "relay mismatch");
    assert_eq!(core.handle("phone", ClientMessage::PushUnregister { id: "c3".into() }).await, CommandResult::ok("c3"));
    let r = core.handle("phone", ClientMessage::PushStatus { id: "c4".into() }).await;
    assert_eq!(r.push.unwrap().status, Some(PushDeviceStatus::Ok));
    // The handler is told the session's client id and nothing else.
    assert_eq!(
        *spy.calls.lock().unwrap(),
        ["register phone https://relay.example", "register phone https://other.example", "unregister phone", "status phone"]
    );
}

#[tokio::test]
async fn a_retried_push_register_is_answered_from_the_idempotency_cache() {
    let core = Core::new(test_machine(), vec![]);
    let spy = std::sync::Arc::new(Spy::default());
    core.set_push(spy.clone());
    let first = core.handle("phone", register("c1", "https://relay.example")).await;
    let again = core.handle("phone", register("c1", "https://relay.example")).await;
    assert_eq!(first, again);
    assert_eq!(spy.calls.lock().unwrap().len(), 1, "the retry did not reach the handler");
    // The same id with another payload is refused, not re-run.
    let r = core.handle("phone", register("c1", "https://other.example")).await;
    assert_eq!(r.error_code(), Some(ErrorCode::DuplicateMismatch));
}

#[tokio::test]
async fn oversized_push_fields_are_refused_before_the_handler() {
    let core = Core::new(test_machine(), vec![]);
    let spy = std::sync::Arc::new(Spy::default());
    core.set_push(spy.clone());
    let big_handle = ClientMessage::PushRegister {
        id: "c1".into(),
        relay: "https://relay.example".into(),
        handle: "A".repeat(513),
        platform: PushPlatform::Android,
        expires_at: "2026-12-04T10:00:00Z".parse().unwrap(),
    };
    let r = core.handle("phone", big_handle).await;
    assert_eq!(r.error_code(), Some(ErrorCode::BadRequest));
    let r = core.handle("phone", register("c2", &format!("https://{}", "r".repeat(2048)))).await;
    assert_eq!(r.error_code(), Some(ErrorCode::BadRequest));
    assert!(spy.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_handler_hears_of_each_opened_request_and_of_nothing_else() {
    let core = Core::new(test_machine(), vec![]);
    let spy = std::sync::Arc::new(Spy::default());
    core.set_push(spy.clone());
    core.observe(Observation::AgentUpsert(agent_obs("a1")));
    assert_eq!(spy.opened.load(Ordering::SeqCst), 0, "an agent is not a request");
    core.observe(Observation::RequestSeen(permission_obs("a1", "k1")));
    assert_eq!(spy.opened.load(Ordering::SeqCst), 1);
    // The same dialog seen again updates in place: no new request.
    core.observe(Observation::RequestSeen(permission_obs("a1", "k1")));
    assert_eq!(spy.opened.load(Ordering::SeqCst), 1);
    core.observe(Observation::RequestEnded {
        agent_id: "a1".into(),
        key: CorrelationKey("k1".into()),
        by: ResolvedBy::Inbox,
    });
    assert_eq!(spy.opened.load(Ordering::SeqCst), 1, "a resolution is not an opening");
    core.observe(Observation::RequestSeen(permission_obs("a1", "k2")));
    assert_eq!(spy.opened.load(Ordering::SeqCst), 2);
}
