use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub type Timestamp = DateTime<Utc>;

/// RFC 3339 UTC with millisecond precision, as required by acd/1.
pub(crate) mod ts {
    use chrono::{DateTime, SecondsFormat, Utc};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(t: &DateTime<Utc>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&t.to_rfc3339_opts(SecondsFormat::Millis, true))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<DateTime<Utc>, D::Error> {
        let s = String::deserialize(d)?;
        DateTime::parse_from_rfc3339(&s)
            .map(|t| t.with_timezone(&Utc))
            .map_err(serde::de::Error::custom)
    }
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Herdr,
    T3,
    Hooks,
    #[serde(other)]
    Unknown,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Claude,
    Codex,
    Other,
    #[serde(other)]
    Unknown,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    Working,
    NeedsYou,
    Done,
    Idle,
    #[serde(other)]
    Unknown,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RequestKind {
    Permission,
    Question,
    BlockedUnparsed,
    #[serde(other)]
    Unknown,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RequestStatus {
    Open,
    Answering,
    Resolved,
    #[serde(other)]
    Unknown,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AnswerPath {
    Hook,
    Keys,
    T3,
    None,
    #[serde(other)]
    Unknown,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    Allow,
    AllowAlways,
    Deny,
    Choose,
    #[serde(other)]
    Unknown,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ResolvedBy {
    Inbox,
    Terminal,
    Agent,
    Superseded,
    Uncertain,
    Lost,
    #[serde(other)]
    Unknown,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SourceState {
    Ok,
    Degraded,
    Disabled,
    Unconfigured,
    #[serde(other)]
    Unknown,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
pub struct SourceStatus {
    pub state: SourceState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl SourceStatus {
    pub fn unconfigured() -> Self {
        Self {
            state: SourceState::Unconfigured,
            message: None,
        }
    }
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
pub struct Sources {
    pub hooks: SourceStatus,
    pub herdr: SourceStatus,
    pub t3: SourceStatus,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
pub struct Machine {
    pub id: String,
    pub name: String,
    pub acd_version: String,
    pub sources: Sources,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
pub struct RequestOption {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub effect: Effect,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
pub struct Question {
    pub id: String,
    pub header: Option<String>,
    pub text: String,
    pub multi_select: bool,
    pub options: Vec<RequestOption>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub id: String,
    pub agent_id: String,
    pub kind: RequestKind,
    pub status: RequestStatus,
    pub answer_path: AnswerPath,
    pub title: String,
    pub detail: String,
    pub options: Vec<RequestOption>,
    pub questions: Option<Vec<Question>>,
    pub free_text: bool,
    pub raw_screen: Option<String>,
    #[serde(with = "ts")]
    #[schemars(with = "String", extend("format" = "date-time"))]
    #[cfg_attr(feature = "ts", ts(type = "string"))]
    pub opened_at: Timestamp,
}

impl Request {
    /// Spec §5: answerable iff a real answer path exists, the kind is not
    /// blocked_unparsed/unknown, and questions are exactly one single-select.
    pub fn is_answerable(&self) -> bool {
        if matches!(self.answer_path, AnswerPath::None | AnswerPath::Unknown) {
            return false;
        }
        match self.kind {
            RequestKind::Permission => true,
            RequestKind::Question => match &self.questions {
                Some(qs) => qs.len() == 1 && !qs[0].multi_select,
                None => false,
            },
            RequestKind::BlockedUnparsed | RequestKind::Unknown => false,
        }
    }
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
pub struct Agent {
    pub id: String,
    pub machine_id: String,
    pub source: Source,
    pub kind: AgentKind,
    pub title: String,
    pub cwd: Option<String>,
    pub project: Option<String>,
    pub state: AgentState,
    #[serde(with = "ts")]
    #[schemars(with = "String", extend("format" = "date-time"))]
    #[cfg_attr(feature = "ts", ts(type = "string"))]
    pub state_since: Timestamp,
    pub failed: bool,
    pub last_message: Option<String>,
    pub pending: Option<Request>,
    pub can_send: bool,
}
