//! Text rules shared by adapters (spec §5 limits, §6.4 follow-ups).

use regex::Regex;
use std::sync::LazyLock;

pub const RAW_SCREEN_LINES: usize = 40;
pub const LAST_MESSAGE_CHARS: usize = 500;
pub const PROMPT_MAX_BYTES: usize = 16 * 1024;
pub const TITLE_CHARS: usize = 200;

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
    if text
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err("text contains control characters");
    }
    if text.chars().any(is_forbidden_format) {
        return Err("text contains bidi or separator characters");
    }
    Ok(())
}

/// Keeps the last `max` lines. Drops a trailing newline and normalizes CRLF to LF.
pub fn cap_lines(text: &str, max: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(max)..].join("\n")
}

/// Strips every control and bidi/separator char (terminal title sequences are
/// program-controlled), trims, then caps at `max_chars`.
pub fn clean_display(s: &str, max_chars: usize) -> String {
    let cleaned: String = s
        .chars()
        .filter(|&c| !c.is_control() && !is_forbidden_format(c))
        .collect();
    cap_chars(cleaned.trim(), max_chars)
}

/// Screen text for display: keeps newlines, drops every other control
/// character and the bidi/separator set.
pub fn clean_screen(s: &str) -> String {
    s.chars()
        .filter(|&c| c == '\n' || !(c.is_control() || is_forbidden_format(c)))
        .collect()
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

pub const LOG_BYTES: usize = 4096;

static SECRETS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    vec![
        (
            Regex::new(r"(?i)(bearer\s+)[A-Za-z0-9._~+/=-]+").unwrap(),
            "${1}[REDACTED]",
        ),
        (
            Regex::new(r#"("(?:ticket|token|wsTicket|authorization)"\s*:\s*")[^"]*"#).unwrap(),
            "${1}[REDACTED]",
        ),
        (Regex::new(r"(wsTicket=)[^&\s]+").unwrap(), "${1}[REDACTED]"),
        (Regex::new(r"\bsk-[A-Za-z0-9_-]{6,}").unwrap(), "[REDACTED]"),
        (
            Regex::new(r"\b([A-Z][A-Z0-9_]{2,}=)[^\s&]+").unwrap(),
            "${1}[REDACTED]",
        ),
    ]
});

/// Redacts secrets (spec §7.1, Logs) and caps the result at 4 KiB.
pub fn redact_log(s: &str) -> String {
    let mut out = s.to_string();
    for (re, rep) in SECRETS.iter() {
        out = re.replace_all(&out, *rep).into_owned();
    }
    if out.len() <= LOG_BYTES {
        return out;
    }
    let mut end = LOG_BYTES;
    while !out.is_char_boundary(end) {
        end -= 1;
    }
    out.truncate(end);
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_text_rules() {
        assert!(check_prompt_text("fix the test\nthen push\tplease").is_ok());
        assert!(check_prompt_text("emoji 👩‍💻 with a ZWJ is fine").is_ok());
        for bad in [
            "a\u{1b}[31m",
            "a\u{7}",
            "a\u{202E}b",
            "a\u{2066}b",
            "a\u{200F}b",
            "a\u{061C}b",
            "a\u{2028}b",
            "a\u{2029}b",
            "a\rb",
        ] {
            assert!(check_prompt_text(bad).is_err(), "{bad:?} must be rejected");
        }
        assert!(check_prompt_text(&"x".repeat(PROMPT_MAX_BYTES)).is_ok());
        assert!(check_prompt_text(&"x".repeat(PROMPT_MAX_BYTES + 1)).is_err());
        assert!(
            check_prompt_text("   \n ").is_err(),
            "blank text is rejected"
        );
    }

    #[test]
    fn clean_display_strips_controls_and_bidi() {
        assert_eq!(
            clean_display("\u{1b}]0;evil\u{7}fix\u{202E}tests\n", 200),
            "]0;evilfixtests"
        );
        assert_eq!(clean_display("  a\tb  ", 200), "ab");
        let c = clean_display(&"x".repeat(300), TITLE_CHARS);
        assert_eq!(c.chars().count(), TITLE_CHARS);
        assert!(c.ends_with('…'));
        assert_eq!(
            clean_display("/Users/u/src/app", usize::MAX),
            "/Users/u/src/app"
        );
    }

    #[test]
    fn clean_screen_keeps_newlines_and_strips_the_rest() {
        assert_eq!(
            clean_screen("a\u{1b}[31mb\n\u{202E}c\td\r\ne\u{2028}"),
            "a[31mb\ncd\ne"
        );
        assert_eq!(clean_screen("👩‍💻\nok"), "👩‍💻\nok");
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

    #[test]
    fn redact_log_hides_secrets_and_truncates() {
        let s = r#"Authorization: Bearer abc.def-ghi {"ticket":"t0p","token":"s3cret"} sk-ant-api03-XYZ123 GITHUB_TOKEN=ghp_1 path=/tmp"#;
        let r = redact_log(s);
        for secret in ["abc.def-ghi", "t0p", "s3cret", "XYZ123", "ghp_1"] {
            assert!(!r.contains(secret), "{secret} leaked: {r}");
        }
        assert!(
            r.contains("path=/tmp"),
            "lower-case keys are not secrets: {r}"
        );
        let long = "é".repeat(5000);
        let r = redact_log(&long);
        assert!(r.len() <= 4096 + '…'.len_utf8());
        assert!(r.ends_with('…'));
    }
}
