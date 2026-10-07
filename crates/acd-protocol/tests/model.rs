use acd_protocol::*;
use chrono::{TimeZone, Utc};
use serde_json::json;

fn opened_at() -> Timestamp {
    Utc.with_ymd_and_hms(2026, 10, 7, 12, 0, 0).unwrap()
}

fn opt(id: &str, label: &str, effect: Effect) -> RequestOption {
    RequestOption { id: id.into(), label: label.into(), description: None, effect }
}

fn permission() -> Request {
    Request {
        id: "r1".into(),
        agent_id: "herdr:abc:w1:p1".into(),
        kind: RequestKind::Permission,
        status: RequestStatus::Open,
        answer_path: AnswerPath::Hook,
        title: "Bash".into(),
        detail: "touch x".into(),
        options: vec![opt("allow", "Yes", Effect::Allow), opt("deny", "No", Effect::Deny)],
        questions: None,
        free_text: false,
        raw_screen: None,
        opened_at: opened_at(),
    }
}

fn question(questions: Vec<Question>) -> Request {
    Request { kind: RequestKind::Question, answer_path: AnswerPath::Keys, questions: Some(questions), ..permission() }
}

fn q(multi_select: bool) -> Question {
    Question { id: "q1".into(), header: None, text: "Color?".into(), multi_select, options: vec![opt("red", "Red", Effect::Choose)] }
}

#[test]
fn request_serializes_to_wire_shape() {
    assert_eq!(
        serde_json::to_value(permission()).unwrap(),
        json!({
            "id": "r1", "agent_id": "herdr:abc:w1:p1", "kind": "permission", "status": "open",
            "answer_path": "hook", "title": "Bash", "detail": "touch x",
            "options": [
                {"id": "allow", "label": "Yes", "description": null, "effect": "allow"},
                {"id": "deny", "label": "No", "description": null, "effect": "deny"}
            ],
            "questions": null, "free_text": false, "raw_screen": null,
            "opened_at": "2026-10-07T12:00:00.000Z"
        })
    );
}

#[test]
fn unknown_enum_values_and_fields_are_tolerated() {
    let mut v = serde_json::to_value(permission()).unwrap();
    v["kind"] = json!("telepathy");
    v["answer_path"] = json!("carrier_pigeon");
    v["options"][0]["effect"] = json!("maybe");
    v["future_field"] = json!({"x": 1});
    let r: Request = serde_json::from_value(v).unwrap();
    assert_eq!(r.kind, RequestKind::Unknown);
    assert_eq!(r.answer_path, AnswerPath::Unknown);
    assert_eq!(r.options[0].effect, Effect::Unknown);
    assert!(!r.is_answerable());
}

#[test]
fn unknown_agent_state_maps_to_unknown() {
    let s: AgentState = serde_json::from_value(json!("hibernating")).unwrap();
    assert_eq!(s, AgentState::Unknown);
    assert_eq!(serde_json::to_value(AgentState::NeedsYou).unwrap(), json!("needs_you"));
}

#[test]
fn answerability_rules() {
    assert!(permission().is_answerable());
    assert!(!Request { answer_path: AnswerPath::None, ..permission() }.is_answerable());
    assert!(!Request { kind: RequestKind::BlockedUnparsed, ..permission() }.is_answerable());
    assert!(question(vec![q(false)]).is_answerable());
    assert!(!question(vec![q(true)]).is_answerable());
    assert!(!question(vec![q(false), q(false)]).is_answerable());
    assert!(!Request { questions: None, ..question(vec![]) }.is_answerable());
}

#[test]
fn timestamps_round_trip_with_millisecond_utc() {
    let v = serde_json::to_value(permission()).unwrap();
    assert_eq!(v["opened_at"], json!("2026-10-07T12:00:00.000Z"));
    let back: Request = serde_json::from_value(v).unwrap();
    assert_eq!(back.opened_at, opened_at());
}
