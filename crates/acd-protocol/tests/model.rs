use acd_protocol::*;
use chrono::{TimeZone, Utc};
use serde_json::json;

fn opened_at() -> Timestamp {
    Utc.with_ymd_and_hms(2026, 10, 7, 12, 0, 0).unwrap()
}

fn opt(id: &str, label: &str, effect: Effect) -> RequestOption {
    RequestOption {
        id: id.into(),
        label: label.into(),
        description: None,
        effect,
    }
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
        options: vec![
            opt("allow", "Yes", Effect::Allow),
            opt("deny", "No", Effect::Deny),
        ],
        questions: None,
        free_text: false,
        raw_screen: None,
        opened_at: opened_at(),
    }
}

fn question(questions: Vec<Question>) -> Request {
    Request {
        kind: RequestKind::Question,
        answer_path: AnswerPath::Keys,
        questions: Some(questions),
        ..permission()
    }
}

fn q(multi_select: bool) -> Question {
    Question {
        id: "q1".into(),
        header: None,
        text: "Color?".into(),
        multi_select,
        options: vec![opt("red", "Red", Effect::Choose)],
    }
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
    assert_eq!(
        serde_json::to_value(AgentState::NeedsYou).unwrap(),
        json!("needs_you")
    );
}

#[test]
fn answerability_rules() {
    assert!(permission().is_answerable());
    assert!(
        !Request {
            answer_path: AnswerPath::None,
            ..permission()
        }
        .is_answerable()
    );
    assert!(
        !Request {
            kind: RequestKind::BlockedUnparsed,
            ..permission()
        }
        .is_answerable()
    );
    assert!(question(vec![q(false)]).is_answerable());
    assert!(!question(vec![q(true)]).is_answerable());
    assert!(!question(vec![q(false), q(false)]).is_answerable());
    assert!(
        !Request {
            questions: None,
            ..question(vec![])
        }
        .is_answerable()
    );
}

#[test]
fn timestamps_round_trip_with_millisecond_utc() {
    let v = serde_json::to_value(permission()).unwrap();
    assert_eq!(v["opened_at"], json!("2026-10-07T12:00:00.000Z"));
    let back: Request = serde_json::from_value(v).unwrap();
    assert_eq!(back.opened_at, opened_at());
}

#[test]
fn unknown_answer_path_alone_makes_a_request_read_only() {
    let mut v = serde_json::to_value(permission()).unwrap();
    v["answer_path"] = json!("carrier_pigeon");
    let r: Request = serde_json::from_value(v).unwrap();
    assert_eq!(
        (r.kind, r.answer_path),
        (RequestKind::Permission, AnswerPath::Unknown)
    );
    assert!(!r.is_answerable());
}

#[test]
fn source_status_omits_an_absent_message() {
    assert_eq!(
        serde_json::to_value(SourceStatus::unconfigured()).unwrap(),
        json!({"state": "unconfigured"})
    );
    let degraded = SourceStatus {
        state: SourceState::Degraded,
        message: Some("t3 unreachable".into()),
    };
    assert_eq!(
        serde_json::to_value(&degraded).unwrap(),
        json!({"state": "degraded", "message": "t3 unreachable"})
    );
}

#[test]
fn machine_serializes_to_wire_shape() {
    let m = Machine {
        id: "mac-1a2b3c4d".into(),
        name: "Mac".into(),
        acd_version: "0.1.0".into(),
        sources: Sources {
            hooks: SourceStatus::unconfigured(),
            herdr: SourceStatus {
                state: SourceState::Ok,
                message: None,
            },
            t3: SourceStatus {
                state: SourceState::Disabled,
                message: None,
            },
        },
    };
    assert_eq!(
        serde_json::to_value(&m).unwrap(),
        json!({
            "id": "mac-1a2b3c4d", "name": "Mac", "acd_version": "0.1.0",
            "sources": {"hooks": {"state": "unconfigured"}, "herdr": {"state": "ok"}, "t3": {"state": "disabled"}}
        })
    );
}

#[test]
fn agent_serializes_to_wire_shape() {
    let a = Agent {
        id: "t3:th_1".into(),
        machine_id: "m".into(),
        source: Source::T3,
        kind: AgentKind::Codex,
        title: "fix bug".into(),
        cwd: None,
        project: Some("app".into()),
        state: AgentState::NeedsYou,
        state_since: opened_at(),
        failed: false,
        last_message: Some("done?".into()),
        pending: Some(permission()),
        can_send: true,
    };
    let v = serde_json::to_value(&a).unwrap();
    assert_eq!(
        v,
        json!({
            "id": "t3:th_1", "machine_id": "m", "source": "t3", "kind": "codex", "title": "fix bug",
            "cwd": null, "project": "app", "state": "needs_you", "state_since": "2026-10-07T12:00:00.000Z",
            "failed": false, "last_message": "done?", "pending": serde_json::to_value(permission()).unwrap(),
            "can_send": true
        })
    );
    assert_eq!(serde_json::from_value::<Agent>(v).unwrap(), a);
}
