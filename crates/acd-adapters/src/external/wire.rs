//! The producer API's request and response bodies, and their validation
//! (docs/superpowers/specs/2026-10-13-external-source-design.md §4).
//!
//! Nothing here derives `Debug`, and no error echoes input text: a title,
//! label, answer or ack message must never reach a log (§8).
use std::collections::HashSet;

use acd_protocol::text::has_forbidden_chars;
use serde::{Deserialize, Serialize};

pub const LEASE_MIN_MS: u64 = 30_000;
pub const LEASE_MAX_MS: u64 = 900_000;
pub const ANSWER_TIMEOUT_MIN_MS: u64 = 30_000;
pub const ANSWER_TIMEOUT_MAX_MS: u64 = 900_000;
pub const ANSWER_TIMEOUT_DEFAULT_MS: u64 = 180_000;
pub const MAX_ITEMS: usize = 32;
pub const MAX_OPTIONS: usize = 8;
pub const MAX_ACKS: usize = 32;
pub const MAX_BODY: usize = 256 * 1024;
pub const MAX_WAIT_MS: u64 = 25_000;
const MAX_TITLE: usize = 200;
const MAX_DETAIL: usize = 4000;
const MAX_LABEL: usize = 80;
const MAX_OPTION_DESCRIPTION: usize = 200;
const MAX_QUESTION: usize = 1000;
const MAX_HEADER: usize = 200;
const MAX_PROJECT: usize = 200;
const MAX_CWD: usize = 1024;
pub const MAX_MESSAGE: usize = 500;

/// A validation failure. The text names a field, never its content.
pub struct Invalid(pub String);

fn invalid<T>(why: impl Into<String>) -> Result<T, Invalid> {
    Err(Invalid(why.into()))
}

/// `^[a-z0-9][a-z0-9-]{0,31}$`
pub fn valid_producer_id(id: &str) -> bool {
    let mut chars = id.chars();
    id.len() <= 32
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// `^[A-Za-z0-9._:-]{1,128}$`, for item keys, agent keys and option ids.
pub fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 128
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
}

#[derive(Deserialize)]
pub struct PutBody {
    pub lease_ms: u64,
    #[serde(default)]
    pub answer_timeout_ms: Option<u64>,
    #[serde(default)]
    pub items: Vec<Item>,
    #[serde(default)]
    pub acks: Vec<Ack>,
}

#[derive(Deserialize, Default)]
pub struct AgentInfo {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
}

#[derive(Deserialize)]
pub struct QuestionIn {
    #[serde(default)]
    pub header: Option<String>,
    pub text: String,
}

#[derive(Deserialize)]
pub struct OptionIn {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Deserialize)]
pub struct Item {
    pub key: String,
    pub agent_key: String,
    #[serde(default)]
    pub agent: AgentInfo,
    pub title: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub question: Option<QuestionIn>,
    #[serde(default)]
    pub options: Vec<OptionIn>,
    #[serde(default)]
    pub free_text: bool,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Applied,
    Rejected,
    Failed,
}

#[derive(Deserialize)]
pub struct Ack {
    pub answer_id: String,
    pub outcome: Outcome,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Serialize, Clone)]
pub struct AnswerOut {
    pub answer_id: String,
    pub key: String,
    pub revision: u64,
    pub option_id: Option<String>,
    pub text: Option<String>,
    pub expires_at_ms: i64,
}

#[derive(Serialize)]
pub struct Refused {
    pub key: String,
    pub reason: &'static str,
}

#[derive(Serialize)]
pub struct AckRefused {
    pub answer_id: String,
    pub reason: &'static str,
}

#[derive(Serialize)]
pub struct PutResponse {
    pub answers: Vec<AnswerOut>,
    pub refused: Vec<Refused>,
    pub ack_refused: Vec<AckRefused>,
}

#[derive(Serialize)]
pub struct AnswersResponse {
    pub answers: Vec<AnswerOut>,
}

fn text(field: &str, value: &str, max: usize) -> Result<(), Invalid> {
    if value.len() > max {
        return invalid(format!("{field}: longer than {max} bytes"));
    }
    if has_forbidden_chars(value) {
        return invalid(format!(
            "{field}: holds a control, bidi or separator character"
        ));
    }
    Ok(())
}

fn opt_text(field: &str, value: &Option<String>, max: usize) -> Result<(), Invalid> {
    value.as_deref().map_or(Ok(()), |v| text(field, v, max))
}

impl PutBody {
    pub fn parse(raw: &[u8]) -> Result<PutBody, Invalid> {
        if raw.len() > MAX_BODY {
            return invalid("body: longer than 256 KiB");
        }
        let body: PutBody = match serde_json::from_slice(raw) {
            Ok(body) => body,
            Err(_) => return invalid("body: not a valid state document"),
        };
        body.validate()?;
        Ok(body)
    }

    pub fn answer_timeout_ms(&self) -> u64 {
        self.answer_timeout_ms.unwrap_or(ANSWER_TIMEOUT_DEFAULT_MS)
    }

    fn validate(&self) -> Result<(), Invalid> {
        if !(LEASE_MIN_MS..=LEASE_MAX_MS).contains(&self.lease_ms) {
            return invalid("lease_ms: outside 30000..=900000");
        }
        if !(ANSWER_TIMEOUT_MIN_MS..=ANSWER_TIMEOUT_MAX_MS).contains(&self.answer_timeout_ms()) {
            return invalid("answer_timeout_ms: outside 30000..=900000");
        }
        if self.items.len() > MAX_ITEMS {
            return invalid("items: more than 32");
        }
        if self.acks.len() > MAX_ACKS {
            return invalid("acks: more than 32");
        }
        let mut keys = HashSet::new();
        let mut agent_keys = HashSet::new();
        for (i, item) in self.items.iter().enumerate() {
            item.validate(i)?;
            if !keys.insert(item.key.as_str()) {
                return invalid(format!("items[{i}].key: duplicate"));
            }
            if !agent_keys.insert(item.agent_key.as_str()) {
                return invalid(format!("items[{i}].agent_key: duplicate"));
            }
        }
        for (i, ack) in self.acks.iter().enumerate() {
            text(&format!("acks[{i}].answer_id"), &ack.answer_id, 128)?;
            opt_text(&format!("acks[{i}].message"), &ack.message, MAX_MESSAGE)?;
        }
        Ok(())
    }
}

impl Item {
    fn validate(&self, i: usize) -> Result<(), Invalid> {
        if !valid_key(&self.key) {
            return invalid(format!("items[{i}].key: not 1 to 128 of A-Za-z0-9._:-"));
        }
        if !valid_key(&self.agent_key) {
            return invalid(format!(
                "items[{i}].agent_key: not 1 to 128 of A-Za-z0-9._:-"
            ));
        }
        text(&format!("items[{i}].title"), &self.title, MAX_TITLE)?;
        text(&format!("items[{i}].detail"), &self.detail, MAX_DETAIL)?;
        opt_text(
            &format!("items[{i}].agent.title"),
            &self.agent.title,
            MAX_TITLE,
        )?;
        opt_text(
            &format!("items[{i}].agent.project"),
            &self.agent.project,
            MAX_PROJECT,
        )?;
        opt_text(&format!("items[{i}].agent.cwd"), &self.agent.cwd, MAX_CWD)?;
        if let Some(q) = &self.question {
            text(&format!("items[{i}].question.text"), &q.text, MAX_QUESTION)?;
            opt_text(
                &format!("items[{i}].question.header"),
                &q.header,
                MAX_HEADER,
            )?;
        }
        if self.options.len() > MAX_OPTIONS {
            return invalid(format!("items[{i}].options: more than 8"));
        }
        let mut ids = HashSet::new();
        for (j, o) in self.options.iter().enumerate() {
            if !valid_key(&o.id) {
                return invalid(format!(
                    "items[{i}].options[{j}].id: not 1 to 128 of A-Za-z0-9._:-"
                ));
            }
            if !ids.insert(o.id.as_str()) {
                return invalid(format!("items[{i}].options[{j}].id: duplicate"));
            }
            text(
                &format!("items[{i}].options[{j}].label"),
                &o.label,
                MAX_LABEL,
            )?;
            opt_text(
                &format!("items[{i}].options[{j}].description"),
                &o.description,
                MAX_OPTION_DESCRIPTION,
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn producer_ids_follow_the_grammar() {
        for ok in ["a", "0", "native-autopilot", &"a".repeat(32)] {
            assert!(valid_producer_id(ok), "{ok}");
        }
        for bad in ["", "-a", "A", "a_b", "a:b", "a.b", &"a".repeat(33)] {
            assert!(!valid_producer_id(bad), "{bad}");
        }
    }

    #[test]
    fn keys_follow_the_grammar() {
        for ok in ["occ-41:3f2a9c1b0d4e", "a", "A.b_c:d-e", &"k".repeat(128)] {
            assert!(valid_key(ok), "{ok}");
        }
        for bad in ["", "a b", "a/b", "é", &"k".repeat(129)] {
            assert!(!valid_key(bad), "{bad}");
        }
    }
}
