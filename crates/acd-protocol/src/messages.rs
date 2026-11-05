use crate::model::{Agent, Machine, Request, ResolveReason, ResolvedBy, Timestamp, ts, ts_opt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
pub struct ClientInfo {
    pub id: String,
    pub name: String,
    pub version: String,
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub epoch: String,
    pub seq: u64,
}

// `Debug` (below) hides the user's text and the token: it shows their
// lengths. A plain comment, so the JSON schema does not change.
#[derive(Serialize, Deserialize, JsonSchema, Clone, PartialEq, Eq)]
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
        revision: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        option_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
    #[serde(rename = "agent.send")]
    AgentSend {
        id: String,
        agent_id: String,
        text: String,
    },
    #[serde(rename = "agent.mark_seen")]
    AgentMarkSeen { id: String, agent_id: String },
    #[serde(rename = "ping")]
    Ping { id: String },
    /// Registers the session's `client.id` for wake pushes. Needs the `push`
    /// capability. `handle` is a relay secret: `Debug` shows only its length.
    #[serde(rename = "push.register")]
    PushRegister {
        id: String,
        relay: String,
        handle: String,
        platform: PushPlatform,
        #[serde(with = "ts")]
        #[schemars(with = "String", extend("format" = "date-time"))]
        expires_at: Timestamp,
    },
    #[serde(rename = "push.unregister")]
    PushUnregister { id: String },
    #[serde(rename = "push.status")]
    PushStatus { id: String },
}

/// The push service behind a registered device.
#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PushPlatform {
    Android,
    Ios,
}

/// What the daemon last learned about a device's wakes. `expired` and
/// `invalid` mean wakes are disabled until the device registers again;
/// `relay_error` means the last wake failed transiently and wakes go on.
#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PushDeviceStatus {
    Ok,
    Expired,
    Invalid,
    RelayError,
    /// A status this version does not know.
    #[serde(other)]
    Unknown,
}

/// The `push` field of a `push.status` result.
#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
pub struct PushInfo {
    pub registered: bool,
    /// Absent when `registered` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<PushDeviceStatus>,
    #[serde(default, with = "ts_opt")]
    #[schemars(with = "Option<String>", extend("format" = "date-time"))]
    pub last_wake_at: Option<Timestamp>,
}

impl std::fmt::Debug for ClientMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientMessage::Hello { protocol, token, client, resume } => f
                .debug_struct("Hello")
                .field("protocol", protocol)
                .field("token_bytes", &token.len())
                .field("client", client)
                .field("resume", resume)
                .finish(),
            ClientMessage::RequestAnswer { id, request_id, revision, option_id, text } => f
                .debug_struct("RequestAnswer")
                .field("id", id)
                .field("request_id", request_id)
                .field("revision", revision)
                .field("option_id", option_id)
                .field("text_bytes", &text.as_ref().map(String::len))
                .finish(),
            ClientMessage::AgentSend { id, agent_id, text } => f
                .debug_struct("AgentSend")
                .field("id", id)
                .field("agent_id", agent_id)
                .field("text_bytes", &text.len())
                .finish(),
            ClientMessage::AgentMarkSeen { id, agent_id } => {
                f.debug_struct("AgentMarkSeen").field("id", id).field("agent_id", agent_id).finish()
            }
            ClientMessage::Ping { id } => f.debug_struct("Ping").field("id", id).finish(),
            ClientMessage::PushRegister { id, relay, handle, platform, expires_at } => f
                .debug_struct("PushRegister")
                .field("id", id)
                .field("relay", relay)
                .field("handle_bytes", &handle.len())
                .field("platform", platform)
                .field("expires_at", expires_at)
                .finish(),
            ClientMessage::PushUnregister { id } => {
                f.debug_struct("PushUnregister").field("id", id).finish()
            }
            ClientMessage::PushStatus { id } => f.debug_struct("PushStatus").field("id", id).finish(),
        }
    }
}

impl ClientMessage {
    pub fn command_id(&self) -> Option<&str> {
        match self {
            ClientMessage::Hello { .. } => None,
            ClientMessage::RequestAnswer { id, .. }
            | ClientMessage::AgentSend { id, .. }
            | ClientMessage::AgentMarkSeen { id, .. }
            | ClientMessage::PushRegister { id, .. }
            | ClientMessage::PushUnregister { id }
            | ClientMessage::PushStatus { id }
            | ClientMessage::Ping { id } => Some(id),
        }
    }
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    RequestGone,
    RequestChanged,
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
            ErrorCode::RequestChanged => "the request changed; review it again",
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
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Delivered,
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
pub struct CommandResult {
    pub id: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
    /// Only on the result of `push.status`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push: Option<PushInfo>,
}

impl CommandResult {
    pub fn ok(id: &str) -> Self {
        Self {
            id: id.into(),
            ok: true,
            outcome: None,
            error: None,
            push: None,
        }
    }
    pub fn push_status(id: &str, info: PushInfo) -> Self {
        Self {
            push: Some(info),
            ..Self::ok(id)
        }
    }
    pub fn delivered(id: &str) -> Self {
        Self {
            id: id.into(),
            ok: true,
            outcome: Some(Outcome::Delivered),
            error: None,
            push: None,
        }
    }
    pub fn err(id: &str, code: ErrorCode, message: &str) -> Self {
        Self {
            id: id.into(),
            ok: false,
            outcome: None,
            error: Some(ErrorBody {
                code,
                message: message.into(),
            }),
            push: None,
        }
    }
    pub fn error_code(&self) -> Option<ErrorCode> {
        self.error.as_ref().map(|e| e.code)
    }
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum ServerMessage {
    #[serde(rename = "welcome")]
    Welcome {
        machine: Machine,
        capabilities: Vec<String>,
        epoch: String,
        seq: u64,
    },
    #[serde(rename = "snapshot")]
    Snapshot {
        epoch: String,
        seq: u64,
        agents: Vec<Agent>,
    },
    #[serde(rename = "agent.updated")]
    AgentUpdated {
        epoch: String,
        seq: u64,
        agent: Agent,
    },
    #[serde(rename = "agent.removed")]
    AgentRemoved {
        epoch: String,
        seq: u64,
        agent_id: String,
    },
    #[serde(rename = "request.opened")]
    RequestOpened {
        epoch: String,
        seq: u64,
        request: Request,
    },
    #[serde(rename = "request.resolved")]
    RequestResolved {
        epoch: String,
        seq: u64,
        request_id: String,
        agent_id: String,
        by: ResolvedBy,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        option_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<ResolveReason>,
        /// What the producer reported, at most 500 bytes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    #[serde(rename = "machine.updated")]
    MachineUpdated {
        epoch: String,
        seq: u64,
        machine: Machine,
    },
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
            ServerMessage::Result(_) | ServerMessage::Pong { .. } | ServerMessage::Error { .. } => {
                None
            }
        }
    }
}

/// The `type` of every message this version of the daemon sends.
pub const SERVER_MESSAGE_TYPES: [&str; 10] = [
    "welcome",
    "snapshot",
    "agent.updated",
    "agent.removed",
    "request.opened",
    "request.resolved",
    "machine.updated",
    "result",
    "pong",
    "error",
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
        && !SERVER_MESSAGE_TYPES.contains(&t)
    {
        return Ok(None);
    }
    serde_json::from_value(value).map(Some)
}
