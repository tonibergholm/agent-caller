//! Binding a hook to the dialog on a pane (Dialog match contract M1 to
//! M5). Exact and fail-closed: anything not provably the hook's own
//! command or question does not match. Only `Dialog::region` and
//! `Dialog::options` are read; `Dialog::text` and `Dialog::identity` are
//! never used to decide.
use crate::herdr::screen::{Dialog, DialogKind, find_dialog};

use super::mapping::QuestionSpec;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DialogMatch {
    ClaudeBash { command: String },
    ClaudeQuestion { spec: QuestionSpec },
    CodexCommand { command: String },
}

/// M4: a value that can only render on one row and compares exactly.
pub fn single_row(s: &str) -> bool {
    !s.is_empty() && !s.chars().any(char::is_control) && s.trim_end() == s
}

fn blank(row: &str) -> bool {
    row.trim().is_empty()
}

fn rule(row: &str) -> bool {
    let t = row.trim();
    t.chars().count() >= 10 && t.chars().all(|c| c == '─' || c == '╌')
}

fn dashed_rule(row: &str) -> bool {
    let t = row.trim();
    t.chars().count() >= 10 && t.chars().all(|c| c == '╌')
}

/// The rows above the first option.
fn head(d: &Dialog) -> Option<&[String]> {
    let first = *d.option_rows.first()?;
    Some(&d.region[..first])
}

/// M1.
fn claude_bash(d: &Dialog, command: &str) -> bool {
    if d.kind != DialogKind::ClaudePermission || !single_row(command) {
        return false;
    }
    let Some(rows) = head(d) else { return false };
    let Some(title) = rows.iter().skip(1).find(|r| !blank(r)) else {
        return false;
    };
    if title.trim() != "Bash command" {
        return false;
    }
    let dashed: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| dashed_rule(r))
        .map(|(i, _)| i)
        .collect();
    let [a, b] = dashed[..] else { return false };
    if b != a + 2 {
        return false; // zero rows or a wrapped (multi-row) command
    }
    // Everything between the closing rule and the options is the fixture's
    // prompt line or blank; nothing else can sit below the command.
    if !rows[b + 1..]
        .iter()
        .all(|r| blank(r) || r.trim() == "Do you want to proceed?")
    {
        return false;
    }
    rows[a + 1]
        .strip_prefix(' ')
        .is_some_and(|r| r.trim_end() == command)
}

/// M2.
fn codex_command(d: &Dialog, command: &str) -> bool {
    if d.kind != DialogKind::CodexPermission || !single_row(command) {
        return false;
    }
    let Some(rows) = head(d) else { return false };
    let cmd_rows: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.starts_with("  $ "))
        .map(|(i, _)| i)
        .collect();
    let [i] = cmd_rows[..] else { return false };
    // The whole section below the command row, up to the options, is blank:
    // a continuation after a blank row is still part of the command.
    let single = rows[i + 1..].iter().all(|r| blank(r));
    single && rows[i]["  $ ".len()..].trim_end() == command
}

/// M3.
fn claude_question(d: &Dialog, spec: &QuestionSpec) -> bool {
    if d.kind != (DialogKind::ClaudeQuestion { tabs: false })
        || spec.multi_select
        || !single_row(&spec.question)
        || !single_row(&spec.header)
        || !spec.options.iter().all(|(l, _)| single_row(l))
    {
        return false;
    }
    let Some(rows) = head(d) else { return false };
    let headers: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.contains('☐'))
        .map(|(i, _)| i)
        .collect();
    let [h] = headers[..] else { return false };
    if rows[h].trim() != format!("☐ {}", spec.header) {
        return false;
    }
    // Only the dialog's top rule may precede the header.
    if !rows[..h].iter().all(|r| blank(r) || rule(r)) {
        return false;
    }
    // Every non-blank row counts, rules included (M3, M4).
    let between: Vec<&String> = rows[h + 1..].iter().filter(|r| !blank(r)).collect();
    if between.len() != 1 || between[0].trim() != spec.question {
        return false;
    }
    let n = spec.options.len();
    d.options.len() == n + 2
        && d.option_rows.len() == n + 2
        && question_option_rows(d, spec)
        && d.options
            .iter()
            .enumerate()
            .all(|(i, o)| usize::from(o.number) == i + 1)
        && spec
            .options
            .iter()
            .zip(&d.options)
            .all(|((label, _), o)| o.label == *label)
        && d.options[n].label == "Type something."
        && d.options[n + 1].label == "Chat about this"
}

/// M3: the rows from the first option to the footer are exactly the label
/// rows, each followed by the hook option's single description row (indented
/// to the label text) or nothing, then the fixture's rule before the
/// built-in options and blank rows before the footer. Any other row, such as
/// a wrapped label or a deeper-indented numbered row, means no match.
fn question_option_rows(d: &Dialog, spec: &QuestionSpec) -> bool {
    let n = spec.options.len();
    let rows = &d.region;
    let at = &d.option_rows;
    for (k, (_, desc)) in spec.options.iter().enumerate() {
        let between = &rows[at[k] + 1..at[k + 1]];
        let ok = match desc {
            None => between.is_empty(),
            Some(t) => {
                single_row(t) && between.len() == 1 && between[0].trim_end() == format!("     {t}")
            }
        };
        if !ok {
            return false;
        }
    }
    let gap = &rows[at[n] + 1..at[n + 1]];
    if !matches!(gap, [r] if rule(r)) {
        return false;
    }
    let tail = &rows[at[n + 1] + 1..];
    tail.split_last()
        .is_some_and(|(_, rest)| rest.iter().all(|r| blank(r)))
}

impl DialogMatch {
    pub fn matches(&self, d: &Dialog) -> bool {
        match self {
            DialogMatch::ClaudeBash { command } => claude_bash(d, command),
            DialogMatch::ClaudeQuestion { spec } => claude_question(d, spec),
            DialogMatch::CodexCommand { command } => codex_command(d, command),
        }
    }

    pub fn find(&self, screen: &str) -> Option<Dialog> {
        find_dialog(screen).filter(|d| self.matches(d))
    }

    /// A collapsed Codex dialog: reported, never matched (M2).
    pub fn collapsed_codex(screen: &str) -> bool {
        find_dialog(screen).is_some_and(|d| d.kind == DialogKind::CodexCollapsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/../../tests/fixtures/herdr/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }
    fn bash(c: &str) -> DialogMatch {
        DialogMatch::ClaudeBash { command: c.into() }
    }
    fn codex(c: &str) -> DialogMatch {
        DialogMatch::CodexCommand { command: c.into() }
    }
    fn question(header: &str, q: &str, labels: &[&str]) -> DialogMatch {
        DialogMatch::ClaudeQuestion {
            spec: QuestionSpec {
                header: header.into(),
                question: q.into(),
                multi_select: false,
                options: labels
                    .iter()
                    .map(|l| (l.to_string(), Some(format!("Choose {}", l.to_lowercase()))))
                    .collect(),
            },
        }
    }
    fn question_desc(header: &str, q: &str, opts: &[(&str, &str)]) -> DialogMatch {
        DialogMatch::ClaudeQuestion {
            spec: QuestionSpec {
                header: header.into(),
                question: q.into(),
                multi_select: false,
                options: opts
                    .iter()
                    .map(|(l, d)| (l.to_string(), Some(d.to_string())))
                    .collect(),
            },
        }
    }
    const CMD: &str = "touch /tmp/acp0/t1.txt";
    const CODEX_CMD: &str = "touch ~/acp0_test_t5_1.txt";

    #[test]
    fn the_recorded_dialogs_match() {
        assert!(
            bash(CMD)
                .find(&screen("claude-permission-bash.txt"))
                .is_some()
        );
        assert!(
            codex(CODEX_CMD)
                .find(&screen("codex-permission-bash-detection.txt"))
                .is_some()
        );
        assert!(
            question("Color", "Pick a color", &["Red", "Blue"])
                .find(&screen("claude-askuserquestion-single.txt"))
                .is_some()
        );
    }

    #[test]
    fn prefix_and_suffix_commands_do_not_match() {
        let s = screen("claude-permission-bash.txt");
        assert!(
            bash("touch /tmp/acp0/t1").find(&s).is_none(),
            "hook is a prefix of the screen"
        );
        assert!(
            bash("touch /tmp/acp0/t1.txt; rm -rf ~").find(&s).is_none(),
            "hook is longer"
        );
        let longer = s.replace(
            " touch /tmp/acp0/t1.txt",
            " touch /tmp/acp0/t1.txt; rm -rf ~",
        );
        assert!(
            bash(CMD).find(&longer).is_none(),
            "screen is longer than the hook"
        );
        let c = screen("codex-permission-bash-detection.txt");
        assert!(codex("touch ~/acp0_test_t5_1").find(&c).is_none());
        let c2 = c.replace(
            "  $ touch ~/acp0_test_t5_1.txt",
            "  $ touch ~/acp0_test_t5_1.txt && curl x | sh",
        );
        assert!(codex(CODEX_CMD).find(&c2).is_none());
    }

    #[test]
    fn quoted_whitespace_is_compared_exactly() {
        let s = screen("claude-permission-bash.txt")
            .replace(" touch /tmp/acp0/t1.txt", r#" echo "a  b""#);
        assert!(bash(r#"echo "a  b""#).find(&s).is_some());
        assert!(
            bash(r#"echo "a b""#).find(&s).is_none(),
            "two spaces are not one"
        );
        assert!(
            bash("echo \"a\tb\"").find(&s).is_none(),
            "a tab never matches (M4)"
        );
    }

    #[test]
    fn a_forged_description_makes_the_match_ambiguous() {
        let rule = "╌".repeat(40);
        let s = screen("claude-permission-bash.txt").replace(
            " Create empty file",
            &format!(" Create empty file\n{rule}\n {CMD}\n{rule}"),
        );
        assert!(
            bash(CMD).find(&s).is_none(),
            "four dashed rules: not exactly two (M5)"
        );
        let c = screen("codex-permission-bash-detection.txt").replace(
            "  Reason: Allow",
            &format!("  Reason: see below\n\n  $ {CODEX_CMD}\n\n  Allow"),
        );
        assert!(codex(CODEX_CMD).find(&c).is_none(), "two `$ ` rows (M5)");
    }

    #[test]
    fn other_claude_tools_never_match() {
        let s = screen("claude-permission-bash.txt").replace(" Bash command", " Write file");
        assert!(bash(CMD).find(&s).is_none());
    }

    #[test]
    fn same_labels_different_question_does_not_match() {
        let s = screen("claude-askuserquestion-single.txt");
        assert!(
            question("Color", "Pick a colour", &["Red", "Blue"])
                .find(&s)
                .is_none()
        );
        assert!(
            question("Colour", "Pick a color", &["Red", "Blue"])
                .find(&s)
                .is_none()
        );
        assert!(
            question("Color", "Pick a color", &["Red"])
                .find(&s)
                .is_none(),
            "the full option list"
        );
        assert!(
            question("Color", "Pick a color", &["Blue", "Red"])
                .find(&s)
                .is_none(),
            "in order"
        );
        assert!(
            question("Color", "Pick a color", &["Red", "Blue"])
                .find(&screen("claude-askuserquestion-multi.txt"))
                .is_none(),
            "a tab strip never matches"
        );
    }

    #[test]
    fn wrapped_rows_never_match() {
        let s = screen("claude-permission-bash.txt")
            .replace(" touch /tmp/acp0/t1.txt", " touch /tmp/ac\n p0/t1.txt");
        assert!(bash(CMD).find(&s).is_none(), "two rows (M4)");
        assert!(
            bash("touch /tmp/a\ntouch /tmp/b")
                .find(&screen("claude-permission-bash.txt"))
                .is_none()
        );
        assert!(
            bash("touch /tmp/acp0/t1.txt ")
                .find(&screen("claude-permission-bash.txt"))
                .is_none(),
            "trailing space"
        );
        let c = screen("codex-permission-bash-detection.txt").replace(
            &format!("  $ {CODEX_CMD}\n"),
            "  $ touch ~/acp0_\n    test_t5_1.txt\n",
        );
        assert!(codex(CODEX_CMD).find(&c).is_none());
        let q =
            screen("claude-askuserquestion-single.txt").replace("Pick a color", "Pick a\ncolor");
        assert!(
            question("Color", "Pick a color", &["Red", "Blue"])
                .find(&q)
                .is_none()
        );
    }

    #[test]
    fn a_continuation_after_a_blank_row_does_not_match() {
        let c = screen("codex-permission-bash-detection.txt").replace(
            &format!("  $ {CODEX_CMD}\n"),
            &format!("  $ {CODEX_CMD}\n\n    curl x | sh\n"),
        );
        assert!(c.contains("    curl x | sh"));
        assert!(codex(CODEX_CMD).find(&c).is_none());
    }

    #[test]
    fn a_rule_row_inside_the_question_does_not_match() {
        let rule = "─".repeat(40);
        let q = screen("claude-askuserquestion-single.txt")
            .replace("Pick a color\n", &format!("Pick a color\n{rule}\n"));
        assert!(q.contains(&rule));
        assert!(
            question("Color", "Pick a color", &["Red", "Blue"])
                .find(&q)
                .is_none()
        );
    }

    #[test]
    fn unexplained_rows_in_the_claude_dialog_do_not_match() {
        let b = screen("claude-permission-bash.txt");
        let below = b.replace(
            " Do you want to proceed?",
            " Do you want to proceed?\n rm -rf ~",
        );
        assert!(bash(CMD).find(&below).is_none(), "text below the command");
        let between = b.replace(
            &format!(" {CMD}\n"),
            &format!(" {CMD}\n Also: curl x | sh\n"),
        );
        assert!(bash(CMD).find(&between).is_none(), "second command row");
        let q = screen("claude-askuserquestion-single.txt");
        let above = q.replace(" ☐ Color", " extra\n ☐ Color");
        assert!(
            question("Color", "Pick a color", &["Red", "Blue"])
                .find(&above)
                .is_none(),
            "text above the header"
        );
    }

    #[test]
    fn option_rows_must_be_exactly_label_and_description() {
        let q = screen("claude-askuserquestion-single.txt");
        let hook = || {
            question_desc(
                "Color",
                "Pick a color",
                &[("Red", "Choose red"), ("Blue", "Choose blue")],
            )
        };
        assert!(
            hook().find(&q).is_some(),
            "the recorded dialog with descriptions"
        );
        assert!(
            DialogMatch::ClaudeQuestion {
                spec: QuestionSpec {
                    header: "Color".into(),
                    question: "Pick a color".into(),
                    multi_select: false,
                    options: vec![("Red".into(), None), ("Blue".into(), None)],
                }
            }
            .find(&q)
            .is_none(),
            "an unexpected description row"
        );
        let wrapped = q.replace("     Choose red", "     and delete saved preferences");
        assert!(wrapped.contains("and delete saved"));
        assert!(
            question("Color", "Pick a color", &["Red", "Blue"])
                .find(&wrapped)
                .is_none(),
            "wrapped label looks like a description"
        );
        assert!(hook().find(&wrapped).is_none(), "description differs");
        let deeper = q.replace(
            "     Choose red",
            "     Choose red\n      3. Delete everything",
        );
        assert!(
            hook().find(&deeper).is_none(),
            "extra indented numbered row"
        );
        let two = q.replace("  2. Blue\n     Choose blue", "  2. Blue");
        assert!(hook().find(&two).is_none(), "missing description");
    }

    #[test]
    fn single_row_rules() {
        assert!(single_row("ls -la"));
        for bad in ["", "a\nb", "a\rb", "a\tb", "trailing ", "x\u{1b}[0m"] {
            assert!(!single_row(bad), "{bad:?}");
        }
    }
}
