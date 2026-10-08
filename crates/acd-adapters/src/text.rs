//! Text rules shared by adapters (spec §5 limits, §6.4 follow-ups).

pub const RAW_SCREEN_LINES: usize = 40;
pub const LAST_MESSAGE_CHARS: usize = 500;
pub const PROMPT_MAX_BYTES: usize = 16 * 1024;

/// Bidi controls and line/paragraph separators. Other format characters
/// (ZWJ, ZWNJ, variation selectors) appear in ordinary emoji and are allowed.
fn is_forbidden_format(c: char) -> bool {
    matches!(c, '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{2028}' | '\u{2029}')
}

pub fn check_prompt_text(text: &str) -> Result<(), &'static str> {
    if text.len() > PROMPT_MAX_BYTES {
        return Err("text longer than 16 KiB");
    }
    if text.trim().is_empty() {
        return Err("text is empty");
    }
    if text.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
        return Err("text contains control characters");
    }
    if text.chars().any(is_forbidden_format) {
        return Err("text contains bidi or separator characters");
    }
    Ok(())
}

/// Keeps the last `max` lines.
pub fn cap_lines(text: &str, max: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(max)..].join("\n")
}

/// Keeps the first `max` chars, ending in `…` when cut.
pub fn cap_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut s: String = text.chars().take(max.saturating_sub(1)).collect();
    s.push('…');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_text_rules() {
        assert!(check_prompt_text("fix the test\nthen push\tplease").is_ok());
        assert!(check_prompt_text("emoji 👩‍💻 with a ZWJ is fine").is_ok());
        for bad in ["a\u{1b}[31m", "a\u{7}", "a\u{202E}b", "a\u{2066}b", "a\u{200F}b", "a\u{061C}b", "a\u{2028}b", "a\u{2029}b", "a\rb"] {
            assert!(check_prompt_text(bad).is_err(), "{bad:?} must be rejected");
        }
        assert!(check_prompt_text(&"x".repeat(PROMPT_MAX_BYTES)).is_ok());
        assert!(check_prompt_text(&"x".repeat(PROMPT_MAX_BYTES + 1)).is_err());
        assert!(check_prompt_text("   \n ").is_err(), "blank text is rejected");
    }

    #[test]
    fn caps() {
        let screen: String = (1..=50).map(|i| format!("line {i}\n")).collect();
        let capped = cap_lines(&screen, RAW_SCREEN_LINES);
        assert_eq!(capped.lines().count(), 40);
        assert!(capped.starts_with("line 11") && capped.ends_with("line 50"));
        assert_eq!(cap_lines("a\nb", 40), "a\nb");
        let long = "é".repeat(600);
        let c = cap_chars(&long, LAST_MESSAGE_CHARS);
        assert_eq!(c.chars().count(), LAST_MESSAGE_CHARS);
        assert!(c.ends_with('…'));
        assert_eq!(cap_chars("short", 500), "short");
    }
}
