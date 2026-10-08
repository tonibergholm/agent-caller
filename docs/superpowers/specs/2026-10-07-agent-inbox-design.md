# Agent inbox v1: design spec

Date: 2026-10-07
Status: revision 4.4. It folds in the P0 findings, the P1 review rulings,
Sol rounds 4 and 5, and gates 3 and 6 (see §15).
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
     - herdr has no conditional input, so a terminal answer, or a new
       dialog replacing the checked one, can land between acd's screen
       check and its keystroke.
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
  - The result follows the correlated outcome evidence of §6.2 (step 3,
    Attribution). The fingerprint leaving the screen alone yields
    `uncertain`.
  - If the connection closes before acd replies, acd reconciles as
    described in §6.2.
  - **Hooks-only agents** have no screen to check. If acd has delivered an
    answer and no `PostToolUse`, `Stop` or new request arrives within 30 s,
    the request resolves `by: "uncertain"`.
- **`keys`.** After sending the key, acd polls the detection snapshot for
  up to 3 s, and checks the prompt line for a stray keystroke (§6.2).
  - The dialog closing (the fingerprint absent in two consecutive reads)
    is required, but it is not enough on its own.
  - The result is `by: "inbox"` only when the correlated outcome evidence
    of §6.2 or §6.3 is consistent with the sent option, and no stray
    keystroke is detected.
  - A contradicting outcome gives `terminal`. Anything else gives
    `uncertain`: clients show "check the terminal", and acd sends nothing
    further.
  - Codex's transient `working` state while hooks run is not evidence.
- **`t3`.** T3 acknowledges acd's command. T3 accepts one answer per
  request, so the acknowledgement is the outcome evidence (§6.5).
- **Terminal answer.**
  - For `keys`, `none` and herdr fallback requests: the fingerprint
    disappears from the screen without a claim from acd.
  - For `hook`: see §6.2.
- **`lost`.** The source can no longer observe the request. Examples: the
  hold expires on a hooks-only agent; the agent is removed.

**Attribution.** `by: "inbox"` (with the claimed `option_id`) means that
acd delivered the client's answer **and** the observed outcome is
consistent with it. Agents do not report which decider was used, so
another hook or the user making the same choice at the same time is
indistinguishable. It is also harmless, because the effect is the one the
client asked for. A contradicting outcome gives `terminal`, without
`option_id`. Missing evidence gives `uncertain`. A request that a client has
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
- Attribution follows §5.1:
  - `by: "inbox"` when the adapter delivered the answer and the observed
    outcome is consistent with it;
  - `by: "terminal"` when the outcome contradicts it, or when the dialog
    ended without any acd delivery;
  - `by: "uncertain"` when acd delivered but the evidence is
    insufficient.
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

**Token storage.** Claude's hook entries carry the hook token as a literal
header, so these rules apply:

- Entries go only into the **user** settings file,
  `~/.claude/settings.json`, never into project settings that could be
  committed.
- The installer refuses a symlinked settings file. It writes the file with
  mode 0600. That is the one exception to "keep the file's mode": a file
  holding a token is always 0600.
- Backups go to `~/.config/acd/backups/` (directory 0700, files 0600), never
  next to the settings file.
- Before writing, the installer checks whether the settings file is
  tracked by a Git repository. If it is, the installer refuses, telling
  the user the token would be committed, unless `--allow-tracked` is
  given.
- `acd token rotate hook`:
  1. writes the new token;
  2. rewrites every managed Claude header;
  3. reports any file it could not update.

  If every managed header was updated, the endpoint accepts the previous
  token for 10 more minutes, for hooks already in flight. If any update
  failed, rotation exits non-zero, and the endpoint keeps accepting the
  previous token until a later `acd hooks install` succeeds. `acd status`
  shows "hook token rotation incomplete" until then.

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
| `SessionStart` | — | 1 s |
| `UserPromptSubmit` | — | 1 s |
| `PermissionRequest` | — | `permission_hold` + 10 s (default 3600 s + 10 s) |
| `PreToolUse` | `AskUserQuestion` | 1 s |
| `PostToolUse` | — | 1 s |
| `Stop` | — | 1 s |
| `SessionEnd` | — | 1 s |

acd answers lifecycle hooks immediately, before doing any work: it records
the event and replies, and processing happens afterwards. A stalled acd
therefore delays a lifecycle hook by at most its 1 s timeout. The held
`PermissionRequest` is exempt from that bound, because the terminal dialog
stays usable while it waits (§4, invariant 1).

**Session discovery** (P0 item 9). Every 30 s and at startup, acd scans
`~/.claude/sessions/*.json` and never opens the `.key` files there.

- Each live entry is validated by its `pid` and `procStart`.
- A live entry creates or updates a hooks-only agent with its `sessionId`,
  `cwd`, `name` and `status`.
- A hooks-only agent can therefore exist before its first hook.
- Discovery is best-effort. An unreadable directory, an unparseable entry,
  or an entry whose `pid`/`procStart` cannot be verified (process metadata
  unreadable) is skipped and logged once. It never creates an agent. An unknown `status` value maps to
  `unknown`. A missing or failed scan never removes an agent that hooks
  keep alive.
- Discovery applies the same ownership filter as hooks (below), so the
  scan never creates a T3-owned session.

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

**Ownership** (P0 additional finding 2). `CLAUDE_CODE_ENTRYPOINT` is
inherited from the launching shell, so the value alone is not proof that
T3 owns the session. One rule decides ownership of a Claude `session_id`,
for hook payloads and for discovery alike, applied in this order:

1. **T3** owns it when the T3 adapter reports a thread whose provider
   session id equals `session_id`. Claude-adapter state for that session is
   dropped and the T3 thread is the only agent.
2. **herdr** owns it when the session is bound to a herdr `claude` pane
   (§6.2 Binding).
3. **Hooks** own it when the entrypoint is not in the ignore list (default
   `["sdk-ts"]`).
4. Otherwise ownership is **unknown**: an ignore-listed entrypoint with no
   binding. The session is not shown. If rule 1 or 2 later applies, it is
   re-evaluated.

Payloads from subagents (`agent_id` present) are always dropped. T3
exposes the provider session id through thread detail (§6.5).

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

   **Attribution** (§5.1: the outcome must be consistent with the answer).
   Writing the reply does not prove Claude used it, because another hook
   may already have decided (P0 items 6 and 8b).
   acd correlates the outcome with this request's `tool_name` and
   `tool_input`:
   - **allow:** a `PostToolUse` with the same tool and input gives
     `by: "inbox"`.
   - **deny:** a `Stop`, or the next `UserPromptSubmit`, with no matching
     `PostToolUse` gives `by: "inbox"`.
   - **Contradicting outcome** (acd allowed but the tool never ran, or acd
     denied but it ran): `by: "terminal"`, with no `option_id`.
   - **No correlatable evidence within 30 s:** `by: "uncertain"`.

   The fingerprint leaving the screen only says the dialog closed. It
   yields `uncertain` unless the correlated evidence above arrives.
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
   after the request opens. If no matching dialog is captured, the request
   has no `keys` fallback. When the hold ends, it resolves as in step 4 for
   hooks-only agents and never switches to `keys`.
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

- acd reads the prompt line just before sending, and again 300 ms, 1 s
  and 3 s after. The last read coincides with the end of the
  confirmation poll in §5.1.
- If the prompt line gained the sent character, the request resolves
  `by: "uncertain"`, with the client message "a stray keystroke may be in
  the input box".
- `esc` produces no character, so it gets no prompt check. For `esc`,
  attribution relies on correlated evidence only.
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
   (herdr's own Codex integration reports it; §11 item 4);
2. exactly one herdr `codex` pane, in the same `cwd`, whose detection
   snapshot shows the request's dialog.

The environment hint is used only to order candidates for checking. It
never selects among several candidates. If two or more panes could match
(for example two Codex panes in one `cwd` showing the same dialog), the
binding is ambiguous and the request stays read-only. With no binding, the
request is read-only.

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

1. Open one subscription connection for `pane.created`, `pane.closed`,
   `pane.agent_detected` and `pane.updated`, plus one
   `pane.agent_status_changed{pane_id}` per known pane. Add per-pane
   subscriptions as panes appear. Stream frames are named differently
   from subscriptions (gate 6): `pane_created`, `pane_closed`,
   `pane_agent_detected` (with `released: true` when the agent exits) and
   `pane_updated`, but `pane.agent_status_changed`. Any other frame is a
   resync hint.
2. Build the initial state from `agent.list`, on a separate connection.
3. Treat every stream event as a hint. On an event, call
   `agent.get {target: pane_id}` for that pane. Every `agent.list` and
   `agent.get` call happens serially on a single per-socket reconciler
   task, and results are applied in the order the calls were issued. A
   stale list can therefore never roll back a newer per-pane result.
   (Gate 6: `AgentInfo.revision` is per pane but counts metadata changes
   such as title, cwd and detection, not status changes, so it cannot
   order state results. `state_change_seq` is server-wide.)
3a. Install a `pane.agent_status_changed` subscription for every pane
   found by any means: event, initial list or periodic list.
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

**Agents shown.** Only panes with a detected agent (`agent.list`). A plain
shell is not listed, and `agent.get` on it returns `agent_not_found`.
Agent trust dialogs (Claude, Codex) also report `blocked` and get the
fallback.

**Follow-ups.**

- `agent.send` uses `agent.prompt`. herdr refuses a blocked agent with
  `agent_blocked`, which acd reports as `agent_busy`.
- Text is rejected with `bad_request` when it:
  - contains control characters other than `\n` and `\t`;
  - contains bidi controls (U+061C, U+200E, U+200F, U+202A–U+202E,
    U+2066–U+2069) or U+2028/U+2029;
  - is longer than 16 KiB.

  Other format characters, such as ZWJ, ZWNJ and variation selectors,
  are allowed because ordinary emoji use them.

**Seen state** (P0 item 4). herdr has no mark-seen method.

- acd keeps its own seen flag (P1).
- acd never calls `agent.focus`, because that moves the user's herdr UI.

### 6.5 T3 adapter

Evidence: gate 3, a live check on T3 `v0.0.46-nightly.20261008.2801`
(`docs/research/2026-10-08-t3-gate3.md`, fixtures in `tests/fixtures/t3/`).

**Connection.**

- The WebSocket URL is
  `ws://<host>:<port>/ws?orchestrationProtocol=2`. A wrong or missing
  protocol gets HTTP 426.
- acd sends `Authorization: Bearer <t3.token>` on the upgrade, and re-reads
  `t3.token` on every connect. T3 also offers a 5-minute ticket from
  `POST /api/auth/websocket-ticket`; acd does not need it.
- The token comes from
  `t3 auth session issue --scope orchestration:read --scope orchestration:operate`
  (30 days by default). Write it with a private umask, for example
  `(umask 077; t3 auth session issue … > ~/.config/acd/t3.token)`.
- `t3.url` must point at loopback or a Tailscale address, because the token
  travels over plain `ws://`.

**Framing** (effect/rpc JSON, one object or an array per text frame).

- Requests: `{"_tag":"Request","id","tag","payload","headers":[]}`.
- Streams reply with `Chunk {requestId, values}`. **The client must Ack
  every Chunk** (`{"_tag":"Ack","requestId"}`), or no further chunks
  arrive.
- Calls and streams end with `Exit`: `Success {value}`, or `Failure` with
  causes `Fail {error}` (typed), `Die {defect}` (decode errors, unknown
  tags, bad literals) or `Interrupt`.
- `Ping` gets `Pong`. The server never pings, so acd does. A server
  shutdown closes sockets with code 1001.

**Contract.**

- Hand-written Rust types cover `subscribeShell`, `subscribeThread` and
  `dispatchCommand` (`message.dispatch` and `runtime-request.respond`).
- The T3 version is pinned. Unknown fields, item kinds and event types are
  ignored. An unknown status maps to `unknown`.
- Required scopes: `orchestration:read` for subscriptions,
  `orchestration:operate` for `dispatchCommand`. A missing scope gives
  `Fail EnvironmentAuthorizationError`, and the source becomes `degraded`.

**Sync.**

- The shell stream lists threads with their status and
  `pendingRuntimeRequest {id, kind}`. Request detail and the provider
  session id exist only in a thread stream.
- Thread streams come in three classes. **Request streams** are
  mandatory and uncapped: every thread with a pending request, or one acd
  still shows. **Working streams** cover working threads, at most 32.
  **One discovery stream** at a time learns session ids, for working
  threads over the cap and for recently active ones, taking turns, with a
  null id retried at most every 60 s. A stream that does not synchronize
  within 15 s is dropped and retried later. Releasing a stream drops that
  thread's detail.
- Resume uses `afterSequence`. Shell and thread events share one global
  sequence. Resume never fails: past T3's replay limits, the server sends a
  snapshot instead.
- A shell snapshot that carries `resolvedRepositoryIdentityRoots` holds
  project metadata only (`threads: []`). It arrives on every resume and
  mid-stream. It never replaces thread state and never moves the cursor.
  Any other snapshot replaces state. Events at or below the cursor are
  dropped.
- A dropped connection makes the source `degraded` and T3 agents
  `unknown`. acd reconnects with backoff and resumes.

**State mapping.** An agent is `needs_you` exactly when its shell entry has
`pendingRuntimeRequest != null`. While a request is pending the status
stays `running`. `waiting` without a request is the 140–200 ms drain after
an answer. Otherwise:

| T3 status | acd state |
|---|---|
| `running`, `starting`, `preparing`, `queued`, `waiting` | `working` |
| `completed` | `done` |
| `failed`, `interrupted` | `done` with `failed: true` |
| `idle`, `cancelled`, `rolled_back` | `idle` |

**Requests.**

- The shell's `pendingRuntimeRequest.id` decides which request is shown.
  The thread stream decides when it ends: `runtimeRequests[].status`
  `resolved` ends it `terminal` (unless acd answered it), and `cancelled`
  ends it `lost`.
- The key is the runtime request id, not reusable.
- An approval is the `approval_request` turn item with the same
  `requestId`. Neither provider sends `options[]`, so acd always offers
  `accept` (allow), `acceptForSession` (allow_always) and `decline`
  (deny). Only `requestKind: "command"` is answerable in v1; other kinds
  are read-only.
- For Claude, `prompt` is the tool's description. The command is in the
  `command_execution` turn item with the same `nativeItemRef.nativeId`.
  The detail is the command, then the description when it differs. For
  Codex, `prompt` is already the command.
- A question is the `user_input_request` turn item. It keeps its full
  structure. For Claude, the question id is the question text, and options
  have no `value`.
- A pending request without a matching turn item is shown read-only.
- **A T3 restart cancels a pending request.** It becomes `cancelled` with
  `responseCapability: not_resumable`, and acd resolves it `lost`.

**Answers.**

- `runtime-request.respond` with `decision` (one of `accept`,
  `acceptForSession`, `acceptAlways`, `decline`, `cancel`), or with
  `answers: {<question id>: option.value ?? option.label}`.
- T3 accepts one answer per request. A second respond fails with
  "Runtime request … is resolved." (or "is cancelled.", "was not found.").
  Only a failure with that single cause counts; a failure with several
  causes is `uncertain`.
  acd reports that as `request_gone` and resolves the request `terminal`
  (or `lost`).
- T3 does **not** compare payloads when a `commandId` is reused: it returns
  the stored receipt. acd therefore sends a fresh `commandId` with every
  dispatch, never derived from a client command id, and relies on its own
  idempotency cache (§7.4) for retries and `duplicate_mismatch`.
- The `Success` acknowledgement of acd's respond is the outcome evidence:
  the request resolves `by: "inbox"`. This rests on T3 accepting exactly
  one of two *concurrent* responds, which P4 must verify live before merge
  (gate 3 only showed the sequential case).
- A reply lost after the frame was written, or a send error once the
  write has started, is `uncertain`. acd never resends, so each acd
  command makes at most one dispatch attempt within the idempotency
  window (§7.4).

**Follow-ups.** `message.dispatch` with `createdBy: "user"`,
`creationSource: "web"`, a new `messageId`, `attachments: []`,
`deliveryIntent: "auto"` and `dispatchMode: {"type":"start_immediately"}`.
T3 queues a message sent while a run is active. Text follows the §6.4
rules.

**Ownership.** The provider session id is
`providerThreads[].nativeThreadRef.nativeId` of the provider thread whose
id equals `thread.activeProviderThreadId`. acd publishes it as soon as
the shell and the thread detail (a snapshot or a live event) name the
same active provider thread and the native id is set, without waiting for
the stream's `synchronized` marker. It removes it only on identity evidence:
another active provider thread, a null or different native id, or the
thread's removal. A stalled stream is not evidence. It
equals the Claude Code
session id and the Codex rollout id, is stable across runs, and is `null`
until the first turn. The adapter publishes a map from session id to T3
agent for §6.2 rule 1. T3's Claude sessions report the entrypoint
`sdk-ts`.

## 7. Protocol acd/1

### 7.1 Transport and security

**Binding.**
- WebSocket on port 47800 by default.
- TCP listeners serve WebSocket over TLS 1.3 only. The Unix socket serves
  plain WebSocket.
- acd binds only to loopback and to addresses that the local Tailscale
  daemon reports as its own (`tailscale ip`, or the LocalAPI). Each such
  address must also fall in `100.64.0.0/10` or `fd7a:115c:a1e0::/48`.
- A configured address that fails either check is refused at startup.
- When Tailscale's addresses change, or Tailscale stays unreachable for
  5 minutes while a Tailscale address is bound, acd exits with code 75 and
  its supervisor restarts it. Clients see a new epoch.
- When the Mac app connects to a MagicDNS name, it checks that the name
  resolved to an address in these ranges before sending `hello`.
- The address range check is not authentication; the TLS certificate pin is.
  `acd pair` prints a pairing line containing the pin and client token. TCP
  clients verify that pin; there is no plaintext fallback. `acd tls rotate`
  changes the certificate for new sessions, keeps existing sessions open,
  and leaves the client token unchanged. Pair every app again.

**Pre-authentication stages.**

| Stage | Bound |
|---|---|
| A. TCP accept → TLS finished | At most 16 handshakes at once. 5 s each, then the socket is dropped. |
| B. TLS finished → HTTP upgrade request complete | Covered by the 10 s pre-authentication deadline; silent or trickling HTTP clients are cut when it passes. |
| C. Upgrade → `hello` authenticated | At most 8 at once; `hello` within 5 s. |
| A + B + C | At most 32 unauthenticated TCP connections daemon-wide across all TCP listeners. Each must authenticate within 10 s of TCP accept. |

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

**Decided in P5** (docs/superpowers/plans/2026-10-08-p5-mac-app.md):
- Pair with `acd pair` and clipboard import; remote machines use pinned TLS (D1, D6).
- The local machine connects over `acd.sock` (D2).
- `⏎` submits only the reply field, with IME and modifier guards (D10).
- The countdown policy uses atomic `Esc` and connection generations (D9).
- Readiness and liveness are distinct; cards are actionable only when ready (D11).
- Notifications contain only the tool name (D12).
- Duplicate machine entries are identified and suppressed (D14).
- The default hotkey is Control+Option+Space; the panel is non-activating and the app is `LSUIElement` (D15).
- The IME guard is `isComposing || keyCode === 229` (D10).
- macOS notifications use `mac-notification-sys` directly and unique ids (D12).

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
- Opened by a global hotkey, by default ⌃⌥Space (Control+Option+Space). ⌥Space
  is commonly taken by launchers such as Raycast and Alfred. It is a non-activating
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
- `acd pair [--host H]`
- `acd tls rotate`

The config directory also contains `tls.pem` (the private key and matching
certificate, mode 0600) and `tls.lock` (the cross-process publication lock).

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
The gates below are still open. Each one blocks the plan named beside it
until it has been checked and the spec updated.

1. **Deferred past v1:** free text in Claude `AskUserQuestion` on the
   `keys` path.
2. **Claude `http` hooks, live (blocks P3).** On Claude Code 2.1.292 or
   later, confirm:
   - header env interpolation with `allowedEnvVars`;
   - the held connection closing on a terminal answer;
   - a held hook left open while another hook decides;
   - an acd reply written after another hook decided is ignored, and the
     correlated outcome evidence of §6.2 gives `terminal` when the
     decisions differ;
   - `allow_always` with `addRules`/`localSettings` persists across a new
     session (re-confirms P0 item 2);
   - a stalled endpoint bounded by the 1 s lifecycle timeout.
3. **T3: resolved 2026-10-08** (`docs/research/2026-10-08-t3-gate3.md`).
   Auth, framing, resume, scopes, prompts and the provider session id are
   folded into §6.5. Still unverified: multi-select questions, the
   `acceptForSession` and `acceptAlways` decisions, the statuses
   `preparing`, `queued`, `failed`, `interrupted` and `rolled_back`, replay
   past T3's limits, and concurrent responds. The P4 smoke test checks
   `acceptForSession`.
4. **Codex binding (blocks P3):**
   - herdr's Codex integration reports `agent_session`;
   - the stale `HERDR_*` reproduction with a fresh `codex app-server`;
   - two Codex panes in one cwd stay read-only.
5. **Claude session registry (blocks P3):** observed `status` values, and
   the behaviour when entries are unreadable.
6. **herdr ordering: resolved 2026-10-08**
   (`docs/research/2026-10-08-herdr-gate6.md`). `revision` exists per pane
   but does not move on status changes, so issue-order application on the
   single reconciler is the rule (§6.4 step 3).
7. **Hook token rotation (blocks P3):** rotate while Claude sessions run,
   and confirm the old-token grace window and the failure report.
8. **Upstream ask, not blocking.** Propose a conditional-input method to
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
- T3 fixtures, recorded live in gate 3;
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

**P5 Mac app manual checklist** (run from the production bundle; record each
result in the PR description):
1. With Terminal frontmost, `Control+Option+Space` opens the panel while Terminal stays active. J/K/1–9/Esc reach the panel; Esc hides it and typing returns to Terminal.
2. In Settings, change the hotkey; an unavailable hotkey leaves the old key working.
3. In the reply field, type English and Japanese. The Enter that commits IME composition does not send; `⏎` starts the countdown and Esc cancels it.
4. Pick Yes and immediately press Esc; acd's debug log has no `request.answer`.
5. With real herdr agents, check amber with a count while blocked, blue while working and green when done.
6. While the panel is hidden, create a request and verify only the tool name appears in one notification. With the panel visible, verify no notification.
7. Create repeated requests and verify all repeated notifications show.
8. Run `acd pair` on a remote machine, copy its output and use Add from clipboard; it connects. Relaunch and verify it reconnects without pairing. Record any Keychain prompt after rebuild.
9. Verify a Tailscale MagicDNS host connects and an address outside the Tailscale ranges is rejected before connection.
10. Replace the remote daemon with `openssl s_server` using a fresh key; the app says "Server identity changed. Re-pair this machine." and the server logs no application data.
11. Rotate the remote TLS key; the open session continues, reconnect reports identity changed, and re-pairing fixes it.
12. Rotate this Mac's client token; the local machine reconnects. Rotate a remote token; the app reports `auth_failed` until re-paired.
13. Stop and restart acd; cards grey with "offline since", then refresh after Syncing without a stuck spinner.
14. Import pairing from the clipboard and answer a real request end to end.
15. Use a token file owned by another user in a test config directory; the local machine reports "not owned by you".

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
6. **acd stopped or stalled.**
   - With acd stopped (connection refused), prompts behave as they do
     without agent-caller, and hooks add no noticeable delay.
   - With acd stalled (accepting connections but not replying), each
     lifecycle hook adds at most 1 s.
   - A held `PermissionRequest` never blocks the terminal dialog.
7. **Undo.** `Esc` within 2 s sends nothing.
8. **acd restart.** Restarting acd while requests are open leaves no agent
   stuck. Requests still on screen reappear.
9. **Hook install round trip.** Running `acd hooks install` twice and then
   `uninstall` leaves the hook configs semantically identical to before.
10. **Other agents.** An agent of another kind that herdr detects is shown
    with its state. When it is blocked, its raw screen is shown read-only.

## 15. Review log

**P5 Task 18, 2026-10-08.** Records the decided Mac app details in §8, adds the P5 production-bundle checklist to §13, and adds TLS-backed end-to-end Inbox/Core tests (docs/superpowers/plans/2026-10-08-p5-mac-app.md).

**Task 3, 2026-10-08.** Task 3 adds TLS 1.3 on TCP, atomic `tls.pem`, `acd pair` and `acd tls rotate`.

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

**Round 4, Sol (revision 4), 2026-10-07.** It found 6 blocking items, 3
non-blocking items and 1 question. All were accepted and fixed in place:

- Codex binding ambiguity now leaves the request read-only, and the
  environment hint can no longer select a pane.
- Inbox attribution for held Claude hooks is correlated with the tool and
  input.
- Hook tokens live only in user settings, at mode 0600, with private
  backups, and rotation has a grace window.
- One Claude ownership rule now covers both discovery and hooks.
- herdr reconciliation runs on a single task and is revision-ordered.
- The lifecycle hook timeout is 1 s and acceptance criterion 14.6 is split
  by case.
- Discovery degrades gracefully, the stray-keystroke check compares the
  prompt line before and after, and §11 gates now name the plan each one
  blocks.

**Round 5, Sol (revision 4.1), 2026-10-07.** B1, B4, B5, B6 and Q1 were
resolved. B2, B3, NB1, NB2 and NB3 were partly resolved, and one new
contradiction was found. Revision 4.2 fixes them:

- `inbox` now means "delivered and the outcome is consistent". The same
  rule applies to the `hook` and `keys` paths, which removes the §5.1
  versus §6.2 contradiction.
- The prompt check now extends to 3 s.
- The installer refuses Git-tracked settings files, and an incomplete
  hook-token rotation keeps the old token valid and is reported.
- Session entries whose process metadata can't be verified are skipped.
- The `allow_always` persistence check is added to the §11 item 2 gate.

**Round 6, Sol (revision 4.2), 2026-10-07.** All items were resolved
except one remaining contradiction: the §6 adapter contract still used the
old attribution rule. Revision 4.3 aligns it with §5.1.

**Gate 6, 2026-10-08.** A live check on herdr 0.9.3 settled §11 item 6
(`docs/research/2026-10-08-herdr-gate6.md`, fixtures in
`tests/fixtures/herdr/api/`). §6.4 now states the issue-order rule, the
frame names, the `target` parameter, which panes are shown, and the
`agent_blocked` refusal. Sol reviewed the P2 plan (9 blocking findings, all
accepted). §6.4 now lists the exact set of rejected follow-up characters.

**Gate 3, 2026-10-08.** A live check on T3 `v0.0.46-nightly.20261008.2801`
settled §11 item 3 (`docs/research/2026-10-08-t3-gate3.md`, fixtures in
`tests/fixtures/t3/`). It contradicted the old §6.5 in nine places, and
§6.5 is rewritten: needs-you comes from `pendingRuntimeRequest`, not
`waiting`; every Chunk needs an Ack; resume never fails and project-only
snapshots must not replace state; T3 does not check payloads on a reused
`commandId`; a restart cancels a pending request; `message.dispatch` needs
more fields; the provider session id is in thread detail; Claude's command
comes from the sibling `command_execution` item; and neither provider sends
approval options. acd authenticates with a Bearer header on the upgrade
instead of a ticket, and an acknowledged T3 answer resolves `inbox`
(§5.1). Sol reviewed the P4 plan (7 blocking findings, all accepted). §6.5
now defines the three thread-stream classes, the at-most-one dispatch
guarantee, and the concurrent-respond gate. Round 2 (4 blocking findings,
all accepted) added the agreement rule for session ownership, fair
discovery, a synchronization deadline for thread streams, and
conservative handling of multi-cause T3 failures.
