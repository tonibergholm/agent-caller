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

/// The same message with its token or typed text replaced by a different text
/// of the same byte length. `Debug` shows only that length, so the two must
/// render identically; a leak of the secret would tell them apart. Comparing
/// against this baseline needs no search for the secret in the output, which
/// also prints public field names such as `text_bytes`.
fn swapped(m: &ClientMessage) -> Option<ClientMessage> {
    let other = |s: &str| {
        let c = if s.starts_with('x') { 'y' } else { 'x' };
        c.to_string().repeat(s.len())
    };
    let mut n = m.clone();
    match &mut n {
        ClientMessage::Hello { token, .. } => *token = other(token),
        ClientMessage::RequestAnswer { text: Some(t), .. } => *t = other(t),
        ClientMessage::AgentSend { text, .. } => *text = other(text),
        _ => return None,
    }
    Some(n)
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
        if let Some(n) = swapped(&m) {
            assert_eq!(format!("{m:?}"), format!("{n:?}"), "Debug shows a secret: {m:?}");
        }
    }
});
