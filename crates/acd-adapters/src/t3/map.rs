//! Ids, state and request mapping, and command payloads (spec §6.5).
use std::collections::HashSet;

use acd_core::{AgentObs, Answer, CorrelationKey, RequestObs};
use acd_protocol::{
    AgentKind, AgentRole, AgentState, AnswerPath, Effect, Question, Request, RequestKind,
    RequestOption, Source,
};
use serde_json::{Value, json};

use super::detail::PendingView;
use crate::hooks::mapping::escape_text;
use super::wire::{Project, ShellThread, TurnItem};
use crate::text::{LAST_MESSAGE_CHARS, TITLE_CHARS, cap_chars, clean_display, clean_screen};

pub use crate::hooks::mapping::DETAIL_CHARS;
pub const DECISIONS: [&str; 5] = [
    "accept",
    "acceptForSession",
    "acceptAlways",
    "decline",
    "cancel",
];
const FALLBACK: [(&str, &str, Effect); 3] = [
    ("accept", "Yes", Effect::Allow),
    (
        "acceptForSession",
        "Yes, for this session",
        Effect::AllowAlways,
    ),
    ("decline", "No", Effect::Deny),
];

pub fn agent_id(thread_id: &str) -> String {
    crate::ids::bounded("t3:", thread_id)
}

pub fn kind(provider_instance_id: Option<&str>) -> AgentKind {
    match provider_instance_id {
        Some(p) if p.starts_with("claude") => AgentKind::Claude,
        Some(p) if p.starts_with("codex") => AgentKind::Codex,
        _ => AgentKind::Other,
    }
}

pub fn state(status: Option<&str>, needs_you: bool) -> (AgentState, bool) {
    if needs_you {
        return (AgentState::NeedsYou, false);
    }
    match status {
        Some("running" | "starting" | "preparing" | "queued" | "waiting") => {
            (AgentState::Working, false)
        }
        Some("completed") => (AgentState::Done, false),
        Some("failed" | "interrupted") => (AgentState::Done, true),
        Some("idle" | "cancelled" | "rolled_back") => (AgentState::Idle, false),
        _ => (AgentState::Unknown, false),
    }
}

/// `state` for a shell entry: a `completed` thread with pending background
/// tasks is still working in T3.
pub fn shell_state(t: &ShellThread, needs_you: bool) -> (AgentState, bool) {
    if !needs_you && t.status.as_deref() == Some("completed") && t.has_background_tasks() {
        return (AgentState::Working, false);
    }
    state(t.status.as_deref(), needs_you)
}

fn message_text(v: &Value) -> Option<&str> {
    v.as_str().or_else(|| v.get("text").and_then(Value::as_str))
}

pub fn agent_obs(t: &ShellThread, project: Option<&Project>, hidden: &HashSet<String>) -> AgentObs {
    let needs_you = t
        .pending_runtime_request
        .as_ref()
        .is_some_and(|p| !hidden.contains(&p.id));
    let (state, failed) = shell_state(t, needs_you);
    let cwd = t
        .worktree_path
        .clone()
        .or_else(|| project.and_then(|p| p.workspace_root.clone()));
    AgentObs {
        id: agent_id(&t.id),
        source: Source::T3,
        kind: kind(t.provider_instance_id.as_deref()),
        title: clean_display(t.title.as_deref().unwrap_or(""), TITLE_CHARS),
        cwd: cwd.map(|c| clean_display(&c, usize::MAX)),
        project: project
            .and_then(|p| p.title.as_deref())
            .map(|s| clean_display(s, TITLE_CHARS)),
        state,
        failed,
        last_message: t
            .latest_visible_message
            .as_ref()
            .and_then(message_text)
            .map(|m| clean_display(m, LAST_MESSAGE_CHARS))
            .filter(|m| !m.is_empty()),
        can_send: true,
        // The reconciler fills in the parent's title; the shell alone lacks it.
        role: if t.is_subagent() {
            AgentRole::Subagent
        } else {
            AgentRole::Main
        },
        parent_title: None,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub option_id: String,
    pub label: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TargetAnswer {
    Decision,
    Question {
        question_id: String,
        choices: Vec<Choice>,
        /// A typed answer is accepted (`answers[question_id] = text`).
        free_text: bool,
    },
    ReadOnly,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub agent_id: String,
    pub thread_id: String,
    pub t3_request_id: String,
    pub answer: TargetAnswer,
}

fn detail(s: &str) -> String {
    cap_chars(clean_screen(s).trim(), DETAIL_CHARS)
}

/// A command as it would run, for display: control, bidi and format
/// characters are shown as escapes instead of being dropped (same as the
/// hooks mapping). Returns the text and whether it is cut at `DETAIL_CHARS`
/// characters (not bytes). The escaped text is never shorter than the raw
/// one, so a raw command over the limit is always cut.
fn command_detail(s: &str) -> (String, bool) {
    let escaped = escape_text(s, true);
    let cut = escaped.chars().count() > DETAIL_CHARS;
    (cap_chars(&escaped, DETAIL_CHARS), cut)
}

/// Whether a typed answer is offered for `q`. T3's own UI treats an absent
/// `allowCustomAnswer` as allowed, but only Claude is verified to take a typed
/// string (live check, 2026-10-11). Any other provider needs an explicit
/// `true`, and an explicit `false` always wins.
fn takes_free_text(q: &super::wire::T3Question, provider: Option<&str>) -> bool {
    match q.allow_custom_answer {
        Some(allowed) => allowed,
        None => provider.is_some_and(|p| p.starts_with("claude")),
    }
}

/// `provider` is the shell thread's `providerInstanceId`.
pub fn request_obs(
    agent_id: &str,
    thread_id: &str,
    provider: Option<&str>,
    view: &PendingView,
) -> (RequestObs, Target) {
    let rid = view.request.id.clone();
    let mut obs = RequestObs {
        agent_id: agent_id.into(),
        key: CorrelationKey(rid.clone()),
        upgrade_from: None,
        kind: RequestKind::Permission,
        answer_path: AnswerPath::None,
        title: clean_display(&format!("T3 request: {}", view.request.kind), TITLE_CHARS),
        detail: "Answer this in T3.".into(),
        options: vec![],
        questions: None,
        free_text: false,
        raw_screen: None,
        ready: true,
        reusable_key: false,
    };
    let mut answer = TargetAnswer::ReadOnly;
    match &view.item {
        Some(TurnItem::ApprovalRequest {
            request_kind,
            prompt,
            ..
        }) if request_kind.as_deref() == Some("command") => {
            let prompt = prompt.as_deref().unwrap_or("").trim();
            obs.title = "Run a command".into();
            // The prompt is model-written text; it never stands in for the
            // command. Without the joined command the request is read-only.
            match view
                .command
                .as_deref()
                .filter(|c| !c.trim().is_empty())
            {
                Some(c) => {
                    let text = if !prompt.is_empty() && c != prompt {
                        format!("{c}\n{prompt}")
                    } else {
                        c.to_owned()
                    };
                    let (shown, cut) = command_detail(&text);
                    obs.detail = shown;
                    // A cut command hides part of what would run: read-only,
                    // answered in T3 (same rule as the hooks mapping).
                    if !cut {
                        obs.answer_path = AnswerPath::T3;
                        obs.options = FALLBACK
                            .iter()
                            .map(|(id, label, effect)| RequestOption {
                                id: (*id).into(),
                                label: (*label).into(),
                                description: None,
                                effect: *effect,
                                confirm: false,
                            })
                            .collect();
                        answer = TargetAnswer::Decision;
                    }
                }
                None => {
                    let mut text = String::from("Command not available yet \u{2014} answer in T3.");
                    if !prompt.is_empty() {
                        text.push_str("\nDescription: ");
                        text.push_str(prompt);
                    }
                    obs.detail = detail(&text);
                }
            }
        }
        Some(TurnItem::UserInputRequest { questions, .. }) if !questions.is_empty() => {
            let qs: Vec<Question> = questions
                .iter()
                .map(|q| Question {
                    id: q.id.clone(),
                    header: q.header.as_deref().map(|h| clean_display(h, TITLE_CHARS)),
                    text: detail(&q.question),
                    multi_select: q.multi_select,
                    options: q
                        .options
                        .iter()
                        .enumerate()
                        .map(|(i, o)| RequestOption {
                            id: format!("o{}", i + 1),
                            label: clean_display(&o.label, TITLE_CHARS),
                            description: o
                                .description
                                .as_deref()
                                .map(|d| clean_display(d, TITLE_CHARS)),
                            effect: Effect::Choose,
                            confirm: false,
                        })
                        .collect(),
                })
                .collect();
            let q0 = &questions[0];
            obs.kind = RequestKind::Question;
            obs.title = qs[0].header.clone().unwrap_or_else(|| "Question".into());
            obs.detail = qs[0].text.clone();
            // Only a single single-select question can be answered: by one of
            // its options, or, with none, by typed text alone (the panel
            // renders a text input). A zero-option question T3 does not let
            // the user type into stays read-only.
            let free_text = takes_free_text(q0, provider);
            // Whole or read-only: text, header, label or description that
            // cleaning cut or altered must not be answerable from the card.
            let whole = questions.iter().zip(&qs).all(|(q, shown)| {
                shown.text == q.question.trim()
                    && shown.header.as_deref() == q.header.as_deref().map(str::trim)
                    && q.options.iter().zip(&shown.options).all(|(o, so)| {
                        so.label == o.label.trim()
                            && so.description.as_deref() == o.description.as_deref().map(str::trim)
                    })
            });
            if whole
                && questions.len() == 1
                && !q0.multi_select
                && (!q0.options.is_empty() || free_text)
            {
                answer = TargetAnswer::Question {
                    question_id: q0.id.clone(),
                    choices: q0
                        .options
                        .iter()
                        .enumerate()
                        .map(|(i, o)| Choice {
                            option_id: format!("o{}", i + 1),
                            label: clean_display(&o.label, TITLE_CHARS),
                            value: o.value.clone().unwrap_or_else(|| o.label.clone()),
                        })
                        .collect(),
                    free_text,
                };
                obs.free_text = free_text;
                obs.options = qs[0].options.clone();
                obs.answer_path = AnswerPath::T3;
            }
            obs.questions = Some(qs);
        }
        _ => {}
    }
    let target = Target {
        agent_id: agent_id.into(),
        thread_id: thread_id.into(),
        t3_request_id: rid,
        answer,
    };
    (obs, target)
}

/// The `runtime-request.respond` payload for a claimed answer. Errors mean
/// the answer cannot be expressed; nothing is sent.
pub fn respond_payload(
    command_id: &str,
    target: &Target,
    request: &Request,
    answer: &Answer,
) -> Result<Value, &'static str> {
    let mut p = json!({
        "type": "runtime-request.respond", "commandId": command_id,
        "threadId": target.thread_id, "requestId": target.t3_request_id,
    });
    if let Answer::Text(text) = answer {
        let TargetAnswer::Question {
            question_id,
            free_text: true,
            ..
        } = &target.answer
        else {
            return Err("this T3 request takes no typed answer");
        };
        let text = text.trim();
        crate::text::check_prompt_text(text)?;
        p["answers"] = json!({ question_id.clone(): text });
        return Ok(p);
    }
    let Answer::Option(option_id) = answer else {
        unreachable!("text handled above")
    };
    match &target.answer {
        TargetAnswer::Decision => {
            if !DECISIONS.contains(&option_id.as_str()) {
                return Err("not a T3 decision");
            }
            p["decision"] = json!(option_id);
        }
        TargetAnswer::Question {
            question_id,
            choices,
            ..
        } => {
            let shown = request
                .options
                .iter()
                .find(|o| &o.id == option_id)
                .ok_or("unknown option")?;
            let choice = choices
                .iter()
                .find(|c| &c.option_id == option_id && c.label == shown.label)
                .ok_or("option does not match the recorded question")?;
            p["answers"] = json!({ question_id.clone(): choice.value });
        }
        TargetAnswer::ReadOnly => return Err("read-only request"),
    }
    Ok(p)
}

pub fn dispatch_payload(command_id: &str, message_id: &str, thread_id: &str, text: &str) -> Value {
    json!({
        "type": "message.dispatch", "createdBy": "user", "creationSource": "web",
        "commandId": command_id, "threadId": thread_id, "messageId": message_id,
        "text": text, "attachments": [], "deliveryIntent": "auto",
        "dispatchMode": {"type": "start_immediately"},
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t3::{detail::ThreadDetail, fake::question_projection};

    fn question_view(zero_options: bool, allow: Option<bool>) -> PendingView {
        let mut projection = question_projection("thread", "request");
        let item = projection["turnItems"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|item| item["type"] == "user_input_request")
            .unwrap();
        if zero_options {
            item["questions"][0]["options"] = json!([]);
        }
        if let Some(a) = allow {
            item["questions"][0]["allowCustomAnswer"] = json!(a);
        }
        let (detail, _) = ThreadDetail::from_projection(&projection);
        detail.pending_view("request").unwrap()
    }

    fn obs_for(provider: Option<&str>, view: &PendingView) -> (RequestObs, Target) {
        request_obs("t3:thread", "thread", provider, view)
    }

    #[test]
    fn zero_option_questions_are_answerable_by_text_when_free_text_is_allowed() {
        for (provider, allow) in [
            (Some("claudeAgent"), None),
            (Some("claudeAgent"), Some(true)),
            (Some("codex"), Some(true)),
        ] {
            let (obs, target) = obs_for(provider, &question_view(true, allow));
            assert_eq!(obs.answer_path, AnswerPath::T3, "{provider:?} {allow:?}");
            assert!(obs.free_text && obs.options.is_empty());
            assert!(matches!(
                target.answer,
                TargetAnswer::Question {
                    free_text: true,
                    ..
                }
            ));
        }
    }

    /// Whole or read-only: a cut question, header, label or description is
    /// not answerable (including by free text).
    #[test]
    fn a_cut_question_is_read_only() {
        for (field, value) in [
            ("question", json!("x".repeat(5000))),
            ("header", json!("H".repeat(300))),
        ] {
            let mut projection = question_projection("thread", "request");
            let item = projection["turnItems"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|item| item["type"] == "user_input_request")
                .unwrap();
            item["questions"][0][field] = value;
            let (detail, _) = ThreadDetail::from_projection(&projection);
            let (obs, target) = obs_for(Some("claudeAgent"), &detail.pending_view("request").unwrap());
            assert_eq!(obs.answer_path, AnswerPath::None, "{field}");
            assert!(!obs.free_text, "{field}");
            assert!(matches!(target.answer, TargetAnswer::ReadOnly), "{field}");
        }
        let mut projection = question_projection("thread", "request");
        let item = projection["turnItems"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|item| item["type"] == "user_input_request")
            .unwrap();
        item["questions"][0]["options"][0]["label"] = json!("L".repeat(300));
        let (detail, _) = ThreadDetail::from_projection(&projection);
        let (obs, _) = obs_for(Some("claudeAgent"), &detail.pending_view("request").unwrap());
        assert_eq!(obs.answer_path, AnswerPath::None, "label");
    }

    #[test]
    fn zero_option_questions_stay_read_only_when_text_is_not_allowed() {
        for (provider, allow) in [
            (Some("claudeAgent"), Some(false)),
            (Some("codex"), None),
            (None, None),
        ] {
            let (obs, target) = obs_for(provider, &question_view(true, allow));
            assert_eq!(obs.answer_path, AnswerPath::None, "{provider:?} {allow:?}");
            assert!(!obs.free_text);
            assert!(matches!(target.answer, TargetAnswer::ReadOnly));
        }
    }

    #[test]
    fn free_text_is_offered_to_claude_by_default_and_to_others_only_when_explicit() {
        let free = |provider, allow| obs_for(provider, &question_view(false, allow)).0.free_text;
        assert!(free(Some("claudeAgent"), None));
        assert!(free(Some("claudeAgent"), Some(true)));
        assert!(!free(Some("claudeAgent"), Some(false)));
        assert!(!free(Some("codex"), None));
        assert!(!free(None, None));
        assert!(free(Some("codex"), Some(true)));
        assert!(!free(Some("opencode"), Some(false)));
        // Options stay answerable either way.
        let (obs, _) = obs_for(Some("codex"), &question_view(false, None));
        assert_eq!(obs.answer_path, AnswerPath::T3);
    }

    fn command_view(command: &str) -> PendingView {
        let projection = crate::t3::fake::approval_projection("thread", "request", None);
        let (detail, _) = ThreadDetail::from_projection(&projection);
        let mut view = detail.pending_view("request").unwrap();
        let Some(TurnItem::ApprovalRequest {
            request_kind,
            prompt,
            ..
        }) = &mut view.item
        else {
            panic!("an approval");
        };
        assert_eq!(request_kind.as_deref(), Some("command"));
        *prompt = None;
        view.command = Some(command.into());
        view
    }

    #[test]
    fn a_command_at_the_cap_is_answerable() {
        let (obs, target) = obs_for(Some("claude"), &command_view(&"a".repeat(DETAIL_CHARS)));
        assert_eq!(obs.answer_path, AnswerPath::T3);
        assert_eq!(obs.options.len(), 3);
        assert_eq!(target.answer, TargetAnswer::Decision);
    }

    #[test]
    fn a_command_over_the_cap_is_read_only() {
        let (obs, target) = obs_for(Some("claude"), &command_view(&"a".repeat(DETAIL_CHARS + 1)));
        assert_eq!(obs.answer_path, AnswerPath::None);
        assert!(obs.options.is_empty());
        assert_eq!(target.answer, TargetAnswer::ReadOnly);
        assert!(obs.detail.chars().count() <= DETAIL_CHARS);
    }

    #[test]
    fn a_tab_in_a_command_is_shown_escaped_and_stays_answerable() {
        let (obs, _) = obs_for(Some("claude"), &command_view("printf 'a\tb'"));
        assert_eq!(obs.detail, "printf 'a\\tb'");
        assert_eq!(obs.answer_path, AnswerPath::T3);
    }

    #[test]
    fn a_command_of_mostly_tabs_over_the_cap_is_read_only() {
        let cmd = format!("a{}", "\t".repeat(DETAIL_CHARS));
        assert_eq!(cmd.chars().count(), DETAIL_CHARS + 1);
        let (obs, target) = obs_for(Some("claude"), &command_view(&cmd));
        assert_eq!(obs.answer_path, AnswerPath::None);
        assert_eq!(target.answer, TargetAnswer::ReadOnly);
        // Fewer raw characters, but the escaped text is over the cap.
        let (obs, _) = obs_for(Some("claude"), &command_view(&format!("a{}a", "\t".repeat(2500))));
        assert_eq!(obs.answer_path, AnswerPath::None);
    }

    #[test]
    fn a_bidi_override_in_a_command_is_not_dropped_silently() {
        let (obs, _) = obs_for(Some("claude"), &command_view("echo \u{202e}ok"));
        assert!(!obs.detail.contains('\u{202e}'));
        assert!(obs.detail.contains("\\u{202e}"));
    }

    #[test]
    fn a_command_is_shown_with_exact_escapes_for_every_character_class() {
        let cases: &[(&str, &str)] = &[
            ("a\rb", "a\\rb"),
            ("a\0b", "a\\u{0}b"),
            ("\u{1b}[31mred\u{1b}[0m", "\\u{1b}[31mred\\u{1b}[0m"),
            ("a\u{2066}b\u{2067}c\u{2068}d\u{2069}e", "a\\u{2066}b\\u{2067}c\\u{2068}d\\u{2069}e"),
            ("a\u{200b}b\u{200c}c\u{200d}d\u{2060}e\u{feff}f", "a\\u{200b}b\\u{200c}c\\u{200d}d\\u{2060}e\\u{feff}f"),
            ("a\\b\\\\c", "a\\\\b\\\\\\\\c"),
            ("  ls  ", "\\u{20}\\u{20}ls\\u{20}\\u{20}"),
            ("\n ls\t", "\\n\\u{20}ls\\t"),
            ("a b\nc", "a b\nc"),
            (
                "  a\r\0\u{1b}[31m\u{2066}x\u{2069}\u{200b}\\ b  ",
                "\\u{20}\\u{20}a\\r\\u{0}\\u{1b}[31m\\u{2066}x\\u{2069}\\u{200b}\\\\ b\\u{20}\\u{20}",
            ),
        ];
        for (raw, want) in cases {
            let (shown, cut) = command_detail(raw);
            assert_eq!((shown.as_str(), cut), (*want, false), "{raw:?}");
        }
        // The same text on a whole observation stays answerable, unchanged.
        let (obs, _) = obs_for(Some("claude"), &command_view("a\rb\u{2066}c\\d"));
        assert_eq!(obs.detail, "a\\rb\\u{2066}c\\\\d");
        assert_eq!(obs.answer_path, AnswerPath::T3);
    }

    /// Accepted residual (follow-ups, 2026-11-03): U+034F and similar pass
    /// the escaper unchanged. Pinned so a change shows.
    #[test]
    fn a_combining_grapheme_joiner_in_a_command_passes_unchanged() {
        let (shown, cut) = command_detail("ls\u{34f}x");
        assert_eq!((shown.as_str(), cut), ("ls\u{34f}x", false));
    }

    #[test]
    fn the_cap_counts_characters_not_bytes() {
        let ok = "é".repeat(DETAIL_CHARS);
        assert!(ok.len() > DETAIL_CHARS);
        let (obs, _) = obs_for(Some("claude"), &command_view(&ok));
        assert_eq!(obs.answer_path, AnswerPath::T3);
        assert_eq!(obs.detail, ok);
        let (obs, target) = obs_for(Some("claude"), &command_view(&"é".repeat(DETAIL_CHARS + 1)));
        assert_eq!(obs.answer_path, AnswerPath::None);
        assert_eq!(target.answer, TargetAnswer::ReadOnly);
    }
}
