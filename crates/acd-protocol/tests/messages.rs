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
        if path.extension().and_then(|e| e.to_str()) != Some("json") { continue; }
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
        assert_eq!(serde_json::to_value(&parsed).unwrap(), original, "{} does not round-trip", path.display());
    }
}

/// Validates every example in `rel` against the committed schema `name`.
fn validates_against_schema(rel: &str, name: &str) {
    let raw = fs::read_to_string(dir("schema").join(format!("{name}.schema.json"))).unwrap();
    let validator = jsonschema::validator_for(&serde_json::from_str(&raw).unwrap()).unwrap();
    for (path, v) in examples(rel) {
        let errors: Vec<String> = validator.iter_errors(&v).map(|e| e.to_string()).collect();
        assert!(errors.is_empty(), "{} fails {name}.schema.json: {errors:?}", path.display());
    }
}

fn example_types(rel: &str) -> Vec<String> {
    let mut types: Vec<String> = examples(rel).into_iter().map(|(_, v)| v["type"].as_str().unwrap().to_string()).collect();
    types.sort();
    types.dedup();
    types
}

#[test]
fn client_golden_examples_round_trip() { golden::<ClientMessage>("examples/client"); }

#[test]
fn server_golden_examples_round_trip() { golden::<ServerMessage>("examples/server"); }

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
    let mut client = vec!["hello", "request.answer", "agent.send", "agent.mark_seen", "ping"];
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
    assert!(decode_server(r#"{"type":"welcome"}"#).is_err(), "known type with an invalid body");
}

#[test]
fn unknown_error_code_decodes_as_unknown() {
    let m = decode_server(r#"{"type":"result","id":"c","ok":false,"error":{"code":"quota_exceeded","message":"m"}}"#)
        .unwrap()
        .unwrap();
    let ServerMessage::Result(r) = m else { panic!("expected result") };
    assert_eq!(r.error_code(), Some(ErrorCode::Unknown));
}

#[test]
fn unknown_client_fields_are_ignored() {
    let m: ClientMessage = serde_json::from_str(
        r#"{"type":"agent.send","id":"1","agent_id":"a","text":"hi","priority":"high","meta":{"x":1}}"#).unwrap();
    assert_eq!(m, ClientMessage::AgentSend { id: "1".into(), agent_id: "a".into(), text: "hi".into() });
}

#[test]
fn command_ids_and_seq_accessors() {
    let m: ClientMessage = serde_json::from_str(r#"{"type":"ping","id":"p1"}"#).unwrap();
    assert_eq!(m.command_id(), Some("p1"));
    let s: ServerMessage = serde_json::from_str(
        r#"{"type":"agent.removed","epoch":"e","seq":9,"agent_id":"a"}"#).unwrap();
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
        ("client", serde_json::to_string_pretty(&schemars::schema_for!(ClientMessage)).unwrap()),
        ("server", serde_json::to_string_pretty(&schemars::schema_for!(ServerMessage)).unwrap()),
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
        assert_eq!(committed.trim_end(), schema, "schema drift in {name}; run with UPDATE_SCHEMA=1 and commit");
    }
}
