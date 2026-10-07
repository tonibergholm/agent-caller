# acd/1 protocol (normative)

## 1. Transport and security

**Binding.**
- WebSocket on port 47800 by default.
- acd binds only to loopback and to addresses that the local Tailscale
  daemon reports as its own (`tailscale ip`, or the LocalAPI). Each such
  address must also fall in `100.64.0.0/10` or `fd7a:115c:a1e0::/48`.
- A configured address that fails either check is refused at startup.
- When Tailscale's addresses change, acd rebinds.
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
- Request details and screen content are logged only at `debug` level.
  Even there they are redacted, using the same secret patterns as the hook
  payload sanitizer (API keys, bearer tokens, `KEY=value` environment
  assignments), and truncated to 4 KiB.

**Hook endpoint.** It is a separate HTTP listener on `127.0.0.1` only, using
`hook.token`.

## 2. Wire format

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

## 3. Messages

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

## 4. Sequencing, resync and idempotency

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

## 5. Capabilities

- In v1, the daemon advertises `["answer", "send", "mark_seen"]`.
- A client must not send a command whose capability is not advertised.
- New features add new capability strings.

## 6. Data model and request lifecycle

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

### 6.1 Request lifecycle and identity

**Correlation keys.** acd uses these keys to deduplicate and to match
requests across sources:

| Origin | Key |
|---|---|
| Claude hook | `prompt_id`. It links `PermissionRequest` and `PreToolUse` for the same `AskUserQuestion`. |
| Codex hook | `(session_id, turn_id, sha256(tool_name + tool_input))` |
| herdr fallback | `(socket, pane_id, dialog fingerprint)` |
| T3 | the runtime request id |

**Occurrences.** A key identifies one *occurrence* of a dialog.

- While a request with that key is not yet resolved, observing the key
  again updates the existing request.
- Once the request is resolved, and the source has confirmed the dialog is
  gone, a later observation of the same key starts a **new occurrence**:
  acd increments a per-key generation counter and opens a new request with
  a new id.
- "Confirmed gone" means the fingerprint was absent from two consecutive
  detection reads, or the hook source reported the end of the request.
- Claude's `prompt_id` and T3 request ids are already unique per
  occurrence. The generation counter matters for Codex and for the herdr
  fallback keys.

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

## 7. Machine-readable definitions

`schema/client.schema.json` and `schema/server.schema.json` are generated
from the Rust types in this crate. The `examples/` directory holds golden
messages for every message type, validated in CI.
