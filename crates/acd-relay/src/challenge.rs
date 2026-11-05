//! Pending registration challenges (spec §5.3.1, §8). Memory only, fixed cap.
//!
//! The relay keeps `{code_mac, binding_mac, attempts, expires_at}` per
//! challenge: no code, no address. Confirm resends the binding fields and the
//! relay compares MACs. All state changes happen under one lock, so a
//! challenge is used at most once even under concurrent confirms.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use ring::rand::{SecureRandom, SystemRandom};

use crate::limits::Keyer;

pub const CHALLENGE_TTL_SECS: u64 = 120;
pub const MAX_PENDING: usize = 256;
pub const MAX_PER_BINDING: usize = 2;
pub const MAX_ATTEMPTS: u8 = 3;

#[derive(Debug, PartialEq, Eq)]
pub enum IssueError {
    /// The table is full even after sweeping expired entries (503).
    Busy,
    /// This binding already has the maximum pending (429).
    BindingCap,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Confirm {
    Ok,
    /// Unknown, expired or already used (404).
    Unknown,
    /// Wrong code or binding (403).
    Failed,
}

struct Entry {
    code_mac: [u8; 32],
    binding_mac: [u8; 32],
    attempts: u8,
    expires_at: u64,
}

pub struct Challenges {
    keyer: Arc<Keyer>,
    cap: usize,
    inner: Mutex<HashMap<String, Entry>>,
}

impl Challenges {
    pub fn new(keyer: Arc<Keyer>) -> Challenges {
        Challenges::with_cap(keyer, MAX_PENDING)
    }

    pub fn with_cap(keyer: Arc<Keyer>, cap: usize) -> Challenges {
        Challenges { keyer, cap, inner: Mutex::new(HashMap::new()) }
    }

    fn code_mac(&self, code: &str) -> [u8; 32] {
        self.keyer.mac(&[b"code", code.as_bytes()])
    }

    /// Returns `(challenge_id, code)`.
    pub fn issue(&self, binding_mac: [u8; 32], now: u64) -> Result<(String, String), IssueError> {
        let mut g = self.inner.lock().unwrap();
        if g.len() >= self.cap {
            g.retain(|_, e| e.expires_at > now);
        }
        if g.len() >= self.cap {
            return Err(IssueError::Busy);
        }
        let same = g
            .values()
            .filter(|e| e.expires_at > now && e.binding_mac == binding_mac)
            .count();
        if same >= MAX_PER_BINDING {
            return Err(IssueError::BindingCap);
        }
        let rng = SystemRandom::new();
        let (mut id, mut code) = ([0u8; 16], [0u8; 16]);
        rng.fill(&mut id).expect("os rng");
        rng.fill(&mut code).expect("os rng");
        let (id, code) = (B64.encode(id), B64.encode(code));
        g.insert(
            id.clone(),
            Entry {
                code_mac: self.code_mac(&code),
                binding_mac,
                attempts: 0,
                expires_at: now + CHALLENGE_TTL_SECS,
            },
        );
        Ok((id, code))
    }

    /// Drop a challenge that could not be delivered (push failed).
    pub fn cancel(&self, id: &str) {
        self.inner.lock().unwrap().remove(id);
    }

    pub fn confirm(&self, id: &str, code: &str, binding_mac: &[u8; 32], now: u64) -> Confirm {
        let mut g = self.inner.lock().unwrap();
        let Some(e) = g.get_mut(id) else {
            return Confirm::Unknown;
        };
        if e.expires_at <= now {
            g.remove(id);
            return Confirm::Unknown;
        }
        let code_ok = Keyer::eq(&e.code_mac, &self.code_mac(code));
        let bind_ok = Keyer::eq(&e.binding_mac, binding_mac);
        if code_ok && bind_ok {
            g.remove(id);
            return Confirm::Ok;
        }
        e.attempts += 1;
        if e.attempts >= MAX_ATTEMPTS {
            g.remove(id);
        }
        Confirm::Failed
    }

    /// Drop expired challenges (the periodic sweep).
    pub fn sweep(&self, now: u64) {
        self.inner.lock().unwrap().retain(|_, e| e.expires_at > now);
    }

    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch() -> Challenges {
        Challenges::new(Arc::new(Keyer::new()))
    }
    fn b(n: u8) -> [u8; 32] {
        [n; 32]
    }

    #[test]
    fn code_is_128_bits_and_unique() {
        let c = ch();
        let mut seen = std::collections::HashSet::new();
        for i in 0..50u8 {
            let (_, code) = c.issue(b(i), 0).unwrap();
            assert_eq!(code.len(), 22);
            assert_eq!(B64.decode(&code).unwrap().len(), 16);
            assert!(seen.insert(code));
        }
    }

    #[test]
    fn wrong_code_counts_and_third_burns() {
        let c = ch();
        let (id, code) = c.issue(b(1), 0).unwrap();
        assert_eq!(c.confirm(&id, "nope", &b(1), 1), Confirm::Failed);
        assert_eq!(c.confirm(&id, "nope", &b(1), 1), Confirm::Failed);
        assert_eq!(c.confirm(&id, "nope", &b(1), 1), Confirm::Failed);
        // Burned: even the right code no longer works.
        assert_eq!(c.confirm(&id, &code, &b(1), 1), Confirm::Unknown);
        assert_eq!(c.len(), 0);
    }

    #[test]
    fn correct_code_is_single_use_under_concurrency() {
        let c = Arc::new(ch());
        let (id, code) = c.issue(b(1), 0).unwrap();
        let wins: usize = std::thread::scope(|s| {
            let hs: Vec<_> = (0..16)
                .map(|_| {
                    let (c, id, code) = (c.clone(), id.clone(), code.clone());
                    s.spawn(move || (c.confirm(&id, &code, &b(1), 1) == Confirm::Ok) as usize)
                })
                .collect();
            hs.into_iter().map(|h| h.join().unwrap()).sum()
        });
        assert_eq!(wins, 1);
    }

    #[test]
    fn expired_challenge_is_unknown() {
        let c = ch();
        let (id, code) = c.issue(b(1), 0).unwrap();
        assert_eq!(c.confirm(&id, &code, &b(1), CHALLENGE_TTL_SECS), Confirm::Unknown);
    }

    #[test]
    fn binding_mismatch_is_refused_and_counts() {
        let c = ch();
        let (id, code) = c.issue(b(1), 0).unwrap();
        assert_eq!(c.confirm(&id, &code, &b(2), 1), Confirm::Failed);
        assert_eq!(c.confirm(&id, &code, &b(2), 1), Confirm::Failed);
        assert_eq!(c.confirm(&id, &code, &b(2), 1), Confirm::Failed);
        assert_eq!(c.confirm(&id, &code, &b(1), 1), Confirm::Unknown, "burned");
    }

    #[test]
    fn outstanding_cap_refuses_new_and_keeps_pending() {
        let c = Challenges::with_cap(Arc::new(Keyer::new()), 4);
        let issued: Vec<_> = (0..4u8).map(|i| c.issue(b(i), 0).unwrap()).collect();
        assert_eq!(c.issue(b(9), 1), Err(IssueError::Busy));
        assert_eq!(c.len(), 4);
        for (i, (id, code)) in issued.iter().enumerate() {
            assert_eq!(c.confirm(id, code, &b(i as u8), 2), Confirm::Ok, "pending {i} kept");
        }
        // Expired entries are swept to make room.
        let c = Challenges::with_cap(Arc::new(Keyer::new()), 1);
        c.issue(b(1), 0).unwrap();
        assert!(c.issue(b(2), CHALLENGE_TTL_SECS).is_ok());
    }

    #[test]
    fn per_binding_cap() {
        let c = ch();
        c.issue(b(1), 0).unwrap();
        c.issue(b(1), 0).unwrap();
        assert_eq!(c.issue(b(1), 0), Err(IssueError::BindingCap));
        assert!(c.issue(b(2), 0).is_ok());
    }
}
