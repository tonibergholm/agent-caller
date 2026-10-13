//! Plan V3: the JSON Swift's `PlanJSON` writes is what Rust decodes. The
//! Swift side checks the same fixtures (PlanContractTests.swift).
use acd_voice::router::model::{ModelPlan, ModelStep, plan_schema};
use serde_json::{Value, json};

fn fixture(name: &str) -> String {
    let path = format!("{}/../../tests/fixtures/voice/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("missing fixture {path}"))
}

/// Every variant, by name; a new variant does not compile until it is listed.
fn wire(step: ModelStep) -> &'static str {
    match step {
        ModelStep::Answer => "answer",
        ModelStep::AnswerText => "answer_text",
        ModelStep::Send => "send",
        ModelStep::Cancel => "cancel",
        ModelStep::Read => "read",
        ModelStep::Summarize => "summarize",
        ModelStep::Select => "select",
        ModelStep::None => "none",
    }
}

#[test]
fn the_swift_fixtures_decode_and_match_the_schema() {
    let schema = plan_schema();
    let v = jsonschema::validator_for(&schema).unwrap();
    for name in ["plan-full.json", "plan-min.json"] {
        let plan: Value = serde_json::from_str(&fixture(name)).unwrap();
        assert!(v.is_valid(&plan), "{name}");
        assert!(serde_json::from_value::<ModelPlan>(plan).is_ok(), "{name}");
    }
    let full: Value = serde_json::from_str(&fixture("plan-full.json")).unwrap();
    let mut keys: Vec<&String> = full.as_object().unwrap().keys().collect();
    let mut props: Vec<&String> = schema["properties"].as_object().unwrap().keys().collect();
    keys.sort();
    props.sort();
    assert_eq!(keys, props, "the full fixture sets every field the schema has, and no other");
}

#[test]
fn the_step_names_are_every_variant() {
    let steps: Vec<String> = serde_json::from_str(&fixture("plan-steps.json")).unwrap();
    let mut decoded = Vec::new();
    for s in &steps {
        let step: ModelStep = serde_json::from_value(json!(s)).unwrap_or_else(|_| panic!("{s}"));
        assert_eq!(wire(step), s);
        decoded.push(step);
    }
    assert_eq!(decoded.len(), 8, "{steps:?}");
}
