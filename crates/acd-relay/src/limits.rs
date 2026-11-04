//! Bounded rate limiting (spec §4.6).
//!
//! Fixed windows (one minute, one hour) that start at a key's first hit.
//! Every map has a fixed key cap. When it is full, expired entries are swept;
//! if it is still full a new key is *not* inserted (that would evict, and
//! eviction lets an attacker reset their own counters). It is counted
//! against one shared overflow bucket with the same limits instead.

use std::collections::HashMap;
use std::sync::Mutex;

use ring::hmac;
use ring::rand::{SecureRandom, SystemRandom};

pub type Key = [u8; 16];

#[derive(Debug, Clone, Copy)]
pub struct Limit {
    pub per_minute: u32,
    pub per_hour: u32,
}

impl Limit {
    pub const fn hourly(n: u32) -> Limit {
        Limit { per_minute: u32::MAX, per_hour: n }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Counter {
    min_start: u64,
    min_n: u32,
    hour_start: u64,
    hour_n: u32,
}

impl Counter {
    /// Counts the hit and returns true if allowed.
    fn hit(&mut self, limit: Limit, now: u64) -> bool {
        if now >= self.min_start + 60 || self.min_n == 0 {
            self.min_start = now;
            self.min_n = 0;
        }
        if now >= self.hour_start + 3600 || self.hour_n == 0 {
            self.hour_start = now;
            self.hour_n = 0;
        }
        if self.min_n >= limit.per_minute || self.hour_n >= limit.per_hour {
            return false;
        }
        self.min_n += 1;
        self.hour_n += 1;
        true
    }

    fn live(&self, now: u64) -> bool {
        self.hour_n > 0 && now < self.hour_start + 3600
    }
}

struct Inner {
    map: HashMap<Key, Counter>,
    overflow: Counter,
    last_sweep: u64,
}

pub struct Limiter {
    cap: usize,
    limit: Limit,
    inner: Mutex<Inner>,
}

impl Limiter {
    pub fn new(cap: usize, limit: Limit) -> Limiter {
        Limiter {
            cap,
            limit,
            inner: Mutex::new(Inner {
                map: HashMap::new(),
                overflow: Counter::default(),
                last_sweep: 0,
            }),
        }
    }

    /// True if the hit is allowed (and counted).
    pub fn check(&self, key: Key, now: u64) -> bool {
        let mut g = self.inner.lock().unwrap();
        if let Some(c) = g.map.get_mut(&key) {
            return c.hit(self.limit, now);
        }
        if g.map.len() >= self.cap && g.last_sweep != now {
            g.last_sweep = now;
            g.map.retain(|_, c| c.live(now));
        }
        if g.map.len() < self.cap {
            let mut c = Counter::default();
            let ok = c.hit(self.limit, now);
            g.map.insert(key, c);
            return ok;
        }
        g.overflow.hit(self.limit, now)
    }

    /// Drop counters whose windows have ended. A counter with a live window
    /// is never removed (removing it would reset a caller's budget).
    pub fn sweep(&self, now: u64) {
        self.inner.lock().unwrap().map.retain(|_, c| c.live(now));
    }

    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// HMAC under a per-boot random key: map keys and log tags never hold the
/// IP, handle or address themselves.
pub struct Keyer(hmac::Key);

impl Keyer {
    pub fn new() -> Keyer {
        let mut k = [0u8; 32];
        SystemRandom::new().fill(&mut k).expect("os rng");
        Keyer(hmac::Key::new(hmac::HMAC_SHA256, &k))
    }

    pub fn mac(&self, parts: &[&[u8]]) -> [u8; 32] {
        let mut ctx = hmac::Context::with_key(&self.0);
        for p in parts {
            ctx.update(&(p.len() as u32).to_be_bytes());
            ctx.update(p);
        }
        let tag = ctx.sign();
        let mut out = [0u8; 32];
        out.copy_from_slice(tag.as_ref());
        out
    }

    pub fn key(&self, tag: &str, data: &[u8]) -> Key {
        let m = self.mac(&[tag.as_bytes(), data]);
        let mut k = [0u8; 16];
        k.copy_from_slice(&m[..16]);
        k
    }

    /// Constant-time comparison of two MACs.
    pub fn eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
        subtle::ConstantTimeEq::ct_eq(&a[..], &b[..]).into()
    }
}

impl Default for Keyer {
    fn default() -> Self {
        Keyer::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(n: u32) -> Key {
        let mut x = [0u8; 16];
        x[..4].copy_from_slice(&n.to_be_bytes());
        x
    }

    #[test]
    fn window_blocks_after_limit_and_resets() {
        let l = Limiter::new(8, Limit { per_minute: 3, per_hour: 5 });
        assert!(l.check(k(1), 100));
        assert!(l.check(k(1), 101));
        assert!(l.check(k(1), 102));
        assert!(!l.check(k(1), 103), "4th in a minute");
        assert!(l.check(k(1), 161), "minute window reset");
        assert!(l.check(k(1), 162));
        assert!(!l.check(k(1), 163), "hour budget (5) spent");
        assert!(l.check(k(1), 100 + 3600), "hour window reset");
    }

    #[test]
    fn map_never_exceeds_cap_under_key_flood() {
        let l = Limiter::new(16, Limit::hourly(1000));
        for i in 0..10_000 {
            l.check(k(i), 50);
            assert!(l.len() <= 16);
        }
        assert_eq!(l.len(), 16);
    }

    #[test]
    fn full_map_does_not_evict_existing_counters() {
        let l = Limiter::new(4, Limit::hourly(2));
        for i in 0..4 {
            assert!(l.check(k(i), 10));
        }
        assert!(l.check(k(0), 10));
        assert!(!l.check(k(0), 10), "key 0 is at its limit");
        // A flood of new keys must not push key 0 out and reset it.
        for i in 100..1000 {
            l.check(k(i), 11);
        }
        assert!(!l.check(k(0), 12), "key 0 still limited after the flood");
        assert_eq!(l.len(), 4);
    }

    #[test]
    fn overflow_bucket_limits_keys_that_do_not_fit() {
        let l = Limiter::new(2, Limit::hourly(3));
        assert!(l.check(k(1), 10));
        assert!(l.check(k(2), 10));
        // Unseen keys now share one bucket of 3.
        let allowed = (100..200).filter(|i| l.check(k(*i), 10)).count();
        assert_eq!(allowed, 3);
        // After the windows end, sweeping makes room again.
        assert!(l.check(k(300), 10 + 3601));
        assert!(l.len() <= 2);
    }
}
