//! The herdr screen parser (herdr/screen.rs) and the herdr agent mapping.
//! The input is a screen, optionally followed by a NUL and a command line
//! for the Codex cancel check. A JSON object is read as a herdr agent too.
//! Invariants (fail closed): a dialog is only found when its footer is the
//! last non-empty line; any line added below it loses it; the fingerprint
//! is cut from the digest; the keys sent for Yes and No name real options.
#![no_main]
use acd_adapters::herdr::{
    map, screen::{self, DialogKind},
    wire::AgentInfo,
};
use acd_adapters::hooks::mapping::claude_permission_keys;
use acd_protocol::AgentKind;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut halves = data.splitn(2, |b| *b == 0);
    let text = String::from_utf8_lossy(halves.next().unwrap_or(&[])).into_owned();
    let command = String::from_utf8_lossy(halves.next().unwrap_or(&[])).into_owned();

    if let Ok(info) = serde_json::from_str::<AgentInfo>(&text) {
        if let Some(o) = map::agent_obs("h", &info) {
            assert!(!o.title.chars().any(char::is_control), "{:?}", o.title);
        }
    }
    let _ = screen::codex_cancel_tail(&text, &command);
    let generic = screen::generic_fingerprint(&text);
    for kind in [AgentKind::Claude, AgentKind::Codex, AgentKind::Other] {
        if let Some(line) = screen::prompt_line(kind, &text) {
            assert!(!line.contains('\n'));
            assert!(screen::find_dialog(&text).is_none(), "a prompt is read from a screen with a dialog");
        }
    }
    let found = screen::find_dialog(&text);
    assert_eq!(found, screen::find_dialog(&text), "find_dialog is not deterministic");
    let Some(d) = found else {
        assert!(screen::guard_digest(&text).is_none());
        assert_eq!(screen::fallback_fingerprint(&text), generic);
        return;
    };
    // Region and rows are consistent.
    assert!(d.option_rows.iter().all(|&i| i < d.region.len()), "option row outside the region");
    assert!(d.option_rows.windows(2).all(|w| w[0] < w[1]), "option rows out of order");
    assert!(!d.options.is_empty(), "a dialog without options");
    assert_eq!(d.options.len(), d.option_rows.len());
    // The digest and the fingerprint go together, except where the command is hidden.
    match d.kind {
        DialogKind::CodexCollapsed => assert!(d.digest.is_none() && d.fingerprint.is_none()),
        _ => {
            let digest = d.digest.expect("a matchable dialog has a digest");
            assert_eq!(d.fingerprint.as_ref(), Some(&digest.fingerprint()));
            assert_eq!(screen::guard_digest(&text), Some(digest));
            assert_eq!(screen::fallback_fingerprint(&text), digest.fingerprint());
            // Anything below the footer ends the match.
            let below = format!("{text}\nx");
            assert_ne!(screen::guard_digest(&below), Some(digest), "text below the footer still matches");
            let below = format!("{}\n\nx", text.trim_end());
            assert_ne!(screen::guard_digest(&below), Some(digest), "text below the footer still matches");
        }
    }
    if matches!(d.kind, DialogKind::ClaudePermission) {
        let keys = claude_permission_keys(&d);
        assert_eq!(keys["allow"], "1");
        let no = &keys["deny"];
        assert!(
            no == "esc" || d.options.iter().any(|o| &o.number.to_string() == no && o.label.trim() == "No"),
            "deny key {no:?} names no option"
        );
    }
});
