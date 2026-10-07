use acd_protocol::*;
use std::{fs, path::Path};

fn dir(rel: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn golden<T: serde::de::DeserializeOwned + serde::Serialize>(rel: &str) {
    let mut n = 0;
    for entry in fs::read_dir(dir(rel)).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") { continue; }
        let raw = fs::read_to_string(&path).unwrap();
        let original: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let parsed: T = serde_json::from_value(original.clone())
            .unwrap_or_else(|e| panic!("{} does not parse: {e}", path.display()));
        assert_eq!(serde_json::to_value(&parsed).unwrap(), original, "{} does not round-trip", path.display());
        n += 1;
    }
    assert!(n > 0, "no examples in {rel}");
}

#[test]
fn client_golden_examples_round_trip() { golden::<ClientMessage>("examples/client"); }

#[test]
fn server_golden_examples_round_trip() { golden::<ServerMessage>("examples/server"); }

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
