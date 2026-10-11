//! Text rules shared by adapters (spec §5 limits, §6.4 follow-ups).

use acd_protocol::text::{is_forbidden_control, is_forbidden_format};
use regex::Regex;
use std::sync::LazyLock;

pub const RAW_SCREEN_LINES: usize = 40;
pub const LAST_MESSAGE_CHARS: usize = 500;
pub const PROMPT_MAX_BYTES: usize = 16 * 1024;
pub const TITLE_CHARS: usize = 200;

pub fn check_prompt_text(text: &str) -> Result<(), &'static str> {
    if text.len() > PROMPT_MAX_BYTES {
        return Err("text longer than 16 KiB");
    }
    if text.trim().is_empty() {
        return Err("text is empty");
    }
    if text.chars().any(is_forbidden_control) {
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

static SECRET_INDICATORS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        Regex::new(r"(?i)\b(?:[a-z0-9_]*(?:token|secret|password|passwd|api_?key|key|auth|authorization|ticket|credential)[a-z0-9_]*)\s*[:=]\s*").unwrap(),
        Regex::new(r"\b[A-Z][A-Z0-9_]*=").unwrap(),
        Regex::new(r#"(?i)(?:\\"|\")?[a-z0-9_]*(?:token|secret|password|passwd|api_?key|key|auth|authorization|ticket|credential)[a-z0-9_]*(?:\\"|\")?\s*:\s*(?:String\()?"#).unwrap(),
        Regex::new(r"(?i)\b(?:token|ticket|wsticket)=").unwrap(),
    ]
});
static BEARER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)bearer\s+[A-Za-z0-9._~+/=-]+").unwrap());
static KNOWN_PREFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bsk-[A-Za-z0-9_-]{6,}").unwrap());

/// Redacts secrets (spec §7.1, Logs) and caps the result at 4 KiB. Callers
/// should log JSON via `serde_json::to_string` where possible; Debug and
/// debug-escaped forms are covered too but are best effort.
pub fn redact_log(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_control() && c != '\n' && c != '\t' {
            out.push_str(&format!(r"\u{{{:x}}}", c as u32));
        } else if !is_forbidden_format(c) && !is_format_character(c) {
            out.push(c);
        }
    }
    let mut starts: Vec<usize> = BEARER.find_iter(&out).map(|m| m.start()).collect();
    starts.extend(KNOWN_PREFIX.find_iter(&out).map(|m| m.start()));
    for re in SECRET_INDICATORS.iter() {
        starts.extend(re.find_iter(&out).map(|m| m.end()));
    }
    starts.sort_unstable();
    starts.dedup();
    let mut redacted = String::with_capacity(out.len());
    let mut copied = 0;
    let mut masked_until = 0;
    for start in starts {
        if start < masked_until || start >= out.len() {
            continue;
        }
        let line_end = out[start..]
            .find('\n')
            .map_or(out.len(), |offset| start + offset);
        redacted.push_str(&out[copied..start]);
        redacted.push_str("[REDACTED]");
        copied = line_end;
        masked_until = line_end;
    }
    redacted.push_str(&out[copied..]);
    let mut out = redacted;
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

pub(crate) fn is_format_character(c: char) -> bool {
    matches!(c as u32,
        0x00AD | 0x0600..=0x0605 | 0x061C | 0x06DD | 0x070F | 0x0890..=0x0891 |
        0x08E2 | 0x180E | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2064 |
        0x2066..=0x206F | 0xFEFF | 0xFFF9..=0xFFFB | 0x110BD | 0x110CD |
        0x13430..=0x1343F | 0x1BCA0..=0x1BCA3 | 0x1D173..=0x1D17A |
        0xE0001 | 0xE0020..=0xE007F)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_log_masks_any_secret_like_json_key() {
        for input in [
            r#"{"password":"hunter2 x","next":1}"#,
            r#"{"api_key":"hunter2 x"}"#,
            r#"{"client_secret":"hunter2 x"}"#,
            r#"{\"Password\":\"hunter2 x\"}"#,
            r#""credential": String("hunter2 x")"#,
        ] {
            let out = redact_log(input);
            assert!(
                !out.contains("hunter2") && !out.contains(" x"),
                "leaked: {input} -> {out}"
            );
        }
        assert_eq!(redact_log(r#"{"name":"ok"}"#), r#"{"name":"ok"}"#);
    }

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
        assert!(r.contains("Authorization: [REDACTED]"), "{r}");
        let long = "é".repeat(5000);
        let r = redact_log(&long);
        assert!(r.len() <= 4096 + '…'.len_utf8());
        assert!(r.ends_with('…'));
    }

    #[test]
    fn redact_log_masks_secret_tails_to_newline() {
        let cases = [
            (r#"TOKEN=\"first \\\" second\""#, &["first", "second"][..]),
            (r#"FOO='first'\"second\""#, &["first", "second"][..]),
            (r#"FOO=\"first second"#, &["first", "second"][..]),
            ("TOKEN: a b c", &["a", "b", "c"][..]),
            (r#"{"token":"x y","next":1}"#, &["x", "y"][..]),
            ("Authorization: Bearer abc def", &["abc", "def"][..]),
            ("TO\u{200b}KEN: split secret", &["split", "secret"][..]),
        ];
        for (input, words) in cases {
            let output = redact_log(input);
            for word in words {
                assert!(
                    !output.contains(word),
                    "{input:?} leaked {word:?}: {output:?}"
                );
            }
        }
        assert_eq!(
            redact_log("TOKEN=hidden\npublic text"),
            "TOKEN=[REDACTED]\npublic text"
        );
        assert_eq!(
            redact_log("plain text without secrets"),
            "plain text without secrets"
        );
    }

    #[test]
    fn redact_log_covers_json_forms() {
        for (s, secret) in [
            (r#"{"token": "s3cret"}"#, "s3cret"),
            (r#"{"Token":"s3cret"}"#, "s3cret"),
            (r#"{"WSTICKET":"s3cret"}"#, "s3cret"),
            (r#""token": String("s3cret")"#, "s3cret"),
            (r#"{\"token\":\"s3cret\"}"#, "s3cret"),
            (r#"{"token":"ab\"cd"}"#, "cd"),
            (r#"{"token":"ab\"cd","x":1}"#, "ab"),
        ] {
            let r = redact_log(s);
            assert!(!r.contains(secret), "{s} leaked {secret}: {r}");
            assert!(r.contains("[REDACTED]"), "{s}: {r}");
        }
        let r = redact_log(r#"{"token":"ab\"cd","x":1}"#);
        assert!(!r.contains("x"), "same-line tail survives: {r}");
        for (s, secret) in [
            ("token=abc&x=1", "abc"),
            ("Ticket=abc", "abc"),
            ("WSTICKET=abc", "abc"),
            ("API_TOKEN=\"first second\"", "first second"),
            ("AUTH_TOKEN: value here", "value"),
        ] {
            let redacted = redact_log(s);
            assert!(!redacted.contains(secret), "{s} leaked: {redacted}");
        }
        let bidi = redact_log("safe\u{202e}text\u{2028}tail");
        assert_eq!(bidi, "safetexttail");
    }

    #[test]
    fn redact_log_normalizes_and_masks_all_assignment_forms() {
        for (input, fragments) in [
            ("TOKEN=\"first second\"", &["first", "second"][..]),
            ("API_TOKEN=\"first second\"", &["first", "second"][..]),
            ("FOO=secret", &["secret"][..]),
            ("TOKEN: secret", &["secret"][..]),
            ("token=secret", &["secret"][..]),
            ("to\u{200b}ken=secret", &["secret"][..]),
            ("to\u{202e}ken=secret", &["secret"][..]),
        ] {
            let out = redact_log(input);
            for fragment in fragments {
                assert!(
                    !out.contains(fragment),
                    "{input:?} leaked {fragment:?}: {out:?}"
                );
            }
        }
        let escaped = redact_log("before\u{1b}[31m TOKEN=secret");
        assert!(escaped.contains(r"\u{1b}"), "{escaped:?}");
        assert!(
            !escaped.contains('\u{1b}'),
            "raw ESC reached output: {escaped:?}"
        );
        assert!(!escaped.contains("secret"), "{escaped:?}");
        assert_eq!(
            redact_log("ordinary text\nwith\ttabs\rhere"),
            "ordinary text\nwith\ttabs\\u{d}here"
        );
    }
}
