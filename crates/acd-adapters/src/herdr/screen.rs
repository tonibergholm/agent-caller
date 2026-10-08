//! Screen parsing for herdr panes (spec §6.2, §6.3): dialog region,
//! fingerprints and the input-box prompt line. Pure functions over text;
//! the rules are derived from the recorded screens in tests/fixtures/herdr.
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

/// Normalizes one line, or `None` when it should be dropped: strips a
/// leading cursor marker, collapses whitespace, drops empty lines, rule
/// lines and `Tip:` lines.
fn normalize_line(line: &str) -> Option<String> {
    let t = line.trim();
    let t = t
        .strip_prefix('❯')
        .or_else(|| t.strip_prefix('›'))
        .unwrap_or(t);
    let collapsed = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() || is_rule(&collapsed) || collapsed.starts_with("Tip:") {
        return None;
    }
    Some(collapsed)
}

/// Shared by `Dialog::text` and the generic fingerprint so the rules cannot
/// drift apart. Lines are joined with one space so a pane resize that
/// re-wraps text keeps the same result; directory paths are normalized
/// (spec §6.2).
fn normalize_lines(lines: &[&str]) -> String {
    let joined = lines
        .iter()
        .filter_map(|l| normalize_line(l))
        .collect::<Vec<_>>()
        .join(" ");
    joined
        .replace("/private/tmp/", "/tmp/")
        .replace("/private/var/", "/var/")
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

/// Detects a Claude or Codex dialog anywhere in the text.
pub fn find_dialog(text: &str) -> Option<Dialog> {
    let lines: Vec<&str> = text.lines().collect();
    find_claude(&lines).or_else(|| find_codex(&lines))
}

fn make_dialog(
    kind: DialogKind,
    options: Vec<DialogOption>,
    region: &[&str],
    fingerprint: bool,
) -> Dialog {
    let text = normalize_lines(region);
    let fingerprint = fingerprint.then(|| fingerprint_of(&text));
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
/// through the footer. No rule or no option means no dialog.
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
    let options: Vec<_> = region.iter().filter_map(|l| claude_option(l)).collect();
    if options.is_empty() {
        return None;
    }
    let question = region.iter().find(|l| l.contains('☐'));
    let kind = match question {
        Some(l) => DialogKind::ClaudeQuestion {
            tabs: l.trim().starts_with('←'),
        },
        None => DialogKind::ClaudePermission,
    };
    Some(make_dialog(kind, options, region, true))
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
        let options: Vec<_> = region.iter().filter_map(|l| codex_option(l)).collect();
        if options.is_empty() {
            return None;
        }
        return Some(make_dialog(
            DialogKind::CodexPermission,
            options,
            region,
            true,
        ));
    }
    let marker = lines.iter().rposition(|l| is_collapsed_marker(l))?;
    let mut options = Vec::new();
    for line in &lines[marker + 1..] {
        if line.trim().is_empty() {
            continue;
        }
        match codex_option(line) {
            Some(o) => options.push(o),
            None => break,
        }
    }
    if options.is_empty() {
        return None;
    }
    Some(make_dialog(
        DialogKind::CodexCollapsed,
        options,
        &lines[marker..],
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

/// The dialog fingerprint when `find_dialog` finds one that has one,
/// otherwise the generic fingerprint of the last 40 non-empty normalized lines.
pub fn fallback_fingerprint(text: &str) -> Fingerprint {
    if let Some(fp) = find_dialog(text).and_then(|d| d.fingerprint) {
        return fp;
    }
    let kept: Vec<&str> = text
        .lines()
        .filter(|l| normalize_line(l).is_some())
        .collect();
    let tail = &kept[kept.len().saturating_sub(GENERIC_FINGERPRINT_LINES)..];
    fingerprint_of(&normalize_lines(tail))
}

/// The content of the agent's input box (after the `❯`/`›` marker, trimmed).
/// `None` while a dialog is showing, when no input box is found, or for
/// `AgentKind::Other`.
///
/// Claude: the last `❯` line whose previous and next non-empty lines are
/// both `─` rules. Codex: the last `› ` line among the last 6 non-empty lines.
pub fn prompt_line(kind: AgentKind, text: &str) -> Option<String> {
    if matches!(kind, AgentKind::Other) || find_dialog(text).is_some() {
        return None;
    }
    let lines: Vec<&str> = text.lines().collect();
    match kind {
        AgentKind::Claude => {
            let non_empty: Vec<&str> = lines
                .iter()
                .copied()
                .filter(|l| !l.trim().is_empty())
                .collect();
            (1..non_empty.len().saturating_sub(1)).rev().find_map(|i| {
                let t = non_empty[i].trim();
                let content = t.strip_prefix('❯')?;
                (is_solid_rule(non_empty[i - 1]) && is_solid_rule(non_empty[i + 1]))
                    .then(|| content.trim().to_string())
            })
        }
        AgentKind::Codex => {
            let non_empty: Vec<&str> = lines
                .iter()
                .copied()
                .filter(|l| !l.trim().is_empty())
                .collect();
            non_empty[non_empty.len().saturating_sub(CODEX_PROMPT_WINDOW)..]
                .iter()
                .rev()
                .find_map(|l| l.strip_prefix('›').map(|c| c.trim().to_string()))
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
}
