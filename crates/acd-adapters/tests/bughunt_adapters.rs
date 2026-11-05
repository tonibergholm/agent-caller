//! Bug-hunt reproducers (docs/research/2026-11-05-bug-hunt.md). Tests marked
//! `#[ignore]` fail on current code; each names the bug it proves.

use acd_adapters::hooks::wire::{claude_key, codex_key, Payload};
use acd_adapters::text::cap_chars;
use serde_json::json;

fn payload(session: &str, prompt: Option<&str>, turn: Option<&str>) -> Payload {
    Payload::from_value(&json!({
        "session_id": session,
        "prompt_id": prompt,
        "turn_id": turn,
        "tool_name": "Bash",
        "tool_input": {"command": "ls"},
    }))
    .unwrap()
}

/// B1: the correlation key joins ids with `:` but ids may contain `:`, so
/// two different (session, prompt) pairs give one key.
#[test]
#[ignore = "B1: claude_key/codex_key are ambiguous when an id contains ':'"]
fn correlation_keys_of_different_sessions_differ() {
    let a = payload("s:p1", Some("x"), Some("x"));
    let b = payload("s", Some("p1:x"), Some("p1:x"));
    assert_ne!(claude_key(&a), claude_key(&b), "claude");
    assert_ne!(codex_key(&a), codex_key(&b), "codex");
}

/// B2: `cap_chars(text, max)` is documented as "the first `max` chars"; at
/// `max == 0` it returns a one-char string.
#[test]
#[ignore = "B2: cap_chars(_, 0) returns \"…\" (1 char over the cap)"]
fn cap_chars_never_exceeds_its_cap() {
    for max in 0..3 {
        let out = cap_chars("abcdef", max);
        assert!(out.chars().count() <= max, "max {max}: {out:?}");
    }
}
