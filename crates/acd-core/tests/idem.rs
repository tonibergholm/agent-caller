use std::time::{Duration, Instant};

use acd_core::*;
use acd_protocol::*;

const H1: [u8; 32] = [1; 32];
const H2: [u8; 32] = [2; 32];

#[test]
fn first_begin_runs_then_result_is_cached() {
    let mut c = IdemCache::new(Duration::from_secs(900));
    let t = Instant::now();
    assert!(matches!(c.begin("cl", "1", H1, t), Begin::Run));
    c.finish("cl", "1", CommandResult::delivered("1"), true, t);
    assert!(matches!(c.begin("cl", "1", H1, t), Begin::Done(r) if r == CommandResult::delivered("1")));
}

#[test]
fn ids_are_scoped_per_client_and_payload_mismatch_is_reported() {
    let mut c = IdemCache::new(Duration::from_secs(900));
    let t = Instant::now();
    c.begin("a", "1", H1, t);
    assert!(matches!(c.begin("b", "1", H2, t), Begin::Run));
    assert!(matches!(c.begin("a", "1", H2, t), Begin::Mismatch));
}

#[tokio::test]
async fn concurrent_retry_waits_for_the_running_command() {
    let mut c = IdemCache::new(Duration::from_secs(900));
    let t = Instant::now();
    c.begin("cl", "1", H1, t);
    let Begin::Wait(mut rx) = c.begin("cl", "1", H1, t) else { panic!("expected Wait") };
    c.finish("cl", "1", CommandResult::delivered("1"), true, t);
    rx.changed().await.unwrap();
    assert_eq!(rx.borrow().clone(), Some(CommandResult::delivered("1")));
}

#[test]
fn non_cacheable_results_release_the_id() {
    let mut c = IdemCache::new(Duration::from_secs(900));
    let t = Instant::now();
    c.begin("cl", "1", H1, t);
    c.finish("cl", "1", CommandResult::err("1", ErrorCode::RequestNotReady, "x"), false, t);
    assert!(matches!(c.begin("cl", "1", H1, t), Begin::Run));
}

#[test]
fn cached_results_expire_after_ttl() {
    let mut c = IdemCache::new(Duration::from_secs(900));
    let t = Instant::now();
    c.begin("cl", "1", H1, t);
    c.finish("cl", "1", CommandResult::ok("1"), true, t);
    assert!(matches!(c.begin("cl", "1", H1, t + Duration::from_secs(901)), Begin::Run));
}
