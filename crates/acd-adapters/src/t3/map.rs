//! Ids, state and request mapping, and command payloads (spec §6.5).
use std::collections::HashSet;

use acd_core::{AgentObs, Answer, CorrelationKey, RequestObs};
use acd_protocol::{
    AgentKind, AgentState, AnswerPath, Effect, Question, Request, RequestKind, RequestOption,
    Source,
};
use serde_json::{Value, json};

use super::detail::PendingView;
use super::wire::{Project, ShellThread, TurnItem};
use crate::text::{LAST_MESSAGE_CHARS, TITLE_CHARS, cap_chars, clean_display, clean_screen};

pub const DETAIL_CHARS: usize = 4000;
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
    format!("t3:{thread_id}")
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

fn message_text(v: &Value) -> Option<&str> {
    v.as_str().or_else(|| v.get("text").and_then(Value::as_str))
}

pub fn agent_obs(t: &ShellThread, project: Option<&Project>, hidden: &HashSet<String>) -> AgentObs {
    let needs_you = t
        .pending_runtime_request
        .as_ref()
        .is_some_and(|p| !hidden.contains(&p.id));
    let (state, failed) = state(t.status.as_deref(), needs_you);
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

pub fn request_obs(agent_id: &str, thread_id: &str, view: &PendingView) -> (RequestObs, Target) {
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
                .map(str::trim)
                .filter(|c| !c.is_empty())
            {
                Some(c) => {
                    let text = if !prompt.is_empty() && c != prompt {
                        format!("{c}\n{prompt}")
                    } else {
                        c.to_owned()
                    };
                    obs.detail = detail(&text);
                    obs.answer_path = AnswerPath::T3;
                    obs.options = FALLBACK
                        .iter()
                        .map(|(id, label, effect)| RequestOption {
                            id: (*id).into(),
                            label: (*label).into(),
                            description: None,
                            effect: *effect,
                        })
                        .collect();
                    answer = TargetAnswer::Decision;
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
                        })
                        .collect(),
                })
                .collect();
            let q0 = &questions[0];
            obs.kind = RequestKind::Question;
            obs.title = qs[0].header.clone().unwrap_or_else(|| "Question".into());
            obs.detail = qs[0].text.clone();
            // Only a single single-select question can be answered.
            if questions.len() == 1 && !q0.multi_select {
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
                };
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
    let Answer::Option(option_id) = answer else {
        return Err("T3 requests take an option, not text");
    };
    let mut p = json!({
        "type": "runtime-request.respond", "commandId": command_id,
        "threadId": target.thread_id, "requestId": target.t3_request_id,
    });
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
