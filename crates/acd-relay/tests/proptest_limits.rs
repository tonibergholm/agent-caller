//! Property tests of the relay limiter and its sweep (spec §4.6): fixed
//! minute and hour windows that start at a key's first allowed hit, a fixed
//! key cap, a shared overflow bucket for keys that do not fit.
//!
//! The model keeps the allowed times of each key and derives its windows from
//! them (a window starts at the first allowed hit after the previous window
//! ended), so it shares no code with the limiter. Times jump by the window
//! edges (59, 60, 61, 3599, 3600, 3601 seconds) to land on every boundary.
mod proptest_support;

use std::collections::HashMap;

use acd_relay::limits::{Key, Limit, Limiter};
use proptest::prelude::*;
use proptest_support::check;

#[derive(Clone, Copy, Debug)]
enum Op {
    Hit { key: u8, dt: u64 },
    Sweep { dt: u64 },
}

fn dt() -> impl Strategy<Value = u64> {
    prop_oneof![
        3 => Just(0u64),
        3 => 0u64..5,
        2 => prop::sample::select(vec![59u64, 60, 61, 119, 120, 3599, 3600, 3601, 3659, 3660, 7200]),
    ]
}

fn op(keys: u8) -> impl Strategy<Value = Op> {
    prop_oneof![
        8 => (0..keys, dt()).prop_map(|(key, dt)| Op::Hit { key, dt }),
        1 => dt().prop_map(|dt| Op::Sweep { dt }),
    ]
}

fn key(n: u8) -> Key {
    let mut k = [0u8; 16];
    k[0] = n;
    k
}

/// Allowed hits in the window of `len` seconds that `now` falls in.
fn in_window(times: &[u64], len: u64, now: u64) -> usize {
    let mut start: Option<u64> = None;
    for &t in times {
        if start.is_none_or(|s| t >= s + len) {
            start = Some(t);
        }
    }
    match start {
        Some(s) if now < s + len => times.iter().filter(|&&t| t >= s).count(),
        _ => 0,
    }
}

fn allows(times: &[u64], limit: Limit, now: u64) -> bool {
    in_window(times, 60, now) < limit.per_minute as usize && in_window(times, 3600, now) < limit.per_hour as usize
}

fn limit() -> impl Strategy<Value = Limit> {
    (1u32..8, 1u32..30).prop_map(|(m, h)| Limit { per_minute: m, per_hour: h })
}

#[test]
fn with_room_for_every_key_the_limiter_equals_the_window_model_and_a_sweep_changes_nothing() {
    check(
        "limiter_model",
        300,
        (limit(), 1_000_000u64..2_000_000, prop::collection::vec(op(6), 1..120)),
        |(limit, start, ops)| {
            let l = Limiter::new(1 << 12, limit);
            let mut times: HashMap<u8, Vec<u64>> = HashMap::new();
            let mut now = start;
            for op in ops {
                match op {
                    Op::Hit { key: k, dt } => {
                        now += dt;
                        let t = times.entry(k).or_default();
                        let want = allows(t, limit, now);
                        if want {
                            t.push(now);
                        }
                        prop_assert_eq!(l.check(key(k), now), want, "key {} at {}", k, now);
                    }
                    Op::Sweep { dt } => {
                        now += dt;
                        l.sweep(now);
                    }
                }
            }
            Ok(())
        },
    );
}

#[test]
fn the_map_never_exceeds_its_cap_and_never_forgets_a_key_with_a_live_window() {
    // The first `cap` keys are resident. Whatever floods the map afterwards,
    // a resident key is decided exactly as the model decides for as long as
    // its window is live: eviction would reset it.
    check(
        "limiter_cap",
        300,
        (limit(), 1usize..6, 1_000_000u64..2_000_000, prop::collection::vec(op(14), 1..160)),
        |(limit, cap, start, ops)| {
            let l = Limiter::new(cap, limit);
            let mut times: HashMap<u8, Vec<u64>> = HashMap::new();
            let mut resident: Vec<u8> = vec![];
            let mut seen = std::collections::HashSet::new();
            let mut now = start;
            for op in ops {
                match op {
                    Op::Hit { key: k, dt } => {
                        now += dt;
                        let first_sight = seen.insert(k);
                        let t = times.entry(k).or_default();
                        let want = allows(t, limit, now);
                        let live = in_window(t, 60, now) > 0 || in_window(t, 3600, now) > 0;
                        if first_sight && seen.len() <= cap {
                            resident.push(k);
                        } else if !live && !first_sight {
                            resident.retain(|x| *x != k); // its window ended: a sweep may have dropped it
                        }
                        if want {
                            t.push(now);
                        }
                        let got = l.check(key(k), now);
                        if resident.contains(&k) && (live || first_sight) {
                            prop_assert_eq!(got, want, "resident key {} at {}", k, now);
                        }
                    }
                    Op::Sweep { dt } => {
                        now += dt;
                        l.sweep(now);
                    }
                }
                prop_assert!(l.len() <= cap, "{} keys, cap {}", l.len(), cap);
            }
            Ok(())
        },
    );
}

#[test]
fn a_window_edge_is_exact_on_both_sides() {
    // `per_minute` hits fill the minute; the next is refused until exactly 60 s
    // after the first, and the hour budget ends exactly 3600 s after its first.
    check("limiter_edges", 200, (1u32..6, 6u32..20, 1_000_000u64..2_000_000), |(pm, ph, t0)| {
        let l = Limiter::new(8, Limit { per_minute: pm, per_hour: ph });
        for _ in 0..pm {
            prop_assert!(l.check(key(1), t0));
        }
        prop_assert!(!l.check(key(1), t0 + 59), "inside the minute");
        prop_assert!(l.check(key(1), t0 + 60), "the minute ends at +60");
        // Spend the hour budget, then check the hour edge.
        let l = Limiter::new(8, Limit { per_minute: u32::MAX, per_hour: ph });
        for _ in 0..ph {
            prop_assert!(l.check(key(2), t0));
        }
        prop_assert!(!l.check(key(2), t0 + 3599), "inside the hour");
        prop_assert!(l.check(key(2), t0 + 3600), "the hour ends at +3600");
        Ok(())
    });
}

#[test]
fn a_sweep_keeps_a_counter_while_either_window_is_live_and_drops_it_after() {
    check("limiter_sweep", 200, (1u32..6, 1_000_000u64..2_000_000, 0u64..3600, 0u64..60), |(pm, t0, a, b)| {
        let limit = Limit { per_minute: pm, per_hour: 100 };
        // Keys 0..4 each hit once at t0; a flood of other keys must not matter.
        let l = Limiter::new(4, limit);
        for k in 0..4u8 {
            prop_assert!(l.check(key(k), t0));
        }
        l.sweep(t0 + a.min(3599));
        prop_assert_eq!(l.len(), 4, "live windows survive a sweep");
        l.sweep(t0 + 3600 + b);
        prop_assert_eq!(l.len(), 0, "dead windows are dropped");
        Ok(())
    });
}
