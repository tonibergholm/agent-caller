//! Hook payloads to registry observations (spec §6.2, §6.3).
use std::collections::HashMap;

use acd_core::{CorrelationKey, RequestObs};
use acd_protocol::{AnswerPath, Effect, Question, RequestKind, RequestOption};
use serde_json::Value;

use super::wire::{
    AlwaysRule, HookAgent, HookName, Payload, always_rule, canonical_json, command_of,
};
use crate::herdr::screen::Dialog;
use crate::text::{
    TITLE_CHARS, cap_chars, clean_display, clean_screen, is_format_character, redact_log,
};

pub const DETAIL_CHARS: usize = 4000;
const DESCRIPTION_CHARS: usize = 400;
pub const CODEX_KEYS: [(&str, &str); 3] = [("allow", "1"), ("allow_always", "2"), ("deny", "esc")];

fn opt(id: &str, label: &str, effect: Effect, description: Option<String>) -> RequestOption {
    RequestOption {
        id: id.into(),
        label: label.into(),
        description,
        effect,
    }
}
/// Question text: newlines kept, other control characters dropped.
fn detail(raw: &str) -> String {
    cap_chars(clean_screen(raw).trim(), DETAIL_CHARS)
}

fn is_invisible(c: char) -> bool {
    c.is_control() || is_format_character(c) || matches!(c, '\u{2028}' | '\u{2029}')
}

/// Command text for display. Newlines stay real line breaks (the display
/// contract keeps them); every other control, format or invisible
/// character is shown as an escape (`\t`, `\u{7}`, `\u{200b}`) so the
/// text shown matches what runs. Returns the text and whether it was cut
/// at `DETAIL_CHARS`.
fn escaped_detail(raw: &str) -> (String, bool) {
    let mut out = String::new();
    let mut n = 0usize;
    let mut cut = false;
    for c in raw.trim().chars() {
        let piece = match c {
            '\n' => "\n".to_string(),
            '\t' => "\\t".to_string(),
            '\r' => "\\r".to_string(),
            c if is_invisible(c) => format!("\\u{{{:x}}}", c as u32),
            c => c.to_string(),
        };
        let len = piece.chars().count();
        if n + len > DETAIL_CHARS {
            cut = true;
            break;
        }
        out.push_str(&piece);
        n += len;
    }
    (out, cut)
}

const TOO_LONG: &str =
    "This is too long to show in full, so it cannot be answered here. Answer it in the terminal.";

/// The detail text and whether it was cut. A cut detail hides part of what
/// would run, so callers must offer no answer for it.
fn detail_checked(input: &Value) -> (String, bool) {
    let raw = match command_of(input) {
        Some(c) => c.to_owned(),
        None => match input.get("file_path").and_then(Value::as_str) {
            Some(f) => f.to_owned(),
            None => canonical_json(input),
        },
    };
    let (text, cut) = escaped_detail(&raw);
    if cut {
        let head = cap_chars(&text, DETAIL_CHARS - 200);
        (format!("{head}\n\n{TOO_LONG}"), true)
    } else {
        (text, false)
    }
}

/// The command, the file path, or compact JSON (spec §6.2 step 1).
pub fn detail_of(input: &Value) -> String {
    detail_checked(input).0
}

pub fn rule_text(r: &AlwaysRule) -> String {
    format!("{}({})", r.tool_name, r.rule_content)
}
/// Shown whole or not offered at all: no control, bidi, separator or other
/// format/invisible characters (they would be stripped, hidden or reorder
/// the text), at most 4000 chars (the detail cap).
pub fn rule_displayable(text: &str) -> bool {
    text.chars().count() <= DETAIL_CHARS
        && !text.chars().any(is_format_character)
        && clean_display(text, usize::MAX) == text
}

pub fn claude_permission(
    agent_id: &str,
    key: CorrelationKey,
    upgrade_from: Option<CorrelationKey>,
    p: &Payload,
) -> RequestObs {
    let (detail, cut) = detail_checked(&p.tool_input);
    let mut options = vec![];
    if !cut {
        options = vec![
            opt("allow", "Yes", Effect::Allow, None),
            opt("deny", "No", Effect::Deny, None),
        ];
        if let Some(rule) = always_rule(p)
            && rule_displayable(&rule_text(&rule))
        {
            let note = format!(
                "Adds the rule {} to .claude/settings.local.json in this project. \
                 Paths outside this directory may still prompt.",
                rule_text(&rule)
            );
            options.insert(
                1,
                opt(
                    "allow_always",
                    "Yes, always",
                    Effect::AllowAlways,
                    Some(note),
                ),
            );
        }
    }
    RequestObs {
        agent_id: agent_id.into(),
        key,
        upgrade_from,
        kind: RequestKind::Permission,
        // A cut detail hides part of the command: read-only, answered in the terminal.
        answer_path: if cut {
            AnswerPath::None
        } else {
            AnswerPath::Hook
        },
        title: clean_display(p.tool_name.as_deref().unwrap_or("Permission"), TITLE_CHARS),
        detail,
        options,
        questions: None,
        free_text: false,
        raw_screen: None,
        ready: !cut,
        reusable_key: true,
    }
}

pub fn claude_keys_options() -> Vec<RequestOption> {
    vec![
        opt("allow", "Yes", Effect::Allow, None),
        opt("deny", "No", Effect::Deny, None),
    ]
}

pub fn claude_permission_keys(dialog: &Dialog) -> HashMap<String, String> {
    let no = dialog
        .options
        .iter()
        .find(|o| o.label.trim() == "No")
        .map(|o| o.number.to_string())
        .unwrap_or_else(|| "esc".into());
    [
        ("allow".to_string(), "1".to_string()),
        ("deny".to_string(), no),
    ]
    .into()
}

/// One question exactly as Claude sent it (raw strings, not cleaned).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionSpec {
    pub header: String,
    pub question: String,
    pub multi_select: bool,
    /// `(label, description)` in order.
    pub options: Vec<(String, Option<String>)>,
}

pub const MAX_QUESTIONS: usize = 4;
pub const MAX_OPTIONS: usize = 9;

/// Full structural validation of `tool_input.questions` (D25): a
/// non-empty array of at most 4 objects, each with a non-empty string
/// `question`, a string `header`, an optional boolean `multiSelect`, and a
/// non-empty `options` array of at most 9 objects with a non-empty string
/// `label` (distinct after cleaning and the 200-char cut) and an optional
/// string `description`. Any violation rejects the whole input.
pub fn question_specs(input: &Value) -> Result<Vec<QuestionSpec>, &'static str> {
    let qs = input
        .get("questions")
        .and_then(Value::as_array)
        .ok_or("questions is not an array")?;
    if qs.is_empty() {
        return Err("no questions");
    }
    if qs.len() > MAX_QUESTIONS {
        return Err("too many questions");
    }
    qs.iter()
        .map(|q| {
            let q = q.as_object().ok_or("a question is not an object")?;
            let question = q
                .get("question")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or("question text")?;
            let header = q.get("header").and_then(Value::as_str).ok_or("header")?;
            let multi_select = match q.get("multiSelect") {
                None => false,
                Some(v) => v.as_bool().ok_or("multiSelect")?,
            };
            let opts = q
                .get("options")
                .and_then(Value::as_array)
                .filter(|a| !a.is_empty())
                .ok_or("options")?;
            if opts.len() > MAX_OPTIONS {
                return Err("too many options");
            }
            let options = opts
                .iter()
                .map(|o| {
                    let o = o.as_object().ok_or("an option is not an object")?;
                    let label = o
                        .get("label")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .ok_or("label")?;
                    let description = match o.get("description") {
                        None => None,
                        Some(d) => Some(d.as_str().ok_or("description")?.to_owned()),
                    };
                    Ok((label.to_owned(), description))
                })
                .collect::<Result<Vec<_>, &'static str>>()?;
            let mut shown = std::collections::HashSet::new();
            for (label, _) in &options {
                let l = clean_display(label, TITLE_CHARS);
                if l.is_empty() || !shown.insert(l) {
                    return Err("option labels are empty or not distinct");
                }
            }
            Ok(QuestionSpec {
                header: header.to_owned(),
                question: question.to_owned(),
                multi_select,
                options,
            })
        })
        .collect()
}

pub fn question_keys(n: usize) -> HashMap<String, String> {
    (1..=n.min(9))
        .map(|i| (format!("o{i}"), i.to_string()))
        .collect()
}

/// `bound`: the session is bound to a herdr pane. Only a single,
/// single-select question with 1 to 9 options on a bound session is
/// answerable (spec §6.2 Questions); its path is `keys`, not ready until
/// the dialog is captured. `None` only when `tool_input` has no `questions`
/// key; malformed input gives a read-only request.
pub fn claude_question(
    agent_id: &str,
    key: CorrelationKey,
    upgrade_from: Option<CorrelationKey>,
    p: &Payload,
    bound: bool,
) -> Option<RequestObs> {
    p.tool_input.get("questions")?;
    let Ok(specs) = question_specs(&p.tool_input) else {
        return Some(RequestObs {
            agent_id: agent_id.into(),
            key,
            upgrade_from,
            kind: RequestKind::Question,
            answer_path: AnswerPath::None,
            title: "Question".into(),
            detail: "The question could not be read. Answer it in the terminal.".into(),
            options: vec![],
            questions: None,
            free_text: false,
            raw_screen: None,
            ready: false,
            reusable_key: true,
        });
    };
    let questions: Vec<Question> = specs
        .iter()
        .enumerate()
        .map(|(qi, q)| Question {
            id: format!("q{}", qi + 1),
            header: Some(clean_display(&q.header, TITLE_CHARS)),
            text: detail(&q.question),
            multi_select: q.multi_select,
            options: q
                .options
                .iter()
                .enumerate()
                .map(|(i, (label, d))| {
                    opt(
                        &format!("o{}", i + 1),
                        &clean_display(label, TITLE_CHARS),
                        Effect::Choose,
                        d.as_deref().map(|d| clean_display(d, DESCRIPTION_CHARS)),
                    )
                })
                .collect(),
        })
        .collect();
    let first = questions.first()?.clone();
    let answerable = bound
        && questions.len() == 1
        && !first.multi_select
        && (1..=9).contains(&first.options.len());
    Some(RequestObs {
        agent_id: agent_id.into(),
        key,
        upgrade_from,
        kind: RequestKind::Question,
        answer_path: if answerable {
            AnswerPath::Keys
        } else {
            AnswerPath::None
        },
        title: first.header.clone().unwrap_or_else(|| "Question".into()),
        detail: first.text.clone(),
        options: first.options.clone(),
        questions: Some(questions),
        free_text: false,
        raw_screen: None,
        ready: false,
        reusable_key: true,
    })
}

/// spec §6.3 `PermissionRequest`. `collapsed`: the pane is too short and
/// the dialog hides the command, so it can never be matched.
pub fn codex_permission(
    agent_id: &str,
    key: CorrelationKey,
    upgrade_from: Option<CorrelationKey>,
    p: &Payload,
    collapsed: bool,
) -> RequestObs {
    let (mut detail, cut) = detail_checked(&p.tool_input);
    if !cut && let Some(d) = p.tool_input.get("description").and_then(Value::as_str) {
        // The agent's own description of the command, labelled as such.
        let room = DETAIL_CHARS.saturating_sub(detail.chars().count() + 12);
        let (reason, _) = escaped_detail(d);
        if room >= 20 && !reason.is_empty() {
            detail.push_str("\n\nReason: ");
            detail.push_str(&cap_chars(&reason, room.min(DESCRIPTION_CHARS)));
        }
    }
    if collapsed && !cut {
        detail.push_str("\n\nEnlarge the pane to answer here.");
    }
    RequestObs {
        agent_id: agent_id.into(),
        key,
        upgrade_from,
        kind: RequestKind::Permission,
        // A cut detail hides part of the command: read-only, answered in the terminal.
        answer_path: if cut {
            AnswerPath::None
        } else {
            AnswerPath::Keys
        },
        title: clean_display(p.tool_name.as_deref().unwrap_or("Permission"), TITLE_CHARS),
        detail,
        options: if cut {
            vec![]
        } else {
            vec![
                opt("allow", "Yes", Effect::Allow, None),
                opt(
                    "allow_always",
                    "Yes, don't ask again",
                    Effect::AllowAlways,
                    None,
                ),
                opt(
                    "deny",
                    "No, and tell Codex what to do differently",
                    Effect::Deny,
                    None,
                ),
            ]
        },
        questions: None,
        free_text: false,
        raw_screen: None,
        ready: false,
        reusable_key: true,
    }
}

pub fn payload_for_log(raw: &[u8]) -> String {
    redact_log(&String::from_utf8_lossy(raw))
}
pub fn log_payload(agent: HookAgent, name: &HookName, raw: &[u8]) {
    if tracing::enabled!(tracing::Level::DEBUG) {
        tracing::debug!(?agent,?name,payload=%payload_for_log(raw),"hook payload");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::wire::Payload;
    fn payload(rel: &str) -> Payload {
        let p = format!(
            "{}/../../tests/fixtures/hooks/{rel}",
            env!("CARGO_MANIFEST_DIR")
        );
        Payload::from_value(&serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap())
            .unwrap()
    }
    fn screen(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/../../tests/fixtures/herdr/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }
    fn k() -> CorrelationKey {
        CorrelationKey("k".into())
    }
    #[test]
    fn claude_permission_is_a_held_hook_request() {
        let o = claude_permission(
            "a",
            k(),
            None,
            &payload("claude/payload-PermissionRequest-Bash.json"),
        );
        assert_eq!(
            (o.kind, o.answer_path, o.ready, o.reusable_key),
            (RequestKind::Permission, AnswerPath::Hook, true, true)
        );
        assert_eq!(o.title, "Bash");
        assert_eq!(o.detail, "touch /tmp/acp0/t1.txt");
        assert_eq!(
            o.options
                .iter()
                .map(|x| (x.id.as_str(), x.effect))
                .collect::<Vec<_>>(),
            vec![
                ("allow", Effect::Allow),
                ("allow_always", Effect::AllowAlways),
                ("deny", Effect::Deny)
            ]
        );
    }
    #[test]
    fn allow_always_shows_the_exact_rule_it_will_write() {
        let p = payload("claude/payload-PermissionRequest-Bash.json");
        let o = claude_permission("a", k(), None, &p);
        let d = o
            .options
            .iter()
            .find(|x| x.effect == Effect::AllowAlways)
            .unwrap()
            .description
            .as_deref()
            .unwrap();
        let rule = crate::hooks::wire::always_rule(&p).unwrap();
        assert!(d.contains(&rule_text(&rule)));
        assert_eq!(rule_text(&rule), "Bash(touch /tmp/acp0/t1.txt)");
        assert!(d.contains(".claude/settings.local.json") && d.contains("outside"));
    }
    #[test]
    fn a_rule_that_cannot_be_shown_whole_is_not_offered() {
        for cmd in [
            "x".repeat(5000),
            "echo \u{1b}[31m".into(),
            "a\nb".into(),
            "rm \u{202e}x".into(),
        ] {
            let mut p = payload("claude/payload-PermissionRequest-Bash.json");
            p.tool_input["command"] = cmd.clone().into();
            let o = claude_permission("a", k(), None, &p);
            assert!(
                o.options.iter().all(|x| x.effect != Effect::AllowAlways),
                "{cmd:?}"
            );
        }
    }
    #[test]
    fn claude_keys_after_the_hold_drop_allow_always_and_use_the_no_option() {
        let d = crate::herdr::screen::find_dialog(&screen("claude-permission-bash.txt")).unwrap();
        let keys = claude_permission_keys(&d);
        assert_eq!(keys.get("allow").map(String::as_str), Some("1"));
        assert_eq!(keys.get("deny").map(String::as_str), Some("4"));
        assert_eq!(keys.len(), 2);
        assert_eq!(claude_keys_options().len(), 2);
    }
    #[test]
    fn single_question_is_answerable_only_when_bound() {
        let p = payload("claude/payload-PreToolUse-AskUserQuestion.json");
        let b = claude_question("a", k(), None, &p, true).unwrap();
        assert_eq!(
            (b.kind, b.answer_path, b.ready),
            (RequestKind::Question, AnswerPath::Keys, false)
        );
        assert_eq!(
            b.options
                .iter()
                .map(|o| o.label.as_str())
                .collect::<Vec<_>>(),
            vec!["Red", "Blue"]
        );
        assert_eq!(b.options[0].id, "o1");
        assert_eq!(b.questions.as_ref().unwrap()[0].text, "Pick a color");
        assert!(!b.free_text);
        let u = claude_question("a", k(), None, &p, false).unwrap();
        assert_eq!(u.answer_path, AnswerPath::None);
        assert_eq!(
            question_keys(2),
            [("o1".into(), "1".into()), ("o2".into(), "2".into())].into()
        );
    }
    #[test]
    fn malformed_questions_are_read_only() {
        for broken in [
            serde_json::json!([]),
            serde_json::json!([{"question":"Q","header":"H","options":[]}]),
            serde_json::json!([{"question":"Q","header":"H","options":[{"label":5}]}]),
            serde_json::json!([{"question":"Q","options":[{"label":"A"}]}]),
            serde_json::json!([{"question":"","header":"H","options":[{"label":"A"}]}]),
            serde_json::json!([{"question":"Q","header":"H","multiSelect":"no","options":[{"label":"A"}]}]),
            serde_json::json!([{"question":"Q","header":"H","options":[{"label":"A"}]},7]),
        ] {
            let mut p = payload("claude/payload-PreToolUse-AskUserQuestion.json");
            p.tool_input["questions"] = broken.clone();
            assert!(question_specs(&p.tool_input).is_err(), "{broken}");
            let o = claude_question("a", k(), None, &p, true).unwrap();
            assert_eq!(o.answer_path, AnswerPath::None);
            assert!(o.options.is_empty());
        }
    }
    #[test]
    fn multi_select_or_many_questions_stay_read_only() {
        let mut p = payload("claude/payload-PreToolUse-AskUserQuestion.json");
        p.tool_input["questions"][0]["multiSelect"] = true.into();
        assert_eq!(
            claude_question("a", k(), None, &p, true)
                .unwrap()
                .answer_path,
            AnswerPath::None
        );
        let mut p = payload("claude/payload-PreToolUse-AskUserQuestion.json");
        let q = p.tool_input["questions"][0].clone();
        p.tool_input["questions"].as_array_mut().unwrap().push(q);
        assert_eq!(
            claude_question("a", k(), None, &p, true)
                .unwrap()
                .answer_path,
            AnswerPath::None
        );
    }
    #[test]
    fn codex_permission_uses_keys_and_shows_the_command_then_the_reason() {
        let o = codex_permission(
            "a",
            k(),
            None,
            &payload("codex/payload-PermissionRequest-Bash.json"),
            false,
        );
        assert_eq!(
            (o.answer_path, o.ready, o.reusable_key),
            (AnswerPath::Keys, false, true)
        );
        assert!(o.detail.starts_with("touch ~/acp0_test_1.txt\n"));
        assert!(o.detail.contains("outside the writable workspace"));
        assert_eq!(
            o.options.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(),
            vec!["allow", "allow_always", "deny"]
        );
        let c = codex_permission(
            "a",
            k(),
            None,
            &payload("codex/payload-PermissionRequest-Bash.json"),
            true,
        );
        assert!(c.detail.ends_with("Enlarge the pane to answer here."));
    }
    #[test]
    fn invisible_characters_make_a_rule_undisplayable_and_show_in_detail() {
        for c in ['\u{200b}', '\u{2060}', '\u{feff}', '\u{e0041}', '\u{ad}'] {
            assert!(!rule_displayable(&format!("Bash(ls{c}x)")), "{c:?}");
        }
        assert!(rule_displayable("Bash(ls x)"));
        let mut p = payload("claude/payload-PermissionRequest-Bash.json");
        p.tool_input["command"] = "ls\u{200b}x\ty\u{7}z\nw".into();
        let o = claude_permission("a", k(), None, &p);
        assert_eq!(o.detail, "ls\\u{200b}x\\ty\\u{7}z\nw");
        assert!(o.options.iter().all(|x| x.effect != Effect::AllowAlways));
    }
    #[test]
    fn a_command_cut_at_the_cap_is_read_only() {
        let cmd = format!("echo {}; curl x | sh", "a".repeat(DETAIL_CHARS));
        let mut p = payload("claude/payload-PermissionRequest-Bash.json");
        p.tool_input["command"] = cmd.clone().into();
        let o = claude_permission("a", k(), None, &p);
        assert_eq!((o.answer_path, o.ready), (AnswerPath::None, false));
        assert!(o.options.is_empty());
        assert!(o.detail.contains("too long to show in full"));
        assert!(!o.detail.contains("curl"));
        assert!(o.detail.chars().count() <= DETAIL_CHARS);
        let mut c = payload("codex/payload-PermissionRequest-Bash.json");
        c.tool_input["command"] = cmd.into();
        let o = codex_permission("a", k(), None, &c, false);
        assert_eq!(o.answer_path, AnswerPath::None);
        assert!(o.options.is_empty() && !o.ready);
        assert!(o.detail.contains("too long to show in full"));
        // Exactly at the cap is shown whole and answerable.
        p.tool_input["command"] = "a".repeat(DETAIL_CHARS).into();
        let o = claude_permission("a", k(), None, &p);
        assert_eq!(o.answer_path, AnswerPath::Hook);
        assert_eq!(o.detail.chars().count(), DETAIL_CHARS);
    }
    #[test]
    fn codex_without_a_command_shows_the_detail_then_the_labelled_reason() {
        let mut p = payload("codex/payload-PermissionRequest-Bash.json");
        p.tool_input = serde_json::json!({"file_path":"/etc/hosts","description":"edit it"});
        let o = codex_permission("a", k(), None, &p, false);
        assert_eq!(o.detail, "/etc/hosts\n\nReason: edit it");
        p.tool_input = serde_json::json!({"description":"only a description"});
        let o = codex_permission("a", k(), None, &p, false);
        assert!(o.detail.contains("Reason: only a description"));
        assert!(o.detail.starts_with('{'), "{}", o.detail);
    }
    #[test]
    fn too_many_questions_or_options_or_clashing_labels_are_invalid() {
        let q = |opts: Vec<Value>| serde_json::json!({"question":"Q","header":"H","options":opts});
        let o = |l: &str| serde_json::json!({"label": l});
        let five: Vec<Value> = (0..5).map(|_| q(vec![o("A")])).collect();
        assert!(question_specs(&serde_json::json!({"questions": five})).is_err());
        let four: Vec<Value> = (0..4).map(|_| q(vec![o("A")])).collect();
        assert!(question_specs(&serde_json::json!({"questions": four})).is_ok());
        let ten: Vec<Value> = (0..10).map(|i| o(&format!("L{i}"))).collect();
        assert!(question_specs(&serde_json::json!({"questions":[q(ten.clone())]})).is_err());
        assert!(question_specs(&serde_json::json!({"questions":[q(ten[..9].to_vec())]})).is_ok());
        let long = "x".repeat(300);
        let clash = vec![o(&format!("{long}1")), o(&format!("{long}2"))];
        assert!(question_specs(&serde_json::json!({"questions":[q(clash)]})).is_err());
        assert!(
            question_specs(&serde_json::json!({"questions":[q(vec![o("A"), o("A")])]})).is_err()
        );
        let mut p = payload("claude/payload-PreToolUse-AskUserQuestion.json");
        p.tool_input["questions"] =
            serde_json::json!([q((0..10).map(|i| o(&format!("L{i}"))).collect())]);
        let r = claude_question("a", k(), None, &p, true).unwrap();
        assert_eq!(r.answer_path, AnswerPath::None);
        assert!(r.options.is_empty());
    }
    #[test]
    fn hostile_text_is_cleaned_and_capped() {
        let mut p = payload("claude/payload-PermissionRequest-Bash.json");
        p.tool_name = Some("Ba\u{1b}[31msh\u{202e}".into());
        p.tool_input["command"] = format!("rm -rf /\u{7}{}", "x".repeat(5000)).into();
        let o = claude_permission("a", k(), None, &p);
        assert_eq!(o.title, "Ba[31msh");
        assert!(o.detail.chars().count() <= DETAIL_CHARS && !o.detail.contains('\u{7}'));
    }
    #[test]
    fn logged_payloads_are_redacted() {
        let s = payload_for_log(br#"{"tool_input":{"command":"GITHUB_TOKEN=ghp_secret make"}}"#);
        assert!(!s.contains("ghp_secret"), "{s}");
    }
}
