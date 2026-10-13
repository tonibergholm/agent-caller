# acd/1 protocol (normative)

References written as "spec §N" point to docs/superpowers/specs/2026-10-07-agent-inbox-design.md.

## 1. Transport and security

**Binding.**
- WebSocket on port 47800 by default.
- TCP listeners serve WebSocket over TLS 1.3 only. The Unix socket serves
  plain WebSocket.
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
- The address range check is not authentication; the TLS certificate pin is.
  `acd pair` prints a pairing line containing the pin and client token. TCP
  clients must verify the configured pin; there is no plaintext fallback.
  `acd tls rotate` changes the certificate for new sessions, keeps existing
  sessions open, and keeps the client token unchanged. Pair every app again.

**Pre-authentication stages.**

| Stage | Bound |
|---|---|
| A. TCP accept → TLS finished | At most 16 handshakes at once per listener. 5 s each, then the socket is dropped. |
| B. TLS finished → HTTP upgrade request complete | Covered by the 10 s pre-authentication deadline; silent or trickling HTTP clients are cut when it passes. |
| C. Upgrade → `hello` authenticated | At most 8 at once; `hello` within 5 s. |
| A + B + C | At most 32 unauthenticated TCP connections shared by the daemon across all TCP listeners. Each must authenticate within 10 s of TCP accept. |

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
`hook.token`. The same routes are served on `hook.sock`, which also serves
the producer API (§8). The TCP listener has no `/external` routes and
answers them like any unknown route.

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
    - an unknown resolution is shown as "check the terminal";
    - unknown `reason`: decoded as `unknown` and ignored.
- **Version mismatch.** If `hello.protocol` is not supported, the daemon
  sends `error {code: "unsupported_protocol", supported: ["acd/1"]}` and
  closes with code 4400.

## 3. Messages

**Client → daemon.** Every command has a client-generated `id`, which is a
UUID.

```text
hello            {protocol: "acd/1", token, client: {id, name, version}, resume?: {epoch, seq}}
request.answer   {id, request_id, revision?, option_id?, text?}    // revision is sent only when answer.revision is advertised; exactly one of option_id or text
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
                  by: "inbox"|"terminal"|"agent"|"superseded"|"uncertain"|"lost", option_id?, reason?, message?}   // option_id is absent/null for a text answer
machine.updated  {epoch, seq, machine}
result           {id, ok: true, outcome?} | {id, ok: false, error: {code, message}}
pong             {id}
```

**Answer outcomes.** A successful `request.answer` result carries an
`outcome`:
- `"delivered"`: acd wrote the answer to the hook reply, the keys, or T3.
- Confirmation arrives later as `request.resolved {by: "inbox"}`, or as
  `by: "uncertain"`.

`reason` is optional. `"stray_keystroke"` means acd sent a key, and the
key may have landed in the agent's input box instead of the dialog
(`by` is then `"uncertain"`). Clients show "A stray keystroke may be in
the input box". `"rejected"` (`by` is then `"agent"`) means the producer
refused the answer before acting on it. Unknown reasons are ignored.

`message` is optional text the source reported with the resolution, at
most 500 bytes. Only the external source sets it (§8): on `rejected`
and on an `uncertain` that follows a `failed` ack. Clients show it on the
resolved card in place of the generic notice, including for `by: "agent"`,
which otherwise shows nothing.

**Error codes:**

| Code | Meaning |
|---|---|
| `request_gone` | The request was already resolved. |
| `request_changed` | The request changed; review it again. |
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
- **`request_changed` is not cached either.** It means the answer's
  `revision` no longer matches the request. Nothing was claimed or
  delivered, and the reservation is released, so the same command id may
  be sent again with the corrected revision after the user reviews the
  request again.
- **Revisions and older peers.** A `Request` without `revision` reads as
  revision 0. A `request.answer` without `revision` is not checked
  against the request (the behaviour before revisions).

**Recovery after a lost result.** The cache does not survive an epoch
change. After a reconnect into a new epoch, a client with commands in
flight must not resend them. It shows those commands as uncertain and lets
the user decide again from the fresh snapshot.

**Event content.** `agent.updated` always carries the full Agent, including
`pending`. `request.opened` and `request.resolved` exist for clients that
react to events, such as notifications.

## 5. Capabilities

- In v1, the daemon advertises `["answer", "send", "mark_seen", "answer.revision"]`.
- A client must not send a command whose capability is not advertised.
- New features add new capability strings.

## 6. Data model and request lifecycle

```text
Machine {
  id: string               // stable: the hostname plus a random suffix, created by `acd init`
  name: string
  acd_version: string
  sources: { hooks: SourceStatus, herdr: SourceStatus, t3: SourceStatus,
             external: bool }  // optional on the wire; absent means false. true when this daemon serves the producer API (§8)
}
SourceStatus { state: "ok"|"degraded"|"disabled"|"unconfigured", message?: string }

Agent {
  id: string               // source-prefixed opaque id, at most 128 bytes; long native ids are hashed
  machine_id: string
  source: "herdr" | "t3" | "hooks" | "external"
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
  role: "main" | "subagent"          // optional on the wire; absent or unknown means "main"
  parent_title: string | null        // optional; a subagent's parent thread title when known
}

Request {
  revision: u64                      // increases when answer-relevant content changes
  id: string                         // generated by acd, unique within the daemon epoch
  agent_id: string
  kind: "permission" | "question" | "blocked_unparsed"
  status: "open" | "answering" | "resolved"
  answer_path: "hook" | "keys" | "t3" | "external" | "none"
  title: string
  detail: string
  options: RequestOption[]           // permission options; for a question, the options of questions[0]
  questions: Question[] | null       // kind=question: the full original structure
  free_text: bool                    // whether a typed answer is accepted (questions only; v1: T3 and external)
  raw_screen: string | null          // blocked_unparsed only; at most 40 lines
  opened_at: timestamp
}
RequestOption { id: string, label: string, description: string | null,
                effect: "allow" | "allow_always" | "deny" | "choose",
                confirm?: bool }   // absent means false; clients ask for a second tap before sending it
Question { id: string, header: string | null, text: string, multi_select: bool,
           options: RequestOption[] }
```

A request is **structurally answerable** when all three conditions hold:

- `answer_path` is not `none`;
- `kind` is not `blocked_unparsed`;
- if it is a question, there is exactly one question and it is
  single-select.

Clients must also be connected, have the `answer` capability and see an
open request before offering an action. The daemon validates the request
ID, revision when supplied, and delivery readiness; structural
answerability alone does not guarantee delivery.

Anything else is shown read-only: "answer in T3" for T3 agents, "answer in
<producer>" (the agent's title) for external agents, "answer in terminal"
for other sources. A question with no options can still be answered when
`free_text` is true. Typed answers are supported only on the T3 and
external paths. Within T3, non-Claude providers require an explicit
`allowCustomAnswer: true`, while Claude defaults to enabled when absent.
This T3 field never enables text on hook or terminal paths. For an
external request the producer sets `free_text` itself (§8).

An option with `confirm: true` is sent only after a second deliberate tap
(or key press) on the same request revision; any other action in between
cancels the first. A client that does not know `external` shows these
requests read-only, so it never sends such an option without the
confirmation.

### 6.1 Request lifecycle and identity

**Correlation keys.** acd uses these keys to deduplicate and to match
requests across sources:

| Origin | Key | Reusable |
|---|---|---|
| Claude hook | `(session_id, prompt_id, sha256(tool_name + tool_input))`. `prompt_id` alone is per user turn, so the digest separates the tools of one turn. The same key links the `PermissionRequest` and `PreToolUse` of one `AskUserQuestion`. | yes |
| Codex hook | `(session_id, turn_id, sha256(tool_name + tool_input))`. Outcome evidence matches `tool_input.command`, because `PostToolUse.tool_input` drops `description`. | yes |
| herdr fallback | `(socket, pane_id, dialog fingerprint)` | yes |
| T3 | the runtime request id | no |
| External | `(producer id, item key)`; the key is minted by the producer for each occurrence | no |

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
- Claude, Codex and herdr fallback keys are reusable, so the generation
  counter applies to them. T3 request ids and external item keys are not
  reusable: once resolved, the key is tombstoned and never opens another
  request. External tombstones are kept by the adapter, not the registry
  (§8).
- Hook occurrences follow rules O1 to O7 in spec §5.1. For Claude, each
  `PermissionRequest` opens a new occurrence even when it is byte-identical
  to an earlier one, so the key alone does not decide whether a hook event
  is a new occurrence.

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
  up to 3 s. If the dialog disappears in two consecutive reads, it then
  waits up to 35 s for correlated outcome evidence. Evidence consistent
  with the selected option resolves `by: "inbox"`; contradictory evidence
  resolves `by: "terminal"`; missing evidence resolves `by: "uncertain"`.
  Codex cancellation-tail text for the tracked command also counts as
  evidence. A dialog still present after the observation window, or a
  detected stray key, resolves `uncertain`. acd sends nothing further.
  Codex's transient `working` state while hooks run is not evidence.
  Correlated hook outcomes require an evidence provider. The shipped
  daemon installs `hooks.evidence()` with `herdr.set_evidence` before
  starting the adapters (`crates/acd/src/daemon.rs`). A bare keys engine
  defaults to `NoEvidence`, which returns immediately and resolves a
  closed dialog as uncertain without waiting for hook evidence.
  The timing defaults live in `KeysTiming` in
  [`herdr/keys.rs`](../acd-adapters/src/herdr/keys.rs).
- **`t3`.** The T3 command is acknowledged and the thread's request
  disappears from its projection.
- **`external`.** The producer acknowledges the answer it fetched (§8).
  `applied` resolves `by: "inbox"`; `rejected` resolves `by: "agent"` with
  reason `rejected`; `failed`, an unacknowledged answer past its deadline
  and a lease that ends while the answer is outstanding resolve
  `by: "uncertain"`.
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
  - its T3 thread is hidden by the adapter's visibility rules (including
    settled and snoozed threads), once neither an open adapter request
    nor a pending core request remains;
  - for a hooks-only agent, 10 minutes after `SessionEnd`, or after 24 hours
    with no event.

## 7. Machine-readable definitions

`schema/client.schema.json` and `schema/server.schema.json` are generated
from the Rust types in this crate (regenerate with
`UPDATE_SCHEMA=1 cargo test -p acd-protocol`). The `examples/` directory
holds a golden message for every message type. `cargo test -p acd-protocol`
checks that each one round-trips through the Rust types and validates
against the committed schema; CI runs it on every push and pull request.

## 8. Producer API (external source)

A local program (a *producer*) puts questions in the inbox and acts on the
answers itself. acd stays a mailbox: it never runs a command for a
producer. The producer pulls answers, carries them out and reports the
outcome. Design: `docs/superpowers/specs/2026-10-13-external-source-design.md`.

**Transport.** HTTP/1.1 with JSON bodies on `hook.sock` in the config
directory, never on TCP. Every call carries `X-Acd-Token` with the hook
token. A wrong token, a wrong method or an unknown route answers 401, as
on the hook routes, and so does a daemon that predates the API. A producer
treats 401, 404, a refused connection and a timeout alike: no inbox for
now, retry later. Before connecting, a producer verifies the socket as the
Codex hook shim does (`crates/acd/src/hook_shim.rs`): the canonical config
directory, every ancestor owned by root or the user and not writable by
others (unless sticky), the directory owned by the user with mode 0700, and
the socket not a symlink, owned by the user, mode 0600.

**Trust.** The producer id is not authenticated: any holder of the hook
token can act as any producer (the same-user trust boundary of
`hook.sock`). A producer must not treat an acd ack, an answer or the
absence of a refusal as authority for an irreversible action; it decides
from its own durable record.

**Identity.**

| Name | Grammar |
|---|---|
| Producer id `pid` | `^[a-z0-9][a-z0-9-]{0,31}$` |
| Item `key`, `agent_key`, option `id` | `^[A-Za-z0-9._:-]{1,128}$` |

Agent ids and request ids derive from `(pid, agent_key)` and `(pid, key)`
and are opaque to clients. Keys are not reusable (§6.1). The producer
builds each key from its own durable occurrence id plus a hash of the
card's meaning, so a new occurrence is a new key. Tombstones live in
memory, the most recent 4096 per producer, and are lost on a daemon
restart; the producer's own record of resolved occurrences is the authority
and it never lists a key it has recorded as resolved. A producer that lost
its lease keeps its tombstones; at most 16 such producers are remembered; past that the longest idle one is
forgotten, with any card it still kept for a notice. An answer that expires
without an ack resolves `uncertain` with a `message` naming the agent.

**`PUT /external/v1/producers/{pid}/state`** is the heartbeat, upsert, ack
and fetch in one call.

```text
request  {lease_ms, answer_timeout_ms?, items?: Item[], acks?: Ack[]}
Item     {key, agent_key, agent?: {title?, project?, cwd?}, title, detail?,
          question?: {header?, text}, options?: Option[], free_text?}
Option   {id, label, description?, confirm?}
Ack      {answer_id, outcome: "applied"|"rejected"|"failed", message?}
200      {answers: Answer[], refused: [{key, reason: "resolved"}],
          ack_refused: [{answer_id, reason: "resolved"|"unknown"}]}
Answer   {answer_id, key, revision, option_id|null, text|null, expires_at_ms}
```

- `lease_ms` and `answer_timeout_ms` are in `30000..=900000`; the timeout
  defaults to `180000`. `agent.title` defaults to the producer id.
- Limits: at most 8 live producers, 32 items, 8 options per item, 32 acks.
  Byte limits: title 200, detail 4000, option label 80, option description
  200, question text 1000, question header 200, `agent.title` 200,
  `agent.project` 200, `agent.cwd` 1024, ack message 500, body 256 KiB.
  Text holds no control characters other than newline and tab and no bidi
  override or line/paragraph separator (the typed-answer rule).
- **All or nothing.** The whole body is validated first. A violation
  (including a duplicate `key`, `agent_key` or option id in one body, an
  item `key` already open under another `agent_key`, a body that is not
  valid JSON or is truncated, or a ninth live producer) answers 422 with
  `{"error": "<field and rule>"}` and changes nothing: no lease renewal, no
  acks, no diff. The error never repeats the input text. A 422 still runs the time-driven
  sweep (lease and answer expiry) that every call starts with; it never
  renews a lease, applies an ack or runs the diff.
- **Order**, under one registry lock: renew the lease; apply `acks`; upsert
  `items` (an item whose key is tombstoned goes to `refused` and is
  skipped, the others still apply); then the diff. A request of this
  producer that is not in `items` and was not refused is gone from the
  producer's side: open resolves `by: "agent"`, answering with no ack in
  this call resolves `by: "uncertain"`. An agent with no remaining item is
  removed, except one whose request just resolved `uncertain` or with a
  `rejected`/`failed` ack: it stays for 15 s with no request, so a client
  can show the resolution message on its card, and is then removed. A new
  item for the same `agent_key` ends the wait. The linger survives
  `DELETE` and the end of the lease. During it the agent counts as needing
  the user (intended: the message is the thing to read).
- Each item becomes a `kind: "question"` request with one single-select
  question, `effect: "choose"` options and `answer_path: "external"`. With
  no options and `free_text: false` the path is `none` and the request is
  read-only. A new key on the same `agent_key` resolves the previous open
  request `by: "agent"` (an answering one resolves `uncertain`). Changing
  the title, detail, question or options of a live key raises the
  revision; content is frozen while an answer is outstanding.

**`GET /external/v1/producers/{pid}/answers?wait_ms=N`** long-polls for
answers, `N <= 25000`, and returns `{answers}` without renewing the lease.
A producer may use `PUT` alone. At most one long-poll is in flight per producer: a second
`GET` for the same producer returns the current answers at once without
waiting. A poll holds one connection of the hook socket's 128-connection
cap, shared with the hook routes.

**`DELETE /external/v1/producers/{pid}`** ends the lease at once (204).

**Answers.** When a client answers, the request moves to `answering` and
the answer is queued with an `answer_id`, the request revision and
`expires_at_ms`, the wall-clock deadline in epoch milliseconds. acd counts
the window on a monotonic clock, which stands still while the machine
sleeps, and stamps each offer with the wall clock now plus the awake time
left. `expires_at_ms` may therefore differ between re-offers of the same
`answer_id`; a producer keeps the earliest value it saw. The window is `answer_timeout_ms` from the moment of the
answer. It is offered on every call until acked or expired. The producer dedupes by `answer_id`, records
the answer durably before acting and must not act after `expires_at_ms`;
it acks an expired answer `failed`. An answer to a producer without a live
lease fails with `source_unavailable` and the request stays open.

| Ack outcome | Meaning | Resolution |
|---|---|---|
| `applied` | the producer carried it out | `by: "inbox"` |
| `rejected` | refused before acting (stale, invalid input) | `by: "agent"`, reason `rejected`, `message` |
| `failed` | outcome unknown or partial (crash mid-action, timeout, expired) | `by: "uncertain"`, `message` (acd supplies "<agent title> reported the outcome as failed; check it there." when the ack has no message or only blank text) |

An ack for an unknown or already resolved answer goes to `ack_refused`;
the producer logs it and does nothing else. An answer that is not acked by
its deadline resolves the request `uncertain`.

**Lease.** With no `PUT` for `lease_ms`, or after `DELETE`, the producer's
agents are gone: open requests resolve `lost`, answering ones `uncertain`,
queued answers are dropped. A later `PUT` starts a new lease; keys
tombstoned earlier stay tombstoned. Expiry is checked every second and on
every producer call.

**Logging.** Logs name the producer id, keys and outcomes. They never
contain titles, detail, option labels, answer text or ack messages.
