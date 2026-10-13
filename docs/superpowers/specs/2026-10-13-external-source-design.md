# External source: design

Status: approved by the user 2026-10-13, after Sol's review of the merged
plan. Target release 0.9.0.

## 1. Purpose

A local program (a *producer*) can put questions in the inbox and act on
the answers itself. acd stays a mailbox: it never runs a command for a
producer. The producer pulls answers, carries them out, and reports the
outcome.

The first producer is a supervisor that publishes "needs you" items (a
plan awaiting approval, a parked job) and accepts approve, abandon and
typed corrective answers. Nothing in acd is specific to it.

## 2. Transport and authentication

- The producer API is served only on the Unix hook socket `hook.sock` in
  the acd config directory, never on TCP. The daemon builds a separate
  router for the Unix listener. The TCP hook router stays as it is and
  does not contain `/external` routes. A test asserts that an
  `/external` request over TCP gets the same response as any unknown
  route.
- Every call carries `X-Acd-Token` with the hook token, as hook calls
  do. The producer id is not authenticated: any hook-token holder can act
  as any producer (same-user trust boundary), so a producer must not treat
  an acd ack as authority for an irreversible action.
- A producer verifies the socket before connecting, using the same
  checks as the hook shim (`crates/acd/src/hook_shim.rs`): canonical
  directory, safe ancestors, socket not a symlink, owned by the user.
- Older daemons answer `/external` with 401. Producers treat 401, 404,
  connection failure and timeouts alike: no inbox for now, retry later.

## 3. Identity

- Producer id `pid`: `^[a-z0-9][a-z0-9-]{0,31}$`.
- Item key: `^[A-Za-z0-9._:-]{1,128}$`. Agent key: the same grammar.
- Agent id is derived from `(pid, agent_key)`, request id from
  `(pid, key)`. Both are opaque to clients.
- Keys are **not reusable**, like T3 request ids. A resolved key is
  tombstoned and never opens another request. The producer builds each
  key from its own durable occurrence id plus a hash of the card's
  meaning, so it opens a new occurrence by minting a new key.
- Tombstones live in memory, bounded to the most recent 4096 per
  producer (and 16 producers without a lease), and do not survive a daemon restart. The producer's durable
  occurrence record is the authority. A producer never lists a key it
  has recorded as resolved.

## 4. API

### 4.1 `PUT /external/v1/producers/{pid}/state`

One call is the heartbeat, upsert, ack and fetch.

Request:

```json
{
  "lease_ms": 60000,
  "answer_timeout_ms": 180000,
  "items": [Item],
  "acks": [Ack]
}
```

- `lease_ms` in `30000..=900000`. `answer_timeout_ms` in
  `30000..=900000`, default `180000`.
- Limits: at most 8 live producers, 32 items, 8 options per item, 32
  acks. Title 200 bytes, detail 4000, option label 80, option
  description 200, question text 1000, ack message 500, body 256 KiB.
  Text must pass `has_forbidden_control` and the typed-answer rule (no bidi
  overrides or line/paragraph separators).

**Atomicity.** acd validates the whole body before touching the
registry. Any violation, including a duplicate `key` or `agent_key` in
`items`, returns 422 and changes nothing: no lease renewal, no acks and
no diff. The time-driven sweep (lease and answer expiry) that starts
every call still runs. A malformed or truncated body can therefore never resolve an
existing card.

After validation the steps run in this order, under one registry lock:

1. Renew the lease.
2. Apply `acks` (§4.4).
3. Upsert `items`. An item whose key is tombstoned goes into `refused`
   with reason `resolved` and is skipped. Skipping it does not affect
   other items.
4. Diff: a request of this producer that is not in `items` and was not
   refused is gone from the producer's side:
   - open → resolved `by: agent` (answered elsewhere or withdrawn);
   - answering with no ack in this call → resolved `uncertain`.

Response 200:

```json
{
  "answers": [{"answer_id": "…", "key": "…", "revision": 7,
               "option_id": "abandon", "text": null,
               "expires_at_ms": 1760000000000}],
  "refused": [{"key": "…", "reason": "resolved"}],
  "ack_refused": [{"answer_id": "…", "reason": "resolved"}]
}
```

### 4.2 `GET /external/v1/producers/{pid}/answers?wait_ms=N`

Optional long-poll, `wait_ms <= 25000`. Returns the same `answers` list
without renewing the lease. v1 producers may poll with PUT only.

### 4.3 `DELETE /external/v1/producers/{pid}`

Clean stop. Same effect as lease expiry (§5).

### 4.4 Item

```json
{
  "key": "occ-41:3f2a9c1b0d4e",
  "agent_key": "parked:ENG-12",
  "agent": {"title": "native autopilot", "project": "engine", "cwd": "/…"},
  "title": "ENG-12 parked: repair exhausted",
  "detail": "…",
  "question": {"header": "Parked", "text": "…"},
  "options": [{"id": "abandon", "label": "Abandon", "description": "…",
               "confirm": true}],
  "free_text": true
}
```

- acd creates a request with `kind: question`, one single-select
  question, `effect: choose` and `answer_path: external`.
- No options and `free_text: false` gives `answer_path: none`, shown
  read-only.
- One open request per `agent_key`. A new key on the same `agent_key`
  resolves the previous open request `by: agent`.
- Changing title, detail, question or options of a live key bumps the
  revision.

### 4.5 Answers

- When a client answers, the request moves to answering and the answer
  is queued for the producer with an `answer_id`, the request revision
  and `expires_at_ms = answered_at + answer_timeout_ms`, counted on a monotonic
  clock and stamped on the wire as wall clock now plus the awake time left; it may differ
  between re-offers, so producers keep the earliest value they saw.
- Answers are re-offered on every call until acked or expired. The
  producer dedupes by `answer_id`, records the answer durably before
  acting, and **must not act after `expires_at_ms`**. It acks an
  expired answer `failed`.
- On expiry without an ack, acd resolves the request `uncertain`.
- An answer to a producer with no live lease fails with
  `NotDelivered(SourceUnavailable)`, as for other sources.

### 4.6 Ack

```json
{"answer_id": "…", "outcome": "applied|rejected|failed", "message": "…"}
```

| Outcome | Meaning | Resolution |
|---|---|---|
| `applied` | the producer carried it out | `by: inbox` |
| `rejected` | the producer refused it before acting (stale, invalid input) | `by: agent`, reason `rejected`, `message` shown |
| `failed` | outcome unknown or partial (crash mid-action, timeout, expired) | `uncertain`, `message` shown |

An ack for an unknown or already resolved answer goes into
`ack_refused` with reason `resolved` or `unknown`. The producer logs it
and does nothing else.

**Resolution notices.** An agent whose request resolved `uncertain` or
with a `rejected`/`failed` ack stays 15 s without a request after the
producer stops listing it, so clients can show the message on its card.

Long-polls share the hook socket's 128-connection cap.

## 5. Lease

With no PUT for `lease_ms`, or after DELETE, the producer's agents are
gone: open requests resolve `lost`, answering ones `uncertain`. Queued
answers are dropped. A later PUT starts a new lease. Keys tombstoned
earlier stay tombstoned.

## 6. Protocol changes (acd/1, additive)

- `Source::External` and `AnswerPath::External`. Older clients decode
  unknown values as read-only (`model.rs`).
- `Machine.sources.external: bool`, with a serde default.
- `ResolveReason::Rejected`, and an optional `message` on
  `request.resolved` (max 500 bytes).
- An option field `confirm: bool`, default false. Clients ask for a
  second tap before sending that option.
- `free_text` is allowed on `t3` and `external`. Update the
  PROTOCOL.md §6 sentence "Typed answers are supported only on the T3
  path".
- No new capability string. Clients that do not know `external` show
  these cards read-only, so they never send an abandon without the
  confirm step.
- Regenerate the JSON schema in `crates/acd-protocol`.

## 7. Client work

- `acd-client` view: show `message` on resolved cards, including
  `by: agent` with reason `rejected`, which shows nothing today.
- Mac and iOS (`Card.svelte`, `Row.svelte`): source label "external"
  uses the producer's `agent.title`. Confirm options need a second tap.
  Free text is enabled for `external`.
- Omarchy (`clients/omarchy`, `model.mjs`): the same three changes.

## 8. Logging and privacy

Logs name the producer id, keys and outcomes. They never contain
titles, detail, option labels, answer text or ack messages.

## 9. Tests

- The TCP listener has no `/external` routes.
- 422 on a duplicate key, a duplicate agent_key or any limit violation,
  and the registry is unchanged afterwards.
- Omission diff: open → `by: agent`; answering without ack →
  `uncertain`.
- Ack outcomes map as in §4.6. A late ack goes into `ack_refused`.
- Answer expiry gives `uncertain`. Lease expiry gives `lost` and
  `uncertain`.
- A tombstoned key is refused, and other items in the same call still
  apply.
- `confirm` and `message` round-trip through the protocol, and clients
  render them.

## 10. Decisions

| Decision | Choice | Why |
|---|---|---|
| Who executes | The producer | acd stays a mailbox with no command surface |
| Transport | Unix socket only | No network exposure for a write path |
| Key reuse | Not reusable, the producer mints occurrences | Content hashes cannot reopen a timed-out card (Sol) |
| PUT validation | All or nothing | A bad body must never resolve cards (Sol) |
| Interrupted action | `failed` → `uncertain` | A crash mid-action is not a refusal (Sol) |
| Abandon safety | `confirm` option flag | Destructive answers need a second tap; old clients are read-only |
