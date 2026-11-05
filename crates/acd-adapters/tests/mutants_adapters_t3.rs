//! Test added from the second cargo-mutants run over the question read-only
//! gate (docs/research/2026-11-04-mutation-testing.md).
use acd_adapters::t3::{detail::ThreadDetail, fake::question_projection, map::*};
use acd_protocol::{AnswerPath, RequestKind};
use serde_json::json;

/// map.rs:239 match guard `!questions.is_empty()` -> true in `request_obs`: a
/// question request that carries no questions has nothing to show or answer.
/// It is published as an unanswerable card, and mapping it does not index into
/// the empty list.
#[test]
fn a_question_request_without_questions_is_not_answerable() {
    let mut projection = question_projection("thread", "request");
    let item = projection["turnItems"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|item| item["type"] == "user_input_request")
        .unwrap();
    item["questions"] = json!([]);
    let (detail, _) = ThreadDetail::from_projection(&projection);
    let view = detail.pending_view("request").expect("the request is still pending");
    let (obs, target) = request_obs("t3:thread", "thread", Some("claudeAgent"), &view);
    assert_ne!(obs.kind, RequestKind::Question);
    assert_eq!(obs.answer_path, AnswerPath::None);
    assert!(!obs.free_text && obs.questions.is_none());
    assert!(matches!(target.answer, TargetAnswer::ReadOnly));
}
