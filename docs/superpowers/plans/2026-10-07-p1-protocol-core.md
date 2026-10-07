# P1: Protocol and daemon core implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the `acd/1` protocol types, the daemon core (agent registry,
request lifecycle, event log, command idempotency, WebSocket server), and
the `acd` binary (`init`, `run`, `status`, `token rotate`). Adapters plug
into this core in later plans.

**Architecture:** A Cargo workspace with three crates:

- `acd-protocol`: wire types, JSON Schema, golden examples, PROTOCOL.md.
- `acd-core`: a pure `Registry` state machine fed by `Observation`s; an
  `EventLog` that assigns epoch and seq; an `IdemCache` for command
  idempotency; `Core`, which ties these together and routes answers to
  `Adapter`s; and an axum WebSocket server.
- `acd`: the CLI and daemon wiring (config, tokens, bind allowlist).

**Tech Stack:** Rust 1.99 (edition 2024), tokio, axum 0.8 (ws), serde,
schemars 1, chrono, sha2, subtle, getrandom, base64, clap 4, toml,
tokio-tungstenite 0.30 (client and tests), tempfile.

**Spec:** `docs/superpowers/specs/2026-10-07-agent-inbox-design.md`, in
particular §5 (data model and lifecycle), §7 (protocol) and §9
(configuration). Roadmap: `docs/superpowers/plans/2026-10-07-roadmap.md`.

## Global Constraints

These come from the spec and apply to every task.

- **Protocol identity.** Protocol string `"acd/1"`. v1 capabilities:
  `["answer", "send", "mark_seen"]`.
- **Default ports.** WebSocket on 47800. 47801 is reserved for the hook
  endpoint (P3).
- **Frames.** Every frame is a JSON object with a `type` discriminator.
  Timestamps are RFC 3339 UTC with milliseconds (`2026-10-07T12:00:00.000Z`).
  Integers stay within ±2^53.
- **Unknown input.** Unknown fields are ignored. An unknown agent state
  deserializes as `unknown`. An unknown request `kind`, `answer_path` or
  option `effect` makes the request or option unanswerable.
- **Close codes.** 4400 unsupported protocol, 4401 unauthorized or no
  hello, 4408 client too slow.
- **Server limits.**
  - The `hello` frame must arrive within 5 s.
  - Frames are limited to 1 MiB.
  - At most 8 connections may be waiting to authenticate at once.
  - A request carrying an `Origin` header gets HTTP 403 unless the origin
    is in `allowed_origins`, which is empty by default.
- **Event log.** The ring buffer holds the last 1000 events. A client that
  falls more than 1000 events behind is closed with 4408.
- **Idempotency.** Command results are cached for 15 minutes, keyed by
  `(client.id, command id)`. `request_not_ready`, `request_busy` and
  `source_unavailable` are never cached.
- **Follow-up text.** Reject any control character except `\n` and `\t`.
- **Files.**
  - The config directory is `$ACD_CONFIG_DIR`, or `~/.config/acd`, with
    mode 0700.
  - `client.token`, `hook.token` and `config.toml` have mode 0600.
  - acd refuses to start if any of these is looser.
  - Tokens are 32 random bytes, base64url without padding, compared in
    constant time.
- **Bind allowlist.** acd may bind only to loopback, or to addresses that
  `tailscale ip` reports **and** that fall in `100.64.0.0/10` or
  `fd7a:115c:a1e0::/48`. Any other address is refused at startup.
- **Logs.** Never log tokens.
- **License.** `MIT OR Apache-2.0` (pending owner confirmation; see the
  handoff note).

## Review Focus

These are inputs the spec implies that no feature test covers by default.
Each has a test in the task named.

1. **Bad frames after authentication.** Binary frames, invalid JSON, and
   unknown `type`s from an authenticated client get a `bad_request` error,
   and the connection stays open (Task 8).
2. **Out-of-order or repeated `hello`.** A command sent before `hello` is
   closed with 4401. A second `hello` gets `bad_request` (Task 8).
3. **Agent disappears mid-answer.** If an agent disappears while its
   request is being delivered, the request resolves as `lost`, the late
   adapter result is ignored, and nothing panics (Task 7).
4. **Observations for unknown agents.** These are ignored and emit no
   events (Task 3).
5. **Loosened file modes.** If a user has loosened a token file's mode
   (for example 0644), `acd run` refuses with a message that names the
   file and the `chmod` fix (Task 9).

---

## File map

```
Cargo.toml                                   workspace
crates/acd-protocol/Cargo.toml
crates/acd-protocol/src/lib.rs               re-exports, PROTOCOL and CAPABILITIES constants
crates/acd-protocol/src/model.rs             Machine, Agent, Request, enums, timestamp serde
crates/acd-protocol/src/messages.rs          ClientMessage, ServerMessage, CommandResult, ErrorCode
crates/acd-protocol/tests/model.rs
crates/acd-protocol/tests/messages.rs        golden examples and schema drift
crates/acd-protocol/schema/{client,server}.schema.json   generated, committed
crates/acd-protocol/examples/{client,server}/*.json      golden examples
crates/acd-protocol/PROTOCOL.md
crates/acd-core/Cargo.toml
crates/acd-core/src/lib.rs
crates/acd-core/src/registry.rs              Observation, Change, Answer, Registry
crates/acd-core/src/log.rs                   EventLog
crates/acd-core/src/idem.rs                  IdemCache
crates/acd-core/src/adapter.rs               Adapter trait, DeliverError
crates/acd-core/src/core.rs                  Core
crates/acd-core/src/server.rs                Auth, ServerOptions, router
crates/acd-core/src/testing.rs               FakeAdapter and builders (feature "testing")
crates/acd-core/tests/{registry,claims,log,idem,core,server}.rs
crates/acd/Cargo.toml
crates/acd/src/main.rs                       clap CLI
crates/acd/src/{paths,secrets,config,identity,bind,daemon,status}.rs
crates/acd/tests/cli.rs
```

---

### Task 1: Workspace and protocol model types

**Files:**
- Create: `Cargo.toml`, `crates/acd-protocol/Cargo.toml`, `crates/acd-protocol/src/lib.rs`, `crates/acd-protocol/src/model.rs`
- Test: `crates/acd-protocol/tests/model.rs`

**Interfaces:**
- Produces (crate `acd_protocol`, re-exported at the root):
  - `Timestamp = chrono::DateTime<Utc>`
  - `Source { Herdr, T3, Hooks }`
  - `AgentKind { Claude, Codex, Other }`
  - `AgentState { Working, NeedsYou, Done, Idle, Unknown }`
  - `RequestKind { Permission, Question, BlockedUnparsed, Unknown }`
  - `RequestStatus { Open, Answering, Resolved }`
  - `AnswerPath { Hook, Keys, T3, None, Unknown }`
  - `Effect { Allow, AllowAlways, Deny, Choose, Unknown }`
  - `ResolvedBy { Inbox, Terminal, Agent, Superseded, Uncertain, Lost }`
  - `SourceState { Ok, Degraded, Disabled, Unconfigured }`
  - `SourceStatus { state, message: Option<String> }`
  - `Sources { hooks, herdr, t3 }`
  - `Machine`, `Agent`, `Request`, `RequestOption`, `Question`, with
    fields exactly as in spec §5
  - `Request::is_answerable(&self) -> bool`

- [ ] **Step 1: Create the workspace manifest**

`Cargo.toml`:

```toml
[workspace]
resolver = "3"
members = ["crates/*"]

[workspace.package]
version = "0.1.0"
edition = "2024"
license = "MIT OR Apache-2.0"
rust-version = "1.90"

[workspace.dependencies]
acd-protocol = { path = "crates/acd-protocol" }
acd-core = { path = "crates/acd-core" }
anyhow = "1"
async-trait = "0.1"
axum = { version = "0.8", features = ["ws"] }
base64 = "0.23"
chrono = { version = "0.4", default-features = false, features = ["clock", "std"] }
clap = { version = "4", features = ["derive"] }
futures-util = "0.3"
gethostname = "1"
getrandom = "0.4"
schemars = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
sha2 = "0.11"
subtle = "2"
tempfile = "3"
thiserror = "2"
tokio = { version = "1", features = ["full"] }
tokio-tungstenite = "0.30"
toml = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
uuid = { version = "1", features = ["v4"] }
```

`crates/acd-protocol/Cargo.toml`:

```toml
[package]
name = "acd-protocol"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
chrono.workspace = true
schemars.workspace = true
serde.workspace = true
serde_json.workspace = true
```

- [ ] **Step 2: Write the failing tests**

`crates/acd-protocol/tests/model.rs`:

```rust
use acd_protocol::*;
use chrono::{TimeZone, Utc};
use serde_json::json;

fn opened_at() -> Timestamp {
    Utc.with_ymd_and_hms(2026, 10, 7, 12, 0, 0).unwrap()
}

fn opt(id: &str, label: &str, effect: Effect) -> RequestOption {
    RequestOption { id: id.into(), label: label.into(), description: None, effect }
}

fn permission() -> Request {
    Request {
        id: "r1".into(),
        agent_id: "herdr:abc:w1:p1".into(),
        kind: RequestKind::Permission,
        status: RequestStatus::Open,
        answer_path: AnswerPath::Hook,
        title: "Bash".into(),
        detail: "touch x".into(),
        options: vec![opt("allow", "Yes", Effect::Allow), opt("deny", "No", Effect::Deny)],
        questions: None,
        free_text: false,
        raw_screen: None,
        opened_at: opened_at(),
    }
}

fn question(questions: Vec<Question>) -> Request {
    Request { kind: RequestKind::Question, answer_path: AnswerPath::Keys, questions: Some(questions), ..permission() }
}

fn q(multi_select: bool) -> Question {
    Question { id: "q1".into(), header: None, text: "Color?".into(), multi_select, options: vec![opt("red", "Red", Effect::Choose)] }
}

#[test]
fn request_serializes_to_wire_shape() {
    assert_eq!(
        serde_json::to_value(permission()).unwrap(),
        json!({
            "id": "r1", "agent_id": "herdr:abc:w1:p1", "kind": "permission", "status": "open",
            "answer_path": "hook", "title": "Bash", "detail": "touch x",
            "options": [
                {"id": "allow", "label": "Yes", "description": null, "effect": "allow"},
                {"id": "deny", "label": "No", "description": null, "effect": "deny"}
            ],
            "questions": null, "free_text": false, "raw_screen": null,
            "opened_at": "2026-10-07T12:00:00.000Z"
        })
    );
}

#[test]
fn unknown_enum_values_and_fields_are_tolerated() {
    let mut v = serde_json::to_value(permission()).unwrap();
    v["kind"] = json!("telepathy");
    v["answer_path"] = json!("carrier_pigeon");
    v["options"][0]["effect"] = json!("maybe");
    v["future_field"] = json!({"x": 1});
    let r: Request = serde_json::from_value(v).unwrap();
    assert_eq!(r.kind, RequestKind::Unknown);
    assert_eq!(r.answer_path, AnswerPath::Unknown);
    assert_eq!(r.options[0].effect, Effect::Unknown);
    assert!(!r.is_answerable());
}

#[test]
fn unknown_agent_state_maps_to_unknown() {
    let s: AgentState = serde_json::from_value(json!("hibernating")).unwrap();
    assert_eq!(s, AgentState::Unknown);
    assert_eq!(serde_json::to_value(AgentState::NeedsYou).unwrap(), json!("needs_you"));
}

#[test]
fn answerability_rules() {
    assert!(permission().is_answerable());
    assert!(!Request { answer_path: AnswerPath::None, ..permission() }.is_answerable());
    assert!(!Request { kind: RequestKind::BlockedUnparsed, ..permission() }.is_answerable());
    assert!(question(vec![q(false)]).is_answerable());
    assert!(!question(vec![q(true)]).is_answerable());
    assert!(!question(vec![q(false), q(false)]).is_answerable());
    assert!(!Request { questions: None, ..question(vec![]) }.is_answerable());
}

#[test]
fn timestamps_round_trip_with_millisecond_utc() {
    let v = serde_json::to_value(permission()).unwrap();
    assert_eq!(v["opened_at"], json!("2026-10-07T12:00:00.000Z"));
    let back: Request = serde_json::from_value(v).unwrap();
    assert_eq!(back.opened_at, opened_at());
}
```

- [ ] **Step 3: Run the tests and confirm they fail**

Run: `cargo test -p acd-protocol --test model`
Expected: compile error (`unresolved import acd_protocol` items).

- [ ] **Step 4: Implement the model**

`crates/acd-protocol/src/lib.rs`:

```rust
//! Wire types for the acd/1 protocol. See PROTOCOL.md.
mod messages;
mod model;

pub use messages::*;
pub use model::*;

/// The protocol identifier clients send in `hello`.
pub const PROTOCOL: &str = "acd/1";
/// Capabilities advertised by a v1 daemon.
pub const CAPABILITIES: [&str; 3] = ["answer", "send", "mark_seen"];
```

Create `crates/acd-protocol/src/messages.rs` empty for now (Task 2 fills
it in).

`crates/acd-protocol/src/model.rs`:

```rust
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

#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Source { Herdr, T3, Hooks }

#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind { Claude, Codex, Other }

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

#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RequestKind {
    Permission,
    Question,
    BlockedUnparsed,
    #[serde(other)]
    Unknown,
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RequestStatus { Open, Answering, Resolved }

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

#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ResolvedBy { Inbox, Terminal, Agent, Superseded, Uncertain, Lost }

#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SourceState { Ok, Degraded, Disabled, Unconfigured }

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
pub struct SourceStatus {
    pub state: SourceState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl SourceStatus {
    pub fn unconfigured() -> Self { Self { state: SourceState::Unconfigured, message: None } }
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
pub struct Sources { pub hooks: SourceStatus, pub herdr: SourceStatus, pub t3: SourceStatus }

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
pub struct Machine {
    pub id: String,
    pub name: String,
    pub acd_version: String,
    pub sources: Sources,
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
pub struct RequestOption {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub effect: Effect,
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
pub struct Question {
    pub id: String,
    pub header: Option<String>,
    pub text: String,
    pub multi_select: bool,
    pub options: Vec<RequestOption>,
}

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
    #[schemars(with = "String")]
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
    #[schemars(with = "String")]
    pub state_since: Timestamp,
    pub failed: bool,
    pub last_message: Option<String>,
    pub pending: Option<Request>,
    pub can_send: bool,
}
```

- [ ] **Step 5: Run the tests and confirm they pass**

Run: `cargo test -p acd-protocol --test model`
Expected: 5 passed.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/acd-protocol
git commit -m "feat(protocol): acd/1 model types with tolerant enums"
```

---

### Task 2: Protocol messages, schema, golden examples, PROTOCOL.md

**Files:**
- Modify: `crates/acd-protocol/src/messages.rs`
- Create: `crates/acd-protocol/examples/client/*.json`, `crates/acd-protocol/examples/server/*.json`, `crates/acd-protocol/schema/*.schema.json` (generated), `crates/acd-protocol/PROTOCOL.md`
- Test: `crates/acd-protocol/tests/messages.rs`

**Interfaces:**
- Consumes: the model types from Task 1.
- Produces:
  - `ClientMessage`, a tagged enum with variants:
    - `Hello { protocol, token, client: ClientInfo, resume: Option<Cursor> }`
    - `RequestAnswer { id, request_id, option_id: Option<String>, text: Option<String> }`
    - `AgentSend { id, agent_id, text }`
    - `AgentMarkSeen { id, agent_id }`
    - `Ping { id }`
  - `ClientMessage::command_id(&self) -> Option<&str>`
  - `ClientInfo { id, name, version }`
  - `Cursor { epoch: String, seq: u64 }`
  - `ServerMessage` variants: `Welcome`, `Snapshot`, `AgentUpdated`,
    `AgentRemoved`, `RequestOpened`, `RequestResolved`, `MachineUpdated`,
    `Result(CommandResult)`, `Pong`, `Error`
  - `ServerMessage::seq(&self) -> Option<u64>`
  - `CommandResult { id, ok, outcome: Option<Outcome>, error: Option<ErrorBody> }`,
    with the constructors `CommandResult::ok(id)`,
    `CommandResult::delivered(id)` and `CommandResult::err(id, code, msg)`
  - `Outcome { Delivered }`
  - `ErrorBody { code, message }`
  - `ErrorCode`: the 13 codes from spec §7.3 plus `UnsupportedProtocol`.
    `ErrorCode::describe(&self) -> &'static str`.

- [ ] **Step 1: Write the golden example files**

`crates/acd-protocol/examples/client/hello.json`:

```json
{"type":"hello","protocol":"acd/1","token":"tok","client":{"id":"6f1c2b9e-0000-4000-8000-000000000001","name":"agent-caller-mac","version":"0.1.0"},"resume":{"epoch":"e1","seq":41}}
```

`crates/acd-protocol/examples/client/request.answer.json`:

```json
{"type":"request.answer","id":"c-1","request_id":"r7","option_id":"allow"}
```

`crates/acd-protocol/examples/client/agent.send.json`:

```json
{"type":"agent.send","id":"c-2","agent_id":"herdr:abc:w1:p1","text":"now run the tests"}
```

`crates/acd-protocol/examples/client/agent.mark_seen.json`:

```json
{"type":"agent.mark_seen","id":"c-3","agent_id":"t3:th_1"}
```

`crates/acd-protocol/examples/server/welcome.json`:

```json
{"type":"welcome","machine":{"id":"mac-1a2b3c4d","name":"Mac","acd_version":"0.1.0","sources":{"hooks":{"state":"unconfigured"},"herdr":{"state":"ok"},"t3":{"state":"degraded","message":"t3 unreachable"}}},"capabilities":["answer","send","mark_seen"],"epoch":"e1","seq":42}
```

`crates/acd-protocol/examples/server/agent.updated.json`:

```json
{"type":"agent.updated","epoch":"e1","seq":43,"agent":{"id":"herdr:abc:w1:p1","machine_id":"mac-1a2b3c4d","source":"herdr","kind":"claude","title":"sp1","cwd":"/src/app","project":"app","state":"needs_you","state_since":"2026-10-07T12:00:00.000Z","failed":false,"last_message":null,"can_send":true,"pending":{"id":"r7","agent_id":"herdr:abc:w1:p1","kind":"permission","status":"open","answer_path":"hook","title":"Bash","detail":"rm -rf build","options":[{"id":"allow","label":"Yes","description":null,"effect":"allow"},{"id":"deny","label":"No","description":null,"effect":"deny"}],"questions":null,"free_text":false,"raw_screen":null,"opened_at":"2026-10-07T12:00:00.000Z"}}}
```

`crates/acd-protocol/examples/server/request.resolved.json`:

```json
{"type":"request.resolved","epoch":"e1","seq":44,"request_id":"r7","agent_id":"herdr:abc:w1:p1","by":"inbox","option_id":"allow"}
```

`crates/acd-protocol/examples/server/result.ok.json`:

```json
{"type":"result","id":"c-1","ok":true,"outcome":"delivered"}
```

`crates/acd-protocol/examples/server/result.err.json`:

```json
{"type":"result","id":"c-1","ok":false,"error":{"code":"request_gone","message":"the request was already resolved"}}
```

`crates/acd-protocol/examples/server/error.unsupported.json`:

```json
{"type":"error","code":"unsupported_protocol","message":"unsupported protocol acd/9","supported":["acd/1"]}
```

- [ ] **Step 2: Write the failing tests**

`crates/acd-protocol/tests/messages.rs`:

```rust
use acd_protocol::*;
use std::{fs, path::Path};

fn dir(rel: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn golden<T: serde::de::DeserializeOwned + serde::Serialize>(rel: &str) {
    let mut n = 0;
    for entry in fs::read_dir(dir(rel)).unwrap() {
        let path = entry.unwrap().path();
        let raw = fs::read_to_string(&path).unwrap();
        let original: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let parsed: T = serde_json::from_value(original.clone())
            .unwrap_or_else(|e| panic!("{} does not parse: {e}", path.display()));
        assert_eq!(serde_json::to_value(&parsed).unwrap(), original, "{} does not round-trip", path.display());
        n += 1;
    }
    assert!(n > 0, "no examples in {rel}");
}

#[test]
fn client_golden_examples_round_trip() { golden::<ClientMessage>("examples/client"); }

#[test]
fn server_golden_examples_round_trip() { golden::<ServerMessage>("examples/server"); }

#[test]
fn command_ids_and_seq_accessors() {
    let m: ClientMessage = serde_json::from_str(r#"{"type":"ping","id":"p1"}"#).unwrap();
    assert_eq!(m.command_id(), Some("p1"));
    let s: ServerMessage = serde_json::from_str(
        r#"{"type":"agent.removed","epoch":"e","seq":9,"agent_id":"a"}"#).unwrap();
    assert_eq!(s.seq(), Some(9));
    assert_eq!(ServerMessage::Pong { id: "x".into() }.seq(), None);
}

#[test]
fn unknown_client_type_is_an_error() {
    assert!(serde_json::from_str::<ClientMessage>(r#"{"type":"agent.launch","id":"1"}"#).is_err());
}

#[test]
fn committed_schemas_match_types() {
    let generated = [
        ("client", serde_json::to_string_pretty(&schemars::schema_for!(ClientMessage)).unwrap()),
        ("server", serde_json::to_string_pretty(&schemars::schema_for!(ServerMessage)).unwrap()),
    ];
    for (name, schema) in generated {
        let path = dir("schema").join(format!("{name}.schema.json"));
        if std::env::var_os("UPDATE_SCHEMA").is_some() {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, format!("{schema}\n")).unwrap();
            continue;
        }
        let committed = fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("missing {}; run with UPDATE_SCHEMA=1", path.display()));
        assert_eq!(committed.trim_end(), schema, "schema drift in {name}; run with UPDATE_SCHEMA=1 and commit");
    }
}
```

- [ ] **Step 3: Run the tests and confirm they fail**

Run: `cargo test -p acd-protocol --test messages`
Expected: compile error (`ClientMessage` not found).

- [ ] **Step 4: Implement the messages**

`crates/acd-protocol/src/messages.rs`:

```rust
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
```

- [ ] **Step 5: Generate the schemas, then run all protocol tests**

Run: `UPDATE_SCHEMA=1 cargo test -p acd-protocol --test messages committed_schemas_match_types && cargo test -p acd-protocol`
Expected: every test passes. `schema/client.schema.json` and
`schema/server.schema.json` now exist.

- [ ] **Step 6: Write PROTOCOL.md**

Create `crates/acd-protocol/PROTOCOL.md` with these parts:

1. A title line: `# acd/1 protocol (normative)`.
2. Spec sections §7.1 to §7.5 from
   `docs/superpowers/specs/2026-10-07-agent-inbox-design.md`, copied
   verbatim. Keep their headings, but renumber them 1 to 5.
3. Spec §5's data-model block and §5.1, copied verbatim, as section 6
   "Data model and request lifecycle".
4. A closing section 7 "Machine-readable definitions". It states that
   `schema/client.schema.json` and `schema/server.schema.json` are
   generated from the Rust types, and that `examples/` holds golden
   messages validated in CI.

- [ ] **Step 7: Commit**

```bash
git add crates/acd-protocol
git commit -m "feat(protocol): acd/1 messages, JSON Schema, golden examples, PROTOCOL.md"
```

---

### Task 3: Registry, agents and the request lifecycle (open, update, supersede, upgrade, end)

**Files:**
- Create: `crates/acd-core/Cargo.toml`, `crates/acd-core/src/lib.rs`, `crates/acd-core/src/registry.rs`
- Test: `crates/acd-core/tests/registry.rs`

**Interfaces:**
- Consumes: `acd_protocol` model types.
- Produces (module `acd_core::registry`, re-exported at the crate root):

```rust
pub struct CorrelationKey(pub String);
pub struct AgentObs {
    pub id: String, pub source: Source, pub kind: AgentKind, pub title: String,
    pub cwd: Option<String>, pub project: Option<String>, pub state: AgentState,
    pub failed: bool, pub last_message: Option<String>, pub can_send: bool,
}
pub struct RequestObs {
    pub agent_id: String, pub key: CorrelationKey, pub upgrade_from: Option<CorrelationKey>,
    pub kind: RequestKind, pub answer_path: AnswerPath, pub title: String, pub detail: String,
    pub options: Vec<RequestOption>, pub questions: Option<Vec<Question>>, pub free_text: bool,
    pub raw_screen: Option<String>, pub ready: bool,
}
pub enum Observation {
    AgentUpsert(AgentObs),
    AgentGone { agent_id: String },
    RequestSeen(RequestObs),
    RequestReady { key: CorrelationKey, ready: bool },
    RequestPath { key: CorrelationKey, answer_path: AnswerPath },
    /// Source evidence that the dialog for `key` ended. Adapters pass
    /// `Terminal` for "the dialog went away", and the registry attributes
    /// it to `Inbox` if a client had claimed the request. `Uncertain` and
    /// `Lost` resolve the request without confirming that the dialog is gone.
    RequestEnded { key: CorrelationKey, by: ResolvedBy },
    /// The dialog for `key` was observed absent. This enables a new
    /// occurrence of the same key.
    DialogGone { key: CorrelationKey },
}
pub enum Change {
    AgentUpdated(Agent), AgentRemoved(String), RequestOpened(Request),
    RequestResolved { request_id: String, agent_id: String, by: ResolvedBy, option_id: Option<String> },
    MachineUpdated(Machine),
}
pub enum Answer { Option(String), Text(String) }
impl Registry {
    pub fn new(machine_id: String) -> Self;
    pub fn apply(&mut self, obs: Observation) -> Vec<Change>;
    pub fn agents(&self) -> Vec<Agent>;
    pub fn agent(&self, id: &str) -> Option<&Agent>;
    pub fn request(&self, id: &str) -> Option<&Request>;   // only unresolved requests
    pub fn resolve_request(&mut self, request_id: &str, by: ResolvedBy) -> Vec<Change>;
}
```

- [ ] **Step 1: Create the crate manifest**

`crates/acd-core/Cargo.toml`:

```toml
[package]
name = "acd-core"
version.workspace = true
edition.workspace = true
license.workspace = true

[features]
testing = []

[dependencies]
acd-protocol.workspace = true
async-trait.workspace = true
axum.workspace = true
chrono.workspace = true
futures-util.workspace = true
serde_json.workspace = true
sha2.workspace = true
subtle.workspace = true
thiserror.workspace = true
tokio.workspace = true
tracing.workspace = true
uuid.workspace = true

[dev-dependencies]
acd-core = { path = ".", features = ["testing"] }
serde_json.workspace = true
tokio-tungstenite.workspace = true
```

`crates/acd-core/src/lib.rs`:

```rust
//! The acd daemon core: registry, request lifecycle, event log and acd/1 server.
pub mod registry;

pub use registry::*;
```

- [ ] **Step 2: Write the failing tests**

`crates/acd-core/tests/registry.rs`:

```rust
use acd_core::*;
use acd_protocol::*;

fn agent(id: &str) -> AgentObs {
    AgentObs {
        id: id.into(), source: Source::Herdr, kind: AgentKind::Claude, title: "sp1".into(),
        cwd: Some("/src/app".into()), project: Some("app".into()), state: AgentState::Working,
        failed: false, last_message: None, can_send: true,
    }
}

fn perm(agent_id: &str, key: &str, detail: &str) -> RequestObs {
    RequestObs {
        agent_id: agent_id.into(), key: CorrelationKey(key.into()), upgrade_from: None,
        kind: RequestKind::Permission, answer_path: AnswerPath::Hook, title: "Bash".into(),
        detail: detail.into(),
        options: vec![
            RequestOption { id: "allow".into(), label: "Yes".into(), description: None, effect: Effect::Allow },
            RequestOption { id: "deny".into(), label: "No".into(), description: None, effect: Effect::Deny },
        ],
        questions: None, free_text: false, raw_screen: None, ready: true,
    }
}

fn opened(changes: &[Change]) -> Vec<Request> {
    changes.iter().filter_map(|c| match c { Change::RequestOpened(r) => Some(r.clone()), _ => None }).collect()
}

fn resolved(changes: &[Change]) -> Vec<(String, ResolvedBy, Option<String>)> {
    changes.iter().filter_map(|c| match c {
        Change::RequestResolved { request_id, by, option_id, .. } => Some((request_id.clone(), *by, option_id.clone())),
        _ => None,
    }).collect()
}

fn reg_with_agent() -> Registry {
    let mut r = Registry::new("m1".into());
    r.apply(Observation::AgentUpsert(agent("a1")));
    r
}

#[test]
fn identical_agent_upsert_emits_nothing() {
    let mut r = Registry::new("m1".into());
    assert_eq!(r.apply(Observation::AgentUpsert(agent("a1"))).len(), 1);
    assert!(r.apply(Observation::AgentUpsert(agent("a1"))).is_empty());
    assert_eq!(r.agent("a1").unwrap().machine_id, "m1");
}

#[test]
fn observations_for_unknown_agents_are_ignored() {
    let mut r = Registry::new("m1".into());
    assert!(r.apply(Observation::RequestSeen(perm("ghost", "k", "ls"))).is_empty());
    assert!(r.apply(Observation::AgentGone { agent_id: "ghost".into() }).is_empty());
    assert!(r.apply(Observation::RequestEnded { key: CorrelationKey("k".into()), by: ResolvedBy::Terminal }).is_empty());
    assert!(r.agents().is_empty());
}

#[test]
fn opening_a_request_sets_pending_and_needs_you() {
    let mut r = reg_with_agent();
    let ch = r.apply(Observation::RequestSeen(perm("a1", "k1", "ls")));
    let req = &opened(&ch)[0];
    let a = r.agent("a1").unwrap();
    assert_eq!(a.state, AgentState::NeedsYou);
    assert_eq!(a.pending.as_ref().unwrap().id, req.id);
    assert_eq!(req.status, RequestStatus::Open);
}

#[test]
fn same_key_updates_in_place() {
    let mut r = reg_with_agent();
    let id = opened(&r.apply(Observation::RequestSeen(perm("a1", "k1", "ls"))))[0].id.clone();
    let ch = r.apply(Observation::RequestSeen(perm("a1", "k1", "ls -la")));
    assert!(opened(&ch).is_empty());
    assert_eq!(r.agent("a1").unwrap().pending.as_ref().unwrap().id, id);
    assert_eq!(r.request(&id).unwrap().detail, "ls -la");
}

#[test]
fn different_key_supersedes() {
    let mut r = reg_with_agent();
    let first = opened(&r.apply(Observation::RequestSeen(perm("a1", "k1", "ls"))))[0].id.clone();
    let ch = r.apply(Observation::RequestSeen(perm("a1", "k2", "rm x")));
    assert_eq!(resolved(&ch), vec![(first, ResolvedBy::Superseded, None)]);
    assert_eq!(opened(&ch).len(), 1);
}

#[test]
fn replay_of_resolved_key_is_ignored_until_dialog_gone_then_new_occurrence() {
    let mut r = reg_with_agent();
    let first = opened(&r.apply(Observation::RequestSeen(perm("a1", "k1", "ls"))))[0].id.clone();
    r.apply(Observation::RequestEnded { key: CorrelationKey("k1".into()), by: ResolvedBy::Uncertain });
    assert!(r.apply(Observation::RequestSeen(perm("a1", "k1", "ls"))).is_empty(), "replay must not reopen");
    r.apply(Observation::DialogGone { key: CorrelationKey("k1".into()) });
    let second = opened(&r.apply(Observation::RequestSeen(perm("a1", "k1", "ls"))));
    assert_eq!(second.len(), 1);
    assert_ne!(second[0].id, first);
}

#[test]
fn terminal_end_confirms_gone_so_same_dialog_can_reopen() {
    let mut r = reg_with_agent();
    r.apply(Observation::RequestSeen(perm("a1", "k1", "ls")));
    let ch = r.apply(Observation::RequestEnded { key: CorrelationKey("k1".into()), by: ResolvedBy::Terminal });
    assert_eq!(resolved(&ch)[0].1, ResolvedBy::Terminal);
    assert_eq!(opened(&r.apply(Observation::RequestSeen(perm("a1", "k1", "ls")))).len(), 1);
}

#[test]
fn upgrade_from_fallback_keeps_request_id() {
    let mut r = reg_with_agent();
    let fallback = RequestObs {
        kind: RequestKind::BlockedUnparsed, answer_path: AnswerPath::None,
        raw_screen: Some("Do you want to proceed?".into()), options: vec![], ..perm("a1", "fp:1", "")
    };
    let id = opened(&r.apply(Observation::RequestSeen(fallback)))[0].id.clone();
    let structured = RequestObs { upgrade_from: Some(CorrelationKey("fp:1".into())), ..perm("a1", "prompt:9", "ls") };
    let ch = r.apply(Observation::RequestSeen(structured));
    assert!(opened(&ch).is_empty());
    assert!(resolved(&ch).is_empty());
    let req = r.request(&id).unwrap();
    assert_eq!(req.kind, RequestKind::Permission);
    assert_eq!(req.answer_path, AnswerPath::Hook);
}

#[test]
fn agent_gone_resolves_pending_as_lost_and_removes_agent() {
    let mut r = reg_with_agent();
    let id = opened(&r.apply(Observation::RequestSeen(perm("a1", "k1", "ls"))))[0].id.clone();
    let ch = r.apply(Observation::AgentGone { agent_id: "a1".into() });
    assert_eq!(resolved(&ch), vec![(id.clone(), ResolvedBy::Lost, None)]);
    assert!(ch.contains(&Change::AgentRemoved("a1".into())));
    assert!(r.request(&id).is_none());
    assert!(r.agent("a1").is_none());
}

#[test]
fn path_and_readiness_updates_apply_to_open_requests() {
    let mut r = reg_with_agent();
    let id = opened(&r.apply(Observation::RequestSeen(RequestObs { ready: false, ..perm("a1", "k1", "ls") })))[0].id.clone();
    r.apply(Observation::RequestPath { key: CorrelationKey("k1".into()), answer_path: AnswerPath::Keys });
    assert_eq!(r.request(&id).unwrap().answer_path, AnswerPath::Keys);
    r.apply(Observation::RequestReady { key: CorrelationKey("k1".into()), ready: true });
    assert!(r.is_ready(&id));
}
```

- [ ] **Step 3: Run the tests and confirm they fail**

Run: `cargo test -p acd-core --test registry`
Expected: compile error (types not defined).

- [ ] **Step 4: Implement the registry**

`crates/acd-core/src/registry.rs`:

```rust
use std::collections::{BTreeMap, HashMap};

use acd_protocol::*;
use chrono::Utc;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CorrelationKey(pub String);

#[derive(Clone, Debug, PartialEq)]
pub struct AgentObs {
    pub id: String,
    pub source: Source,
    pub kind: AgentKind,
    pub title: String,
    pub cwd: Option<String>,
    pub project: Option<String>,
    pub state: AgentState,
    pub failed: bool,
    pub last_message: Option<String>,
    pub can_send: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RequestObs {
    pub agent_id: String,
    pub key: CorrelationKey,
    pub upgrade_from: Option<CorrelationKey>,
    pub kind: RequestKind,
    pub answer_path: AnswerPath,
    pub title: String,
    pub detail: String,
    pub options: Vec<RequestOption>,
    pub questions: Option<Vec<Question>>,
    pub free_text: bool,
    pub raw_screen: Option<String>,
    pub ready: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Observation {
    AgentUpsert(AgentObs),
    AgentGone { agent_id: String },
    RequestSeen(RequestObs),
    RequestReady { key: CorrelationKey, ready: bool },
    RequestPath { key: CorrelationKey, answer_path: AnswerPath },
    RequestEnded { key: CorrelationKey, by: ResolvedBy },
    DialogGone { key: CorrelationKey },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    AgentUpdated(Agent),
    AgentRemoved(String),
    RequestOpened(Request),
    RequestResolved { request_id: String, agent_id: String, by: ResolvedBy, option_id: Option<String> },
    MachineUpdated(Machine),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    Option(String),
    Text(String),
}

struct ReqEntry {
    req: Request,
    key: CorrelationKey,
    ready: bool,
    claimed: Option<Answer>,
}

#[derive(Default)]
struct KeyState {
    agent_id: String,
    generation: u64,
    open_request: Option<String>,
    confirmed_gone: bool,
}

pub struct Registry {
    machine_id: String,
    agents: BTreeMap<String, Agent>,
    requests: HashMap<String, ReqEntry>,
    keys: HashMap<CorrelationKey, KeyState>,
    next_id: u64,
}

impl Registry {
    pub fn new(machine_id: String) -> Self {
        Self { machine_id, agents: BTreeMap::new(), requests: HashMap::new(), keys: HashMap::new(), next_id: 0 }
    }

    pub fn agents(&self) -> Vec<Agent> { self.agents.values().cloned().collect() }
    pub fn agent(&self, id: &str) -> Option<&Agent> { self.agents.get(id) }
    pub fn request(&self, id: &str) -> Option<&Request> { self.requests.get(id).map(|e| &e.req) }
    pub fn is_ready(&self, id: &str) -> bool { self.requests.get(id).is_some_and(|e| e.ready) }

    pub fn apply(&mut self, obs: Observation) -> Vec<Change> {
        match obs {
            Observation::AgentUpsert(o) => self.upsert_agent(o),
            Observation::AgentGone { agent_id } => self.agent_gone(&agent_id),
            Observation::RequestSeen(o) => self.request_seen(o),
            Observation::RequestReady { key, ready } => {
                if let Some(rid) = self.open_request_for(&key) {
                    if let Some(e) = self.requests.get_mut(&rid) { e.ready = ready; }
                }
                vec![]
            }
            Observation::RequestPath { key, answer_path } => {
                let Some(rid) = self.open_request_for(&key) else { return vec![] };
                let Some(e) = self.requests.get_mut(&rid) else { return vec![] };
                if e.req.status != RequestStatus::Open || e.req.answer_path == answer_path { return vec![]; }
                e.req.answer_path = answer_path;
                self.sync_pending(&rid).into_iter().collect()
            }
            Observation::RequestEnded { key, by } => {
                if matches!(by, ResolvedBy::Inbox | ResolvedBy::Terminal | ResolvedBy::Agent) {
                    if let Some(k) = self.keys.get_mut(&key) { k.confirmed_gone = true; }
                }
                match self.open_request_for(&key) {
                    Some(rid) => self.resolve_request(&rid, by),
                    None => vec![],
                }
            }
            Observation::DialogGone { key } => {
                if let Some(k) = self.keys.get_mut(&key) { k.confirmed_gone = true; }
                vec![]
            }
        }
    }

    /// Resolves an unresolved request. A `Terminal` resolution of a claimed
    /// request is attributed to the inbox, with the claimed option.
    pub fn resolve_request(&mut self, request_id: &str, by: ResolvedBy) -> Vec<Change> {
        let Some(e) = self.requests.remove(request_id) else { return vec![] };
        let by = if e.req.status == RequestStatus::Answering && by == ResolvedBy::Terminal { ResolvedBy::Inbox } else { by };
        let option_id = match (by, &e.claimed) {
            (ResolvedBy::Inbox, Some(Answer::Option(o))) => Some(o.clone()),
            _ => None,
        };
        if let Some(k) = self.keys.get_mut(&e.key) {
            if k.open_request.as_deref() == Some(request_id) { k.open_request = None; }
            if by == ResolvedBy::Superseded { k.confirmed_gone = true; }
        }
        let mut out = vec![Change::RequestResolved {
            request_id: request_id.to_string(), agent_id: e.req.agent_id.clone(), by, option_id,
        }];
        if let Some(a) = self.agents.get_mut(&e.req.agent_id) {
            if a.pending.as_ref().map(|p| p.id.as_str()) == Some(request_id) {
                a.pending = None;
                out.push(Change::AgentUpdated(a.clone()));
            }
        }
        out
    }

    fn open_request_for(&self, key: &CorrelationKey) -> Option<String> {
        self.keys.get(key).and_then(|k| k.open_request.clone())
    }

    fn upsert_agent(&mut self, o: AgentObs) -> Vec<Change> {
        let now = Utc::now();
        let (pending, state_since) = match self.agents.get(&o.id) {
            Some(a) => (a.pending.clone(), if a.state == o.state { a.state_since } else { now }),
            None => (None, now),
        };
        let agent = Agent {
            id: o.id.clone(), machine_id: self.machine_id.clone(), source: o.source, kind: o.kind,
            title: o.title, cwd: o.cwd, project: o.project, state: o.state, state_since,
            failed: o.failed, last_message: o.last_message, pending, can_send: o.can_send,
        };
        if self.agents.get(&o.id) == Some(&agent) { return vec![]; }
        self.agents.insert(o.id, agent.clone());
        vec![Change::AgentUpdated(agent)]
    }

    fn agent_gone(&mut self, agent_id: &str) -> Vec<Change> {
        let Some(agent) = self.agents.get(agent_id) else { return vec![] };
        let mut out = Vec::new();
        if let Some(p) = agent.pending.clone() {
            out.extend(self.resolve_request(&p.id, ResolvedBy::Lost).into_iter()
                .filter(|c| !matches!(c, Change::AgentUpdated(_))));
        }
        self.agents.remove(agent_id);
        self.keys.retain(|_, k| k.agent_id != agent_id);
        out.push(Change::AgentRemoved(agent_id.to_string()));
        out
    }

    fn request_seen(&mut self, o: RequestObs) -> Vec<Change> {
        let Some(agent) = self.agents.get(&o.agent_id) else { return vec![] };
        let pending = agent.pending.clone();

        // 1. Same key with an open request: update it in place.
        if let Some(rid) = self.open_request_for(&o.key) {
            return self.update_request(&rid, &o);
        }
        // 2. Replay of a resolved occurrence whose dialog was never confirmed gone.
        if let Some(k) = self.keys.get(&o.key) {
            if k.generation > 0 && !k.confirmed_gone { return vec![]; }
        }
        // 3. Upgrade a fallback request in place.
        if let (Some(p), Some(from)) = (&pending, &o.upgrade_from) {
            if self.open_request_for(from).as_deref() == Some(p.id.as_str()) {
                let rid = p.id.clone();
                if let Some(k) = self.keys.get_mut(from) { k.open_request = None; k.confirmed_gone = true; }
                self.claim_key(&o.key, &o.agent_id, &rid);
                if let Some(e) = self.requests.get_mut(&rid) { e.key = o.key.clone(); }
                return self.update_request(&rid, &o);
            }
        }
        let mut out = Vec::new();
        // 4. A different request on the same agent supersedes the pending one.
        if let Some(p) = pending {
            out.extend(self.resolve_request(&p.id, ResolvedBy::Superseded));
        }
        // 5. Open a new occurrence.
        self.next_id += 1;
        let rid = format!("r{}", self.next_id);
        let req = Request {
            id: rid.clone(), agent_id: o.agent_id.clone(), kind: o.kind, status: RequestStatus::Open,
            answer_path: o.answer_path, title: o.title.clone(), detail: o.detail.clone(),
            options: o.options.clone(), questions: o.questions.clone(), free_text: o.free_text,
            raw_screen: o.raw_screen.clone(), opened_at: Utc::now(),
        };
        self.claim_key(&o.key, &o.agent_id, &rid);
        self.requests.insert(rid.clone(), ReqEntry { req: req.clone(), key: o.key, ready: o.ready, claimed: None });
        let a = self.agents.get_mut(&o.agent_id).expect("agent checked above");
        if a.state != AgentState::NeedsYou {
            a.state = AgentState::NeedsYou;
            a.state_since = Utc::now();
        }
        a.pending = Some(req.clone());
        out.push(Change::RequestOpened(req));
        out.push(Change::AgentUpdated(a.clone()));
        out
    }

    fn claim_key(&mut self, key: &CorrelationKey, agent_id: &str, rid: &str) {
        let k = self.keys.entry(key.clone()).or_default();
        k.agent_id = agent_id.to_string();
        k.generation += 1;
        k.open_request = Some(rid.to_string());
        k.confirmed_gone = false;
    }

    fn update_request(&mut self, rid: &str, o: &RequestObs) -> Vec<Change> {
        let Some(e) = self.requests.get_mut(rid) else { return vec![] };
        let before = e.req.clone();
        e.req.kind = o.kind;
        if e.req.status == RequestStatus::Open { e.req.answer_path = o.answer_path; }
        e.req.title = o.title.clone();
        e.req.detail = o.detail.clone();
        e.req.options = o.options.clone();
        e.req.questions = o.questions.clone();
        e.req.free_text = o.free_text;
        e.req.raw_screen = o.raw_screen.clone();
        e.ready = o.ready;
        if e.req == before { return vec![]; }
        self.sync_pending(rid).into_iter().collect()
    }

    /// Copies the current request into its agent's `pending` field.
    pub(crate) fn sync_pending(&mut self, rid: &str) -> Option<Change> {
        let req = self.requests.get(rid)?.req.clone();
        let a = self.agents.get_mut(&req.agent_id)?;
        a.pending = Some(req);
        Some(Change::AgentUpdated(a.clone()))
    }
}
```

- [ ] **Step 5: Run the tests and confirm they pass**

Run: `cargo test -p acd-core --test registry`
Expected: 10 passed.

- [ ] **Step 6: Commit**

```bash
git add crates/acd-core
git commit -m "feat(core): registry with request occurrences, supersede and upgrade"
```

---

### Task 4: Claims, release, follow-up checks and mark-seen

**Files:**
- Modify: `crates/acd-core/src/registry.rs` (add `impl Registry` methods)
- Test: `crates/acd-core/tests/claims.rs`

**Interfaces:**
- Consumes: `Registry` from Task 3.
- Produces:

```rust
impl Registry {
    pub fn claim(&mut self, request_id: &str, answer: Answer) -> Result<(Request, Vec<Change>), ErrorCode>;
    pub fn release(&mut self, request_id: &str) -> Vec<Change>;
    pub fn check_send(&self, agent_id: &str, text: &str) -> Result<Agent, ErrorCode>;
    pub fn mark_seen(&mut self, agent_id: &str) -> Result<Vec<Change>, ErrorCode>;
}
pub fn has_forbidden_control(text: &str) -> bool;
```

- [ ] **Step 1: Write the failing tests**

`crates/acd-core/tests/claims.rs`:

```rust
use acd_core::*;
use acd_protocol::*;

fn agent(id: &str, can_send: bool) -> AgentObs {
    AgentObs {
        id: id.into(), source: Source::Herdr, kind: AgentKind::Claude, title: "t".into(), cwd: None,
        project: None, state: AgentState::Done, failed: false, last_message: None, can_send,
    }
}

fn perm(key: &str) -> RequestObs {
    RequestObs {
        agent_id: "a1".into(), key: CorrelationKey(key.into()), upgrade_from: None,
        kind: RequestKind::Permission, answer_path: AnswerPath::Hook, title: "Bash".into(), detail: "ls".into(),
        options: vec![RequestOption { id: "allow".into(), label: "Yes".into(), description: None, effect: Effect::Allow }],
        questions: None, free_text: false, raw_screen: None, ready: true,
    }
}

fn setup(obs: RequestObs) -> (Registry, String) {
    let mut r = Registry::new("m".into());
    r.apply(Observation::AgentUpsert(agent("a1", true)));
    let ch = r.apply(Observation::RequestSeen(obs));
    let id = ch.iter().find_map(|c| match c { Change::RequestOpened(q) => Some(q.id.clone()), _ => None }).unwrap();
    (r, id)
}

#[test]
fn claim_moves_to_answering_and_second_claim_is_busy() {
    let (mut r, id) = setup(perm("k"));
    let (req, ch) = r.claim(&id, Answer::Option("allow".into())).unwrap();
    assert_eq!(req.status, RequestStatus::Answering);
    assert!(matches!(&ch[0], Change::AgentUpdated(a) if a.pending.as_ref().unwrap().status == RequestStatus::Answering));
    assert_eq!(r.claim(&id, Answer::Option("allow".into())).unwrap_err(), ErrorCode::RequestBusy);
}

#[test]
fn claim_error_cases() {
    let (mut r, id) = setup(perm("k"));
    assert_eq!(r.claim("nope", Answer::Option("allow".into())).unwrap_err(), ErrorCode::RequestGone);
    assert_eq!(r.claim(&id, Answer::Option("maybe".into())).unwrap_err(), ErrorCode::BadRequest);
    assert_eq!(r.claim(&id, Answer::Text("hi".into())).unwrap_err(), ErrorCode::BadRequest);

    let (mut r2, id2) = setup(RequestObs { answer_path: AnswerPath::None, ..perm("k") });
    assert_eq!(r2.claim(&id2, Answer::Option("allow".into())).unwrap_err(), ErrorCode::NotAnswerable);

    let (mut r3, id3) = setup(RequestObs { ready: false, ..perm("k") });
    assert_eq!(r3.claim(&id3, Answer::Option("allow".into())).unwrap_err(), ErrorCode::RequestNotReady);
}

#[test]
fn free_text_answers_reject_control_characters() {
    let q = Question { id: "q".into(), header: None, text: "Name?".into(), multi_select: false, options: vec![] };
    let obs = RequestObs { kind: RequestKind::Question, questions: Some(vec![q]), free_text: true, options: vec![], answer_path: AnswerPath::T3, ..perm("k") };
    let (mut r, id) = setup(obs);
    assert_eq!(r.claim(&id, Answer::Text("a\u{1b}[2J".into())).unwrap_err(), ErrorCode::BadRequest);
    assert_eq!(r.claim(&id, Answer::Text("   ".into())).unwrap_err(), ErrorCode::BadRequest);
    assert!(r.claim(&id, Answer::Text("Ada\nLovelace".into())).is_ok());
}

#[test]
fn release_returns_to_open() {
    let (mut r, id) = setup(perm("k"));
    r.claim(&id, Answer::Option("allow".into())).unwrap();
    assert_eq!(r.release(&id).len(), 1);
    assert_eq!(r.request(&id).unwrap().status, RequestStatus::Open);
    assert!(r.release(&id).is_empty(), "releasing an open request is a no-op");
}

#[test]
fn terminal_end_after_claim_is_attributed_to_inbox() {
    let (mut r, id) = setup(perm("k"));
    r.claim(&id, Answer::Option("allow".into())).unwrap();
    let ch = r.apply(Observation::RequestEnded { key: CorrelationKey("k".into()), by: ResolvedBy::Terminal });
    assert!(ch.contains(&Change::RequestResolved {
        request_id: id.clone(), agent_id: "a1".into(), by: ResolvedBy::Inbox, option_id: Some("allow".into()),
    }));
}

#[test]
fn check_send_rules() {
    let mut r = Registry::new("m".into());
    r.apply(Observation::AgentUpsert(agent("a1", true)));
    r.apply(Observation::AgentUpsert(agent("hooks-only", false)));
    assert!(r.check_send("a1", "run the tests").is_ok());
    assert_eq!(r.check_send("ghost", "x").unwrap_err(), ErrorCode::AgentGone);
    assert_eq!(r.check_send("hooks-only", "x").unwrap_err(), ErrorCode::NotSupported);
    assert_eq!(r.check_send("a1", "").unwrap_err(), ErrorCode::BadRequest);
    assert_eq!(r.check_send("a1", "rm -rf /\r").unwrap_err(), ErrorCode::BadRequest);
    r.apply(Observation::RequestSeen(perm("k")));
    assert_eq!(r.check_send("a1", "x").unwrap_err(), ErrorCode::AgentBusy);
}

#[test]
fn mark_seen_moves_done_to_idle_only() {
    let mut r = Registry::new("m".into());
    r.apply(Observation::AgentUpsert(agent("a1", true)));
    let ch = r.mark_seen("a1").unwrap();
    assert!(matches!(&ch[0], Change::AgentUpdated(a) if a.state == AgentState::Idle));
    assert!(r.mark_seen("a1").unwrap().is_empty());
    assert_eq!(r.mark_seen("ghost").unwrap_err(), ErrorCode::AgentGone);
}
```

- [ ] **Step 2: Run the tests and confirm they fail**

Run: `cargo test -p acd-core --test claims`
Expected: compile error (`claim` not found).

- [ ] **Step 3: Implement the methods**

Append to `crates/acd-core/src/registry.rs`:

```rust
/// Control characters other than newline and tab are never forwarded to an agent.
pub fn has_forbidden_control(text: &str) -> bool {
    text.chars().any(|c| c.is_control() && c != '\n' && c != '\t')
}

impl Registry {
    pub fn claim(&mut self, request_id: &str, answer: Answer) -> Result<(Request, Vec<Change>), ErrorCode> {
        let e = self.requests.get_mut(request_id).ok_or(ErrorCode::RequestGone)?;
        match e.req.status {
            RequestStatus::Answering => return Err(ErrorCode::RequestBusy),
            RequestStatus::Resolved => return Err(ErrorCode::RequestGone),
            RequestStatus::Open => {}
        }
        if !e.req.is_answerable() { return Err(ErrorCode::NotAnswerable); }
        if !e.ready { return Err(ErrorCode::RequestNotReady); }
        match &answer {
            Answer::Option(id) => {
                let valid = e.req.options.iter().any(|o| &o.id == id && o.effect != Effect::Unknown);
                if !valid { return Err(ErrorCode::BadRequest); }
            }
            Answer::Text(t) => {
                if !e.req.free_text || t.trim().is_empty() || has_forbidden_control(t) {
                    return Err(ErrorCode::BadRequest);
                }
            }
        }
        e.req.status = RequestStatus::Answering;
        e.claimed = Some(answer);
        let req = e.req.clone();
        Ok((req, self.sync_pending(request_id).into_iter().collect()))
    }

    pub fn release(&mut self, request_id: &str) -> Vec<Change> {
        let Some(e) = self.requests.get_mut(request_id) else { return vec![] };
        if e.req.status != RequestStatus::Answering { return vec![]; }
        e.req.status = RequestStatus::Open;
        e.claimed = None;
        self.sync_pending(request_id).into_iter().collect()
    }

    pub fn check_send(&self, agent_id: &str, text: &str) -> Result<Agent, ErrorCode> {
        let a = self.agents.get(agent_id).ok_or(ErrorCode::AgentGone)?;
        if !a.can_send { return Err(ErrorCode::NotSupported); }
        if text.trim().is_empty() || has_forbidden_control(text) { return Err(ErrorCode::BadRequest); }
        if a.pending.is_some() || a.state == AgentState::NeedsYou { return Err(ErrorCode::AgentBusy); }
        Ok(a.clone())
    }

    pub fn mark_seen(&mut self, agent_id: &str) -> Result<Vec<Change>, ErrorCode> {
        let a = self.agents.get_mut(agent_id).ok_or(ErrorCode::AgentGone)?;
        if a.state != AgentState::Done { return Ok(vec![]); }
        a.state = AgentState::Idle;
        a.state_since = Utc::now();
        Ok(vec![Change::AgentUpdated(a.clone())])
    }
}
```

- [ ] **Step 4: Run all core tests and confirm they pass**

Run: `cargo test -p acd-core`
Expected: the `registry` and `claims` suites pass.

- [ ] **Step 5: Commit**

```bash
git add crates/acd-core
git commit -m "feat(core): claims, release, follow-up checks and mark-seen"
```

---

### Task 5: EventLog with epoch, seq and resume

**Files:**
- Create: `crates/acd-core/src/log.rs`
- Modify: `crates/acd-core/src/lib.rs` (add `pub mod log; pub use log::EventLog;`)
- Test: `crates/acd-core/tests/log.rs`

**Interfaces:**
- Consumes: `Change` (Task 3), and `ServerMessage` and `Cursor` (Task 2).
- Produces:

```rust
pub struct EventLog;
impl EventLog {
    pub fn new(epoch: String, capacity: usize) -> Self;
    pub fn epoch(&self) -> &str;
    pub fn seq(&self) -> u64;
    pub fn record(&mut self, change: Change) -> ServerMessage;
    pub fn replay_after(&self, cursor: &Cursor) -> Option<Vec<ServerMessage>>;
}
```

- [ ] **Step 1: Write the failing tests**

`crates/acd-core/tests/log.rs`:

```rust
use acd_core::*;
use acd_protocol::*;

fn removed(id: &str) -> Change { Change::AgentRemoved(id.into()) }

fn cursor(epoch: &str, seq: u64) -> Cursor { Cursor { epoch: epoch.into(), seq } }

#[test]
fn record_assigns_increasing_seq_with_epoch() {
    let mut log = EventLog::new("e1".into(), 1000);
    let m1 = log.record(removed("a"));
    let m2 = log.record(removed("b"));
    assert_eq!((m1.seq(), m2.seq(), log.seq()), (Some(1), Some(2), 2));
    assert!(matches!(m2, ServerMessage::AgentRemoved { ref epoch, .. } if epoch == "e1"));
}

#[test]
fn replay_returns_events_after_cursor() {
    let mut log = EventLog::new("e1".into(), 1000);
    for i in 0..5 { log.record(removed(&i.to_string())); }
    let replay = log.replay_after(&cursor("e1", 3)).unwrap();
    assert_eq!(replay.iter().map(|m| m.seq().unwrap()).collect::<Vec<_>>(), vec![4, 5]);
    assert_eq!(log.replay_after(&cursor("e1", 5)).unwrap().len(), 0);
}

#[test]
fn replay_refuses_other_epoch_future_seq_and_gaps() {
    let mut log = EventLog::new("e1".into(), 3);
    for i in 0..10 { log.record(removed(&i.to_string())); }
    assert!(log.replay_after(&cursor("e0", 9)).is_none(), "other epoch");
    assert!(log.replay_after(&cursor("e1", 11)).is_none(), "future seq");
    assert!(log.replay_after(&cursor("e1", 5)).is_none(), "evicted range");
    assert_eq!(log.replay_after(&cursor("e1", 7)).unwrap().len(), 3);
}

#[test]
fn empty_log_replays_nothing_for_zero_cursor() {
    let log = EventLog::new("e1".into(), 1000);
    assert_eq!(log.replay_after(&cursor("e1", 0)).unwrap().len(), 0);
}
```

- [ ] **Step 2: Run the tests and confirm they fail**

Run: `cargo test -p acd-core --test log`
Expected: compile error (`EventLog` not found).

- [ ] **Step 3: Implement the event log**

`crates/acd-core/src/log.rs`:

```rust
use std::collections::VecDeque;

use acd_protocol::{Cursor, ServerMessage};

use crate::registry::Change;

/// Assigns `(epoch, seq)` to changes and keeps the last `capacity` events
/// for resume (spec §7.4).
pub struct EventLog {
    epoch: String,
    seq: u64,
    ring: VecDeque<ServerMessage>,
    capacity: usize,
}

impl EventLog {
    pub fn new(epoch: String, capacity: usize) -> Self {
        Self { epoch, seq: 0, ring: VecDeque::with_capacity(capacity), capacity }
    }

    pub fn epoch(&self) -> &str { &self.epoch }
    pub fn seq(&self) -> u64 { self.seq }

    pub fn record(&mut self, change: Change) -> ServerMessage {
        self.seq += 1;
        let (epoch, seq) = (self.epoch.clone(), self.seq);
        let msg = match change {
            Change::AgentUpdated(agent) => ServerMessage::AgentUpdated { epoch, seq, agent },
            Change::AgentRemoved(agent_id) => ServerMessage::AgentRemoved { epoch, seq, agent_id },
            Change::RequestOpened(request) => ServerMessage::RequestOpened { epoch, seq, request },
            Change::RequestResolved { request_id, agent_id, by, option_id } => {
                ServerMessage::RequestResolved { epoch, seq, request_id, agent_id, by, option_id }
            }
            Change::MachineUpdated(machine) => ServerMessage::MachineUpdated { epoch, seq, machine },
        };
        if self.ring.len() == self.capacity { self.ring.pop_front(); }
        self.ring.push_back(msg.clone());
        msg
    }

    /// The events after `cursor`, or `None` when the client must take a snapshot.
    pub fn replay_after(&self, cursor: &Cursor) -> Option<Vec<ServerMessage>> {
        if cursor.epoch != self.epoch || cursor.seq > self.seq { return None; }
        let oldest = self.ring.front().and_then(ServerMessage::seq).unwrap_or(self.seq + 1);
        if cursor.seq + 1 < oldest { return None; }
        Some(self.ring.iter().filter(|m| m.seq().is_some_and(|s| s > cursor.seq)).cloned().collect())
    }
}
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p acd-core --test log`
Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/acd-core
git commit -m "feat(core): event log with epoch, seq and bounded resume"
```

---

### Task 6: IdemCache for command reservation and caching

**Files:**
- Create: `crates/acd-core/src/idem.rs`
- Modify: `crates/acd-core/src/lib.rs` (add `pub mod idem; pub use idem::{Begin, IdemCache};`)
- Test: `crates/acd-core/tests/idem.rs`

**Interfaces:**
- Produces:

```rust
pub enum Begin { Run, Wait(tokio::sync::watch::Receiver<Option<CommandResult>>), Done(CommandResult), Mismatch }
pub struct IdemCache;
impl IdemCache {
    pub fn new(ttl: Duration) -> Self;
    pub fn begin(&mut self, client: &str, id: &str, hash: [u8; 32], now: Instant) -> Begin;
    pub fn finish(&mut self, client: &str, id: &str, result: CommandResult, cacheable: bool, now: Instant);
}
```

- [ ] **Step 1: Write the failing tests**

`crates/acd-core/tests/idem.rs`:

```rust
use std::time::{Duration, Instant};

use acd_core::*;
use acd_protocol::*;

const H1: [u8; 32] = [1; 32];
const H2: [u8; 32] = [2; 32];

#[test]
fn first_begin_runs_then_result_is_cached() {
    let mut c = IdemCache::new(Duration::from_secs(900));
    let t = Instant::now();
    assert!(matches!(c.begin("cl", "1", H1, t), Begin::Run));
    c.finish("cl", "1", CommandResult::delivered("1"), true, t);
    assert!(matches!(c.begin("cl", "1", H1, t), Begin::Done(r) if r == CommandResult::delivered("1")));
}

#[test]
fn ids_are_scoped_per_client_and_payload_mismatch_is_reported() {
    let mut c = IdemCache::new(Duration::from_secs(900));
    let t = Instant::now();
    c.begin("a", "1", H1, t);
    assert!(matches!(c.begin("b", "1", H2, t), Begin::Run));
    assert!(matches!(c.begin("a", "1", H2, t), Begin::Mismatch));
}

#[tokio::test]
async fn concurrent_retry_waits_for_the_running_command() {
    let mut c = IdemCache::new(Duration::from_secs(900));
    let t = Instant::now();
    c.begin("cl", "1", H1, t);
    let Begin::Wait(mut rx) = c.begin("cl", "1", H1, t) else { panic!("expected Wait") };
    c.finish("cl", "1", CommandResult::delivered("1"), true, t);
    rx.changed().await.unwrap();
    assert_eq!(rx.borrow().clone(), Some(CommandResult::delivered("1")));
}

#[test]
fn non_cacheable_results_release_the_id() {
    let mut c = IdemCache::new(Duration::from_secs(900));
    let t = Instant::now();
    c.begin("cl", "1", H1, t);
    c.finish("cl", "1", CommandResult::err("1", ErrorCode::RequestNotReady, "x"), false, t);
    assert!(matches!(c.begin("cl", "1", H1, t), Begin::Run));
}

#[test]
fn cached_results_expire_after_ttl() {
    let mut c = IdemCache::new(Duration::from_secs(900));
    let t = Instant::now();
    c.begin("cl", "1", H1, t);
    c.finish("cl", "1", CommandResult::ok("1"), true, t);
    assert!(matches!(c.begin("cl", "1", H1, t + Duration::from_secs(901)), Begin::Run));
}
```

- [ ] **Step 2: Run the tests and confirm they fail**

Run: `cargo test -p acd-core --test idem`
Expected: compile error (`IdemCache` not found).

- [ ] **Step 3: Implement the cache**

`crates/acd-core/src/idem.rs`:

```rust
use std::collections::HashMap;
use std::time::{Duration, Instant};

use acd_protocol::CommandResult;
use tokio::sync::watch;

/// What the caller should do with a command (spec §7.4).
pub enum Begin {
    /// No entry exists: run the command, then call `finish`.
    Run,
    /// The same command is running: wait for its result.
    Wait(watch::Receiver<Option<CommandResult>>),
    /// The command already finished: return the cached result.
    Done(CommandResult),
    /// The id was reused with a different payload.
    Mismatch,
}

enum State {
    Pending(watch::Sender<Option<CommandResult>>),
    Done { result: CommandResult, at: Instant },
}

struct Entry {
    hash: [u8; 32],
    state: State,
}

pub struct IdemCache {
    ttl: Duration,
    entries: HashMap<(String, String), Entry>,
}

impl IdemCache {
    pub fn new(ttl: Duration) -> Self { Self { ttl, entries: HashMap::new() } }

    pub fn begin(&mut self, client: &str, id: &str, hash: [u8; 32], now: Instant) -> Begin {
        let ttl = self.ttl;
        self.entries.retain(|_, e| match &e.state {
            State::Done { at, .. } => now.duration_since(*at) < ttl,
            State::Pending(_) => true,
        });
        let key = (client.to_string(), id.to_string());
        if let Some(e) = self.entries.get(&key) {
            if e.hash != hash { return Begin::Mismatch; }
            return match &e.state {
                State::Pending(tx) => Begin::Wait(tx.subscribe()),
                State::Done { result, .. } => Begin::Done(result.clone()),
            };
        }
        let (tx, _rx) = watch::channel(None);
        self.entries.insert(key, Entry { hash, state: State::Pending(tx) });
        Begin::Run
    }

    pub fn finish(&mut self, client: &str, id: &str, result: CommandResult, cacheable: bool, now: Instant) {
        let key = (client.to_string(), id.to_string());
        let Some(e) = self.entries.remove(&key) else { return };
        let hash = e.hash;
        if let State::Pending(tx) = e.state {
            let _ = tx.send(Some(result.clone()));
        }
        if cacheable {
            self.entries.insert(key, Entry { hash, state: State::Done { result, at: now } });
        }
    }
}
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p acd-core --test idem`
Expected: 5 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/acd-core
git commit -m "feat(core): command reservation and result cache"
```

---

### Task 7: Adapter trait, Core and FakeAdapter

**Files:**
- Create: `crates/acd-core/src/adapter.rs`, `crates/acd-core/src/core.rs`, `crates/acd-core/src/testing.rs`
- Modify: `crates/acd-core/src/lib.rs`
- Test: `crates/acd-core/tests/core.rs`

**Interfaces:**
- Consumes: `Registry`, `EventLog`, `IdemCache`, and the protocol types.
- Produces:

```rust
#[derive(Debug, thiserror::Error)]
pub enum DeliverError { NotDelivered(ErrorCode), Uncertain(String) }
#[async_trait] pub trait Adapter: Send + Sync {
    fn name(&self) -> &'static str;
    fn answer_paths(&self) -> &[AnswerPath];
    fn sources(&self) -> &[Source];
    async fn deliver(&self, request: &Request, answer: &Answer) -> Result<(), DeliverError>;
    async fn send(&self, agent: &Agent, text: &str) -> Result<(), DeliverError>;
    async fn mark_seen(&self, agent: &Agent) -> Result<(), DeliverError> { Ok(()) }
}
pub const RING_CAPACITY: usize = 1000;
pub const IDEM_TTL: Duration = Duration::from_secs(900);
impl Core {
    pub fn new(machine: Machine, adapters: Vec<Arc<dyn Adapter>>) -> Arc<Core>;
    pub fn epoch(&self) -> String;
    pub fn observe(&self, obs: Observation);
    pub fn set_source_status(&self, source: Source, status: SourceStatus);
    pub fn welcome(&self) -> ServerMessage;
    pub fn attach(&self, resume: Option<&Cursor>) -> (Vec<ServerMessage>, tokio::sync::broadcast::Receiver<ServerMessage>);
    pub async fn handle(&self, client_id: &str, msg: ClientMessage) -> CommandResult;
}
// feature "testing":
pub mod testing { pub struct FakeAdapter; pub fn test_machine() -> Machine;
  pub fn agent_obs(id: &str) -> AgentObs; pub fn permission_obs(agent_id: &str, key: &str) -> RequestObs; }
```

- [ ] **Step 1: Write the failing tests**

`crates/acd-core/tests/core.rs`:

```rust
use std::{sync::Arc, time::Duration};

use acd_core::testing::*;
use acd_core::*;
use acd_protocol::*;

fn answer(id: &str, request_id: &str) -> ClientMessage {
    ClientMessage::RequestAnswer { id: id.into(), request_id: request_id.into(), option_id: Some("allow".into()), text: None }
}

fn open_request(core: &Core) -> String {
    core.observe(Observation::AgentUpsert(agent_obs("a1")));
    core.observe(Observation::RequestSeen(permission_obs("a1", "k1")));
    let (initial, _) = core.attach(None);
    let ServerMessage::Snapshot { agents, .. } = &initial[0] else { panic!("expected snapshot") };
    agents[0].pending.as_ref().unwrap().id.clone()
}

#[tokio::test]
async fn answer_is_delivered_then_resolved_by_source_evidence() {
    let fake = FakeAdapter::new();
    let core = Core::new(test_machine(), vec![fake.clone()]);
    let rid = open_request(&core);
    let res = core.handle("cl", answer("c1", &rid)).await;
    assert_eq!(res, CommandResult::delivered("c1"));
    assert_eq!(fake.delivered(), vec![(rid.clone(), Answer::Option("allow".into()))]);
    let (_, mut rx) = core.attach(None);
    core.observe(Observation::RequestEnded { key: CorrelationKey("k1".into()), by: ResolvedBy::Terminal });
    loop {
        if let ServerMessage::RequestResolved { by, option_id, .. } = rx.recv().await.unwrap() {
            assert_eq!((by, option_id.as_deref()), (ResolvedBy::Inbox, Some("allow")));
            break;
        }
    }
}

#[tokio::test]
async fn pre_delivery_error_releases_and_is_not_cached() {
    let fake = FakeAdapter::new();
    let core = Core::new(test_machine(), vec![fake.clone()]);
    let rid = open_request(&core);
    fake.fail_next(DeliverError::NotDelivered(ErrorCode::ScreenMismatch));
    let first = core.handle("cl", answer("c1", &rid)).await;
    assert_eq!(first.error_code(), Some(ErrorCode::ScreenMismatch));
    let retry = core.handle("cl", answer("c2", &rid)).await;
    assert_eq!(retry, CommandResult::delivered("c2"), "request went back to open");

    let fake2 = FakeAdapter::new();
    let core2 = Core::new(test_machine(), vec![fake2.clone()]);
    let rid2 = open_request(&core2);
    fake2.fail_next(DeliverError::NotDelivered(ErrorCode::RequestNotReady));
    assert_eq!(core2.handle("cl", answer("same", &rid2)).await.error_code(), Some(ErrorCode::RequestNotReady));
    assert_eq!(core2.handle("cl", answer("same", &rid2)).await, CommandResult::delivered("same"));
}

#[tokio::test]
async fn concurrent_retry_delivers_once() {
    let fake = FakeAdapter::gated();
    let core = Core::new(test_machine(), vec![fake.clone()]);
    let rid = open_request(&core);
    let (c1, c2) = (core.clone(), core.clone());
    let (r1, r2) = (rid.clone(), rid.clone());
    let a = tokio::spawn(async move { c1.handle("cl", answer("dup", &r1)).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let b = tokio::spawn(async move { c2.handle("cl", answer("dup", &r2)).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    fake.open_gate();
    let (ra, rb) = (a.await.unwrap(), b.await.unwrap());
    assert_eq!(ra, rb);
    assert_eq!(fake.delivered().len(), 1);
}

#[tokio::test]
async fn reused_id_with_different_payload_is_rejected() {
    let fake = FakeAdapter::new();
    let core = Core::new(test_machine(), vec![fake]);
    let rid = open_request(&core);
    core.handle("cl", answer("c1", &rid)).await;
    let other = ClientMessage::AgentMarkSeen { id: "c1".into(), agent_id: "a1".into() };
    assert_eq!(core.handle("cl", other).await.error_code(), Some(ErrorCode::DuplicateMismatch));
}

#[tokio::test]
async fn agent_gone_during_delivery_resolves_lost_without_panic() {
    let fake = FakeAdapter::gated();
    let core = Core::new(test_machine(), vec![fake.clone()]);
    let rid = open_request(&core);
    let c = core.clone();
    let r = rid.clone();
    let task = tokio::spawn(async move { c.handle("cl", answer("c1", &r)).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    core.observe(Observation::AgentGone { agent_id: "a1".into() });
    fake.fail_next(DeliverError::NotDelivered(ErrorCode::SourceUnavailable));
    fake.open_gate();
    let res = task.await.unwrap();
    assert_eq!(res.error_code(), Some(ErrorCode::SourceUnavailable));
    let (initial, _) = core.attach(None);
    let ServerMessage::Snapshot { agents, .. } = &initial[0] else { panic!() };
    assert!(agents.is_empty());
}

#[tokio::test]
async fn uncertain_delivery_resolves_uncertain() {
    let fake = FakeAdapter::new();
    let core = Core::new(test_machine(), vec![fake.clone()]);
    let rid = open_request(&core);
    fake.fail_next(DeliverError::Uncertain("no confirmation".into()));
    let (_, mut rx) = core.attach(None);
    assert_eq!(core.handle("cl", answer("c1", &rid)).await, CommandResult::delivered("c1"));
    loop {
        if let ServerMessage::RequestResolved { by, .. } = rx.recv().await.unwrap() {
            assert_eq!(by, ResolvedBy::Uncertain);
            break;
        }
    }
}

#[tokio::test]
async fn missing_adapter_returns_source_unavailable_and_reopens() {
    let core = Core::new(test_machine(), vec![]);
    let rid = open_request(&core);
    assert_eq!(core.handle("cl", answer("c1", &rid)).await.error_code(), Some(ErrorCode::SourceUnavailable));
    let (initial, _) = core.attach(None);
    let ServerMessage::Snapshot { agents, .. } = &initial[0] else { panic!() };
    assert_eq!(agents[0].pending.as_ref().unwrap().status, RequestStatus::Open);
}

#[tokio::test]
async fn attach_replays_within_epoch_and_snapshots_otherwise() {
    let core = Core::new(test_machine(), vec![]);
    core.observe(Observation::AgentUpsert(agent_obs("a1")));
    let epoch = core.epoch();
    core.observe(Observation::AgentUpsert(AgentObs { title: "renamed".into(), ..agent_obs("a1") }));
    let (replay, _) = core.attach(Some(&Cursor { epoch: epoch.clone(), seq: 1 }));
    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0].seq(), Some(2));
    let (snap, _) = core.attach(Some(&Cursor { epoch: "old".into(), seq: 1 }));
    assert!(matches!(snap[0], ServerMessage::Snapshot { seq: 2, .. }));
}

#[tokio::test]
async fn follow_up_send_goes_to_source_adapter() {
    let fake = FakeAdapter::new();
    let core = Core::new(test_machine(), vec![fake.clone()]);
    core.observe(Observation::AgentUpsert(AgentObs { state: AgentState::Done, ..agent_obs("a1") }));
    let res = core.handle("cl", ClientMessage::AgentSend { id: "s1".into(), agent_id: "a1".into(), text: "run tests".into() }).await;
    assert_eq!(res, CommandResult::delivered("s1"));
    assert_eq!(fake.sent(), vec![("a1".to_string(), "run tests".to_string())]);
}

#[test]
fn welcome_lists_v1_capabilities() {
    let core = Core::new(test_machine(), vec![]);
    let ServerMessage::Welcome { capabilities, .. } = core.welcome() else { panic!() };
    assert_eq!(capabilities, vec!["answer", "send", "mark_seen"]);
    let _: Arc<Core> = core;
}
```

- [ ] **Step 2: Run the tests and confirm they fail**

Run: `cargo test -p acd-core --test core`
Expected: compile error (`Core`, `testing` not found).

- [ ] **Step 3: Implement `adapter.rs`**

```rust
use acd_protocol::{Agent, AnswerPath, ErrorCode, Request, Source};
use async_trait::async_trait;

use crate::registry::Answer;

#[derive(Debug, thiserror::Error)]
pub enum DeliverError {
    /// Nothing reached the agent. The request goes back to `open`.
    #[error("not delivered: {0:?}")]
    NotDelivered(ErrorCode),
    /// Something may have reached the agent, but there is no confirmation.
    #[error("delivery uncertain: {0}")]
    Uncertain(String),
}

/// A source of agents that can also deliver answers and follow-ups.
#[async_trait]
pub trait Adapter: Send + Sync {
    fn name(&self) -> &'static str;
    /// Answer paths this adapter delivers (routing for `request.answer`).
    fn answer_paths(&self) -> &[AnswerPath];
    /// Agent sources this adapter owns (routing for `agent.send` and `agent.mark_seen`).
    fn sources(&self) -> &[Source];
    async fn deliver(&self, request: &Request, answer: &Answer) -> Result<(), DeliverError>;
    async fn send(&self, agent: &Agent, text: &str) -> Result<(), DeliverError>;
    async fn mark_seen(&self, _agent: &Agent) -> Result<(), DeliverError> { Ok(()) }
}
```

- [ ] **Step 4: Implement `core.rs`**

```rust
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use acd_protocol::*;
use sha2::{Digest, Sha256};
use tokio::sync::broadcast;

use crate::adapter::{Adapter, DeliverError};
use crate::idem::{Begin, IdemCache};
use crate::log::EventLog;
use crate::registry::{Answer, Change, Observation, Registry};

pub const RING_CAPACITY: usize = 1000;
pub const IDEM_TTL: Duration = Duration::from_secs(900);

struct Inner {
    machine: Machine,
    registry: Registry,
    log: EventLog,
    idem: IdemCache,
}

pub struct Core {
    inner: Mutex<Inner>,
    events: broadcast::Sender<ServerMessage>,
    adapters: Vec<Arc<dyn Adapter>>,
}

impl Core {
    pub fn new(machine: Machine, adapters: Vec<Arc<dyn Adapter>>) -> Arc<Core> {
        let epoch = uuid::Uuid::new_v4().simple().to_string();
        let (events, _) = broadcast::channel(RING_CAPACITY);
        Arc::new(Core {
            inner: Mutex::new(Inner {
                registry: Registry::new(machine.id.clone()),
                machine,
                log: EventLog::new(epoch, RING_CAPACITY),
                idem: IdemCache::new(IDEM_TTL),
            }),
            events,
            adapters,
        })
    }

    pub fn epoch(&self) -> String { self.inner.lock().unwrap().log.epoch().to_string() }

    pub fn observe(&self, obs: Observation) {
        let mut st = self.inner.lock().unwrap();
        let changes = st.registry.apply(obs);
        self.publish(&mut st, changes);
    }

    pub fn set_source_status(&self, source: Source, status: SourceStatus) {
        let mut st = self.inner.lock().unwrap();
        let slot = match source {
            Source::Hooks => &mut st.machine.sources.hooks,
            Source::Herdr => &mut st.machine.sources.herdr,
            Source::T3 => &mut st.machine.sources.t3,
        };
        if *slot == status { return; }
        *slot = status;
        let machine = st.machine.clone();
        self.publish(&mut st, vec![Change::MachineUpdated(machine)]);
    }

    pub fn welcome(&self) -> ServerMessage {
        let st = self.inner.lock().unwrap();
        ServerMessage::Welcome {
            machine: st.machine.clone(),
            capabilities: CAPABILITIES.iter().map(|c| c.to_string()).collect(),
            epoch: st.log.epoch().to_string(),
            seq: st.log.seq(),
        }
    }

    /// The initial messages for a new session, plus a live receiver. Both
    /// are taken under one lock, so every live event has a seq greater
    /// than the initial messages (spec §7.4).
    pub fn attach(&self, resume: Option<&Cursor>) -> (Vec<ServerMessage>, broadcast::Receiver<ServerMessage>) {
        let st = self.inner.lock().unwrap();
        let rx = self.events.subscribe();
        let initial = match resume.and_then(|c| st.log.replay_after(c)) {
            Some(events) => events,
            None => vec![ServerMessage::Snapshot {
                epoch: st.log.epoch().to_string(),
                seq: st.log.seq(),
                agents: st.registry.agents(),
            }],
        };
        (initial, rx)
    }

    pub async fn handle(&self, client_id: &str, msg: ClientMessage) -> CommandResult {
        let Some(id) = msg.command_id().map(str::to_owned) else {
            return CommandResult::err("", ErrorCode::BadRequest, "not a command");
        };
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&Sha256::digest(serde_json::to_vec(&msg).expect("serializable")));
        let begin = self.inner.lock().unwrap().idem.begin(client_id, &id, hash, Instant::now());
        match begin {
            Begin::Done(result) => return result,
            Begin::Mismatch => return CommandResult::err(&id, ErrorCode::DuplicateMismatch, ErrorCode::DuplicateMismatch.describe()),
            Begin::Wait(mut rx) => loop {
                if let Some(result) = rx.borrow().clone() { return result; }
                if rx.changed().await.is_err() {
                    return CommandResult::err(&id, ErrorCode::SourceUnavailable, "command abandoned");
                }
            },
            Begin::Run => {}
        }
        let result = self.execute(&id, msg).await;
        let cacheable = !matches!(
            result.error_code(),
            Some(ErrorCode::RequestNotReady | ErrorCode::RequestBusy | ErrorCode::SourceUnavailable)
        );
        self.inner.lock().unwrap().idem.finish(client_id, &id, result.clone(), cacheable, Instant::now());
        result
    }

    async fn execute(&self, id: &str, msg: ClientMessage) -> CommandResult {
        match msg {
            ClientMessage::RequestAnswer { request_id, option_id, text, .. } => {
                let answer = match (option_id, text) {
                    (Some(o), None) => Answer::Option(o),
                    (None, Some(t)) => Answer::Text(t),
                    _ => return CommandResult::err(id, ErrorCode::BadRequest, "exactly one of option_id or text is required"),
                };
                let request = {
                    let mut st = self.inner.lock().unwrap();
                    match st.registry.claim(&request_id, answer.clone()) {
                        Ok((req, changes)) => { self.publish(&mut st, changes); req }
                        Err(code) => return CommandResult::err(id, code, code.describe()),
                    }
                };
                let Some(adapter) = self.adapters.iter().find(|a| a.answer_paths().contains(&request.answer_path)).cloned() else {
                    self.release(&request_id);
                    return CommandResult::err(id, ErrorCode::SourceUnavailable, ErrorCode::SourceUnavailable.describe());
                };
                match adapter.deliver(&request, &answer).await {
                    Ok(()) => CommandResult::delivered(id),
                    Err(DeliverError::NotDelivered(code)) => {
                        self.release(&request_id);
                        CommandResult::err(id, code, code.describe())
                    }
                    Err(DeliverError::Uncertain(why)) => {
                        tracing::warn!(request_id, adapter = adapter.name(), %why, "answer delivery uncertain");
                        let mut st = self.inner.lock().unwrap();
                        let changes = st.registry.resolve_request(&request_id, ResolvedBy::Uncertain);
                        self.publish(&mut st, changes);
                        CommandResult::delivered(id)
                    }
                }
            }
            ClientMessage::AgentSend { agent_id, text, .. } => {
                let agent = match self.inner.lock().unwrap().registry.check_send(&agent_id, &text) {
                    Ok(a) => a,
                    Err(code) => return CommandResult::err(id, code, code.describe()),
                };
                let Some(adapter) = self.adapter_for_source(agent.source) else {
                    return CommandResult::err(id, ErrorCode::SourceUnavailable, ErrorCode::SourceUnavailable.describe());
                };
                match adapter.send(&agent, &text).await {
                    Ok(()) => CommandResult::delivered(id),
                    Err(DeliverError::NotDelivered(code)) => CommandResult::err(id, code, code.describe()),
                    Err(DeliverError::Uncertain(why)) => {
                        tracing::warn!(agent_id, %why, "follow-up delivery uncertain");
                        CommandResult::delivered(id)
                    }
                }
            }
            ClientMessage::AgentMarkSeen { agent_id, .. } => {
                let agent = {
                    let mut st = self.inner.lock().unwrap();
                    match st.registry.mark_seen(&agent_id) {
                        Ok(changes) => self.publish(&mut st, changes),
                        Err(code) => return CommandResult::err(id, code, code.describe()),
                    }
                    st.registry.agent(&agent_id).cloned()
                };
                if let (Some(agent), Some(adapter)) = (agent.as_ref(), agent.as_ref().and_then(|a| self.adapter_for_source(a.source))) {
                    if let Err(e) = adapter.mark_seen(agent).await {
                        tracing::debug!(agent_id, error = %e, "adapter mark_seen failed; acd keeps its own flag");
                    }
                }
                CommandResult::ok(id)
            }
            ClientMessage::Hello { .. } | ClientMessage::Ping { .. } => {
                CommandResult::err(id, ErrorCode::BadRequest, "not a command")
            }
        }
    }

    fn adapter_for_source(&self, source: Source) -> Option<Arc<dyn Adapter>> {
        self.adapters.iter().find(|a| a.sources().contains(&source)).cloned()
    }

    fn release(&self, request_id: &str) {
        let mut st = self.inner.lock().unwrap();
        let changes = st.registry.release(request_id);
        self.publish(&mut st, changes);
    }

    fn publish(&self, st: &mut Inner, changes: Vec<Change>) {
        for change in changes {
            let msg = st.log.record(change);
            let _ = self.events.send(msg);
        }
    }
}
```

- [ ] **Step 5: Implement `testing.rs` and wire up `lib.rs`**

`crates/acd-core/src/testing.rs`:

```rust
//! Test helpers. Enabled by the `testing` feature.
use std::sync::{Arc, Mutex};

use acd_protocol::*;
use async_trait::async_trait;
use tokio::sync::Notify;

use crate::adapter::{Adapter, DeliverError};
use crate::registry::{AgentObs, Answer, CorrelationKey, RequestObs};

pub struct FakeAdapter {
    delivered: Mutex<Vec<(String, Answer)>>,
    sent: Mutex<Vec<(String, String)>>,
    next_error: Mutex<Option<DeliverError>>,
    gate: Option<Arc<Notify>>,
}

impl FakeAdapter {
    pub fn new() -> Arc<Self> { Arc::new(Self::build(None)) }
    /// `deliver` waits until `open_gate` is called.
    pub fn gated() -> Arc<Self> { Arc::new(Self::build(Some(Arc::new(Notify::new())))) }
    fn build(gate: Option<Arc<Notify>>) -> Self {
        Self { delivered: Mutex::default(), sent: Mutex::default(), next_error: Mutex::default(), gate }
    }
    pub fn open_gate(&self) { if let Some(g) = &self.gate { g.notify_waiters(); g.notify_one(); } }
    pub fn fail_next(&self, e: DeliverError) { *self.next_error.lock().unwrap() = Some(e); }
    pub fn delivered(&self) -> Vec<(String, Answer)> { self.delivered.lock().unwrap().clone() }
    pub fn sent(&self) -> Vec<(String, String)> { self.sent.lock().unwrap().clone() }
}

#[async_trait]
impl Adapter for FakeAdapter {
    fn name(&self) -> &'static str { "fake" }
    fn answer_paths(&self) -> &[AnswerPath] { &[AnswerPath::Hook, AnswerPath::Keys, AnswerPath::T3] }
    fn sources(&self) -> &[Source] { &[Source::Herdr, Source::T3, Source::Hooks] }
    async fn deliver(&self, request: &Request, answer: &Answer) -> Result<(), DeliverError> {
        if let Some(g) = &self.gate { g.notified().await; }
        if let Some(e) = self.next_error.lock().unwrap().take() { return Err(e); }
        self.delivered.lock().unwrap().push((request.id.clone(), answer.clone()));
        Ok(())
    }
    async fn send(&self, agent: &Agent, text: &str) -> Result<(), DeliverError> {
        if let Some(e) = self.next_error.lock().unwrap().take() { return Err(e); }
        self.sent.lock().unwrap().push((agent.id.clone(), text.to_string()));
        Ok(())
    }
}

pub fn test_machine() -> Machine {
    Machine {
        id: "test-machine".into(), name: "Test".into(), acd_version: "0.0.0".into(),
        sources: Sources { hooks: SourceStatus::unconfigured(), herdr: SourceStatus::unconfigured(), t3: SourceStatus::unconfigured() },
    }
}

pub fn agent_obs(id: &str) -> AgentObs {
    AgentObs {
        id: id.into(), source: Source::Herdr, kind: AgentKind::Claude, title: "agent".into(),
        cwd: None, project: None, state: AgentState::Working, failed: false, last_message: None, can_send: true,
    }
}

pub fn permission_obs(agent_id: &str, key: &str) -> RequestObs {
    RequestObs {
        agent_id: agent_id.into(), key: CorrelationKey(key.into()), upgrade_from: None,
        kind: RequestKind::Permission, answer_path: AnswerPath::Hook, title: "Bash".into(), detail: "ls".into(),
        options: vec![
            RequestOption { id: "allow".into(), label: "Yes".into(), description: None, effect: Effect::Allow },
            RequestOption { id: "deny".into(), label: "No".into(), description: None, effect: Effect::Deny },
        ],
        questions: None, free_text: false, raw_screen: None, ready: true,
    }
}
```

`crates/acd-core/src/lib.rs`:

```rust
//! The acd daemon core: registry, request lifecycle, event log and acd/1 server.
pub mod adapter;
pub mod core;
pub mod idem;
pub mod log;
pub mod registry;
#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use adapter::{Adapter, DeliverError};
pub use core::{Core, IDEM_TTL, RING_CAPACITY};
pub use idem::{Begin, IdemCache};
pub use log::EventLog;
pub use registry::*;
```

- [ ] **Step 6: Run the tests and confirm they pass**

Run: `cargo test -p acd-core`
Expected: all suites pass. That is `registry`, `claims`, `log`, `idem`,
and `core` with 10 tests.

- [ ] **Step 7: Commit**

```bash
git add crates/acd-core
git commit -m "feat(core): Core with adapter routing, idempotent commands and resume"
```

---

### Task 8: acd/1 WebSocket server

**Files:**
- Create: `crates/acd-core/src/server.rs`
- Modify: `crates/acd-core/src/lib.rs` (add `pub mod server; pub use server::{router, Auth, ServerOptions};`)
- Test: `crates/acd-core/tests/server.rs`

**Interfaces:**
- Consumes: `Core` (Task 7).
- Produces:

```rust
pub struct Auth;
impl Auth {
    pub fn new(token: String) -> Self;
    pub fn rotate(&self, token: String);
    pub fn generation(&self) -> u64;
}
#[derive(Clone)] pub struct ServerOptions {
    pub allowed_origins: Vec<String>, pub hello_timeout: Duration,
    pub max_frame: usize, pub max_preauth: usize, pub rotation_check: Duration,
}
impl Default for ServerOptions // ([], 5 s, 1 MiB, 8, 1 s)
pub fn router(core: Arc<Core>, auth: Arc<Auth>, opts: ServerOptions) -> axum::Router;
pub const CLOSE_BAD_PROTOCOL: u16 = 4400;
pub const CLOSE_UNAUTHORIZED: u16 = 4401;
pub const CLOSE_TOO_SLOW: u16 = 4408;
```

- [ ] **Step 1: Write the failing tests**

`crates/acd-core/tests/server.rs`:

```rust
use std::{sync::Arc, time::Duration};

use acd_core::testing::*;
use acd_core::*;
use acd_protocol::*;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    connect_async, tungstenite::{client::IntoClientRequest, Message}, MaybeTlsStream, WebSocketStream,
};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct Server { core: Arc<Core>, auth: Arc<Auth>, url: String, fake: Arc<FakeAdapter> }

async fn start(fake: Arc<FakeAdapter>) -> Server {
    let core = Core::new(test_machine(), vec![fake.clone()]);
    let auth = Arc::new(Auth::new("secret".into()));
    let opts = ServerOptions { hello_timeout: Duration::from_millis(300), rotation_check: Duration::from_millis(50), ..Default::default() };
    let app = router(core.clone(), auth.clone(), opts);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Server { core, auth, url, fake }
}

async fn connect(url: &str) -> Ws { connect_async(url).await.unwrap().0 }

async fn send(ws: &mut Ws, v: Value) { ws.send(Message::text(v.to_string())).await.unwrap(); }

/// The next JSON frame, or the close code as `{"closed": code}`.
async fn next(ws: &mut Ws) -> Value {
    loop {
        match tokio::time::timeout(Duration::from_secs(2), ws.next()).await.expect("timed out") {
            Some(Ok(Message::Text(t))) => return serde_json::from_str(t.as_str()).unwrap(),
            Some(Ok(Message::Close(frame))) => return json!({"closed": frame.map(|f| u16::from(f.code))}),
            Some(Ok(_)) => continue,
            Some(Err(_)) | None => return json!({"closed": null}),
        }
    }
}

fn hello(token: &str, resume: Option<Value>) -> Value {
    let mut v = json!({"type":"hello","protocol":"acd/1","token":token,"client":{"id":"c1","name":"test","version":"0"}});
    if let Some(r) = resume { v["resume"] = r; }
    v
}

async fn authed(url: &str) -> (Ws, Value, Value) {
    let mut ws = connect(url).await;
    send(&mut ws, hello("secret", None)).await;
    let welcome = next(&mut ws).await;
    let initial = next(&mut ws).await;
    (ws, welcome, initial)
}

#[tokio::test]
async fn bad_token_is_closed_with_4401() {
    let s = start(FakeAdapter::new()).await;
    let mut ws = connect(&s.url).await;
    send(&mut ws, hello("wrong", None)).await;
    assert_eq!(next(&mut ws).await["code"], "unauthorized");
    assert_eq!(next(&mut ws).await["closed"], 4401);
}

#[tokio::test]
async fn unknown_protocol_is_rejected_with_4400() {
    let s = start(FakeAdapter::new()).await;
    let mut ws = connect(&s.url).await;
    let mut h = hello("secret", None);
    h["protocol"] = json!("acd/9");
    send(&mut ws, h).await;
    let err = next(&mut ws).await;
    assert_eq!((err["code"].clone(), err["supported"].clone()), (json!("unsupported_protocol"), json!(["acd/1"])));
    assert_eq!(next(&mut ws).await["closed"], 4400);
}

#[tokio::test]
async fn command_before_hello_is_closed_with_4401() {
    let s = start(FakeAdapter::new()).await;
    let mut ws = connect(&s.url).await;
    send(&mut ws, json!({"type":"ping","id":"1"})).await;
    assert_eq!(next(&mut ws).await["closed"], 4401);
}

#[tokio::test]
async fn silent_client_is_closed_after_hello_timeout() {
    let s = start(FakeAdapter::new()).await;
    let mut ws = connect(&s.url).await;
    assert_eq!(next(&mut ws).await["closed"], 4401);
}

#[tokio::test]
async fn origin_header_is_forbidden() {
    let s = start(FakeAdapter::new()).await;
    let mut req = s.url.as_str().into_client_request().unwrap();
    req.headers_mut().insert("Origin", "https://evil.example".parse().unwrap());
    let err = connect_async(req).await.unwrap_err();
    assert!(err.to_string().contains("403"), "got {err}");
}

#[tokio::test]
async fn hello_returns_welcome_then_snapshot_then_live_events() {
    let s = start(FakeAdapter::new()).await;
    let (mut ws, welcome, initial) = authed(&s.url).await;
    assert_eq!(welcome["type"], "welcome");
    assert_eq!(welcome["capabilities"], json!(["answer","send","mark_seen"]));
    assert_eq!(initial["type"], "snapshot");
    s.core.observe(Observation::AgentUpsert(agent_obs("a1")));
    let ev = next(&mut ws).await;
    assert_eq!((ev["type"].clone(), ev["agent"]["id"].clone()), (json!("agent.updated"), json!("a1")));
    assert!(ev["seq"].as_u64().unwrap() > initial["seq"].as_u64().unwrap());
}

#[tokio::test]
async fn answer_round_trip_and_second_client_gets_busy() {
    let fake = FakeAdapter::gated();
    let s = start(fake.clone()).await;
    s.core.observe(Observation::AgentUpsert(agent_obs("a1")));
    s.core.observe(Observation::RequestSeen(permission_obs("a1", "k1")));
    let (mut a, _, snap) = authed(&s.url).await;
    let (mut b, _, _) = authed(&s.url).await;
    let rid = snap["agents"][0]["pending"]["id"].as_str().unwrap().to_string();
    send(&mut a, json!({"type":"request.answer","id":"x1","request_id":rid,"option_id":"allow"})).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    send(&mut b, json!({"type":"request.answer","id":"y1","request_id":rid,"option_id":"deny"})).await;
    let busy = loop { let m = next(&mut b).await; if m["type"] == "result" { break m; } };
    assert_eq!(busy["error"]["code"], "request_busy");
    fake.open_gate();
    let ok = loop { let m = next(&mut a).await; if m["type"] == "result" { break m; } };
    assert_eq!((ok["ok"].clone(), ok["outcome"].clone()), (json!(true), json!("delivered")));
    assert_eq!(s.fake.delivered().len(), 1);
}

#[tokio::test]
async fn resume_in_same_epoch_replays_and_other_epoch_snapshots() {
    let s = start(FakeAdapter::new()).await;
    let (_, welcome, snap) = authed(&s.url).await;
    let epoch = welcome["epoch"].as_str().unwrap().to_string();
    let seq = snap["seq"].as_u64().unwrap();
    s.core.observe(Observation::AgentUpsert(agent_obs("a1")));

    let mut ws = connect(&s.url).await;
    send(&mut ws, hello("secret", Some(json!({"epoch": epoch, "seq": seq})))).await;
    next(&mut ws).await; // welcome
    assert_eq!(next(&mut ws).await["type"], "agent.updated");

    let mut ws2 = connect(&s.url).await;
    send(&mut ws2, hello("secret", Some(json!({"epoch": "stale", "seq": seq})))).await;
    next(&mut ws2).await;
    assert_eq!(next(&mut ws2).await["type"], "snapshot");
}

#[tokio::test]
async fn malformed_frames_get_bad_request_and_connection_stays_open() {
    let s = start(FakeAdapter::new()).await;
    let (mut ws, _, _) = authed(&s.url).await;
    ws.send(Message::text("{not json")).await.unwrap();
    assert_eq!(next(&mut ws).await["code"], "bad_request");
    ws.send(Message::binary(vec![1u8, 2, 3])).await.unwrap();
    assert_eq!(next(&mut ws).await["code"], "bad_request");
    send(&mut ws, json!({"type":"agent.launch","id":"z9"})).await;
    let r = next(&mut ws).await;
    assert_eq!((r["type"].clone(), r["id"].clone(), r["error"]["code"].clone()), (json!("result"), json!("z9"), json!("bad_request")));
    send(&mut ws, json!({"type":"ping","id":"p1"})).await;
    assert_eq!(next(&mut ws).await, json!({"type":"pong","id":"p1"}));
}

#[tokio::test]
async fn second_hello_is_rejected_but_session_continues() {
    let s = start(FakeAdapter::new()).await;
    let (mut ws, _, _) = authed(&s.url).await;
    send(&mut ws, hello("secret", None)).await;
    assert_eq!(next(&mut ws).await["code"], "bad_request");
    send(&mut ws, json!({"type":"ping","id":"p2"})).await;
    assert_eq!(next(&mut ws).await["type"], "pong");
}

#[tokio::test]
async fn token_rotation_closes_existing_sessions() {
    let s = start(FakeAdapter::new()).await;
    let (mut ws, _, _) = authed(&s.url).await;
    s.auth.rotate("new-secret".into());
    assert_eq!(next(&mut ws).await["closed"], 4401);
    let mut fresh = connect(&s.url).await;
    send(&mut fresh, hello("new-secret", None)).await;
    assert_eq!(next(&mut fresh).await["type"], "welcome");
}

#[tokio::test]
async fn oversized_frame_closes_the_connection() {
    let s = start(FakeAdapter::new()).await;
    let (mut ws, _, _) = authed(&s.url).await;
    let big = "x".repeat(2 * 1024 * 1024);
    let _ = ws.send(Message::text(format!("{{\"type\":\"ping\",\"id\":\"{big}\"}}"))).await;
    assert!(next(&mut ws).await.get("closed").is_some());
}
```

- [ ] **Step 2: Run the tests and confirm they fail**

Run: `cargo test -p acd-core --test server`
Expected: compile error (`router`, `Auth` not found).

- [ ] **Step 3: Implement the server**

`crates/acd-core/src/server.rs`:

```rust
use std::sync::{Arc, RwLock};
use std::time::Duration;

use acd_protocol::*;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{header::ORIGIN, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use subtle::ConstantTimeEq;
use tokio::sync::{broadcast, mpsc, OwnedSemaphorePermit, Semaphore};

use crate::core::Core;

pub const CLOSE_BAD_PROTOCOL: u16 = 4400;
pub const CLOSE_UNAUTHORIZED: u16 = 4401;
pub const CLOSE_TOO_SLOW: u16 = 4408;

/// The client token. `generation` increments on rotation, and sessions
/// authenticated under an older generation are closed.
pub struct Auth {
    token: RwLock<(u64, String)>,
}

impl Auth {
    pub fn new(token: String) -> Self { Self { token: RwLock::new((0, token)) } }
    pub fn rotate(&self, token: String) {
        let mut g = self.token.write().unwrap();
        g.0 += 1;
        g.1 = token;
    }
    pub fn generation(&self) -> u64 { self.token.read().unwrap().0 }
    fn check(&self, presented: &str) -> Option<u64> {
        let g = self.token.read().unwrap();
        bool::from(g.1.as_bytes().ct_eq(presented.as_bytes())).then_some(g.0)
    }
}

#[derive(Clone)]
pub struct ServerOptions {
    pub allowed_origins: Vec<String>,
    pub hello_timeout: Duration,
    pub max_frame: usize,
    pub max_preauth: usize,
    pub rotation_check: Duration,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            allowed_origins: vec![],
            hello_timeout: Duration::from_secs(5),
            max_frame: 1 << 20,
            max_preauth: 8,
            rotation_check: Duration::from_secs(1),
        }
    }
}

#[derive(Clone)]
struct AppState {
    core: Arc<Core>,
    auth: Arc<Auth>,
    opts: ServerOptions,
    preauth: Arc<Semaphore>,
}

pub fn router(core: Arc<Core>, auth: Arc<Auth>, opts: ServerOptions) -> Router {
    let preauth = Arc::new(Semaphore::new(opts.max_preauth));
    Router::new().route("/", get(upgrade)).with_state(AppState { core, auth, opts, preauth })
}

async fn upgrade(State(st): State<AppState>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    if let Some(origin) = headers.get(ORIGIN) {
        let allowed = origin.to_str().is_ok_and(|o| st.opts.allowed_origins.iter().any(|a| a == o));
        if !allowed { return StatusCode::FORBIDDEN.into_response(); }
    }
    let Ok(permit) = st.preauth.clone().try_acquire_owned() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let max = st.opts.max_frame;
    ws.max_message_size(max).max_frame_size(max).on_upgrade(move |socket| session(st, socket, permit))
}

fn text(msg: &ServerMessage) -> Message {
    Message::Text(serde_json::to_string(msg).expect("serializable").into())
}

fn close_msg(code: u16, reason: &str) -> Message {
    Message::Close(Some(CloseFrame { code, reason: reason.to_string().into() }))
}

fn error(code: ErrorCode, message: &str) -> ServerMessage {
    ServerMessage::Error { code, message: message.into(), supported: None }
}

async fn session(st: AppState, mut socket: WebSocket, permit: OwnedSemaphorePermit) {
    let hello = match tokio::time::timeout(st.opts.hello_timeout, socket.recv()).await {
        Ok(Some(Ok(Message::Text(t)))) => serde_json::from_str::<ClientMessage>(t.as_str()).ok(),
        _ => None,
    };
    let Some(ClientMessage::Hello { protocol, token, client, resume }) = hello else {
        let _ = socket.send(close_msg(CLOSE_UNAUTHORIZED, "hello required")).await;
        return;
    };
    if protocol != PROTOCOL {
        let msg = ServerMessage::Error {
            code: ErrorCode::UnsupportedProtocol,
            message: format!("unsupported protocol {protocol}"),
            supported: Some(vec![PROTOCOL.to_string()]),
        };
        let _ = socket.send(text(&msg)).await;
        let _ = socket.send(close_msg(CLOSE_BAD_PROTOCOL, "unsupported protocol")).await;
        return;
    }
    let Some(generation) = st.auth.check(&token) else {
        let _ = socket.send(text(&error(ErrorCode::Unauthorized, "invalid token"))).await;
        let _ = socket.send(close_msg(CLOSE_UNAUTHORIZED, "unauthorized")).await;
        return;
    };
    drop(permit);

    let (mut sink, mut stream) = socket.split();
    let (out, mut out_rx) = mpsc::channel::<Message>(64);
    let writer = tokio::spawn(async move {
        while let Some(m) = out_rx.recv().await {
            let closing = matches!(m, Message::Close(_));
            if sink.send(m).await.is_err() || closing { break; }
        }
    });

    let _ = out.send(text(&st.core.welcome())).await;
    let (initial, mut events) = st.core.attach(resume.as_ref());
    for m in &initial { let _ = out.send(text(m)).await; }

    let mut tick = tokio::time::interval(st.opts.rotation_check);
    loop {
        tokio::select! {
            frame = stream.next() => match frame {
                Some(Ok(Message::Text(t))) => on_frame(&st, &client.id, t.as_str(), &out),
                Some(Ok(Message::Binary(_))) => {
                    let _ = out.send(text(&error(ErrorCode::BadRequest, "binary frames are not supported"))).await;
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
            ev = events.recv() => match ev {
                Ok(m) => { if out.send(text(&m)).await.is_err() { break; } }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let _ = out.send(close_msg(CLOSE_TOO_SLOW, "client too slow")).await;
                    break;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            _ = tick.tick() => {
                if st.auth.generation() != generation {
                    let _ = out.send(close_msg(CLOSE_UNAUTHORIZED, "token rotated")).await;
                    break;
                }
            }
        }
    }
    drop(out);
    let _ = writer.await;
}

fn on_frame(st: &AppState, client_id: &str, raw: &str, out: &mpsc::Sender<Message>) {
    let value: serde_json::Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => { let _ = out.try_send(text(&error(ErrorCode::BadRequest, "invalid JSON"))); return; }
    };
    let id = value.get("id").and_then(|v| v.as_str()).map(str::to_owned);
    let msg: ClientMessage = match serde_json::from_value(value) {
        Ok(m) => m,
        Err(e) => {
            let reply = match id {
                Some(id) => ServerMessage::Result(CommandResult::err(&id, ErrorCode::BadRequest, &e.to_string())),
                None => error(ErrorCode::BadRequest, &e.to_string()),
            };
            let _ = out.try_send(text(&reply));
            return;
        }
    };
    match msg {
        ClientMessage::Ping { id } => { let _ = out.try_send(text(&ServerMessage::Pong { id })); }
        ClientMessage::Hello { .. } => { let _ = out.try_send(text(&error(ErrorCode::BadRequest, "already authenticated"))); }
        command => {
            let core = st.core.clone();
            let out = out.clone();
            let client = client_id.to_owned();
            tokio::spawn(async move {
                let result = core.handle(&client, command).await;
                let _ = out.send(text(&ServerMessage::Result(result))).await;
            });
        }
    }
}
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p acd-core --test server`
Expected: 12 passed. If `origin_header_is_forbidden` fails because the
error text differs, assert on the tungstenite error instead:
`tokio_tungstenite::tungstenite::Error::Http(resp)` with
`resp.status() == 403`.

- [ ] **Step 5: Run the whole workspace**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: everything passes with no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/acd-core
git commit -m "feat(core): acd/1 WebSocket server with auth, limits and resume"
```

---

### Task 9: `acd` binary: paths, secrets, config, identity, `init` and `token rotate`

**Files:**
- Create: `crates/acd/Cargo.toml`, `crates/acd/src/main.rs`, `crates/acd/src/paths.rs`, `crates/acd/src/secrets.rs`, `crates/acd/src/config.rs`, `crates/acd/src/identity.rs`
- Test: unit tests inside `secrets.rs` and `config.rs`, plus `crates/acd/tests/cli.rs`

**Interfaces:**
- Produces:
  - `paths::config_dir() -> PathBuf`, which honors `ACD_CONFIG_DIR`
  - `secrets::new_token() -> String`
  - `secrets::write_secret(path, value) -> anyhow::Result<()>`
  - `secrets::read_secret(path) -> anyhow::Result<String>`
  - `secrets::ensure_private_dir(path) -> anyhow::Result<()>`
  - `secrets::check_mode(path, max_mode: u32) -> anyhow::Result<()>`
  - `config::Config { machine_name, port, listen, allowed_origins }` with
    `Config::load(dir) -> anyhow::Result<Config>`
  - `identity::machine_id(dir) -> anyhow::Result<String>` and
    `identity::create_machine_id(dir) -> anyhow::Result<String>`
  - CLI commands `acd init [--force]` and `acd token rotate client`

- [ ] **Step 1: Create the crate manifest**

`crates/acd/Cargo.toml`:

```toml
[package]
name = "acd"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
acd-core.workspace = true
acd-protocol.workspace = true
anyhow.workspace = true
axum.workspace = true
base64.workspace = true
clap.workspace = true
futures-util.workspace = true
gethostname.workspace = true
getrandom.workspace = true
serde.workspace = true
serde_json.workspace = true
tokio.workspace = true
tokio-tungstenite.workspace = true
toml.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true

[dev-dependencies]
tempfile.workspace = true
```

- [ ] **Step 2: Write the failing CLI tests**

`crates/acd/tests/cli.rs`:

```rust
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

fn acd(dir: &std::path::Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_acd"));
    c.env("ACD_CONFIG_DIR", dir).env_remove("RUST_LOG");
    c
}

fn mode(p: std::path::PathBuf) -> u32 { std::fs::metadata(p).unwrap().permissions().mode() & 0o777 }

#[test]
fn init_creates_private_files_and_refuses_to_run_twice() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("acd");
    let out = acd(&dir).arg("init").output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(mode(dir.clone()), 0o700);
    for f in ["client.token", "hook.token", "config.toml", "machine-id"] {
        assert_eq!(mode(dir.join(f)), 0o600, "{f}");
    }
    let token = std::fs::read_to_string(dir.join("client.token")).unwrap();
    assert_eq!(token.trim().len(), 43, "32 bytes base64url without padding");
    assert!(String::from_utf8_lossy(&out.stdout).contains(token.trim()));

    let again = acd(&dir).arg("init").output().unwrap();
    assert!(!again.status.success());
    assert!(String::from_utf8_lossy(&again.stderr).contains("already initialized"));
}

#[test]
fn token_rotate_replaces_client_token() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("acd");
    assert!(acd(&dir).arg("init").status().unwrap().success());
    let before = std::fs::read_to_string(dir.join("client.token")).unwrap();
    assert!(acd(&dir).args(["token", "rotate", "client"]).status().unwrap().success());
    let after = std::fs::read_to_string(dir.join("client.token")).unwrap();
    assert_ne!(before, after);
    assert_eq!(mode(dir.join("client.token")), 0o600);
}

#[test]
fn loose_token_mode_is_refused_with_fix() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("acd");
    assert!(acd(&dir).arg("init").status().unwrap().success());
    std::fs::set_permissions(dir.join("client.token"), std::fs::Permissions::from_mode(0o644)).unwrap();
    let out = acd(&dir).arg("run").output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("client.token") && err.contains("chmod 600"), "{err}");
}
```

- [ ] **Step 3: Run the tests and confirm they fail**

Run: `cargo test -p acd --test cli`
Expected: build failure (no `acd` binary).

- [ ] **Step 4: Implement `paths.rs`, `secrets.rs`, `config.rs` and `identity.rs`**

`crates/acd/src/paths.rs`:

```rust
use std::path::PathBuf;

/// `$ACD_CONFIG_DIR`, or `~/.config/acd`.
pub fn config_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("ACD_CONFIG_DIR") { return PathBuf::from(d); }
    let home = std::env::var_os("HOME").expect("HOME is set");
    PathBuf::from(home).join(".config").join("acd")
}
```

`crates/acd/src/secrets.rs`:

```rust
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;

/// 32 random bytes, base64url without padding.
pub fn new_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("OS randomness is available");
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn ensure_private_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// Writes `value` atomically with mode 0600.
pub fn write_secret(path: &Path, value: &str) -> Result<()> {
    let tmp = path.with_extension("tmp");
    let _ = fs::remove_file(&tmp);
    let mut f = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)
        .with_context(|| format!("creating {}", tmp.display()))?;
    f.write_all(value.as_bytes())?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Fails if `path` grants any permission bit outside `max_mode`.
pub fn check_mode(path: &Path, max_mode: u32) -> Result<()> {
    let mode = fs::metadata(path).with_context(|| format!("reading {}", path.display()))?.permissions().mode() & 0o777;
    if mode & !max_mode != 0 {
        bail!(
            "{} has mode {:o}, which lets other users read it. Fix with: chmod {:o} {}",
            path.display(), mode, max_mode, path.display()
        );
    }
    Ok(())
}

pub fn read_secret(path: &Path) -> Result<String> {
    check_mode(path, 0o600)?;
    Ok(fs::read_to_string(path)?.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_unique_and_url_safe() {
        let (a, b) = (new_token(), new_token());
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn check_mode_rejects_group_and_world_bits() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t");
        write_secret(&p, "x").unwrap();
        check_mode(&p, 0o600).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o640)).unwrap();
        let err = check_mode(&p, 0o600).unwrap_err().to_string();
        assert!(err.contains("chmod 600"), "{err}");
    }
}
```

`crates/acd/src/config.rs`:

```rust
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const DEFAULT_PORT: u16 = 47800;

/// `config.toml`. Unknown keys are ignored, so later plans can add sections.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct Config {
    pub machine_name: String,
    pub port: u16,
    pub listen: Vec<String>,
    pub allowed_origins: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            machine_name: gethostname::gethostname().to_string_lossy().into_owned(),
            port: DEFAULT_PORT,
            listen: vec!["loopback".into(), "tailscale".into()],
            allowed_origins: vec![],
        }
    }
}

impl Config {
    pub fn load(dir: &Path) -> Result<Config> {
        let path = dir.join("config.toml");
        if !path.exists() { return Ok(Config::default()); }
        crate::secrets::check_mode(&path, 0o600)?;
        let raw = std::fs::read_to_string(&path)?;
        toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_file_fills_defaults_and_ignores_unknown_keys() {
        let c: Config = toml::from_str("port = 9000\n[herdr]\nenabled = true\n").unwrap();
        assert_eq!(c.port, 9000);
        assert_eq!(c.listen, vec!["loopback", "tailscale"]);
    }
}
```

`crates/acd/src/identity.rs`:

```rust
use std::path::Path;

use anyhow::{Context, Result};

/// `<hostname>-<8 hex>`, created once by `acd init`.
pub fn create_machine_id(dir: &Path) -> Result<String> {
    let host = gethostname::gethostname().to_string_lossy().to_lowercase().replace(|c: char| !c.is_ascii_alphanumeric(), "-");
    let mut b = [0u8; 4];
    getrandom::fill(&mut b).expect("OS randomness is available");
    let id = format!("{host}-{:02x}{:02x}{:02x}{:02x}", b[0], b[1], b[2], b[3]);
    crate::secrets::write_secret(&dir.join("machine-id"), &id)?;
    Ok(id)
}

pub fn machine_id(dir: &Path) -> Result<String> {
    Ok(std::fs::read_to_string(dir.join("machine-id")).context("machine-id missing; run `acd init`")?.trim().to_string())
}
```

- [ ] **Step 5: Implement `main.rs` with `init`, `token rotate` and a `run` stub that checks file modes**

`crates/acd/src/main.rs`:

```rust
mod config;
mod identity;
mod paths;
mod secrets;

use std::path::Path;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "acd", version, about = "agent-caller daemon")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create tokens, machine id and default config.
    Init {
        #[arg(long)]
        force: bool,
    },
    /// Run the daemon in the foreground.
    Run,
    /// Show sources, agents and open requests of the local daemon.
    Status,
    /// Token management.
    Token {
        #[command(subcommand)]
        cmd: TokenCmd,
    },
}

#[derive(Subcommand)]
enum TokenCmd {
    /// Replace a token. Running daemons close sessions that used the old one.
    Rotate { which: TokenKind },
}

#[derive(clap::ValueEnum, Clone, Copy)]
enum TokenKind { Client, Hook }

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_writer(std::io::stderr)
        .init();
    if let Err(e) = real_main() {
        eprintln!("acd: {e:#}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    let cli = Cli::parse();
    let dir = paths::config_dir();
    match cli.cmd {
        Cmd::Init { force } => init(&dir, force),
        Cmd::Token { cmd: TokenCmd::Rotate { which } } => {
            let file = match which { TokenKind::Client => "client.token", TokenKind::Hook => "hook.token" };
            let token = secrets::new_token();
            secrets::write_secret(&dir.join(file), &token)?;
            println!("{token}");
            Ok(())
        }
        Cmd::Run => {
            preflight(&dir)?;
            bail!("`acd run` is implemented in Task 10")
        }
        Cmd::Status => bail!("`acd status` is implemented in Task 10"),
    }
}

/// Checks file modes before the daemon reads anything.
pub(crate) fn preflight(dir: &Path) -> Result<()> {
    secrets::check_mode(dir, 0o700)?;
    for f in ["client.token", "hook.token", "config.toml"] {
        let p = dir.join(f);
        if p.exists() { secrets::check_mode(&p, 0o600)?; }
    }
    Ok(())
}

fn init(dir: &Path, force: bool) -> Result<()> {
    if dir.join("client.token").exists() && !force {
        bail!("{} is already initialized; use --force to regenerate tokens", dir.display());
    }
    secrets::ensure_private_dir(dir)?;
    let client = secrets::new_token();
    secrets::write_secret(&dir.join("client.token"), &client)?;
    secrets::write_secret(&dir.join("hook.token"), &secrets::new_token())?;
    let id = identity::create_machine_id(dir)?;
    let cfg_path = dir.join("config.toml");
    if !cfg_path.exists() {
        secrets::write_secret(&cfg_path, &toml::to_string(&config::Config::default())?)?;
    }
    println!("Initialized acd in {}", dir.display());
    println!("machine id: {id}");
    println!("client token (add it to the Mac app): {client}");
    Ok(())
}
```

- [ ] **Step 6: Run the tests and confirm they pass**

Run: `cargo test -p acd`
Expected: the 3 unit tests and 3 CLI tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/acd
git commit -m "feat(acd): init, token rotation and private config files"
```

---

### Task 10: Bind allowlist, `acd run` and `acd status`

**Files:**
- Create: `crates/acd/src/bind.rs`, `crates/acd/src/daemon.rs`, `crates/acd/src/status.rs`
- Modify: `crates/acd/src/main.rs` (wire up `Run` and `Status`)
- Test: unit tests in `bind.rs`; extend `crates/acd/tests/cli.rs`

**Interfaces:**
- Consumes: `acd_core::{Core, router, Auth, ServerOptions}`, and
  `config`, `secrets` and `identity` from Task 9.
- Produces:
  - `bind::in_tailscale_range(&IpAddr) -> bool`
  - `bind::resolve_listen(listen: &[String], reported: &[IpAddr]) -> anyhow::Result<Vec<IpAddr>>`
  - `bind::tailscale_reported() -> Vec<IpAddr>`
  - `daemon::run(dir) -> anyhow::Result<()>`
  - `status::status(dir) -> anyhow::Result<()>`

- [ ] **Step 1: Write the failing unit tests**

`crates/acd/src/bind.rs` starts as only the test module plus empty
function signatures that return `todo!()`, so that the file compiles:

```rust
use std::net::IpAddr;

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr { s.parse().unwrap() }

    #[test]
    fn tailscale_ranges() {
        assert!(in_tailscale_range(&ip("100.64.0.1")));
        assert!(in_tailscale_range(&ip("100.127.255.254")));
        assert!(!in_tailscale_range(&ip("100.128.0.1")));
        assert!(!in_tailscale_range(&ip("192.168.1.10")));
        assert!(in_tailscale_range(&ip("fd7a:115c:a1e0::2f01:9db1")));
        assert!(!in_tailscale_range(&ip("fd7a:115c:a1e1::1")));
    }

    #[test]
    fn loopback_and_reported_tailscale_addresses_are_allowed() {
        let reported = [ip("100.123.157.75"), ip("fd7a:115c:a1e0::2f01:9db1")];
        let got = resolve_listen(&["loopback".into(), "tailscale".into()], &reported).unwrap();
        assert_eq!(got, vec![ip("127.0.0.1"), ip("::1"), reported[0], reported[1]]);
    }

    #[test]
    fn explicit_addresses_must_be_loopback_or_reported_tailscale() {
        let reported = [ip("100.123.157.75")];
        assert!(resolve_listen(&["100.123.157.75".into()], &reported).is_ok());
        assert!(resolve_listen(&["127.0.0.1".into()], &reported).is_ok());
        let unreported = resolve_listen(&["100.100.100.100".into()], &reported).unwrap_err().to_string();
        assert!(unreported.contains("refusing to listen on 100.100.100.100"));
        assert!(resolve_listen(&["0.0.0.0".into()], &reported).is_err());
        assert!(resolve_listen(&["192.168.1.10".into()], &reported).is_err());
    }

    #[test]
    fn reported_addresses_outside_tailscale_ranges_are_ignored() {
        let got = resolve_listen(&["tailscale".into()], &[ip("10.0.0.5")]).unwrap();
        assert!(got.is_empty());
    }
}
```

- [ ] **Step 2: Run the tests and confirm they fail**

Run: `cargo test -p acd bind`
Expected: FAIL. The functions are missing or call `todo!()`.

- [ ] **Step 3: Implement `bind.rs`**

Put this above the test module:

```rust
use anyhow::{bail, Result};

/// 100.64.0.0/10 or fd7a:115c:a1e0::/48.
pub fn in_tailscale_range(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => { let o = v4.octets(); o[0] == 100 && (o[1] & 0xC0) == 0x40 }
        IpAddr::V6(v6) => { let s = v6.segments(); s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0 }
    }
}

/// The addresses the local Tailscale daemon reports. Empty if Tailscale is
/// unavailable.
pub fn tailscale_reported() -> Vec<IpAddr> {
    let Ok(out) = std::process::Command::new("tailscale").arg("ip").output() else { return vec![] };
    if !out.status.success() { return vec![]; }
    String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.trim().parse().ok()).collect()
}

/// Expands `listen` entries into addresses allowed by spec §7.1.
pub fn resolve_listen(listen: &[String], reported: &[IpAddr]) -> Result<Vec<IpAddr>> {
    let mut out: Vec<IpAddr> = Vec::new();
    let mut push = |ip: IpAddr| if !out.contains(&ip) { out.push(ip) };
    for entry in listen {
        match entry.as_str() {
            "loopback" => { push("127.0.0.1".parse().unwrap()); push("::1".parse().unwrap()); }
            "tailscale" => {
                for ip in reported.iter().filter(|ip| in_tailscale_range(ip)) { push(*ip); }
            }
            other => {
                let ip: IpAddr = other.parse().map_err(|_| anyhow::anyhow!("invalid listen entry {other:?}"))?;
                let allowed = ip.is_loopback() || (in_tailscale_range(&ip) && reported.contains(&ip));
                if !allowed {
                    bail!("refusing to listen on {ip}: only loopback and this machine's Tailscale addresses are allowed");
                }
                push(ip);
            }
        }
    }
    Ok(out)
}
```

- [ ] **Step 4: Run the bind tests and confirm they pass**

Run: `cargo test -p acd bind`
Expected: 4 passed.

- [ ] **Step 5: Write the failing end-to-end CLI test**

Append to `crates/acd/tests/cli.rs`:

```rust
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

#[test]
fn run_serves_loopback_and_status_reports_machine() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("acd");
    assert!(acd(&dir).arg("init").status().unwrap().success());
    let port = free_port();
    let cfg = format!("machine_name = \"e2e-box\"\nport = {port}\nlisten = [\"loopback\"]\n");
    std::fs::write(dir.join("config.toml"), cfg).unwrap();

    let mut daemon = acd(&dir).arg("run").spawn().unwrap();
    let mut ok = None;
    for _ in 0..50 {
        let out = acd(&dir).arg("status").output().unwrap();
        if out.status.success() { ok = Some(String::from_utf8_lossy(&out.stdout).into_owned()); break; }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    daemon.kill().unwrap();
    let out = ok.expect("status never succeeded");
    assert!(out.contains("machine: e2e-box"), "{out}");
    assert!(out.contains("agents: 0"), "{out}");
}
```

Run: `cargo test -p acd --test cli run_serves_loopback`
Expected: FAIL (`acd run` bails with "implemented in Task 10").

- [ ] **Step 6: Implement `daemon.rs`**

```rust
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use acd_core::{router, Auth, Core, ServerOptions};
use acd_protocol::{Machine, SourceStatus, Sources};
use anyhow::{bail, Result};

use crate::{bind, config::Config, identity, secrets};

/// Exit code that asks the supervisor (launchd or systemd) to restart acd
/// after the Tailscale addresses change.
pub const EXIT_ADDRESSES_CHANGED: i32 = 75;

pub async fn run(dir: &Path) -> Result<()> {
    crate::preflight(dir)?;
    let cfg = Config::load(dir)?;
    let token = secrets::read_secret(&dir.join("client.token"))?;
    let machine = Machine {
        id: identity::machine_id(dir)?,
        name: cfg.machine_name.clone(),
        acd_version: env!("CARGO_PKG_VERSION").into(),
        sources: Sources { hooks: SourceStatus::unconfigured(), herdr: SourceStatus::unconfigured(), t3: SourceStatus::unconfigured() },
    };
    let core = Core::new(machine, vec![]);
    let auth = Arc::new(Auth::new(token.clone()));
    let app = router(core, auth.clone(), ServerOptions { allowed_origins: cfg.allowed_origins.clone(), ..Default::default() });

    let reported = bind::tailscale_reported();
    let ips = bind::resolve_listen(&cfg.listen, &reported)?;
    let mut bound = 0;
    for ip in &ips {
        let addr = SocketAddr::new(*ip, cfg.port);
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                tracing::info!(%addr, "listening");
                let app = app.clone();
                tokio::spawn(async move {
                    if let Err(e) = axum::serve(listener, app).await { tracing::error!(%addr, error = %e, "server stopped"); }
                });
                bound += 1;
            }
            Err(e) => tracing::warn!(%addr, error = %e, "could not bind"),
        }
    }
    if bound == 0 { bail!("could not bind any listen address ({ips:?})"); }

    // Token rotation: re-read client.token every 2 s.
    let token_path = dir.join("client.token");
    let rot_auth = auth.clone();
    tokio::spawn(async move {
        let mut current = token;
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            match secrets::read_secret(&token_path) {
                Ok(t) if t != current => { tracing::info!("client token rotated"); rot_auth.rotate(t.clone()); current = t; }
                Ok(_) => {}
                Err(e) => tracing::error!(error = %e, "cannot read client token"),
            }
        }
    });

    // Tailscale address changes: exit so the supervisor restarts acd with
    // the new bind set.
    let listen = cfg.listen.clone();
    let watch = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            let now: Vec<IpAddr> = bind::resolve_listen(&listen, &bind::tailscale_reported()).unwrap_or_default();
            if now != ips { tracing::warn!(?now, "listen addresses changed; exiting for restart"); return; }
        }
    });

    tokio::select! {
        _ = tokio::signal::ctrl_c() => Ok(()),
        _ = watch => std::process::exit(EXIT_ADDRESSES_CHANGED),
    }
}
```

- [ ] **Step 7: Implement `status.rs`**

```rust
use std::path::Path;

use acd_protocol::{Agent, ClientMessage, ClientInfo, ServerMessage, PROTOCOL};
use anyhow::{bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::{config::Config, secrets};

pub async fn status(dir: &Path) -> Result<()> {
    let cfg = Config::load(dir)?;
    let token = secrets::read_secret(&dir.join("client.token"))?;
    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{}/", cfg.port))
        .await
        .context("cannot connect to the local acd; is `acd run` running?")?;
    let hello = ClientMessage::Hello {
        protocol: PROTOCOL.into(),
        token,
        client: ClientInfo { id: "acd-status".into(), name: "acd status".into(), version: env!("CARGO_PKG_VERSION").into() },
        resume: None,
    };
    ws.send(Message::text(serde_json::to_string(&hello)?)).await?;
    let mut welcome = None;
    while let Some(frame) = ws.next().await {
        let Message::Text(t) = frame? else { continue };
        match serde_json::from_str::<ServerMessage>(t.as_str())? {
            ServerMessage::Welcome { machine, epoch, .. } => welcome = Some((machine, epoch)),
            ServerMessage::Snapshot { agents, .. } => {
                let Some((machine, epoch)) = welcome else { bail!("snapshot before welcome") };
                println!("machine: {} ({})  epoch {}", machine.name, machine.id, epoch);
                println!("sources: hooks={:?} herdr={:?} t3={:?}", machine.sources.hooks.state, machine.sources.herdr.state, machine.sources.t3.state);
                println!("agents: {}", agents.len());
                for a in &agents { println!("  {}", line(a)); }
                return Ok(());
            }
            ServerMessage::Error { message, .. } => bail!("daemon refused: {message}"),
            _ => {}
        }
    }
    bail!("connection closed before snapshot")
}

fn line(a: &Agent) -> String {
    let pending = a.pending.as_ref().map(|r| format!("  [{}: {} {}]", r.id, r.title, r.detail)).unwrap_or_default();
    format!("{:?} {:?} {} {}{}", a.state, a.kind, a.title, a.cwd.as_deref().unwrap_or(""), pending)
}
```

- [ ] **Step 8: Wire the commands in `main.rs`**

Add `mod bind; mod daemon; mod status;` and replace the two stub arms:

```rust
        Cmd::Run => tokio::runtime::Runtime::new()?.block_on(daemon::run(&dir)),
        Cmd::Status => tokio::runtime::Runtime::new()?.block_on(status::status(&dir)),
```

Remove the `preflight` call that used to sit in the `Run` arm;
`daemon::run` now calls it.

- [ ] **Step 9: Run all the tests and confirm they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: everything passes, including
`run_serves_loopback_and_status_reports_machine` and
`loose_token_mode_is_refused_with_fix`.

- [ ] **Step 10: Manual smoke test over Tailscale**

Run: `ACD_CONFIG_DIR=$(mktemp -d)/acd cargo run -p acd -- init && ACD_CONFIG_DIR=<same dir> cargo run -p acd -- run`
Expected: log lines `listening` for `127.0.0.1:47800`, `[::1]:47800`, and
each address that `tailscale ip` prints. Stop it with Ctrl-C.

- [ ] **Step 11: Commit**

```bash
git add crates/acd
git commit -m "feat(acd): run daemon on loopback and Tailscale, status command"
```

---

## Self-review notes

**Spec coverage.** Each spec section maps to a task as follows:

| Spec section | Task |
|---|---|
| §5 model | 1 |
| §5.1 correlation keys, occurrences, claims, resolution attribution, upgrade, supersede | 3, 4 |
| §5.1 exclusive answer paths: `RequestPath` updates only `open` requests | 3 |
| §7.1 binding | 10 |
| §7.1 limits, Origin, tokens and rotation | 8, 9 |
| §7.2 wire format | 1, 2 |
| §7.3 messages and errors | 2, 7 |
| §7.4 epoch, snapshot boundary, resume | 5, 7 |
| §7.4 idempotency | 6, 7 |
| §7.4 slow clients | 8 (broadcast lag → 4408) |
| §7.5 capabilities | 2, 7 |
| §7.6 normative document, schemas, golden examples | 2 |
| §9 commands: `init`, `run`, `status`, `token rotate` | 9, 10 |

**Out of scope here, owned by later plans (see the roadmap):**
- §6 adapters
- `acd service install`
- `acd hooks …`
- hook endpoint and port 47801
- debug-level log redaction of request content. P1 never logs request
  content.

**Accepted approximations:**
- The 4408 slow-client rule uses tokio broadcast lag at 1000 queued events,
  which is equivalent to "more than 1000 events queued".
- On a Tailscale address change, acd exits with code 75 and relies on the
  supervisor to restart it. `acd service install` (P3) must set
  restart-on-exit. Until then, a foreground `acd run` simply stops.
