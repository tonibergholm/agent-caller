# Agent inbox v1: design spec

Date: 2026-10-07
Status: revision 2. Sol review round 1 is addressed (see §15). The spec is
awaiting re-review and owner approval.
Research: `docs/research/2026-10-07-landscape.md`, which includes the spike
log.

## 1. Purpose

agent-caller is an open-source alternative to One (getone.one). It gives one
place to see every coding agent you run, answer the ones waiting on you, and
send follow-ups, across local and remote machines.

It is built for two audiences, in this order:

1. **The owner's own workflow.** Machines are connected over Tailscale.
   Agents run inside herdr and inside T3 Code.
2. **A reusable core.** The per-machine daemon `acd` and its documented
   protocol `acd/1` should let other user interfaces (a CLI, a phone app,
   third-party apps) be built on top. The Mac app is the reference client.

## 2. Scope

### In v1

- A per-machine daemon, `acd`, in Rust. It merges agent state from three
  sources: Claude Code and Codex hooks, herdr, and T3 Code.
- A WebSocket protocol, `acd/1`, defined in this spec and published as a
  document in the repo.
- A Mac menu bar app built with Tauri. It has a status icon, a floating
  panel opened by a global hotkey, and keyboard control.
- Answering:
  - permission prompts
  - single-question, single-select questions

  Both come with a 2-second undo. Other question shapes are shown but are
  read-only.
- Free-text follow-ups to idle or done agents.
- Agent kinds:
  - Claude Code and Codex are fully supported.
  - Any other agent that herdr detects is **visible**, with its state and a
    raw-screen preview when it is blocked. It is **read-only**.
- Remote machines are reached over Tailscale. The daemon listens only on
  loopback and Tailscale addresses.

### Not in v1 (planned later; the protocol leaves room through `capabilities`)

- Voice router (planned reuse of Handy's crates: `transcribe-rs`, `vad-rs`,
  `handy-keys`)
- Phone client and push notifications
- A self-hosted end-to-end encrypted relay
- Starting new agents
- Transcript and history browsing
- Writable raw-screen input
- Multi-select and multi-question answers
- Agent kinds other than Claude Code and Codex with full support
- Windows and Linux desktop clients

## 3. Glossary

| Term | Meaning |
|---|---|
| **acd** | The agent-caller daemon. One runs per machine. |
| **Source** | Where acd learns about agents: `hooks`, `herdr` or `t3`. |
| **Agent** | One live agent session: a herdr pane that hosts an agent, a Claude session outside herdr, or a T3 thread. |
| **Request** | Something an agent is waiting on the user for: a permission or a question. |
| **Answer path** | How acd delivers an answer: `hook` (the reply to a held hook), `keys` (keystrokes through herdr), `t3` (a T3 command), or `none` (read-only). |
| **Dialog fingerprint** | A normalized hash of the dialog block on screen: the title line, the detail text (for example the command), and the option labels. It ties a keystroke to the dialog that was observed. |
| **Client** | Anything connected to acd over `acd/1`. |
| **Hook shim** | `acd hook …`. Hook configs call it, and it forwards the hook to acd. |

## 4. Architecture

```
 Mac: Tauri menu bar app (status icon, floating panel, hotkey)
        │  acd/1 over WebSocket, one connection per machine, via Tailscale
        ▼
 acd (Rust daemon, one per machine; the laptop runs one too)
   ├─ hook endpoint (HTTP, 127.0.0.1)  ◄── `acd hook` shim ◄── Claude/Codex hooks
   ├─ herdr adapter  ◄─► herdr Unix socket(s): state, events, screen, keys, prompt
   ├─ t3 adapter     ◄─► T3 server WebSocket RPC: threads, requests, messages
   └─ core: agent registry, request lifecycle (§5.1), acd/1 server
```

### Invariants

1. **Fail open.** acd never causes an agent to hang.
   - When acd is unreachable, slow or undecided, the agent's own terminal
     dialog takes over.
   - A hook is held only where the spike showed the terminal dialog stays
     usable while it waits. Today that is only Claude's
     `PermissionRequest`.
2. **First answer wins.** How strong this guarantee is depends on the
   answer path:
   - **`hook` and `t3`: atomic at the source.** The agent or T3 accepts
     exactly one answer. A late answer is rejected (`request_gone`).
   - **`keys`: best effort, with a guard.**
     - herdr has no conditional input, so a terminal answer can land
       between acd's screen check and its keystroke.
     - acd keeps that window small: it re-reads the screen and compares the
       fingerprint immediately before sending one key.
     - If the outcome cannot be confirmed, acd reports it as uncertain.
     - acd never retries keystrokes automatically.
     - The residual risk is accepted in v1. Upstream herdr support for
       conditional input is tracked in §11.
3. **Bound input.** acd sends keystrokes or text to a pane only in two
   cases:
   - a single option key for a request whose dialog fingerprint matches the
     screen at the moment of sending;
   - a follow-up through herdr `agent.prompt`, which herdr refuses when the
     agent is blocked.

   There is no free-form keystroke or raw-text input in v1.
4. **One owner per agent.** Each agent has exactly one owning adapter.
   Every observation of the agent is reconciled to that owner by
   correlation keys (§5.1).

## 5. Data model

All types live in the `acd-protocol` crate. The Tauri app gets TypeScript
bindings for them, generated with `ts-rs` or `specta`. The wire format is
defined in §7.

```text
Machine {
  id: string               // stable: the hostname plus a random suffix, created by `acd init`
  name: string
  acd_version: string
  sources: { hooks: SourceStatus, herdr: SourceStatus, t3: SourceStatus }
}
SourceStatus { state: "ok"|"degraded"|"disabled"|"unconfigured", message?: string }

Agent {
  id: string               // "herdr:<socket_hash>:<pane_id>" | "t3:<thread_id>" | "claude:<session_id>"
  machine_id: string
  source: "herdr" | "t3" | "hooks"
  kind: "claude" | "codex" | "other"
  title: string
  cwd: string | null
  project: string | null
  state: "working" | "needs_you" | "done" | "idle" | "unknown"
  state_since: timestamp
  failed: bool
  last_message: string | null        // at most 500 characters
  pending: Request | null
  can_send: bool                     // whether a follow-up through agent.send is possible
}

Request {
  id: string                         // generated by acd, unique within the daemon epoch
  agent_id: string
  kind: "permission" | "question" | "blocked_unparsed"
  status: "open" | "answering" | "resolved"
  answer_path: "hook" | "keys" | "t3" | "none"
  title: string
  detail: string
  options: RequestOption[]           // permission options; for a question, the options of questions[0]
  questions: Question[] | null       // kind=question: the full original structure
  free_text: bool                    // whether a typed answer is accepted (questions only)
  raw_screen: string | null          // blocked_unparsed only; at most 40 lines
  opened_at: timestamp
}
RequestOption { id: string, label: string, description: string | null,
                effect: "allow" | "allow_always" | "deny" | "choose" }
Question { id: string, header: string | null, text: string, multi_select: bool,
           options: RequestOption[] }
```

A request is **answerable** when all three conditions hold:

- `answer_path` is not `none`;
- `kind` is not `blocked_unparsed`;
- if it is a question, there is exactly one question and it is
  single-select.

Anything else is shown read-only, with the label "answer in terminal".

### 5.1 Request lifecycle and identity

**Correlation keys.** acd uses these keys to deduplicate and to match
requests across sources:

| Origin | Key |
|---|---|
| Claude hook | `prompt_id`. It links `PermissionRequest` and `PreToolUse` for the same `AskUserQuestion`. |
| Codex hook | `(session_id, turn_id, sha256(tool_name + tool_input))` |
| herdr fallback | `(socket, pane_id, dialog fingerprint)` |
| T3 | the runtime request id |

Observing the same key again updates the existing request. It never opens
a new one.

**States.**

```
open ──claim──► answering ──confirmed──► resolved
  │                │  └─uncertain──► resolved (by: "uncertain")
  └──────── source evidence that the dialog ended ──────► resolved (by: terminal|agent|lost)
```

**Claim.**
- `request.answer` claims an `open` request atomically. A second claim
  gets `request_gone` if the request is already resolved, or
  `request_busy` while another client's claim is in flight.
- Every client sees `status: "answering"`.
- If delivery fails before anything reaches the agent, the request goes
  back to `open`. This covers a held hook that has already closed, or a
  fingerprint mismatch.

**Resolution needs source evidence.** "No longer blocked" alone is not
enough. The evidence per answer path:

- **`hook` (Claude).**
  - acd's reply was written and the connection completed normally. Then
    either a `PostToolUse` or `Stop` arrives for the session, or herdr
    shows that the request's fingerprint is gone. The result is
    `by: "inbox"`.
  - If the connection closes before acd replies, acd reconciles as
    described in §6.2.
- **`keys`.** After sending the key, acd polls the detection snapshot for
  up to 3 s.
  - If the fingerprint disappears in two consecutive reads, the result is
    `by: "inbox"`.
  - Otherwise the request resolves as `by: "uncertain"`. Clients show
    "check the terminal", and acd sends nothing further.
  - Codex's transient `working` state while hooks run is not evidence.
- **`t3`.** The T3 command is acknowledged and the thread's request
  disappears from its projection.
- **Terminal answer.**
  - For `keys`, `none` and herdr fallback requests: the fingerprint
    disappears from the screen without a claim from acd.
  - For `hook`: see §6.2.
- **`lost`.** The source can no longer observe the request. Examples: a
  hooks-only agent whose held connection dropped; an agent removed.

**Upgrade and supersede.**
- A herdr `blocked_unparsed` request is **upgraded in place** when a
  structured hook request arrives for the same pane within 5 s, or when a
  later fingerprint matches the hook's dialog. The request keeps its id;
  its kind, options and answer path change.
- A request is `superseded` only when the source shows a request with a
  **different** correlation key on the same agent. Replayed observations
  never supersede.
- **Exclusive answer paths.** A request has exactly one active answer path.
  A held Claude request switches to `keys` only after its hook connection
  has ended. acd never sends keys while it still holds that request's hook.

**Agent rules.**
- `done` means finished and not yet seen.
  - herdr's `done`/`idle` maps directly.
  - T3 and hooks-only agents change from `done` to `idle` when a client
    sends `agent.mark_seen`.
- An agent is removed when:
  - its pane closes;
  - its T3 thread is archived or deleted;
  - for a hooks-only agent, 10 minutes after `SessionEnd`, or after 24 hours
    with no event.

## 6. Sources and adapters

### 6.1 Hook shim, hook endpoint and installation

**The shim.** `acd hook <agent> <event>`:

1. Reads the stdin payload, up to 1 MiB. A larger payload makes the shim
   reply empty.
2. Adds these values from the environment: `HERDR_PANE_ID`,
   `HERDR_SOCKET_PATH`, `HERDR_ENV`, `CLAUDE_CODE_ENTRYPOINT`, and the
   parent PID.
3. POSTs to `http://127.0.0.1:<hook_port>/hook/<agent>/<event>` with
   `hook.token`.
4. Prints acd's response body and exits 0.

**Failure.** Any failure makes the shim print nothing and exit 0. That
covers connect refused, a timeout, or a malformed response.
- Connect timeout: 500 ms.
- Response timeout: per event (§6.2, §6.3).
- **acd always replies before the shim's timeout.** It times out its own
  holds first, so the shim timeout is only a backstop.

**Installation.** `acd hooks install claude|codex`:

- It is **idempotent.** It finds existing entries by the marker
  `acd-managed:v1` in the command string and replaces them in place, so
  running it twice never duplicates entries.
- Before any write, it backs up the file to `<file>.acd-backup-<timestamp>`.
- It writes atomically: a temporary file, then a rename. It keeps the
  file's mode.
- It records the entries it added in `~/.config/acd/hooks-installed.json`.
- It never edits or reorders entries it does not own, including those
  from herdr, Supacode and Warp.

`acd hooks uninstall` removes exactly the recorded entries. If the user
has edited an owned entry by hand, it refuses and lists the difference.

Codex shows a one-time hook review after install. The README documents
this.

**Coexisting permission hooks.** Other `PermissionRequest` hooks may decide
first. Claude and Codex combine hook decisions themselves (deny wins). acd
treats the effect like a terminal answer and resolves through
reconciliation. This behavior is verified in §11, item 6.

### 6.2 Claude adapter (hooks)

**Hooks installed:**

| Hook | Matcher | Shim response timeout |
|---|---|---|
| `SessionStart` | — | 2 s |
| `UserPromptSubmit` | — | 2 s (sets `working`) |
| `PermissionRequest` | — | `permission_hold` + 10 s |
| `PreToolUse` | `AskUserQuestion` | 2 s |
| `PostToolUse` | — | 2 s (resolution evidence) |
| `Stop` | — | 2 s |
| `SessionEnd` | — | 2 s |

- `permission_hold` defaults to 3600 s.
- The hook entry's `timeout` is set to the shim timeout plus 5 s.

**Filter.** These payloads get an immediate empty reply and are otherwise
ignored:

- `CLAUDE_CODE_ENTRYPOINT` is in the configurable ignore list (default
  `["sdk-ts"]`). T3-hosted sessions belong to the T3 adapter.
- `agent_id` is present, meaning the payload comes from a subagent.

**Binding.** The binding is reconciled on **every** hook, not only on
`SessionStart`:

- If `HERDR_PANE_ID` and `HERDR_SOCKET_PATH` are present, the session
  belongs to the herdr agent `herdr:<hash(socket)>:<pane>`.
- The herdr adapter also records `agent_session.value` (the Claude
  `session_id`) for each pane. Either signal is enough to bind.
- A hooks-only agent `claude:<session_id>` is created only when neither
  signal exists.
- When herdr later reports a pane with that session id, the hooks-only
  agent **migrates** into the herdr agent. Its open request moves with it,
  keeping the same request id, and `agent.removed` is emitted for the old
  id.
- Hooks-only agents have `can_send: false`.

**Permission requests** (`tool_name` ≠ `AskUserQuestion`):

1. Open a `permission` request:
   - `answer_path: "hook"`
   - `title`: `tool_name`
   - `detail`: `tool_input.command`, `tool_input.file_path`, or compact JSON
   - options: `allow` "Yes", and `deny` "No"
   - `allow_always` "Yes, always" only if §11, item 2 confirms a working
     field. Otherwise it is omitted for held requests.
2. Hold the HTTP response. acd watches the connection for close.
3. On a claim, reply with one of these and set the request to
   `answering`:
   - allow: `{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}`
   - deny: `{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"deny","message":"Denied from agent inbox"}}}`
     (deny is pending verification as a recorded fixture, §11 item 2)
4. Resolve as in §5.1.
5. **The connection closes before acd replies.** acd reconciles:
   - **Bound to herdr:** read the detection snapshot.
     - If the request's fingerprint is still present, the dialog is still
       open. Keep the request `open` and switch to `answer_path: "keys"`.
       This happens when the shim crashed or was killed by a timeout.
     - Otherwise resolve `by: "terminal"`. The spike showed this case
       arrives as a closed connection.
   - **Hooks-only:** acd cannot tell which case happened. Resolve
     `by: "lost"`, shown as "check the terminal".
6. **`permission_hold` expires.** acd replies empty. If the request is
   bound to herdr, it switches to `keys`; otherwise it resolves `lost`.
7. **Fingerprint capture.** When a request is bound to herdr, acd records
   the dialog fingerprint from the detection snapshot within 2 s of
   opening it. Matching uses the option labels **and** the detail text, so
   a different dialog with the same labels does not match.

**Questions** (`AskUserQuestion`):

1. Both `PreToolUse` and `PermissionRequest` reply empty at once, so the
   terminal dialog stays visible. They are joined by `prompt_id`.
2. A `question` request opens with:
   - `questions` = the full `tool_input.questions` (`header`, `question`,
     `options`, `multiSelect`)
   - `options` mirroring `questions[0]`
   - `free_text: true`
3. It is answerable only for a single question with `multiSelect: false`,
   and only when bound to herdr (`answer_path: "keys"`). Anything else is
   read-only.
4. **Answer with an option:** send the option's 1-based number key,
   guarded by the fingerprint.
5. **Answer with free text:** use the "Type something" row. The key
   sequence is unverified (§11, item 1). Until it is verified,
   `free_text` is `false`.

**Completion.**
- `Stop` sets `done`. `last_message` comes from `last_assistant_message`,
  or from the last assistant entry in `transcript_path`.
- `UserPromptSubmit` sets `working`. This keeps hooks-only agents current;
  herdr-bound agents take their state from herdr.

### 6.3 Codex adapter (hooks + herdr)

**Hooks installed:**

| Hook | Shim response timeout |
|---|---|
| `SessionStart` | 2 s |
| `UserPromptSubmit` | 2 s |
| `PermissionRequest` | 1 s |
| `PostToolUse` | 2 s |
| `Stop` | 2 s |

**`PermissionRequest` replies empty at once.** Codex hides its dialog
while the hook runs, so the request is opened instead:

- `answer_path: "keys"` when bound to herdr, `"none"` otherwise
- `detail`: `tool_input.command`, followed by `tool_input.description`
- options:
  - `allow` "Yes": key `1`
  - `allow_always` "Yes, don't ask again": key `2`
  - `deny` "No": key `esc`

**Answering.**
- acd waits up to 3 s for the dialog fingerprint to appear. The timing is
  verified in §11, item 5.
- Until the fingerprint appears, the request is visible but **not yet
  claimable**. A claim gets `request_not_ready`, and the app shows a
  spinner.

Resolution and binding follow the same rules as Claude: §5.1 and §6.2's
binding rule. The herdr pane owns the agent. Hook data adds detail, and
the hook's structured request upgrades herdr's fallback.

### 6.4 herdr adapter

**Discovery.** The adapter scans for sockets at startup and every 30 s:

- the default socket, from `$HERDR_SOCKET_PATH` or
  `~/.config/herdr/herdr.sock`;
- `~/.config/herdr/sessions/*/herdr.sock`;
- extra paths from the config.

Agent ids include a hash of the socket path, so panes in different sessions
never collide.

**Compatibility.**
- `ping` checks the server.
- Supported: herdr `>= 0.9.3, < 0.10` and protocol `22`. This is a tested
  range, widened only after a contract-test run. Anything outside it puts
  the session in `degraded` with a reason.

**Sync.** The subscription opens first, so no change is missed between the
subscription and the snapshot:

1. Subscribe with `events.subscribe` to `pane_agent_status_changed`,
   `pane_agent_detected` and `pane_closed`, and buffer the events.
2. Call `agent.list`.
3. Apply the buffered events whose `state_change_seq` is newer than the
   list entry.
4. Resync with steps 1–3 on `events_lost`, on reconnect, or on a gap in
   `state_change_seq`.

Exact sequence semantics are verified in §11, item 7.

**State mapping.** `blocked` becomes `needs_you`. The other states map
directly. Panes without an agent are ignored.

**Fallback.** A pane that stays `blocked` for 2 s with no structured
request opens a read-only `blocked_unparsed` request:

- `answer_path: "none"`
- `raw_screen` = the detection snapshot, at most 40 lines
- the request is keyed by its fingerprint

**Follow-ups.**
- `agent.send` calls herdr `agent.prompt` with the text as one paste.
- Text containing control characters other than `\n` and `\t` is
  rejected with `bad_request`.
- herdr's `agent_blocked` is returned as `agent_busy`.

**Seen state.** acd uses herdr's own mark-seen if one exists (§11, item 4).
Otherwise acd keeps its own flag.

**Known gap.** herdr reports Codex's trust and hook-review screens as
`idle`. These appear only at startup, and v1 accepts the gap.

### 6.5 T3 adapter

**Connection.**
- WebSocket RPC to the local T3 server, at the URL in the config.
- Bearer token from
  `t3 auth session issue --scope orchestration:read --scope orchestration:operate`.

**Contract.**
- Hand-written Rust types for `subscribeShell`, `subscribeThread`, and
  `dispatchCommand` (`message.dispatch`, `runtime-request.respond`).
- The T3 version is pinned. Unknown fields are ignored. Unknown statuses
  and request kinds become `unknown`, or a read-only request.

**State mapping.**

| T3 status | acd state |
|---|---|
| `running`, `starting`, `preparing`, `queued` | `working` |
| `waiting` | `needs_you` |
| `completed` | `done` |
| `failed`, `interrupted` | `done`, with `failed: true` |
| `idle`, `cancelled`, `rolled_back` | `idle` |

**Requests.**
- acd subscribes to each waiting thread to read its pending runtime
  requests.
- Approvals become `permission` requests with `answer_path: "t3"`. Their
  options come from the request's decision values.
- User-input questions become `question` requests that keep T3's full
  structure in `questions`. They are answerable under the same shape rule
  as Claude questions.

**Answers.**
- `runtime-request.respond`, with `decision` for an approval or `answers`
  keyed by question id.
- A stale answer must be rejected by T3. That behavior is verified in
  §11, item 3.

**Follow-ups.** `message.dispatch`.

## 7. Protocol acd/1

### 7.1 Transport and security

**Binding.**
- WebSocket on port 47800 by default.
- acd binds only to loopback and to addresses inside Tailscale's ranges:
  `100.64.0.0/10` and `fd7a:115c:a1e0::/48`. Addresses come from the local
  interfaces. A configured address outside this allowlist is refused at
  startup.
- When the Mac app connects to a MagicDNS name, it checks that the name
  resolved to an address in these ranges before sending `hello`.

**Limits.**
- `hello` must arrive within 5 s.
- Frames are limited to 1 MiB.
- At most 8 unauthenticated connections at once.
- A request carrying an `Origin` header is rejected with HTTP 403 unless
  the origin is in `allowed_origins`, which is empty by default. This
  blocks connections opened from web pages.

**Tokens.**
- `~/.config/acd/` has mode 0700. `client.token`, `hook.token` and
  `t3.token` have mode 0600.
- acd refuses to start if these modes are looser.
- Tokens are 32 random bytes, base64url-encoded, and compared in constant
  time.
- `acd token rotate client|hook` replaces a token and closes the sessions
  that used the old one.

**Logs.**
- Logs never contain tokens.
- Request details and screen content are logged only at `debug` level,
  and truncated to 4 KiB.

**Hook endpoint.** It is a separate HTTP listener on `127.0.0.1` only, using
`hook.token`.

### 7.2 Wire format

- **Frames.** Every frame is one JSON object with a `type` field, the
  message name.
- **Timestamps.** RFC 3339 UTC strings with millisecond precision.
- **Integers.** Values stay within ±2^53.
- **Ids.** Opaque strings.
- **Unknown input.**
  - Clients ignore unknown `type`s and unknown fields.
  - Unknown enum values are treated as follows:
    - unknown agent state: `unknown`;
    - unknown request `kind` or `answer_path`: shown read-only;
    - unknown `effect`: that option is not selectable.
- **Version mismatch.** If `hello.protocol` is not supported, the daemon
  sends `error {code: "unsupported_protocol", supported: ["acd/1"]}` and
  closes with code 4400.

### 7.3 Messages

**Client → daemon.** Every command has a client-generated `id`, which is a
UUID.

```text
hello            {protocol: "acd/1", token, client: {id, name, version}, resume?: {epoch, seq}}
request.answer   {id, request_id, option_id?, text?}    // exactly one of option_id or text
agent.send       {id, agent_id, text}
agent.mark_seen  {id, agent_id}
ping             {id}
```

**Daemon → client:**

```text
welcome          {machine, capabilities: string[], epoch: string, seq: int}
snapshot         {epoch, seq, agents: Agent[]}        // the state as of seq
agent.updated    {epoch, seq, agent}
agent.removed    {epoch, seq, agent_id}
request.opened   {epoch, seq, request}
request.resolved {epoch, seq, request_id, agent_id,
                  by: "inbox"|"terminal"|"agent"|"superseded"|"uncertain"|"lost", option_id?}
machine.updated  {epoch, seq, machine}
result           {id, ok: true, outcome?} | {id, ok: false, error: {code, message}}
pong             {id}
```

**Answer outcomes.** A successful `request.answer` result carries an
`outcome`:
- `"delivered"`: acd wrote the answer to the hook reply, the keys, or T3.
- Confirmation arrives later as `request.resolved {by: "inbox"}`, or as
  `by: "uncertain"`.

**Error codes:**

| Code | Meaning |
|---|---|
| `request_gone` | The request was already resolved. |
| `request_busy` | Another claim is in flight. |
| `request_not_ready` | The dialog is not yet confirmed on screen. |
| `not_answerable` | The request is read-only. |
| `agent_gone` | The agent no longer exists. |
| `agent_busy` | The agent cannot take a follow-up right now. |
| `not_supported` | The capability is not available. |
| `screen_mismatch` | The fingerprint did not match. The request stays `open`. |
| `bad_request` | The message is malformed or invalid. |
| `duplicate_mismatch` | The command id was reused with a different payload. |
| `source_unavailable` | The owning adapter is disconnected. |
| `unauthorized` | The token was rejected; the daemon closes with code 4401. |

### 7.4 Sequencing, resync and idempotency

**Epoch and seq.**
- `epoch` is a random id that acd creates at startup.
- `seq` increases monotonically within an epoch, and all clients share it.

**Snapshot boundary.** The snapshot is built atomically at a known `seq`.
Events delivered after it always have a larger `seq`. Clients set their
cursor from the `seq` in `snapshot`.

**Resume.**
- If `hello.resume.epoch` equals the current epoch and `resume.seq` is
  still inside the ring of the last 1000 events, acd replays the events
  after it.
- Otherwise acd sends a full snapshot.
- A different epoch always gets a snapshot.

**Slow clients.** A client with more than 1000 queued events is closed with
code 4408.

**Idempotency.**
- Command ids are scoped to `client.id`.
- acd caches each command's result for 15 minutes.
- A retry with the same id and the same payload returns the cached result
  without running the command again. The same id with a different payload
  returns `duplicate_mismatch`.

**Recovery after a lost result.** The cache does not survive an epoch
change. After a reconnect into a new epoch, a client with commands in
flight must not resend them. It shows those commands as uncertain and lets
the user decide again from the fresh snapshot.

**Event content.** `agent.updated` always carries the full Agent, including
`pending`. `request.opened` and `request.resolved` exist for clients that
react to events, such as notifications.

### 7.5 Capabilities

- In v1, the daemon advertises `["answer", "send", "mark_seen"]`.
- A client must not send a command whose capability is not advertised.
- New features add new capability strings.

### 7.6 Normative document

- `crates/acd-protocol/PROTOCOL.md` is the normative version of this
  section.
- JSON Schema files are generated from the Rust types and committed.
- Golden JSON examples for every message type are tested in CI.

## 8. Mac app (Tauri)

**Configuration.**
- Each machine has a name, an address (MagicDNS name or IP, plus port) and
  a token.
- Tokens are stored in the Keychain.
- The local machine is added automatically.
- `client.id` is a UUID created once per installation.

**Menu bar.**
- One icon, colored by the most urgent state: amber for `needs_you`, then
  blue for `working`, then green for `done`.
- The number of `needs_you` agents is shown next to it.

**Floating panel.**
- Opened by a global hotkey, by default ⌥Space. It is a non-activating
  `tauri-nspanel`, so focus stays where it was.
- Cards are ordered:
  1. requests, oldest first;
  2. `done`;
  3. `working`;
  4. `idle`, hidden behind a toggle.
- Read-only requests show their content and "answer in terminal".
- `blocked_unparsed` shows the raw-screen preview.

**Keyboard.**
- `J`/`K` move between cards.
- `1`–`9` pick an option.
- `R` opens the reply field.
- `E` marks the agent seen.
- `⏎` confirms.
- `Esc` undoes during the countdown, or closes the panel.

**Undo.**
- Choosing an option or sending a reply starts a 2-second countdown. The
  command is sent only when the countdown ends.
- `Esc` or a click cancels it, and nothing is sent.

**Feedback.**
- `answering` shows a spinner.
- `request_gone` shows "Answered elsewhere".
- `uncertain` and `lost` show "Check the terminal".
- `request_not_ready` keeps the spinner, and the app retries the claim
  automatically for up to 3 s.

**Notifications.** A macOS notification is posted on `request.opened`
while the panel is closed.

**Offline machines.**
- Cards stay visible but greyed out and read-only, with an
  "offline since" time.
- The app reconnects with backoff between 1 s and 30 s.

## 9. Configuration and deployment

**`~/.config/acd/config.toml`.**
- `machine_name`
- `port`
- `listen` (an allowlist checked as in §7.1)
- `hook_port`, default 47801
- `allowed_origins`
- `claude.permission_hold`
- `ignored_entrypoints`
- `herdr.enabled`, `herdr.extra_sockets`
- `t3.enabled`, `t3.url`. The T3 token is kept in `t3.token`.

**Commands.**
- `acd init`
- `acd run`
- `acd service install`: launchd on macOS, a systemd user unit on Linux
- `acd status`
- `acd hooks install|uninstall claude|codex`
- `acd token rotate client|hook`

**Release targets.**
- `aarch64-apple-darwin`
- `x86_64-apple-darwin`
- `x86_64-unknown-linux-gnu`
- `aarch64-unknown-linux-gnu`

The Mac app is built locally in v1. Signing and notarization wait for a
public release.

## 10. Repository layout

```
Cargo.toml                 (workspace)
crates/acd-protocol/       types, PROTOCOL.md, JSON Schema, golden examples
crates/acd-core/           registry, request lifecycle, acd/1 server
crates/acd-adapters/       claude, codex, herdr, t3
crates/acd/                binary: daemon, hook shim, CLI
app/                       Tauri app
tests/fixtures/            hook payloads, herdr samples, T3 samples, detection snapshots
docs/                      research, specs, plans
```

## 11. Open items to settle first in implementation

Each item is a bounded check. Its result updates this spec before any
dependent work begins. Live checks use an isolated herdr session, as in the
spike.

1. **Claude `AskUserQuestion` free text.** Find the key sequence for the
   "Type something" row.
2. **Claude permission decisions.** Record fixtures for allow and for deny
   with the `hookSpecificOutput` envelope. Confirm that the field which
   persists "always allow" works, if one exists.
3. **T3.** Settle these from `packages/client-runtime/src/{rpc,connection}`
   and a live server, recording fixtures:
   - the WebSocket URL and how the token is passed;
   - the decision literals;
   - the shape of a pending request in the thread projection;
   - the question and answer shape, including multiple questions;
   - whether T3 rejects a stale or duplicate `runtime-request.respond`;
   - replay behavior when the subscription reconnects.
4. **herdr seen state.** Can the socket API mark a pane seen?
5. **Codex dialog timing.** Measure the delay from hook return to dialog,
   to settle the 3 s wait.
6. **Coexisting permission hooks.** Run Claude and Codex with an extra
   `PermissionRequest` hook alongside acd's, one deciding allow and one
   deny. Confirm that acd reconciles to the correct outcome.
7. **herdr sequencing.** Check what `state_change_seq` covers (per pane or
   per server). Check the ordering between `events.subscribe` and
   `agent.list`, and whether a gap can be detected.
8. **Shim lifecycle edge cases.**
   - Kill the shim during a hold, and confirm the dialog stays and the
     request moves to `keys`.
   - Answer in the terminal and in the inbox within the same 100 ms, and
     confirm exactly one outcome.
   - Restart acd during a hold.
9. **Claude hooks-only discovery.** Check whether already-running Claude
   sessions outside herdr can be discovered before their next hook, for
   example from `~/.claude/projects` transcripts. If they cannot, v1
   accepts that they appear on their next hook.
10. **Upstream ask, not blocking.** Propose a conditional-input method to
    herdr, such as "send keys if the detection fingerprint equals X". That
    would make the `keys` path atomic.

## 12. Error handling

| Failure | Behavior |
|---|---|
| acd down or the hook endpoint unreachable | The shim replies empty within about 1 s, and the normal dialog shows. |
| A held hook's connection closes early | Reconcile as in §6.2, step 5: switch to `keys` if the dialog is still on screen, otherwise resolve `terminal`. Hooks-only agents resolve `lost`. |
| `permission_hold` expires | acd replies empty. Herdr-bound requests switch to `keys`; others resolve `lost`. |
| Key answer cannot be confirmed | `by: "uncertain"`, with no automatic retry. |
| Wrong dialog on screen | `screen_mismatch`. The request stays `open`, and the client may refresh. |
| Answer arrives after resolution | `request_gone`. |
| Result lost when the client disconnects | Within the same epoch, the cached result is returned on retry. In a new epoch, the client treats the command as uncertain. |
| herdr or T3 unavailable, or the version is out of range | That source becomes `degraded`, and its agents show `unknown`. acd reconnects with backoff. |
| T3 decode error | The source becomes `degraded`. The frame is logged at `debug` level with redaction and truncated to 4 KiB. |
| acd restarts | A new epoch starts. Held hooks drop, and their agents fall back to the terminal dialog. State is rebuilt from herdr and T3. Requests still on screen reappear: parsed when the fingerprint matches a known format, otherwise as `blocked_unparsed`. |
| Client disconnects during the undo window | Nothing is sent. |

## 13. Testing

**Unit tests:**
- the lifecycle state machine (§5.1): claim, upgrade, supersede, exclusive
  answer paths;
- correlation keys and deduplication;
- binding and migration;
- the entrypoint and subagent filter;
- payload-to-Request mapping;
- the fingerprint, against recorded detection snapshots;
- option-to-key mapping;
- epoch, seq, ring buffer, snapshot boundary;
- idempotency cache;
- bind allowlist;
- undo timing, with fake timers.

**Contract tests:**
- hook fixtures, both those recorded in the spike and new ones;
- herdr fixtures for `agent.list`, events and `pane.read`;
- T3 fixtures, generated from T3's TypeScript schemas;
- `acd/1` golden examples, validated against the generated JSON Schema.

**Integration tests in CI:**
- a fake herdr socket;
- a fake T3 server;
- real HTTP through the shim, including shim kill and timeout boundaries.

**Live end-to-end, manual (`just e2e`):** real Claude Code and Codex in an
isolated herdr session, covering:
- answering from the inbox;
- answering in the terminal first;
- simultaneous answers;
- acd down;
- hold expiry;
- acd restart mid-hold;
- coexisting permission hooks;
- running hook install and uninstall twice each.

## 14. Acceptance criteria (v1)

1. **Claude permission, remote.** A Claude permission prompt in a herdr
   pane on a remote Tailscale machine appears in the panel within 2 s.
   Choosing "Yes" unblocks the agent, and the dialog closes.
2. **Codex approval.** A Codex approval in herdr becomes answerable within
   3 s of appearing. Yes and No both act as chosen.
3. **T3.** A T3 approval and a T3 single-select question can each be
   answered from the panel.
4. **Answered elsewhere.** Answering any of the above in the terminal or in
   T3 first resolves the card on every client within 2 s. The second
   answer gets `request_gone`, or for `keys` `screen_mismatch` or
   `uncertain`. It never produces a second keystroke.
5. **Follow-ups.** A free-text follow-up reaches an idle herdr agent and a
   T3 thread exactly once, even if the client retries after a dropped
   connection in the same epoch.
6. **acd stopped.** With acd stopped, prompts behave as they do without
   agent-caller, and hooks add at most 1 s of delay.
7. **Undo.** `Esc` within 2 s sends nothing.
8. **acd restart.** Restarting acd while requests are open leaves no agent
   stuck. Requests still on screen reappear.
9. **Hook install round trip.** Running `acd hooks install` twice and then
   `uninstall` leaves the hook configs semantically identical to before.
10. **Other agents.** An agent of another kind that herdr detects is shown
    with its state. When it is blocked, its raw screen is shown read-only.

## 15. Review log

**Round 1, Sol (`gpt-6.1-sol`, read-only), 2026-10-07.** It found 7 blocking
items, 4 non-blocking items and 2 questions. All were accepted.

| # | Finding | Resolution |
|---|---|---|
| 1 | Keystrokes are not atomic | Invariant 2 states the weaker guarantee for `keys`. The fingerprint includes the detail text and is re-read just before sending. No automatic retries; `uncertain` outcome. Upstream ask in §11.10. |
| 2 | A closed connection does not mean a terminal answer | Reconcile against the screen (§6.2, step 5). acd times out before the shim. Live tests in §11.8. |
| 3 | Lifecycle and identity | §5.1: correlation keys, claim, `answering` status, resolution by source evidence, upgrade and supersede rules, exclusive answer paths. |
| 4 | Resume after restart | `epoch` plus `seq`, and the snapshot boundary (§7.4). |
| 5 | Idempotency | Command-id cache and the `delivered` outcome, plus recovery across epochs (§7.3, §7.4). |
| 6 | Question semantics | `questions[]` and `multi_select` are kept. Only single-select single questions can be answered; others are read-only. T3 shapes are checked in §11.3. |
| 7 | Raw input fallback | `blocked_unparsed` is read-only. Follow-up text excludes control characters (§6.4). Invariant 3. |
| 8 | Ownership recovery | Binding is reconciled on every hook, with migration and socket-namespaced ids (§6.2). |
| 9 | Coexisting hooks | Idempotent install, backup, owned-entry record and exact uninstall. Live test in §11.6. |
| 10 | Auth hardening | Modes enforced, pre-auth limits, Origin policy, bind allowlist, MagicDNS check, rotation, redaction (§7.1). |
| 11 | Wire definitions | §7.2: discriminator, formats, unknown-value handling, version rejection. |
| 12 | Deny envelope, competing answer paths | Deny envelope corrected. Verified in §11.2. Exclusive paths (§5.1). |
| 13 | More assumptions to verify | §11, items 3 and 6–9. herdr range bounded to `< 0.10`. |
