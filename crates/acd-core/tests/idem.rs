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

#[test]
fn pending_entries_survive_ttl_pruning() {
    let mut c = IdemCache::new(Duration::from_secs(900));
    let t = Instant::now();
    assert!(matches!(c.begin("cl", "1", H1, t), Begin::Run));
    assert!(matches!(c.begin("cl", "1", H1, t + Duration::from_secs(901)), Begin::Wait(_)));
}

#[test]
fn full_table_rejects_new_reservations_until_entries_expire() {
    let mut c = IdemCache::with_capacity(Duration::from_secs(900), 2);
    let t = Instant::now();
    assert!(matches!(c.begin("cl", "1", H1, t), Begin::Run));
    c.finish("cl", "1", CommandResult::ok("1"), true, t);
    assert!(matches!(c.begin("cl", "2", H1, t), Begin::Run)); // still pending
    assert!(matches!(c.begin("cl", "3", H1, t), Begin::Full));
    assert!(matches!(c.begin("other", "3", H1, t), Begin::Full));
    // Existing entries still answer while the table is full.
    assert!(matches!(c.begin("cl", "1", H1, t), Begin::Done(_)));
    assert!(matches!(c.begin("cl", "2", H1, t), Begin::Wait(_)));
    assert!(matches!(c.begin("cl", "3", H1, t + Duration::from_secs(901)), Begin::Run), "expired entry freed a slot");
}

fn limits() -> IdemLimits {
    IdemLimits { capacity: 100, network: 100, local: 100, per_client: 100, per_source: 100, client_ids_per_source: usize::MAX }
}

fn net(n: u8) -> Origin {
    Origin::net(std::net::IpAddr::from([192, 0, 2, n]))
}

fn deliver(c: &mut IdemCache, cl: &str, o: Origin, id: &str, t: Instant) -> bool {
    match c.begin_from(cl, o, id, H1, t) {
        Begin::Run => {
            c.finish(cl, id, CommandResult::ok(id), true, t);
            true
        }
        Begin::Full => false,
        _ => panic!("unexpected replay"),
    }
}

/// A client at its share is refused; it never loses a delivered entry to make
/// room, so the replay and the mismatch check keep working.
#[test]
fn a_client_at_its_share_is_refused_and_keeps_every_delivered_entry() {
    let mut c = IdemCache::with_limits(Duration::from_secs(900), IdemLimits { per_client: 2, ..limits() });
    let t = Instant::now();
    assert!(deliver(&mut c, "a", net(1), "1", t));
    assert!(deliver(&mut c, "a", net(1), "2", t));
    assert!(!deliver(&mut c, "a", net(1), "3", t), "refused, not evicting");
    assert!(matches!(c.begin_from("a", net(1), "1", H1, t), Begin::Done(_)), "oldest still replays");
    assert!(matches!(c.begin_from("a", net(1), "1", [9; 32], t), Begin::Mismatch));
    assert!(deliver(&mut c, "b", net(2), "1", t), "another client is untouched");
}

#[test]
fn a_client_whose_entries_are_all_pending_is_refused_not_others() {
    let mut c = IdemCache::with_limits(Duration::from_secs(900), IdemLimits { per_client: 2, ..limits() });
    let t = Instant::now();
    assert!(matches!(c.begin("a", "1", H1, t), Begin::Run));
    assert!(matches!(c.begin("a", "2", H1, t), Begin::Run));
    assert!(matches!(c.begin("a", "3", H1, t), Begin::Full));
    assert!(matches!(c.begin("b", "1", H1, t), Begin::Run));
}

/// The peer picks `client.id`, so a source changing ids must not get more room.
#[test]
fn one_source_is_held_to_its_quota_whatever_client_ids_it_uses() {
    let mut c = IdemCache::with_limits(Duration::from_secs(900), IdemLimits { per_source: 3, ..limits() });
    let t = Instant::now();
    for i in 0..3 {
        assert!(deliver(&mut c, &format!("id{i}"), net(1), "x", t));
    }
    assert!(!deliver(&mut c, "id-fresh", net(1), "x", t), "a new id does not reset the source's quota");
    assert!(deliver(&mut c, "other", net(2), "x", t), "another source still gets in");
}

#[test]
fn one_source_may_hold_only_so_many_client_ids() {
    let mut c = IdemCache::with_limits(Duration::from_secs(900), IdemLimits { client_ids_per_source: 2, ..limits() });
    let t = Instant::now();
    assert!(deliver(&mut c, "c1", net(1), "1", t));
    assert!(deliver(&mut c, "c2", net(1), "1", t));
    assert!(!deliver(&mut c, "c3", net(1), "1", t), "a third id from one source is refused");
    assert!(deliver(&mut c, "c1", net(1), "2", t), "an id it already uses still works");
    assert!(deliver(&mut c, "c3", net(2), "1", t), "another source has its own ids");
}

#[test]
fn network_sources_cannot_take_the_local_quota() {
    let mut c = IdemCache::with_limits(Duration::from_secs(900), IdemLimits { capacity: 6, network: 4, local: 2, ..limits() });
    let t = Instant::now();
    for (i, src) in [1u8, 2, 3, 4, 5].into_iter().enumerate() {
        assert_eq!(deliver(&mut c, "n", net(src), &i.to_string(), t), i < 4, "network stops at its 4");
    }
    assert!(deliver(&mut c, "l", Origin::Local, "1", t));
    assert!(deliver(&mut c, "l", Origin::Local, "2", t));
    assert!(!deliver(&mut c, "l", Origin::Local, "3", t), "local has a fixed quota too");
}
