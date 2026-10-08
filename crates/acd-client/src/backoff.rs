//! Exponential reconnect delay with a fixed upper bound.
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Backoff {
    base: Duration,
    max: Duration,
    next: Duration,
}

impl Backoff {
    pub fn new(base: Duration, max: Duration) -> Self {
        Self {
            base,
            max,
            next: base.min(max),
        }
    }
    pub fn next_delay(&mut self) -> Duration {
        let d = self.next;
        self.next = d.saturating_mul(2).min(self.max);
        d
    }
    pub fn reset(&mut self) {
        self.next = self.base.min(self.max);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_to_the_cap_and_resets() {
        let mut b = Backoff::new(Duration::from_secs(1), Duration::from_secs(30));
        let got: Vec<u64> = (0..7).map(|_| b.next_delay().as_secs()).collect();
        assert_eq!(got, [1, 2, 4, 8, 16, 30, 30]);
        b.reset();
        assert_eq!(b.next_delay(), Duration::from_secs(1));
    }

    #[test]
    fn base_above_max_starts_at_the_cap() {
        let mut b = Backoff::new(Duration::from_secs(60), Duration::from_secs(30));
        assert_eq!(b.next_delay(), Duration::from_secs(30));
        b.reset();
        assert_eq!(b.next_delay(), Duration::from_secs(30));
    }
}
