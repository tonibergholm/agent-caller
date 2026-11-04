//! acd/1 frames in both directions. Invariants: parse(serialise(x)) == x,
//! the serialised form satisfies the committed JSON schema, and the Debug
//! form of a client message never shows its token or typed text.
#![no_main]
use acd_protocol::{ClientMessage, ServerMessage, decode_server, schema};
use libfuzzer_sys::fuzz_target;
use std::sync::OnceLock;

/// Whether `v` satisfies the schema. The decoders are more tolerant than the
/// schema on purpose (a `result` with `ok: false` and no error is read as a
/// bad request), so the output is only held to the schema when the input met it.
fn valid(cell: &'static OnceLock<jsonschema::Validator>, schema: fn() -> serde_json::Value, v: &serde_json::Value) -> bool {
    validator(cell, schema).is_valid(v)
}

fn validator(cell: &'static OnceLock<jsonschema::Validator>, schema: fn() -> serde_json::Value) -> &'static jsonschema::Validator {
    cell.get_or_init(|| jsonschema::validator_for(&schema()).unwrap())
}

fn secrets(m: &ClientMessage) -> Option<(&str, String)> {
    match m {
        ClientMessage::Hello { token, protocol, client, resume } => {
            let others = format!("{protocol}{}{}{}{:?}", client.id, client.name, client.version, resume);
            Some((token, others))
        }
        ClientMessage::RequestAnswer { text: Some(t), id, request_id, option_id, .. } => {
            Some((t, format!("{id}{request_id}{option_id:?}")))
        }
        ClientMessage::AgentSend { text, id, agent_id } => Some((text, format!("{id}{agent_id}"))),
        _ => None,
    }
}

fuzz_target!(|data: &[u8]| {
    static CLIENT: OnceLock<jsonschema::Validator> = OnceLock::new();
    static SERVER: OnceLock<jsonschema::Validator> = OnceLock::new();
    let Ok(raw) = std::str::from_utf8(data) else { return };
    // Daemon direction.
    if let Ok(Some(m)) = decode_server(raw) {
        let again = serde_json::to_string(&m).unwrap();
        let back = decode_server(&again).unwrap_or_else(|e| panic!("own output {again:?} fails: {e}"));
        assert_eq!(back.as_ref(), Some(&m), "round trip of {again:?}");
        let v: serde_json::Value = serde_json::from_str(&again).unwrap();
        let input: serde_json::Value = serde_json::from_str(raw).unwrap();
        if valid(&SERVER, schema::server_schema, &input) {
            let errors: Vec<String> = validator(&SERVER, schema::server_schema).iter_errors(&v).map(|e| e.to_string()).collect();
            assert!(errors.is_empty(), "{again:?} fails the server schema: {errors:?}");
        }
        let _ = m.seq();
    }
    if let Ok(m) = serde_json::from_str::<ServerMessage>(raw) {
        assert_eq!(decode_server(raw).unwrap().as_ref(), Some(&m));
    }
    // Client direction (what the daemon reads off a socket).
    if let Ok(m) = serde_json::from_str::<ClientMessage>(raw) {
        let again = serde_json::to_string(&m).unwrap();
        let back: ClientMessage = serde_json::from_str(&again).unwrap_or_else(|e| panic!("own output {again:?} fails: {e}"));
        assert_eq!(back, m, "round trip of {again:?}");
        let v: serde_json::Value = serde_json::from_str(&again).unwrap();
        let input: serde_json::Value = serde_json::from_str(raw).unwrap();
        if valid(&CLIENT, schema::client_schema, &input) {
            let errors: Vec<String> = validator(&CLIENT, schema::client_schema).iter_errors(&v).map(|e| e.to_string()).collect();
            assert!(errors.is_empty(), "{again:?} fails the client schema: {errors:?}");
        }
        let _ = m.command_id();
        if let Some((secret, others)) = secrets(&m) {
            let dbg = format!("{m:?}");
            if secret.len() >= 8 && !others.contains(secret) && !dbg.is_empty() {
                assert!(!dbg.contains(secret), "Debug shows a secret: {dbg}");
            }
        }
    }
});
