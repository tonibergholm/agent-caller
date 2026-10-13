use acd_protocol::*;
use std::{fs, path::Path};

fn dir(rel: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// Every example in `rel`, as (path, JSON).
fn examples(rel: &str) -> Vec<(std::path::PathBuf, serde_json::Value)> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir(rel)).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let raw = fs::read_to_string(&path).unwrap();
        out.push((path, serde_json::from_str(&raw).unwrap()));
    }
    assert!(!out.is_empty(), "no examples in {rel}");
    out
}

fn golden<T: serde::de::DeserializeOwned + serde::Serialize>(rel: &str) {
    for (path, original) in examples(rel) {
        let parsed: T = serde_json::from_value(original.clone())
            .unwrap_or_else(|e| panic!("{} does not parse: {e}", path.display()));
        assert_eq!(
            serde_json::to_value(&parsed).unwrap(),
            original,
            "{} does not round-trip",
            path.display()
        );
    }
}

/// Validates every example in `rel` against the committed schema `name`.
fn validates_against_schema(rel: &str, name: &str) {
    let raw = fs::read_to_string(dir("schema").join(format!("{name}.schema.json"))).unwrap();
    let validator = jsonschema::validator_for(&serde_json::from_str(&raw).unwrap()).unwrap();
    for (path, v) in examples(rel) {
        let errors: Vec<String> = validator.iter_errors(&v).map(|e| e.to_string()).collect();
        assert!(
            errors.is_empty(),
            "{} fails {name}.schema.json: {errors:?}",
            path.display()
        );
    }
}

fn example_types(rel: &str) -> Vec<String> {
    let mut types: Vec<String> = examples(rel)
        .into_iter()
        .map(|(_, v)| v["type"].as_str().unwrap().to_string())
        .collect();
    types.sort();
    types.dedup();
    types
}

#[test]
fn client_golden_examples_round_trip() {
    golden::<ClientMessage>("examples/client");
}

#[test]
fn server_golden_examples_round_trip() {
    golden::<ServerMessage>("examples/server");
}

#[test]
fn golden_examples_validate_against_committed_schemas() {
    validates_against_schema("examples/client", "client");
    validates_against_schema("examples/server", "server");
}

#[test]
fn every_message_type_has_a_golden_example() {
    let mut server: Vec<String> = SERVER_MESSAGE_TYPES.iter().map(|t| t.to_string()).collect();
    server.sort();
    assert_eq!(example_types("examples/server"), server);
    let mut client = vec![
        "hello",
        "request.answer",
        "agent.send",
        "agent.mark_seen",
        "ping",
    ];
    client.sort();
    assert_eq!(example_types("examples/client"), client);
}

#[test]
fn decode_server_skips_unknown_types_only() {
    let unknown = decode_server(r#"{"type":"agent.launched","epoch":"e","seq":1,"x":{}}"#).unwrap();
    assert_eq!(unknown, None);
    let pong = decode_server(r#"{"type":"pong","id":"p","future":true}"#).unwrap();
    assert_eq!(pong, Some(ServerMessage::Pong { id: "p".into() }));
    assert!(decode_server("{not json").is_err(), "malformed JSON");
    assert!(decode_server("[1,2]").is_err(), "not an object");
    assert!(decode_server(r#"{"id":"p"}"#).is_err(), "missing type");
    assert!(
        decode_server(r#"{"type":"welcome"}"#).is_err(),
        "known type with an invalid body"
    );
}

#[test]
fn unknown_error_code_decodes_as_unknown() {
    let m = decode_server(
        r#"{"type":"result","id":"c","ok":false,"error":{"code":"quota_exceeded","message":"m"}}"#,
    )
    .unwrap()
    .unwrap();
    let ServerMessage::Result(r) = m else {
        panic!("expected result")
    };
    assert_eq!(r.error_code(), Some(ErrorCode::Unknown));
}

#[test]
fn unknown_client_fields_are_ignored() {
    let m: ClientMessage = serde_json::from_str(
        r#"{"type":"agent.send","id":"1","agent_id":"a","text":"hi","priority":"high","meta":{"x":1}}"#).unwrap();
    assert_eq!(
        m,
        ClientMessage::AgentSend {
            id: "1".into(),
            agent_id: "a".into(),
            text: "hi".into()
        }
    );
}

#[test]
fn command_ids_and_seq_accessors() {
    let m: ClientMessage = serde_json::from_str(r#"{"type":"ping","id":"p1"}"#).unwrap();
    assert_eq!(m.command_id(), Some("p1"));
    let s: ServerMessage =
        serde_json::from_str(r#"{"type":"agent.removed","epoch":"e","seq":9,"agent_id":"a"}"#)
            .unwrap();
    assert_eq!(s.seq(), Some(9));
    assert_eq!(ServerMessage::Pong { id: "x".into() }.seq(), None);
}

#[test]
fn unknown_client_type_is_an_error() {
    assert!(serde_json::from_str::<ClientMessage>(r#"{"type":"agent.launch","id":"1"}"#).is_err());
}

#[test]
fn committed_schemas_match_types() {
    let generated = [
        (
            "client",
            serde_json::to_string_pretty(&schema::client_schema()).unwrap(),
        ),
        (
            "server",
            serde_json::to_string_pretty(&schema::server_schema()).unwrap(),
        ),
    ];
    for (name, schema) in generated {
        let path = dir("schema").join(format!("{name}.schema.json"));
        if std::env::var_os("UPDATE_SCHEMA").is_some() {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, format!("{schema}\n")).unwrap();
            continue;
        }
        let committed = fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("missing {}; run with UPDATE_SCHEMA=1", path.display()));
        assert_eq!(
            committed.trim_end(),
            schema,
            "schema drift in {name}; run with UPDATE_SCHEMA=1 and commit"
        );
    }
}

#[test]
fn unknown_values_decode_on_every_enum() {
    let a: Agent = serde_json::from_value(serde_json::json!({
        "id": "x", "machine_id": "m", "source": "satellite", "kind": "gemini", "title": "t",
        "cwd": null, "project": null, "state": "dreaming", "state_since": "2026-10-08T00:00:00.000Z",
        "failed": false, "last_message": null, "pending": null, "can_send": false
    })).unwrap();
    assert_eq!(
        (a.source, a.kind, a.state),
        (Source::Unknown, AgentKind::Unknown, AgentState::Unknown)
    );
    let s: SourceStatus = serde_json::from_value(serde_json::json!({"state": "sleeping"})).unwrap();
    assert_eq!(s.state, SourceState::Unknown);
    let m = decode_server(r#"{"type":"request.resolved","epoch":"e","seq":3,"request_id":"r","agent_id":"a","by":"oracle"}"#)
        .unwrap().unwrap();
    assert!(matches!(
        m,
        ServerMessage::RequestResolved {
            by: ResolvedBy::Unknown,
            ..
        }
    ));
    let m = decode_server(r#"{"type":"request.resolved","epoch":"e","seq":4,"request_id":"r","agent_id":"a","by":"uncertain","reason":"cosmic_ray"}"#)
        .unwrap().unwrap();
    assert!(matches!(
        m,
        ServerMessage::RequestResolved {
            reason: Some(ResolveReason::Unknown),
            ..
        }
    ));
    let r: RequestStatus = serde_json::from_str(r#""paused""#).unwrap();
    assert_eq!(r, RequestStatus::Unknown);
}

#[test]
fn request_resolved_reason_round_trips_and_is_optional() {
    let with = r#"{"type":"request.resolved","epoch":"e1","seq":45,"request_id":"r8","agent_id":"a","by":"uncertain","reason":"stray_keystroke"}"#;
    let m: ServerMessage = serde_json::from_str(with).unwrap();
    assert!(matches!(
        &m,
        ServerMessage::RequestResolved {
            reason: Some(ResolveReason::StrayKeystroke),
            ..
        }
    ));
    assert_eq!(
        serde_json::to_value(&m).unwrap(),
        serde_json::from_str::<serde_json::Value>(with).unwrap()
    );
    let without = r#"{"type":"request.resolved","epoch":"e1","seq":44,"request_id":"r7","agent_id":"a","by":"inbox"}"#;
    let m: ServerMessage = serde_json::from_str(without).unwrap();
    assert!(matches!(
        &m,
        ServerMessage::RequestResolved { reason: None, .. }
    ));
    assert!(!serde_json::to_string(&m).unwrap().contains("reason"));
    let future = r#"{"type":"request.resolved","epoch":"e1","seq":46,"request_id":"r9","agent_id":"a","by":"uncertain","reason":"cosmic_ray"}"#;
    assert!(matches!(
        serde_json::from_str::<ServerMessage>(future).unwrap(),
        ServerMessage::RequestResolved {
            reason: Some(ResolveReason::Unknown),
            ..
        }
    ));
}

#[test]
fn request_resolved_message_round_trips_and_is_optional() {
    let with = r#"{"type":"request.resolved","epoch":"e1","seq":46,"request_id":"r9","agent_id":"a","by":"agent","reason":"rejected","message":"plan changed"}"#;
    let m: ServerMessage = serde_json::from_str(with).unwrap();
    match &m {
        ServerMessage::RequestResolved {
            by: ResolvedBy::Agent,
            reason: Some(ResolveReason::Rejected),
            message: Some(text),
            ..
        } => assert_eq!(text, "plan changed"),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        serde_json::to_value(&m).unwrap(),
        serde_json::from_str::<serde_json::Value>(with).unwrap()
    );
    let without = r#"{"type":"request.resolved","epoch":"e1","seq":44,"request_id":"r7","agent_id":"a","by":"inbox"}"#;
    let m: ServerMessage = serde_json::from_str(without).unwrap();
    assert!(matches!(
        &m,
        ServerMessage::RequestResolved { message: None, .. }
    ));
    assert!(!serde_json::to_string(&m).unwrap().contains("message"));
}

fn validator(schema: serde_json::Value) -> jsonschema::Validator {
    jsonschema::validator_for(&schema).unwrap()
}

#[test]
fn schema_enforces_answer_one_of() {
    let v = validator(schema::client_schema());
    let ok =
        serde_json::json!({"type":"request.answer","id":"1","request_id":"r","option_id":"allow"});
    let both = serde_json::json!({"type":"request.answer","id":"1","request_id":"r","option_id":"allow","text":"x"});
    let neither = serde_json::json!({"type":"request.answer","id":"1","request_id":"r"});
    let null_option =
        serde_json::json!({"type":"request.answer","id":"1","request_id":"r","option_id":null});
    let option_and_null_text = serde_json::json!({"type":"request.answer","id":"1","request_id":"r","option_id":"allow","text":null});
    let null_both = serde_json::json!({"type":"request.answer","id":"1","request_id":"r","option_id":null,"text":null});
    assert!(v.is_valid(&ok));
    assert!(
        v.is_valid(&option_and_null_text),
        "null means absent for the other field"
    );
    assert!(!v.is_valid(&both), "option_id and text together must fail");
    assert!(
        !v.is_valid(&neither),
        "neither option_id nor text must fail"
    );
    assert!(
        !v.is_valid(&null_option),
        "a null option_id is not an answer"
    );
    assert!(!v.is_valid(&null_both));
}

#[test]
fn schema_accepts_and_decodes_text_answers() {
    let v = validator(schema::client_schema());
    for answer in [
        serde_json::json!({"type":"request.answer","id":"1","request_id":"r","text":"x"}),
        serde_json::json!({"type":"request.answer","id":"1","request_id":"r","option_id":null,"text":"x"}),
    ] {
        assert!(v.is_valid(&answer));
        assert_eq!(
            serde_json::from_value::<ClientMessage>(answer).unwrap(),
            ClientMessage::RequestAnswer {
                id: "1".into(),
                request_id: "r".into(),
                revision: None,
                option_id: None,
                text: Some("x".into()),
            }
        );
    }
}

#[test]
fn request_changed_decodes_and_unknown_old_decoder_falls_back() {
    #[derive(serde::Deserialize, PartialEq, Debug)]
    #[serde(rename_all = "snake_case")]
    enum OldErrorCode {
        RequestGone,
        #[serde(other)]
        Unknown,
    }
    let decoded: ErrorCode = serde_json::from_str("\"request_changed\"").unwrap();
    assert_eq!(decoded, ErrorCode::RequestChanged);
    assert_eq!(decoded.describe(), "the request changed; review it again");
    let old_decoder: OldErrorCode = serde_json::from_str("\"request_changed\"").unwrap();
    assert_eq!(old_decoder, OldErrorCode::Unknown);
}

#[test]
fn schema_enforces_result_shape() {
    let v = validator(schema::server_schema());
    let good_ok = serde_json::json!({"type":"result","id":"1","ok":true,"outcome":"delivered"});
    let good_err = serde_json::json!({"type":"result","id":"1","ok":false,"error":{"code":"request_gone","message":"m"}});
    let ok_with_error = serde_json::json!({"type":"result","id":"1","ok":true,"error":{"code":"request_gone","message":"m"}});
    let err_without_error = serde_json::json!({"type":"result","id":"1","ok":false});
    let err_with_outcome = serde_json::json!({"type":"result","id":"1","ok":false,"outcome":"delivered","error":{"code":"request_gone","message":"m"}});
    let ok_without_outcome = serde_json::json!({"type":"result","id":"1","ok":true});
    let err_null = serde_json::json!({"type":"result","id":"1","ok":false,"error":null});
    assert!(v.is_valid(&good_ok) && v.is_valid(&good_err) && v.is_valid(&ok_without_outcome));
    assert!(!v.is_valid(&err_null), "a failure needs an error object");
    assert!(!v.is_valid(&ok_with_error));
    assert!(!v.is_valid(&err_without_error));
    assert!(!v.is_valid(&err_with_outcome));
}

#[test]
fn timestamps_are_date_time_in_the_schema() {
    let s = schema::server_schema();
    assert_eq!(
        s["$defs"]["Request"]["properties"]["opened_at"]["format"],
        "date-time"
    );
    assert_eq!(
        s["$defs"]["Agent"]["properties"]["state_since"]["format"],
        "date-time"
    );
}
