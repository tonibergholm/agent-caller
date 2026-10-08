//! T3 wire types (orchestration protocol 2, gate 3). Unknown fields are
//! ignored; unknown tags and kinds decode as `Other`.
use serde::Deserialize;
use serde_json::{Value, json};

pub const SUBSCRIBE_SHELL: &str = "orchestration.subscribeShell";
pub const SUBSCRIBE_THREAD: &str = "orchestration.subscribeThread";
pub const DISPATCH: &str = "orchestration.dispatchCommand";
pub const PING_FRAME: &str = r#"{"_tag":"Ping"}"#;

#[derive(Debug, Deserialize)]
#[serde(tag = "_tag")]
pub enum ServerFrame {
    Chunk {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(default)]
        values: Vec<Value>,
    },
    Exit {
        #[serde(rename = "requestId")]
        request_id: String,
        exit: ExitValue,
    },
    Ping {},
    Pong {},
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "_tag")]
pub enum ExitValue {
    Success {
        #[serde(default)]
        value: Value,
    },
    Failure {
        #[serde(default)]
        cause: Vec<Cause>,
    },
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "_tag")]
pub enum Cause {
    Fail {
        #[serde(default)]
        error: Value,
    },
    Die {
        #[serde(default)]
        defect: Value,
    },
    Interrupt {},
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Failure {
    /// Exactly one cause, and it is a typed error. `tag` is `error._tag`,
    /// `message` is `error.message`. Only this shape can ever be read as
    /// "nothing happened" (Task 4).
    Fail { tag: String, message: String },
    /// Every cause is a `Die`; the first defect is kept for the debug log.
    Die(String),
    /// Every cause is an `Interrupt` (the recorded interrupted stream has
    /// four).
    Interrupted,
    /// More than one cause of different kinds, or more than one `Fail`
    /// (Sol round 2, finding 6). The whole set is kept for the debug log.
    ///
    /// Callers should log this with `serde_json::to_string`, not `{:?}`,
    /// so `redact_log` sees plain JSON.
    Mixed(Vec<Cause>),
    /// No causes, or only kinds this version does not know.
    Unknown,
}

/// Classifies the whole cause set. Nothing is dropped: a mixed set never
/// looks like a single typed error.
pub fn failure(causes: &[Cause]) -> Failure {
    let all = |f: fn(&Cause) -> bool| !causes.is_empty() && causes.iter().all(f);
    match causes {
        [Cause::Fail { error }] => Failure::Fail {
            tag: error["_tag"].as_str().unwrap_or("").to_owned(),
            message: error["message"].as_str().unwrap_or("").to_owned(),
        },
        _ if all(|c| matches!(c, Cause::Die { .. })) => {
            let Cause::Die { defect } = &causes[0] else {
                unreachable!()
            };
            Failure::Die(
                defect
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| defect.to_string()),
            )
        }
        _ if all(|c| matches!(c, Cause::Interrupt {})) => Failure::Interrupted,
        _ if causes.iter().all(|c| matches!(c, Cause::Other)) => Failure::Unknown,
        _ => Failure::Mixed(causes.to_vec()),
    }
}

/// A failure as plain JSON for the debug log, which callers pass through
/// `redact_log`. Never log a `Failure` with `{:?}`.
pub(crate) fn failure_json(f: &Failure) -> Value {
    let cause = |c: &Cause| match c {
        Cause::Fail { error } => json!({"fail": error}),
        Cause::Die { defect } => json!({"die": defect}),
        Cause::Interrupt {} => json!("interrupt"),
        Cause::Other => json!("other"),
    };
    match f {
        Failure::Fail { tag, message } => json!({"fail": {"tag": tag, "message": message}}),
        Failure::Die(d) => json!({"die": d}),
        Failure::Interrupted => json!("interrupted"),
        Failure::Mixed(causes) => Value::Array(causes.iter().map(cause).collect()),
        Failure::Unknown => json!("unknown"),
    }
}

/// One text frame holds an object or an array of objects. Elements that are
/// client frames (`Request`, `Ack`, `Interrupt`) decode as `Other`. Elements
/// decode one by one: a malformed element becomes `Other` and never drops
/// its neighbours (a lost `Chunk` would go unacked). A malformed known Chunk
/// envelope is rejected so the reader closes instead of leaving it unacked.
pub fn parse_frames(text: &str) -> Result<Vec<ServerFrame>, serde_json::Error> {
    let decode = |v: Value| -> Result<ServerFrame, serde_json::Error> {
        if v.get("_tag").and_then(Value::as_str) == Some("Chunk") {
            return serde_json::from_value(v);
        }
        Ok(serde_json::from_value(v).unwrap_or(ServerFrame::Other))
    };
    match serde_json::from_str::<Value>(text)? {
        Value::Array(items) => items.into_iter().map(decode).collect(),
        v => Ok(vec![decode(v)?]),
    }
}

pub fn request_frame(id: &str, tag: &str, payload: &Value) -> String {
    json!({"_tag": "Request", "id": id, "tag": tag, "payload": payload, "headers": []}).to_string()
}
pub fn ack_frame(id: &str) -> String {
    json!({"_tag": "Ack", "requestId": id}).to_string()
}
pub fn interrupt_frame(id: &str) -> String {
    json!({"_tag": "Interrupt", "requestId": id}).to_string()
}

// ---- shell ----

#[derive(Debug, Deserialize)]
#[serde(tag = "kind")]
pub enum ShellItem {
    #[serde(rename = "snapshot")]
    Snapshot { snapshot: ShellSnapshot },
    #[serde(rename = "synchronized")]
    Synchronized {},
    #[serde(rename = "thread.updated")]
    ThreadUpdated {
        sequence: u64,
        #[serde(default)]
        location: String,
        thread: Value,
    },
    #[serde(rename = "thread.removed")]
    ThreadRemoved {
        sequence: u64,
        #[serde(rename = "threadId")]
        thread_id: String,
    },
    #[serde(rename = "project.updated")]
    ProjectUpdated { sequence: u64, project: Project },
    #[serde(other)]
    Other,
}

impl ShellItem {
    pub fn decode(v: &Value) -> Result<ShellItem, serde_json::Error> {
        ShellItem::deserialize(v)
    }
}

/// A snapshot carrying `resolvedRepositoryIdentityRoots` holds project
/// metadata only (`threads: []`). It never replaces thread state.
pub fn is_project_only(v: &Value) -> bool {
    v.get("kind").and_then(Value::as_str) == Some("snapshot")
        && v.get("resolvedRepositoryIdentityRoots").is_some()
}

/// `sequence` of any item that has one, known kind or not.
pub fn item_sequence(v: &Value) -> Option<u64> {
    v.get("sequence").and_then(Value::as_u64)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellSnapshot {
    pub snapshot_sequence: u64,
    /// Decoded one by one with `ShellThread::decode`, so one bad entry
    /// never fails the snapshot.
    #[serde(default)]
    pub threads: Vec<Value>,
    /// Decoded one by one with `Project::decode`, like `threads`.
    #[serde(default)]
    pub projects: Vec<Value>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub workspace_root: Option<String>,
}

impl Project {
    pub fn decode(v: &Value) -> Result<Project, serde_json::Error> {
        Project::deserialize(v)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ShellThread {
    pub id: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub provider_instance_id: Option<String>,
    #[serde(default)]
    pub worktree_path: Option<String>,
    #[serde(default)]
    pub active_provider_thread_id: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub pending_runtime_request: Option<PendingRef>,
    #[serde(default)]
    pub latest_visible_message: Option<Value>,
    #[serde(default)]
    pub archived_at: Option<String>,
    #[serde(default)]
    pub deleted_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

impl ShellThread {
    pub fn decode(v: &Value) -> Result<ShellThread, serde_json::Error> {
        ShellThread::deserialize(v)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PendingRef {
    pub id: String,
    pub kind: String,
}

// ---- thread ----

#[derive(Debug, Deserialize)]
#[serde(tag = "kind")]
pub enum ThreadItem {
    #[serde(rename = "snapshot")]
    Snapshot {
        #[serde(rename = "snapshotSequence")]
        snapshot_sequence: u64,
        projection: Value,
    },
    #[serde(rename = "synchronized")]
    Synchronized {},
    #[serde(rename = "event")]
    Event { sequence: u64, event: ThreadEvent },
    #[serde(other)]
    Other,
}

impl ThreadItem {
    pub fn decode(v: &Value) -> Result<ThreadItem, serde_json::Error> {
        ThreadItem::deserialize(v)
    }
}

#[derive(Debug, Deserialize)]
pub struct ThreadEvent {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeRequest {
    pub id: String,
    pub kind: String,
    pub status: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NativeRef {
    #[serde(default)]
    pub native_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderThread {
    pub id: String,
    #[serde(default)]
    pub native_thread_ref: Option<NativeRef>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct T3Option {
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub value: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct T3Question {
    pub id: String,
    #[serde(default)]
    pub header: Option<String>,
    pub question: String,
    #[serde(default)]
    pub options: Vec<T3Option>,
    #[serde(default)]
    pub multi_select: bool,
}

/// The turn items acd reads. Every other `type` is `Other`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum TurnItem {
    #[serde(rename = "approval_request")]
    ApprovalRequest {
        id: String,
        request_id: String,
        #[serde(default)]
        request_kind: Option<String>,
        #[serde(default)]
        prompt: Option<String>,
        #[serde(default)]
        native_item_ref: Option<NativeRef>,
    },
    #[serde(rename = "user_input_request")]
    UserInputRequest {
        id: String,
        request_id: String,
        #[serde(default)]
        questions: Vec<T3Question>,
    },
    #[serde(rename = "command_execution")]
    CommandExecution {
        id: String,
        #[serde(default)]
        status: Option<String>,
        #[serde(default)]
        input: Option<String>,
        #[serde(default)]
        native_item_ref: Option<NativeRef>,
    },
    #[serde(other)]
    Other,
}

pub fn turn_item(v: &Value) -> Result<TurnItem, serde_json::Error> {
    TurnItem::deserialize(v)
}
