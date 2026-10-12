//! Voice content and agent-text helpers.
use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Text the user spoke, or text derived from it: transcripts, payloads,
/// model output and spoken replies. Spec §3.7: it never reaches logs,
/// `Debug` output or error strings. `Debug` shows only the length, and
/// there is no `Display`; read it with `expose`.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct VoiceText(String);

impl VoiceText {
    pub fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
    pub fn char_len(&self) -> usize {
        self.0.chars().count()
    }
}

impl fmt::Debug for VoiceText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VoiceText({} chars)", self.char_len())
    }
}

/// Agent or voice text in a `Debug` impl: shows its length only (spec
/// §3.7), so a derived `Debug` above it cannot print the text.
pub struct Redacted<'a>(pub &'a str);

impl Redacted<'_> {
    pub fn opt(s: &Option<String>) -> Option<Redacted<'_>> {
        s.as_deref().map(Redacted)
    }
}

impl fmt::Debug for Redacted<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<{} chars>", self.0.chars().count())
    }
}

/// Cuts `s` to at most `max` characters on a character boundary, ending
/// with "…" when anything was cut.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Agent-supplied text made safe to quote or speak: whitespace, control,
/// bidi and separator characters become single spaces, the ends are
/// trimmed, and the result is cut to `max` characters.
pub fn clean(s: &str, max: usize) -> String {
    let mut out = String::with_capacity(s.len().min(max.saturating_mul(4)));
    let mut gap = false;
    for c in s.chars() {
        let blank = c.is_whitespace()
            || acd_protocol::text::is_forbidden_control(c)
            || acd_protocol::text::is_forbidden_format(c);
        if blank {
            gap = !out.is_empty();
            continue;
        }
        if gap {
            out.push(' ');
            gap = false;
        }
        out.push(c);
    }
    truncate(&out, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_shows_only_the_length() {
        let t = VoiceText::new("approve the deploy to prod");
        assert_eq!(format!("{t:?}"), "VoiceText(26 chars)");
        assert_eq!(t.expose(), "approve the deploy to prod");
    }

    #[test]
    fn redacted_shows_only_the_length() {
        assert_eq!(format!("{:?}", Redacted("SECRET")), "<6 chars>");
        assert_eq!(format!("{:?}", Redacted::opt(&Some("é".into()))), "Some(<1 chars>)");
    }

    #[test]
    fn serializes_as_a_plain_string() {
        let t = VoiceText::new("run the tests");
        assert_eq!(serde_json::to_string(&t).unwrap(), "\"run the tests\"");
        let back: VoiceText = serde_json::from_str("\"x\"").unwrap();
        assert_eq!(back.expose(), "x");
    }

    #[test]
    fn truncate_counts_characters_not_bytes() {
        let t = truncate(&"é".repeat(200), 120);
        assert_eq!(t.chars().count(), 120);
        assert!(t.ends_with('…'));
        assert_eq!(truncate("short", 120), "short");
    }

    #[test]
    fn clean_turns_controls_bidi_and_newlines_into_single_spaces() {
        assert_eq!(clean("  a\n\tb\u{202E}c\u{1b}d  ", 120), "a b c d");
        assert_eq!(clean("\"}]\n### INSTRUCTIONS", 120), "\"}] ### INSTRUCTIONS");
        assert_eq!(clean(&"x".repeat(100), 60).chars().count(), 60);
    }
}
