//! Registration challenges and their attempt accounting
//! (acd-relay/src/challenge.rs), against a plain model of the spec.
//! Invariants, after every step of a random sequence of issue, confirm
//! (right, wrong code, wrong binding, unknown id), cancel and sweep over a
//! moving clock: the result equals the model's; a challenge is confirmed at
//! most once; the third wrong attempt burns it for the right code too; an
//! expired one is unknown; the table never holds more than its cap; a binding
//! never holds more than two live challenges; and the code is 22 characters
//! of base64url that differ between issues.
#![no_main]
use acd_relay::challenge::{CHALLENGE_TTL_SECS, Challenges, Confirm, IssueError, MAX_ATTEMPTS, MAX_PER_BINDING};
use acd_relay::limits::Keyer;
use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Arbitrary, Debug)]
enum Op {
    Issue { binding: u8, dt: u8 },
    ConfirmRight { pick: u8, dt: u8 },
    WrongCode { pick: u8, dt: u8 },
    WrongBinding { pick: u8, dt: u8 },
    Unknown { dt: u8 },
    Cancel { pick: u8 },
    Sweep { dt: u8 },
}

#[derive(Arbitrary, Debug)]
struct Input {
    cap: u8,
    start: u32,
    ops: Vec<Op>,
}

struct M {
    code: String,
    binding: u8,
    attempts: u8,
    expires: u64,
}

fn mac(b: u8) -> [u8; 32] {
    [b; 32]
}

fuzz_target!(|i: Input| {
    let cap = 1 + i.cap as usize % 6;
    let c = Challenges::with_cap(Arc::new(Keyer::new()), cap);
    let mut model: HashMap<String, M> = HashMap::new();
    let mut order: Vec<String> = vec![]; // every id ever issued, for picking
    let mut codes = std::collections::HashSet::new();
    let mut now = u64::from(i.start);
    let pick = |order: &Vec<String>, p: u8| order.get(p as usize % order.len().max(1)).cloned().unwrap_or_default();
    for op in i.ops.iter().take(200) {
        match *op {
            Op::Issue { binding, dt } => {
                now += u64::from(dt);
                let b = binding % 4;
                let got = c.issue(mac(b), now);
                if model.len() >= cap {
                    model.retain(|_, e| e.expires > now);
                }
                let want = if model.len() >= cap {
                    Some(IssueError::Busy)
                } else if model.values().filter(|e| e.expires > now && e.binding == b).count() >= MAX_PER_BINDING {
                    Some(IssueError::BindingCap)
                } else {
                    None
                };
                match (got, want) {
                    (Err(g), Some(w)) => assert_eq!(g, w),
                    (Ok((id, code)), None) => {
                        assert_eq!(code.len(), 22, "code {code:?}");
                        assert!(code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
                        assert!(codes.insert(code.clone()), "a code was issued twice");
                        assert!(!model.contains_key(&id), "an id was issued twice");
                        model.insert(id.clone(), M { code, binding: b, attempts: 0, expires: now + CHALLENGE_TTL_SECS });
                        order.push(id);
                    }
                    (g, w) => panic!("issue gave {g:?}, the model says {w:?}"),
                }
            }
            Op::ConfirmRight { pick: p, dt } | Op::WrongCode { pick: p, dt } | Op::WrongBinding { pick: p, dt } => {
                now += u64::from(dt);
                let id = pick(&order, p);
                let (code, binding) = match model.get(&id) {
                    Some(m) => (m.code.clone(), m.binding),
                    None => ("whatever".to_string(), 0),
                };
                let (send_code, send_binding) = match op {
                    Op::ConfirmRight { .. } => (code.clone(), binding),
                    Op::WrongCode { .. } => (format!("{code}x"), binding),
                    _ => (code.clone(), binding.wrapping_add(1) % 4 + 4),
                };
                let got = c.confirm(&id, &send_code, &mac(send_binding), now);
                let want = match model.get_mut(&id) {
                    None => Confirm::Unknown,
                    Some(m) if m.expires <= now => {
                        model.remove(&id);
                        Confirm::Unknown
                    }
                    Some(m) if send_code == m.code && send_binding == m.binding => {
                        model.remove(&id);
                        Confirm::Ok
                    }
                    Some(m) => {
                        m.attempts += 1;
                        if m.attempts >= MAX_ATTEMPTS {
                            model.remove(&id);
                        }
                        Confirm::Failed
                    }
                };
                assert_eq!(got, want, "{op:?} at {now}");
            }
            Op::Unknown { dt } => {
                now += u64::from(dt);
                assert_eq!(c.confirm("no-such-id", "x", &mac(0), now), Confirm::Unknown);
            }
            Op::Cancel { pick: p } => {
                let id = pick(&order, p);
                c.cancel(&id);
                model.remove(&id);
            }
            Op::Sweep { dt } => {
                now += u64::from(dt);
                c.sweep(now);
                model.retain(|_, e| e.expires > now);
            }
        }
        assert_eq!(c.len(), model.len());
        assert!(c.len() <= cap, "{} challenges, cap {cap}", c.len());
    }
});
