//! The text rules: `has_forbidden_chars` (acd-protocol) and the adapter
//! rules built on it (prompt check, cleaning, caps, log redaction).
#![no_main]
use acd_adapters::text::{
    LOG_BYTES, PROMPT_MAX_BYTES, cap_chars, cap_lines, check_prompt_text, clean_display, clean_screen, redact_log,
};
use acd_protocol::text::has_forbidden_chars;
use libfuzzer_sys::fuzz_target;

/// Written again here: controls but `\n` and `\t`, bidi controls, separators.
fn forbidden(c: char) -> bool {
    (c.is_control() && c != '\n' && c != '\t')
        || matches!(c, '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{2028}' | '\u{2029}')
}

fn is_prefix(short: &str, long: &str) -> bool {
    long.starts_with(short)
}

fuzz_target!(|data: &[u8]| {
    let Some((&n, rest)) = data.split_first() else { return };
    let s = String::from_utf8_lossy(rest);
    let s = s.as_ref();
    let max = (n % 70) as usize;

    // acd-protocol: the shared rule is exactly "any forbidden character".
    assert_eq!(has_forbidden_chars(s), s.chars().any(forbidden));

    // The adapter prompt rule agrees with the protocol rule.
    let prompt = check_prompt_text(s);
    let expect_ok = s.len() <= PROMPT_MAX_BYTES && !s.trim().is_empty() && !has_forbidden_chars(s);
    assert_eq!(prompt.is_ok(), expect_ok, "check_prompt_text on {s:?}");

    // Display text: no control or bidi characters, bounded, idempotent.
    let shown = clean_display(s, max.max(1));
    assert!(!shown.chars().any(|c| c.is_control() || forbidden(c)), "{shown:?}");
    assert!(shown.chars().count() <= max.max(1));
    assert_eq!(clean_display(&shown, max.max(1)), shown);

    // Screen text keeps newlines only.
    let screen = clean_screen(s);
    assert!(!screen.chars().any(|c| (c.is_control() && c != '\n') || forbidden(c)));
    assert_eq!(clean_screen(&screen), screen);
    if !s.chars().any(|c| c.is_control() || forbidden(c)) {
        assert_eq!(screen, s);
    }

    // cap_chars: whole when it fits, else a prefix ending in an ellipsis.
    let capped = cap_chars(s, max);
    if s.chars().count() <= max {
        assert_eq!(capped, s);
    } else {
        assert!(capped.ends_with('…'));
        assert!(capped.chars().count() <= max.max(1));
        assert!(is_prefix(&capped[..capped.len() - '…'.len_utf8()], s));
    }
    let lines = cap_lines(s, max);
    assert!(lines.lines().count() <= max);

    // Log redaction: bounded, no raw control characters, and a secret placed
    // after a known indicator never survives.
    let logged = redact_log(s);
    assert!(logged.len() <= LOG_BYTES + '…'.len_utf8());
    assert!(!logged.chars().any(|c| (c.is_control() && c != '\n' && c != '\t') || forbidden(c)));
    let x: String = s.chars().filter(|c| c.is_ascii_alphanumeric()).take(40).collect();
    if x.len() >= 8 && !"[REDACTED]".contains(x.as_str()) {
        for prefix in ["Bearer ", "token=", "password: ", "{\"api_key\":\"", "sk-", "MY_VAR="] {
            let noise: String = s.chars().filter(|c| *c != '\n').take(40).collect();
            let line = format!("{noise}\n{prefix}{x}");
            // The noise alone, as logged (format characters removed), must not hold it.
            if redact_log(&noise).contains(x.as_str()) {
                continue;
            }
            let out = redact_log(&line);
            assert!(!out.contains(x.as_str()), "{prefix:?} secret survived: {out:?}");
        }
    }
});
