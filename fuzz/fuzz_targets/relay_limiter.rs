//! The relay rate limiter (acd-relay/src/limits.rs): fixed minute and hour
//! windows that start at a key's first allowed hit, a fixed key cap, and one
//! shared overflow bucket for keys that do not fit.
//! Invariants over a random sequence of hits and sweeps on a moving clock:
//! the map never holds more than its cap; with room for every key the
//! decision equals a model that keeps the allowed times of each key and
//! derives its windows from them, and a sweep never changes a decision; with
//! a small cap, a key that got in first (one of the first `cap` keys) is
//! decided exactly as the model decides for as long as it has a live window,
//! whatever flood of other keys comes, so eviction cannot reset it.
#![no_main]
use acd_relay::limits::{Key, Limit, Limiter};
use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use std::collections::HashMap;

#[derive(Arbitrary, Debug)]
enum Op {
    Hit { key: u8, dt: u8 },
    Sweep { dt: u8 },
    /// A step of a minute or more, to cross window edges.
    Jump { dt: u8 },
}

#[derive(Arbitrary, Debug)]
struct Input {
    cap: u8,
    per_minute: u8,
    per_hour: u8,
    start: u32,
    ops: Vec<Op>,
}

fn key(n: u8) -> Key {
    let mut k = [0u8; 16];
    k[0] = n;
    k
}

/// The allowed hits in the window of `len` seconds that `now` falls in,
/// where each window starts at the first allowed hit after the last ended.
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

fn model_allows(times: &[u64], limit: Limit, now: u64) -> bool {
    in_window(times, 60, now) < limit.per_minute as usize && in_window(times, 3600, now) < limit.per_hour as usize
}

fuzz_target!(|i: Input| {
    let limit = Limit { per_minute: u32::from(i.per_minute % 8), per_hour: u32::from(i.per_hour % 24) };
    let cap = 1 + i.cap as usize % 6;
    let small = Limiter::new(cap, limit);
    let big = Limiter::new(1 << 16, limit);
    let mut times: HashMap<u8, Vec<u64>> = HashMap::new();
    let mut first: Vec<u8> = vec![]; // the keys known to be resident
    let mut seen = std::collections::HashSet::new();
    let mut now = u64::from(i.start);
    for op in i.ops.iter().take(300) {
        match *op {
            Op::Hit { key: k, dt } => {
                now += u64::from(dt % 8);
                let k = k % 12;
                let first_sight = seen.insert(k);
                let t = times.entry(k).or_default();
                let want = model_allows(t, limit, now);
                let live = in_window(t, 60, now) > 0 || in_window(t, 3600, now) > 0;
                if first_sight && first.len() < cap {
                    first.push(k); // the map has room, so this key is resident
                } else if !live && !first_sight {
                    first.retain(|x| *x != k); // its window ended: a sweep may have dropped it
                }
                if want {
                    t.push(now);
                }
                assert_eq!(big.check(key(k), now), want, "key {k} at {now}: the exact limiter and the model differ");
                let got = small.check(key(k), now);
                if first.contains(&k) && (live || first_sight) {
                    assert_eq!(got, want, "key {k} at {now}: a resident key's window was lost or changed");
                }
            }
            Op::Sweep { dt } => {
                now += u64::from(dt % 8);
                small.sweep(now);
                big.sweep(now);
            }
            Op::Jump { dt } => now += 60 + u64::from(dt) * 15,
        }
        assert!(small.len() <= cap, "{} keys, cap {cap}", small.len());
    }
});
