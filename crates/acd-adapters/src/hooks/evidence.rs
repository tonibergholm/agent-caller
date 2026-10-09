//! Correlated outcome evidence (spec §5.1 Attribution, §6.2 step 3, §6.3
//! Completion). Pure rules here; `EvidenceHub` (Task 16) feeds the keys
//! engine with them.
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use acd_protocol::Effect;
use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::broadcast;
use tokio::time::Instant;

use super::wire::{HookEvent, HookName, command_of, tool_digest};
pub use crate::herdr::keys::Verdict;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expect {
    ClaudeTool {
        session_id: String,
        tool_name: String,
        digest: String,
    },
    ClaudeQuestion {
        session_id: String,
        question: String,
        labels: HashMap<String, String>,
    },
    CodexTool {
        session_id: String,
        tool_name: String,
        command: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Seen {
    ToolRan,
    TurnEnded,
    Answered(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sent {
    Allow,
    Deny,
    Choose(String),
}

pub fn sent_for_effect(e: Effect) -> Option<Sent> {
    match e {
        Effect::Allow | Effect::AllowAlways => Some(Sent::Allow),
        Effect::Deny => Some(Sent::Deny),
        Effect::Choose | Effect::Unknown => None,
    }
}

fn session_of(e: &Expect) -> &str {
    match e {
        Expect::ClaudeTool { session_id, .. }
        | Expect::ClaudeQuestion { session_id, .. }
        | Expect::CodexTool { session_id, .. } => session_id,
    }
}

/// What `ev` says about the expected request, if anything.
pub fn observe(expect: &Expect, ev: &HookEvent) -> Option<Seen> {
    if ev.payload.session_id != session_of(expect) {
        return None;
    }
    let turn_end = matches!(ev.name, HookName::Stop | HookName::UserPromptSubmit);
    // A failed or interrupted tool still ran: it was allowed.
    let post = matches!(
        ev.name,
        HookName::PostToolUse | HookName::PostToolUseFailure
    );
    let tool = ev.payload.tool_name.as_deref();
    match expect {
        Expect::ClaudeTool {
            tool_name, digest, ..
        } => {
            if post
                && tool == Some(tool_name.as_str())
                && tool_digest(tool_name, &ev.payload.tool_input) == *digest
            {
                Some(Seen::ToolRan)
            } else {
                turn_end.then_some(Seen::TurnEnded)
            }
        }
        Expect::ClaudeQuestion { question, .. } => {
            if post && tool == Some("AskUserQuestion") {
                // Sol R3-2: identity evidence only when the answers map has
                // this question with a string answer; another question's
                // outcome is no evidence at all.
                let answers = ev
                    .payload
                    .tool_response
                    .get("answers")
                    .or_else(|| ev.payload.tool_input.get("answers"));
                answers
                    .and_then(|a| a.get(question.as_str()))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .map(Seen::Answered)
            } else {
                turn_end.then_some(Seen::TurnEnded)
            }
        }
        Expect::CodexTool {
            tool_name, command, ..
        } => {
            if post
                && tool == Some(tool_name.as_str())
                && command_of(&ev.payload.tool_input) == Some(command.as_str())
            {
                Some(Seen::ToolRan)
            } else {
                turn_end.then_some(Seen::TurnEnded)
            }
        }
    }
}

/// O7: the evidence identity of an expectation (Sol R3-2). Question
/// identity is the question text; option labels are not part of it,
/// because answer evidence names only the question and the chosen label.
pub fn same_identity(a: &Expect, b: &Expect) -> bool {
    match (a, b) {
        (
            Expect::ClaudeTool {
                session_id: s1,
                tool_name: t1,
                digest: d1,
            },
            Expect::ClaudeTool {
                session_id: s2,
                tool_name: t2,
                digest: d2,
            },
        ) => s1 == s2 && t1 == t2 && d1 == d2,
        (
            Expect::ClaudeQuestion {
                session_id: s1,
                question: q1,
                ..
            },
            Expect::ClaudeQuestion {
                session_id: s2,
                question: q2,
                ..
            },
        ) => s1 == s2 && q1 == q2,
        (
            Expect::CodexTool {
                session_id: s1,
                tool_name: t1,
                command: c1,
            },
            Expect::CodexTool {
                session_id: s2,
                tool_name: t2,
                command: c2,
            },
        ) => s1 == s2 && t1 == t2 && c1 == c2,
        _ => false,
    }
}

/// O7: evidence that names an occurrence's identity, as opposed to a
/// turn end.
pub fn is_identity(seen: &Seen) -> bool {
    matches!(seen, Seen::ToolRan | Seen::Answered(_))
}

/// O7: a shadowed occurrence ignores identity evidence; turn ends apply.
pub fn observe_shadowed(expect: &Expect, ev: &HookEvent, shadowed: bool) -> Option<Seen> {
    observe(expect, ev).filter(|seen| !(shadowed && is_identity(seen)))
}

pub fn verdict(sent: &Sent, seen: &Seen) -> Verdict {
    match (sent, seen) {
        (Sent::Allow, Seen::ToolRan) | (Sent::Deny, Seen::TurnEnded) => Verdict::Consistent,
        (Sent::Choose(want), Seen::Answered(got)) if want == got => Verdict::Consistent,
        _ => Verdict::Contradicts,
    }
}

const RECENT: usize = 256;

/// Per-request evidence provider for herdr's guarded keys engine.
pub struct EvidenceHub {
    expects: Mutex<HashMap<String, (Expect, bool)>>,
    recent: Mutex<VecDeque<HookEvent>>,
    tx: broadcast::Sender<HookEvent>,
}

impl Default for EvidenceHub {
    fn default() -> Self {
        Self {
            expects: Mutex::default(),
            recent: Mutex::default(),
            tx: broadcast::channel(RECENT).0,
        }
    }
}

impl EvidenceHub {
    pub fn expect(&self, request_id: &str, e: Expect, shadowed: bool) {
        self.expects
            .lock()
            .expect("evidence")
            .insert(request_id.to_owned(), (e, shadowed));
    }

    pub fn forget(&self, request_id: &str) {
        self.expects.lock().expect("evidence").remove(request_id);
    }

    pub fn publish(&self, ev: &HookEvent) {
        let mut recent = self.recent.lock().expect("evidence");
        recent.push_back(ev.clone());
        while recent.len() > RECENT {
            recent.pop_front();
        }
        drop(recent);
        let _ = self.tx.send(ev.clone());
    }
}

pub fn sent_for_option(expect: &Expect, option_id: &str) -> Option<Sent> {
    match expect {
        Expect::ClaudeQuestion { labels, .. } => labels.get(option_id).cloned().map(Sent::Choose),
        _ => match option_id {
            "allow" | "allow_always" => Some(Sent::Allow),
            "deny" => Some(Sent::Deny),
            _ => None,
        },
    }
}

#[async_trait]
impl crate::herdr::keys::Evidence for EvidenceHub {
    async fn outcome(
        &self,
        target: &crate::herdr::keys::KeysTarget,
        option_id: &str,
        sent_at: Instant,
    ) -> Verdict {
        let Some((expect, shadowed)) = self
            .expects
            .lock()
            .expect("evidence")
            .get(target.request_id())
            .cloned()
        else {
            return Verdict::NoEvidence;
        };
        let Some(sent) = sent_for_option(&expect, option_id) else {
            return Verdict::NoEvidence;
        };
        let mut rx = self.tx.subscribe();
        let backlog: Vec<_> = self
            .recent
            .lock()
            .expect("evidence")
            .iter()
            .filter(|ev| ev.at >= sent_at)
            .cloned()
            .collect();
        for ev in &backlog {
            if let Some(seen) = observe_shadowed(&expect, ev, shadowed) {
                return if shadowed {
                    Verdict::NoEvidence
                } else {
                    verdict(&sent, &seen)
                };
            }
        }
        loop {
            match rx.recv().await {
                Ok(ev) if ev.at >= sent_at => {
                    if let Some(seen) = observe_shadowed(&expect, &ev, shadowed) {
                        return if shadowed {
                            Verdict::NoEvidence
                        } else {
                            verdict(&sent, &seen)
                        };
                    }
                }
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return Verdict::NoEvidence,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::wire::{Hints, HookAgent, Payload};

    fn ev(rel: &str, name: HookName) -> HookEvent {
        let p = format!(
            "{}/../../tests/fixtures/hooks/{rel}",
            env!("CARGO_MANIFEST_DIR")
        );
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
        HookEvent {
            agent: HookAgent::Claude,
            name,
            payload: Payload::from_value(&v).unwrap(),
            hints: Hints::default(),
            at: tokio::time::Instant::now(),
        }
    }
    fn bash(command: &str) -> Expect {
        let input = serde_json::json!({"command": command, "description": "Create empty file"});
        Expect::ClaudeTool {
            session_id: "4e3885ee-7454-410d-a2e2-86f4dc2533c0".into(),
            tool_name: "Bash".into(),
            digest: crate::hooks::wire::tool_digest("Bash", &input),
        }
    }

    fn target(rid: &str) -> crate::herdr::keys::KeysTarget {
        let dialog = std::fs::read_to_string(format!(
            "{}/../../tests/fixtures/herdr/claude-permission-bash.txt",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        crate::herdr::keys::KeysTarget::from_screen(
            rid.into(),
            "a".into(),
            acd_core::CorrelationKey("k".into()),
            &dialog,
            [
                ("allow".to_string(), "1".to_string()),
                ("deny".to_string(), "4".to_string()),
            ]
            .into(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn hub_correlates_only_post_send_evidence_to_its_request() {
        use crate::herdr::keys::Evidence as _;
        let hub = EvidenceHub::default();
        hub.expect("r1", bash("touch /tmp/acp0/t2.txt"), false);
        let mut post = ev(
            "claude/payload-PostToolUse-Bash.json",
            HookName::PostToolUse,
        );
        hub.publish(&post);
        let sent_at = Instant::now();
        post.at = Instant::now();
        hub.publish(&post);
        assert_eq!(
            hub.outcome(&target("r1"), "allow", sent_at).await,
            Verdict::Consistent
        );
        assert_eq!(
            hub.outcome(&target("r1"), "deny", sent_at).await,
            Verdict::Contradicts
        );
        assert_eq!(
            hub.outcome(&target("r2"), "allow", sent_at).await,
            Verdict::NoEvidence
        );
        hub.forget("r1");
        assert_eq!(
            hub.outcome(&target("r1"), "allow", sent_at).await,
            Verdict::NoEvidence
        );
    }

    #[tokio::test]
    async fn hub_waits_for_live_evidence_and_ignores_shadowed_identity() {
        use crate::herdr::keys::Evidence as _;
        let hub = std::sync::Arc::new(EvidenceHub::default());
        hub.expect("r1", bash("touch /tmp/acp0/t2.txt"), true);
        let sent_at = Instant::now();
        let h = hub.clone();
        let waiting = tokio::spawn(async move { h.outcome(&target("r1"), "allow", sent_at).await });
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        let mut post = ev(
            "claude/payload-PostToolUse-Bash.json",
            HookName::PostToolUse,
        );
        post.at = Instant::now();
        hub.publish(&post);
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert!(!waiting.is_finished());
        let mut stop = ev("claude/payload-Stop.json", HookName::Stop);
        stop.at = Instant::now();
        hub.publish(&stop);
        assert_eq!(waiting.await.unwrap(), Verdict::NoEvidence);
    }

    #[tokio::test]
    async fn claude_allow_and_deny_follow_section_6_2() {
        let post = ev(
            "claude/payload-PostToolUse-Bash.json",
            HookName::PostToolUse,
        );
        let stop = ev("claude/payload-Stop.json", HookName::Stop);
        let e = bash("touch /tmp/acp0/t2.txt");
        assert_eq!(observe(&e, &post), Some(Seen::ToolRan));
        assert_eq!(
            observe(&bash("touch /tmp/acp0/other"), &post),
            None,
            "another tool call is no evidence"
        );
        assert_eq!(observe(&e, &stop), Some(Seen::TurnEnded));
        assert_eq!(verdict(&Sent::Allow, &Seen::ToolRan), Verdict::Consistent);
        assert_eq!(
            verdict(&Sent::Allow, &Seen::TurnEnded),
            Verdict::Contradicts
        );
        assert_eq!(verdict(&Sent::Deny, &Seen::TurnEnded), Verdict::Consistent);
        assert_eq!(verdict(&Sent::Deny, &Seen::ToolRan), Verdict::Contradicts);
    }

    #[tokio::test]
    async fn other_sessions_are_never_evidence() {
        let mut stop = ev("claude/payload-Stop.json", HookName::Stop);
        stop.payload.session_id = "someone-else".into();
        assert_eq!(observe(&bash("x"), &stop), None);
    }

    #[tokio::test]
    async fn questions_compare_the_answer_label() {
        let post = ev(
            "claude/payload-PostToolUse-AskUserQuestion.json",
            HookName::PostToolUse,
        );
        let e = question("Pick a color", &[("o1", "Red"), ("o2", "Blue")]);
        assert_eq!(observe(&e, &post), Some(Seen::Answered("Red".into())));
        assert_eq!(
            verdict(&Sent::Choose("Red".into()), &Seen::Answered("Red".into())),
            Verdict::Consistent
        );
        assert_eq!(
            verdict(&Sent::Choose("Blue".into()), &Seen::Answered("Red".into())),
            Verdict::Contradicts
        );
    }

    #[tokio::test]
    async fn a_shadowed_occurrence_sees_only_turn_ends() {
        // O7: identity evidence may belong to an older identical occurrence.
        let post = ev(
            "claude/payload-PostToolUse-Bash.json",
            HookName::PostToolUse,
        );
        let e = bash(post.payload.tool_input["command"].as_str().unwrap());
        assert_eq!(observe_shadowed(&e, &post, false), Some(Seen::ToolRan));
        assert_eq!(observe_shadowed(&e, &post, true), None);
        let stop = ev("claude/payload-Stop.json", HookName::Stop);
        assert_eq!(observe_shadowed(&e, &stop, true), Some(Seen::TurnEnded));
        assert!(
            is_identity(&Seen::ToolRan)
                && is_identity(&Seen::Answered("x".into()))
                && !is_identity(&Seen::TurnEnded)
        );
    }

    fn question(q: &str, labels: &[(&str, &str)]) -> Expect {
        Expect::ClaudeQuestion {
            session_id: "4e3885ee-7454-410d-a2e2-86f4dc2533c0".into(),
            question: q.into(),
            labels: labels
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    #[test]
    fn question_identity_ignores_option_labels() {
        // Sol R3-2.
        let a = question("Pick a color", &[("o1", "Red"), ("o2", "Blue")]);
        let b = question("Pick a color", &[("o1", "Blue"), ("o2", "Green")]);
        assert!(same_identity(&a, &b));
        assert!(!same_identity(
            &a,
            &question("Pick a size", &[("o1", "Red")])
        ));
        assert!(!same_identity(&a, &bash("x")));
    }

    #[tokio::test]
    async fn another_questions_outcome_is_no_evidence() {
        let post = ev(
            "claude/payload-PostToolUse-AskUserQuestion.json",
            HookName::PostToolUse,
        );
        assert_eq!(
            observe(&question("A different question?", &[("o1", "Red")]), &post),
            None
        );
        let mut bad = post.clone();
        bad.payload.tool_response = serde_json::json!({"answers": {"Pick a color": 3}});
        bad.payload.tool_input = serde_json::json!({});
        assert_eq!(
            observe(&question("Pick a color", &[("o1", "Red")]), &bad),
            None,
            "not a string answer"
        );
    }

    #[tokio::test]
    async fn a_failed_tool_still_counts_as_run() {
        let mut fail = ev(
            "claude/payload-PostToolUse-Bash.json",
            HookName::PostToolUseFailure,
        );
        fail.payload.tool_response = serde_json::json!({});
        let e = bash("touch /tmp/acp0/t2.txt");
        let seen = observe(&e, &fail);
        assert_eq!(seen, Some(Seen::ToolRan));
        assert_eq!(
            verdict(&Sent::Allow, &seen.clone().unwrap()),
            Verdict::Consistent
        );
        assert_eq!(verdict(&Sent::Deny, &seen.unwrap()), Verdict::Contradicts);
        assert_eq!(observe(&bash("other"), &fail), None);
    }

    #[test]
    fn choose_with_a_turn_end_contradicts() {
        assert_eq!(
            verdict(&Sent::Choose("Red".into()), &Seen::TurnEnded),
            Verdict::Contradicts
        );
    }
}
