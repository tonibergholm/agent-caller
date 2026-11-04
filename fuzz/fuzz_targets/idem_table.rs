//! The idempotency table (acd-core/src/idem.rs, PR #106) under random command
//! sequences from several network sources, the local socket and several
//! `client.id`s, against a model of its rules.
//! Invariants after every step: the answer (Run, Wait, Done, Mismatch, Full)
//! equals the model's, so the table never holds more than its capacity,
//! network, local, per-client, per-source and ids-per-source bounds; a
//! delivered command is answered `Done` with its own result until its TTL
//! ends and is never evicted before, however another source fills the table;
//! a source that is full cannot push another source's entries out; waiters
//! get the result when the command finishes; `conflicts` agrees with what
//! `begin` would answer.
#![no_main]
use acd_core::{Begin, IdemCache, IdemLimits, Origin};
use acd_protocol::CommandResult;
use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::time::{Duration, Instant};

#[derive(Arbitrary, Debug)]
enum Op {
    Begin { client: u8, origin: u8, id: u8, payload: u8, dt: u8 },
    /// Finish the pick-th command that is running.
    Finish { pick: u8, cacheable: bool },
    Conflicts { client: u8, id: u8, payload: u8 },
}

#[derive(Arbitrary, Debug)]
struct Input {
    capacity: u8,
    network: u8,
    local: u8,
    per_client: u8,
    per_source: u8,
    ids_per_source: u8,
    ttl: u8,
    ops: Vec<Op>,
}

#[derive(Clone)]
enum St {
    Pending,
    Done { at: Instant },
}

#[derive(Clone)]
struct E {
    hash: u8,
    origin: Origin,
    st: St,
}

fn origin(o: u8) -> Origin {
    match o % 5 {
        0 => Origin::Local,
        n => Origin::Net(IpAddr::V4(Ipv4Addr::new(10, 0, 0, n))),
    }
}

fuzz_target!(|i: Input| {
    let l = IdemLimits {
        capacity: 1 + i.capacity as usize % 24,
        network: 1 + i.network as usize % 24,
        local: 1 + i.local as usize % 12,
        per_client: 1 + i.per_client as usize % 8,
        per_source: 1 + i.per_source as usize % 8,
        client_ids_per_source: 1 + i.ids_per_source as usize % 4,
    };
    let ttl = Duration::from_secs(1 + u64::from(i.ttl % 20));
    let mut cache = IdemCache::with_limits(ttl, l);
    let mut m: HashMap<(u8, u8), E> = HashMap::new();
    let mut running: Vec<(u8, u8)> = vec![];
    let mut waiters: HashMap<(u8, u8), Vec<tokio::sync::watch::Receiver<Option<CommandResult>>>> = HashMap::new();
    let mut now = Instant::now();
    let name = |c: u8| format!("client-{}", c % 6);
    let idn = |x: u8| format!("id-{}", x % 5);
    let hash = |p: u8| [p % 3; 32];
    for op in i.ops.iter().take(300) {
        match *op {
            Op::Begin { client, origin: o, id, payload, dt } => {
                now += Duration::from_millis(u64::from(dt) * 250);
                m.retain(|_, e| match e.st {
                    St::Done { at } => now.duration_since(at) < ttl,
                    St::Pending => true,
                });
                let key = (client % 6, id % 5);
                let org = origin(o);
                let got = cache.begin_from(&name(client), org, &idn(id), hash(payload), now);
                let admits = || {
                    let net: usize = m.values().filter(|e| matches!(e.origin, Origin::Net(_))).count();
                    let by_client = m.keys().filter(|k| k.0 == key.0).count();
                    let by_origin: Vec<u8> = m.iter().filter(|(_, e)| e.origin == org).map(|(k, _)| k.0).collect();
                    let mut ids = by_origin.clone();
                    ids.sort();
                    ids.dedup();
                    m.len() < l.capacity
                        && by_client < l.per_client
                        && match org {
                            Origin::Local => by_origin.len() < l.local,
                            Origin::Net(_) => {
                                net < l.network
                                    && by_origin.len() < l.per_source
                                    && (ids.contains(&key.0) || ids.len() < l.client_ids_per_source)
                            }
                        }
                };
                match (&got, m.get(&key).cloned()) {
                    (Begin::Mismatch, Some(e)) => assert_ne!(e.hash, payload % 3),
                    (Begin::Wait(rx), Some(E { hash: h, st: St::Pending, .. })) => {
                        assert_eq!(h, payload % 3);
                        waiters.entry(key).or_default().push(rx.clone());
                    }
                    (Begin::Done(r), Some(E { hash: h, st: St::Done { .. }, .. })) => {
                        assert_eq!(h, payload % 3);
                        assert_eq!(r.id, format!("{}/{}", key.0, key.1), "a delivered command answers with another's result");
                    }
                    (Begin::Run, None) => {
                        assert!(admits(), "admitted past a bound: {l:?} with {} entries", m.len());
                        m.insert(key, E { hash: payload % 3, origin: org, st: St::Pending });
                        running.push(key);
                    }
                    (Begin::Full, None) => assert!(!admits(), "refused with room: {l:?} with {} entries", m.len()),
                    (g, e) => panic!(
                        "{} for {key:?}, the model has {}",
                        match g {
                            Begin::Run => "Run",
                            Begin::Wait(_) => "Wait",
                            Begin::Done(_) => "Done",
                            Begin::Mismatch => "Mismatch",
                            Begin::Full => "Full",
                        },
                        match e {
                            None => "no entry",
                            Some(E { st: St::Pending, .. }) => "a running command",
                            Some(_) => "a delivered command (or a different hash)",
                        }
                    ),
                }
            }
            Op::Finish { pick, cacheable } => {
                if running.is_empty() {
                    continue;
                }
                let key = running.remove(pick as usize % running.len());
                let Some(e) = m.get_mut(&key) else { continue };
                let result = CommandResult::ok(&format!("{}/{}", key.0, key.1));
                cache.finish(&name(key.0), &idn(key.1), result.clone(), cacheable, now);
                if cacheable {
                    e.st = St::Done { at: now };
                } else {
                    m.remove(&key);
                }
                for rx in waiters.remove(&key).into_iter().flatten() {
                    assert_eq!(rx.borrow().as_ref(), Some(&result), "a waiter was not told the result");
                }
            }
            Op::Conflicts { client, id, payload } => {
                let key = (client % 6, id % 5);
                let want = m.get(&key).is_some_and(|e| {
                    e.hash != payload % 3
                        && match e.st {
                            St::Pending => true,
                            St::Done { at } => now.duration_since(at) < ttl,
                        }
                });
                assert_eq!(cache.conflicts(&name(client), &idn(id), hash(payload), now), want);
            }
        }
    }
});
