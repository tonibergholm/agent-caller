# P2: herdr adapter implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make herdr agents show up in acd: discover herdr sockets, keep
every detected agent's state in sync, open read-only `blocked_unparsed`
requests for blocked panes, deliver follow-ups through `agent.prompt`, and
provide the fingerprint-guarded `keys` delivery engine that P3 plugs its
structured requests into.

**Architecture:** A new crate, `crates/acd-adapters`, with a `herdr`
module (spec §10). Per herdr socket, one **reconciler task** owns every
herdr call that reads state, so results apply in the order they were
issued. A subscription task feeds it hints. A discovery task starts and
stops reconcilers as sockets come and go. `HerdrAdapter` implements
`acd_core::Adapter` for `agent.send` and for the `keys` answer path. The
keys engine checks the dialog fingerprint and sends one key inline, then a
spawned task gathers outcome evidence and reports how the request ended.

**Tech Stack:** Rust (edition 2024), tokio (Unix sockets, timers), serde
and serde_json, sha2, async-trait, tracing. Tests use tokio's paused clock
where timing matters and an in-process fake herdr server.

**Spec:** `docs/superpowers/specs/2026-10-07-agent-inbox-design.md`, mainly
§4 (invariants 2 and 3), §5 and §5.1 (lifecycle), §6 (adapter contract),
§6.4 (herdr) and §12. Gate 6 evidence:
`docs/research/2026-10-08-herdr-gate6.md`. Earlier herdr evidence:
`docs/research/2026-10-07-p0-checks.md` items 4, 5 and 7. Follow-ups this
plan must close: `docs/superpowers/plans/2026-10-07-p1-followups.md`, section
"P2".

## Global Constraints

These apply to every task.

- **herdr compatibility.** `ping` must report `version` `>= 0.9.3, < 0.10`
  and `protocol` `22`. Anything else makes that socket `degraded` with a
  message naming the version.
- **herdr socket protocol** (gate 6, P0 item 7):
  - NDJSON over a Unix socket.
  - **One request per connection.** Write one line, read one reply line,
    close. Never reuse a connection.
  - Request: `{"id":"<str>","method":"<m>","params":{...}}`.
  - Reply: `{"id","result":{"type":...}}` or
    `{"id","error":{"code","message"}}`.
  - `events.subscribe` keeps its connection open. It replies
    `{"result":{"type":"subscription_started"}}`, then streams
    `{"event":"<name>","data":{...}}` lines.
- **Method params** (gate 6, quoted from the live server):
  - `agent.list {}` → `result.agents: [AgentInfo]`. Only panes with a
    detected agent are listed.
  - `agent.get {"target": pane_id}` → `result.agent: AgentInfo`. A pane
    with no agent gives the error `agent_not_found`.
  - `agent.read {"target", "source": "detection"|"visible"|"recent", "lines"?, "strip_ansi"?}`
    → `result.read.text`.
  - `agent.prompt {"target", "text"}` → `result.type == "agent_prompted"`.
    A blocked agent gives the error `agent_blocked`.
  - `agent.send_keys {"target", "keys": [str]}`. Key names: `"1"`…`"9"`,
    `"esc"`, `"backspace"`.
  - `ping {}` → `result.type == "pong"`, `version`, `protocol`.
  - `events.subscribe {"subscriptions": [{"type": "pane.created"}, {"type": "pane.closed"}, {"type": "pane.agent_detected"}, {"type": "pane.updated"}, {"type": "pane.agent_status_changed", "pane_id": p}, ...]}`
- **Event frame names are not the subscription names.**
  - Frames arrive as `pane_created`, `pane_closed`, `pane_agent_detected`
    and `pane_updated`, but as `pane.agent_status_changed` (dotted).
  - A `pane_agent_detected` with `"released": true` means the agent exited.
  - Any other or malformed frame is treated as a "resync" hint, never as an
    error.
- **Ordering** (gate 6, spec §6.4 step 3):
  - `AgentInfo.revision` is per pane and counts metadata changes (title,
    cwd, detection). It does **not** move on agent status changes, so it
    cannot order state results.
  - Every `agent.list` and `agent.get` call for a socket, and the
    fallback's `agent.read`, is made **serially** by that socket's
    reconciler task.
  - Results apply in the order the calls were issued.
  - Never issue those calls concurrently.
  - The keys engine (Task 9) is the one exception: it reads screens with
    `agent.read` outside the reconciler. Those reads never change agent
    state; they only decide its own guard and outcome.
- **Never call `agent.focus`.** Never call `agent.send_keys` or
  `pane.send_keys` outside the keys engine (Task 9). Never call
  `pane.send_text` or `pane.send_input`. Integration tests assert this on
  the fake.
- **Timeouts.**
  - Socket connect: 1 s.
  - A non-subscription call: 3 s total.
  - `deliver`, `send` and `mark_seen` finish within 10 s (adapter contract).
- **Delivery classification** (acceptance §14.5, "exactly once").
  - `NotDelivered`: the connect failed, the request line was not fully
    written, or herdr answered with an `error` object. In these cases
    nothing reached the agent.
  - `Uncertain`: the write succeeded but no reply came (timeout, EOF or
    garbage).
- **Ids.**
  - Agent id: `herdr:<12 hex of sha256(socket path)>:<pane_id>`.
  - Pane ids contain colons (`w1:p2`), so an id is **never parsed back**.
  - Correlation key for a fallback request:
    `herdr:<socket hash>:<pane_id>:<fingerprint>`, reusable.
- **Limits** (spec §5):
  - `raw_screen` keeps at most the last 40 lines.
  - `last_message` keeps at most 500 characters.
  - Follow-up text (§6.4) is rejected with `bad_request` when it:
    - contains control characters other than `\n` and `\t`;
    - contains bidi controls (U+061C, U+200E, U+200F, U+202A–U+202E,
      U+2066–U+2069) or U+2028/U+2029;
    - is longer than 16 KiB.
- **Fallback timing** (§6.4, §5.1):
  - A pane `blocked` for 2 s with no other pending request opens
    `blocked_unparsed`.
  - "Confirmed gone" means the fingerprint was absent from two consecutive
    detection reads.
- **Keys timing** (§5.1, §6.2):
  - Re-read the fingerprint immediately before sending one key.
  - After sending, poll detection for up to 3 s.
  - Read the prompt line just before the key, and again 300 ms, 1 s and
    3 s after.
  - Never retry a key. Never send a corrective key.
- **Sync timing** (§6.4):
  - Full `agent.list` every 5 s while any agent on the socket is blocked,
    and every 30 s otherwise.
  - A full resync on every (re)connect.
  - Discovery runs at startup and every 30 s.
- **Workspace rules.** Rust edition 2024 and `rust-version = "1.90"`, from
  the workspace. CI runs `cargo test --workspace` and
  `cargo clippy --workspace --all-targets -- -D warnings` on macOS and
  Linux. Every task ends with both green.
- **CI trigger.** Push and PR events do not start Actions in this repo yet.
  After pushing, run `gh workflow run CI --ref <branch>` and watch it.

## Review Focus

These are the inputs most likely to hurt a real user that no single task's
happy path covers. Each has a pinned test in the task named.

1. **A herdr socket disappears or herdr restarts** (spec §12):
   - the source becomes `degraded` and its agents show `unknown` at once;
   - they are removed only after the socket has been missing from two
     consecutive discovery scans;
   - a restarted herdr gets a full resync.

   Tests: Task 5 (`connection_loss_marks_agents_unknown_then_resyncs`) and
   Task 7 (`socket_missing_twice_removes_its_agents`).
2. **A pane id reused after `pane_closed`.** A pane id that reappears gets
   a fresh agent with no pending request and no seen flag. Test: Task 5
   (`closed_then_reused_pane_is_a_fresh_agent`).
3. **Retried follow-up after a dropped client.** The same command id sends
   `agent.prompt` once. A herdr reply lost after the write is reported as
   delivered (uncertain), never retried. Tests: Task 8
   (`prompt_reply_lost_after_write_is_uncertain`) and Task 10
   (`follow_up_retry_sends_once`).
4. **The screen changes between the guard read and the key.** A changed
   fingerprint gives `screen_mismatch` and sends no key. A key that lands
   after the dialog closed is caught by the prompt-line check and reported
   `uncertain`. Tests: Task 9 (`mismatch_sends_nothing` and
   `stray_digit_in_prompt_is_uncertain`).
5. **Malformed or unexpected herdr output:**
   - an unknown event name or a non-JSON line on the subscription;
   - an `agent.list` entry with unknown fields or an unknown status;
   - herdr out of the version range.

   None of these may crash a task or drop known agents. Tests: Task 2
   (`client_tolerates_unknown_fields_and_errors`) and Task 5
   (`garbage_event_triggers_resync_not_crash`).

## Out of scope (owned elsewhere)

- **Structured requests, owned by P3.** Claude and Codex permission and
  question requests, hook binding, opening `keys` requests, and the
  outcome evidence: PostToolUse/Stop correlation through the `Evidence`
  trait defined here.
- **Parsed requests reappearing after a restart.** §12 "parsed when the
  fingerprint matches a known format" needs P3's option mapping and
  evidence. In P2 every blocked pane after a restart reappears as
  `blocked_unparsed`, which meets acceptance §14.8 ("no agent stuck,
  requests still on screen reappear").
- **The T3 adapter, owned by P4.** P4 adds `crates/acd-adapters/src/t3/`.
  Expect a trivial merge in `lib.rs` and `Cargo.toml`.

## File structure

```
crates/acd-adapters/
  Cargo.toml
  src/lib.rs                  pub mod text; pub mod herdr;
  src/text.rs                 follow-up text rules, §5 caps            (Task 3)
  src/herdr/mod.rs            HerdrAdapter, HerdrConfig, start()        (Task 8)
  src/herdr/wire.rs           serde types for herdr JSON                (Task 2)
  src/herdr/client.rs         one-shot calls, subscription stream       (Task 2)
  src/herdr/fake.rs           FakeHerdr test server (feature "testing") (Task 2)
  src/herdr/map.rs            ids, state/kind mapping, AgentInfo→AgentObs (Task 3)
  src/herdr/screen.rs         dialog region, fingerprints, prompt line  (Task 4)
  src/herdr/reconciler.rs     per-socket sync task                      (Task 5)
  src/herdr/fallback.rs       blocked_unparsed state machine            (Task 6)
  src/herdr/discovery.rs      socket discovery and supervision          (Task 7)
  src/herdr/keys.rs           keys engine, KeysTargets, Evidence        (Task 9)
  tests/herdr_sync.rs         reconciler, fallback, discovery over FakeHerdr
  tests/herdr_adapter.rs      send, keys over FakeHerdr
crates/acd/src/config.rs      [herdr] section                           (Task 8)
crates/acd/src/daemon.rs      start the herdr adapter                   (Task 8)
crates/acd/tests/herdr_e2e.rs acd/1 client ↔ Core ↔ HerdrAdapter ↔ FakeHerdr (Task 10)
tests/fixtures/herdr/         screens (txt) and api/ (json)             (already recorded)
```

## Task order and parallelism

| Task | Depends on | Can run in parallel with |
|---|---|---|
| 1 Core preparation | — | 2, 3, 4 |
| 2 Crate, wire, client, fake | — | 1 (3 and 4 after its Step 1) |
| 3 Text rules and mapping | 2 Step 1 | 4 |
| 4 Screen parsing | 2 Step 1 | 3 |
| 5 Reconciler | 1, 2, 3 | — |
| 6 Fallback | 4, 5 | — |
| 7 Discovery | 5 | 6 |
| 8 Adapter, config, daemon | 6, 7 | — |
| 9 Keys engine | 4, 8 | — |
| 10 End-to-end and docs | all | — |

Integration owner: the orchestrator merges each task's branch into
`p2-herdr` in the order above. Tasks 3 and 4 touch disjoint files.
Task 1 is the only one that edits `acd-core`.

---
### Task 1: Core preparation (acd-core)

P2 owns these `acd-core` edits. P4 runs in parallel and must not touch
`engine.rs` or `registry.rs` until this task has merged.

**Files:**
- Modify: `crates/acd-core/src/engine.rs` (dead arms, adapter check, accessors)
- Modify: `crates/acd-core/src/registry.rs` (no behaviour change expected; fix only if a new test fails)
- Test: `crates/acd-core/tests/registry.rs`, `crates/acd-core/tests/claims.rs`

**Interfaces:**
- Produces:
  - `Core::agent(&self, agent_id: &str) -> Option<acd_protocol::Agent>`: a clone of the registry entry, taken under the core lock. The herdr fallback (Task 6) uses it to ask "does this agent already have a pending request?".
  - `Core::request_answer` routing is unchanged, but a claim on a request whose `answer_path` has no adapter now fails with `source_unavailable` **before** the claim, so no `answering` → `open` churn is published.

- [ ] **Step 1: Write the failing tests**

Add to `crates/acd-core/tests/registry.rs`:

```rust
#[test]
fn replayed_lost_end_is_a_no_op() {
    let mut r = reg_with_agent();
    r.apply(Observation::RequestSeen(perm("a1", "k", "ls")));
    let first = r.apply(Observation::RequestEnded { agent_id: "a1".into(), key: CorrelationKey("k".into()), by: ResolvedBy::Lost });
    assert_eq!(resolved(&first).len(), 1);
    let again = r.apply(Observation::RequestEnded { agent_id: "a1".into(), key: CorrelationKey("k".into()), by: ResolvedBy::Lost });
    assert!(again.is_empty(), "a replayed Lost must not emit anything: {again:?}");
}

#[test]
fn request_path_is_ignored_unless_open() {
    let mut r = reg_with_agent();
    let rid = opened(&r.apply(Observation::RequestSeen(perm("a1", "k", "ls"))))[0].id.clone();
    r.claim(&rid, Answer::Option("allow".into())).unwrap();
    let changes = r.apply(Observation::RequestPath { agent_id: "a1".into(), key: CorrelationKey("k".into()), answer_path: AnswerPath::Keys });
    assert!(changes.is_empty());
    assert_eq!(r.request(&rid).unwrap().answer_path, AnswerPath::Hook);
}

#[test]
fn removed_then_readded_agent_starts_clean() {
    let mut r = reg_with_agent();
    let rid = opened(&r.apply(Observation::RequestSeen(perm("a1", "k", "ls"))))[0].id.clone();
    r.apply(Observation::AgentGone { agent_id: "a1".into() });
    assert!(r.request(&rid).is_none_or(|q| q.status == RequestStatus::Resolved));
    r.apply(Observation::AgentUpsert(agent("a1")));
    let a = r.agent("a1").unwrap();
    assert!(a.pending.is_none(), "a re-added agent has no pending request");
    // The old key is forgotten, so the same key opens a fresh request.
    let again = opened(&r.apply(Observation::RequestSeen(perm("a1", "k", "ls"))));
    assert_eq!(again.len(), 1);
    assert_ne!(again[0].id, rid);
}
```

Add to `crates/acd-core/tests/claims.rs` (reuse that file's existing helpers for building a `Core` with a `FakeAdapter`, a WS-free `handle` call and an event receiver; read the top of the file first):

```rust
#[tokio::test]
async fn answer_without_an_adapter_for_the_path_does_not_claim() {
    // No adapter at all, so no adapter serves the request's Hook path.
    let core = Core::new(test_machine(), vec![]);
    let (_, mut rx) = core.attach(None);
    core.observe(Observation::AgentUpsert(agent_obs("a1")));
    let obs = permission_obs("a1", "k"); // answer_path: Hook, ready: true
    core.observe(Observation::RequestSeen(obs));
    while rx.try_recv().is_ok() {}
    let rid = core.agent("a1").unwrap().pending.unwrap().id;
    let res = core.handle("c1", ClientMessage::RequestAnswer {
        id: "x1".into(), request_id: rid.clone(), option_id: Some("allow".into()), text: None,
    }).await;
    assert_eq!(res.error_code(), Some(ErrorCode::SourceUnavailable));
    assert!(rx.try_recv().is_err(), "no answering/open churn may be published");
    assert_eq!(core.agent("a1").unwrap().pending.unwrap().status, RequestStatus::Open);
}
```

Read the top of `claims.rs` first and match its imports.

- [ ] **Step 2: Run the tests and confirm which fail**

Run: `cargo test -p acd-core --test registry --test claims`
Expected: `answer_without_an_adapter_for_the_path_does_not_claim` fails to
compile (`Core::agent` does not exist). The three registry tests may already
pass; if one fails, fix `registry.rs` minimally so it passes and say so in
the commit message.

- [ ] **Step 3: Implement**

In `engine.rs`:

```rust
    /// A copy of one agent's current state (adapters use it to see whether
    /// the agent already has a pending request).
    pub fn agent(&self, agent_id: &str) -> Option<Agent> {
        self.inner.lock().unwrap().registry.agent(agent_id).cloned()
    }
```

In `execute`, for `RequestAnswer`, pick the adapter inside the same lock
as the claim, and undo the claim without publishing it when there is none:

```rust
                let (request, adapter) = {
                    let mut st = self.inner.lock().unwrap();
                    match st.registry.claim(&request_id, answer.clone()) {
                        Ok((req, changes)) => {
                            let Some(adapter) = self.adapters.iter().find(|a| a.answer_paths().contains(&req.answer_path)).cloned() else {
                                // Nothing was published, so clients never see answering -> open churn.
                                st.registry.release_quiet(&request_id);
                                return (CommandResult::err(id, ErrorCode::SourceUnavailable, ErrorCode::SourceUnavailable.describe()), Delivery::Attempted);
                            };
                            *phase.lock().unwrap() = Phase::Claimed(request_id.clone());
                            self.publish(&mut st, changes);
                            (req, adapter)
                        }
                        Err(code) => return (CommandResult::err(id, code, code.describe()), Delivery::Attempted),
                    }
                };
```

Delete the old `let Some(adapter) = ... else { self.release(...) }` block
that followed the claim.

`claim` keeps its error precedence (`request_gone`, `request_busy`,
`not_answerable`, `request_not_ready`, `bad_request`), which is why the
adapter check runs after a successful claim, inside the same lock. Add to `registry.rs`:

```rust
    /// Undoes a claim whose changes were never published (no adapter).
    pub(crate) fn release_quiet(&mut self, request_id: &str) {
        let _ = self.release(request_id);
    }
```

Remove the dead `ClientMessage::Hello { .. } | ClientMessage::Ping { .. }`
arm at the end of `execute` by matching it as
`ClientMessage::Hello { .. } | ClientMessage::Ping { .. } => unreachable!("rejected in Core::handle")`.
The `AbandonGuard` already turns a panic into an uncached
`source_unavailable`, so this cannot hang a client.

- [ ] **Step 4: Run the whole crate**

Run: `cargo test -p acd-core && cargo clippy -p acd-core --all-targets -- -D warnings`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/acd-core
git commit -m "acd-core: check the adapter before claiming, add Core::agent, registry tests"
```

---

### Task 2: Crate skeleton, herdr wire types, socket client and FakeHerdr

**Files:**
- Create: `crates/acd-adapters/Cargo.toml`, `crates/acd-adapters/src/lib.rs`, `crates/acd-adapters/src/herdr/mod.rs`
- Create: `crates/acd-adapters/src/herdr/wire.rs`, `crates/acd-adapters/src/herdr/client.rs`, `crates/acd-adapters/src/herdr/fake.rs`
- Modify: root `Cargo.toml` (`acd-adapters = { path = "crates/acd-adapters" }` under `[workspace.dependencies]`)
- Test: `crates/acd-adapters/tests/herdr_client.rs`

**Interfaces:**
- Produces (later tasks use these exact names):
  - `wire::AgentStatus` enum `{ Idle, Working, Blocked, Done, #[serde(other)] Unknown }` (`snake_case`).
  - `wire::AgentInfo { pane_id: String, agent: Option<String>, agent_status: AgentStatus, cwd: Option<String>, name: Option<String>, title: Option<String>, terminal_title_stripped: Option<String>, revision: u64, state_change_seq: u64, agent_session: Option<AgentSession> }`. All fields except `pane_id` use `#[serde(default)]`. Unknown fields are ignored.
  - `wire::AgentSession { value: Option<String> }` (P3 binds by it).
  - `wire::Pong { version: String, protocol: u32 }`.
  - `wire::Event` enum, produced by `wire::parse_event(line: &str) -> Event`:
    `Created { pane_id }`, `Closed { pane_id }`, `AgentDetected { pane_id }`, `AgentReleased { pane_id }`, `Updated { pane_id }`, `StatusChanged { pane_id, status: AgentStatus }`, `Other` (anything unknown or malformed).
  - `client::CallError` enum: `Connect(io::Error)`, `Write(io::Error)`, `NoReply(String)` (written, then timeout/EOF/garbage), `Server { code: String, message: String }`.
    - `CallError::reached_agent(&self) -> bool`: `true` only for `NoReply`.
  - `client::Herdr` (cheap `Clone`, holds the socket `PathBuf`):
    - `Herdr::new(path: PathBuf) -> Herdr`, `path(&self) -> &Path`
    - `async fn call(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, CallError>`: returns the `result` object.
    - `async fn ping(&self) -> Result<Pong, CallError>`
    - `async fn list(&self) -> Result<Vec<AgentInfo>, CallError>`
    - `async fn get(&self, pane_id: &str) -> Result<Option<AgentInfo>, CallError>`: `Ok(None)` on `agent_not_found`.
    - `async fn read(&self, pane_id: &str, source: &str, lines: Option<u32>) -> Result<String, CallError>`: `agent.read` with `strip_ansi: true`, returns `read.text`.
    - `async fn prompt(&self, pane_id: &str, text: &str) -> Result<(), CallError>`
    - `async fn send_keys(&self, pane_id: &str, keys: &[&str]) -> Result<(), CallError>`: **only Task 9 may call it.**
    - `async fn subscribe(&self, panes: &[String]) -> Result<Subscription, CallError>`: subscribes to `pane.created`, `pane.closed`, `pane.agent_detected`, `pane.updated`, plus `pane.agent_status_changed` for each pane.
  - `client::Subscription { async fn next(&mut self) -> Option<Event> }`: `None` on EOF or an I/O error.
  - `client::CONNECT_TIMEOUT = 1 s`, `client::CALL_TIMEOUT = 3 s`.
  - `fake::FakeHerdr` (behind `feature = "testing"`; see Step 5 for its API).

- [ ] **Step 1: Skeleton**

`crates/acd-adapters/Cargo.toml`:

```toml
[package]
name = "acd-adapters"
version.workspace = true
edition.workspace = true
license.workspace = true

[features]
testing = []

[dependencies]
acd-core.workspace = true
acd-protocol.workspace = true
async-trait.workspace = true
serde.workspace = true
serde_json.workspace = true
sha2.workspace = true
thiserror.workspace = true
tokio.workspace = true
tracing.workspace = true

[dev-dependencies]
acd-adapters = { path = ".", features = ["testing"] }
acd-core = { workspace = true, features = ["testing"] }
tempfile.workspace = true
```

`src/lib.rs`:

```rust
//! Source adapters for acd (spec §6). P2 adds herdr; P4 adds t3.
pub mod herdr;
pub mod text;
```

`src/herdr/mod.rs` starts as:

```rust
//! herdr adapter (spec §6.4).
pub mod client;
#[cfg(any(test, feature = "testing"))]
pub mod fake;
pub mod wire;
```

Create `text.rs` as an empty file with a `//!` doc line; Task 3 fills it.
Run `cargo build -p acd-adapters` and expect success. Commit:
`git commit -m "acd-adapters: crate skeleton"`. Tasks 3 and 4 may start
from this commit.

- [ ] **Step 2: Write the failing tests**

`crates/acd-adapters/tests/herdr_client.rs`. Fixture paths resolve from the
workspace root:
`concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/herdr/api/")`.

```rust
use acd_adapters::herdr::{client::*, fake::FakeHerdr, wire::*};

fn fixture(name: &str) -> serde_json::Value {
    let p = format!("{}/../../tests/fixtures/herdr/api/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

#[test]
fn recorded_frames_parse() {
    let info: AgentInfo = serde_json::from_value(fixture("agent.get.claude-blocked.json")["result"]["agent"].clone()).unwrap();
    assert_eq!((info.pane_id.as_str(), info.agent.as_deref(), info.agent_status), ("w1:p2", Some("claude"), AgentStatus::Blocked));
    assert!(info.agent_session.unwrap().value.is_some());
    let list: Vec<AgentInfo> = serde_json::from_value(fixture("agent.list.claude-blocked.json")["result"]["agents"].clone()).unwrap();
    assert!(!list.is_empty());
    let line = |n: &str| fixture(n).to_string();
    assert_eq!(parse_event(&line("event-pane_created.json")), Event::Created { pane_id: "w1:p3".into() });
    assert_eq!(parse_event(&line("event-pane_closed.json")), Event::Closed { pane_id: "w1:p3".into() });
    assert_eq!(parse_event(&line("event-pane_agent_detected.json")), Event::AgentDetected { pane_id: "w1:p2".into() });
    assert_eq!(parse_event(&line("event-pane_agent_detected.released.json")), Event::AgentReleased { pane_id: "w1:p1".into() });
    assert_eq!(parse_event(&line("event-pane_updated.json")), Event::Updated { pane_id: "w1:p2".into() });
    assert_eq!(parse_event(&line("event-pane.agent_status_changed.blocked.json")),
        Event::StatusChanged { pane_id: "w1:p2".into(), status: AgentStatus::Blocked });
    assert_eq!(parse_event("not json"), Event::Other);
    assert_eq!(parse_event(r#"{"event":"pane_exploded","data":{}}"#), Event::Other);
}

#[tokio::test]
async fn client_tolerates_unknown_fields_and_errors() {
    let fake = FakeHerdr::start().await;
    fake.set_agent(serde_json::json!({
        "pane_id": "w1:p2", "terminal_id": "t", "workspace_id": "w1", "tab_id": "w1:t1",
        "focused": false, "revision": 1, "agent": "claude", "agent_status": "teleporting", "new_field": [1]
    }));
    let h = Herdr::new(fake.path().to_path_buf());
    let pong = h.ping().await.unwrap();
    assert_eq!((pong.version.as_str(), pong.protocol), ("0.9.3", 22));
    let list = h.list().await.unwrap();
    assert_eq!(list[0].agent_status, AgentStatus::Unknown);
    assert!(h.get("w9:p9").await.unwrap().is_none(), "agent_not_found is Ok(None)");
    match h.call("no.such.method", serde_json::json!({})).await {
        Err(CallError::Server { code, .. }) => assert_eq!(code, "invalid_request"),
        other => panic!("{other:?}"),
    }
    assert_eq!(fake.connections_reused(), 0, "the client must open one connection per request");
}

#[tokio::test]
async fn write_then_silence_is_no_reply_and_counts_as_reached() {
    let fake = FakeHerdr::start().await;
    fake.set_agent(fake_agent("w1:p2", "claude", "idle"));
    fake.hang_next("agent.prompt"); // reads the request, records it, never replies
    let h = Herdr::new(fake.path().to_path_buf());
    let err = tokio::time::timeout(std::time::Duration::from_secs(5), h.prompt("w1:p2", "hi")).await.unwrap().unwrap_err();
    assert!(matches!(err, CallError::NoReply(_)), "{err:?}");
    assert!(err.reached_agent());
    assert_eq!(fake.prompts(), vec![("w1:p2".to_string(), "hi".to_string())]);
}

#[tokio::test]
async fn connect_failure_did_not_reach_the_agent() {
    let dir = tempfile::tempdir().unwrap();
    let h = Herdr::new(dir.path().join("missing.sock"));
    let err = h.prompt("w1:p2", "hi").await.unwrap_err();
    assert!(matches!(err, CallError::Connect(_)));
    assert!(!err.reached_agent());
}

#[tokio::test]
async fn blocked_prompt_is_a_server_error() {
    let fake = FakeHerdr::start().await;
    fake.set_agent(fake_agent("w1:p2", "claude", "blocked"));
    let h = Herdr::new(fake.path().to_path_buf());
    match h.prompt("w1:p2", "hi").await {
        Err(CallError::Server { code, .. }) => assert_eq!(code, "agent_blocked"),
        other => panic!("{other:?}"),
    }
    assert!(fake.prompts().is_empty());
}

#[tokio::test]
async fn subscription_streams_pushed_events_and_ends_on_close() {
    let fake = FakeHerdr::start().await;
    let h = Herdr::new(fake.path().to_path_buf());
    let mut sub = h.subscribe(&["w1:p2".into()]).await.unwrap();
    assert_eq!(fake.subscriptions().last().unwrap(), &vec!["pane.created", "pane.closed", "pane.agent_detected", "pane.updated", "pane.agent_status_changed:w1:p2"]);
    fake.push_raw("garbage");
    fake.push_event(serde_json::json!({"event":"pane_closed","data":{"pane_id":"w1:p2","type":"pane_closed","workspace_id":"w1"}}));
    assert_eq!(sub.next().await, Some(Event::Other));
    assert_eq!(sub.next().await, Some(Event::Closed { pane_id: "w1:p2".into() }));
    fake.drop_subscribers();
    assert_eq!(sub.next().await, None);
}
```

`fake_agent(pane, agent, status) -> serde_json::Value` is a public helper in
`fake.rs` that builds a minimal valid `AgentInfo` JSON
(`terminal_id`, `workspace_id: "w1"`, `tab_id: "w1:t1"`, `focused: false`,
`revision: 1`, `state_change_seq: 1`, `cwd: "/tmp/proj"`).

- [ ] **Step 3: Run them to see them fail**

Run: `cargo test -p acd-adapters --test herdr_client`
Expected: compile errors.

- [ ] **Step 4: Implement `wire.rs` and `client.rs`**

`wire.rs` key parts:

```rust
#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus { Idle, Working, Blocked, Done, #[serde(other)] Unknown }

impl Default for AgentStatus { fn default() -> Self { AgentStatus::Unknown } }

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Created { pane_id: String },
    Closed { pane_id: String },
    AgentDetected { pane_id: String },
    AgentReleased { pane_id: String },
    Updated { pane_id: String },
    StatusChanged { pane_id: String, status: AgentStatus },
    Other,
}

/// Frame names differ from subscription names (gate 6): underscores for
/// most, dotted for status changes. Anything unexpected is `Other`, which
/// the reconciler treats as a resync hint.
pub fn parse_event(line: &str) -> Event {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { return Event::Other };
    let data = &v["data"];
    let pane = |v: &serde_json::Value| v.as_str().map(str::to_owned);
    match v["event"].as_str() {
        Some("pane_created") => pane(&data["pane"]["pane_id"]).map(|pane_id| Event::Created { pane_id }),
        Some("pane_closed") => pane(&data["pane_id"]).map(|pane_id| Event::Closed { pane_id }),
        Some("pane_agent_detected") => pane(&data["pane_id"]).map(|pane_id| {
            if data["released"].as_bool() == Some(true) { Event::AgentReleased { pane_id } } else { Event::AgentDetected { pane_id } }
        }),
        Some("pane_updated") => pane(&data["pane"]["pane_id"]).map(|pane_id| Event::Updated { pane_id }),
        Some("pane.agent_status_changed") => pane(&data["pane_id"]).map(|pane_id| Event::StatusChanged {
            pane_id,
            status: serde_json::from_value(data["agent_status"].clone()).unwrap_or(AgentStatus::Unknown),
        }),
        _ => None,
    }.unwrap_or(Event::Other)
}
```

`client.rs` key parts. Each call opens a fresh `UnixStream`, writes one
line, reads one line and drops the stream:

```rust
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
pub const CALL_TIMEOUT: Duration = Duration::from_secs(3);

impl Herdr {
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, CallError> {
        let stream = timeout(CONNECT_TIMEOUT, UnixStream::connect(&self.path)).await
            .map_err(|_| CallError::Connect(io::Error::new(io::ErrorKind::TimedOut, "connect timed out")))?
            .map_err(CallError::Connect)?;
        let (r, mut w) = stream.into_split();
        let mut line = serde_json::to_vec(&json!({"id": "acd", "method": method, "params": params})).expect("json");
        line.push(b'\n');
        // Until write_all and flush return, nothing has reached herdr.
        timeout(CALL_TIMEOUT, async { w.write_all(&line).await?; w.flush().await }).await
            .map_err(|_| CallError::Write(io::Error::new(io::ErrorKind::TimedOut, "write timed out")))?
            .map_err(CallError::Write)?;
        let mut reply = String::new();
        let n = timeout(CALL_TIMEOUT, BufReader::new(r).read_line(&mut reply)).await
            .map_err(|_| CallError::NoReply("timed out".into()))?
            .map_err(|e| CallError::NoReply(e.to_string()))?;
        if n == 0 { return Err(CallError::NoReply("connection closed".into())); }
        let v: Value = serde_json::from_str(&reply).map_err(|e| CallError::NoReply(format!("bad reply: {e}")))?;
        if let Some(err) = v.get("error") {
            return Err(CallError::Server {
                code: err["code"].as_str().unwrap_or("unknown").into(),
                message: err["message"].as_str().unwrap_or("").into(),
            });
        }
        v.get("result").cloned().ok_or_else(|| CallError::NoReply("reply has no result".into()))
    }
}
```

Implement the typed helpers on top of `call`.

- `get` maps `Server { code: "agent_not_found" }` to `Ok(None)`.
- `prompt` and `send_keys` ignore the result body.
- `subscribe` writes the subscription request and waits up to
  `CALL_TIMEOUT` for the `subscription_started` reply. `Subscription`
  then owns the read half and returns `parse_event(line)` for each line.
- Do not log params or text at `info` level or above. Follow-up text is
  user content, so at `debug` log only its length.

- [ ] **Step 5: Implement `FakeHerdr`**

`fake.rs` (compiled only under `cfg(any(test, feature = "testing"))`) is a
Unix-socket server in a temp dir that behaves like herdr 0.9.3 for the
methods above. It must **close every non-subscription connection after one
reply**, and count any second line on a connection in
`connections_reused()`. State lives behind one `std::sync::Mutex`. Public
API:

```rust
impl FakeHerdr {
    pub async fn start() -> FakeHerdr;                      // version "0.9.3", protocol 22
    pub async fn start_with_version(version: &str, protocol: u32) -> FakeHerdr;
    pub fn path(&self) -> &Path;
    pub fn set_agent(&self, info: Value);                   // insert or replace by pane_id
    pub fn set_status(&self, pane_id: &str, status: &str);  // also pushes a pane.agent_status_changed frame to subscribers of that pane
    pub fn remove_agent(&self, pane_id: &str);              // agent.get -> agent_not_found
    pub fn set_screen(&self, pane_id: &str, source: &str, text: &str); // "detection" | "visible"
    pub fn on_keys(&self, f: impl Fn(&FakeHerdr, &str, &[String]) + Send + Sync + 'static); // runs on agent.send_keys, may change screens
    pub fn hang_next(&self, method: &str);                  // read + record, never reply, hold the connection
    pub fn fail_next(&self, method: &str, code: &str);      // reply with {"error":{"code":code,...}}
    pub fn push_event(&self, frame: Value);
    pub fn push_raw(&self, line: &str);
    pub fn drop_subscribers(&self);
    pub async fn stop(self);                                // close listener and remove the socket file
    pub fn calls(&self) -> Vec<(String, Value)>;            // every request: (method, params)
    pub fn prompts(&self) -> Vec<(String, String)>;
    pub fn keys_sent(&self) -> Vec<(String, Vec<String>)>;
    pub fn subscriptions(&self) -> Vec<Vec<String>>;        // per subscribe call; status entries as "pane.agent_status_changed:<pane>"
    pub fn connections_reused(&self) -> usize;
}
pub fn fake_agent(pane_id: &str, agent: &str, status: &str) -> Value;
```

Behaviour:

- `agent.prompt` on a `blocked` agent replies `agent_blocked`. Otherwise it
  records the prompt and sets the status to `working`.
- Unknown methods reply `invalid_request`.
- `agent.focus` is recorded like every other call and replies
  `{"type":"ok"}`, so tests can assert that it never happens.
- Status frames go only to subscriptions that listed that pane.

- [ ] **Step 6: Run the tests**

Run: `cargo test -p acd-adapters && cargo clippy -p acd-adapters --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml crates/acd-adapters
git commit -m "acd-adapters: herdr wire types, one-shot socket client and FakeHerdr"
```

---

### Task 3: Text rules, limits and the AgentInfo mapping

Pure functions with no I/O. They can be written in parallel with Task 2
once the crate skeleton from Task 2 Step 1 exists. If Task 2 has not started
yet, create the skeleton exactly as Task 2 Step 1 describes and tell the
Task 2 owner.

**Files:**
- Create: `crates/acd-adapters/src/text.rs`
- Create: `crates/acd-adapters/src/herdr/map.rs`
- Modify: `crates/acd-adapters/src/lib.rs` (`pub mod text;`), `crates/acd-adapters/src/herdr/mod.rs` (`pub mod map;`)
- Test: unit tests in both files (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `herdr::wire::AgentInfo` (Task 2).
- Produces:
  - `text::check_prompt_text(text: &str) -> Result<(), &'static str>`: the §6.4 follow-up rule. `Err` carries a short reason.
  - `text::cap_lines(text: &str, max: usize) -> String`: keeps the **last** `max` lines (`raw_screen`, spec §5: at most 40).
  - `text::cap_chars(text: &str, max: usize) -> String`: keeps the first `max` chars, appending `…` when cut (counts `char`s, never splits one) (`last_message`, at most 500).
  - `text::RAW_SCREEN_LINES: usize = 40`, `text::LAST_MESSAGE_CHARS: usize = 500`, `text::PROMPT_MAX_BYTES: usize = 16 * 1024`.
  - `map::socket_hash(path: &Path) -> String`: 12 lowercase hex chars of `sha256(path as bytes)`. The path is used as given after discovery canonicalised it (Task 7).
  - `map::agent_id(socket_hash: &str, pane_id: &str) -> String`: `format!("herdr:{socket_hash}:{pane_id}")`. **Never parse an id back**: pane ids contain colons (`w1:p7`). Task 5 keeps a map from agent id to `(socket, pane_id)`.
  - `map::kind(agent: Option<&str>) -> Option<AgentKind>`: `None` for a pane with no detected agent (the pane is not shown); `"claude"` → `Claude`, `"codex"` → `Codex`, any other string → `Other`.
  - `map::state(s: wire::AgentStatus) -> AgentState`: `Blocked` → `NeedsYou`, `Working` → `Working`, `Done` → `Done`, `Idle` → `Idle`, `Unknown` (and any unknown value) → `Unknown`.
  - `map::agent_obs(socket_hash: &str, info: &wire::AgentInfo) -> Option<AgentObs>`: `None` when `kind(info.agent)` is `None`. Fields: `source: Herdr`; `title`: first non-empty of `info.name`, `info.title`, `info.terminal_title_stripped`, else the pane id; `cwd`: `info.cwd`; `project`: last path component of `cwd`; `state`: `map::state`; `failed: false`; `last_message: None`; `can_send`: `true` for every detected agent (herdr refuses `agent.prompt` while blocked, and the core already rejects `agent.send` while `needs_you`).

- [ ] **Step 1: Write the failing tests**

`text.rs` tests:

```rust
#[test]
fn prompt_text_rules() {
    assert!(check_prompt_text("fix the test\nthen push\tplease").is_ok());
    assert!(check_prompt_text("emoji 👩‍💻 with a ZWJ is fine").is_ok());
    for bad in ["a\u{1b}[31m", "a\u{7}", "a\u{202E}b", "a\u{2066}b", "a\u{200F}b", "a\u{061C}b", "a\u{2028}b", "a\u{2029}b", "a\rb"] {
        assert!(check_prompt_text(bad).is_err(), "{bad:?} must be rejected");
    }
    assert!(check_prompt_text(&"x".repeat(PROMPT_MAX_BYTES)).is_ok());
    assert!(check_prompt_text(&"x".repeat(PROMPT_MAX_BYTES + 1)).is_err());
    assert!(check_prompt_text("   \n ").is_err(), "blank text is rejected");
}

#[test]
fn caps() {
    let screen: String = (1..=50).map(|i| format!("line {i}\n")).collect();
    let capped = cap_lines(&screen, RAW_SCREEN_LINES);
    assert_eq!(capped.lines().count(), 40);
    assert!(capped.starts_with("line 11") && capped.ends_with("line 50"));
    assert_eq!(cap_lines("a\nb", 40), "a\nb");
    let long = "é".repeat(600);
    let c = cap_chars(&long, LAST_MESSAGE_CHARS);
    assert_eq!(c.chars().count(), LAST_MESSAGE_CHARS);
    assert!(c.ends_with('…'));
    assert_eq!(cap_chars("short", 500), "short");
}
```

`map.rs` tests (build `AgentInfo` with `serde_json::from_value`, so unknown
fields and missing optional fields are exercised):

```rust
fn info(v: serde_json::Value) -> wire::AgentInfo { serde_json::from_value(v).unwrap() }

#[test]
fn ids_are_namespaced_and_never_parsed() {
    let h = socket_hash(Path::new("/Users/user/.config/herdr/herdr.sock"));
    assert_eq!(h.len(), 12);
    assert!(h.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    assert_ne!(h, socket_hash(Path::new("/Users/user/.config/herdr/sessions/p0/herdr.sock")));
    assert_eq!(agent_id(&h, "w1:p7"), format!("herdr:{h}:w1:p7"));
}

#[test]
fn maps_states_kinds_and_titles() {
    let base = serde_json::json!({
        "pane_id": "w1:p2", "terminal_id": "t1", "workspace_id": "w1", "tab_id": "w1:t1",
        "focused": false, "revision": 3, "agent_status": "blocked", "agent": "claude",
        "cwd": "/Users/user/src/app", "title": "fix tests", "some_future_field": 1
    });
    let o = agent_obs("abc", &info(base.clone())).unwrap();
    assert_eq!(o.id, "herdr:abc:w1:p2");
    assert_eq!((o.kind, o.state, o.source), (AgentKind::Claude, AgentState::NeedsYou, Source::Herdr));
    assert_eq!(o.project.as_deref(), Some("app"));
    assert_eq!(o.title, "fix tests");
    assert!(o.can_send);

    let mut v = base.clone(); v["agent"] = "aider".into(); v["agent_status"] = "done".into();
    let o = agent_obs("abc", &info(v)).unwrap();
    assert_eq!((o.kind, o.state), (AgentKind::Other, AgentState::Done));

    let mut v = base.clone(); v["agent"] = serde_json::Value::Null;
    assert!(agent_obs("abc", &info(v)).is_none(), "a plain shell pane is not an agent");

    let mut v = base.clone(); v["agent_status"] = "hibernating".into();
    assert_eq!(agent_obs("abc", &info(v)).unwrap().state, AgentState::Unknown);

    let mut v = base; v["title"] = serde_json::Value::Null; v["cwd"] = serde_json::Value::Null;
    let o = agent_obs("abc", &info(v)).unwrap();
    assert_eq!(o.title, "w1:p2");
    assert_eq!((o.cwd, o.project), (None, None));
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p acd-adapters text:: map::`
Expected: compile errors (functions missing).

- [ ] **Step 3: Implement**

```rust
// text.rs
//! Text rules shared by adapters (spec §5 limits, §6.4 follow-ups).

pub const RAW_SCREEN_LINES: usize = 40;
pub const LAST_MESSAGE_CHARS: usize = 500;
pub const PROMPT_MAX_BYTES: usize = 16 * 1024;

/// Bidi controls and line/paragraph separators. Other format characters
/// (ZWJ, ZWNJ, variation selectors) appear in ordinary emoji and are allowed.
fn is_forbidden_format(c: char) -> bool {
    matches!(c, '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{2028}' | '\u{2029}')
}

pub fn check_prompt_text(text: &str) -> Result<(), &'static str> {
    if text.len() > PROMPT_MAX_BYTES { return Err("text longer than 16 KiB"); }
    if text.trim().is_empty() { return Err("text is empty"); }
    if text.chars().any(|c| c.is_control() && c != '\n' && c != '\t') { return Err("text contains control characters"); }
    if text.chars().any(is_forbidden_format) { return Err("text contains bidi or separator characters"); }
    Ok(())
}

pub fn cap_lines(text: &str, max: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(max)..].join("\n")
}

pub fn cap_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max { return text.to_string(); }
    let mut s: String = text.chars().take(max - 1).collect();
    s.push('…');
    s
}
```

`map.rs` implements the functions exactly as listed under **Produces**.
`project` is `Path::new(cwd).file_name()` as a lossy string, `None` for `/`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p acd-adapters && cargo clippy -p acd-adapters --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/acd-adapters
git commit -m "acd-adapters: follow-up text rules, §5 caps and herdr agent mapping"
```

---

### Task 4: Screen parsing: dialog region, fingerprints and the prompt line

Pure functions over screen text, tested against recorded screens. P3 reuses
`find_dialog` to match hook requests to the screen. The keys engine
(Task 9) uses `prompt_line` for the stray-keystroke check.

**Files:**
- Create: `crates/acd-adapters/src/herdr/screen.rs`
- Modify: `crates/acd-adapters/src/herdr/mod.rs` (`pub mod screen;`)
- Test: unit tests in `screen.rs`, reading `tests/fixtures/herdr/*.txt`

**Interfaces:**
- Produces:
  - `screen::Fingerprint(pub String)`: 16 lowercase hex chars. Derives `Clone, Debug, PartialEq, Eq, Hash`.
  - `screen::DialogKind` enum: `ClaudePermission`, `ClaudeQuestion { tabs: bool }`, `CodexPermission`, `CodexCollapsed`.
  - `screen::DialogOption { number: u8, label: String, key_hint: Option<String> }`. `key_hint` is the Codex `(y)`, `(p)` or `(esc)` suffix.
  - `screen::Dialog { kind: DialogKind, options: Vec<DialogOption>, text: String, fingerprint: Option<Fingerprint> }`.
    - `text` is the normalized region.
    - `fingerprint` is `None` for `CodexCollapsed`: the command text is hidden, so the dialog can never be matched (spec §6.3).
  - `screen::find_dialog(text: &str) -> Option<Dialog>`: detects a Claude or Codex dialog anywhere in the text.
  - `screen::fallback_fingerprint(text: &str) -> Fingerprint`:
    - the dialog fingerprint when `find_dialog` finds one that has a fingerprint;
    - otherwise the generic fingerprint of the last 40 non-empty normalized lines.
  - `screen::prompt_line(kind: AgentKind, text: &str) -> Option<String>`: the content of the agent's input box (after the `❯`/`›` marker, trimmed). `None` when a dialog is showing, or for `AgentKind::Other`.

**Rules** (derived from the fixtures; keep them in code comments):

- **Normalization**, used for fingerprints and `Dialog::text`.
  - Per line:
    - strip a leading cursor marker (`❯` or `›`) and surrounding spaces;
    - trim, and collapse runs of whitespace to one space;
    - drop empty lines;
    - drop rule lines, meaning every char is `─` or `╌` and there are at
      least 10 of them;
    - drop lines starting with `Tip:`.
  - Replace `/private/tmp/` with `/tmp/` and `/private/var/` with
    `/var/` (spec §6.2: directory paths are normalized).
  - Join the remaining lines with a single space, so a pane resize that
    re-wraps text keeps the same fingerprint.
  - The fingerprint is the first 16 hex chars of `sha256` of that string.
- **Claude dialog.**
  - The footer is the last line containing `Esc to cancel`.
  - Scan upward from the footer, at most 60 lines.
  - Stop at the first line that starts (after trimming) with `⏺` or `✻`,
    or with `❯` followed by anything other than a digit and a `.`.
  - The region runs from the topmost `─` rule line seen before the stop
    through the footer.
  - Options are lines matching `^\s*(?:❯\s*)?(\d+)\.\s+(.+)$`.
  - It is `ClaudeQuestion` when the region contains a line with `☐`, and
    `tabs` is true when that line starts with `←`. Otherwise it is
    `ClaudePermission`.
- **Codex dialog.**
  - The footer is the last line containing `Press enter to confirm or esc to cancel`.
  - The region starts at the last line above it containing `Would you like to`.
  - Options match `^\s*(?:›\s*)?(\d+)\.\s+(.+?)(?:\s+\(([^)]+)\))?\s*$`.
  - If there is no footer, but a line matches `\[… \d+ lines\] ctrl\+a view all`
    and is followed by an option line, the dialog is `CodexCollapsed`.
- **Prompt line.**
  - Claude: the last line whose trimmed form starts with `❯` and whose
    previous and next non-empty lines are both `─` rule lines.
  - Codex: the last line starting with `› ` among the last 6 non-empty
    lines.
  - Either way, `None` if `find_dialog(text)` is `Some`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use acd_protocol::AgentKind;

    fn fx(name: &str) -> String {
        std::fs::read_to_string(format!("{}/../../tests/fixtures/herdr/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    #[test]
    fn claude_permission() {
        let d = find_dialog(&fx("claude-permission-bash.txt")).unwrap();
        assert_eq!(d.kind, DialogKind::ClaudePermission);
        let labels: Vec<_> = d.options.iter().map(|o| (o.number, o.label.as_str())).collect();
        assert_eq!(labels[0], (1, "Yes"));
        assert_eq!(labels[3], (4, "No"));
        assert!(d.text.contains("touch /tmp/acp0/t1.txt"));
        assert!(!d.text.contains("Tip:"));
        assert!(d.fingerprint.is_some());
    }

    #[test]
    fn fingerprint_ignores_cursor_tip_and_wrapping_but_not_content() {
        let s = fx("claude-permission-bash.txt");
        let fp = find_dialog(&s).unwrap().fingerprint.unwrap();
        let moved = s.replace(" ❯ 1. Yes", "   1. Yes").replace("   4. No", " ❯ 4. No");
        assert_eq!(find_dialog(&moved).unwrap().fingerprint.unwrap(), fp, "cursor position is not content");
        let tip = s.replace("Tip: auto mode handles these prompts for you", "Tip: press ? for shortcuts");
        assert_eq!(find_dialog(&tip).unwrap().fingerprint.unwrap(), fp, "tips rotate");
        let private = s.replace("always allow access to /tmp/acp0", "always allow access to /private/tmp/acp0");
        assert_eq!(find_dialog(&private).unwrap().fingerprint.unwrap(), fp, "§6.2 path normalization");
        let other = s.replace("touch /tmp/acp0/t1.txt", "rm -rf /tmp/acp0");
        assert_ne!(find_dialog(&other).unwrap().fingerprint.unwrap(), fp, "the command is content");
    }

    #[test]
    fn claude_questions() {
        let single = find_dialog(&fx("claude-askuserquestion-single.txt")).unwrap();
        assert_eq!(single.kind, DialogKind::ClaudeQuestion { tabs: false });
        assert_eq!(single.options.iter().map(|o| o.label.as_str()).collect::<Vec<_>>(),
            vec!["Red", "Blue", "Type something.", "Chat about this"]);
        let multi = find_dialog(&fx("claude-askuserquestion-multi.txt")).unwrap();
        assert_eq!(multi.kind, DialogKind::ClaudeQuestion { tabs: true });
        assert_ne!(single.fingerprint, multi.fingerprint);
    }

    #[test]
    fn codex_permission_and_collapsed() {
        let d = find_dialog(&fx("codex-permission-bash-detection.txt")).unwrap();
        assert_eq!(d.kind, DialogKind::CodexPermission);
        assert_eq!(d.options.iter().map(|o| o.key_hint.as_deref()).collect::<Vec<_>>(), vec![Some("y"), Some("p"), Some("esc")]);
        assert!(d.text.contains("touch ~/acp0_test_t5_1.txt"));
        let c = find_dialog(&fx("codex-permission-collapsed-small-pane-detection.txt")).unwrap();
        assert_eq!(c.kind, DialogKind::CodexCollapsed);
        assert!(c.fingerprint.is_none(), "a collapsed dialog can never be matched");
    }

    #[test]
    fn unknown_dialogs_use_the_generic_fingerprint() {
        let trust = fx("codex-trust-dialog-detection.txt");
        assert!(find_dialog(&trust).is_none());
        assert_eq!(fallback_fingerprint(&trust), fallback_fingerprint(&format!("{trust}\n\n   \n")), "blank lines do not count");
        assert_ne!(fallback_fingerprint(&trust), fallback_fingerprint(&trust.replace("Trust this folder?", "Trust that folder?")));
        let perm = fx("claude-permission-bash.txt");
        assert_eq!(fallback_fingerprint(&perm), find_dialog(&perm).unwrap().fingerprint.unwrap());
    }

    #[test]
    fn prompt_lines() {
        assert_eq!(prompt_line(AgentKind::Claude, &fx("claude-idle-prompt-stray-1.txt")).as_deref(), Some("1"));
        assert_eq!(prompt_line(AgentKind::Claude, &fx("claude-idle-detection-stray-1.txt")).as_deref(), Some("1"));
        let idle = prompt_line(AgentKind::Claude, &fx("claude-idle-prompt.txt")).unwrap();
        assert_ne!(idle, "1", "placeholder text, not a stray key");
        assert_eq!(prompt_line(AgentKind::Codex, &fx("codex-idle-prompt-stray-1.txt")).as_deref(), Some("1"));
        assert_eq!(prompt_line(AgentKind::Codex, &fx("codex-idle-detection-stray-1.txt")).as_deref(), Some("1"));
        assert_eq!(prompt_line(AgentKind::Codex, &fx("codex-idle-prompt.txt")).as_deref(), Some("Ask Codex to do anything"));
        assert_eq!(prompt_line(AgentKind::Claude, &fx("claude-permission-bash.txt")), None, "dialog showing");
        assert_eq!(prompt_line(AgentKind::Codex, &fx("codex-permission-bash-detection.txt")), None, "dialog showing");
        assert_eq!(prompt_line(AgentKind::Other, &fx("claude-idle-prompt.txt")), None);
    }
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p acd-adapters screen::`
Expected: compile errors.

- [ ] **Step 3: Implement**

Write `screen.rs` following the **Rules** above.

- Do not add a regex dependency. The patterns are simple enough for
  `str` methods: `trim_start_matches`, `split_once(". ")`, a digit-prefix
  parse and `rsplit_once(" (")`.
- Keep one private `normalize_lines(lines: &[&str]) -> String` that both
  `Dialog::text` and the generic fingerprint use, so the rules cannot
  drift apart.

If a fixture does not match a rule as written, the fixture wins. Adjust
the rule, document the change in a code comment, and say so in the commit
message. Do not edit the fixtures.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p acd-adapters screen:: && cargo clippy -p acd-adapters --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/acd-adapters/src/herdr
git commit -m "acd-adapters: herdr dialog parsing, fingerprints and prompt line"
```

---

### Task 5: The per-socket reconciler

**Files:**
- Create: `crates/acd-adapters/src/herdr/reconciler.rs`
- Modify: `crates/acd-adapters/src/herdr/mod.rs` (`pub mod reconciler;`)
- Test: `crates/acd-adapters/tests/herdr_sync.rs`

**Interfaces:**
- Consumes:
  - `client::Herdr`, `wire::*` (Task 2);
  - `map::*`, `text::*` (Task 3);
  - `acd_core::{Core, Observation}`, `Core::agent` (Task 1).
- Produces:
  - `reconciler::Timing { list_idle: Duration, list_blocked: Duration, backoff_max: Duration, fallback_after: Duration, poll: Duration }`, with `Timing::default()` = 30 s, 5 s, 30 s, 2 s, 1 s. Tests pass millisecond values.
  - `reconciler::PaneRef { herdr: Herdr, pane_id: String, kind: AgentKind, hints: mpsc::Sender<Hint> }` (`Clone`).
  - `reconciler::PaneMap`, the shared map `agent_id -> PaneRef`.
    - `pub type PaneMap = std::sync::RwLock<HashMap<String, PaneRef>>`.
    - The reconciler inserts on upsert and removes on gone.
    - `HerdrAdapter` (Tasks 8 and 9) reads it.
  - `reconciler::Hint` enum: `Pane(String)`, `Closed(String)`, `Resync`. Other tasks send `Hint::Pane(pane_id)` to ask for a fresh `agent.get` after they act on a pane.
  - `reconciler::Health` enum: `Connecting`, `Connected`, `Down(String)`, `Incompatible(String)`.
  - `reconciler::spawn(herdr: Herdr, core: Arc<Core>, panes: Arc<PaneMap>, timing: Timing) -> ReconcilerHandle`
  - `ReconcilerHandle`:
    - `health(&self) -> watch::Receiver<Health>`;
    - `hints(&self) -> mpsc::Sender<Hint>`;
    - `async fn shutdown(self, remove_agents: bool)`. It stops the task. When `remove_agents` is set, it emits `AgentGone` for every agent of this socket and removes them from `PaneMap`.

**Behaviour** (spec §6.4 Sync, gate 6):

1. **Connect.**
   - Call `ping`.
   - A version outside `>= 0.9.3, < 0.10`, or a protocol other than 22,
     gives `Health::Incompatible("herdr <v> protocol <p> is not supported (need 0.9.x, protocol 22)")`.
     The task then retries `ping` every `backoff_max`.
   - On success, call `agent.list`, apply it, subscribe with every known
     pane, and set `Health::Connected`.
2. **Apply a result.** Everything goes through
   `apply(pane_id, Option<AgentInfo>)`.
   - `Some(info)` where `map::agent_obs` returns `Some(obs)`:
     - insert a `PaneRef` into `PaneMap`;
     - emit `Observation::AgentUpsert(obs)` only when `obs` differs from
       the last one emitted for that pane (keep a
       `HashMap<pane_id, AgentObs>`).
   - `None`, or no detected agent: if the pane was known, emit
     `Observation::AgentGone` and remove it from `PaneMap` and from the
     subscription set.
   - A list result applies each entry, then treats every known pane that
     is absent from the list as gone.
3. **Hints.** A separate subscription task reads `Subscription::next()`
   and forwards hints into the reconciler's `mpsc` channel (capacity 256;
   if it is full, send `Hint::Resync` with `try_send` and drop the event).
   - `StatusChanged`, `AgentDetected` and `Updated` become `Hint::Pane`.
   - `Created` becomes `Hint::Pane`. `agent.get` then returns `None` for a
     plain shell, which is fine.
   - `AgentReleased` and `Closed` become `Hint::Closed`.
   - `Other` becomes `Hint::Resync`.
   - When the stream ends, the task sends the internal `Disconnected`
     message.
4. **One task makes every call.** The reconciler loop handles one message
   or timer at a time and `await`s each `agent.list`, `agent.get` and
   `agent.read` call before taking the next, so results apply in issue
   order. Never `spawn` these calls.
5. **Re-subscribe.** When the set of known panes changes, open a new
   subscription with the full set, then drop the old one. Events in the
   overlap only cause extra `agent.get` calls.
6. **Periodic list.** Every `list_blocked` while any known agent is
   `NeedsYou`, otherwise every `list_idle`. A change found this way is a
   missed event: log it at `debug`.
7. **Connection loss.** A `Connect` or `NoReply` error from any call, or a
   subscription EOF, does the following:
   - set `Health::Down(reason)`;
   - emit an `AgentUpsert` with `state: Unknown` for every known agent
     (spec §12);
   - reconnect with exponential backoff (500 ms doubling up to
     `backoff_max`);
   - run step 1 again, which performs a full resync.

   A `Server` error on `agent.get` other than `agent_not_found` is logged
   and turned into `Hint::Resync`.

- [ ] **Step 1: Extend FakeHerdr for these tests**

Add to `fake.rs`, keeping the rest of the Task 2 API:

```rust
    pub async fn start_at(path: &Path) -> FakeHerdr;  // reuse a socket path (herdr restart)
    pub fn max_in_flight(&self) -> usize;               // max concurrent agent.list/get/read connections seen
```

- [ ] **Step 2: Write the failing tests**

`crates/acd-adapters/tests/herdr_sync.rs`:

```rust
use std::{sync::Arc, time::Duration};
use acd_adapters::herdr::{client::Herdr, fake::{FakeHerdr, fake_agent}, map, reconciler::*};
use acd_core::{testing::test_machine, Core};
use acd_protocol::*;

fn fast() -> Timing {
    Timing { list_idle: Duration::from_millis(300), list_blocked: Duration::from_millis(100), backoff_max: Duration::from_millis(200),
             fallback_after: Duration::from_millis(200), poll: Duration::from_millis(50) }
}

async fn until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    for _ in 0..200 { if let Some(v) = f() { return v; } tokio::time::sleep(Duration::from_millis(20)).await; }
    panic!("timed out waiting for {what}");
}

struct Rig { fake: FakeHerdr, core: Arc<Core>, panes: Arc<PaneMap>, handle: ReconcilerHandle, hash: String }

async fn rig(fake: FakeHerdr) -> Rig {
    let core = Core::new(test_machine(), vec![]);
    let panes = Arc::new(PaneMap::default());
    let herdr = Herdr::new(fake.path().to_path_buf());
    let hash = map::socket_hash(fake.path());
    let handle = spawn(herdr, core.clone(), panes.clone(), fast());
    Rig { fake, core, panes, handle, hash }
}

impl Rig {
    fn id(&self, pane: &str) -> String { map::agent_id(&self.hash, pane) }
    fn state(&self, pane: &str) -> Option<AgentState> { self.core.agent(&self.id(pane)).map(|a| a.state) }
}

#[tokio::test]
async fn initial_list_builds_agents_and_ignores_plain_shells() {
    let fake = FakeHerdr::start().await;
    fake.set_agent(fake_agent("w1:p1", "claude", "idle"));
    fake.set_agent(fake_agent("w1:p2", "codex", "working"));
    let r = rig(fake).await;
    until("two agents", || (r.state("w1:p1") == Some(AgentState::Idle) && r.state("w1:p2") == Some(AgentState::Working)).then_some(())).await;
    assert_eq!(r.panes.read().unwrap().len(), 2);
    assert_eq!(*r.handle.health().borrow(), Health::Connected);
}

#[tokio::test]
async fn status_event_triggers_get_and_update() {
    let fake = FakeHerdr::start().await;
    fake.set_agent(fake_agent("w1:p1", "claude", "working"));
    let r = rig(fake).await;
    until("agent", || r.state("w1:p1")).await;
    r.fake.set_status("w1:p1", "blocked");
    until("needs_you", || (r.state("w1:p1") == Some(AgentState::NeedsYou)).then_some(())).await;
}

#[tokio::test]
async fn new_pane_is_subscribed_and_released_agent_removed() {
    let fake = FakeHerdr::start().await;
    let r = rig(fake).await;
    until("connected", || (*r.handle.health().borrow() == Health::Connected).then_some(())).await;
    r.fake.set_agent(fake_agent("w1:p5", "claude", "idle"));
    r.fake.push_event(serde_json::json!({"event":"pane_agent_detected","data":{"agent":"claude","pane_id":"w1:p5","type":"pane_agent_detected","workspace_id":"w1"}}));
    until("p5", || r.state("w1:p5")).await;
    until("resubscribed", || r.fake.subscriptions().last().is_some_and(|s| s.contains(&"pane.agent_status_changed:w1:p5".to_string())).then_some(())).await;
    r.fake.remove_agent("w1:p5");
    r.fake.push_event(serde_json::json!({"event":"pane_agent_detected","data":{"agent":"claude","final_status":"idle","pane_id":"w1:p5","released":true,"type":"pane_agent_detected","workspace_id":"w1"}}));
    until("p5 gone", || r.core.agent(&r.id("w1:p5")).is_none().then_some(())).await;
    assert!(r.panes.read().unwrap().get(&r.id("w1:p5")).is_none());
}

#[tokio::test]
async fn closed_then_reused_pane_is_a_fresh_agent() {
    let fake = FakeHerdr::start().await;
    fake.set_agent(fake_agent("w1:p1", "claude", "done"));
    let r = rig(fake).await;
    until("agent", || r.state("w1:p1")).await;
    r.core.handle("c", ClientMessage::AgentMarkSeen { id: "s1".into(), agent_id: r.id("w1:p1") }).await;
    assert_eq!(r.state("w1:p1"), Some(AgentState::Idle));
    r.fake.remove_agent("w1:p1");
    r.fake.push_event(serde_json::json!({"event":"pane_closed","data":{"pane_id":"w1:p1","type":"pane_closed","workspace_id":"w1"}}));
    until("gone", || r.core.agent(&r.id("w1:p1")).is_none().then_some(())).await;
    r.fake.set_agent(fake_agent("w1:p1", "codex", "done"));
    r.fake.push_event(serde_json::json!({"event":"pane_agent_detected","data":{"agent":"codex","pane_id":"w1:p1","type":"pane_agent_detected","workspace_id":"w1"}}));
    let a = until("re-added", || r.core.agent(&r.id("w1:p1"))).await;
    assert_eq!((a.kind, a.state), (AgentKind::Codex, AgentState::Done), "the old seen flag must not carry over");
    assert!(a.pending.is_none());
}

#[tokio::test]
async fn missed_event_is_caught_by_periodic_list() {
    let fake = FakeHerdr::start().await;
    fake.set_agent(fake_agent("w1:p1", "claude", "working"));
    let r = rig(fake).await;
    until("agent", || r.state("w1:p1")).await;
    r.fake.set_agent(fake_agent("w1:p1", "claude", "idle")); // no event pushed
    until("idle via list", || (r.state("w1:p1") == Some(AgentState::Idle)).then_some(())).await;
}

#[tokio::test]
async fn connection_loss_marks_agents_unknown_then_resyncs() {
    let fake = FakeHerdr::start().await;
    fake.set_agent(fake_agent("w1:p1", "claude", "working"));
    let path = fake.path().to_path_buf();
    let r = rig(fake).await;
    until("agent", || r.state("w1:p1")).await;
    let Rig { fake, core, handle, hash, .. } = r;
    fake.stop().await;
    let id = map::agent_id(&hash, "w1:p1");
    until("unknown", || (core.agent(&id).map(|a| a.state) == Some(AgentState::Unknown)).then_some(())).await;
    assert!(matches!(*handle.health().borrow(), Health::Down(_)));
    let fake2 = FakeHerdr::start_at(&path).await;
    fake2.set_agent(fake_agent("w1:p1", "claude", "blocked"));
    until("resynced", || (core.agent(&id).map(|a| a.state) == Some(AgentState::NeedsYou)).then_some(())).await;
    assert_eq!(*handle.health().borrow(), Health::Connected);
}

#[tokio::test]
async fn incompatible_version_degrades_and_shows_nothing() {
    let fake = FakeHerdr::start_with_version("0.10.0", 23).await;
    fake.set_agent(fake_agent("w1:p1", "claude", "idle"));
    let r = rig(fake).await;
    until("incompatible", || matches!(*r.handle.health().borrow(), Health::Incompatible(_)).then_some(())).await;
    assert!(r.core.agent(&r.id("w1:p1")).is_none());
}

#[tokio::test]
async fn garbage_event_triggers_resync_not_crash() {
    let fake = FakeHerdr::start().await;
    fake.set_agent(fake_agent("w1:p1", "claude", "idle"));
    let r = rig(fake).await;
    until("agent", || r.state("w1:p1")).await;
    let lists_before = r.fake.calls().iter().filter(|(m, _)| m == "agent.list").count();
    r.fake.push_raw("{{{ not json");
    r.fake.push_event(serde_json::json!({"event":"pane_exploded","data":{}}));
    until("resync list", || (r.fake.calls().iter().filter(|(m, _)| m == "agent.list").count() > lists_before).then_some(())).await;
    assert_eq!(r.state("w1:p1"), Some(AgentState::Idle));
    assert_eq!(*r.handle.health().borrow(), Health::Connected);
}

#[tokio::test]
async fn state_calls_are_serial() {
    let fake = FakeHerdr::start().await;
    for i in 0..5 { fake.set_agent(fake_agent(&format!("w1:p{i}"), "claude", "working")); }
    let r = rig(fake).await;
    until("agents", || r.state("w1:p4")).await;
    for i in 0..5 { r.fake.set_status(&format!("w1:p{i}"), "blocked"); }
    until("all blocked", || (0..5).all(|i| r.state(&format!("w1:p{i}")) == Some(AgentState::NeedsYou)).then_some(())).await;
    assert_eq!(r.fake.max_in_flight(), 1, "agent.list/get/read must never overlap");
}

#[tokio::test]
async fn shutdown_with_removal_emits_gone() {
    let fake = FakeHerdr::start().await;
    fake.set_agent(fake_agent("w1:p1", "claude", "idle"));
    let r = rig(fake).await;
    until("agent", || r.state("w1:p1")).await;
    let id = r.id("w1:p1");
    r.handle.shutdown(true).await;
    assert!(r.core.agent(&id).is_none());
    assert!(r.panes.read().unwrap().is_empty());
}
```

The `garbage_event_triggers_resync_not_crash` test assumes `list_idle` is
longer than the wait. If it is flaky because the periodic list fires
first, lengthen `list_idle` for that one test. Do not weaken the
assertion.

- [ ] **Step 3: Run them to see them fail**

Run: `cargo test -p acd-adapters --test herdr_sync`
Expected: compile errors.

- [ ] **Step 4: Implement `reconciler.rs`**

Structure:

```rust
pub fn spawn(herdr: Herdr, core: Arc<Core>, panes: Arc<PaneMap>, timing: Timing) -> ReconcilerHandle {
    let (hint_tx, hint_rx) = mpsc::channel(256);
    let (health_tx, health_rx) = watch::channel(Health::Connecting);
    let (stop_tx, stop_rx) = oneshot::channel();
    let task = tokio::spawn(Reconciler { herdr, core, panes, timing, hash, hints_tx: hint_tx.clone(), known: HashMap::new(), sub: None, health: health_tx }
        .run(hint_rx, stop_rx));
    ReconcilerHandle { hints: hint_tx, health: health_rx, stop: stop_tx, task }
}
```

- `run` loops through `connect`, then `serve`. `serve` is a
  `tokio::select!` over:
  - `stop_rx`;
  - `hint_rx.recv()`;
  - the list timer;
  - the subscription's `Disconnected`, sent through an internal channel.
- `shutdown(remove_agents)`:
  - sends on `stop` and awaits the task;
  - then, if requested, emits `AgentGone` for every agent of this socket
    and removes it from `PaneMap`.
  - The task returns its `known` map so the handle can do this.
- Compare versions by parsing `major.minor.patch` as numbers. A version
  that doesn't parse is incompatible.
- Log at `debug` only. herdr titles and paths are user content.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p acd-adapters && cargo clippy -p acd-adapters --all-targets -- -D warnings`
Expected: PASS. Then run the sync suite 5 times
(`for i in 1 2 3 4 5; do cargo test -p acd-adapters --test herdr_sync -q || break; done`).
All five runs must pass.

- [ ] **Step 6: Commit**

```bash
git add crates/acd-adapters
git commit -m "acd-adapters: herdr reconciler with serial state calls and resync"
```

---

### Task 6: Read-only fallback for blocked panes

**Files:**
- Create: `crates/acd-adapters/src/herdr/fallback.rs`
- Modify: `crates/acd-adapters/src/herdr/reconciler.rs` (poll branch and observations)
- Test: unit tests in `fallback.rs`; add to `crates/acd-adapters/tests/herdr_sync.rs`

**Interfaces:**
- Consumes:
  - `screen::fallback_fingerprint`, `screen::Fingerprint` (Task 4);
  - `text::cap_lines`, `text::RAW_SCREEN_LINES` (Task 3);
  - the reconciler (Task 5).
- Produces:
  - `fallback::Fallback::new(after: Duration)`, a pure state machine with
    no I/O:
    - `on_state(&mut self, pane: &str, blocked: bool, now: Instant)`;
    - `due(&self, now: Instant) -> Vec<String>`: the panes that need a
      detection read now;
    - `on_read(&mut self, pane: &str, text: &str, other_pending: bool, now: Instant) -> Vec<FbOut>`;
    - `forget(&mut self, pane: &str)`.
  - `fallback::FbOut` enum: `Open { fingerprint: Fingerprint, raw_screen: String }` and `End { fingerprint: Fingerprint }`.
  - `fallback::key(socket_hash: &str, pane_id: &str, fp: &Fingerprint) -> CorrelationKey`: `herdr:<hash>:<pane>:<fp>`.

**Rules:**

- A pane is **due** when either:
  - it has been blocked for at least `after` and has no open fallback; or
  - it has an open fallback, in which case it is read every `poll` until
    the dialog is confirmed gone.
- `on_read` with an open fallback:
  - if the pane is still blocked and the fingerprint equals the open one,
    reset the absent count;
  - otherwise increment it;
  - at 2, emit `End` and clear the open fallback.
- `on_read` with no open fallback, the pane blocked for at least `after`,
  and `other_pending == false`: emit `Open` with
  `raw_screen = cap_lines(text, 40)`.
- When a different dialog replaces the first, the old one ends after two
  reads and the next read opens the new one. The registry sees a new key,
  which is a new request.
- **Reconciler wiring.**
  - Every `poll`, when `due` is not empty, the reconciler reads
    `agent.read source=detection` for each due pane, serially, in the same
    loop.
  - `other_pending` is
    `core.agent(id).and_then(|a| a.pending).is_some_and(|p| p.kind != RequestKind::BlockedUnparsed)`.
  - `Open` becomes `Observation::RequestSeen` with these fields:
    - `kind: BlockedUnparsed`, `answer_path: None`;
    - `title: "Waiting for input in the terminal"`, `detail: ""`;
    - `options: []`, `questions: None`, `free_text: false`;
    - `raw_screen: Some(raw_screen)`, `ready: false`,
      `reusable_key: true`, `upgrade_from: None`;
    - `key: fallback::key(..)`.
  - `End` becomes `Observation::RequestEnded { by: Terminal }`, then
    `Observation::DialogGone`. These requests never have an acd delivery,
    so §5.1 says `terminal`.
  - `apply` calls `on_state` for every result.
  - A gone pane calls `forget`.

- [ ] **Step 1: Write the failing tests**

Unit tests in `fallback.rs`:

```rust
#[test]
fn opens_after_two_seconds_and_ends_after_two_absent_reads() {
    let t0 = Instant::now();
    let s = Duration::from_secs(1);
    let mut f = Fallback::new(2 * s);
    f.on_state("p", true, t0);
    assert!(f.due(t0 + s).is_empty());
    assert_eq!(f.due(t0 + 2 * s), vec!["p".to_string()]);
    let out = f.on_read("p", "dialog A", false, t0 + 2 * s);
    assert!(matches!(&out[..], [FbOut::Open { .. }]));
    assert!(f.on_read("p", "dialog A", false, t0 + 3 * s).is_empty());
    f.on_state("p", false, t0 + 4 * s);
    assert!(f.on_read("p", "prompt", false, t0 + 4 * s).is_empty(), "one absent read is not enough");
    assert!(matches!(&f.on_read("p", "prompt", false, t0 + 5 * s)[..], [FbOut::End { .. }]));
    assert!(f.due(t0 + 6 * s).is_empty());
}

#[test]
fn flicker_resets_the_absent_count() {
    let t0 = Instant::now();
    let mut f = Fallback::new(Duration::ZERO);
    f.on_state("p", true, t0);
    f.on_read("p", "dialog A", false, t0);
    assert!(f.on_read("p", "redraw", false, t0).is_empty());
    assert!(f.on_read("p", "dialog A", false, t0).is_empty());
    assert!(f.on_read("p", "redraw", false, t0).is_empty(), "absent reads must be consecutive");
}

#[test]
fn another_pending_request_suppresses_opening() {
    let t0 = Instant::now();
    let mut f = Fallback::new(Duration::ZERO);
    f.on_state("p", true, t0);
    assert!(f.on_read("p", "dialog A", true, t0).is_empty());
}

#[test]
fn raw_screen_is_capped() {
    let t0 = Instant::now();
    let mut f = Fallback::new(Duration::ZERO);
    f.on_state("p", true, t0);
    let screen: String = (0..100).map(|i| format!("l{i}\n")).collect();
    match &f.on_read("p", &screen, false, t0)[..] {
        [FbOut::Open { raw_screen, .. }] => assert_eq!(raw_screen.lines().count(), 40),
        o => panic!("{o:?}"),
    }
}
```

Add to `tests/herdr_sync.rs`:

```rust
fn screen(name: &str) -> String {
    std::fs::read_to_string(format!("{}/../../tests/fixtures/herdr/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

#[tokio::test]
async fn blocked_pane_gets_a_read_only_request_then_terminal() {
    let fake = FakeHerdr::start().await;
    fake.set_agent(fake_agent("w1:p1", "other-agent", "blocked"));
    fake.set_screen("w1:p1", "detection", &screen("codex-trust-dialog-detection.txt"));
    let r = rig(fake).await;
    let req = until("fallback", || r.core.agent(&r.id("w1:p1")).and_then(|a| a.pending)).await;
    assert_eq!((req.kind, req.answer_path), (RequestKind::BlockedUnparsed, AnswerPath::None));
    assert!(!req.is_answerable());
    assert!(req.raw_screen.as_ref().unwrap().lines().count() <= 40);
    let res = r.core.handle("c", ClientMessage::RequestAnswer { id: "a1".into(), request_id: req.id.clone(), option_id: Some("x".into()), text: None }).await;
    assert_eq!(res.error_code(), Some(ErrorCode::NotAnswerable));
    // Answered in the terminal.
    r.fake.set_screen("w1:p1", "detection", &screen("codex-idle-detection.txt"));
    r.fake.set_status("w1:p1", "idle");
    until("cleared", || r.core.agent(&r.id("w1:p1")).is_some_and(|a| a.pending.is_none()).then_some(())).await;
}

#[tokio::test]
async fn structured_request_suppresses_the_fallback() {
    let fake = FakeHerdr::start().await;
    fake.set_agent(fake_agent("w1:p1", "claude", "working"));
    fake.set_screen("w1:p1", "detection", &screen("claude-permission-bash.txt"));
    let r = rig(fake).await;
    until("agent", || r.state("w1:p1")).await;
    let mut obs = acd_core::testing::permission_obs(&r.id("w1:p1"), "hook-key");
    obs.answer_path = AnswerPath::Hook;
    r.core.observe(acd_core::Observation::RequestSeen(obs));
    r.fake.set_status("w1:p1", "blocked");
    tokio::time::sleep(Duration::from_millis(600)).await; // > fallback_after + several polls
    let p = r.core.agent(&r.id("w1:p1")).unwrap().pending.unwrap();
    assert_eq!(p.kind, RequestKind::Permission, "no blocked_unparsed may replace a structured request");
}

#[tokio::test]
async fn restart_with_a_blocked_pane_reopens_the_request() {
    // Spec §12 / §14.8: after an acd restart, requests still on screen reappear.
    let fake = FakeHerdr::start().await;
    fake.set_agent(fake_agent("w1:p1", "claude", "blocked"));
    fake.set_screen("w1:p1", "detection", &screen("claude-permission-bash.txt"));
    let r = rig(fake).await; // a fresh Core is a restarted acd
    let req = until("reopened", || r.core.agent(&r.id("w1:p1")).and_then(|a| a.pending)).await;
    assert_eq!(req.kind, RequestKind::BlockedUnparsed);
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p acd-adapters fallback:: && cargo test -p acd-adapters --test herdr_sync`
Expected: compile errors, then failures.

- [ ] **Step 3: Implement** `fallback.rs` and the reconciler wiring as
described under **Rules**.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p acd-adapters && cargo clippy -p acd-adapters --all-targets -- -D warnings`.
Run the sync suite 5 times as in Task 5. Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/acd-adapters
git commit -m "acd-adapters: read-only blocked_unparsed fallback for herdr panes"
```

---

### Task 7: Socket discovery, supervision and source status

**Files:**
- Create: `crates/acd-adapters/src/herdr/discovery.rs`
- Modify: `crates/acd-adapters/src/herdr/mod.rs` (`pub mod discovery;`)
- Test: unit tests in `discovery.rs`; add to `tests/herdr_sync.rs`

**Interfaces:**
- Consumes: `reconciler::{spawn, ReconcilerHandle, Health, Timing, PaneMap}` (Task 5).
- Produces:
  - `discovery::candidates(env_socket: Option<&Path>, home: &Path, extra: &[PathBuf]) -> Vec<PathBuf>`:
    - `env_socket`, or else `<home>/.config/herdr/herdr.sock`;
    - then every `<home>/.config/herdr/sessions/*/herdr.sock`;
    - then `extra`.
    - Each path is canonicalized when possible, and the list is
      deduplicated in that order.
  - `discovery::is_live(path: &Path) -> bool`: the path is a socket and
    `UnixStream::connect` succeeds within 1 s.
  - `discovery::Discovery { env_socket: Option<PathBuf>, home: PathBuf, extra: Vec<PathBuf>, interval: Duration, timing: Timing }`
  - `discovery::spawn(d: Discovery, core: Arc<Core>, panes: Arc<PaneMap>) -> JoinHandle<()>`
  - `discovery::aggregate(healths: &[(PathBuf, Health)]) -> SourceStatus`:
    - an empty list gives `unconfigured`, message `"no herdr socket found"`;
    - all `Connected` gives `ok`;
    - otherwise `degraded`, with a message joining the problem sockets,
      for example `"<path>: herdr 0.10.0 protocol 23 is not supported …"`.

**Rules:**

- **Scans.**
  - Scan at startup and every `interval` (30 s).
  - A live candidate with no reconciler gets one, started with
    `reconciler::spawn`.
  - A known socket that is not live in two consecutive scans is shut down
    with `remove_agents = true`. One missed scan only counts toward that.
  - The reconciler has already marked its agents `unknown` through
    `Health::Down`.
- **Source status.** After every scan, and on every health change, call
  `core.set_source_status(Source::Herdr, aggregate(..))`. To watch health
  changes, select over each handle's `health()` receiver, or poll them
  every 500 ms. Both are acceptable; pick the simpler.
- **Scan errors.** An unreadable `sessions` directory is skipped and
  logged once at `debug`. It never stops the scan.

- [ ] **Step 1: Write the failing tests**

Unit tests (with `tempfile`; create sockets with
`std::os::unix::net::UnixListener::bind`):

```rust
#[test]
fn candidate_order_and_dedup() {
    let home = tempfile::tempdir().unwrap();
    let base = home.path().join(".config/herdr");
    std::fs::create_dir_all(base.join("sessions/a")).unwrap();
    std::fs::create_dir_all(base.join("sessions/b")).unwrap();
    let _l1 = std::os::unix::net::UnixListener::bind(base.join("herdr.sock")).unwrap();
    let _l2 = std::os::unix::net::UnixListener::bind(base.join("sessions/a/herdr.sock")).unwrap();
    let extra = vec![base.join("herdr.sock")]; // duplicate of the default
    let c = candidates(None, home.path(), &extra);
    let names: Vec<_> = c.iter().map(|p| p.strip_prefix(base.canonicalize().unwrap()).unwrap().to_path_buf()).collect();
    assert_eq!(names, vec![PathBuf::from("herdr.sock"), PathBuf::from("sessions/a/herdr.sock")]);
    let env = home.path().join("custom.sock");
    assert_eq!(candidates(Some(&env), home.path(), &[])[0].file_name().unwrap(), "custom.sock");
}

#[test]
fn aggregate_status() {
    assert_eq!(aggregate(&[]).state, SourceState::Unconfigured);
    assert_eq!(aggregate(&[("/a".into(), Health::Connected)]).state, SourceState::Ok);
    let s = aggregate(&[("/a".into(), Health::Connected), ("/b".into(), Health::Incompatible("herdr 0.10.0".into()))]);
    assert_eq!(s.state, SourceState::Degraded);
    assert!(s.message.unwrap().contains("/b"));
}
```

Integration tests in `tests/herdr_sync.rs` (`FakeHerdr::start_at` places
a fake at `<home>/.config/herdr/sessions/x/herdr.sock`):

```rust
#[tokio::test]
async fn discovery_finds_sessions_and_reports_status() {
    let home = tempfile::tempdir().unwrap();
    let sock = home.path().join(".config/herdr/sessions/x/herdr.sock");
    std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
    let fake = FakeHerdr::start_at(&sock).await;
    fake.set_agent(fake_agent("w1:p1", "claude", "idle"));
    let core = Core::new(test_machine(), vec![]);
    let panes = Arc::new(PaneMap::default());
    let d = Discovery { env_socket: None, home: home.path().into(), extra: vec![], interval: Duration::from_millis(100), timing: fast() };
    let _task = acd_adapters::herdr::discovery::spawn(d, core.clone(), panes.clone());
    until("agent", || (panes.read().unwrap().len() == 1).then_some(())).await;
    until("ok", || matches!(core.welcome(), ServerMessage::Welcome { ref machine, .. } if machine.sources.herdr.state == SourceState::Ok).then_some(())).await;
}

#[tokio::test]
async fn socket_missing_twice_removes_its_agents() {
    let home = tempfile::tempdir().unwrap();
    let sock = home.path().join(".config/herdr/sessions/x/herdr.sock");
    std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
    let fake = FakeHerdr::start_at(&sock).await;
    fake.set_agent(fake_agent("w1:p1", "claude", "idle"));
    let core = Core::new(test_machine(), vec![]);
    let panes = Arc::new(PaneMap::default());
    let d = Discovery { env_socket: None, home: home.path().into(), extra: vec![], interval: Duration::from_millis(150), timing: fast() };
    let _task = acd_adapters::herdr::discovery::spawn(d, core.clone(), panes.clone());
    let id = until("agent", || panes.read().unwrap().keys().next().cloned()).await;
    fake.stop().await;
    until("unknown first", || (core.agent(&id).map(|a| a.state) == Some(AgentState::Unknown)).then_some(())).await;
    until("removed", || core.agent(&id).is_none().then_some(())).await;
    until("unconfigured", || matches!(core.welcome(), ServerMessage::Welcome { ref machine, .. } if machine.sources.herdr.state == SourceState::Unconfigured).then_some(())).await;
}
```

- [ ] **Step 2: Run them to see them fail.** Expected: compile errors.

- [ ] **Step 3: Implement** `discovery.rs` following the **Rules** above.

- [ ] **Step 4: Run the tests** as in Task 5, including the five-times
loop. Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/acd-adapters
git commit -m "acd-adapters: herdr socket discovery and source status"
```

---

### Task 8: HerdrAdapter, follow-ups, config and daemon wiring

**Files:**
- Modify: `crates/acd-adapters/src/herdr/mod.rs` (`HerdrAdapter`, `HerdrConfig`)
- Modify: `crates/acd/Cargo.toml` (`acd-adapters.workspace = true`), `crates/acd/src/config.rs`, `crates/acd/src/daemon.rs`
- Test: `crates/acd-adapters/tests/herdr_adapter.rs`, unit test in `crates/acd/src/config.rs`

**Interfaces:**
- Consumes:
  - `discovery::{spawn, Discovery}` (Task 7);
  - `reconciler::{PaneMap, PaneRef, Hint, Timing}` (Task 5);
  - `text::check_prompt_text` (Task 3).
- Produces:
  - `herdr::HerdrConfig { enabled: bool, home: PathBuf, env_socket: Option<PathBuf>, extra_sockets: Vec<PathBuf>, scan_interval: Duration, timing: Timing }`.
    - `HerdrConfig::from_env(enabled: bool, extra_sockets: Vec<PathBuf>) -> HerdrConfig` reads `$HOME` and `$HERDR_SOCKET_PATH`, and uses the default timing with a 30 s scan.
  - `herdr::HerdrAdapter`:
    - `HerdrAdapter::new(cfg: HerdrConfig) -> Arc<HerdrAdapter>`;
    - `start(self: &Arc<Self>, core: Arc<Core>)`. It spawns discovery and
      keeps a `Weak<Core>` for Task 9. A disabled config only calls
      `core.set_source_status(Source::Herdr, SourceStatus { state: Disabled, message: None })`.
    - `panes(&self) -> Arc<PaneMap>`.
  - `impl Adapter for HerdrAdapter`:
    - `name()` is `"herdr"`, and `sources()` is `&[Source::Herdr]`.
    - `answer_paths()` is `&[]` in this task. Task 9 changes it to
      `&[AnswerPath::Keys]`.
    - `send(agent, text)`:
      1. If `check_prompt_text` returns `Err`, give
         `NotDelivered(BadRequest)`.
      2. If the agent id is not in `PaneMap`, give
         `NotDelivered(AgentGone)`.
      3. Call `herdr.prompt(pane_id, text)` and map the result:

         | Result | Returns |
         |---|---|
         | `Ok` | `Ok(())` |
         | `Server{agent_blocked}` | `NotDelivered(AgentBusy)` |
         | `Server{agent_not_found}` | `NotDelivered(AgentGone)` |
         | any other `Server` | `NotDelivered(SourceUnavailable)` |
         | `Connect` or `Write` | `NotDelivered(SourceUnavailable)` |
         | `NoReply` | `Uncertain("herdr did not confirm the prompt")` |

      4. After `Ok` or `Uncertain`, send `Hint::Pane(pane_id)` with
         `try_send`, so the agent's new state shows up.
      5. Wrap the whole call in `tokio::time::timeout(10 s)`. Timing out
         after the write gives `Uncertain`.
    - `mark_seen` is the trait default, `Ok(())`. It makes **no herdr
      call**: spec §6.4 forbids `agent.focus`.
  - Config, `crates/acd/src/config.rs`:
    `pub herdr: HerdrSection` with
    `#[serde(default)] pub struct HerdrSection { pub enabled: bool /* default true */, pub extra_sockets: Vec<String> }`.
    A leading `~/` expands to `$HOME`.

- [ ] **Step 1: Write the failing tests**

`crates/acd-adapters/tests/herdr_adapter.rs`:

```rust
use std::{sync::Arc, time::Duration};
use acd_adapters::herdr::{fake::{FakeHerdr, fake_agent}, reconciler::Timing, HerdrAdapter, HerdrConfig};
use acd_core::{testing::test_machine, Adapter, Core, DeliverError};
use acd_protocol::*;

async fn until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    for _ in 0..200 { if let Some(v) = f() { return v; } tokio::time::sleep(Duration::from_millis(20)).await; }
    panic!("timed out waiting for {what}");
}

struct Rig { fake: FakeHerdr, core: Arc<Core>, adapter: Arc<HerdrAdapter>, _home: tempfile::TempDir }

async fn rig(agents: &[(&str, &str, &str)]) -> Rig {
    let home = tempfile::tempdir().unwrap();
    let sock = home.path().join(".config/herdr/herdr.sock");
    std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
    let fake = FakeHerdr::start_at(&sock).await;
    for (p, a, s) in agents { fake.set_agent(fake_agent(p, a, s)); }
    let timing = Timing { list_idle: Duration::from_millis(300), list_blocked: Duration::from_millis(100),
        backoff_max: Duration::from_millis(200), fallback_after: Duration::from_millis(200), poll: Duration::from_millis(50) };
    let cfg = HerdrConfig { enabled: true, home: home.path().into(), env_socket: None, extra_sockets: vec![], scan_interval: Duration::from_millis(100), timing };
    let adapter = HerdrAdapter::new(cfg);
    let core = Core::new(test_machine(), vec![adapter.clone()]);
    adapter.start(core.clone());
    until("agents", || (core.attach(None).0.len() == 1 && adapter.panes().read().unwrap().len() == agents.len()).then_some(())).await;
    Rig { fake, core, adapter, _home: home }
}

fn agent(r: &Rig) -> Agent { r.core.agent(r.adapter.panes().read().unwrap().keys().next().unwrap()).unwrap() }

#[tokio::test]
async fn follow_up_reaches_an_idle_agent() {
    let r = rig(&[("w1:p1", "claude", "idle")]).await;
    r.adapter.send(&agent(&r), "run the tests").await.unwrap();
    assert_eq!(r.fake.prompts(), vec![("w1:p1".to_string(), "run the tests".to_string())]);
    until("working", || (agent(&r).state == AgentState::Working).then_some(())).await;
}

#[tokio::test]
async fn follow_up_errors_map_to_not_delivered() {
    let r = rig(&[("w1:p1", "claude", "blocked")]).await;
    let a = agent(&r);
    assert!(matches!(r.adapter.send(&a, "hi").await, Err(DeliverError::NotDelivered(ErrorCode::AgentBusy))));
    assert!(matches!(r.adapter.send(&a, "a\u{202E}b").await, Err(DeliverError::NotDelivered(ErrorCode::BadRequest))));
    assert!(matches!(r.adapter.send(&a, &"x".repeat(16 * 1024 + 1)).await, Err(DeliverError::NotDelivered(ErrorCode::BadRequest))));
    let mut gone = a.clone(); gone.id = "herdr:000000000000:w9:p9".into();
    assert!(matches!(r.adapter.send(&gone, "hi").await, Err(DeliverError::NotDelivered(ErrorCode::AgentGone))));
    assert!(r.fake.prompts().is_empty());
}

#[tokio::test]
async fn prompt_reply_lost_after_write_is_uncertain() {
    let r = rig(&[("w1:p1", "claude", "idle")]).await;
    r.fake.hang_next("agent.prompt");
    let res = tokio::time::timeout(Duration::from_secs(11), r.adapter.send(&agent(&r), "hi")).await.unwrap();
    assert!(matches!(res, Err(DeliverError::Uncertain(_))), "{res:?}");
    assert_eq!(r.fake.prompts().len(), 1);
}

#[tokio::test]
async fn mark_seen_never_touches_herdr() {
    let r = rig(&[("w1:p1", "claude", "done")]).await;
    let before = r.fake.calls().len();
    r.core.handle("c", ClientMessage::AgentMarkSeen { id: "m1".into(), agent_id: agent(&r).id }).await;
    assert_eq!(agent(&r).state, AgentState::Idle);
    assert!(r.fake.calls()[before..].iter().all(|(m, _)| m != "agent.focus"));
}

#[tokio::test]
async fn disabled_config_reports_disabled_and_starts_nothing() {
    let home = tempfile::tempdir().unwrap();
    let cfg = HerdrConfig { enabled: false, ..HerdrConfig::from_env(false, vec![]) };
    let cfg = HerdrConfig { home: home.path().into(), ..cfg };
    let adapter = HerdrAdapter::new(cfg);
    let core = Core::new(test_machine(), vec![adapter.clone()]);
    adapter.start(core.clone());
    let ServerMessage::Welcome { machine, .. } = core.welcome() else { panic!() };
    assert_eq!(machine.sources.herdr.state, SourceState::Disabled);
}
```

The `rig` helper's first condition (`attach(None).0.len() == 1`) only
checks that a snapshot exists. The real wait is on `panes()`. Simplify it
if you like, but keep the wait.

Config unit test in `crates/acd/src/config.rs`:

```rust
#[test]
fn herdr_section_defaults_and_parses() {
    let c: Config = toml::from_str("").unwrap();
    assert!(c.herdr.enabled);
    assert!(c.herdr.extra_sockets.is_empty());
    let c: Config = toml::from_str("[herdr]\nenabled = false\nextra_sockets = [\"~/x.sock\"]\n").unwrap();
    assert!(!c.herdr.enabled);
    assert_eq!(c.herdr.extra_sockets, vec!["~/x.sock"]);
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p acd-adapters --test herdr_adapter && cargo test -p acd config::`
Expected: compile errors.

- [ ] **Step 3: Implement**

Write `HerdrAdapter` as described under **Produces**. In `daemon.rs`,
build the adapter before the core and start it right after:

```rust
    let extra = cfg.herdr.extra_sockets.iter().map(|s| expand_home(s)).collect();
    let herdr = acd_adapters::herdr::HerdrAdapter::new(acd_adapters::herdr::HerdrConfig::from_env(cfg.herdr.enabled, extra));
    let core = Core::new(machine, vec![herdr.clone()]);
    herdr.start(core.clone());
```

`expand_home` lives in `config.rs`. It replaces a leading `~/` with
`$HOME/` and returns the path unchanged otherwise.

Update the existing config test
`partial_file_fills_defaults_and_ignores_unknown_keys`: it still parses
`[herdr] enabled = true`, and that now lands in `c.herdr`.

- [ ] **Step 4: Run the tests**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Smoke test against real herdr** (manual, read-only)

- Start an isolated session:
  `tmux new-session -d -s acp2b 'herdr --session acp2b'`.
- In it, start a plain shell and one `claude` in `/tmp/acp2b/proj`. Run
  `git init` there first, and decline any trust prompt with `esc`.
- Run `ACD_CONFIG_DIR=$(mktemp -d) cargo run -p acd -- init` then `run`
  with `RUST_LOG=acd_adapters=debug`.
- Check that `acd status` shows herdr `ok`, and that a WebSocket client
  (the `acd status` output, or a short `websocat` session if installed)
  lists the Claude agent and not the shell.
- Clean up:
  - `herdr --session acp2b server stop`;
  - `tmux kill-session -t acp2b`;
  - `rm -rf ~/.config/herdr/sessions/acp2b`.

Record what you saw in the commit message body. If no herdr is available,
say so instead.

- [ ] **Step 6: Commit**

```bash
git add crates/acd crates/acd-adapters
git commit -m "acd: run the herdr adapter; herdr follow-ups through agent.prompt"
```

---

### Task 9: Keys delivery engine

This is the shared contract with P3. P3 creates `keys` requests, which are
Claude and Codex requests with a captured fingerprint. It registers a
`KeysTarget` for each and provides `Evidence`. P2 builds and tests the
engine with test doubles.

**Files:**
- Create: `crates/acd-adapters/src/herdr/keys.rs`
- Modify: `crates/acd-adapters/src/herdr/mod.rs` (`answer_paths`, `deliver`, `keys_targets()`, `set_evidence()`)
- Test: add to `crates/acd-adapters/tests/herdr_adapter.rs`

**Interfaces:**
- Consumes:
  - `screen::{Fingerprint, fallback_fingerprint, prompt_line}` (Task 4);
  - `PaneMap` and `Hint` (Task 5);
  - `HerdrAdapter` (Task 8).
- Produces (P3 depends on these exact names):

```rust
/// One keys-answerable dialog on one agent. Registered by whoever opened
/// the request (P3); one per agent, because a request has one answer path.
#[derive(Clone, Debug)]
pub struct KeysTarget {
    pub key: CorrelationKey,                 // the request's correlation key
    pub fingerprint: Fingerprint,            // captured from the detection snapshot
    pub option_keys: HashMap<String, String>, // option id -> herdr key name ("1".."9", "esc")
}

#[derive(Default)]
pub struct KeysTargets(Mutex<HashMap<String, KeysTarget>>); // agent_id -> target
impl KeysTargets {
    pub fn set(&self, agent_id: &str, t: KeysTarget);
    pub fn remove(&self, agent_id: &str);
    pub fn get(&self, agent_id: &str) -> Option<KeysTarget>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict { Consistent, Contradicts, NoEvidence }

/// Correlated outcome evidence (spec §5.1, §6.2 Attribution, §6.3 Completion).
/// Implementations may wait (P3 waits up to 30 s for PostToolUse/Stop).
#[async_trait]
pub trait Evidence: Send + Sync {
    async fn outcome(&self, agent_id: &str, key: &CorrelationKey, option_id: &str) -> Verdict;
}

/// P2's default: no evidence source exists yet, so nothing is ever `inbox`.
pub struct NoEvidence;
```

- `HerdrAdapter::keys_targets(&self) -> Arc<KeysTargets>`
- `HerdrAdapter::set_evidence(&self, e: Arc<dyn Evidence>)`: replaces `NoEvidence`.
- `KeysTiming { poll: Duration /*150 ms*/, window: Duration /*3 s*/, prompt_checks: [Duration; 3] /*300 ms, 1 s, 3 s*/ }` lives on `HerdrConfig` as `keys: KeysTiming`, with `Default`.

**`deliver(request, answer)`** runs inline. Nothing may be sent from a
spawned task, so a dropped future can never complete a send later.

1. An `Answer::Text` gives `NotDelivered(NotSupported)`. Free text on
   `keys` is deferred (§11 item 1).
2. Look up the target with `targets.get(request.agent_id)`.
   - No target, or `target.key` not registered, gives
     `NotDelivered(RequestNotReady)`.
   - Look up `option_keys[option_id]`. A missing entry gives
     `NotDelivered(BadRequest)`.
3. Look up the pane with `PaneMap.get(agent_id)`. A missing pane gives
   `NotDelivered(AgentGone)`.
4. **Guard.**
   - Read `text = herdr.read(pane, "detection", None)`. An error gives
     `NotDelivered(SourceUnavailable)`.
   - `fallback_fingerprint(&text) != target.fingerprint` gives
     `NotDelivered(ScreenMismatch)`.
   - Record `baseline = screen::prompt_line(kind, &text)`.
5. Call `herdr.send_keys(pane, &[key_name])`. This is the only
   `send_keys` call site in the crate.

   | Result | Returns |
   |---|---|
   | `Connect`, `Write` or `Server` | `NotDelivered(SourceUnavailable)`, nothing sent |
   | `NoReply` | `Uncertain`, no confirmation task (the core resolves `uncertain` itself) |

6. Spawn the confirmation task and return `Ok(())`.

**Confirmation task** (`t0` = the moment the key was sent):

1. **Poll.**
   - Every `poll`, until `window`, read `detection`.
   - The dialog is **closed** after two consecutive reads whose
     `fallback_fingerprint` differs from the target's.
   - A read error counts as neither present nor absent.
2. **Stray check.**
   - This only applies when the key is a single digit. `esc` types nothing
     (§6.2).
   - At each `prompt_checks` offset, read `visible` and compute
     `p = prompt_line(kind, text)`.
   - It is a **stray** if `p` ends with the digit and `p != baseline`.
3. **Result.**
   - Remove the target from `KeysTargets`.
   - Pick `by`:
     - a stray gives `Uncertain`;
     - otherwise, a dialog that is not closed gives `Uncertain`;
     - otherwise ask `evidence.outcome(...)`: `Consistent` gives `Inbox`,
       `Contradicts` gives `Terminal`, and `NoEvidence` gives `Uncertain`.
   - Emit `Observation::RequestEnded { agent_id, key, by }`, and, if
     closed, `Observation::DialogGone`.
   - Then send `Hint::Pane(pane)` so the reconciler emits the agent's new
     state (adapter contract: an upsert after a resolution).
   - **Never** send another key.
4. **Lifetime.** Hold the core through the `Weak<Core>` from Task 8. If it
   is gone, exit quietly.

- [ ] **Step 1: Write the failing tests**

Add to `tests/herdr_adapter.rs`.

- Each test opens a `keys` request on the rig's agent through
  `core.observe(RequestSeen(...))`, using
  `acd_core::testing::permission_obs` with `answer_path: Keys`,
  `ready: true` and `key: "k1"`.
- It registers a `KeysTarget` whose `fingerprint` is
  `fallback_fingerprint` of the screen it set.
- It answers through `core.handle(RequestAnswer)`, so the core's
  claim/delivered/uncertain path is exercised too.

```rust
use acd_adapters::herdr::{keys::*, screen::fallback_fingerprint};
use acd_core::{testing::permission_obs, CorrelationKey, Observation};

fn screen(name: &str) -> String {
    std::fs::read_to_string(format!("{}/../../tests/fixtures/herdr/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

struct Fixed(Verdict);
#[async_trait::async_trait]
impl Evidence for Fixed {
    async fn outcome(&self, _: &str, _: &CorrelationKey, _: &str) -> Verdict { self.0 }
}

/// Opens a keys request whose dialog is `dialog`; when "1" (or esc) arrives the
/// screens switch to `after_detection` / `after_visible`.
async fn keys_rig(after_detection: &'static str, after_visible: &'static str) -> (Rig, String) {
    let r = rig(&[("w1:p1", "claude", "blocked")]).await;
    let dialog = screen("claude-permission-bash.txt");
    r.fake.set_screen("w1:p1", "detection", &dialog);
    r.fake.set_screen("w1:p1", "visible", &dialog);
    r.fake.on_keys(move |f, pane, _keys| {
        f.set_screen(pane, "detection", &screen(after_detection));
        f.set_screen(pane, "visible", &screen(after_visible));
        f.set_status(pane, "working");
    });
    let id = agent(&r).id;
    let mut obs = permission_obs(&id, "k1");
    obs.answer_path = AnswerPath::Keys;
    r.core.observe(Observation::RequestSeen(obs));
    r.adapter.keys_targets().set(&id, KeysTarget {
        key: CorrelationKey("k1".into()),
        fingerprint: fallback_fingerprint(&dialog),
        option_keys: [("allow".to_string(), "1".to_string()), ("deny".to_string(), "esc".to_string())].into(),
    });
    let rid = r.core.agent(&id).unwrap().pending.unwrap().id;
    (r, rid)
}

async fn answer(r: &Rig, rid: &str, option: &str) -> CommandResult {
    r.core.handle("c", ClientMessage::RequestAnswer { id: format!("a-{option}"), request_id: rid.into(), option_id: Some(option.into()), text: None }).await
}

/// The first `request.resolved` on `rx` (attach before answering).
async fn resolution(rx: &mut tokio::sync::broadcast::Receiver<ServerMessage>) -> (ResolvedBy, Option<String>) {
    tokio::time::timeout(Duration::from_secs(6), async {
        loop {
            match rx.recv().await {
                Ok(ServerMessage::RequestResolved { by, option_id, .. }) => return (by, option_id),
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(e) => panic!("event stream ended: {e}"),
            }
        }
    }).await.expect("no request.resolved within 6 s")
}

fn delivered(res: &CommandResult) -> bool { res.ok && res.outcome == Some(Outcome::Delivered) }
```

The tests:

```rust
#[tokio::test]
async fn closed_dialog_without_evidence_is_uncertain() {
    let (r, rid) = keys_rig("claude-idle-detection.txt", "claude-idle-prompt.txt").await;
    let (_, mut rx) = r.core.attach(None);
    assert!(delivered(&answer(&r, &rid, "allow").await));
    assert_eq!(resolution(&mut rx).await, (ResolvedBy::Uncertain, None));
    assert_eq!(r.fake.keys_sent(), vec![("w1:p1".to_string(), vec!["1".to_string()])]);
}

#[tokio::test]
async fn consistent_evidence_is_inbox_and_contradicting_is_terminal() {
    let (r, rid) = keys_rig("claude-idle-detection.txt", "claude-idle-prompt.txt").await;
    r.adapter.set_evidence(Arc::new(Fixed(Verdict::Consistent)));
    let (_, mut rx) = r.core.attach(None);
    answer(&r, &rid, "allow").await;
    assert_eq!(resolution(&mut rx).await, (ResolvedBy::Inbox, Some("allow".into())));

    let (r, rid) = keys_rig("claude-idle-detection.txt", "claude-idle-prompt.txt").await;
    r.adapter.set_evidence(Arc::new(Fixed(Verdict::Contradicts)));
    let (_, mut rx) = r.core.attach(None);
    answer(&r, &rid, "allow").await;
    assert_eq!(resolution(&mut rx).await, (ResolvedBy::Terminal, None));
}

#[tokio::test]
async fn mismatch_sends_nothing() {
    let (r, rid) = keys_rig("claude-idle-detection.txt", "claude-idle-prompt.txt").await;
    let changed = screen("claude-permission-bash.txt").replace("touch /tmp/acp0/t1.txt", "rm -rf /tmp/acp0");
    r.fake.set_screen("w1:p1", "detection", &changed);
    let res = answer(&r, &rid, "allow").await;
    assert_eq!(res.error_code(), Some(ErrorCode::ScreenMismatch));
    assert!(r.fake.keys_sent().is_empty());
    assert_eq!(agent(&r).pending.unwrap().status, RequestStatus::Open, "nothing reached the agent: back to open");
}

#[tokio::test]
async fn stray_digit_in_prompt_is_uncertain() {
    let (r, rid) = keys_rig("claude-idle-detection-stray-1.txt", "claude-idle-prompt-stray-1.txt").await;
    r.adapter.set_evidence(Arc::new(Fixed(Verdict::Consistent)));
    let (_, mut rx) = r.core.attach(None);
    answer(&r, &rid, "allow").await;
    assert_eq!(resolution(&mut rx).await.0, ResolvedBy::Uncertain, "a stray key beats consistent evidence");
    assert_eq!(r.fake.keys_sent().len(), 1, "never a corrective key");
}

#[tokio::test]
async fn esc_gets_no_prompt_check() {
    let (r, rid) = keys_rig("claude-idle-detection-stray-1.txt", "claude-idle-prompt-stray-1.txt").await;
    r.adapter.set_evidence(Arc::new(Fixed(Verdict::Consistent)));
    let (_, mut rx) = r.core.attach(None);
    answer(&r, &rid, "deny").await;
    assert_eq!(resolution(&mut rx).await, (ResolvedBy::Inbox, Some("deny".into())));
    assert_eq!(r.fake.keys_sent(), vec![("w1:p1".to_string(), vec!["esc".to_string()])]);
}

#[tokio::test]
async fn dialog_that_never_closes_is_uncertain() {
    let (r, rid) = keys_rig("claude-permission-bash.txt", "claude-permission-bash.txt").await;
    r.adapter.set_evidence(Arc::new(Fixed(Verdict::Consistent)));
    let (_, mut rx) = r.core.attach(None);
    answer(&r, &rid, "allow").await;
    assert_eq!(resolution(&mut rx).await.0, ResolvedBy::Uncertain);
}

#[tokio::test]
async fn no_target_is_not_ready_and_text_is_not_supported() {
    let (r, rid) = keys_rig("claude-idle-detection.txt", "claude-idle-prompt.txt").await;
    r.adapter.keys_targets().remove(&agent(&r).id);
    assert_eq!(answer(&r, &rid, "allow").await.error_code(), Some(ErrorCode::RequestNotReady));
    let res = r.core.handle("c", ClientMessage::RequestAnswer { id: "t1".into(), request_id: rid.clone(), option_id: None, text: Some("hi".into()) }).await;
    assert!(res.error_code().is_some());
    assert!(r.fake.keys_sent().is_empty());
}

#[tokio::test]
async fn send_keys_reply_lost_is_uncertain_and_sends_once() {
    let (r, rid) = keys_rig("claude-idle-detection.txt", "claude-idle-prompt.txt").await;
    r.fake.hang_next("agent.send_keys");
    let (_, mut rx) = r.core.attach(None);
    assert!(delivered(&answer(&r, &rid, "allow").await));
    assert_eq!(resolution(&mut rx).await.0, ResolvedBy::Uncertain);
    assert_eq!(answer(&r, &rid, "allow").await.error_code(), Some(ErrorCode::RequestGone));
    assert_eq!(r.fake.keys_sent().len(), 1);
}
```

`CommandResult` has public `ok` and `outcome` fields and an `error_code()`
method (`crates/acd-protocol/src/messages.rs`). If a name differs, adapt
the helper without changing what the assertions mean.

The answer-rejected-for-text case may come back as `not_supported` from
the adapter, or as `bad_request` from the core if it rejects text for
permission requests first. Either is acceptable, as long as no key is sent.

The tests use the default `KeysTiming`, which keeps the `window` at 3 s.
They therefore take about 3 s each, which is acceptable.

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p acd-adapters --test herdr_adapter`
Expected: compile errors.

- [ ] **Step 3: Implement** `keys.rs` and the adapter changes as described
under **Produces**. Then check the single call site:
`grep -rn "send_keys" crates/acd-adapters/src` must show only `client.rs`
(the definition) and `keys.rs`.

- [ ] **Step 4: Run the tests**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`.
Then run `herdr_adapter` 5 times. Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/acd-adapters
git commit -m "acd-adapters: fingerprint-guarded keys delivery engine with outcome evidence hook"
```

---

### Task 10: End-to-end over acd/1, invariants and docs

**Files:**
- Create: `crates/acd-adapters/tests/herdr_e2e.rs`
- Modify: `crates/acd-adapters/Cargo.toml` (dev-deps `axum`, `tokio-tungstenite`, `futures-util`)
- Modify: `docs/superpowers/plans/2026-10-07-p1-followups.md` (mark the P2 items done; add anything new for P3 and P5)
- Modify: `crates/acd-protocol/PROTOCOL.md` only if a wire behaviour changed. None is expected: P2 adds no message types.

**Interfaces:**
- Consumes: everything above, plus `acd_core::{router, Auth, ServerOptions}`.

- [ ] **Step 1: Write the tests**

Start a real acd/1 server, as `crates/acd-core/tests/server.rs` does: copy
its `connect`, `send`, `next`, `hello` and `authed` helpers. Wire it to a
`Core` with a `HerdrAdapter` over a `FakeHerdr`. Then add these tests.

1. **`agent_appears_over_the_protocol`**
   - Start with a claude pane `idle` and a shell pane.
   - The snapshot holds exactly one agent, with `source: "herdr"`,
     `kind: "claude"` and `can_send: true`.
   - `set_status(blocked)` produces an `agent.updated` event with
     `state: "needs_you"` within 2 s.
2. **`follow_up_retry_sends_once`** (acceptance §14.5)
   - Send `{"type":"agent.send","id":"f1",...}`.
   - Drop the WebSocket before reading the result, reconnect with
     `resume`, and send the identical `f1` again.
   - Both results are `delivered` (the second from the idempotency cache),
     and `fake.prompts().len() == 1`.
3. **`blocked_pane_is_read_only_over_the_protocol`** (acceptance §14.10)
   - Use an `aider` pane, `blocked`, whose detection screen is the trust
     dialog.
   - Within 2 s, `agent.updated` carries a `pending` request with
     `kind: "blocked_unparsed"`, `answer_path: "none"` and a `raw_screen`
     of at most 40 lines.
   - `request.answer` gives `not_answerable`.
4. **`invariants_hold_on_the_wire`**
   - Runs last in the same test after 1–3, or as a check at the end of
     each.
   - `fake.calls()` contains no `agent.focus`, `pane.send_keys`,
     `pane.send_text` or `pane.send_input`, and no `agent.send_keys`
     (these tests never answer a keys request).

- [ ] **Step 2: Run them**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`.
Expected: PASS. If a test fails, fix the code, not the test, unless the
test contradicts the spec. In that case, stop and report.

- [ ] **Step 3: Update the follow-ups file**

In `2026-10-07-p1-followups.md`, under "P2", mark each item done with the
task that closed it:

- Cf/bidi and 16 KiB: Task 3 and Task 8.
- §5 caps: Task 3 and Task 6. `last_message` has no herdr source in P2.
  The helper exists for P3 `Stop`.
- Adapter contract: Tasks 8 and 9.
- Resolution upsert: Task 9 `Hint::Pane`.
- Adapter check before claim, and dead code: Task 1.
- Registry tests: Task 1.

Add these new items under "P3":

- Implement `Evidence`: correlation with PostToolUse, Stop and
  UserPromptSubmit.
- Register a `KeysTarget` for every keys-answerable request, and remove it
  when the request ends without a delivery.
- Bind Codex sessions with `agent_session.value` (now present in
  `AgentInfo`).
- Upgrade `blocked_unparsed` in place (`upgrade_from` = the fallback key).

- [ ] **Step 4: Commit, push, PR, CI**

```bash
git add crates/acd-adapters docs/superpowers/plans/2026-10-07-p1-followups.md
git commit -m "P2: herdr end-to-end tests over acd/1; follow-ups updated"
git push -u origin p2-herdr
gh workflow run CI --ref p2-herdr
```

Then open the PR against `main`, and confirm that CI passes on both
operating systems for the exact head commit.
