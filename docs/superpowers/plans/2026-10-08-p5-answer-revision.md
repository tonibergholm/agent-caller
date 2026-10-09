# P5 follow-up: answer revisions

## Problem

`Registry::update_request` (crates/acd-core/src/registry.rs:319) rewrites an
open request in place. It keeps the id and replaces `kind`, `title`,
`detail`, `options`, `questions` and `free_text`. `request.answer` carries
only `request_id` and `option_id`. A client can therefore review one
version of a request and have its answer applied to another. Option ids
are adapter-chosen, often digits, so "1" can mean something else after an
update.

The Mac app checks semantics just before it submits (acd-client
`inbox.rs`, `actions::check`). That narrows the window but cannot close
it, because the change can land while the command is in flight.

Freezing content once a request is `Answering` (registry.rs:321–326)
already protects the claimed-to-delivered window. The gap is from the
client's last view to `claim`.

## Change

1. **acd-protocol**
   - `Request` gets `revision: u64`, serialized always. On decode,
     `#[serde(default)]` makes a missing field read as 0, so old servers
     still parse.
   - `ClientMessage::RequestAnswer` gets
     `revision: Option<u64>` (`default`, `skip_serializing_if = "Option::is_none"`).
   - `ErrorCode` gets `RequestChanged`, with the message "the request
     changed; review it again". Old clients map it to `Unknown`, which is
     already tolerated (Task 1).
   - The welcome capabilities add `"answer.revision"`. A client only
     relies on the check when the server advertises it.
   - Regenerate the JSON schemas and TS bindings. Update PROTOCOL.md.
2. **acd-core**
   - A new request starts at revision 1.
   - Every semantic mutation bumps `revision` with checked arithmetic,
     never wrapping. That covers `update_request` (changes to `kind`,
     `answer_path`, `title`, `detail`, `options` or `questions`, with
     options and questions compared in full and in order, and
     `free_text`) and `Observation::RequestPath` (registry.rs:130), which
     sets `answer_path` directly. `ready` alone does not bump it, since
     `claim` checks readiness separately. `raw_screen` alone does not bump
     it either, because today it is set only on unanswerable fallback
     requests. Any answer-relevant screen context added later must be
     versioned or immutable.
   - `claim(request_id, answer, revision: Option<u64>)` returns
     `ErrorCode::RequestChanged` when it gets `Some(r)` and `r` differs
     from the current revision. The check runs under the same lock, after
     the lifecycle checks and before answer validation, so a removed
     option reports `RequestChanged`. Nothing is claimed on a mismatch.
     `None` keeps today's behaviour for old clients.
   - Idempotency: today claim failures return `Delivery::Attempted`
     (engine.rs:135, :170). Add `RequestChanged` to the uncached
     exclusions, so `idem.finish(.., false)` releases the id.
3. **acd-client**
   - `Semantics` stores `revision`. `Command::Answer` carries it.
   - The connection sends `revision` only when the session advertises
     `answer.revision`.
   - `RequestChanged` maps to the notice "The request changed. Nothing
     was sent." (Warning). It is not retried.
4. **Adapters**: no change in P5. Revisions are owned by the registry.
   herdr keys targets are fetched after the claim, from a replaceable
   map (keys.rs:171, :330), so a revision alone does not bind them. P3,
   which registers production keys targets, must keep a target
   immutable per occurrence or bind it to the revision or digest that
   was claimed. This goes into the P3 obligations in
   docs/superpowers/plans/2026-10-07-p1-followups.md.

## Tests

- registry: semantic update bumps the revision; a raw_screen-only update
  does not; a claim with a stale revision returns RequestChanged and
  leaves the request Open and unclaimed; a claim with the current
  revision succeeds; `None` behaves as today.
- registry: a RequestPath change bumps the revision; an answer captured
  before it gets RequestChanged.
- engine/server: an update between the client's view and the claim gives
  RequestChanged with zero adapter deliveries. The same command id
  reused with the corrected revision succeeds. An old-style answer still
  works.
- protocol: Request decodes without `revision`; `RequestChanged` decodes;
  an old decoder maps `"request_changed"` to `Unknown`; schemas match.
- acd-client: a fake server that advertises `answer.revision` gets the
  revision in the frame; one that does not advertise it never gets the
  field; RequestChanged produces the notice.

## Compatibility

- Old client against new server: no revision is sent, so the behaviour
  is unchanged (the gap remains for that client).
- New client against old server: no capability is advertised, so the
  client omits the field and keeps its pre-submit check.
- Wire changes are additive. acd/1 stays acd/1.

## Out of scope

- Hook answer paths (P3) pass through `claim`, so they get the revision
  check. Keys targets still need the binding described in step 4.

## Review

Sol reviewed the plan on 2026-10-08. Accepted: bump on RequestPath, make
the uncached rejection explicit, put the check after lifecycle and
before answer validation, test the old-decoder fallback, and use checked
overflow. The herdr target binding is deferred to P3 as an obligation.
Sol rejected a content hash (A→B→A passes) and new ids per change (they
conflict with spec §5.1). The counter stays.
