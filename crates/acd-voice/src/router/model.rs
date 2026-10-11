//! The intent model contract (spec §5.4). The model proposes one step as
//! JSON and has no tools; Rust parses and checks everything it returns.
use std::fmt;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::router::prompt::Prompt;
use crate::text::VoiceText;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelStep {
    Answer,
    AnswerText,
    Send,
    Cancel,
    Read,
    Summarize,
    Select,
    None,
}

/// One step. A targeted step names `agent` or `request` by the ids in the
/// prompt, or lists `candidates` (agent ids) when unsure (spec §5.4).
#[derive(Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelPlan {
    pub step: ModelStep,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub request: Option<String>,
    /// For `answer`: the option's id as listed, "1" to "9".
    #[serde(default)]
    pub option: Option<String>,
    #[serde(default)]
    pub text: Option<VoiceText>,
    /// For `summarize`: a machine name.
    #[serde(default)]
    pub machine: Option<String>,
    #[serde(default)]
    pub candidates: Option<Vec<String>>,
}

/// Model output is never shown (spec §6.3): only which fields are set.
impl fmt::Debug for ModelPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ModelPlan")
            .field("step", &self.step)
            .field("agent", &self.agent.is_some())
            .field("request", &self.request.is_some())
            .field("option", &self.option.is_some())
            .field("text", &self.text)
            .field("machine", &self.machine.is_some())
            .field("candidates", &self.candidates.as_ref().map(Vec::len))
            .finish()
    }
}

/// The JSON schema a cloud model is given (spec §5.4). Local models use
/// guided generation with the same shape.
pub fn plan_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(ModelPlan)).expect("the schema serializes")
}

/// Classified model failures; provider bodies are never kept (spec §6.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelError {
    Unavailable,
    Timeout,
    Failed,
}

#[async_trait]
pub trait IntentModel: Send + Sync {
    /// The model's raw JSON reply. It is untrusted and never logged.
    async fn plan(&self, prompt: &Prompt) -> Result<VoiceText, ModelError>;
}

/// Returns a fixed reply, including malformed and malicious ones (spec §10).
#[cfg(any(test, feature = "testing"))]
pub struct FakeIntentModel {
    reply: Result<String, ModelError>,
    prompts: std::sync::Mutex<Vec<String>>,
}

#[cfg(any(test, feature = "testing"))]
impl FakeIntentModel {
    pub fn replying(json: &str) -> Self {
        Self { reply: Ok(json.to_owned()), prompts: Default::default() }
    }
    pub fn failing(error: ModelError) -> Self {
        Self { reply: Err(error), prompts: Default::default() }
    }
    /// Every prompt it was given, rendered.
    pub fn prompts(&self) -> Vec<String> {
        self.prompts.lock().unwrap().clone()
    }
}

#[cfg(any(test, feature = "testing"))]
#[async_trait]
impl IntentModel for FakeIntentModel {
    async fn plan(&self, prompt: &Prompt) -> Result<VoiceText, ModelError> {
        self.prompts.lock().unwrap().push(prompt.render().expose().to_owned());
        self.reply.clone().map(VoiceText::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_schema_is_closed_and_agrees_with_the_decoder() {
        let schema = plan_schema();
        assert_eq!(schema["additionalProperties"], false);
        let v = jsonschema::validator_for(&schema).unwrap();
        for good in [
            json!({"step": "answer", "request": "r1", "option": "1"}),
            json!({"step": "send", "agent": "a2", "text": "run the tests"}),
            json!({"step": "read", "candidates": ["a1", "a2"]}),
            json!({"step": "none"}),
        ] {
            assert!(v.is_valid(&good), "{good}");
            assert!(serde_json::from_value::<ModelPlan>(good).is_ok());
        }
        for bad in [
            json!({"step": "answer", "tool": "bash"}),
            json!({"step": "launch"}),
            json!({"request": "r1"}),
            json!({"step": "send", "text": 7}),
        ] {
            assert!(!v.is_valid(&bad), "{bad}");
            assert!(serde_json::from_value::<ModelPlan>(bad).is_err());
        }
    }

    #[test]
    fn debug_never_shows_model_text() {
        let plan: ModelPlan = serde_json::from_value(
            json!({"step": "send", "agent": "SECRET a", "text": "SECRET payload", "machine": "SECRET m"}),
        )
        .unwrap();
        let shown = format!("{plan:?}");
        assert!(!shown.contains("SECRET"), "{shown}");
    }

    #[tokio::test]
    async fn the_fake_model_replies_and_records_prompts() {
        let snap = crate::snapshot::fixtures::snapshot(vec![]);
        let (prompt, _) = crate::router::prompt::build(&snap, &VoiceText::new("hi"), Default::default());
        let m = FakeIntentModel::replying("{\"step\":\"none\"}");
        assert_eq!(m.plan(&prompt).await.unwrap().expose(), "{\"step\":\"none\"}");
        assert_eq!(m.prompts().len(), 1);
        let failing = FakeIntentModel::failing(ModelError::Timeout);
        assert_eq!(failing.plan(&prompt).await, Err(ModelError::Timeout));
    }
}
