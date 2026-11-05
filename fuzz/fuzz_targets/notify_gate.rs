//! The client's notification gate (acd-client/src/inbox.rs `NotifyGate`):
//! each request key notifies once, from a history bounded to 256 keys, and
//! each machine may raise at most 5 notifications per 10 seconds.
//! Invariants, against a plain model over a random sequence of requests from
//! a few machines on a monotonic clock, including removing a machine: the
//! answer equals the model's; a key stays suppressed while it is among the
//! last 256 distinct keys and is let through again after 256 others; a
//! machine that spent its budget is refused until its oldest notification is
//! 10 seconds old; one machine's flood never spends another's budget.
#![no_main]
use acd_client::inbox::fuzzing::NotifyGate;
use acd_client::inbox::{NOTIFIED_HISTORY_MAX, NOTIFY_RATE_MAX, NOTIFY_RATE_WINDOW};
use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

#[derive(Arbitrary, Debug)]
enum Op {
    Admit { machine: u8, id: u8, request: u16, opened: u8, dt: u16 },
    /// A burst of fresh keys from one machine at one instant.
    Flood { machine: u8, n: u16, dt: u16 },
    Forget { machine: u8 },
}

#[derive(Arbitrary, Debug)]
struct Input {
    ops: Vec<Op>,
}

type Key = (u8, u32, u8);

struct Model {
    order: VecDeque<Key>,
    recent: HashMap<u8, VecDeque<Instant>>,
}

fn step(gate: &mut NotifyGate, m: &mut Model, now: Instant, machine: u8, key: Key) {
    let mid = machine % 4;
    let got = gate.admit(
        &format!("m{mid}"),
        format!("id{}", key.0),
        format!("r{}", key.1),
        chrono::DateTime::from_timestamp(1_000 + i64::from(key.2), 0).unwrap(),
        now,
    );
    let want = if m.order.contains(&key) {
        false
    } else {
        m.order.push_back(key);
        while m.order.len() > NOTIFIED_HISTORY_MAX {
            m.order.pop_front();
        }
        let q = m.recent.entry(mid).or_default();
        while q.front().is_some_and(|t| now.duration_since(*t) >= NOTIFY_RATE_WINDOW) {
            q.pop_front();
        }
        if q.len() >= NOTIFY_RATE_MAX {
            false
        } else {
            q.push_back(now);
            true
        }
    };
    assert_eq!(got, want, "key {key:?} from m{mid}");
    assert!(m.order.len() <= NOTIFIED_HISTORY_MAX);
    assert!(m.recent.values().all(|q| q.len() <= NOTIFY_RATE_MAX));
}

fuzz_target!(|i: Input| {
    let mut gate = NotifyGate::default();
    let mut m = Model { order: VecDeque::new(), recent: HashMap::new() };
    let mut now = Instant::now();
    let mut fresh: u32 = 0;
    for op in i.ops.iter().take(400) {
        match *op {
            Op::Admit { machine, id, request, opened, dt } => {
                now += Duration::from_millis(u64::from(dt) * 4);
                step(&mut gate, &mut m, now, machine, (id % 3, u32::from(request % 600), opened % 2));
            }
            Op::Flood { machine, n, dt } => {
                now += Duration::from_millis(u64::from(dt) * 4);
                for _ in 0..(n % 700) {
                    fresh += 1;
                    step(&mut gate, &mut m, now, machine, (9, fresh, 0));
                }
            }
            Op::Forget { machine } => {
                gate.forget_machine(&format!("m{}", machine % 4));
                m.recent.remove(&(machine % 4));
            }
        }
    }
});
