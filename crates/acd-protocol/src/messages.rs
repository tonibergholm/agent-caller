use crate::model::{Agent, Machine, Request, ResolvedBy};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
pub struct ClientInfo { pub id: String, pub name: String, pub version: String }

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
pub struct Cursor { pub epoch: String, pub seq: u64 }

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum ClientMessage {
    #[serde(rename = "hello")]
    Hello {
        protocol: String,
        token: String,
        client: ClientInfo,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resume: Option<Cursor>,
    },
    #[serde(rename = "request.answer")]
    RequestAnswer {
        id: String,
        request_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        option_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
    #[serde(rename = "agent.send")]
    AgentSend { id: String, agent_id: String, text: String },
    #[serde(rename = "agent.mark_seen")]
    AgentMarkSeen { id: String, agent_id: String },
    #[serde(rename = "ping")]
    Ping { id: String },
}

impl ClientMessage {
    pub fn command_id(&self) -> Option<&str> {
        match self {
            ClientMessage::Hello { .. } => None,
            ClientMessage::RequestAnswer { id, .. }
            | ClientMessage::AgentSend { id, .. }
            | ClientMessage::AgentMarkSeen { id, .. }
            | ClientMessage::Ping { id } => Some(id),
        }
    }
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    RequestGone,
    RequestBusy,
    RequestNotReady,
    NotAnswerable,
    AgentGone,
    AgentBusy,
    NotSupported,
    ScreenMismatch,
    BadRequest,
    DuplicateMismatch,
    SourceUnavailable,
    Unauthorized,
    UnsupportedProtocol,
    /// A code this version does not know (spec §7.2: tolerate unknown input).
    #[serde(other)]
    Unknown,
}

impl ErrorCode {
    pub fn describe(&self) -> &'static str {
        match self {
            ErrorCode::RequestGone => "the request was already resolved",
            ErrorCode::RequestBusy => "another answer is in flight",
            ErrorCode::RequestNotReady => "the dialog is not yet confirmed on screen",
            ErrorCode::NotAnswerable => "this request can only be answered in the terminal",
            ErrorCode::AgentGone => "the agent no longer exists",
            ErrorCode::AgentBusy => "the agent cannot take a follow-up right now",
            ErrorCode::NotSupported => "not supported for this agent",
            ErrorCode::ScreenMismatch => "the terminal does not show the expected dialog",
            ErrorCode::BadRequest => "malformed or invalid message",
            ErrorCode::DuplicateMismatch => "command id reused with a different payload",
            ErrorCode::SourceUnavailable => "the source for this agent is unavailable",
            ErrorCode::Unauthorized => "invalid token",
            ErrorCode::UnsupportedProtocol => "unsupported protocol",
            ErrorCode::Unknown => "unknown error",
        }
    }
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
pub struct ErrorBody { pub code: ErrorCode, pub message: String }

#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome { Delivered }

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
pub struct CommandResult {
    pub id: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

impl CommandResult {
    pub fn ok(id: &str) -> Self { Self { id: id.into(), ok: true, outcome: None, error: None } }
    pub fn delivered(id: &str) -> Self {
        Self { id: id.into(), ok: true, outcome: Some(Outcome::Delivered), error: None }
    }
    pub fn err(id: &str, code: ErrorCode, message: &str) -> Self {
        Self { id: id.into(), ok: false, outcome: None, error: Some(ErrorBody { code, message: message.into() }) }
    }
    pub fn error_code(&self) -> Option<ErrorCode> { self.error.as_ref().map(|e| e.code) }
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum ServerMessage {
    #[serde(rename = "welcome")]
    Welcome { machine: Machine, capabilities: Vec<String>, epoch: String, seq: u64 },
    #[serde(rename = "snapshot")]
    Snapshot { epoch: String, seq: u64, agents: Vec<Agent> },
    #[serde(rename = "agent.updated")]
    AgentUpdated { epoch: String, seq: u64, agent: Agent },
    #[serde(rename = "agent.removed")]
    AgentRemoved { epoch: String, seq: u64, agent_id: String },
    #[serde(rename = "request.opened")]
    RequestOpened { epoch: String, seq: u64, request: Request },
    #[serde(rename = "request.resolved")]
    RequestResolved {
        epoch: String,
        seq: u64,
        request_id: String,
        agent_id: String,
        by: ResolvedBy,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        option_id: Option<String>,
    },
    #[serde(rename = "machine.updated")]
    MachineUpdated { epoch: String, seq: u64, machine: Machine },
    #[serde(rename = "result")]
    Result(CommandResult),
    #[serde(rename = "pong")]
    Pong { id: String },
    #[serde(rename = "error")]
    Error {
        code: ErrorCode,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        supported: Option<Vec<String>>,
    },
}

impl ServerMessage {
    /// The event sequence number, for events that carry one.
    pub fn seq(&self) -> Option<u64> {
        match self {
            ServerMessage::Welcome { seq, .. }
            | ServerMessage::Snapshot { seq, .. }
            | ServerMessage::AgentUpdated { seq, .. }
            | ServerMessage::AgentRemoved { seq, .. }
            | ServerMessage::RequestOpened { seq, .. }
            | ServerMessage::RequestResolved { seq, .. }
            | ServerMessage::MachineUpdated { seq, .. } => Some(*seq),
            ServerMessage::Result(_) | ServerMessage::Pong { .. } | ServerMessage::Error { .. } => None,
        }
    }
}

/// The `type` of every message this version of the daemon sends.
pub const SERVER_MESSAGE_TYPES: [&str; 10] = [
    "welcome", "snapshot", "agent.updated", "agent.removed", "request.opened",
    "request.resolved", "machine.updated", "result", "pong", "error",
];

/// Decodes a daemon frame, tolerating newer daemons (spec §7.2).
///
/// Returns `Ok(None)` for a JSON object whose `type` this version does not
/// know; the client skips it. Returns an error for malformed JSON, a frame
/// that is not an object with a string `type`, or a known type whose body
/// does not match.
pub fn decode_server(raw: &str) -> Result<Option<ServerMessage>, serde_json::Error> {
    let value: serde_json::Value = serde_json::from_str(raw)?;
    if let Some(t) = value.get("type").and_then(|t| t.as_str())
        && !SERVER_MESSAGE_TYPES.contains(&t) {
            return Ok(None);
        }
    serde_json::from_value(value).map(Some)
}
