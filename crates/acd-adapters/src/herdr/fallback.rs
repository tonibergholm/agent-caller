//! Read-only fallback (spec §6.4): a pane blocked for a while with no
//! structured request gets a `blocked_unparsed` request showing its screen.
//! This is a pure state machine; the reconciler does the I/O.
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use acd_core::CorrelationKey;

use super::screen::{Fingerprint, fallback_fingerprint};
use crate::text::{RAW_SCREEN_LINES, cap_lines, clean_screen};

/// Consecutive reads without the open dialog before it counts as gone.
const ABSENT_READS: u8 = 2;

#[derive(Debug, PartialEq, Eq)]
pub enum FbOut {
    Open {
        fingerprint: Fingerprint,
        raw_screen: String,
    },
    End {
        fingerprint: Fingerprint,
    },
}

/// `herdr:<socket hash>:<pane id>:<fingerprint>`.
pub fn key(socket_hash: &str, pane_id: &str, fp: &Fingerprint) -> CorrelationKey {
    CorrelationKey(format!("herdr:{socket_hash}:{pane_id}:{}", fp.0))
}

struct Open {
    fingerprint: Fingerprint,
    absent: u8,
}

#[derive(Default)]
struct Pane {
    blocked_since: Option<Instant>,
    open: Option<Open>,
}

impl Pane {
    fn idle(&self) -> bool {
        self.blocked_since.is_none() && self.open.is_none()
    }
}

pub struct Fallback {
    after: Duration,
    panes: BTreeMap<String, Pane>,
}

impl Fallback {
    pub fn new(after: Duration) -> Fallback {
        Fallback {
            after,
            panes: BTreeMap::new(),
        }
    }

    /// Records whether the pane's agent is blocked, as of `now`.
    pub fn on_state(&mut self, pane: &str, blocked: bool, now: Instant) {
        if !blocked && !self.panes.contains_key(pane) {
            return;
        }
        let p = self.panes.entry(pane.to_owned()).or_default();
        if !blocked {
            p.blocked_since = None;
        } else if p.blocked_since.is_none() {
            p.blocked_since = Some(now);
        }
        if p.idle() {
            self.panes.remove(pane);
        }
    }

    /// Panes that need a detection read now: blocked long enough, or with an
    /// open fallback not yet confirmed gone.
    pub fn due(&self, now: Instant) -> Vec<String> {
        self.panes
            .iter()
            .filter(|(_, p)| p.open.is_some() || self.long_enough(p, now))
            .map(|(id, _)| id.clone())
            .collect()
    }

    fn long_enough(&self, p: &Pane, now: Instant) -> bool {
        p.blocked_since
            .is_some_and(|s| now.saturating_duration_since(s) >= self.after)
    }

    /// Feeds one `detection` read of `pane`.
    pub fn on_read(
        &mut self,
        pane: &str,
        text: &str,
        other_pending: bool,
        now: Instant,
    ) -> Vec<FbOut> {
        let Some(p) = self.panes.get(pane) else {
            return vec![];
        };
        let long_enough = self.long_enough(p, now);
        let p = self.panes.get_mut(pane).expect("checked above");
        let fp = fallback_fingerprint(text);
        let out = if let Some(open) = &mut p.open {
            if p.blocked_since.is_some() && fp == open.fingerprint {
                open.absent = 0;
                vec![]
            } else {
                open.absent += 1;
                if open.absent >= ABSENT_READS {
                    let fingerprint = open.fingerprint.clone();
                    p.open = None;
                    vec![FbOut::End { fingerprint }]
                } else {
                    vec![]
                }
            }
        } else if long_enough && !other_pending {
            p.open = Some(Open {
                fingerprint: fp.clone(),
                absent: 0,
            });
            vec![FbOut::Open {
                fingerprint: fp,
                raw_screen: clean_screen(&cap_lines(text, RAW_SCREEN_LINES)),
            }]
        } else {
            vec![]
        };
        if p.idle() {
            self.panes.remove(pane);
        }
        out
    }

    /// Drops an `Open` the registry refused (a structured request won), so the
    /// pane is judged afresh on the next read.
    pub fn discard_open(&mut self, pane: &str) {
        if let Some(p) = self.panes.get_mut(pane) {
            p.open = None;
        }
    }

    pub fn forget(&mut self, pane: &str) {
        self.panes.remove(pane);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_after_two_seconds_and_ends_after_two_absent_reads() {
        let t0 = Instant::now();
        let s = Duration::from_secs(1);
        let mut f = Fallback::new(2 * s);
        f.on_state("p", true, t0);
        assert!(f.due(t0 + s).is_empty());
        assert_eq!(f.due(t0 + 2 * s), vec!["p".to_string()]);
        let out = f.on_read("p", "dialog A", false, t0 + 2 * s);
        assert!(matches!(&out[..], [FbOut::Open { .. }]));
        assert!(f.on_read("p", "dialog A", false, t0 + 3 * s).is_empty());
        f.on_state("p", false, t0 + 4 * s);
        assert!(
            f.on_read("p", "prompt", false, t0 + 4 * s).is_empty(),
            "one absent read is not enough"
        );
        assert!(matches!(
            &f.on_read("p", "prompt", false, t0 + 5 * s)[..],
            [FbOut::End { .. }]
        ));
        assert!(f.due(t0 + 6 * s).is_empty());
    }

    #[test]
    fn flicker_resets_the_absent_count() {
        let t0 = Instant::now();
        let mut f = Fallback::new(Duration::ZERO);
        f.on_state("p", true, t0);
        f.on_read("p", "dialog A", false, t0);
        assert!(f.on_read("p", "redraw", false, t0).is_empty());
        assert!(f.on_read("p", "dialog A", false, t0).is_empty());
        assert!(
            f.on_read("p", "redraw", false, t0).is_empty(),
            "absent reads must be consecutive"
        );
    }

    #[test]
    fn another_pending_request_suppresses_opening() {
        let t0 = Instant::now();
        let mut f = Fallback::new(Duration::ZERO);
        f.on_state("p", true, t0);
        assert!(f.on_read("p", "dialog A", true, t0).is_empty());
    }

    #[test]
    fn raw_screen_is_capped() {
        let t0 = Instant::now();
        let mut f = Fallback::new(Duration::ZERO);
        f.on_state("p", true, t0);
        let screen: String = (0..100).map(|i| format!("l{i}\n")).collect();
        match &f.on_read("p", &screen, false, t0)[..] {
            [FbOut::Open { raw_screen, .. }] => assert_eq!(raw_screen.lines().count(), 40),
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn a_replacing_dialog_ends_the_old_one_then_opens_the_new() {
        let t0 = Instant::now();
        let mut f = Fallback::new(Duration::ZERO);
        f.on_state("p", true, t0);
        f.on_read("p", "dialog A", false, t0);
        assert!(f.on_read("p", "dialog B", false, t0).is_empty());
        assert!(matches!(
            &f.on_read("p", "dialog B", false, t0)[..],
            [FbOut::End { .. }]
        ));
        assert!(matches!(
            &f.on_read("p", "dialog B", false, t0)[..],
            [FbOut::Open { .. }]
        ));
    }

    #[test]
    fn discarding_an_open_lets_it_retry_and_forget_clears_the_pane() {
        let t0 = Instant::now();
        let mut f = Fallback::new(Duration::ZERO);
        f.on_state("p", true, t0);
        assert!(matches!(
            &f.on_read("p", "dialog A", false, t0)[..],
            [FbOut::Open { .. }]
        ));
        f.discard_open("p");
        assert!(matches!(
            &f.on_read("p", "dialog A", false, t0)[..],
            [FbOut::Open { .. }]
        ));
        f.forget("p");
        assert!(f.due(t0).is_empty());
    }

    #[test]
    fn key_has_the_documented_shape() {
        let k = key("h", "w1:p1", &Fingerprint("abc".into()));
        assert_eq!(k.0, "herdr:h:w1:p1:abc");
    }
}
