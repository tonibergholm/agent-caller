//! Screen parsing for herdr panes (spec §6.2, §6.3): dialog region,
//! fingerprints and the input-box prompt line. Pure functions over text;
//! the rules are derived from the recorded screens in tests/fixtures/herdr.
//!
//! Threat model: a dialog fingerprint guards a keystroke into an approval
//! dialog, and the command/detail text inside a dialog is attacker-influenced
//! (model output). Dialog normalization is therefore strict: it never merges
//! distinct content, and anything ambiguous fails closed (no dialog, no
//! fingerprint).
use acd_protocol::AgentKind;
use sha2::{Digest, Sha256};

/// 16 lowercase hex chars of `sha256` over normalized screen text.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Fingerprint(pub String);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DialogKind {
    ClaudePermission,
    ClaudeQuestion { tabs: bool },
    CodexPermission,
    CodexCollapsed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialogOption {
    pub number: u8,
    pub label: String,
    /// Codex key suffix such as `y`, `p` or `esc`.
    pub key_hint: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dialog {
    pub kind: DialogKind,
    pub options: Vec<DialogOption>,
    /// The normalized region.
    pub text: String,
    /// `None` for `CodexCollapsed`: the command text is hidden, so the
    /// dialog can never be matched (spec §6.3).
    pub fingerprint: Option<Fingerprint>,
}

const CLAUDE_FOOTER: &str = "Esc to cancel";
const CODEX_FOOTER: &str = "Press enter to confirm or esc to cancel";
const CLAUDE_SCAN_LIMIT: usize = 60;
const GENERIC_FINGERPRINT_LINES: usize = 40;
const CODEX_PROMPT_WINDOW: usize = 6;

/// A rule line: at least 10 chars, all `─` or `╌`.
fn is_rule(line: &str) -> bool {
    let t = line.trim();
    t.chars().count() >= 10 && t.chars().all(|c| c == '─' || c == '╌')
}

/// A solid `─` rule (the box edges Claude draws around dialogs and the input).
fn is_solid_rule(line: &str) -> bool {
    is_rule(line) && !line.contains('╌')
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

fn strip_cursor(t: &str) -> &str {
    t.strip_prefix('❯')
        .or_else(|| t.strip_prefix('›'))
        .map_or(t, str::trim_start)
}

fn path_normalized(s: &str) -> String {
    s.replace("/private/tmp/", "/tmp/")
        .replace("/private/var/", "/var/")
}

/// Dialog normalization (fingerprints and `Dialog::text`). Strict on purpose:
/// - lines are trimmed at both ends only, internal whitespace is kept;
/// - empty lines and rule lines are dropped;
/// - a leading cursor marker is stripped only on option lines;
/// - `Tip:` lines are dropped only in the header (lines before `header_end`);
///   a `Tip:` line inside the command/detail counts as content;
/// - lines are joined with `\n`, so a re-wrap changes the result (a safe
///   mismatch);
/// - directory paths are normalized (spec §6.2).
fn normalize_dialog(region: &[&str], option_idx: &[usize], header_end: usize) -> String {
    let mut out = Vec::new();
    for (i, line) in region.iter().enumerate() {
        let t = line.trim();
        if t.is_empty() || is_rule(t) {
            continue;
        }
        if i < header_end && t.starts_with("Tip:") {
            continue;
        }
        out.push(if option_idx.contains(&i) {
            strip_cursor(t)
        } else {
            t
        });
    }
    path_normalized(&out.join("\n"))
}

/// Tolerant normalization for the generic read-only fingerprint only: it keys
/// read-only requests and must survive cursor moves, rotating tips and
/// re-wraps. Kept separate from `normalize_dialog` so the strict dialog rules
/// cannot be loosened by accident. Never use it for the keys guard.
fn normalize_generic(lines: &[&str]) -> String {
    let joined = lines
        .iter()
        .filter_map(|l| generic_line(l))
        .collect::<Vec<_>>()
        .join(" ");
    path_normalized(&joined)
}

fn generic_line(line: &str) -> Option<String> {
    let collapsed = strip_cursor(line.trim())
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if collapsed.is_empty() || is_rule(&collapsed) || collapsed.starts_with("Tip:") {
        return None;
    }
    Some(collapsed)
}

fn fingerprint_of(normalized: &str) -> Fingerprint {
    let digest = Sha256::digest(normalized.as_bytes());
    Fingerprint(digest.iter().take(8).map(|b| format!("{b:02x}")).collect())
}

/// Parses `[marker] N. rest`. Returns the number and the text after `N. `.
fn parse_numbered(line: &str, marker: char) -> Option<(u8, &str)> {
    let t = line.trim_start();
    let t = t.strip_prefix(marker).map_or(t, str::trim_start);
    let digits = t.len() - t.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 {
        return None;
    }
    let number = t[..digits].parse().ok()?;
    let rest = t[digits..].strip_prefix('.')?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let rest = rest.trim();
    (!rest.is_empty()).then_some((number, rest))
}

fn claude_option(line: &str) -> Option<DialogOption> {
    let (number, label) = parse_numbered(line, '❯')?;
    Some(DialogOption {
        number,
        label: label.to_string(),
        key_hint: None,
    })
}

fn codex_option(line: &str) -> Option<DialogOption> {
    let (number, rest) = parse_numbered(line, '›')?;
    // A trailing `(hint)` with no `)` inside, e.g. `(y)`, `(p)`, `(esc)`.
    if let Some(body) = rest.strip_suffix(')')
        && let Some((label, hint)) = body.rsplit_once(" (")
        && !hint.is_empty()
        && !hint.contains(')')
    {
        return Some(DialogOption {
            number,
            label: label.trim().to_string(),
            key_hint: Some(hint.to_string()),
        });
    }
    Some(DialogOption {
        number,
        label: rest.to_string(),
        key_hint: None,
    })
}

/// The contiguous options block directly above the footer (the last region
/// line), as `(region index, option)` in screen order. Scanning upward it
/// skips blank and rule lines and description lines (indented deeper than the
/// option above them, directly under it), collects numbered lines and stops
/// at the first other line. Numbers must run consecutively down to 1, so a
/// numbered line inside a command above the block is never an option. Empty
/// when the block is not well formed.
fn options_block(
    region: &[&str],
    parse: fn(&str) -> Option<DialogOption>,
) -> Vec<(usize, DialogOption)> {
    let mut found: Vec<(usize, DialogOption)> = Vec::new();
    let mut pending: Vec<usize> = Vec::new(); // indents of candidate description lines
    for i in (0..region.len().saturating_sub(1)).rev() {
        let line = region[i];
        let t = line.trim();
        if t.is_empty() || is_rule(t) {
            if !pending.is_empty() {
                break;
            }
            continue;
        }
        if let Some(opt) = parse(line) {
            let ind = indent(line);
            let sequential = found
                .last()
                .is_none_or(|(_, below)| u16::from(opt.number) + 1 == u16::from(below.number));
            if pending.iter().any(|&p| p <= ind) || !sequential {
                break;
            }
            pending.clear();
            found.push((i, opt));
        } else if found.is_empty() {
            break;
        } else {
            pending.push(indent(line));
        }
    }
    found.reverse();
    if found.first().is_some_and(|(_, o)| o.number == 1) {
        found
    } else {
        Vec::new()
    }
}

/// Detects a Claude or Codex dialog anywhere in the text.
pub fn find_dialog(text: &str) -> Option<Dialog> {
    let lines: Vec<&str> = text.lines().collect();
    find_claude(&lines).or_else(|| find_codex(&lines))
}

/// Builds a dialog from a region. The header (where `Tip:` lines are
/// ignored) ends at the first inner rule or the first option, whichever
/// comes first.
fn make_dialog(
    kind: DialogKind,
    block: Vec<(usize, DialogOption)>,
    region: &[&str],
    fingerprint: bool,
) -> Dialog {
    let option_idx: Vec<usize> = block.iter().map(|(i, _)| *i).collect();
    let first_rule = (1..region.len())
        .find(|&i| is_rule(region[i]))
        .unwrap_or(region.len());
    let header_end = first_rule.min(option_idx.first().copied().unwrap_or(0));
    let text = normalize_dialog(region, &option_idx, header_end);
    let fingerprint = fingerprint.then(|| fingerprint_of(&text));
    let options = block.into_iter().map(|(_, o)| o).collect();
    Dialog {
        kind,
        options,
        text,
        fingerprint,
    }
}

/// Claude: the footer is the last line containing `Esc to cancel`. Scan up
/// from it, at most 60 lines, and stop at a line starting with `⏺` or `✻`,
/// or with `❯` followed by anything but `N.` (the user's earlier prompt).
/// The region runs from the topmost solid `─` rule seen before the stop
/// through the footer. No rule or no well-formed options block means no
/// dialog.
fn find_claude(lines: &[&str]) -> Option<Dialog> {
    let footer = lines.iter().rposition(|l| l.contains(CLAUDE_FOOTER))?;
    let mut top = None;
    for i in (footer.saturating_sub(CLAUDE_SCAN_LIMIT)..footer).rev() {
        let t = lines[i].trim();
        if t.starts_with('⏺')
            || t.starts_with('✻')
            || (t.starts_with('❯') && claude_option(t).is_none())
        {
            break;
        }
        if is_solid_rule(t) {
            top = Some(i);
        }
    }
    let region = &lines[top?..=footer];
    let block = options_block(region, claude_option);
    if block.is_empty() {
        return None;
    }
    let question = region.iter().find(|l| l.contains('☐'));
    let kind = match question {
        Some(l) => DialogKind::ClaudeQuestion {
            tabs: l.trim().starts_with('←'),
        },
        None => DialogKind::ClaudePermission,
    };
    Some(make_dialog(kind, block, region, true))
}

/// Codex: the footer is the last line containing `Press enter to confirm or
/// esc to cancel`; the region starts at the last `Would you like to` above
/// it. Without a footer, a `[… N lines] ctrl+a view all` line followed by an
/// option line is a collapsed dialog (command text hidden, no fingerprint).
fn find_codex(lines: &[&str]) -> Option<Dialog> {
    if let Some(footer) = lines.iter().rposition(|l| l.contains(CODEX_FOOTER)) {
        let start = lines[..footer]
            .iter()
            .rposition(|l| l.contains("Would you like to"))?;
        let region = &lines[start..=footer];
        let block = options_block(region, codex_option);
        if block.is_empty() {
            return None;
        }
        return Some(make_dialog(
            DialogKind::CodexPermission,
            block,
            region,
            true,
        ));
    }
    let marker = lines.iter().rposition(|l| is_collapsed_marker(l))?;
    let mut block = Vec::new();
    for (i, line) in lines.iter().enumerate().skip(marker + 1) {
        if line.trim().is_empty() {
            continue;
        }
        match codex_option(line) {
            Some(o) => block.push((i - marker, o)),
            None => break,
        }
    }
    let last = block.last()?.0;
    Some(make_dialog(
        DialogKind::CodexCollapsed,
        block,
        &lines[marker..=marker + last],
        false,
    ))
}

/// Matches `[… N lines] ctrl+a view all`.
fn is_collapsed_marker(line: &str) -> bool {
    let t = line.trim();
    let Some(inner) = t.strip_prefix("[…") else {
        return false;
    };
    let Some(count) = inner.trim_start().strip_suffix("lines] ctrl+a view all") else {
        return false;
    };
    let count = count.trim();
    !count.is_empty() && count.chars().all(|c| c.is_ascii_digit())
}

/// The fingerprint a keystroke guard may trust: the dialog's own, or `None`.
/// The keys guard must use this, never `fallback_fingerprint`.
pub fn guard_fingerprint(text: &str) -> Option<Fingerprint> {
    find_dialog(text).and_then(|d| d.fingerprint)
}

/// For the read-only fallback only: the dialog fingerprint when there is one,
/// otherwise a tolerant generic fingerprint of the last 40 non-empty
/// normalized lines. Not safe as a keystroke guard (see `guard_fingerprint`).
pub fn fallback_fingerprint(text: &str) -> Fingerprint {
    if let Some(fp) = guard_fingerprint(text) {
        return fp;
    }
    let kept: Vec<&str> = text.lines().filter(|l| generic_line(l).is_some()).collect();
    let tail = &kept[kept.len().saturating_sub(GENERIC_FINGERPRINT_LINES)..];
    fingerprint_of(&normalize_generic(tail))
}

/// The content of the agent's input box (after the `❯`/`›` marker, trimmed).
/// `None` while a dialog is showing, when no input box is found, when the
/// candidate line is really a numbered option of an unrecognized dialog, or
/// for `AgentKind::Other`.
///
/// Claude: the last `❯` line whose previous and next non-empty lines are
/// both `─` rules. Codex: the last `› ` line among the last 6 non-empty lines.
pub fn prompt_line(kind: AgentKind, text: &str) -> Option<String> {
    if find_dialog(text).is_some() {
        return None;
    }
    let non_empty: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    match kind {
        AgentKind::Claude => (1..non_empty.len().saturating_sub(1)).rev().find_map(|i| {
            let content = non_empty[i].trim().strip_prefix('❯')?;
            (is_solid_rule(non_empty[i - 1]) && is_solid_rule(non_empty[i + 1]))
                .then(|| content.trim().to_string())
        }),
        AgentKind::Codex => {
            let window = &non_empty[non_empty.len().saturating_sub(CODEX_PROMPT_WINDOW)..];
            let content = window
                .iter()
                .rev()
                .find_map(|l| l.strip_prefix('›'))?
                .trim();
            // `› 1. Trust and continue` is an option of a dialog we do not
            // recognize, not input-box content.
            parse_numbered(content, '›')
                .is_none()
                .then(|| content.to_string())
        }
        AgentKind::Other => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acd_protocol::AgentKind;

    fn fx(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/../../tests/fixtures/herdr/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    #[test]
    fn claude_permission() {
        let d = find_dialog(&fx("claude-permission-bash.txt")).unwrap();
        assert_eq!(d.kind, DialogKind::ClaudePermission);
        let labels: Vec<_> = d
            .options
            .iter()
            .map(|o| (o.number, o.label.as_str()))
            .collect();
        assert_eq!(labels[0], (1, "Yes"));
        assert_eq!(labels[3], (4, "No"));
        assert!(d.text.contains("touch /tmp/acp0/t1.txt"));
        assert!(!d.text.contains("Tip:"));
        assert!(d.fingerprint.is_some());
    }

    #[test]
    fn fingerprint_ignores_cursor_tip_and_wrapping_but_not_content() {
        let s = fx("claude-permission-bash.txt");
        let fp = find_dialog(&s).unwrap().fingerprint.unwrap();
        let moved = s
            .replace(" ❯ 1. Yes", "   1. Yes")
            .replace("   4. No", " ❯ 4. No");
        assert_eq!(
            find_dialog(&moved).unwrap().fingerprint.unwrap(),
            fp,
            "cursor position is not content"
        );
        let tip = s.replace(
            "Tip: auto mode handles these prompts for you",
            "Tip: press ? for shortcuts",
        );
        assert_eq!(
            find_dialog(&tip).unwrap().fingerprint.unwrap(),
            fp,
            "tips rotate"
        );
        let private = s.replace(
            "always allow access to /tmp/acp0",
            "always allow access to /private/tmp/acp0",
        );
        assert_eq!(
            find_dialog(&private).unwrap().fingerprint.unwrap(),
            fp,
            "§6.2 path normalization"
        );
        let other = s.replace("touch /tmp/acp0/t1.txt", "rm -rf /tmp/acp0");
        assert_ne!(
            find_dialog(&other).unwrap().fingerprint.unwrap(),
            fp,
            "the command is content"
        );
    }

    #[test]
    fn claude_questions() {
        let single = find_dialog(&fx("claude-askuserquestion-single.txt")).unwrap();
        assert_eq!(single.kind, DialogKind::ClaudeQuestion { tabs: false });
        assert_eq!(
            single
                .options
                .iter()
                .map(|o| o.label.as_str())
                .collect::<Vec<_>>(),
            vec!["Red", "Blue", "Type something.", "Chat about this"]
        );
        let multi = find_dialog(&fx("claude-askuserquestion-multi.txt")).unwrap();
        assert_eq!(multi.kind, DialogKind::ClaudeQuestion { tabs: true });
        assert_ne!(single.fingerprint, multi.fingerprint);
    }

    #[test]
    fn codex_permission_and_collapsed() {
        let d = find_dialog(&fx("codex-permission-bash-detection.txt")).unwrap();
        assert_eq!(d.kind, DialogKind::CodexPermission);
        assert_eq!(
            d.options
                .iter()
                .map(|o| o.key_hint.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("y"), Some("p"), Some("esc")]
        );
        assert!(d.text.contains("touch ~/acp0_test_t5_1.txt"));
        let c = find_dialog(&fx("codex-permission-collapsed-small-pane-detection.txt")).unwrap();
        assert_eq!(c.kind, DialogKind::CodexCollapsed);
        assert!(
            c.fingerprint.is_none(),
            "a collapsed dialog can never be matched"
        );
    }

    #[test]
    fn unknown_dialogs_use_the_generic_fingerprint() {
        let trust = fx("codex-trust-dialog-detection.txt");
        assert!(find_dialog(&trust).is_none());
        assert_eq!(
            fallback_fingerprint(&trust),
            fallback_fingerprint(&format!("{trust}\n\n   \n")),
            "blank lines do not count"
        );
        assert_ne!(
            fallback_fingerprint(&trust),
            fallback_fingerprint(&trust.replace("Trust this folder?", "Trust that folder?"))
        );
        let perm = fx("claude-permission-bash.txt");
        assert_eq!(
            fallback_fingerprint(&perm),
            find_dialog(&perm).unwrap().fingerprint.unwrap()
        );
    }

    #[test]
    fn prompt_lines() {
        assert_eq!(
            prompt_line(AgentKind::Claude, &fx("claude-idle-prompt-stray-1.txt")).as_deref(),
            Some("1")
        );
        assert_eq!(
            prompt_line(AgentKind::Claude, &fx("claude-idle-detection-stray-1.txt")).as_deref(),
            Some("1")
        );
        let idle = prompt_line(AgentKind::Claude, &fx("claude-idle-prompt.txt")).unwrap();
        assert_ne!(idle, "1", "placeholder text, not a stray key");
        assert_eq!(
            prompt_line(AgentKind::Codex, &fx("codex-idle-prompt-stray-1.txt")).as_deref(),
            Some("1")
        );
        assert_eq!(
            prompt_line(AgentKind::Codex, &fx("codex-idle-detection-stray-1.txt")).as_deref(),
            Some("1")
        );
        assert_eq!(
            prompt_line(AgentKind::Codex, &fx("codex-idle-prompt.txt")).as_deref(),
            Some("Ask Codex to do anything")
        );
        assert_eq!(
            prompt_line(AgentKind::Claude, &fx("claude-permission-bash.txt")),
            None,
            "dialog showing"
        );
        assert_eq!(
            prompt_line(AgentKind::Codex, &fx("codex-permission-bash-detection.txt")),
            None,
            "dialog showing"
        );
        assert_eq!(
            prompt_line(AgentKind::Other, &fx("claude-idle-prompt.txt")),
            None
        );
    }

    const CMD: &str = " touch /tmp/acp0/t1.txt\n";

    #[test]
    fn guard_fingerprint_never_falls_back_to_the_generic_hash() {
        assert_eq!(
            guard_fingerprint(&fx("codex-permission-collapsed-small-pane-detection.txt")),
            None
        );
        assert_eq!(
            guard_fingerprint(&fx("codex-trust-dialog-detection.txt")),
            None
        );
        for name in [
            "claude-permission-bash.txt",
            "claude-askuserquestion-multi.txt",
            "codex-permission-bash-detection.txt",
        ] {
            let s = fx(name);
            assert_eq!(
                guard_fingerprint(&s),
                find_dialog(&s).unwrap().fingerprint,
                "{name}"
            );
            assert!(guard_fingerprint(&s).is_some());
        }
    }

    #[test]
    fn fallback_fingerprint_on_collapsed_uses_the_generic_hash() {
        let c = fx("codex-permission-collapsed-small-pane-detection.txt");
        assert_eq!(fallback_fingerprint(&c).0.len(), 16);
        assert_ne!(
            fallback_fingerprint(&c),
            fallback_fingerprint(&c.replace("8 lines", "9 lines"))
        );
    }

    #[test]
    fn dialog_normalization_is_strict_about_content() {
        let s = fx("claude-permission-bash.txt");
        let fp = |t: &str| find_dialog(t).unwrap().fingerprint.unwrap();
        let a = s.replace(CMD, " touch /tmp/a  b\n");
        let b = s.replace(CMD, " touch /tmp/a b\n");
        assert_ne!(fp(&a), fp(&b), "internal whitespace is content");
        let tip = s.replace(CMD, &format!("{CMD} Tip: run rm -rf /\n"));
        assert_ne!(fp(&tip), fp(&s), "a Tip line in the detail is content");
    }

    #[test]
    fn numbered_lines_in_the_command_are_not_options() {
        let s = fx("claude-permission-bash.txt");
        let inj = s.replace(CMD, &format!("{CMD} 1. Yes\n"));
        let (o, d) = (find_dialog(&s).unwrap(), find_dialog(&inj).unwrap());
        assert_eq!(o.options, d.options);
        assert_ne!(o.fingerprint, d.fingerprint);
    }

    #[test]
    fn unrecognized_dialog_options_are_not_the_prompt_line() {
        assert_eq!(
            prompt_line(AgentKind::Codex, &fx("codex-trust-dialog-detection.txt")),
            None
        );
    }

    #[test]
    fn malformed_dialogs_fail_closed() {
        let codex = fx("codex-permission-bash-detection.txt");
        assert!(find_dialog(&codex.replace("Would you like to", "Shall we")).is_none());
        let claude = fx("claude-permission-bash.txt");
        assert!(
            find_dialog(&claude.replace("─────", "xxxxx")).is_none(),
            "no solid rule"
        );
    }
}
