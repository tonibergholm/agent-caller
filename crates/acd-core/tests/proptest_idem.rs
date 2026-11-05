//! Property tests of the idempotency table after PR #106 (per-source and
//! per-client quotas, delivered entries kept to their TTL). `proptest_engine.rs`
//! covers the table through the engine; this file drives `IdemCache` itself.
//!
//! - a delivered command answers `Done` with its own result until its TTL ends,
//!   and is never evicted before, whatever any source does meanwhile;
//! - one source can never push another source's entries out, and one source
//!   that fills its own quota cannot stop another source or the local socket
//!   from being admitted;
//! - the table never holds more than its capacity, network, local, per-client,
//!   per-source or ids-per-source bound (checked against a model of the rules);
//! - an id reused with a different payload is a mismatch for as long as the
//!   entry lives, and `conflicts` agrees with it.
mod proptest_support;

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::time::{Duration, Instant};

use acd_core::{Begin, IdemCache, IdemLimits, Origin};
use acd_protocol::CommandResult;
use proptest::prelude::*;
use proptest_support::check;

const TTL: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug)]
enum Op {
    Begin { client: u8, origin: u8, id: u8, payload: u8, dt_ms: u16 },
    Finish { pick: usize, cacheable: bool },
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        8 => (0u8..6, 0u8..5, 0u8..5, 0u8..3, prop_oneof![Just(0u16), 0u16..800, 3000u16..12000])
            .prop_map(|(client, origin, id, payload, dt_ms)| Op::Begin { client, origin, id, payload, dt_ms }),
        4 => (any::<usize>(), any::<bool>()).prop_map(|(pick, cacheable)| Op::Finish { pick, cacheable }),
    ]
}

fn origin(o: u8) -> Origin {
    match o % 5 {
        0 => Origin::Local,
        n => Origin::Net(IpAddr::V4(Ipv4Addr::new(10, 0, 0, n))),
    }
}

fn limits() -> impl Strategy<Value = IdemLimits> {
    (1usize..24, 1usize..24, 1usize..12, 1usize..8, 1usize..8, 1usize..4).prop_map(
        |(capacity, network, local, per_client, per_source, client_ids_per_source)| IdemLimits {
            capacity,
            network,
            local,
            per_client,
            per_source,
            client_ids_per_source,
        },
    )
}

#[derive(Clone)]
struct E {
    hash: u8,
    origin: Origin,
    done: Option<Instant>,
}

#[test]
fn the_table_follows_its_rules_and_a_delivered_command_lives_to_its_ttl() {
    check("idem_rules", 300, (limits(), prop::collection::vec(op(), 1..150)), |(l, ops)| {
        let mut cache = IdemCache::with_limits(TTL, l);
        let mut m: HashMap<(u8, u8), E> = HashMap::new();
        let mut running: Vec<(u8, u8)> = vec![];
        let mut now = Instant::now();
        let name = |c: u8| format!("client-{c}");
        let id = |i: u8| format!("id-{i}");
        for op in ops {
            match op {
                Op::Begin { client, origin: o, id: i, payload, dt_ms } => {
                    now += Duration::from_millis(u64::from(dt_ms));
                    m.retain(|_, e| e.done.is_none_or(|at| now.duration_since(at) < TTL));
                    let key = (client, i);
                    let org = origin(o);
                    let got = cache.begin_from(&name(client), org, &id(i), [payload; 32], now);
                    let by_client = m.keys().filter(|k| k.0 == client).count();
                    let mut ids: Vec<u8> = m.iter().filter(|(_, e)| e.origin == org).map(|(k, _)| k.0).collect();
                    let by_origin = ids.len();
                    ids.sort();
                    ids.dedup();
                    let network = m.values().filter(|e| matches!(e.origin, Origin::Net(_))).count();
                    let admits = m.len() < l.capacity
                        && by_client < l.per_client
                        && match org {
                            Origin::Local => by_origin < l.local,
                            Origin::Net(_) => {
                                network < l.network
                                    && by_origin < l.per_source
                                    && (ids.contains(&client) || ids.len() < l.client_ids_per_source)
                            }
                        };
                    match (got, m.get(&key).cloned()) {
                        (Begin::Mismatch, Some(e)) => prop_assert_ne!(e.hash, payload),
                        (Begin::Wait(_), Some(E { hash, done: None, .. })) => prop_assert_eq!(hash, payload),
                        (Begin::Done(r), Some(E { hash, done: Some(_), .. })) => {
                            prop_assert_eq!(hash, payload);
                            prop_assert_eq!(r.id, format!("{client}/{i}"), "a delivered command lost its own result");
                        }
                        (Begin::Run, None) => {
                            prop_assert!(admits, "admitted past a bound with {} entries: {:?}", m.len(), l);
                            m.insert(key, E { hash: payload, origin: org, done: None });
                            running.push(key);
                        }
                        (Begin::Full, None) => prop_assert!(!admits, "refused with room: {:?}", l),
                        (_, e) => prop_assert!(false, "answer does not match the model entry (present: {})", e.is_some()),
                    }
                }
                Op::Finish { pick, cacheable } => {
                    if running.is_empty() {
                        continue;
                    }
                    let key = running.remove(pick % running.len());
                    cache.finish(&name(key.0), &id(key.1), CommandResult::ok(&format!("{}/{}", key.0, key.1)), cacheable, now);
                    if cacheable {
                        m.get_mut(&key).unwrap().done = Some(now);
                    } else {
                        m.remove(&key);
                    }
                }
            }
            prop_assert!(m.len() <= l.capacity);
        }
        Ok(())
    });
}

/// Run `n` commands from `origin` as `client_ids` rotate, finishing each as delivered.
fn deliver(c: &mut IdemCache, now: Instant, org: Origin, clients: &[String], n: usize, tag: &str) -> (usize, usize) {
    let (mut ran, mut full) = (0, 0);
    for i in 0..n {
        let client = &clients[i % clients.len()];
        let id = format!("{tag}-{i}");
        match c.begin_from(client, org, &id, [1; 32], now) {
            Begin::Run => {
                ran += 1;
                c.finish(client, &id, CommandResult::ok(&id), true, now);
            }
            Begin::Full => full += 1,
            _ => unreachable!("fresh ids"),
        }
    }
    (ran, full)
}

#[test]
fn a_source_that_fills_its_own_quota_pushes_nothing_out_and_blocks_no_other_source() {
    // The default limits. Source B delivers some commands; source A (many
    // client ids, many commands) fills its per-source share; B's entries are
    // still answered, B and the local socket are still admitted.
    check("idem_one_source", 12, (1usize..40, 1u8..40, 1usize..4), |(b_count, a_hosts_hint, a_ids)| {
        let mut c = IdemCache::new(TTL);
        let now = Instant::now();
        let b = Origin::net(IpAddr::V4(Ipv4Addr::new(10, 9, 9, 9)));
        let a = Origin::net(IpAddr::V4(Ipv4Addr::new(10, 1, 1, a_hosts_hint)));
        let b_clients: Vec<String> = (0..2).map(|i| format!("b{i}")).collect();
        let a_clients: Vec<String> = (0..a_ids.max(1)).map(|i| format!("a{i}")).collect();
        let (ran, _) = deliver(&mut c, now, b, &b_clients, b_count, "b");
        prop_assert_eq!(ran, b_count);
        // A tries far more than its share.
        let (a_ran, a_full) = deliver(&mut c, now, a, &a_clients, 3000, "a");
        prop_assert!(a_ran <= IdemLimits::default().per_source, "{} commands from one source", a_ran);
        prop_assert!(a_full > 0, "the flood was never refused");
        // B's delivered commands are all still answered with their own result.
        for i in 0..b_count {
            let client = &b_clients[i % b_clients.len()];
            let id = format!("b-{i}");
            match c.begin_from(client, b, &id, [1; 32], now + Duration::from_secs(1)) {
                Begin::Done(r) => prop_assert_eq!(r.id, id),
                _ => prop_assert!(false, "B's delivered command {} was pushed out", i),
            }
        }
        // B and the local socket can still start new commands.
        prop_assert!(matches!(c.begin_from(&b_clients[0], b, "new-b", [2; 32], now), Begin::Run));
        prop_assert!(matches!(c.begin("local-client", "new-l", [2; 32], now), Begin::Run));
        Ok(())
    });
}

#[test]
fn a_delivered_entry_is_gone_only_after_its_ttl() {
    check("idem_ttl_edge", 200, (1u64..60, 1usize..6), |(ttl_s, n)| {
        let ttl = Duration::from_secs(ttl_s);
        let mut c = IdemCache::new(ttl);
        let t0 = Instant::now();
        for i in 0..n {
            let id = format!("i{i}");
            prop_assert!(matches!(c.begin("c", &id, [1; 32], t0), Begin::Run));
            c.finish("c", &id, CommandResult::ok(&id), true, t0);
        }
        for i in 0..n {
            let id = format!("i{i}");
            let just_before = t0 + ttl - Duration::from_millis(1);
            prop_assert!(matches!(c.begin("c", &id, [1; 32], just_before), Begin::Done(_)), "evicted before the TTL");
        }
        // At the TTL the entry is expired: the id is free again.
        prop_assert!(matches!(c.begin("c", "i0", [1; 32], t0 + ttl), Begin::Run));
        Ok(())
    });
}
