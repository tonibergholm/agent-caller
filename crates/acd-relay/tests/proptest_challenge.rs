//! Property tests of registration challenges (spec §5.3.1, §8): a challenge
//! is used at most once, the third wrong attempt burns it, the table is
//! bounded, and the sequential behaviour equals a plain model under any
//! interleaving of several clients; a threaded run keeps the same bounds.
mod proptest_support;

use std::collections::HashMap;
use std::sync::Arc;

use acd_relay::challenge::{CHALLENGE_TTL_SECS, Challenges, Confirm, IssueError, MAX_ATTEMPTS, MAX_PER_BINDING};
use acd_relay::limits::Keyer;
use proptest::prelude::*;
use proptest_support::check;

#[derive(Clone, Copy, Debug)]
enum Kind {
    Right,
    WrongCode,
    WrongBinding,
}

#[derive(Clone, Copy, Debug)]
enum Op {
    Issue { binding: u8, dt: u64 },
    Confirm { pick: usize, kind: Kind, dt: u64 },
    Cancel { pick: usize },
    Sweep { dt: u64 },
}

fn dt() -> impl Strategy<Value = u64> {
    prop_oneof![
        4 => Just(0u64),
        2 => 0u64..10,
        2 => prop::sample::select(vec![CHALLENGE_TTL_SECS - 1, CHALLENGE_TTL_SECS, CHALLENGE_TTL_SECS + 1]),
    ]
}

fn op() -> impl Strategy<Value = Op> {
    let kind = prop_oneof![3 => Just(Kind::Right), 2 => Just(Kind::WrongCode), 1 => Just(Kind::WrongBinding)];
    prop_oneof![
        4 => (0u8..4, dt()).prop_map(|(binding, dt)| Op::Issue { binding, dt }),
        6 => (any::<usize>(), kind, dt()).prop_map(|(pick, kind, dt)| Op::Confirm { pick, kind, dt }),
        1 => any::<usize>().prop_map(|pick| Op::Cancel { pick }),
        1 => dt().prop_map(|dt| Op::Sweep { dt }),
    ]
}

fn mac(b: u8) -> [u8; 32] {
    [b; 32]
}

struct M {
    code: String,
    binding: u8,
    attempts: u8,
    expires: u64,
}

#[test]
fn the_table_equals_the_model_under_any_interleaving_and_a_challenge_is_used_once() {
    check("challenge_model", 300, (1usize..6, 0u64..100_000, prop::collection::vec(op(), 1..120)), |(cap, start, ops)| {
        let c = Challenges::with_cap(Arc::new(Keyer::new()), cap);
        let mut model: HashMap<String, M> = HashMap::new();
        let mut ids: Vec<String> = vec![];
        let mut oks: HashMap<String, usize> = HashMap::new();
        let mut now = start;
        for op in ops {
            match op {
                Op::Issue { binding, dt } => {
                    now += dt;
                    let got = c.issue(mac(binding), now);
                    if model.len() >= cap {
                        model.retain(|_, e| e.expires > now);
                    }
                    let want = if model.len() >= cap {
                        Some(IssueError::Busy)
                    } else if model.values().filter(|e| e.expires > now && e.binding == binding).count() >= MAX_PER_BINDING {
                        Some(IssueError::BindingCap)
                    } else {
                        None
                    };
                    match (got, want) {
                        (Err(g), Some(w)) => prop_assert_eq!(g, w),
                        (Ok((id, code)), None) => {
                            prop_assert_eq!(code.len(), 22);
                            prop_assert!(!model.contains_key(&id) && !ids.contains(&id), "id reused");
                            model.insert(id.clone(), M { code, binding, attempts: 0, expires: now + CHALLENGE_TTL_SECS });
                            ids.push(id);
                        }
                        (g, w) => prop_assert!(false, "issue gave {:?}, the model says {:?}", g, w),
                    }
                }
                Op::Confirm { pick, kind, dt } => {
                    now += dt;
                    if ids.is_empty() {
                        continue;
                    }
                    let id = ids[pick % ids.len()].clone();
                    let (code, binding) = model.get(&id).map_or(("x".to_string(), 0), |m| (m.code.clone(), m.binding));
                    let (sc, sb) = match kind {
                        Kind::Right => (code.clone(), binding),
                        Kind::WrongCode => (format!("{code}!"), binding),
                        Kind::WrongBinding => (code.clone(), binding + 8),
                    };
                    let got = c.confirm(&id, &sc, &mac(sb), now);
                    let want = match model.get_mut(&id) {
                        None => Confirm::Unknown,
                        Some(m) if m.expires <= now => {
                            model.remove(&id);
                            Confirm::Unknown
                        }
                        Some(m) if sc == m.code && sb == m.binding => {
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
                    prop_assert_eq!(&got, &want, "{:?} on {} at {}", kind, id, now);
                    if got == Confirm::Ok {
                        *oks.entry(id).or_default() += 1;
                    }
                }
                Op::Cancel { pick } => {
                    if let Some(id) = ids.get(pick % ids.len().max(1)).cloned() {
                        c.cancel(&id);
                        model.remove(&id);
                    }
                }
                Op::Sweep { dt } => {
                    now += dt;
                    c.sweep(now);
                    model.retain(|_, e| e.expires > now);
                }
            }
            prop_assert_eq!(c.len(), model.len());
            prop_assert!(c.len() <= cap);
        }
        prop_assert!(oks.values().all(|n| *n == 1), "a challenge confirmed more than once: {:?}", oks);
        Ok(())
    });
}

#[test]
fn threads_racing_right_and_wrong_confirms_use_each_challenge_once_and_fail_at_most_twice_before_it() {
    check("challenge_threads", 40, (2usize..5, 1usize..4, prop::collection::vec((0usize..4, 0u8..3), 4..40)), |(threads, n, plan)| {
        let c = Arc::new(Challenges::new(Arc::new(Keyer::new())));
        let issued: Vec<(String, String)> = (0..n).map(|i| c.issue(mac(i as u8), 0).unwrap()).collect();
        let issued = Arc::new(issued);
        let results: Vec<Vec<(usize, Confirm)>> = std::thread::scope(|s| {
            let hs: Vec<_> = (0..threads)
                .map(|t| {
                    let (c, issued, plan) = (c.clone(), issued.clone(), plan.clone());
                    s.spawn(move || {
                        plan.iter()
                            .enumerate()
                            .filter(|(i, _)| i % threads == t)
                            .map(|(_, (pick, kind))| {
                                let k = pick % issued.len();
                                let (id, code) = &issued[k];
                                let r = match kind {
                                    0 => c.confirm(id, code, &mac(k as u8), 1),
                                    1 => c.confirm(id, "wrong", &mac(k as u8), 1),
                                    _ => c.confirm(id, code, &mac(200), 1),
                                };
                                (k, r)
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        for k in 0..n {
            let all: Vec<&Confirm> = results.iter().flatten().filter(|(i, _)| *i == k).map(|(_, r)| r).collect();
            let oks = all.iter().filter(|r| ***r == Confirm::Ok).count();
            let fails = all.iter().filter(|r| ***r == Confirm::Failed).count();
            prop_assert!(oks <= 1, "challenge {} confirmed {} times", k, oks);
            prop_assert!(fails <= MAX_ATTEMPTS as usize, "challenge {} failed {} times", k, fails);
            if oks == 1 {
                prop_assert!(fails < MAX_ATTEMPTS as usize, "challenge {} confirmed after being burned", k);
            }
        }
        Ok(())
    });
}
