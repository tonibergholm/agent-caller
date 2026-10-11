//! Text rules shared by the daemon, its adapters and clients, so a typed
//! answer that one side would refuse is refused by every side.

/// Bidi controls and line/paragraph separators. Other format characters
/// (ZWJ, ZWNJ, variation selectors) appear in ordinary emoji and are allowed.
pub fn is_forbidden_format(c: char) -> bool {
    matches!(c, '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{2028}' | '\u{2029}')
}

/// Control characters other than newline and tab.
pub fn is_forbidden_control(c: char) -> bool {
    c.is_control() && c != '\n' && c != '\t'
}

/// Whether typed text holds a character no side should pass on (controls
/// other than `\n` and `\t`, bidi overrides, separators).
pub fn has_forbidden_chars(text: &str) -> bool {
    text.chars()
        .any(|c| is_forbidden_control(c) || is_forbidden_format(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newline_tab_and_emoji_joiners_pass() {
        assert!(!has_forbidden_chars("a\nb\tc 👩\u{200D}💻"));
    }

    #[test]
    fn controls_bidi_and_separators_fail() {
        for bad in ["a\u{1b}b", "a\rb", "a\u{202E}b", "a\u{2066}b", "a\u{2028}b"] {
            assert!(has_forbidden_chars(bad), "{bad:?}");
        }
    }
}
