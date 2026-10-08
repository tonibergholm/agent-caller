# acd/1 protocol (normative)

References written as "spec §N" point to docs/superpowers/specs/2026-10-07-agent-inbox-design.md.

## 1. Transport and security

**Binding.**
- WebSocket on port 47800 by default.
- acd binds only to loopback and to addresses that the local Tailscale
  daemon reports as its own (`tailscale ip`, or the LocalAPI). Each such
  address must also fall in `100.64.0.0/10` or `fd7a:115c:a1e0::/48`.
- A configured address that fails either check is refused at startup.
- When Tailscale's addresses change (the same new set seen on two probes
  30 s apart), acd exits with code 75 and is restarted by its supervisor
  (launchd or systemd). Clients reconnect and see a new epoch.
- **Local socket.** acd also serves the same server on the Unix socket
  `acd.sock` in the config directory, with mode 0600. It is meant for local
  tools such as `acd status`, so the client token never travels over TCP
  to whatever process holds the port. `hello` and token authentication
  apply as on TCP. A stale socket from an earlier run is replaced at
  startup.
- When the Mac app connects to a MagicDNS name, it checks that the name
  resolved to an address in these ranges before sending `hello`.

**Limits.**
- `hello` must arrive within 5 s.
- Frames are limited to 1 MiB.
- At most 8 unauthenticated connections at once. Past that, the upgrade
  request gets HTTP 503.
- A request carrying an `Origin` header is rejected with HTTP 403 unless
  the origin is in `allowed_origins`, which is empty by default. This
  blocks connections opened from web pages.
- **Ids and text.** A command `id`, `request_id`, `agent_id` and
  `hello.client.id` are at most 128 bytes. Answer `text` and `agent.send`
  `text` are at most 64 KiB. An oversized command gets a `bad_request`
  result and reserves nothing. A `hello` with an oversized `client.id` gets
  `error {code: "bad_request"}` and the connection is closed with 4401.
- **Commands in flight.** At most 32 per session and 256 across the daemon,
  counting commands whose session has already closed. Past either cap the
  command gets a `bad_request` result with the message
  `too many in-flight commands` and reserves nothing.
- **Idempotency table.** At most 16384 reservations and cached results.
  When it is full, a new command id gets a `bad_request` result
  (`too many recent commands; retry later`) until cached results expire.

**Tokens.**
- The config directory is `$ACD_CONFIG_DIR`, or `~/.config/acd/`. It has
  mode 0700. It holds `client.token`, `hook.token`, `config.toml`,
  `t3.token` (from the T3 adapter onwards), `machine-id` and `acd.sock`.
- `client.token`, `hook.token`, `config.toml` and `t3.token` have mode
  0600. acd refuses to start if any of them, or the directory, is looser.
- Tokens are 32 random bytes, base64url-encoded without padding, and
  compared in constant time.
- `acd token rotate client|hook` replaces a token. acd picks up a new
  client token within about 2 s.
- **Rotation.** Every session authenticated with the old client token is
  closed with code 4401, reason `token rotated`. Commands authorized under
  the old token are cancelled, including those whose session had already
  closed: a request not yet delivered goes back to `open`, and one that
  was mid-delivery resolves `by: "uncertain"`. Nothing is re-sent.

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
  - Clients ignore unknown `type`s and unknown fields. The daemon ignores
    unknown fields in client messages.
  - In Rust, `acd_protocol::decode_server` implements this: it returns
    `Ok(None)` for a JSON object with an unknown `type`, and an error only
    for malformed JSON, a frame without a string `type`, or a known type
    whose body does not match. An unknown error code decodes as
    `ErrorCode::Unknown`.
  - Unknown enum values are treated as follows:
    - unknown agent state: `unknown`;
    - unknown request `kind` or `answer_path`: shown read-only;
    - unknown `effect`: that option is not selectable.
    - unknown `source`, `kind`, `status`, `by` or source `state`: decoded as
      `unknown`;
    - an unknown request status is read-only;
    - an unknown resolution is shown as "check the terminal".
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
| `unsupported_protocol` | `hello.protocol` is not supported. Sent as an `error` with `supported`; the daemon closes with code 4400. |

**Close codes:**

| Code | Reason |
|---|---|
| 4400 | Unsupported protocol. |
| 4401 | No valid `hello`, invalid token (`unauthorized`), or `token rotated`. |
| 4408 | The client is too slow (`client too slow`). |

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
  `source_unavailable`, or any error the adapter reports as not delivered,
  such as `screen_mismatch`), its reservation is released, so the same id
  can be retried.

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
    described in spec §6.2 (Claude adapter).
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
  - For `hook`: see spec §6.2 (Claude adapter).
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
from the Rust types in this crate (regenerate with
`UPDATE_SCHEMA=1 cargo test -p acd-protocol`). The `examples/` directory
holds a golden message for every message type. `cargo test -p acd-protocol`
checks that each one round-trips through the Rust types and validates
against the committed schema; CI runs it on every push and pull request.
