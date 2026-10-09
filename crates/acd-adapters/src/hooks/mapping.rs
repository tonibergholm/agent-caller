//! Hook payloads to registry observations (spec §6.2, §6.3).
use std::collections::HashMap;

use acd_core::{CorrelationKey, RequestObs};
use acd_protocol::{AnswerPath, Effect, Question, RequestKind, RequestOption};
use serde_json::Value;

use super::wire::{
    AlwaysRule, HookAgent, HookName, Payload, always_rule, canonical_json, command_of,
};
use crate::herdr::screen::Dialog;
use crate::text::{TITLE_CHARS, cap_chars, clean_display, clean_screen, redact_log};

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
fn detail(raw: &str) -> String {
    cap_chars(clean_screen(raw).trim(), DETAIL_CHARS)
}

pub fn detail_of(input: &Value) -> String {
    if let Some(c) = command_of(input) {
        return detail(c);
    }
    if let Some(f) = input.get("file_path").and_then(Value::as_str) {
        return detail(f);
    }
    detail(&canonical_json(input))
}

pub fn rule_text(r: &AlwaysRule) -> String {
    format!("{}({})", r.tool_name, r.rule_content)
}
pub fn rule_displayable(text: &str) -> bool {
    text.chars().count() <= DETAIL_CHARS && clean_display(text, usize::MAX) == text
}

pub fn claude_permission(
    agent_id: &str,
    key: CorrelationKey,
    upgrade_from: Option<CorrelationKey>,
    p: &Payload,
) -> RequestObs {
    let mut options = vec![
        opt("allow", "Yes", Effect::Allow, None),
        opt("deny", "No", Effect::Deny, None),
    ];
    if let Some(rule) = always_rule(p)
        && rule_displayable(&rule_text(&rule))
    {
        options.insert(1, opt("allow_always", "Yes, always", Effect::AllowAlways, Some(format!("Adds the rule {} to .claude/settings.local.json in this project. Paths outside this directory may still prompt.", rule_text(&rule)))));
    }
    RequestObs {
        agent_id: agent_id.into(),
        key,
        upgrade_from,
        kind: RequestKind::Permission,
        answer_path: AnswerPath::Hook,
        title: clean_display(p.tool_name.as_deref().unwrap_or("Permission"), TITLE_CHARS),
        detail: detail_of(&p.tool_input),
        options,
        questions: None,
        free_text: false,
        raw_screen: None,
        ready: true,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionSpec {
    pub header: String,
    pub question: String,
    pub multi_select: bool,
    pub options: Vec<(String, Option<String>)>,
}

pub fn question_specs(input: &Value) -> Result<Vec<QuestionSpec>, &'static str> {
    let qs = input
        .get("questions")
        .and_then(Value::as_array)
        .ok_or("questions is not an array")?;
    if qs.is_empty() {
        return Err("no questions");
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

pub fn codex_permission(
    agent_id: &str,
    key: CorrelationKey,
    upgrade_from: Option<CorrelationKey>,
    p: &Payload,
    collapsed: bool,
) -> RequestObs {
    let mut text = command_of(&p.tool_input).unwrap_or_default().to_owned();
    if let Some(d) = p.tool_input.get("description").and_then(Value::as_str) {
        text.push('\n');
        text.push_str(d);
    }
    let mut detail = detail(&text);
    if collapsed {
        detail.push_str("\n\nEnlarge the pane to answer here.");
    }
    RequestObs {
        agent_id: agent_id.into(),
        key,
        upgrade_from,
        kind: RequestKind::Permission,
        answer_path: AnswerPath::Keys,
        title: clean_display(p.tool_name.as_deref().unwrap_or("Permission"), TITLE_CHARS),
        detail,
        options: vec![
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
        ],
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
