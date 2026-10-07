# Agent inbox v1: design spec

Date: 2026-10-07
Status: revision 4. It folds in the P0 findings and the P1 review rulings
(see §15) and awaits a Sol review of the revision.
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
| **Hook shim** | `acd hook codex …`. Codex hook configs call it, and it forwards the hook to acd. Claude uses native `http` hooks instead. |

## 4. Architecture

```
 Mac: Tauri menu bar app (status icon, floating panel, hotkey)
        │  acd/1 over WebSocket, one connection per machine, via Tailscale
        ▼
 acd (Rust daemon, one per machine; the laptop runs one too)
   ├─ hook endpoint (HTTP, 127.0.0.1)  ◄── Claude http hooks / `acd hook codex` shim
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

| Origin | Key | Reusable |
|---|---|---|
| Claude hook | `prompt_id`. It links `PermissionRequest` and `PreToolUse` for the same `AskUserQuestion`. | no |
| Codex hook | `(session_id, turn_id, sha256(tool_name + tool_input))` | yes |
| herdr fallback | `(socket, pane_id, dialog fingerprint)` | yes |
| T3 | the runtime request id | no |

Keys are scoped to their agent. Every key-based observation (`ready`,
answer path, end, gone) names its agent and affects only that agent's
requests.

**Occurrences.** A key identifies one *occurrence* of a dialog.

- While a request with that key is not yet resolved, observing the key
  again updates the existing request.
- Once the request is resolved, and the source has confirmed the dialog is
  gone, a later observation of the same key starts a **new occurrence**:
  acd increments a per-key generation counter and opens a new request with
  a new id.
- "Confirmed gone" means the fingerprint was absent from two consecutive
  detection reads, or the hook source reported the end of the request.
- **Non-reusable keys** (Claude `prompt_id`, T3 request ids) are
  tombstoned once their request resolves. They never open another
  request, even after the dialog is confirmed gone. The generation counter
  only applies to reusable keys.

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
  - **Hooks-only agents** have no screen to check. If acd has delivered an
    answer and no `PostToolUse`, `Stop` or new request arrives within 30 s,
    the request resolves `by: "uncertain"`.
- **`keys`.** After sending the key, acd polls the detection snapshot for
  up to 3 s, and checks the prompt line for a stray keystroke (§6.2).
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
- **`lost`.** The source can no longer observe the request. Examples: the
  hold expires on a hooks-only agent; the agent is removed.

**Attribution.** `by: "inbox"` (with the claimed `option_id`) needs
delivery evidence from the adapter (§6). A request that a client has
claimed but that ends for another reason, such as the user answering in
the terminal first, resolves `by: "terminal"`. acd never relabels it as
`inbox`.

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

All adapters follow the **adapter contract** documented on the `Adapter`
trait (P1):

- `deliver`, `send` and `mark_seen` finish within 10 s and are
  cancellation-safe.
- `NotDelivered` is returned only when nothing reached the agent.
- An adapter reports `by: "inbox"` only when it has evidence that its own
  delivery took effect. Any other "the dialog ended" evidence is
  `by: "terminal"`.
- After a resolution that changes the agent's state, the adapter emits an
  agent upsert.

### 6.1 Hook endpoint and installation

acd's hook endpoint is an HTTP listener on `127.0.0.1:<hook_port>` only. It
accepts `POST /hook/<agent>/<event>` with the header
`X-Acd-Token: <hook.token>`. Any other request gets 401.

The two agents reach it in different ways (P0 item 8 and the additional
findings):

- **Claude: native `http` hooks, no shim.**
  - Each hook entry has `type: "http"`, a `url`, the header
    `X-Acd-Token` (the literal token value), and these environment headers:
    - `X-Herdr-Pane: $HERDR_PANE_ID`
    - `X-Herdr-Socket: $HERDR_SOCKET_PATH`
    - `X-Claude-Entrypoint: $CLAUDE_CODE_ENTRYPOINT`

    The entry lists those variables in `allowedEnvVars`.
  - Claude itself fails open:
    - a connection error, a non-2xx reply or a timeout lets execution
      continue;
    - a malformed `decision` falls back to the dialog (P0 item 2).
  - When the user answers in the terminal, the held connection closes.
- **Codex: the command shim `acd hook codex <event>`.** Codex supports only
  `command` hooks.
  - The shim reads stdin, up to 1 MiB.
  - It adds `CODEX_*` and `HERDR_*` environment values as hints. They are
    unreliable; see §6.3.
  - It POSTs with the token read from `hook.token`.
  - It prints acd's body and exits 0. Any failure prints nothing.
  - Connect timeout: 500 ms.

**Installation.** `acd hooks install claude|codex`:

- is idempotent: owned entries carry the marker `acd-managed:v1` in the
  URL query or the command;
- backs up the file before writing;
- writes atomically and keeps the file's mode;
- records its entries in `~/.config/acd/hooks-installed.json`;
- never edits or reorders entries it doesn't own.

Claude may rewrite `.claude/settings*.json` formatting when it persists a
rule, so ownership is matched by marker, never by byte layout.
`acd hooks uninstall` removes exactly the recorded entries, matched
semantically. The Claude settings file must be mode 0600, because it holds
the literal hook token. The installer enforces that.

**Coexisting permission hooks** (verified, P0 item 6):

- Any decisive hook beats an empty reply, and deny beats allow.
- If another hook decides a Claude request that acd is holding, Claude
  leaves acd's held connection open and ignores it.
- acd therefore resolves such requests through reconciliation:
  `PostToolUse`, `Stop`, or the fingerprint leaving the screen.
- Once acd knows the request ended, it replies empty to the held hook.

### 6.2 Claude adapter (http hooks)

**Hooks installed** (all `type: "http"`):

| Hook | Matcher | Hook `timeout` |
|---|---|---|
| `SessionStart` | — | 5 s |
| `UserPromptSubmit` | — | 5 s |
| `PermissionRequest` | — | `permission_hold` + 10 s (default 3600 s + 10 s) |
| `PreToolUse` | `AskUserQuestion` | 5 s |
| `PostToolUse` | — | 5 s |
| `Stop` | — | 5 s |
| `SessionEnd` | — | 5 s |

acd always replies before the hook timeout.

**Session discovery** (P0 item 9). Every 30 s and at startup, acd scans
`~/.claude/sessions/*.json` and never opens the `.key` files there.

- Each live entry is validated by its `pid` and `procStart`.
- A live entry creates or updates a hooks-only agent with its `sessionId`,
  `cwd`, `name` and `status`.
- A hooks-only agent can therefore exist before its first hook.

**Binding.** The binding is reconciled on every hook and on every scan.

- A session belongs to a herdr agent `herdr:<hash(socket)>:<pane>` if
  either of these holds:
  - the `X-Herdr-Pane` and `X-Herdr-Socket` headers name a pane whose herdr
    agent is `claude`;
  - herdr reports `agent_session.value == session_id`.
- Otherwise the session is hooks-only, `claude:<session_id>`, with
  `can_send: false`.
- When a binding appears later, a hooks-only agent migrates into the herdr
  agent. Its open request moves too and keeps its id.

**Filter** (P0 additional finding 2). `CLAUDE_CODE_ENTRYPOINT` is
inherited from the launching shell, so the value alone is not proof that
T3 owns the session.

- A payload is dropped as T3-owned only if both hold:
  - its entrypoint is in the ignore list (default `["sdk-ts"]`);
  - the session is **not** bound to a herdr pane and not listed in
    `~/.claude/sessions` with an interactive (`cli`) entrypoint.
- Payloads from subagents (`agent_id` present) are always dropped.

**Permission requests** (`tool_name` ≠ `AskUserQuestion`):

1. Open a `permission` request with the key `prompt_id`, not reusable.
   - `answer_path`: `hook`.
   - `title`: the tool name.
   - `detail`: the command, the file path, or compact JSON.
   - Options:
     - `allow` "Yes";
     - `deny` "No";
     - `allow_always` "Yes, always", only when `permission_suggestions`
       contains an `addRules` entry.
2. Hold the HTTP response.
3. On a claim, reply with one of these:
   - **allow:**
     `{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}`
   - **deny:** the same envelope with
     `"decision":{"behavior":"deny","message":"Denied from agent inbox"}`.
     Fixture: `tests/fixtures/hooks/claude/response-deny.json`.
   - **allow_always:** allow, plus
     `"updatedPermissions":[{"type":"addRules","rules":[{"toolName":…,"ruleContent":…}],"behavior":"allow","destination":"localSettings"}]`,
     taken from the suggestion.
     - Only `localSettings`, `projectSettings` and `userSettings` persist
       (P0 item 2).
     - This writes to the user's project settings, so the client must show
       the exact rule before sending.

   Once the reply is written and the connection completes, acd reports
   `by: "inbox"` on the next `PostToolUse` or `Stop` for the session, or
   when the fingerprint leaves the screen.
4. **The connection closes before acd replies.** With `http` hooks this
   means the user answered in the terminal, Claude was interrupted, or the
   process died.
   - Herdr-bound: if the fingerprint is still on screen, switch to
     `answer_path: "keys"`. Otherwise resolve `by: "terminal"`.
   - Hooks-only: resolve `by: "terminal"` if `PostToolUse` or `Stop`
     arrives within 30 s, otherwise `uncertain`.
5. **`permission_hold` expires.** acd replies empty. A herdr-bound request
   switches to `keys`; otherwise it resolves `lost`.
6. **Fingerprint.** Captured from herdr's detection snapshot within 2 s
   after the request opens.
   - It covers the option labels and the detail text.
   - Directory paths in the "always allow access to …" option are
     normalized.

**Questions** (`AskUserQuestion`):

- The `PreToolUse` and `PermissionRequest` events share `prompt_id`. Both
  reply empty at once.
- A `question` request keeps the full `questions` array, with `free_text`
  false. It is answerable only when all of these hold:
  - it has a single question;
  - `multiSelect` is false;
  - it is herdr-bound.

  Any other question is read-only.
- The dialog adds two rows that are not in the tool input: "Type
  something." and "Chat about this". These are not offered as options.
- A multi-question dialog shows a tab strip. Its fingerprint includes the
  strip, so it stays read-only.
- Answering sends the option's 1-based number key, guarded by the
  fingerprint.

**After a keystroke.** A key that arrives after the dialog has closed lands
in the agent's input box (P0 item 8b).

- The post-send check reads the prompt line.
- If the sent character appears there, the request resolves
  `by: "uncertain"`, with the client message "a stray keystroke may be in
  the input box".
- acd never sends a corrective backspace.

**Completion.**

- `Stop` sets `done` and fills `last_message`, at most 500 characters.
- `UserPromptSubmit` sets `working`.
- `SessionEnd`, or the entry disappearing from `~/.claude/sessions`, marks
  a hooks-only agent for removal.

### 6.3 Codex adapter (command shim + herdr)

**Hooks installed:**

| Hook | Shim timeout |
|---|---|
| `SessionStart` | 2 s |
| `UserPromptSubmit` | 2 s |
| `PermissionRequest` | 1 s |
| `PostToolUse` | 2 s |
| `Stop` | 2 s |

**Binding** (P0 additional finding 1). Codex runs hooks inside a shared
`codex app-server` daemon, so `HERDR_PANE_ID` and `HERDR_SOCKET_PATH` in the
hook environment can be stale. acd binds a Codex session to a herdr pane by,
in order of preference:

1. herdr reporting `agent_session` equal to the Codex `session_id`
   (herdr's own Codex integration reports it);
2. a single herdr `codex` pane with the same `cwd` whose detection snapshot
   shows the request's dialog;
3. the environment hint, but only when the pane's agent is `codex` and its
   `cwd` matches.

With no binding, the request is read-only.

**`PermissionRequest`.** It replies empty at once. The request opens with:

- key `(session_id, turn_id, sha256(tool_name + tool_input))`, reusable;
- options:
  - `allow` "Yes" (key `1`);
  - `allow_always` "Yes, don't ask again" (key `2`);
  - `deny` "No, and tell Codex what to do differently" (key `esc`);
- `detail`: the command, then the description.

**Answering.**

- acd waits up to 1 s for the fingerprint. The measured delay is
  35–118 ms (P0 item 5).
- Until the fingerprint appears, a claim gets `request_not_ready`.
- If the pane is too short, Codex collapses the dialog
  (`[… N lines] ctrl+a view all`). The command text is then missing, so the
  fingerprint cannot match and the request stays not ready (read-only).
  Clients show "enlarge the pane to answer here".

**Completion.** `PostToolUse` or `Stop` serve as resolution evidence, along
with the fingerprint leaving the screen.

### 6.4 herdr adapter

**Discovery.** Sockets at startup and every 30 s:

- `$HERDR_SOCKET_PATH`, or `~/.config/herdr/herdr.sock`;
- `~/.config/herdr/sessions/*/herdr.sock`;
- the configured extra paths.

Agent ids include a hash of the socket path.

**Compatibility.** `ping` must report a supported version: herdr
`>= 0.9.3, < 0.10` with protocol `22`.

**Sync** (P0 item 7). The stream carries no `state_change_seq` and has no
`events_lost` message.

1. Open one subscription connection for `pane.created`, `pane.closed` and
   `pane.agent_detected`, plus one `pane.agent_status_changed{pane_id}` per
   known pane. Add per-pane subscriptions as panes appear.
2. Build the initial state from `agent.list`, on a separate connection.
3. Treat every stream event as a hint. On an event, call `agent.get` for
   that pane and replace its state with the result. The order between the
   stream and the list therefore doesn't matter.
4. Reconcile with `agent.list`:
   - on reconnect;
   - every 5 s while any agent is `blocked`;
   - every 30 s otherwise.

   A pane whose `state_change_seq` rose without an event counts as a missed
   change. A dropped connection always triggers a full resync.

**State mapping.** `blocked` maps to `needs_you`; the other states map
directly.

**Fallback.** A pane that stays `blocked` for 2 s with no structured request
opens a read-only `blocked_unparsed` request:

- key `(socket, pane, fingerprint)`, reusable;
- `raw_screen` at most 40 lines.

**Follow-ups.**

- `agent.send` uses `agent.prompt`.
- Text containing control characters (except `\n` and `\t`), Unicode
  format characters (bidi overrides, U+2028/2029) or more than 16 KiB is
  rejected with `bad_request`.

**Seen state** (P0 item 4). herdr has no mark-seen method.

- acd keeps its own seen flag (P1).
- acd never calls `agent.focus`, because that moves the user's herdr UI.

### 6.5 T3 adapter

**Connection** (P0 item 3, from source).

- `POST /api/auth/websocket-ticket` with `Authorization: Bearer <t3.token>`
  returns a ticket valid for 5 minutes. acd requests a fresh ticket for
  every connect.
- The WebSocket URL is
  `ws://<host>:<port>/ws?orchestrationProtocol=2&wsTicket=<ticket>`.
- Framing is effect/rpc JSON.
- The token comes from
  `t3 auth session issue --scope orchestration:read --scope orchestration:operate`.

**Contract.**

- Hand-written Rust types cover `subscribeShell`, `subscribeThread` and
  `dispatchCommand` (`message.dispatch` and `runtime-request.respond`).
- Subscriptions resume with `afterSequence`.
- The T3 version is pinned. Unknown fields are ignored. An unknown status
  maps to `unknown`, and an unknown request kind becomes read-only.

**State mapping** is unchanged:

| T3 status | acd state |
|---|---|
| `running`, `starting`, `preparing`, `queued` | `working` |
| `waiting` | `needs_you` |
| `completed` | `done` |
| `failed`, `interrupted` | `done` with `failed: true` |
| `idle`, `cancelled`, `rolled_back` | `idle` |

**Requests.**

- A waiting thread's shell entry carries `pendingRuntimeRequest {id, kind}`.
- The detail comes from the `turnItems` of type `approval_request` or
  `user_input_request` (same `requestId`) in `subscribeThread`.
- The key is the request id, not reusable.
- Approval options come from `options[].decision`. If there are none, acd
  falls back to `accept`, `acceptForSession` and `decline`. Effects:
  - `accept` → allow;
  - `acceptAlways` and `acceptForSession` → allow_always;
  - `decline` and `cancel` → deny.
- Questions keep their full structure.

**Answers.**

- Answers use `runtime-request.respond` with `decision`, or with `answers`
  as `Record<questionId, string | string[]>`. The value is
  `option.value ?? option.label`.
- Each claim uses one `commandId`.
- T3 rejects stale answers ("Runtime request … is resolved") and serializes
  dispatch per thread.
- A T3 acknowledgement counts as delivery evidence (`by: "inbox"`).

**Follow-ups.** `message.dispatch`.

## 7. Protocol acd/1

### 7.1 Transport and security

**Binding.**
- WebSocket on port 47800 by default.
- acd binds only to loopback and to addresses that the local Tailscale
  daemon reports as its own (`tailscale ip`, or the LocalAPI). Each such
  address must also fall in `100.64.0.0/10` or `fd7a:115c:a1e0::/48`.
- A configured address that fails either check is refused at startup.
- When Tailscale's addresses change, or Tailscale stays unreachable for
  5 minutes while a Tailscale address is bound, acd exits with code 75 and
  its supervisor restarts it. Clients see a new epoch.
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
- `acd token rotate client|hook` replaces a token. It closes the sessions
  that used the old token with code 4401 ("token rotated"), and cancels
  their commands, including commands detached from earlier disconnects:
  - a command that was not yet delivered is released;
  - a command that was mid-delivery resolves `uncertain`;
  - neither is ever re-delivered.

**Logs.**
- Logs never contain tokens.
- Request details and screen content are logged only at `debug` level.
  Even there they are redacted, using the same secret patterns as the hook
  payload sanitizer (API keys, bearer tokens, `KEY=value` environment
  assignments), and truncated to 4 KiB.

**Hook endpoint.** It is a separate HTTP listener on `127.0.0.1` only, using
`hook.token`.

**Local socket.** acd also serves `acd/1` on the Unix socket
`<config_dir>/acd.sock`, with mode 0600 inside the 0700 directory, using the
same hello and token authentication. Local tools such as `acd status` use
it, so a process squatting the TCP port can never receive the token.

**Limits.**
- Ids (`id`, `request_id`, `agent_id`, `client.id`) are at most 128 bytes.
- Answer and follow-up text is at most 64 KiB. Adapters may set lower
  limits (§6.4).
- At most 32 commands per session and 256 daemon-wide may be in flight.
  Excess commands get `bad_request`, rejected before the idempotency
  reservation.
- When the pre-authentication cap is reached, acd answers HTTP 503.

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
- **Ordering.** acd sends replayed events, or the snapshot, before any
  live event, in strictly increasing `seq` order with no gaps.
- **The client cursor** is the `seq` of the last snapshot or event it
  applied. `welcome.seq` is informational: the daemon's current `seq`
  when the connection was accepted. It never moves the cursor.

**Slow clients.** A client with more than 1000 queued events is closed with
code 4408.

**Idempotency.**
- Command ids are scoped to `client.id`.
- **Reservation.** Before running a command, acd atomically reserves
  `(client.id, id)` along with a hash of the payload.
- **Retries:**
  - A retry with the same id and payload while the command is still
    running waits for that run's result. It never starts a second run.
  - Once the command finishes, the same retry returns the cached result.
  - The same id with a different payload returns `duplicate_mismatch`.
- **Retention.** Reservations and results are kept for 15 minutes after
  the command finishes.
- **Pre-delivery errors are not cached.** If a command fails before
  anything is delivered (`request_not_ready`, `request_busy`,
  `source_unavailable`), its reservation is released, so the same id can
  be retried.

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
  automatically for up to 3 s. The retries reuse the command id, which
  works because pre-delivery errors are not cached (§7.4).

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

## 11. Open items

P0 (`docs/research/2026-10-07-p0-checks.md`, fixtures in `tests/fixtures/`)
settled items 2–9 of revision 3. Their results are folded into §5.1 and §6.
The items below are still open. Each is a bounded check that must be done
before the work that depends on it.

1. **Deferred past v1:** free text in Claude `AskUserQuestion` on the
   `keys` path.
2. **Claude `http` hooks, live (blocks P3).** Confirm these behaviours of
   http hooks on Claude Code 2.1.292 or later:
   - header env interpolation with `allowedEnvVars`;
   - the held connection closing on a terminal answer;
   - a lingering held hook when another hook decides first.

   The spike observed the close only with an http PermissionRequest hook.
3. **T3, live (blocks P4).** Confirm against a live server:
   - whether a direct `Authorization: Bearer` header works on the upgrade;
   - the effect/rpc envelope;
   - replay retention for `afterSequence`;
   - per-RPC scopes;
   - how `prompt` is filled for commands.

   Record fixtures from a throwaway T3 project.
4. **Codex stale environment (P3).** Reproduce the stale `HERDR_*` values
   with a freshly restarted `codex app-server`, and confirm that herdr's
   Codex integration reports `agent_session`.
5. **herdr `done` → `idle`.** Check whether anything other than focus marks
   a pane seen (informational).
6. **Upstream ask, not blocking.** Propose a conditional-input method to
   herdr, so the `keys` path becomes atomic.

## 12. Error handling

| Failure | Behavior |
|---|---|
| acd down or the hook endpoint unreachable | Claude's http hook fails open, and the Codex shim replies empty within about 1 s. The normal dialog shows. |
| A held hook's connection closes early | Reconcile as in §6.2, step 4: switch to `keys` if the dialog is still on screen, otherwise resolve `terminal`. Hooks-only agents resolve `terminal` on later evidence, otherwise `uncertain`. |
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
- real HTTP from a fake Claude http hook and through the Codex shim, including connection close and timeout boundaries.

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
4. **Answered elsewhere.**
   - Answering any of the above in the terminal or in T3 first resolves the
     card on every client within 2 s.
   - **`hook` and `t3` requests:** a later inbox answer gets
     `request_gone`, and the agent applies only the first answer.
   - **`keys` requests:** if the terminal answer lands before acd's guard
     check, the inbox answer gets `request_gone` or `screen_mismatch`. If
     it lands between the guard check and the keystroke (the race accepted
     in §4), acd sends at most one keystroke, never redelivers, and reports
     `uncertain` when it cannot confirm the outcome.
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
| 12 | Deny envelope, competing answer paths | Deny envelope corrected; a recorded fixture is pending in §11.2. Exclusive paths (§5.1). |
| 13 | More assumptions to verify | §11, items 3 and 6–9. herdr range bounded to `< 0.10`. |

**Round 2, Sol, 2026-10-07.** Seven round-1 items were resolved and six
partly resolved. It found four new blocking items and three non-blocking
items. All were accepted.

| Item | Resolution |
|---|---|
| R1 #1 and new #4: the race acceptance criteria could not be met | §11.8 and §14.4 now separate `hook`/`t3`, which are atomic, from `keys`, which is guarded, sends at most one keystroke, never retries, and reports `uncertain`. |
| R1 #3 and new #1: a returning dialog was deduplicated forever | Per-key occurrence generations (§5.1). |
| R1 #4: replay ordering and cursor | Ordering rule and cursor definition (§7.4). |
| R1 #5 and new #2: concurrent retries | Atomic reservation before dispatch; retries attach to the running command (§7.4). |
| R1 #10: log redaction and Tailscale ownership | Redaction at debug level; addresses must be reported by `tailscale ip` (§7.1). |
| R1 #12: §15 overstated verification | Wording corrected. |
| New #3: free text vs invariant 3 | Free text on the `keys` path is deferred past v1 (§6.2, §11.1). |
| Non-blocking: readiness retries vs the cache | Pre-delivery errors are not cached (§7.4, §8). |
| Non-blocking: hooks-only confirmation could wait forever | 30 s bound, then `uncertain` (§5.1). |
| Non-blocking: "verified" wording | Changed to "pending verification" throughout. |

**Round 3, Sol, 2026-10-07.** All round-2 items are resolved. No new
blocking contradictions were found. The independent review is complete.

**Revision 4, 2026-10-07.** This revision folds in:

- the P0 verification results (`docs/research/2026-10-07-p0-checks.md`);
- the P1 review rulings (agent-scoped keys, non-reusable key tombstones, no
  relabelling of terminal answers as inbox, the local Unix socket, limits,
  and revoking older-generation commands on rotation).

The main design changes:

- Claude uses native `http` hooks with environment headers, not a shim.
- Codex binding no longer trusts the herdr environment values.
- The T3 filter keeps sessions bound to a herdr pane.
- herdr sync treats events as hints and reconciles periodically.
- The T3 connection uses a WebSocket ticket.
