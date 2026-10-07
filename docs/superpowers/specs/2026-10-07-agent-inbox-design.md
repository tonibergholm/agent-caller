# Agent inbox v1: design spec

Date: 2026-10-07
Status: draft, awaiting independent review and owner approval
Research: `docs/research/2026-10-07-landscape.md`, which also holds the spike log

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
- A Mac menu bar app built with Tauri. It shows state dots, opens a floating
  panel on a global hotkey, and is driven by the keyboard.
- Answering permission prompts and multiple-choice questions, with a
  2-second undo.
- Free-text follow-ups to idle or done agents.
- Agent kinds: Claude Code and Codex are fully supported. Any other agent
  that herdr detects gets the raw-screen fallback described in §7.4.
- Remote machines are reached over Tailscale. The daemon listens only on
  localhost and on the machine's Tailscale address.

### Not in v1 (planned later; the protocol leaves room through `capabilities`)

- Voice router (planned reuse of Handy's crates: `transcribe-rs`, `vad-rs`,
  `handy-keys`)
- Phone client and push notifications
- A self-hosted, end-to-end encrypted relay for users without Tailscale
- Starting new agents from the inbox
- Transcript or history browsing
- Agent kinds other than Claude Code and Codex with full support
- Windows and Linux desktop clients

## 3. Glossary

| Term | Meaning |
|---|---|
| **acd** | The agent-caller daemon. One runs per machine. |
| **Source** | Where acd learns about agents: `hooks`, `herdr` or `t3`. |
| **Agent** | One live agent session: a herdr pane running an agent, a Claude session outside herdr, or a T3 thread. |
| **Request** | Something an agent is waiting on the user for: a permission or a question. |
| **Client** | Anything connected to acd over `acd/1`, such as the Mac app. |
| **Hook shim** | `acd hook …`. Agent hook configs call it; it forwards the hook to acd. |

## 4. Architecture

```
 Mac: Tauri menu bar app (status dots, floating panel, hotkey)
        │  acd/1 over WebSocket, one connection per machine, via Tailscale
        ▼
 acd (Rust daemon, one per machine; the laptop runs one too)
   ├─ hook endpoint (HTTP, 127.0.0.1)  ◄── `acd hook` shim ◄── Claude/Codex hooks
   ├─ herdr adapter  ◄─► herdr Unix socket(s): state, events, screen, keys, prompt
   ├─ t3 adapter     ◄─► T3 server WebSocket RPC: threads, requests, messages
   └─ core: agent registry, request lifecycle, acd/1 server
```

### Invariants

1. **Fail open.** acd never causes an agent to hang. When acd is
   unreachable, slow or undecided, the agent's own terminal dialog takes
   over. Hooks only hold a request where the spike showed that the terminal
   dialog stays usable while they wait. Today that is only Claude's
   `PermissionRequest`.
2. **First answer wins.** An answer can come from the inbox, the terminal,
   T3's own UI or another client. Whichever lands first resolves the
   request, and every client is told how it was resolved.
3. **Agents are owned by one source.** Each agent has exactly one owning
   adapter. Duplicate observations are merged or dropped as described in
   §6.3.

## 5. Data model

All model types live in the `acd-protocol` crate. The Tauri app gets
TypeScript versions of them through generated bindings (`ts-rs` or
`specta`).

```text
Machine {
  id: string               // stable: the hostname plus a random suffix created on first run
  name: string             // display name, from config
  acd_version: string
  sources: { hooks: SourceStatus, herdr: SourceStatus, t3: SourceStatus }
}
SourceStatus = "ok" | "degraded" | "disabled" | "unconfigured"   (+ optional message)

Agent {
  id: string               // "herdr:<session>:<pane_id>" | "t3:<thread_id>" | "claude:<session_id>"
  machine_id: string
  source: "herdr" | "t3" | "hooks"
  kind: "claude" | "codex" | "other"
  title: string            // herdr agent name or terminal title, or the T3 thread title
  cwd: string | null
  project: string | null   // last segment of the git root, or of cwd
  state: AgentState
  state_since: timestamp
  failed: bool             // T3 failed or interrupted
  last_message: string | null   // last reply, at most 500 characters
  pending: Request | null
  capabilities: { answer: bool, send: bool }   // per agent: what the inbox can do with it
}
AgentState = "working" | "needs_you" | "done" | "idle" | "unknown"

Request {
  id: string               // generated by acd; unique per machine
  agent_id: string
  kind: "permission" | "question" | "blocked_unparsed"
  title: string            // e.g. "Bash", or the question's header
  detail: string           // the command or path, or the question text
  options: Option[]
  free_text: bool          // whether a typed answer is accepted
  raw_screen: string | null   // only for blocked_unparsed
  opened_at: timestamp
}
Option {
  id: string               // stable within the request
  label: string
  description: string | null
  effect: "allow" | "allow_always" | "deny" | "choose" | "keys"
}
```

Rules:

- **`done` vs `idle`.** `done` means the agent finished and no client has
  marked it seen. herdr's `done` and `idle` map one to one. For T3 and
  hooks-only agents, `done` changes to `idle` when a client sends
  `agent.mark_seen`.
- **One request per agent.** If a new request arrives while one is open,
  the old one is resolved with `by: "superseded"` and the new one opens.
- **Agent removal.** Agents are removed when their pane closes, their T3
  thread is archived or deleted, or (for hooks-only agents) 10 minutes pass
  after `SessionEnd`, or 24 hours pass without any event.

## 6. Sources and adapters

### 6.1 Hook shim and hook endpoint

**The shim.** It is `acd hook <agent> <event>`, the same binary as acd.

1. Reads the hook's JSON payload from stdin. Payloads larger than 1 MiB
   are truncated, and the shim then returns empty.
2. Adds the environment values it needs: `HERDR_PANE_ID`,
   `HERDR_SOCKET_PATH`, `HERDR_ENV` and `CLAUDE_CODE_ENTRYPOINT`, plus the
   shim's own parent PID.
3. POSTs the result to `http://127.0.0.1:<hook_port>/hook/<agent>/<event>`,
   with the local token from `~/.config/acd/hook.token` (file mode 0600).
4. Writes acd's response body to stdout, unchanged, and exits 0.

**Fail open.** Any error makes the shim print nothing and exit 0: acd is
not running, the connection is refused, the response is malformed, or the
timeout is reached. Connect timeout is 500 ms. The response timeout depends
on the event; see §6.2 and §6.3.

**Install.** `acd hooks install claude|codex` merges entries into:

- `~/.claude/settings.json` for Claude
- `~/.codex/hooks.json` for Codex

It leaves existing entries alone, including those from herdr, Supacode and
Warp. Each entry it adds is tagged with `"acd-managed"` in its command
string so that `acd hooks uninstall` can find and remove it. Codex then
shows its own one-time review of the new hooks; this is expected and is
documented in the README.

### 6.2 Claude adapter (hooks)

**Hooks installed:**

| Hook | Matcher | Shim response timeout |
|---|---|---|
| `SessionStart` | — | 2 s |
| `PermissionRequest` | — | the configured `claude.permission_hold` (default 3600 s) |
| `PreToolUse` | `AskUserQuestion` | 2 s |
| `Stop` | — | 2 s |
| `SessionEnd` | — | 2 s |

The `timeout` field in the hook entry is set to the shim timeout plus 5 s.

**Filter.** Payloads with `CLAUDE_CODE_ENTRYPOINT=sdk-ts`, and payloads
from subagents (where `agent_id` is present), are acknowledged with an
empty reply and otherwise ignored. T3-hosted Claude sessions are covered by
the T3 adapter. The filter's entrypoint list is configurable, so other
hosts that use the Agent SDK can be excluded too.

**Session binding.** `SessionStart` records `session_id → herdr pane`
(from `HERDR_PANE_ID` and `HERDR_SOCKET_PATH`). If no herdr pane is
present, the agent becomes a hooks-only agent with id
`claude:<session_id>`, `capabilities.send = false` and
`capabilities.answer = true` (permissions only).

**Permission requests** (`PermissionRequest` where `tool_name` is not
`AskUserQuestion`):

1. Open a `permission` request with these fields:
   - `title` = `tool_name`
   - `detail` = `tool_input.command`, `tool_input.file_path`, or the
     compact JSON of `tool_input`
   - `options`:
     - `allow`: "Yes"
     - `allow_always`: "Yes, always", only when `permission_suggestions`
       is not empty
     - `deny`: "No"
2. Hold the HTTP response.
3. On `request.answer`, reply with the decision:
   - `allow`: `{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}`
   - `allow_always`: the same `allow` decision, plus `updatedPermissions`
     set to the payload's `permission_suggestions`. **This field is
     unverified; see §11, item 2.**
   - `deny`: `{"behavior":"deny","message":"Denied from agent inbox"}`
   - In every case, resolve the request with `by: "inbox"`.
4. If the shim connection closes first, the user answered in the terminal.
   Resolve the request with `by: "terminal"`. The spike showed this case
   arrives as a broken pipe or reset on the held connection; acd watches
   the socket for it.
5. If `permission_hold` expires, reply empty. The terminal dialog is still
   visible, so the request switches to the key-press path in step 6. The
   request id does not change.
6. **Key-press path.** This is used after a hold timeout, or for a session
   bound to a herdr pane whose hook endpoint has gone away (for example,
   acd restarted). Before sending a key, the adapter:
   1. reads herdr's `detection` snapshot;
   2. checks that it shows the expected dialog, by matching the option
      labels;
   3. sends the number key for that option (`1`, `2`, or the option's
      index);
   4. sends `esc` to deny.

   If the dialog does not match, the adapter refuses with the error
   `screen_mismatch` and converts the request to `blocked_unparsed`.

**Questions** (`AskUserQuestion`):

1. The `PreToolUse` hook replies empty immediately. It does not hold, so
   the terminal dialog stays visible. The adapter opens a `question`
   request from `tool_input.questions[0]`, with options of
   `effect: "choose"` in the order the dialog shows them, and
   `free_text: true`.
2. The matching `PermissionRequest` (where `tool_name` is
   `AskUserQuestion`) also replies empty immediately. It is used only to
   connect the two events, through `prompt_id`.
3. Answers are given by key presses through herdr. For an option, send its
   1-based number key. Free text is sent through the "Type something" row.
   **The exact key sequence for free text is unverified; see §11, item 1.**
4. If a request has more than one question, v1 handles only the first.
   The remaining questions show as `blocked_unparsed` after the first is
   answered.
5. Without a herdr pane, questions are shown read-only:
   `capabilities.answer = false` for that request, displayed as "answer in
   terminal".

**Completion.**

- `Stop` sets the state to `done`. `last_message` is taken from
  `last_assistant_message` when present, otherwise from the last assistant
  entry in `transcript_path`.
- `SessionEnd` marks a hooks-only agent for removal.

### 6.3 Codex adapter (hooks + herdr)

**Hooks installed:**

| Hook | Shim response timeout |
|---|---|
| `SessionStart` | 2 s |
| `PermissionRequest` | 1 s |
| `Stop` | 2 s |

**`PermissionRequest` replies empty at once.** The spike showed that Codex
does not show its dialog while the hook runs, so holding it would stall
the terminal. The adapter opens a `permission` request with:

- `title` = `tool_name`
- `detail` = `tool_input.command`, followed by `tool_input.description`
- `options`: `allow` "Yes" (key `1`), `allow_always` "Yes, don't ask
  again" (key `2`), `deny` "No" (key `esc`)

**Answers** go through the guarded key-press path in §6.2, step 6. Codex's
dialog appears after the hook returns, so the adapter waits up to 3 s for
the dialog to show in the detection snapshot before sending keys.

**Without a herdr pane**, the request is read-only (`answer = false`).

**Completion.** `Stop` sets the state to `done`, with `last_message` taken
from `last_assistant_message`.

**Merging with herdr.** The agent is keyed by its herdr pane. Hook data
adds detail to the herdr-owned agent. herdr's `blocked` state and the
hook's request describe the same moment, and the hook's structured request
takes precedence.

### 6.4 herdr adapter

**Discovery.** At startup, and then every 30 s, the adapter enumerates:

- the default session, at `$HERDR_SOCKET_PATH` or
  `~/.config/herdr/herdr.sock`
- every session under `~/.config/herdr/sessions/*/herdr.sock`

**Compatibility.** For each socket, it sends `ping` and checks the
version. The supported range is herdr ≥ 0.9.3 and protocol ≥ 22. Outside
that range, the session is marked `degraded` with a message saying why.

**Sync.**

1. Load the current agents with `agent.list`.
2. Subscribe with `events.subscribe` to `pane_agent_status_changed`,
   `pane_agent_detected` and `pane_closed`.
3. On `events_lost`, on reconnect, or when `state_change_seq` jumps,
   reload with `agent.list`.

**State mapping.** herdr's `blocked` becomes `needs_you`. `working`,
`done`, `idle` and `unknown` map directly. Panes with no agent are
ignored.

**Fallback.** A pane that stays `blocked` for 2 s, with no structured
request from hooks, opens a `blocked_unparsed` request with:

- `raw_screen` = the `detection` snapshot, at most 40 lines
- `options`: one `effect: "keys"` option for each key in
  `["1","2","3","4","enter","esc"]`
- `free_text: true`, which sends literal text with `pane.send_text`

**Follow-ups.** `agent.send` calls herdr `agent.prompt`. herdr refuses
with `agent_blocked` when the agent is blocked; acd passes that through as
`agent_busy`.

**Mark seen.** `agent.mark_seen` uses herdr's own seen mechanism if the
socket API offers one. **Unverified; see §11, item 4.** Otherwise acd
keeps a seen flag of its own.

**Known gap.** herdr reports Codex's folder-trust and hook-review screens
as `idle`. v1 accepts this: those screens only appear at startup.

### 6.5 T3 adapter

**Connection.** A WebSocket RPC to the local T3 server. Its URL and bearer
token come from acd config. The token is created with
`t3 auth session issue --scope orchestration:read --scope orchestration:operate`.

**Contract.** Hand-written Rust types cover the subset acd uses:

- `subscribeShell`
- `subscribeThread`
- `dispatchCommand`, with `message.dispatch` and `runtime-request.respond`

The supported T3 version is pinned. Unknown fields and unknown enum
values are tolerated: an unknown status maps to `unknown`.

**State mapping:**

| T3 status | acd state |
|---|---|
| `running`, `starting`, `preparing`, `queued` | `working` |
| `waiting` | `needs_you` |
| `completed` | `done` |
| `failed`, `interrupted` | `done` with `failed: true` |
| `idle`, `cancelled`, `rolled_back` | `idle` |

**Requests.**

- For each waiting thread, the adapter subscribes to that thread to read
  the pending runtime request.
- Approval kinds map to `permission`. Their options are built from the
  request's decision values.
- User-input questions map to `question`, with `free_text: true`.

**Answers** are sent as `runtime-request.respond`, with `decision` for
approvals and `answers` for questions.

**Follow-ups** are sent as `message.dispatch`.

**Unverified before implementation (see §11, item 3):**

- the WebSocket URL, and how the token is presented
- the decision literals
- the field path of a pending request in the thread projection

## 7. Protocol acd/1

### 7.1 Transport and auth

- **Transport.** WebSocket on `127.0.0.1:<port>` and on the machine's
  Tailscale IPv4 and IPv6 addresses (default port 47800). acd does not
  bind to other interfaces.
- **Messages.** Text frames, each holding one JSON object.
- **Auth.** The first frame must be `hello`. Its token is compared in
  constant time against `~/.config/acd/client.token`, which `acd init`
  generates as 32 random bytes in base64url. Any other first frame, or a
  bad token, closes the connection with code 4401.
- **Hook endpoint.** It is a separate HTTP listener, bound to 127.0.0.1
  only, and uses a different token (`hook.token`).

### 7.2 Messages

**Client to daemon:**

```text
hello            {protocol: "acd/1", token, client: {name, version}, resume_seq?: int}
request.answer   {id, request_id, option_id? , text?}      // exactly one of option_id or text
agent.send       {id, agent_id, text}
agent.mark_seen  {id, agent_id}
ping             {id}
```

**Daemon to client:**

```text
welcome          {machine: Machine, capabilities: string[], seq: int}
snapshot         {seq, agents: Agent[]}
agent.updated    {seq, agent: Agent}
agent.removed    {seq, agent_id}
request.opened   {seq, request: Request}
request.resolved {seq, request_id, agent_id, by: "inbox"|"terminal"|"agent"|"timeout"|"superseded", option_id?}
machine.updated  {seq, machine: Machine}
result           {id, ok: true} | {id, ok: false, error: {code, message}}
pong             {id}
```

**Error codes:**

| Code | Meaning |
|---|---|
| `request_gone` | The request was already resolved. |
| `agent_gone` | The agent no longer exists. |
| `agent_busy` | The agent is blocked or working and cannot take a follow-up. |
| `not_supported` | The agent's capabilities rule this out. |
| `screen_mismatch` | The terminal did not show the expected dialog. |
| `bad_request` | The message was malformed. |
| `source_unavailable` | The owning adapter is disconnected. |

### 7.3 Sequencing and resync

- `seq` increases monotonically per daemon run and is shared by all
  clients.
- The daemon keeps a ring buffer of the last 1000 events.
- On `hello`, the daemon replays from the ring when `resume_seq` is given
  and still in the buffer. Otherwise it sends a fresh `snapshot`.
- On a slow client (more than 1000 events queued), the daemon closes the
  connection with code 4408. The client reconnects and gets a snapshot.
- `agent.updated` always carries the full Agent, including `pending`.
  `request.opened` and `request.resolved` are sent for clients that react
  to events, such as notifications.

### 7.4 Capabilities

v1 advertises `["answer", "send", "mark_seen", "blocked_unparsed"]`.
Future features are added as new capability strings. Clients must ignore
unknown message types and unknown fields.

### 7.5 Spec document

`acd-protocol/PROTOCOL.md` is the normative copy of this section.
JSON Schema files for every message are generated from the Rust types and
committed alongside it, so third-party clients do not need Rust.

## 8. Mac app (Tauri)

**Configuration.** Machines are listed in settings: name, address
(Tailscale MagicDNS name or IP, plus port) and token. Tokens are stored in
the macOS Keychain. The local machine is added automatically.

**Menu bar.** One icon, colored by the most urgent state across all
machines: amber for `needs_you`, then blue for `working`, then green for
`done`. Next to it is the number of `needs_you` agents.

**Floating panel.**

- Opened by a global hotkey; the default is ⌥Space, and it can be
  changed.
- Implemented as a non-activating panel (`tauri-nspanel`), so it does not
  take focus away from the terminal.
- **Card order:**
  1. Requests, oldest first.
  2. `done`, newest first.
  3. `working`.
  4. `idle` agents are hidden behind a "Show idle" toggle.
- Each card shows machine, project, agent kind and title. Depending on its
  state it also shows the request with numbered options, the
  `last_message` excerpt, or a `raw_screen` preview.

**Keyboard:**

| Key | Action |
|---|---|
| `J` / `K` | Move between cards |
| `1`–`9` | Choose an option |
| `R` | Reply: opens an inline field for free text or a follow-up |
| `E` | Mark a `done` card seen, which clears it |
| `Esc` | Undo during the 2-second window; otherwise close the panel |
| `⏎` | Choose the highlighted option, or send the reply |

**Undo.** Choosing an option starts a 2-second countdown on the card.
`request.answer` is sent when the countdown ends. Pressing Esc or clicking
the card cancels it, and nothing is sent. Follow-ups through `agent.send`
get the same 2-second undo.

**Notifications.** A macOS notification is shown on `request.opened`
while the panel is closed. Clicking it opens the panel on that card.

**Stale machines.** A disconnected machine's cards stay visible, greyed
out, marked "offline since hh:mm", and read-only. The app reconnects with
backoff from 1 s up to 30 s.

**`request_gone`.** The card shows "Answered elsewhere" for 2 s, then
disappears.

## 9. Configuration and deployment

**Daemon config.** `~/.config/acd/config.toml` holds:

- `machine_name`
- the WebSocket port and listen interfaces (default: localhost plus
  Tailscale)
- the hook port (default 47801)
- `claude.permission_hold`
- the ignored entrypoints
- whether herdr is enabled, plus extra socket paths
- whether T3 is enabled, plus its URL; the T3 token is stored in
  `~/.config/acd/t3.token` (mode 0600)

**Commands:**

- `acd init`: generates tokens and the default config, and prints the
  client token.
- `acd run`: runs in the foreground.
- `acd service install`: sets up a launchd agent on macOS or a systemd
  user unit on Linux.
- `acd status`: shows sources, agents and open requests.
- `acd hooks install|uninstall claude|codex`

**Builds.** Release binaries are produced for `aarch64-apple-darwin`,
`x86_64-apple-darwin`, `x86_64-unknown-linux-gnu` and
`aarch64-unknown-linux-gnu`.

**App.** The app is a signed macOS `.app` bundle. Signing and
notarization are out of scope until a public release; local builds are
enough for v1.

## 10. Repository layout

```
Cargo.toml                 (workspace)
crates/acd-protocol/       types, PROTOCOL.md, generated JSON Schema
crates/acd-core/           registry, request lifecycle, acd/1 server
crates/acd-adapters/       claude, codex, herdr, t3 (one module each)
crates/acd/                binary: daemon, hook shim, CLI
app/                       Tauri app (Rust shell + TypeScript UI)
tests/fixtures/            recorded hook payloads, herdr schema samples, T3 message samples
docs/                      research, specs, plans
```

## 11. Open items to settle first in implementation

Each item is a short, bounded task. Its result updates this spec before
any dependent work starts.

1. **Free text in Claude `AskUserQuestion`.** Find the key sequence that
   types free text through the "Type something" row, using the isolated
   herdr session from the spike.
2. **Claude "always allow" through the hook.** Check whether
   `decision.updatedPermissions` (or the current equivalent field)
   persists the suggested rule. If it does not, drop `allow_always` for
   held Claude requests, or send key `2` instead.
3. **T3 handshake.** Read `packages/client-runtime/src/{rpc,connection}`
   in t3code and confirm three things:
   - the WebSocket URL and how the token is presented
   - the `ProviderApprovalDecision` literals
   - the shape of a pending request in the thread projection

   Record fixtures from a live T3 server.
4. **herdr seen state.** Check whether the socket API can mark a pane
   seen. If it cannot, acd keeps its own flag (already the default in
   §6.4).
5. **Codex dialog timing.** Measure the delay between the
   `PermissionRequest` hook returning and the dialog appearing, to confirm
   the 3 s wait in §6.3.

## 12. Error handling

| Failure | Behavior |
|---|---|
| acd down, or the hook endpoint is unreachable | The shim prints nothing and exits 0 within about 1 s. The agent shows its normal dialog. |
| `permission_hold` expires | Empty reply. The request continues on the key-press path. |
| Answered in the terminal | The held connection closes, and the request resolves with `by: "terminal"`. For Codex and for key-press requests, herdr leaving `blocked` resolves it with `by: "terminal"`. |
| Answer arrives after resolution | `request_gone`. |
| The wrong dialog is on screen | `screen_mismatch`, and the request converts to `blocked_unparsed`. |
| herdr socket gone, or an incompatible version | That session's agents become `unknown` and greyed out; `machine.updated` reports the source as `degraded`. Reconnect with backoff. |
| T3 disconnect, or a contract decode error | Same as herdr. Decode errors are logged with the raw frame, truncated to 4 KiB. |
| acd restarts | Held hook connections drop, and those agents fall back to the terminal dialog. On startup, acd rebuilds state from herdr and T3. Claude requests still on screen reappear as `blocked_unparsed`, or as parsed requests when the dialog matches. |
| Client disconnects during the undo window | Nothing is sent: the app sends only after the window ends. |

## 13. Testing

**Unit tests:**

- registry merging and ownership
- the `sdk-ts` and subagent filter
- mapping hook payloads to Requests
- mapping options to keys
- dialog matching against recorded detection snapshots
- seq, ring buffer and resync
- undo timing in the app, using fake timers

**Contract tests:**

- hook payload fixtures, recorded in the spike plus new recordings
- herdr fixtures for `agent.list`, events and `pane.read`
- T3 fixtures, generated by a small script that runs T3's own TypeScript
  schemas against sample values. Changes in T3's schemas then fail CI
  instead of failing in production.

**Integration tests with fakes:**

- a fake herdr socket server
- a fake T3 WebSocket server
- real HTTP calls through the shim

These run in CI.

**Live end-to-end (manual, `just e2e`):** drives real Claude Code and Codex
in an isolated herdr session, as in the spike. It covers:

- answering from the inbox
- answering in the terminal first
- acd down
- the hold timeout

It costs real agent usage, so it is not run in CI.

## 14. Acceptance criteria (v1)

1. A Claude permission prompt in a herdr pane on a remote Tailscale
   machine appears in the panel within 2 s. Choosing "Yes" from the Mac
   unblocks the agent, and the terminal dialog closes.
2. A Codex approval in herdr appears within 2 s. Answering from the Mac
   proceeds or denies, as chosen.
3. A T3 thread approval and a T3 user-input question can each be
   answered from the panel.
4. Answering any of the above in the terminal or in T3 first removes the
   card from every connected client within 2 s.
5. A free-text follow-up reaches an idle herdr agent and a T3 thread.
6. With acd stopped, Claude and Codex prompts behave exactly as without
   agent-caller. Hooks add no more than 1 s of delay.
7. Pressing Esc within 2 s after choosing an option sends nothing.
8. Restarting acd while requests are open leaves no agent stuck.
9. `acd hooks uninstall` restores the hook configs to their pre-install
   state, apart from formatting.
