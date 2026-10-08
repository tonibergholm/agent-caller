//! JSON Schema generators. The committed files in `schema/` are exactly
//! `to_string_pretty` of these values.
use serde_json::{Value, json};

use crate::{ClientMessage, ServerMessage};

/// Adds `rule` to the `allOf` of the tagged-enum variant whose `type` is `name`.
fn add_rule(schema: &mut Value, name: &str, rule: Value) {
    let variants = schema["oneOf"]
        .as_array_mut()
        .expect("tagged enums render as oneOf");
    let v = variants
        .iter_mut()
        .find(|v| v["properties"]["type"]["const"] == name)
        .unwrap_or_else(|| panic!("no variant {name}"));
    let all = v
        .as_object_mut()
        .expect("variant is an object")
        .entry("allOf")
        .or_insert_with(|| json!([]));
    all.as_array_mut().expect("allOf is an array").push(rule);
}

pub fn client_schema() -> Value {
    let mut s = serde_json::to_value(schemars::schema_for!(ClientMessage)).expect("schema");
    // request.answer carries exactly one of option_id or text (spec §7.3).
    // `null` counts as absent, as it does for the Rust decoder (Sol finding 14).
    add_rule(
        &mut s,
        "request.answer",
        json!({"oneOf": [
            {"required": ["option_id"], "properties": {"option_id": {"type": "string"}, "text": {"type": "null"}}},
            {"required": ["text"], "properties": {"text": {"type": "string"}, "option_id": {"type": "null"}}}
        ]}),
    );
    s
}

pub fn server_schema() -> Value {
    let mut s = serde_json::to_value(schemars::schema_for!(ServerMessage)).expect("schema");
    // result: ok=true never has error; ok=false has error and no outcome.
    add_rule(
        &mut s,
        "result",
        json!({"oneOf": [
            {"properties": {"ok": {"const": true}, "error": {"type": "null"}}},
            {"required": ["error"], "properties": {"ok": {"const": false}, "error": {"type": "object"}, "outcome": {"type": "null"}}}
        ]}),
    );
    s
}
