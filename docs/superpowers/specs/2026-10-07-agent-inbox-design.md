# Agent inbox v1: design spec

Date: 2026-10-07
Status: revision 5.0. It folds in the P0 findings, the P1 review rulings,
Sol rounds 4 and 5, gates 3 and 6, and the P3 gates 2, 4, 5 and 7 with the
P3 plan reviews (see §15).
Research: `docs/research/2026-10-07-landscape.md`, which includes the spike
log; `docs/research/2026-10-09-p3-gates-claude.md` (gates 2, 5, 7) and
`docs/research/2026-10-09-p3-gate4-codex.md` (gate 4).

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

  "Yes, always" (`allow_always`, ruling Q2) on a Claude hook permission:
  for `Bash` the rule is the exact command (`{toolName: "Bash",
  ruleContent: <command>}`, destination `localSettings`). One function builds
  it for display and for delivery; it is shown exactly, and it is offered
  only when the whole rule can be displayed (§6.2). Codex hook requests never
  offer it. T3 approvals have their own `acceptForSession` (§6.5).
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
| **Hook shim** | `acd hook codex …`. Codex hook configs call it, and it forwards the hook to acd over `hook.sock`. Claude uses native `http` hooks instead. |

## 4. Architecture

```
 Mac: Tauri menu bar app (status icon, floating panel, hotkey)
        │  acd/1 over WebSocket, one connection per machine, via Tailscale
        ▼
 acd (Rust daemon, one per machine; the laptop runs one too)
   ├─ hook endpoint (HTTP: 127.0.0.1 for Claude, hook.sock for Codex)
   │                 ◄── Claude http hooks / `acd hook codex` shim
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
| Claude hook | `(session_id, prompt_id, sha256(tool_name + tool_input))`. `prompt_id` alone is per user turn, so the digest separates the tools of one turn. The same key links the `PermissionRequest` and `PreToolUse` of one `AskUserQuestion`. | yes |
| Codex hook | `(session_id, turn_id, sha256(tool_name + tool_input))`. Outcome evidence matches `tool_input.command`, because `PostToolUse.tool_input` drops `description`. | yes |
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
- **Non-reusable keys** (T3 request ids) are
  tombstoned once their request resolves. They never open another
  request, even after the dialog is confirmed gone. The generation counter
  only applies to reusable keys.

**Hook occurrences (O1 to O7).** For hooks, the key alone does not decide
whether an event is a new occurrence; these rules do. They keep a delayed
event of an older request from resolving a newer one.

- **O1.** Each Claude tool `PermissionRequest` is a new occurrence, even if
  it is byte-identical to an earlier one: neither agent retries hooks, and
  Claude shows one permission dialog at a time per session. Before it
  opens, every older open hook-path occurrence of the session ends. A held
  one (no reply written) resolves `terminal` and its hold is released
  empty. One whose reply is written and awaits evidence resolves
  `uncertain`. A hold-ended, `keys` or read-only one resolves `terminal`.
  If parallel tool calls fire several hooks at once (live check 13), the
  older hold is released empty and that dialog falls back to the terminal.
- **O2.** `AskUserQuestion`: a `PreToolUse` opens the occurrence. A
  `PermissionRequest` with the same key while it is open and before
  `PostToolUse` merges into it. A second `PreToolUse` with the same key is
  a new occurrence.
- **O3.** A Codex `PermissionRequest` is a new occurrence. O1's ending rule
  applies to older ones.
- **O4 and O5.** A session has at most one open hook occurrence. Evidence
  applies only to it, only if the event arrived after the occurrence opened
  and, for a delivered answer, after the delivery. Older events, events
  after the end and events of other sessions do nothing.
- **O6.** Phases: `Held` (hold open, nothing written), then `Delivered`
  (reply written) or `HoldEnded` (closed or expired, nothing written), then
  ended. Separately, `Keys` (a keys target is registered) and `ReadOnly`.
- **O7.** Claude sends no invocation id that a later `PostToolUse` repeats,
  so a delayed outcome of an older occurrence looks the same as the outcome
  of a newer one with the same evidence identity (tool name and digest for
  Claude, tool name and command for Codex, question text for a question).
  When an occurrence ends without having seen its identity evidence, the
  session keeps an *unconsumed identity* for it, until the first matching
  event, the next turn end (`Stop`, `UserPromptSubmit`), or 10 minutes. At
  most 8 are kept per session; beyond that the session shadows every new
  occurrence until its next turn end. A new occurrence with a matching
  unconsumed identity is *shadowed*: identity evidence is ignored for it,
  so it ends only through its hold, its screen reads, a turn end, a newer
  occurrence or the evidence window. After a delivered answer, a turn end
  resolves a shadowed request `uncertain`. The rule can turn a correct
  attribution into `uncertain`, never into a wrong one.
- Superseding a request that awaits evidence resolves it `uncertain` at
  once. acd does not keep watching for late evidence.

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
  - its T3 thread is archived or deleted, or is hidden (§6.5, "Visibility");
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

acd's hook endpoint is one HTTP/1.1 router on two listeners: TCP
`127.0.0.1:<hook_port>` for Claude, and the Unix socket
`<config_dir>/hook.sock` (mode 0600) for Codex. It accepts
`POST /hook/<agent>/<event>`, with `agent` either `claude` or `codex`, and
the header `X-Acd-Token`. Any other method, path or agent, a malformed
path, and a wrong or empty token get 401. The query string is ignored. The
token is compared in constant time against the current and the previous
hook credential (T2), and each request is tagged with the credential
generation that matched.

**Endpoint bounds.**
- At most 128 open connections per listener; the listener waits beyond
  that.
- Body at most 1 MiB (413), read within 5 s. A body that is not a JSON
  object gets 200 with an empty body.
- Lifecycle events go into a mailbox of at most 512 events and 16 MiB of
  bodies. When it is full, the hook gets an empty reply and the event is
  dropped and counted.
- At most 64 holds (A1). A hold is admitted only with a hold permit and two
  reserved slots in the control channel, one for `Held` and one for its
  `HoldClosed` or `HoldExpired` (R10). The permit stays until the adapter
  is done with the hold, so a disconnected client keeps its permit until its
  queued messages are consumed. The control channel therefore holds at most
  2 × 64 messages, and a close or expiry is never dropped. A request
  without a permit and both slots gets an empty reply at once.
- Header hints (`X-Herdr-Pane`, `X-Herdr-Socket`, `X-Claude-Entrypoint`) are
  accepted only when non-empty, at most 1024 bytes and free of control
  characters. `X-Herdr-Socket` is a lookup key only; acd never opens a path
  taken from a header.
- `session_id`, `prompt_id`, `turn_id` and pane ids are 1 to 128 bytes
  without control characters.
- Rejected requests are logged at `warn`, at most once a minute per remote.
  The remote is an opaque id (the first 8 hex digits of the SHA-256 of the
  pane hint); the table holds at most 256 entries. A header value or token
  is never logged.
- A taken `hook_port`, or any other hook listener bind failure, is fatal
  at startup (D13).

The listeners are served by the daemon. `hook.sock` is created with its
permissions set before it serves, and startup is serialized by an exclusive
daemon lock, `<config_dir>/acd.lock`, which the daemon holds for its whole
life; a second daemon that finds it held refuses to start. A pre-existing path is replaced only on a definite stale
condition: a socket owned by the effective uid whose connect fails with
`ECONNREFUSED`. A live socket, another file type or owner, or any other
error refuses to start. Shutdown removes the socket only if it is still the
one this process created (same inode). `acd.sock` follows the same rules.

The two agents reach it in different ways (P0 item 8, the additional
findings and gate 2):

- **Claude: native `http` hooks, no shim.**
  - Each hook entry has `type: "http"`, a `url` on `127.0.0.1:<hook_port>`,
    the header `X-Acd-Token` (the literal token value), and these
    environment headers:
    - `X-Herdr-Pane: $HERDR_PANE_ID`
    - `X-Herdr-Socket: $HERDR_SOCKET_PATH`
    - `X-Claude-Entrypoint: $CLAUDE_CODE_ENTRYPOINT`

    The entry lists those variables in `allowedEnvVars`. A variable that is
    not listed becomes an empty string, and acd treats an empty header as
    absent.
  - Claude snapshots the hook configuration when a session starts. Editing
    the file does not change a running session; `/clear`, a restart and
    (inferred, not yet run) resume read it again.
  - Claude itself fails open:
    - a connection error, a non-2xx reply or a timeout lets execution
      continue;
    - a malformed `decision` falls back to the dialog (P0 item 2).
  - **Terminal answers and the held connection** (gate 2b). The held
    connection closes about 15 ms after a terminal **deny** or **Esc**. It
    does **not** close after a terminal **allow**: it stays open until the
    hook timeout (`permission_hold` + 10 s). For a terminal allow acd learns
    of the answer from `PostToolUse` or from the dialog leaving the screen
    (§6.2). Once acd knows a request ended, it replies empty to the held
    hook.
  - Claude never sends `SessionStart` over `http` (gate 2). acd does not
    install it.
  - Under a lifecycle timeout of 1 s, a stalled acd delays `UserPromptSubmit`
    and `Stop` by 1 s each. Claude shows `... timed out after 1s - output
    discarded`. A held `PermissionRequest` honours its configured timeout and
    the dialog stays usable throughout.
- **Codex: the command shim `acd hook codex <event>`.** Codex supports only
  `command` hooks. The shim talks to `hook.sock` only and never falls back
  to TCP (amendment A1).
  - It reads stdin, up to 1 MiB, and adds `CODEX_*` and `HERDR_*`
    environment values as hints. They are unreliable; acd never binds with
    them (§6.3).
  - Before connecting, it resolves the config dir once to a canonical path
    and checks that path from `/` down to the leaf with `lstat`, stopping at
    the first failure. Each component must be a real directory owned by root
    or the effective uid and not group- or other-writable unless sticky. The
    config dir must be owned by the effective uid with mode 0700, and
    `hook.sock` must be a socket (not a symlink) owned by the effective uid
    with mode 0600. It reads the token from `hook-tokens.json` in that
    directory and connects to that same path.
  - It requires a reply with exactly the declared `Content-Length`; chunked
    replies, malformed or duplicate lengths, and short bodies are rejected.
    It stops reading once the declared length has arrived.
  - One deadline covers stdin, the checks, connect, write and read:
    800 ms for `PermissionRequest`, 1500 ms for the other events.
  - It prints acd's body and exits 0. Any failure, including a panic,
    prints nothing on stdout or stderr and still exits 0, which restores
    Codex's own dialog.

**Installation.** `acd hooks install claude|codex`:

- is idempotent. An entry is *owned* only when it parses exactly as acd's
  own entry for that event (D22): for Claude an `http` entry to
  `127.0.0.1:<hook_port>` with path `/hook/claude/<event>` and the query
  `marker=acd-managed:v1`; for Codex a `command` entry
  `<exe> hook codex <Event> --config-dir <dir> acd-managed:v1`. An entry
  that mentions the marker but does not parse is *suspicious*: install and
  uninstall leave it alone and warn, and `acd token rotate hook` refuses
  while one exists. Every other entry is foreign;
- when it installs the Claude entries, prints the trust note of §9: the
  hook token and payloads go to a loopback port that another local process
  can take while acd is not running;
- installs these Claude events: `UserPromptSubmit`, `PermissionRequest`,
  `PreToolUse` (matcher `AskUserQuestion`), `PostToolUse`,
  `PostToolUseFailure`, `Stop`, `SessionEnd`; and these Codex events:
  `SessionStart`, `UserPromptSubmit`, `PermissionRequest`, `PostToolUse`,
  `Stop`;
- backs up the file before writing, writes atomically and keeps the file's
  mode;
- records its entries in `~/.config/acd/hooks-installed.json`, keyed by the
  canonical destination, so a changed `CLAUDE_CONFIG_DIR` creates a new
  record (D24);
- never edits or reorders entries it doesn't own. It edits the top-level
  object, the `hooks` object, each group and each group's `hooks` array as
  ordered raw values, so foreign keys and entries keep their exact bytes
  (D16).

**Settings writes.** Several writers can touch one file: acd's installer,
`acd token rotate hook`, and Claude itself.

- One acd writer at a time, under the lock `<config_dir>/hooks.lock`. The
  lock is taken by top-level entry points only and no path reacquires it
  (T6).
- The destination's parent directory is opened once with no-follow and
  checked to be owned by the user. Reads, staging, the compare and the
  rename all go through that descriptor.
- Order: read, edit, stage and fsync a temp file, back up, begin the journal,
  recheck Git, re-read the destination and compare it with the text edited
  from, rename. If the file changed in between, acd re-reads and retries,
  three times at most, then reports "kept changing".
- **Journal (J1 to J4).** One journal covers a settings change and its
  record update. `begin` never overwrites an existing journal. A failure
  before the rename removes the journal; a failure after it keeps it.
  Recovery runs at the start of every installer entry point, under the lock.
  If the settings are in the journal's `after` state, the record is applied;
  in the `before` state nothing changed. If someone else (typically Claude's
  own atomic rewrite) changed the file, acd reads it with the safe reader
  and rebuilds the record from the acd entries it holds, or drops the record
  when it holds none. If the file cannot be read safely, the journal is kept
  and marked as a conflict: every installer command and `acd token rotate
  hook` refuse, `acd hooks status` names the file, and the next command after
  the file is fixed recovers. A multi-destination token rewrite stops at the
  first failure that leaves a journal.
- `hook-tokens.json`, `hooks-installed.json` and `hooks-journal.json` are
  private structured files (F1): mode 0600, no symlink, owned by the user,
  at most 256 KiB, with the bound checked before writing so acd never
  writes a file it cannot read back.

**Token storage.** Claude's hook entries carry the hook token as a literal
header, so these rules apply:

- Entries go only into the **user** settings file,
  `$CLAUDE_CONFIG_DIR/settings.json` or `~/.claude/settings.json`, never
  into project settings that could be committed. Codex entries go into
  `$CODEX_HOME/hooks.json` or `~/.codex/hooks.json` and carry no token.
- The installer refuses a symlinked settings file. It writes the file with
  mode 0600. That is the one exception to "keep the file's mode": a file
  holding a token is always 0600, and a reinstall restores 0600 on a
  loosened file.
- Backups go to `~/.config/acd/backups/` (directory 0700, files 0600), never
  next to the settings file.
- Before every token-bearing write, the installer asks Git about the file.
  The answer is *tracked*, *untracked* or *unknown*. Tracked is refused,
  because the token would be committed, unless `--allow-tracked` is given.
  Unknown (Git cannot run or gives no clear answer) is refused unless
  `--allow-unknown-git` is given. Rotation runs the same check over every
  destination before committing and again for each file it writes.
- The tokens live in one atomic store, `<config_dir>/hook-tokens.json`
  (T1), which replaces the old `hook.token`: `acd init` and `acd run`
  migrate the old file into the store and remove it. The store holds the
  current credential, an optional previous credential, a credential
  generation, and the state of the last header update.
- **Credential freshness (R13).** The daemon checks tokens against an
  in-memory copy of the store. It reloads the store every 2 s, without
  taking `hooks.lock`, so a change made by the CLI (`acd token rotate hook`,
  a forced `acd init`) reaches the check within 2 s. A forced reset therefore
  leaves the token it dropped valid for up to 2 s; v1 accepts that. When the
  daemon retires the previous token itself, the commit also revokes it in
  memory at once. A previous token whose expiry has passed is rejected on
  each request, whether or not the store has been reloaded since. A reload
  that fails leaves the last good copy in use and shows the hooks source as
  degraded in `acd status`.
- `acd token rotate hook`:
  1. **Refuses, writing nothing**, when the current generation's headers are
     not `done` (repair with `acd hooks install claude`), when a previous
     token is still retained (unless `--force`, which cuts off the sessions
     still on it and says so), when a journal conflict exists, or when any
     recorded destination fails its precheck (read with the same no-follow
     pinned reader the writes use, then the Git and marker checks). A
     refusal leaves the store byte-identical;
  2. commits the new store (generation + 1, the old token as `previous` with
     an expiry 30 days out, headers `pending`);
  3. rewrites every managed Claude header to the new token, idempotently;
  4. records `done` or `failed` with the completion time read **after** the
     last write, and lists any file it could not update.

  A crash or failure at any point leaves a store that authenticates both
  tokens, and `acd hooks install claude` repairs the headers. `acd status`
  shows "hook token rotation incomplete: run `acd hooks install claude`"
  until then.
- **Retention (T4).** Sessions already running keep the token they started
  with, because they snapshot their headers (gate 7). A fixed grace period
  would end their hooks, so the previous token is accepted until it is
  retired, and retirement happens when either of these holds:
  - 30 days after the rotation (always honoured); or
  - the headers are `done` for the current generation, **and** a complete
    registry scan that *started* at least `REGISTRY_GRACE_MS` (5 s) after the
    header completion time and finished within the last two scan intervals
    shows no live session that started before that completion time and
    has not authenticated with the current generation since. Both times
    are measured after the writer lock is held. A session seen only on an
    older generation is not excluded. Each generation remembers at most 4096
    session ids and the daemon keeps 4 generations; overflow only keeps the
    previous token longer.

  Retirement is checked every 30 s against the whole store it evaluated and
  does nothing if any field of the store on disk differs (T5). A write
  failure is logged, shown in the hooks source status and leaves the
  previous token accepted. While the previous token is held, `acd status`
  shows "hook token rotation pending: N Claude sessions still use the
  previous token", or "waiting for a complete registry scan". After the
  token is retired, each rejected session is counted and shown as "N Claude
  session(s) use a retired hook token; restart them or run /clear in them".
  Those sessions show `hook error / HTTP 401` on lifecycle hooks, and their
  permission requests fall back to the terminal. The grace is an assumption
  until live check 14 (§11) measures how long Claude takes to write its
  registry entry; an underestimate can retire the previous token early.

Claude may rewrite `.claude/settings*.json` formatting when it persists a
rule (it replaces the file atomically, re-indents it and keeps its mode), so
ownership is matched by marker, never by byte layout.
`acd hooks uninstall` removes exactly the recorded entries, matched
semantically. `acd hooks status` lists the recorded destinations and any
journal conflict. The Claude settings file must be mode 0600, because it
holds the literal hook token. The installer enforces that.

**Coexisting permission hooks** (verified, P0 item 6 and gate 2c, 2d):

- Any decisive hook beats an empty reply, and deny beats allow.
- If another hook decides a Claude request that acd is holding, Claude does
  not wait for acd's held hook. It leaves acd's connection open and ignores
  any reply written afterwards, allow or deny.
- acd therefore resolves such requests through reconciliation:
  `PostToolUse`, `PostToolUseFailure`, `Stop`, or the dialog leaving the
  screen.
- Once acd knows the request ended, it replies empty to the held hook.

### 6.2 Claude adapter (http hooks)

**Hooks installed** (all `type: "http"`):

| Hook | Matcher | Hook `timeout` |
|---|---|---|
| `UserPromptSubmit` | — | 1 s |
| `PermissionRequest` | — | `permission_hold` + 10 s (default 3600 s + 10 s) |
| `PreToolUse` | `AskUserQuestion` | 1 s |
| `PostToolUse` | — | 1 s |
| `PostToolUseFailure` | — | 1 s |
| `Stop` | — | 1 s |
| `SessionEnd` | — | 1 s |

`SessionStart` is not installed: Claude sends no `http` request for it
(gate 2). A hooks-only agent therefore appears from the registry scan or from
its first hook. `PostToolUseFailure` counts as "the tool ran", like
`PostToolUse`; its payload has not been checked live (§11). `PermissionDenied`
is not installed.

acd answers lifecycle hooks immediately, before doing any work: it records
the event and replies, and processing happens afterwards. A stalled acd
therefore delays a lifecycle hook by at most its 1 s timeout. The held
`PermissionRequest` is exempt from that bound, because the terminal dialog
stays usable while it waits (§4, invariant 1).

**Session discovery** (P0 item 9, gate 5). Every 30 s and at startup, acd
scans `<claude config dir>/sessions/*.json` for every root it knows: the
install records' roots, the roots of uninstalled Claude installs whose
sessions may still hold the previous hook token, and the default root, read
again before each scan (D24). It never opens the `.key` files there.

- An uninstalled root stays known because sessions started before
  `acd hooks uninstall claude` keep the headers they read. When its record
  is removed, the install records keep the root with the token store's
  current generation (unknown if the store cannot be read: kept). The root
  is scanned only while the previous token is that generation or older.
  The entry leaves the records at the next record change after no accepted
  token is that old, or when a forced `acd init` resets the store.
- If the install records cannot be read, the root list is unknown and every
  scan is incomplete until they can.
- The registry fields used are `pid`, `sessionId`, `cwd`, `name`, `status`
  and `procStart`. `status` is `idle`, `busy` or `waiting` (a `waiting`
  entry also has `waitingFor`, for example `permission prompt`). An unknown
  `status` maps to `unknown`.
- Each entry is read with no-follow and an owner check, from a directory
  that passes the same check. A scan reads at most 4096 `<pid>.json`
  entries across the roots.
- Each live entry is validated by its `pid` and `procStart`. `procStart` is
  the output of `ps -o lstart=` in UTC, so it is compared in UTC. A process
  killed with `kill -9` leaves its entry behind, so the check is required.
- A live entry creates or updates a hooks-only agent with its `sessionId`,
  `cwd`, `name` and `status`. A hooks-only agent can therefore exist before
  its first hook.
- Discovery is best-effort. A vanished file is skipped. An entry that is
  unreadable, unparseable, has no `procStart`, or whose process metadata
  cannot be verified follows the **dead-pid rule**: if the process named by
  the file name is dead, the entry is skipped; if it is alive or cannot be
  checked, the scan is marked incomplete. A listing error, an unsafe
  directory, a missing root (an uninstalled root's deleted directory
  included: it cannot be shown to be empty) or hitting the entry cap also
  marks it incomplete. Each reason is logged once, without file contents,
  and none creates an agent. A missing or incomplete scan never removes an
  agent that hooks keep alive, and an incomplete scan is never used to retire the previous hook token (§6.1).
  Each scan carries its wall-clock start and end times for that purpose.
- Discovery applies the same ownership filter as hooks (below), so the
  scan never creates a T3-owned session.

**Binding.** The binding is reconciled on every hook and on every scan.

- A session belongs to a herdr agent `herdr:<hash(socket)>:<pane>` if
  either of these holds:
  - the `X-Herdr-Pane` and `X-Herdr-Socket` headers name a pane whose herdr
    agent is `claude`. The header socket path is canonicalized and matched
    against the sockets herdr knows, and the match is remembered across
    scans until the pane's incarnation changes or the pane is gone;
  - exactly one `claude` pane reports `agent_session.value == session_id`.
- **Conflicts bind nothing** (R9). If two panes report the session, or the
  header pane differs from a pane that reports it, acd does not prefer
  either. The session falls through to rule 3 or 4 below.
- Otherwise the session is hooks-only, `claude:<session_id>`, with
  `can_send: false`.
- When a binding appears later, a hooks-only agent migrates into the herdr
  agent. Its open request moves too and keeps its id. If the target agent
  does not exist yet, the hooks-only agent stays until it appears.
- Every pane has an *incarnation*, a counter bumped when herdr adds the pane
  as a new agent (first sight, or replacement after a close or an identity
  change). Keys targets and screen reads are bound to it.

**Ownership** (P0 additional finding 2). `CLAUDE_CODE_ENTRYPOINT` is
inherited from the launching shell, so the value alone is not proof that
T3 owns the session. One rule decides ownership of a Claude `session_id`,
for hook payloads and for discovery alike, applied in this order:

1. **T3** owns it when the T3 adapter's session map
   (`T3Adapter::sessions()`) reports a thread whose provider session id
   equals `session_id`. Claude-adapter state for that session is dropped and
   the T3 thread is the only agent. The decision is made once per event, and
   a T3-owned event reaches neither the request path nor the evidence hub.
2. **herdr** owns it when the session is bound to a herdr `claude` pane
   (§6.2 Binding).
3. **Hooks** own it when the entrypoint is not in the ignore list (default
   `["sdk-ts"]`).
4. Otherwise ownership is **unknown**: an ignore-listed entrypoint with no
   binding. The session is not shown. Sessions with unknown ownership are
   re-evaluated on every change of the T3 session map, and whenever rule 1
   or 2 later applies.

Payloads from subagents (`agent_id` present) are always dropped. T3
exposes the provider session id through thread detail (§6.5).

**Ownership transitions.** Every owner or binding change bumps the
session's binding generation. *Revoke* means, for each open request of the
session and inside the agent's delivery lock: retire its keys target, end
the request (`uncertain` if it is `answering`, otherwise `lost`), and
release its hold with an empty reply. A revocation never removes a herdr
agent.

| From → to | Action |
|---|---|
| Hooks → herdr `b` | migrate `claude:<sid>` into `b`; the request keeps its id |
| Hooks → T3 or unknown | revoke, then remove `claude:<sid>` |
| herdr `a` → T3 or unknown | revoke on `a`; `a` stays |
| herdr `a` → Hooks | revoke on `a`; add `claude:<sid>`; requests are not carried back |
| herdr `a` → herdr `b` | revoke on `a`; new requests open on `b` |
| T3 or unknown → Hooks | add `claude:<sid>` |
| T3 or unknown → herdr `b` | record the binding |

**Concurrency.** One hooks reconciler task is the only code that adds
hooks-only agents or opens, migrates, revokes or ends hook requests. It
never waits on herdr itself:

- Screen reads run in spawned tasks. Each carries a *ticket* (pane
  incarnation, binding generation, occurrence generation, a per-purpose read
  sequence). The result is applied only inside the agent's delivery lock,
  after rechecking that the pane, the binding, the occurrence and the
  request are all unchanged and that no newer read of the same purpose
  exists. Otherwise it is dropped.
- At most 8 reads run at once and a read never waits for a slot; it is
  retried on the next tick. Superseded reads are aborted. At most 256
  occurrences are tracked; one over the cap is released empty (a held
  request) or opens nothing (the herdr fallback still shows the dialog).
- Per turn, the reconciler first serves the timers and any T3 change, then
  at most 32 control messages, 32 read results and 32 lifecycle events, so
  no queue can starve a close, a timer or a takeover.
- At most 4096 Claude sessions, 4096 Codex sessions and 4096 sessions with
  unconsumed identities are stored. A full map evicts the least recently
  active session without an open request, or refuses a new one. Evicting an
  unconsumed identity makes every new occurrence shadowed (O7) for 10 minutes.
- Path resolution never runs on the reconciler. Canonicalizing a hook's
  `cwd` or a header socket path can block on an unavailable mount, so it
  runs in at most 4 blocking workers. The reconciler reads a cache of
  answers, each fresh for 3 s, and waits only for workers it has just
  started, up to a deadline and never while holding a lock. A path whose
  answer is missing, stale or still being resolved is *unknown*, and unknown
  fails closed: a header socket path that is unknown gives no header
  binding, and a directory that is unknown gives no Codex cwd match. The
  next tick retries. The cache holds at most 4096 paths.
- **Lock order** (R11). The herdr per-agent delivery lock is taken before
  the hooks occurrence lock whenever both are held, and an occurrence lock
  is never held while taking a delivery lock. A key delivery holds it for at
  most the keys deliver budget (`deliver_budget`, 8 s) plus the local write
  of the key: the lock wait, the guard read, the C7 rival reads (R12), any
  further read of the target and the connect all share that one deadline.

**Registry status and hook state.** Hook-set state wins for 30 s, and a
registry `status` never turns a hook-set `done` into `idle`.

**Permission requests** (`tool_name` ≠ `AskUserQuestion`):

1. Open a `permission` request with the §5.1 key; O1 decides whether it is a
   new occurrence.
   - `answer_path`: `hook`.
   - `title`: the tool name.
   - `detail`: see **Display rules** below.
   - Options:
     - `allow` "Yes";
     - `deny` "No";
     - `allow_always` "Yes, always", only when acd can build the rule and
       show it whole.
   - **The `allow_always` rule.** One function builds the rule for display
     and for delivery. For `Bash` it is the exact command,
     `{toolName: "Bash", ruleContent: <command>}`, because Bash requests
     carry no `addRules` suggestion (gate 2e). For any other tool it is the
     first rule of the first `addRules` suggestion. No rule is offered when
     the command is empty or whitespace, contains `*`, `(` or `)`, or
     `dangerouslyDisableSandbox` is anything but absent or false, when a
     suggested rule is empty, or when the rule text contains control, bidi or
     other invisible characters or is over 4000 characters. The option
     shows the exact rule and says that paths outside the directory may
     still prompt, because a saved rule does not cover them.
2. Hold the HTTP response, if the session is owned by Hooks or herdr and the
   hold is admitted (§6.1). Otherwise reply empty at once; the terminal
   dialog answers it.
3. On a claim, reply with one of these:
   - **allow:**
     `{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}`
   - **deny:** the same envelope with
     `"decision":{"behavior":"deny","message":"Denied from agent inbox"}`.
     Fixture: `tests/fixtures/hooks/claude/response-deny.json`.
   - **allow_always:** allow, plus
     `"updatedPermissions":[{"type":"addRules","rules":[{"toolName":…,"ruleContent":…}],"behavior":"allow","destination":"localSettings"}]`,
     with the rule built above.
     - Only `localSettings`, `projectSettings` and `userSettings` persist
       (P0 item 2). Claude rewrites the whole file, atomically, and keeps
       its mode (gate 2e).
     - This writes to the user's project settings, so the client must show
       the exact rule before sending.

   **Attribution** (§5.1: the outcome must be consistent with the answer).
   Writing the reply does not prove Claude used it, because another hook
   may already have decided (P0 items 6 and 8b; gate 2c, 2d).
   acd correlates the outcome with this request's `tool_name` and
   `tool_input`, subject to O4, O5 and O7:
   - **allow:** a `PostToolUse` or `PostToolUseFailure` with the same tool
     and input gives `by: "inbox"`. After an allow, Claude sends that event,
     then `Stop`.
   - **deny:** a `Stop`, or the next `UserPromptSubmit`, with no matching
     `PostToolUse` gives `by: "inbox"`. After a deny, Claude sends `Stop`
     without `PostToolUse`, and `Stop.last_assistant_message` often quotes the
     denial.
   - **Contradicting outcome** (acd allowed but the tool never ran, or acd
     denied but it ran): `by: "terminal"`, with no `option_id`.
   - **No correlatable evidence within 30 s:** `by: "uncertain"`.

   The dialog leaving the screen only says it closed. It yields `uncertain`
   unless the correlated evidence above arrives.
4. **The connection closes before acd replies.** With `http` hooks this
   means a terminal deny, Esc, an interruption or a dead process. (A
   terminal allow does not close it; see below.)
   - Herdr-bound: if the captured dialog is still on screen, switch to
     `answer_path: "keys"` (never while the hold is open). If the read is
     unavailable, it is retried a bounded number of times, then the request
     resolves `uncertain`. If the dialog is gone or was never captured, the
     request waits for hook evidence as below.
   - Hooks-only, or no dialog: resolve `by: "terminal"` if `PostToolUse` or
     `Stop` arrives within 30 s, otherwise `uncertain`.
   - **A terminal allow** leaves the connection open (gate 2b). acd resolves
     the request `by: "terminal"` when a `PostToolUse` arrives for a request
     it has not answered, or when the matched dialog leaves the screen, and
     then releases the hold with an empty reply.
5. **`permission_hold` expires.** acd replies empty. A herdr-bound request
   whose dialog is still on screen switches to `keys`; an unbound one
   resolves `lost`.
6. **Dialog match** (M1 to M5). A hook binds to an on-screen dialog only on
   an exact, unambiguous match of the complete command (or the complete
   question text and option list), read from the raw rows of the dialog
   region. The dialog is captured from herdr's detection snapshot within 2 s
   after the request opens. The decision never uses the normalized dialog
   text or identity.
   - **M1, Claude Bash.** The first row after the top rule is exactly
     `Bash command`. The region has exactly two dashed rules, with exactly
     one row between them, and that row (minus one leading space) equals
     `tool_input.command` byte for byte. Rows between the second rule and the
     options are blank or `Do you want to proceed?`. Other Claude tools never
     match.
   - **M4, whitespace and wrapping.** herdr's screen text has no soft-wrap
     marks and herdr reports no pane width, so a row break cannot be told
     from a newline. A command or question that spans more than one row never
     matches, and whitespace inside one row is compared exactly. A hook
     command containing `\n`, `\t`, `\r` or trailing whitespace never matches
     (D26).
   - **M5.** Any extra rule row, `$ ` row or question row, as a forged
     description would add, changes the counts and nothing matches.
   - If nothing matches, the request has no screen match. A held request is
     still answerable through its hold; a request whose hold has ended is
     read-only, and the user answers in the terminal.
   - After the hold, `keys` targets exist only when the match succeeds:
     built from the captured screen for this occurrence, bound to the pane
     incarnation, registered once per request id, and only while the request
     is still open. The options are `allow` (key `1`) and `deny` (the number
     of the `No` option, else `esc`). `allow_always` is dropped (D9).

**Display rules** (R8). The detail is built from the whole payload, at most
4000 characters, so nothing the agent will run is hidden.

- `Bash`: each line of the command, prefixed `$ ` on the first and `> ` on
  continuations; then every other input key except `description` as a line
  `"key": <compact JSON>`. No other tool is special-cased: all keys are
  shown as labelled JSON lines, `file_path` first, including the content of
  Write and Edit. A command line cannot forge a key line, because command
  lines carry the marker.
- Control characters show as escapes (`\t`, `\r`, `\u{7}`), a literal
  backslash as `\\`, invisible and format characters as `\u{XXXX}`, and
  leading and trailing whitespace is escaped rather than trimmed. Newlines in
  a command stay real line breaks.
- A detail over the cap is cut at 3800 characters and followed by "This is
  too long to show in full, so it cannot be answered here. Answer it in the
  terminal." The request is read-only: no options, not ready.
- For Codex requests (§6.3) the agent's own description is shown first, as
  one escaped line `Reason: <description>`. The Claude Bash `description`
  is not shown.
- Clients render the detail as monospace lines, one block per logical line
  with a hanging indent (§8).

**Questions** (`AskUserQuestion`):

- The `PreToolUse` and `PermissionRequest` events share the key. Both reply
  empty at once.
- A `question` request keeps the full `questions` array, with `free_text`
  false. It is answerable only when all of these hold:
  - it has a single question;
  - `multiSelect` is false;
  - it is herdr-bound;
  - the dialog on screen matches (M3): the row with `☐` is exactly
    `☐ <header>`, one non-blank row holds the question text, the options
    are numbered in order with exactly the hook's labels (and one row of
    description each when the hook has one), followed by `Type something.`
    and `Chat about this`.

  Any other question is read-only. A malformed question array is read-only
  too (D25): more than 4 questions, more than 9 options, a missing field, or
  an empty label, or labels that clash once invisible characters are
  removed.
- The dialog adds two rows that are not in the tool input: "Type
  something." and "Chat about this". These are not offered as options.
- A multi-question dialog shows a tab strip. Its fingerprint includes the
  strip, so it stays read-only.
- Answering sends the option's 1-based number key, guarded by the
  fingerprint.
- Outcome evidence is the `answers` map of the `PostToolUse` (response, else
  input), and counts only when it contains the expected question with a
  string answer. Any other outcome is no evidence.

**After a keystroke.** A key that arrives after the dialog has closed lands
in the agent's input box (P0 item 8b).

- acd reads the prompt line just before sending, and again 300 ms, 1 s
  and 3 s after. The last read coincides with the end of the
  confirmation poll in §5.1.
- If the prompt line gained the sent character, the request resolves
  `by: "uncertain"` with `reason: "stray_keystroke"`, which clients show as
  "a stray keystroke may be in the input box".
- `esc` produces no character, so it gets no prompt check. For `esc`,
  attribution relies on correlated evidence only.
- acd never sends a corrective backspace.

**Completion.**

- `Stop` sets `done` and fills `last_message`, at most 500 characters, for
  hooks-only agents only (herdr agents get their message from herdr).
- `UserPromptSubmit` sets `working`.
- `SessionEnd`, or the entry disappearing from the registry, marks a
  hooks-only agent for removal 10 minutes later. Any later hook, or a
  complete scan that lists the session, cancels the mark.

### 6.3 Codex adapter (command shim + herdr)

**Hooks installed:**

| Hook | Shim timeout |
|---|---|
| `SessionStart` | 2 s |
| `UserPromptSubmit` | 2 s |
| `PermissionRequest` | 1 s |
| `PostToolUse` | 2 s |
| `Stop` | 2 s |

Codex fires `SessionStart` lazily, on the first prompt, so a fresh pane has
no hooks and no `agent_session` until then.

**Binding** (P0 additional finding 1, gate 4). Codex runs hooks inside a
`codex app-server`, usually the shared managed daemon, so `HERDR_PANE_ID`,
`HERDR_SOCKET_PATH` and `HERDR_SESSION` in the hook environment are the
daemon's. They can name a dead herdr session or another live pane, and pane
ids repeat across sessions. A fresh isolated app-server does not cure it.
acd therefore **never uses the `HERDR_*` values**, although the shim still
forwards them. It binds a Codex session to a herdr pane by, in order:

1. exactly one herdr `codex` pane reporting `agent_session.value` equal to
   the Codex `session_id`. Two panes reporting it are never resolved by
   preference: the session is not bound. A reporting pane whose cwd differs
   from the payload cwd is not bound either (R9);
2. otherwise the only herdr `codex` pane, across all panes, in the same
   `cwd`. A pane's cwd is its foreground cwd when herdr reports one, and
   paths are compared after canonicalization (herdr reports `/tmp/...`,
   Codex `/private/tmp/...`).

`agent_session` is absent in shared-daemon mode, and herdr can attach it to
the wrong pane. With no binding, or two or more Codex panes in one `cwd`
and no `agent_session`, the request stays read-only. Neither rule is proof,
so every Codex request still needs the screen checks below. There are no
hooks-only Codex agents (D4): an unbound request opens nothing, and herdr's
`blocked_unparsed` fallback shows the dialog read-only.

**`PermissionRequest`.** It replies empty at once. The request opens with:

- key `(session_id, turn_id, sha256(tool_name + tool_input))`, reusable;
- options:
  - `allow` "Yes" (key `1`);
  - `deny` "No, and tell Codex what to do differently" (key `esc`).

  `allow_always` is never offered. Codex's "don't ask again" grants a prefix
  the agent chose, and acd cannot show it;
- `detail`: one escaped line `Reason: <description>`, a blank line, then the
  command and other input keys, per the display rules of §6.2.

**Answering.** A request is answerable only after the dialog on screen is
validated, through one path (C1 to C7):

- The bound pane is read, then every other Codex pane that exists at read
  time (its *rivals*). Each read returns *shows* (the complete command
  matches), *not shown*, or *unknown* (the read failed, timed out or was
  truncated). The rivals read are recorded.
- The request is enabled only if the bound pane shows the dialog, every rival
  is *not shown*, the current set of rivals equals the recorded set, and the
  binding still picks the same pane and incarnation. A rival that is
  *unknown* or that shows the command, or a changed set, leaves the request
  read-only and schedules a new read.
- **M2, the match.** Among the rows above the first option, exactly one row
  starts with two spaces and `$ `, the next row is blank or the first
  option, every row below the command row up to the first option is blank,
  and the command equals `tool_input.command` byte for byte. A command that
  spans more than one row never matches (D26).
- A live target carries the set of rivals it was validated against. The keys
  engine refuses, sending nothing, when the current set differs, even before
  the reconciler has noticed. When it notices, it marks the request not ready,
  re-reads, and either re-enables it or revokes it.
- **C7, at delivery.** Under the herdr delivery lock, right before the key
  is written and after the guard read of the bound pane, the keys engine
  reads every rival of the validated inventory again (R12). Each read must
  be whole and must not show the command, and each rival must still be the
  same pane incarnation. A rival that shows the command, cannot be read
  before the call deadline, was truncated or is gone refuses the answer with
  `request_not_ready` and sends no key. The reconciler then revalidates.
- A Codex session that moves to another pane, becomes unbound, is taken by
  T3, or gets a rival showing the command has its request revoked.
- acd waits up to 1 s for the dialog. The measured delay is 35–118 ms
  (P0 item 5). Until validation succeeds, a claim gets `request_not_ready`.
- If the pane is too short, Codex collapses the dialog
  (`[… N lines] ctrl+a view all`). The command text is then missing, so
  nothing can match and the request stays read-only. Clients show "enlarge
  the pane to answer here"; enlarging goes through the same validation.

**Completion.** `PostToolUse` or `Stop` serve as resolution evidence, along
with the fingerprint leaving the screen. Evidence matches
`tool_input.command`, because `PostToolUse.tool_input` drops `description`
(D3).

**Shared-daemon limitation.** When Codex runs in the shared managed daemon,
`agent_session` is usually missing and the hook environment is wrong, so
acd can answer only when exactly one Codex pane is in the `cwd`. Embedded
mode (for example `-c key=value`) gives hooks the pane's own environment
and a correct `agent_session`; Codex prints "Running without the shared
background server". Restarting the managed daemon does not fix it: its
environment follows whoever starts it. `acd hooks install codex` says so.
Hooks for the first run may also need a one-time review inside Codex.

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
- acd accepts messages and frames up to 256 MiB. A thread stream starts with
  the full thread snapshot, and a busy thread exceeds 16 MiB (one real
  snapshot was 23.7 MB), so a lower cap dropped the connection on every
  discovery pass.

**Visibility.** The shell lists every thread, including subagent threads
and ones the user has put away. acd shows a thread only when all hold:

- `lineage.parentThreadId` is absent or null (a subagent thread has it);
- it is not settled: `settledOverride` is not `"settled"` and `settledAt` is
  null or absent; `settledOverride: "unsettled"` wins over `settledAt`, and
  an `unsettledAt` at or after `settledAt` counts as unsettled;
- it is not snoozed: `snoozedUntil` is null, absent or in the past.

A hidden thread is treated like an archived one: no agent, no discovery
stream, no request stream. It becomes visible anyway while it needs the
user: shell `status` is `waiting`, or `pendingRuntimeRequest` is set. Its
streams then open as for any visible thread, and it is hidden again once
neither holds. acd keeps the shell entries of hidden threads, so a snooze
that ends shows the thread on the next tick. Missing or unreadable fields
count as visible, so older T3 versions keep working.

A shell `status` of `completed` with a non-empty `pendingBackgroundTasks`
maps to `working`: T3 still shows that thread as busy.

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
- `~/.config/acd/` has mode 0700. `client.token`, `t3.token` and the hook
  token store `hook-tokens.json` (§6.1) have mode 0600.
- acd refuses to start if these modes are looser.
- Tokens are 32 random bytes, base64url-encoded, and compared in constant
  time.
- `acd token rotate client` replaces the client token. It closes the
  sessions that used the old token with code 4401 ("token rotated"), and
  cancels their commands, including commands detached from earlier
  disconnects:
  - a command that was not yet delivered is released;
  - a command that was mid-delivery resolves `uncertain`;
  - neither is ever re-delivered.
- `acd token rotate hook` follows §6.1: the previous hook token stays valid
  for running Claude sessions until it is retired.

**Logs.**
- Logs never contain tokens.
- Request details and screen content are logged only at `debug` level.
  Even there they are redacted, using the same secret patterns as the hook
  payload sanitizer (API keys, bearer tokens, `KEY=value` environment
  assignments), and truncated to 4 KiB.

**Hook endpoint.** It is separate from the `acd/1` listeners: an HTTP
listener on `127.0.0.1` for Claude and the owner-only Unix socket `hook.sock`
for Codex, both authenticated with the hook token (§6.1, §9).

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
- A request's detail is shown as monospace lines. Each logical line is a
  block that wraps with a hanging indent, so a long or wrapped command still
  reads as one line, and the escapes of §6.2 stay visible.

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
- `hook_port`, the TCP port of the Claude hook listener on `127.0.0.1`.
  The default is 47801 when the key is absent. `acd init` writes a random
  free port into a new config, so the port is not well known (amendment A1);
  an existing config keeps its port. The port must not change after hooks are
  installed, because running Claude sessions snapshot the URL.
- `allowed_origins`
- `claude.permission_hold`, in seconds, default 3600
- `ignored_entrypoints`, default `["sdk-ts"]`
- `herdr.enabled`, `herdr.extra_sockets`
- `t3.enabled`, `t3.url`. The T3 token is kept in `t3.token`.

**Commands.**
- `acd init`
- `acd run`
- `acd service install [--print] [--force]`: a launchd agent
  (`dev.agent-caller.acd`) on macOS, a systemd user unit on Linux. The file
  carries the ownership marker `acd-managed:v1`; a foreign file at the same
  path is refused without `--force`, an existing definition is backed up to
  `<config_dir>/backups/` (the five newest are kept), and `--print` writes
  nothing. On Linux, install runs `daemon-reload`, `enable` and `restart`; on
  macOS it boots out an existing job (a failure there is ignored) and
  bootstraps the new one. A failing
  `launchctl` or `systemctl` call fails the command and names the call. The
  service sets `ACD_CONFIG_DIR` and a usable `PATH` but not
  `CLAUDE_CONFIG_DIR`, because the daemon reads Claude roots from the install
  records. It restarts acd after any non-zero exit and leaves a clean stop
  alone (exit codes below).
- `acd status`
- `acd hooks install claude|codex [--allow-tracked] [--allow-unknown-git]`
- `acd hooks uninstall claude|codex`
- `acd hooks status`
- `acd token rotate client`
- `acd token rotate hook [--allow-tracked] [--allow-unknown-git] [--force]`
- `acd hook codex <event>`: the hook shim (§6.1), not for direct use
- `acd pair [--host H]`
- `acd tls rotate`

The config directory also contains `tls.pem` (the private key and matching
certificate, mode 0600) and `tls.lock` (the cross-process publication lock).
For hooks it holds `hook-tokens.json`, `hook.sock`, `hooks-installed.json`,
`hooks-journal.json` and the writer lock `hooks.lock` (§6.1). The daemon's
own files are `acd.sock` and `hook.sock`, and its single-instance lock
`acd.lock` (§6.1).

**Supervision.** The daemon exits with a code the supervisor can act on:

| Code | Meaning | Restarted by launchd/systemd |
|---|---|---|
| 0 | SIGTERM or SIGINT; held hook requests get an empty reply first | no |
| 1 | a server task (a listener) was lost | yes |
| 70 | a panic anywhere, including a poisoned lock | yes |
| 75 | Tailscale addresses changed or stayed unreachable (§7.1) | yes |

`tailscale` is found by absolute path (including the Homebrew locations),
because launchd's `PATH` is minimal.

**Hook endpoint trust.** Two things authenticate in different directions,
and v1 protects only one of them.
- *Client authentication.* acd checks `X-Acd-Token` on every hook request.
  Over `hook.sock` the shim also checks the socket's owner, mode and every
  parent directory before it sends anything, so a process that does not own
  the config directory can neither receive a Codex payload nor answer it.
- *Server authentication.* Claude's `http` hooks cannot authenticate acd:
  Claude does no signature check, and an echoed secret, a custom header or a
  health probe cannot prove identity. Claude's hooks therefore go to a TCP
  port on loopback. While acd is not holding that port (stopped, crashed,
  restarting, or started after the agents), another OS user, or a sandboxed
  process that can bind loopback but cannot read the 0700 config directory,
  could bind it, receive `X-Acd-Token` and the payload, and answer a
  `PermissionRequest` with an allow, including a persistent `allow_always`.
  The `acd/1` clients are not affected (Unix socket locally, pinned TLS
  remotely).

v1 accepts this residual risk for a personal, trusted host (ruling R7,
2026-10-09). The mitigations are defence in depth, not a fix: a random
per-install `hook_port`; acd binds it before it reports ready and refuses to
start if the bind fails; `acd status` and `acd hooks status` warn, best
effort, when another process holds the port. **v1 does not protect against
hostile local users, loopback-capable sandboxes, or same-uid processes.**
Moving Claude to the command shim is a later change and needs gate 2
re-tested for command hooks.

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
The gates below were open; each blocked the plan named beside it until it
had been checked and the spec updated.

1. **Deferred past v1:** free text in Claude `AskUserQuestion` on the
   `keys` path.
2. **Claude `http` hooks, live: resolved 2026-10-09**
   (`docs/research/2026-10-09-p3-gates-claude.md`, fixtures in
   `tests/fixtures/hooks/claude/`; Claude Code 2.1.295). Folded into §6.1
   and §6.2:
   - header env interpolation works with `allowedEnvVars`; the config is a
     snapshot taken at session start;
   - the held connection closes within about 15 ms on a terminal deny or
     Esc and does **not** close on a terminal allow;
   - a held hook is left open while another hook decides, and a later reply
     is ignored; the correlated outcome evidence gives `terminal` when the
     decisions differ;
   - `allow_always` with `addRules`/`localSettings` persists across a new
     session, but does not cover paths outside the working directory;
   - a stalled endpoint is bounded by the 1 s lifecycle timeout;
   - `SessionStart` is never sent over `http`.

   Still unverified: the `PostToolUseFailure` and `PermissionDenied` payloads
   after a denial; `allow_always` for a request with a genuine `addRules`
   suggestion; whether resume re-reads the settings (inferred from
   `/clear`); several Claude sessions sharing one endpoint under token
   rejection.
3. **T3: resolved 2026-10-08** (`docs/research/2026-10-08-t3-gate3.md`).
   Auth, framing, resume, scopes, prompts and the provider session id are
   folded into §6.5. Still unverified: multi-select questions, the
   `acceptForSession` and `acceptAlways` decisions, the statuses
   `preparing`, `queued`, `failed`, `interrupted` and `rolled_back`, replay
   past T3's limits, and concurrent responds. The P4 smoke test checks
   `acceptForSession`.
4. **Codex binding: resolved 2026-10-09**
   (`docs/research/2026-10-09-p3-gate4-codex.md`; Codex 0.162.0, herdr
   0.9.3). Folded into §6.3:
   - herdr's Codex integration reports `agent_session`
     (`{source, agent, kind: "id", value}`) and it equals the hook
     `session_id`, but only when the hook environment names the right pane;
     it is absent in shared-daemon mode;
   - the stale `HERDR_*` reproduction holds, and a fresh isolated app-server
     does not fix it; acd never uses those values;
   - two Codex panes in one cwd, with no `agent_session`, stay read-only.

   Still unverified: restarting the user's managed daemon; `codex resume`;
   concurrent requests; two embedded-mode panes.
5. **Claude session registry: resolved 2026-10-09** (same document as item
   2). Folded into §6.2: `idle`, `busy`, `waiting` (+ `waitingFor`),
   `procStart` as UTC `ps -o lstart=`, a killed process leaves its entry,
   and unreadable entries.
6. **herdr ordering: resolved 2026-10-08**
   (`docs/research/2026-10-08-herdr-gate6.md`). `revision` exists per pane
   but does not move on status changes, so issue-order application on the
   single reconciler is the rule (§6.4 step 3).
7. **Hook token rotation: resolved 2026-10-09** (same document as item 2).
   Running sessions keep the token they started with, and a rejected token
   is visible but non-blocking. The 10-minute grace is replaced by the
   retention rule of §6.1.
8. **Upstream ask, not blocking.** Propose a conditional-input method to
   herdr, so the `keys` path becomes atomic.
9. **Live checks of P3 (open).** The results go into
   `docs/research/2026-10-09-p3-live.md` when they are run.
   - *Live check 10.* A `Tip:` line rotating under a visible dialog would
     make the target watch publish a false `terminal`.
   - *Live check 11.* Whether `~/.codex/hooks.json` needs a one-time review
     in Codex.
   - *Live check 12.* How Claude and Codex render a command that wraps or
     spans lines. Until measured, such commands never bind to a screen
     (§6.2 M4, D26); a width-aware rule would be a separate, reviewed
     change.
   - *Live check 13.* Whether Claude fires several `PermissionRequest`
     hooks before the first dialog ends (O1).
   - *Live check 14, a release gate.* How long after a Claude process
     starts its registry entry appears, including sessions started during a
     rotation and under load. `REGISTRY_GRACE_MS` (5 s) must exceed the
     worst delay; an underestimate can retire the previous hook token while
     a live old-token session exists. P3 is not released until it passes.

## 12. Error handling

| Failure | Behavior |
|---|---|
| acd down or the hook endpoint unreachable | Claude's http hook fails open, and the Codex shim replies empty within about 1 s. The normal dialog shows. |
| A held hook's connection closes early | It closes on a terminal deny or Esc (about 15 ms), an interruption or a dead process; a terminal allow leaves it open. Reconcile as in §6.2, step 4: switch to `keys` if the captured dialog is still on screen, otherwise wait for hook evidence. Hooks-only agents resolve `terminal` on later evidence, otherwise `uncertain`. |
| Terminal allow of a held request | The connection stays open. acd resolves `terminal` from `PostToolUse` or the dialog leaving the screen, then replies empty (§6.2, step 4). |
| Hook queues, holds or reads over their bounds | Fail open: an empty reply for the hook; a held request is released empty and the terminal answers it (§6.1, §6.2). |
| Hook settings file or token store unwritable, journal conflict | Installer commands and `acd token rotate hook` refuse and name the file. The previous hook token stays accepted, and `acd status` shows the cause (§6.1). |
| A daemon server task dies, a panic, Tailscale changes | acd exits 1, 70 or 75 and the supervisor restarts it (§9). |
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
- exact dialog matching against forged, prefixed, wrapped and
  differently-spaced renderings (§6.2 M1 to M5, §6.3);
- hook occurrences, shadowing and evidence attribution (§5.1 O1 to O7);
- the hook token store, retention and retirement (§6.1);
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
- real HTTP from a fake Claude http hook and through the Codex shim over
  `hook.sock`, including connection close and timeout boundaries;
- the installer against real settings files: concurrent edits, foreign
  entries that mention the marker, Git tracked, untracked and unknown, record
  failures followed by Claude's own rewrite, and rotation crashes.

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

**P6: T3 thread visibility and frame size, 2026-10-11.** A busy thread's snapshot (23.7 MB) exceeded the 16 MiB WebSocket cap and flapped the T3 connection, and subagent, settled and snoozed threads flooded the inbox with 290 agents. §6.5 now raises the cap to 256 MiB, adds the visibility rule and maps `completed` with pending background tasks to `working`; §5 "Agent rules" removes hidden threads.

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

**P3, 2026-10-09 (revision 5.0).** Gates 2, 4, 5 and 7 were checked live
(`docs/research/2026-10-09-p3-gates-claude.md`,
`docs/research/2026-10-09-p3-gate4-codex.md`), and the P3 plan
(`docs/superpowers/plans/2026-10-09-p3-hooks.md`) went through Sol rounds 1
to 3 and a transport amendment (A1). The orchestrator's task reviews added
rulings R1 to R13. This revision folds the results in:

- §5.1: the Claude key now includes the tool digest and is reusable; Codex
  evidence matches the command; hook occurrences O1 to O7 define a new
  request.
- §6.1: the endpoint's bounds and the hold admission rule (R10); Codex uses
  an owner-only `hook.sock` with root-to-leaf `lstat` checks, never TCP
  (A1); Claude's held connection closes on a terminal deny or Esc but not on
  a terminal allow (gate 2b); the 10-minute token grace is replaced by the
  token store, generations and retention (T1 to T6, gate 7); the settings
  writer lock, the journal (J1 to J4), exact markers and Git tracked,
  untracked and unknown; structured files are bounded (F1).
- §6.2: `SessionStart` is removed (gate 2) and `PostToolUseFailure` added;
  the registry scan rules (gate 5); binding conflicts bind nothing (R9);
  ownership transitions, read tickets, lock order (R11); the Bash
  `allow_always` rule (Q2); display rules (R8); the dialog match contract
  (M1 to M5) with no screen binding for multi-row commands (D26).
- §6.3: the Codex binding rule as implemented (gate 4, round-2 ruling B4),
  its validation (C1 to C7), no `allow_always`, and the shared-daemon
  limitation.
- §6.1 (daemon and credentials): the `acd.lock` single-instance lock and
  the socket replacement rules; credentials reload from the store within
  2 s, retirement revokes in memory at once, and an expired previous token
  is rejected on each request (R13).
- §6.2 (concurrency): path resolution in at most 4 bounded workers with a
  3 s freshness window, unknown failing closed, and session and occurrence
  caps (A8); Claude Bash details omit the agent's description and Codex
  shows it as one escaped `Reason:` line (R8).
- §6.3: delivery-time rival re-reads under the delivery lock, refusing with
  `request_not_ready` (C7, R12).
- §9 and §11: the config keys (a random `hook_port` per new config), the
  commands, the config-directory inventory with `acd.lock`, supervision exit
  codes, the squat warning in `acd status` and `acd hooks status`, the trust
  note printed by the Claude install, and the trust model; gates 2, 4, 5 and 7 resolved, live checks 10 to 14 open (14 is a
  release gate).

Decisions that differ from the gate documents' proposals: acd never binds
Codex by `HERDR_*` even when the socket matches (the env is the
app-server's); and the previous hook token is retired by a scan that started
after the header update, not "when every older session has exited"
(§6.1, T4). Claude's `http` hooks keep the residual risk of §9, accepted for
personal use (R7).

**P3 final, 2026-10-10 (revision 5.0).** The draft's pending points are closed against the approved tasks 17 to 19, 30 and 31: C7 and R12 (§6.3), R13 and A8 (§6.1, §6.2), the daemon socket rules and `acd.lock` (§6.1, §9), the random `hook_port`, the squat warning and the install trust note (§9), and the R8 clarification (§6.2). The unverified points stay in §11 and live checks 10 to 14 remain open.
