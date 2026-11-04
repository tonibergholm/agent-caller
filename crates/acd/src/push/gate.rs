//! The per-device debounce (relay spec §2.3): at most one wake per device per
//! interval, and a request that opens inside the window schedules one
//! trailing wake at the window's end. Nothing else is queued.
//!
//! Pure: callers pass the time, so tests move it by hand.

use std::time::Duration;

use tokio::time::Instant;

#[derive(Debug, Default)]
pub struct Gate {
    /// When the last wake was attempted.
    last: Option<Instant>,
    /// A request opened inside the window; one wake is owed at its end.
    trailing: bool,
}

impl Gate {
    fn window_end(&self, min: Duration) -> Option<Instant> {
        self.last.map(|l| l + min)
    }

    /// A request opened. `true`: wake now. `false`: inside the window, and
    /// one trailing wake is now owed.
    pub fn opened(&mut self, now: Instant, min: Duration) -> bool {
        if self.window_end(min).is_some_and(|end| now < end) {
            self.trailing = true;
            return false;
        }
        true
    }

    /// Whether the owed trailing wake is due.
    pub fn trailing_due(&self, now: Instant, min: Duration) -> bool {
        self.trailing && self.window_end(min).is_none_or(|end| now >= end)
    }

    /// When the owed trailing wake falls due, if one is owed.
    pub fn deadline(&self, min: Duration) -> Option<Instant> {
        self.trailing.then(|| self.window_end(min)).flatten()
    }

    /// A wake was attempted at `now`: a new window starts, nothing is owed.
    pub fn attempted(&mut self, now: Instant) {
        self.last = Some(now);
        self.trailing = false;
    }

    /// The wake was not attempted (backoff or disabled): nothing is owed, and
    /// the window does not move.
    pub fn dropped(&mut self) {
        self.trailing = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: Duration = Duration::from_secs(30);

    #[test]
    fn the_first_open_wakes_and_later_ones_inside_the_window_owe_one() {
        let t0 = Instant::now();
        let mut g = Gate::default();
        assert!(g.opened(t0, MIN));
        g.attempted(t0);
        for s in [1, 2, 10, 29] {
            assert!(!g.opened(t0 + Duration::from_secs(s), MIN));
        }
        // One wake is owed, not four.
        assert_eq!(g.deadline(MIN), Some(t0 + MIN));
        assert!(!g.trailing_due(t0 + Duration::from_secs(29), MIN));
        assert!(g.trailing_due(t0 + MIN, MIN));
        g.attempted(t0 + MIN);
        assert_eq!(g.deadline(MIN), None);
        assert!(!g.trailing_due(t0 + MIN * 3, MIN));
    }

    #[test]
    fn an_open_after_the_window_wakes_at_once() {
        let t0 = Instant::now();
        let mut g = Gate::default();
        g.attempted(t0);
        assert!(g.opened(t0 + MIN, MIN));
    }

    #[test]
    fn without_an_open_nothing_is_owed() {
        let t0 = Instant::now();
        let mut g = Gate::default();
        g.attempted(t0);
        assert_eq!(g.deadline(MIN), None);
        assert!(!g.trailing_due(t0 + MIN * 10, MIN));
    }

    #[test]
    fn a_dropped_wake_owes_nothing_and_keeps_the_window() {
        let t0 = Instant::now();
        let mut g = Gate::default();
        g.attempted(t0);
        assert!(!g.opened(t0 + Duration::from_secs(1), MIN));
        g.dropped();
        assert_eq!(g.deadline(MIN), None);
        assert!(g.opened(t0 + MIN, MIN));
    }
}
