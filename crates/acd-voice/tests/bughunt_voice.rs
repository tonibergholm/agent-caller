//! Bug-hunt reproducers (docs/research/2026-11-05-bug-hunt.md).

use acd_voice::text::{clean, truncate};

/// B4: `truncate(s, max)` is documented as "at most `max` characters"; at
/// `max == 0` it returns "…".
#[test]
#[ignore = "B4: voice truncate(_, 0) returns \"…\""]
fn truncate_never_exceeds_max() {
    assert_eq!(truncate("abc", 0).chars().count(), 0);
    assert_eq!(clean("abc def", 0).chars().count(), 0);
}
