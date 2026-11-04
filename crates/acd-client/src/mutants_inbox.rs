//! Tests added from a cargo-mutants run (docs/research/2026-11-04-mutation-testing.md).
//! A child module of `inbox`, for its private `NotifyGate`.
use super::*;

fn at(secs: i64) -> acd_protocol::Timestamp {
    chrono::DateTime::from_timestamp(secs, 0).unwrap()
}

/// inbox.rs:28 `16 * 1024`: the typed-answer cap is 16 KiB.
#[test]
fn the_typed_answer_cap_is_16_kib() {
    assert_eq!(ANSWER_TEXT_MAX_BYTES, 16_384);
}

/// inbox.rs:157 `>=` -> `>` / `==` (`NotifyGate::admit`): a notification
/// exactly one window old no longer counts toward the rate limit.
#[test]
fn the_rate_window_ends_exactly_at_its_length() {
    for offset in [Duration::ZERO, Duration::from_nanos(1)] {
        let mut g = NotifyGate::default();
        let t0 = Instant::now();
        for i in 0..NOTIFY_RATE_MAX {
            assert!(g.admit("m", "id".into(), format!("r{i}"), at(1), t0));
        }
        assert!(!g.admit("m", "id".into(), "extra".into(), at(1), t0), "the limit holds inside the window");
        let later = t0 + NOTIFY_RATE_WINDOW + offset;
        assert!(g.admit("m", "id".into(), "later".into(), at(1), later), "{offset:?}");
    }
    // One nanosecond short of the window still counts.
    let mut g = NotifyGate::default();
    let t0 = Instant::now();
    for i in 0..NOTIFY_RATE_MAX {
        g.admit("m", "id".into(), format!("r{i}"), at(1), t0);
    }
    assert!(!g.admit("m", "id".into(), "early".into(), at(1), t0 + NOTIFY_RATE_WINDOW - Duration::from_nanos(1)));
}

/// inbox.rs:157 `>` -> `>=` / `==` / `<` (`NotifyGate::admit`): the history keeps
/// exactly `NOTIFIED_HISTORY_MAX` keys, so the oldest is forgotten only on the
/// one after that.
#[test]
fn the_history_holds_exactly_its_maximum() {
    let mut g = NotifyGate::default();
    let now = Instant::now();
    // A distinct machine per key keeps the rate limit out of the way.
    let admit = |g: &mut NotifyGate, i: usize| g.admit(&format!("machine-{i}"), "id".into(), format!("r{i}"), at(1), now);
    for i in 0..NOTIFIED_HISTORY_MAX {
        assert!(admit(&mut g, i));
    }
    assert!(!g.admit("again", "id".into(), "r0".into(), at(1), now), "still remembered at the limit");
    assert!(admit(&mut g, NOTIFIED_HISTORY_MAX));
    assert!(g.admit("again", "id".into(), "r0".into(), at(1), now), "the oldest is forgotten one past the limit");
    assert!(!g.admit("again", "id".into(), format!("r{NOTIFIED_HISTORY_MAX}"), at(1), now));
}
