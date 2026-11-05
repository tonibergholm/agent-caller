# Red team: dynamic attack on a running `acd-relay`

Date: 2026-11-04. Base: `claude/relay-impl-1104-lz759n` (PR #97, the relay
implementation; merged to `main` since) at `8b00ac1`. This is a dynamic,
black-box test: a real `acd-relay` built with `cargo build --release
-p acd-relay`, run as a subprocess against a fake FCM + OAuth endpoint,
attacked from separate OS processes. It is a sequel to
`2026-11-04-red-team-acd.md` (the daemon `acd`); that session's method and
format are not repeated here except where directly relevant. Reads with:
`docs/superpowers/specs/2026-11-03-relay-push-design.md` §3-5 and the trust
model (§4), `crates/acd-relay`, and `docs/research/2026-11-03-security-review-core.md`.

## Method

- Built `acd-relay` in release mode. No production source file is changed
  by this work: findings are reproduced as `#[ignore]`d Rust tests in
  `crates/acd-relay/tests/redteam.rs`, proven to discriminate (fail on the
  tree as it stands, pass with the named one-line fix applied locally,
  fail again once that fix is reverted) rather than as a doc-only claim.
- **Rig** (`tests/redteam-relay/`, new, not run in CI, kept out of
  `tests/redteam/` which is Python reproducers for the *daemon*):
  - `mkcerts.sh`: a throwaway CA and server certificate (`fcm.googleapis.com`
    + `127.0.0.1` SAN).
  - `fakefcm.py`: a stdlib-only (asyncio + `ssl`) TLS server standing in for
    both FCM's OAuth token endpoint and `messages:send`, with a plaintext
    control port (`/mode`, `/stats`, `/reset`) to switch each endpoint
    between `ok`, `401`, `429`, `500`, `unregistered`, `invalid_argument`,
    `redirect`, `garbage`, `stall_head`, `stall_body`, `slow_body`, `huge`,
    `huge_expires` on demand.
  - `up.sh`/`down.sh`: map `fcm.googleapis.com` to `127.0.0.1` in `/etc/hosts`
    for the duration (restored after), write a throwaway config pointing
    the real `acd-relay` binary at the fake FCM over TLS with the
    throwaway CA, and start/stop both processes. Needs root (port 443,
    `/etc/hosts`); local test relay only.
  - `rr.py`: a raw-socket HTTP/1.1 client (stdlib `socket`, no library that
    would normalize or refuse what an attacker's client would not) --
    needed to build malformed heads, duplicate/forged headers, oversized
    or short `Content-Length`, and to hold connections open without
    completing a request.
  - `nsrun.py`: brings `lo` up in a fresh network namespace (no `ip`
    binary available in this sandbox) so the hour-long soak runs isolated
    from, and without port conflicts with, the rest of this session's
    testing.
  - `soak.py`: the resource-exhaustion run (below).
- Attacked from many source addresses using `127.0.0.2`-`127.0.0.250`
  loopback aliases, the same technique as the daemon session (`lo` answers
  the whole `127.0.0.0/8` range).
- Ran the relay both with and without `trusted_proxy` set, matching the
  README's own example (`trusted_proxy = "127.0.0.1"`, relay also
  listening on `127.0.0.1`) and the default (no trusted proxy).
- For the wake client (`crates/acd-relay/src/client.rs`, `acd`'s side, not
  yet wired to the engine per its own doc comment), reviewed the existing
  `tests/wake_client.rs` suite first rather than re-deriving coverage it
  already has (see "Reviewed, already covered" below), then checked the
  one gap it does not dynamically exercise (TLS trust).
- Everything ran against disposable config directories under this
  session's scratch space; nothing outside it was touched. `/etc/hosts`
  was restored after every run (`down.sh`, guarded by `KEEP_HOSTS` only
  while a same-host follow-up run needed the mapping to stay).

## Attack table

Every attack attempted, including the ones that found nothing. "Outcome"
is what happened; a finding number appears where one resulted.

| # | Area | Attack | Outcome |
|---|---|---|---|
| 1 | Rate limits | Single source, garbage handles, drain `/v1/wake` | **Finding 1**: shared global budget drained past the source's own per-IP cap |
| 2 | Rate limits | Single source, valid-shaped registrations, drain `/v1/handles` | **Finding 1** (same bug, registration side) |
| 3 | Rate limits | Two `X-Forwarded-For` header lines from the trusted peer | No bypass found distinct from documented behavior: `HeaderMap::get` returns the first-inserted value, matching the existing `xff_is_honoured_only_from_the_trusted_proxy` test's own characterization ("every distinct XFF is its own source" from a trusted peer) -- see "Trust-boundary observation" below, not a new code finding |
| 4 | Rate limits | Comma-joined `X-Forwarded-For` (`"a, b"`) | Rightmost (`b`) used, as documented |
| 5 | Rate limits | Loopback co-residency: any local process, not just the configured reverse proxy, shares the TCP peer address `127.0.0.1` that `trusted_proxy` trusts | Confirmed: a process with no special privilege beyond reaching the loopback port gets an unlimited supply of fresh per-IP rate-limit buckets by choosing its own `X-Forwarded-For` value. Documented as a deployment trust-boundary note, not a code finding (the code does exactly what §4.6 specifies) |
| 6 | Crypto | Handle: flip every bit of every byte, one at a time | All rejected (`invalid_handle` or `gone` for the 2/182 flips that happened to land on the kid byte and produce an unused kid) |
| 7 | Crypto | Handle: truncate (drop last 10 chars, keep only first 5), empty string | All `invalid_handle` |
| 8 | Crypto | Handle: version byte 0 and 2 | Both `invalid_handle` |
| 9 | Crypto | Handle: kid byte set to an unused value (255) | `gone`, distinct from `invalid_handle` as designed |
| 10 | Crypto | Replay a valid handle 3x | All `204`; replay is by design (§4.5), limited only by the handle/address rate limits |
| 11 | Crypto | Nonce reuse: two handles sealed for the same address | Byte-different (fresh random salt each time); no collision observed |
| 12 | Crypto | Challenge brute force: 3 wrong codes then the real one | `challenge_failed` x3, challenge deleted at the attempt cap; the real code arriving after is `unknown_challenge` (matches `MAX_ATTEMPTS = 3`) |
| 13 | Crypto | 32 concurrent confirms of one challenge | Exactly 1 `200`, 31 `unknown_challenge`: single-use atomicity holds under concurrency |
| 14 | Crypto | Timing: median/stdev over 300 samples each of valid / bad-auth-tag / bad-kid / bad-base64 handles | All ~0.00014s median, no distinguishable timing side channel at this sample size |
| 15 | HTTP abuse | `GET`/`PUT`/`HEAD` on `/v1/wake` and `/healthz` | `405`/`200` as axum's routing gives by default; `/v1/wake` has no `GET` handler, so no handle-shaped data is ever read from a query string |
| 16 | HTTP abuse | `Content-Type: text/plain` with a JSON body | Accepted (content-type is not checked, by design -- `read_json` parses whatever bytes arrive); no exploit value found: the relay sets no cookies and needs no ambient credential, so this is not a CSRF vector (the "credential" is the handle itself, which a cross-site caller does not have) |
| 17 | HTTP abuse | Missing `Content-Type` header | Accepted (same) |
| 18 | HTTP abuse | Path traversal (`/v1/../v1/wake`, `%2f` encoded) | Both `404`; axum's router normalizes before matching |
| 19 | HTTP abuse | Duplicate JSON key (`{"v":1,"v":2,...}`) | `400 bad_request`: `serde`'s derived `Deserialize` rejects a duplicate field by construction (not an app-level check, but confirmed present) |
| 20 | HTTP abuse | Unknown field, wrong-typed `v` (string/float) | Both `400 bad_request`, matches the schema-pinning unit tests in `wire.rs` |
| 21 | HTTP abuse | Body at exactly 2048 bytes vs 2049 | 2048: parsed (then rejected downstream, e.g. `invalid_handle`, since the content was garbage) confirming the boundary is `<=`; 2049: would be `413` once content is large enough to exceed the axum body-limit extractor -- the 2048-byte case used a short valid-looking JSON envelope padded in the handle field, not a literal 2048-byte handle (handle is capped separately at 512) |
| 22 | HTTP abuse | 5000 pipelined `GET /healthz` on one connection, sent without ever reading a response | Completed instantly (`sendall` returned in <0.01s; the kernel socket buffer absorbed it) and the relay answered all 5000 once read back. No backpressure stall observed at this scale |
| 23 | HTTP abuse | 1024+ connections from one source, idle (connect only, no bytes sent) | The sandbox's own TCP stack refused further `connect()`s after ~183 (not the relay's 1024-connection semaphore -- an OS/container limit on this box); `/healthz` from a fresh connection still answered immediately while they were open, and again after the 10s head-timeout freed them. The relay's own cap and its deterministic enforcement are already covered by `tests/connections.rs`'s `a_connection_over_the_cap_is_served_only_after_a_slot_frees` (virtual-time, not real-socket-bound); not independently re-proven at 1024 real sockets in this sandbox |
| 24 | HTTP abuse | `slowloris`-style: hold a request head open without finishing it, many connections, several batches | Each is closed at the 10s head-timeout (consistent with `tests/connections.rs`); folded into the soak (below) rather than tested standalone |
| 25 | Wake/FCM path | Can a caller choose the push token, title, body, or data via the wake request? | No: `WakeRequest` has exactly `v` and `handle` (pinned by `wake_request_has_exactly_v_and_handle`/`...rejects_extra_text_field` in `wire.rs`); the FCM body's `title`/`body` are the `WAKE_TITLE`/`WAKE_BODY` constants, never derived from the request |
| 26 | Wake/FCM path | Wake another account's device | Not reachable: the only way to target a device is to hold ITS sealed handle, and a handle only comes from confirming a challenge pushed to that device's own push address |
| 27 | Wake/FCM path | FCM error body leaking credentials/addresses to the caller | `map_fcm_error` only inspects `error.status`/`error.details[].errorCode`; the relay's own HTTP answer to the wake caller is always one of the fixed `{"error":"..."}` shapes, confirmed by sending FCM bodies containing a literal canary string (`SECRET-LEAK-CANARY-*`) in `401`/`429`/`500`/`invalid_argument` fake responses and checking the caller-visible body never contains it |
| 28 | Wake/FCM path | FCM returns a redirect (302) | Not followed; `http.rs` has no redirect-following code. Classified `unavailable` (empty body fails FCM-error JSON parsing, falls to the default) |
| 29 | Wake/FCM path | FCM response: huge (unbounded chunked body, no `Content-Length`) | Bounded: the relay's `HttpClient` is constructed with `max_body = 16 KiB` (`main.rs`) and stops reading once that many bytes have arrived, then drops the connection; observed RSS delta negligible, call returned immediately, no hang |
| 30 | Wake/FCM path | FCM response: stalls after sending headers but before any body (`stall_body`) | Bounded by `SEND_DEADLINE` (10s); `503 unavailable` |
| 31 | Wake/FCM path | FCM response: a malformed/garbage status line | Handled; `503 unavailable`, no panic |
| 32 | Wake/FCM path | FCM OAuth token response: `expires_in` = `u64::MAX` | Accepted at face value (`huge_expires` mode) -- see "Reviewed, not a new finding" below |
| 33 | Resource use | Detached-task fd leak: does the outbound `HttpClient`'s spawned connection task leak a socket when the outer deadline fires while FCM stalls (`stall_head`)? | No leak observed: 0 fd delta across 20 concurrent stalled sends. Hyper's `Connection` task observes its paired `SendRequest` sender being dropped (when the cancelled future's locals are dropped) and winds the connection down itself; `http.rs`'s own `task.abort()` on the happy path is not the only thing keeping this bounded |
| 34 | Wake client (`acd` side) | Hostile relay: huge body (5 MiB), malformed/hostile bodies, redirects not followed, backoff math, outage-vs-invalid distinction | Already covered by the existing `tests/wake_client.rs` suite (`hostile_bodies_change_nothing_but_the_status`, `redirects_are_not_followed`, `backoff_doubles_and_caps`, `outage_never_disables_but_invalid_registration_does`, `transient_outage_then_recovery`); reviewed, re-run, all pass; no gap found worth a new reproducer |
| 35 | Wake client (`acd` side) | Wrong TLS certificate / no pin | Not independently re-tested with a dedicated hostile-TLS Rust harness (the wake client is not yet wired to the engine, per `client.rs`'s own doc comment, and `WakeSender::https` shares the exact `HttpClient::https` code already exercised, with a real mismatched-CA scenario, by every fake-FCM run in this session -- the relay refuses to use a CA it was not given, which is the same mechanism). See "Not tried" |
| 36 | Resource use | 1-hour mixed hostile soak: slowloris (400 conns), oversized-head holders (100 conns, 300 KB padding header), `/healthz` pipeliners (20 conns), garbage wakes, registrations, legitimate wakes through a cycling set of FCM fault modes, sampled every 10s | See "Resource exhaustion" below |

## Findings

### Finding 1 -- shared global rate-limit budget is spent by requests that are rejected anyway (severity: unauthenticated remote, single source, no valid handle needed)

**What.** `wake_inner` and `register_inner` check the **global** rate-limit
bucket before the **per-IP** (and, for `wake`, per-handle/per-address)
bucket, matching the spec's own stated order (§5.3.2, "global limit;
per-IP limit"). `Limiter::check` / `Counter::hit` increments and allows a
request, or refuses it, but a request that is later rejected by a
*different*, cheaper check never gets its global hit refunded. So once a
single source's own per-IP budget is spent, every further request it
sends keeps charging the **shared** global bucket for nothing -- the
request is always going to be rejected by the per-IP check a moment
later, but the global counter has already been spent. With wake's default
numbers (global 6000/hour, per-IP 600/hour), ten sources acting within
their own per-IP budgets already exhaust the global bucket for everyone;
with the bug, a **single** source past its own budget can keep draining
the rest of the global budget indefinitely (bounded only by the global
cap itself), denying wakes and registrations to every other handle,
address and IP on the relay.

- **Severity**: unauthenticated, remote, single source, no valid handle or
  address needed (garbage handles are enough for `/v1/wake`; any
  syntactically valid address is enough for `/v1/handles`). Impact:
  denial of alerts / denial of registration to every other user of the
  relay, for up to an hour (until the global window resets) at a time,
  repeatable.
- **Reproducer**: `crates/acd-relay/tests/redteam.rs`,
  `one_source_cannot_drain_the_global_wake_budget_past_its_own_ip_limit`
  and `one_source_cannot_drain_the_global_registration_budget_past_its_own_ip_limit`
  (`cargo test -p acd-relay --test redteam -- --ignored`). Also reproduced
  against the real release binary and fake FCM:
  `tests/redteam-relay/a_global_drain.py` (`./up.sh $W && python3
  a_global_drain.py`) -- one source IP sending garbage wakes brings a
  victim on a different IP, with its own valid handle, from `204` to
  `429`; the same for registration, with 10 successful attacker
  registrations (its own per-IP cap) followed by 790 429s that still deny
  a victim's registration from a fresh IP.
- **Observed result (current tree)**: both `#[ignore]`d tests fail --
  `assertion left == right failed: victim wake must still succeed, got
  429 {"error":"rate_limited"}` (left 429, right 204), and the same shape
  for registration (429 vs 202).
- **Minimal fix**: swap the check order so the per-IP (and, for wake,
  per-handle/per-address) checks run *before* the global check, in both
  `wake_inner` and `register_inner` (`crates/acd-relay/src/server.rs`).
  This was applied locally, confirmed to make both tests (and the full
  existing `acd-relay` suite, 10/10 plus the full crate suite) pass, then
  reverted -- no production code is changed by this session; see
  "Verification" for the before/after proof.
- **Residual note for the fix's author**: after reordering, the global
  check is still the *last* line of defense against many distinct,
  individually-within-budget sources (ten honest-looking IPs each at
  their own 600/hour cap already sum past the 6000/hour global cap by
  design -- that part is not a bug, it is the documented backstop,
  §4.6). The bug this finding fixes is specifically: a request *rejected*
  by a cheaper check must not also consume the *shared* resource.

### Trust-boundary observation -- `trusted_proxy` trusts the TCP peer address, which loopback does not make unique to the configured proxy (not a code finding)

**What.** §4.6 says `X-Forwarded-For` is honoured "only when the TCP peer
equals the configured `trusted_proxy`", and the relay's own
`xff_is_honoured_only_from_the_trusted_proxy` test already demonstrates,
and accepts, that from that trusted peer "every distinct XFF is its own
source". The README's own example sets `trusted_proxy = "127.0.0.1"` with
the relay also listening on `127.0.0.1`. On that deployment, the TCP peer
address `127.0.0.1` is not unique to the intended reverse proxy -- it is
shared by **every** local process on the host (or, in a container/VM
where the relay's loopback port is reachable from more than the proxy's
own process, every tenant that can reach it). Any such process, with no
special privilege beyond being able to open a TCP connection to the
relay's port, can set its own `X-Forwarded-For` value and get a fresh,
unlimited-by-rotation per-IP rate-limit bucket -- fully defeating the
per-IP layer of §4.6 for that deployment, independent of Finding 1.

- **Severity**: local (or shared-loopback) attacker only; does not apply
  to a remote attacker who cannot make the TCP peer address equal
  `trusted_proxy` (that would require IP spoofing *and* completing a TCP
  handshake, which the real network path prevents).
- **Reproducer**: `tests/redteam-relay/c_xff_loopback_bypass.py`: 601
  wakes claiming `X-Forwarded-For: 9.9.9.1` from the real loopback peer
  exhaust that claimed IP's 600/hour budget (`{400: 600, 429: 1}`); the
  very next wake, claiming a brand-new `9.9.9.2`, is immediately allowed
  again (`400`, not `429`) -- a fresh budget on demand, from the same
  real attacker.
- **Not a code finding**: the code does exactly what the spec specifies
  and what an existing, passing test already documents as intended
  behavior for a trusted peer. No `#[ignore]`d test is added for this;
  adding one would contradict the already-passing
  `xff_is_honoured_only_from_the_trusted_proxy` test, which asserts the
  same mechanism as correct. Recorded here as a deployment-hardening note
  for whoever owns the README/deployment docs (out of this session's
  scope to edit; see the PR body): IP-based proxy trust needs the
  relay's listen port to be reachable **only** by the real proxy (a
  dedicated network namespace, a Unix socket, or a shared secret header
  from the proxy), not merely "the proxy also happens to use this
  address".

## Resource exhaustion (1-hour soak)

<!-- SOAK-RESULTS -->

## Reviewed, not a new finding

- **OAuth token `expires_in: u64::MAX`** (attack #32): `FcmClient::access_token`
  computes `expires_at = now + ttl` as a plain `u64` add. In the release
  binary (no overflow checks) `now + u64::MAX` wraps to `now - 1`: the
  token is treated as already expired rather than as never expiring.
  Confirmed dynamically: with the fake OAuth endpoint returning
  `expires_in: 18446744073709551615` on every call, the relay requested a
  fresh token on **every** subsequent wake (`token_requests` advanced by
  exactly one per wake) instead of caching it. This fails in the safe
  direction (extra OAuth round-trips, not an unbounded trust period) and
  is not a security finding, but it is a real availability footgun if a
  genuinely misbehaving (not malicious -- FCM is a trusted dependency)
  OAuth endpoint ever sent an oversized `expires_in`: every send would pay
  a full token round-trip, and a slow or rate-limited OAuth endpoint would
  then show up as `SendError::Unavailable` on every wake rather than only
  periodically. Not pursued as a finding since it requires FCM itself to
  misbehave, which is outside this design's threat model (§4.1: FCM is
  "not involved" / only untrusted for content and timing, not for
  protocol correctness).
- **Concurrent-confirm atomicity, challenge attempt limit, handle crypto,
  timing**: all reviewed dynamically (attacks #6-14); no bug found, see
  the attack table.
- **Wake client against a hostile relay** (huge/redirect/hostile bodies,
  backoff, outage-vs-invalid): already covered by the existing
  `tests/wake_client.rs` suite; re-run, all pass, no gap worth a new
  reproducer (attack #34).

## Not tried / not run

- **Wrong-TLS-certificate dynamic test for the wake client specifically**
  (`crates/acd-relay/src/client.rs`'s `WakeSender::https`): the client is
  not yet wired into `acd`'s engine (its own doc comment: "Not wired to
  the engine yet (P5b)"), so there is no running daemon path to attack it
  through. `WakeSender::https` and the relay's own outbound
  `FcmClient`/`HttpClient::https` share the same TLS-trust code
  (`src/http.rs`), and every fake-FCM run in this session exercised that
  code path with a real mismatched-CA scenario working correctly (the
  relay only trusts the CA it was given); a dedicated harness spinning up
  a second hostile TLS listener specifically for `WakeSender` was not
  built, since it would exercise the identical `rustls` construction
  already proven live elsewhere in this session.
- **Clock jumps** (a real wall-clock `date -s` jump): not run; this
  sandbox session did not have a safe way to jump the system clock
  without affecting other concurrent sessions on the same container.
  `limits.rs`'s own fixed-window and sweep-boundary logic is covered by
  six existing unit tests with a `ManualClock`
  (`window_blocks_after_limit_and_resets`,
  `a_sweep_keeps_a_counter_whose_minute_window_outlives_the_hour_window`,
  and others) that already drive exactly the hour/minute boundary cases a
  real clock jump would hit; reviewed, not independently re-run against
  wall-clock time.
- **1024 real concurrent connections from one source**: this sandbox's
  own TCP stack refused further loopback connects after ~183 (attack
  #23), below the relay's own cap; the cap's enforcement and the
  HEAD_TIMEOUT recovery are proven deterministically (virtual time, no
  OS socket ceiling) by the existing `tests/connections.rs` suite instead.
- **APNs / iOS**: not implemented yet (phase P8 per the spec); nothing to
  attack.
- **A real Firebase project / real FCM**: this session used a
  same-shape, locally-hosted fake per the design's own test guidance
  (`README.md`: "The FCM client is tested only against in-process fakes
  of the documented request and response shapes. Run `validateOnly`
  against a real project before relying on it.").

## Verification

<!-- VERIFY-RESULTS -->
