# Red team: dynamic attack on a running `acd`

Date: 2026-11-04. Base: `main` at `3404287` (includes #87 sec-local-adapters,
#89 hello-gate test hardening, #90 sec-core-log-notify). This is a dynamic,
black-box test: a real `acd` built with `cargo build --release -p acd`, run
as a subprocess, attacked from separate OS processes. It is a sequel to the
two static reviews (`2026-11-03-security-review-core.md`,
`2026-11-03-security-review-local.md`), which read `main` at `108034c`; the
delta table below says what each of their findings looks like now, and new
findings are reported only where they are genuinely new or a live bypass of
a since-landed fix.

## Method

- Built `acd` in release mode (`cargo build --release -p acd`) and ran it as
  `acd run` against throwaway config directories under `/tmp` (socket paths
  must be short — `AF_UNIX`'s `SUN_LEN`, so the project's own scratch
  directory, with its long per-session path, cannot hold `acd.sock`).
  `listen = ["127.0.0.1"]` (not `"loopback"`, which also binds `::1`; this
  sandbox has no IPv6) on a TCP port, plus the Unix sockets, the hook TCP
  port and `hook.sock`.
- Paired with `acd pair --host 127.0.0.1`, then spoke the wire protocol
  directly: a hand-rolled WebSocket client (`tests/redteam/acdws.py`,
  stdlib `ssl` + raw RFC 6455 framing — needed anyway to build malformed,
  oversized, fragmented and unmasked frames a conforming client library
  would refuse to construct) over TLS with `ssl.CERT_NONE` (an attacker has
  no pin either).
- Opened real `PermissionRequest`s by POSTing a recorded Claude Code hook
  payload (`tests/fixtures/hooks/claude/payload-PermissionRequest-Bash.json`)
  to `hook.sock`, to get live, answerable `request_id`s for the TOCTOU and
  idempotency cases.
- Attacked from many source addresses using `127.0.0.2`–`127.0.0.250`
  aliases (Linux's `lo` answers the whole `127.0.0.0/8` range, so
  `bind((src_ip, 0))` before `connect` is enough — no network namespaces
  needed) to simulate a multi-source attacker without one.
- For the local-attacker surface, created a second, unprivileged account
  (`useradd redattacker`) and ran attacks with `runuser -u redattacker`,
  since this session itself runs as root (`id -u` = 0) and a root-vs-root
  test would prove nothing about file permissions.
- For the Omarchy `acd-inbox` socket, this sandbox has no D-Bus session
  bus, so `acd-inbox serve`'s own `main.rs` (which initializes the real
  Secret Service keyring over D-Bus) cannot run directly. Used the crate's
  own `common::rig` test fixture instead — the *same* `Service` +
  `UnixListener` + JSON-lines protocol code that `serve::run` wires up,
  with `NoKeyring` and an in-memory `SecretStore` standing in only for the
  keyring backend — and attacked that real socket from a new Rust
  integration test (`crates/acd-inbox/tests/redteam_socket.rs`), since
  Python cannot reach into the test process that owns the socket's
  lifetime. No production code changed.
- Ran an hour-long mixed hostile soak (pre-auth churn from many sources +
  an authenticated flooding client + hook-endpoint idle-connection/malformed
  abuse) against a dedicated daemon, sampling RSS/thread/fd counts every
  10s (`tests/redteam/soak_sample.py`, `soak_attack.py`).
- Measured `hello.token` comparison timing across many interleaved samples
  of two token classes, and the sustained single-source wrong-token rate.
- Tested process-level behavior directly: `SIGTERM`/`SIGKILL`, stale-socket
  recovery on restart, and symlink races on `client.token` and `hook.sock`
  (these need no second user: the daemon's own `O_NOFOLLOW`/`openat` checks
  are kernel-atomic, not a userspace check-then-act race, so they're
  reproducible deterministically rather than by racing).
- Everything above ran against disposable config directories this session
  created; nothing in `~/.config/acd` or any real user's data was touched.
  No production source file was changed; two *new* test files were added
  (`crates/acd-inbox/tests/redteam_socket.rs`,
  `crates/acd-core/tests/redteam_idem_exhaustion.rs`), both passing except
  the latter's single `#[ignore]`d test, which fails on purpose to
  document a finding (see below) and does not run in the normal suite or CI.

## What the static reviews' findings look like today

| # | Finding | Status at `3404287` |
|---|---|---|
| core #1 | 8 idle pre-auth sockets lock out every client, including the local socket | **Partially fixed, live bypass found.** The local Unix socket now has its own pool (`local_preauth`), confirmed unaffected throughout. The per-source cap (`tls::Admission`, 2/source) stops the *single-attacker* case the finding described. But the smaller, shared, **source-blind** post-upgrade `preauth` semaphore (8 permits, `crates/acd-core/src/server.rs`) is still exhaustible — just now needs ≥4 distinct source addresses (2×4=8) instead of 1. See **Finding 1**. |
| core #2 | No notification for a request id reused after an acd restart | Client-side (`acd-client`/the Mac app), out of this daemon-only session's scope; the static review's own fix (`8d68064`) already landed. Not retested (no Mac app or `acd-inbox`-as-a-client running against this daemon in this sandbox). |
| core #3 | Hostile peer exhausts client memory via unbounded frames/agent state | Client-side; fix landed (`9ab7535`, "Bound client frames and agent state"). Out of scope for a daemon-only dynamic test — this session attacked the *server*, which already enforces its own 1 MiB frame cap (confirmed, see **Attacks tried**). |
| core #4 | acd logs no connection/auth event | **Fixed, confirmed live.** `hello rejected`/accepted log lines carry `peer`, `rejected_total`, `suppressed`; grepped the real daemon log throughout this session — token value never appears. |
| core #5 | Notification flood / unbounded dedupe set | Client-side; fix landed (`a981d53`). Out of scope (no app/`acd-inbox` client attached). |
| core #6 | Close code 4408 causes a reconnect storm | Client-side; fix landed (`1ba0a9d`). Out of scope, same reason. |
| core #7 | Pairing host accepts control/bidi characters | Client-side (`acd_protocol::pairing`); fix landed (`9ab7535`). Not retested (library-level, not reachable by attacking the running daemon). |
| core #8 | `agent.send` has a weaker character rule than answers | **Fixed, confirmed by source.** `Registry::check_send` now calls `acd_protocol::text::has_forbidden_chars` (`crates/acd-core/src/registry.rs:777`). Not dynamically exercised: every agent this sandbox can create (hook-sourced, no herdr/T3) has `can_send: false`, so `agent.send` always fails earlier with `not_supported` before the character check — matches the static review's own scope limit. |
| local F1 | Omarchy panel hides the tail of a long command | Rendering-only (QML); this sandbox has no Quickshell/Qt either. Not retested. |
| local F2 | T3 bearer token goes to a loopback squatter | No `t3.url` configured in any test daemon here (and no real T3 server available); not retested dynamically. Static finding stands. |
| local F3 | Hook listeners starved by idle connections, no read timeout | **Fixed, confirmed live.** 64 idle connections on the hook port got no response-delaying effect on a concurrent legitimate request (0.01s), and all 64 were closed server-side within 32s. See **Attacks tried**. |
| local F4 | herdr discovery trusts any socket it can connect to | Needs `herdr.extra_sockets` pointed at a shared-writable directory; not reconfigured/retested here (this sandbox has no herdr running either way). Static finding stands as described. |
| local F5 | Service files built from unescaped paths (newline injection) | Self-injection only (the invoking user's own environment); not retested (out of this session's threat model — attacking a *running daemon*, not `acd service install`). |

## Attacks tried

| # | Category | Attack | Outcome |
|---|---|---|---|
| 1 | Network/pre-auth | Distributed pre-auth pool exhaustion, 2/3/4 distinct `127.0.0.x` sources × 2 held WS-upgraded, hello-withheld connections | **4 sources (8 held) reliably locks out new TLS clients** (HTTP 503); 2 and 3 sources (4, 6 held) do not. Local socket unaffected at every count. See Finding 1. |
| 2 | Network/pre-auth | Same, raw-TCP-only and partial-TLS-only (no WS upgrade) from one source, 20 connections held 8s | No effect; per-source cap (2) bounds these at the `tls::Admission` layer before they ever reach the smaller `preauth` pool. |
| 3 | Network/pre-auth | Slowloris stage C: TLS-complete, partial HTTP upgrade headers, held | Legitimate client transiently refused (503) during a run with the long-soak's ambient pre-auth churn also active — consistent with Finding 1, not a separate bug; not independently reproduced in isolation (would need ≥4 sources at this stage too). |
| 4 | Network/pre-auth | Slowloris stage D: WS-upgraded, `hello` trickled one byte then stalled, 1 source | No effect (per-source cap). |
| 5 | WebSocket frames | Oversized single frame (2 MiB) pre-auth and post-auth | Pre-auth: closed, 4401. Post-auth: connection reset (not a graceful close frame, but terminated — no resource effect observed). |
| 6 | WebSocket frames | Fragment bomb: ~300 × 4 KiB continuation frames, total over the 1 MiB message cap | Connection reset; no crash, no resource growth. |
| 7 | WebSocket frames | Fragment flood: 50,000 one-byte continuation frames forming one valid (whitespace-padded) JSON message, under the size cap | **No effect** — server reassembled and answered correctly in 0.23s. Fragment *count* is not a distinct resource. |
| 8 | WebSocket frames | Unmasked client frame (RFC 6455 violation) | Closed, 4401. |
| 9 | WebSocket frames | RSV bits set, no negotiated extension | Closed, 4401. |
| 10 | WebSocket frames | Reserved/invalid opcode (0x3) | Closed, 4401. |
| 11 | WebSocket frames | Lone continuation frame (no preceding start) | Closed, 4401. |
| 12 | WebSocket frames | Oversized control frame (ping payload >125 bytes, RFC violation) | Closed, 4401. |
| 13 | WebSocket frames | Invalid UTF-8 in a text frame | Closed, 4401. |
| 14 | WebSocket frames | Ping flood post-auth, 2,000 pings | All 2,000 answered with `pong` in 1.09s; no effect. |
| 15 | WebSocket frames | Compression (`Sec-WebSocket-Extensions: permessage-deflate`) requested at upgrade | **Not offered** — the response never echoes any extension; acd negotiates no compression at all (no code path for it either; confirmed by reading). Not applicable. |
| 16 | TLS | Plaintext (non-TLS) connection to the TLS listener | Server responds with a TLS alert record, not a plaintext WS upgrade — TLS-only enforced. |
| 17 | `hello` fuzzing | Control/bidi/NUL characters in every string field (`id`, `token`, `client.id/name/version`) | Rejected (`unauthorized`, since the fuzzed token is wrong); no crash. |
| 18 | `hello` fuzzing | 500,000-byte `client.id` | `bad_request: "client.id longer than 128 bytes"`. |
| 19 | `hello` fuzzing | Wrong protocol version (`acd/99`) | `unsupported_protocol`, closed 4400, `supported: ["acd/1"]`. |
| 20 | `hello` fuzzing | Malformed JSON | Closed, 4401. |
| 21 | `hello` fuzzing | Wrong-length token vs. right-length-wrong-content token | Both `unauthorized`. See Finding/timing below — no distinguishable timing. |
| 22 | `hello` fuzzing | `resume.seq = 2^62` (huge), current-looking but wrong epoch | Treated as a different epoch; full `welcome`+`snapshot` sent, no crash. |
| 23 | `hello` fuzzing | `resume.seq = -1` (negative; field is an unsigned int on the wire) | Closed, 4401 (fails JSON deserialization as a bad `hello`). |
| 24 | Token brute force | Timing: 400 interleaved samples each of wrong-length vs. right-length-wrong-content tokens | Rank-sum `z = -0.54` — no detectable difference; medians ~2.06ms / ~2.09ms apart from each other, consistent with `ct_eq`. |
| 25 | Token brute force | Sustained single-source wrong-token `hello` rate | ~464 attempts/s (not timeout-bound, since each wrong `hello` completes — and the connection is released — almost immediately rather than waiting out `hello_timeout`). Immaterial against a 256-bit token. |
| 26 | Idempotency/TOCTOU | Two clients race to answer the same real request | Exactly one wins (`delivered`); the loser gets `request_busy`, never a second delivery. |
| 27 | Idempotency/TOCTOU | Replay the same command id + same payload while/after the first run | Same cached result both times; no second execution. |
| 28 | Idempotency/TOCTOU | Replay the same command id with a *different* payload after the first finished | `duplicate_mismatch`, as documented. |
| 29 | Idempotency/TOCTOU | Answer with `revision = 2^62` against a real request | `request_changed`; not delivered (confirmed by then answering correctly and seeing the hook resolve only on the second, correct answer). |
| 30 | Idempotency/TOCTOU | Answer an already-resolved request again | Refused (`request_busy`, not `request_gone`, for at least 1s after resolution — see note below); never delivered twice. |
| 31 | Idempotency/TOCTOU | A second authenticated client (different `client.id`) answers a request it didn't open | Succeeds (`delivered`) — **by design**: PROTOCOL.md's trust model is "any token holder may act on any request" (§8); not a bypass. |
| 32 | **Idempotency DoS** | One authenticated client floods `request.answer` for 16,500 distinct, nonexistent `request_id`s (one connection, one `client.id`) | **Fills the shared, daemon-wide 16,384-entry idempotency table** (`RequestGone` is `cacheable`, unlike `RequestNotReady`/`RequestBusy`/`SourceUnavailable`/`RequestChanged`). A brand-new, unrelated `client.id`'s very first command then gets `bad_request: "too many recent commands; retry later"`. See **Finding 2**. |
| 33 | Idempotency DoS | Same flood using `ping` instead of `request.answer` | **No effect** — `ping` is handled directly in `server.rs`, bypassing the idempotency engine entirely; confirmed by reading and by this failed attempt before finding the working vector above. |
| 34 | Hook endpoint (local, no token) | POST with no token / wrong token | `401 Unauthorized`, immediately, before the body is parsed (confirmed via a Content-Length lie and malformed JSON both also getting instant 401 — auth gates everything else). |
| 35 | Hook endpoint | Oversized body (2 MiB; documented cap 1 MiB) | Connection closed before the body is consumed (`401`/broken pipe); no crash. |
| 36 | Hook endpoint | Malformed JSON with a correct Content-Length | `401` (auth-gated first; never got to parse it). |
| 37 | Hook endpoint | `Content-Length` lies (claims far more than is ever sent) | `401` immediately — does not hang waiting for the promised bytes. |
| 38 | Hook endpoint | Slow body: 1 byte every 0.5s | Connection dropped (broken pipe) after ~1s — closed fast via the auth-first check, not the 5s body timeout (never reached it). |
| 39 | Hook endpoint | Path tricks (`../../etc/passwd`, `%2e%2e`, double slash, trailing slash, embedded NUL, case) | All `401` (auth-gated before routing matters for an unauthenticated caller; no path reached a different outcome). |
| 40 | Hook endpoint (local-F3 re-check) | 64 idle connections (no bytes at all), then a legitimate POST | Legitimate POST answered in 0.01s — **unaffected**. All 64 idle connections closed server-side within 32s (confirms the `header_read_timeout` fix). |
| 41 | Unix sockets | Unprivileged user (`redattacker`, no acd config access) connects to `acd.sock` / `hook.sock` | `Permission denied` — the 0700 config directory blocks it before the socket's own checks matter. |
| 42 | File access | Unprivileged user reads `client.token` / lists the config directory | `Permission denied` on both. |
| 43 | File race | Symlink `client.token` → `/etc/passwd`, then `acd token rotate client --force` | Refused: `"client.token is a symlink; replace it with a regular file"`. `/etc/passwd` untouched; the daemon's own token file also untouched (still the dangling symlink — nothing was written through it). |
| 44 | File race | Symlink `hook.sock` → `/etc/shadow`, then start a fresh daemon | Refused: `"hook.sock exists and is not a socket; remove it"`; daemon exits before binding anything else (fails the *whole* daemon closed, not just that listener). `/etc/shadow` untouched. |
| 45 | Signals/shutdown | `SIGTERM` to a running daemon | Clean exit; both Unix sockets removed immediately; process gone within ~1-2s. |
| 46 | Signals/shutdown | `SIGKILL` (no chance to clean up), then restart | Stale sockets left on disk as expected; restart detects and replaces them correctly (new epoch, no crash loop, no manual cleanup needed). |
| 47 | Omarchy socket | Oversized line (5 MiB; `MAX_LINE` = 4 MiB) | Connection closed with no reply; a fresh client right after is served normally. |
| 48 | Omarchy socket | Malformed JSON, non-object JSON, missing `type`/`id`, embedded NUL/bidi in `id`, binary garbage | All closed (`Malformed`) with no reply, no crash. |
| 49 | Omarchy socket | Empty `id` / 65-byte `id` (cap is 64) | Both closed. |
| 50 | Omarchy socket | Duplicate command id on one connection | Second attempt: `error`/`duplicate_id`; never re-run. |
| 51 | Omarchy socket | Unknown command `type` | `error`/`unsupported` (not a crash or silent drop). |
| 52 | Omarchy socket | Control/bidi/NUL characters (×50 repeat) in every string field of a `pick` command | Clean `error` reply; server stays up for the next client. |
| 53 | Omarchy socket | `revision: u64::MAX`, and a revision literal too large to fit any integer type | Clean `error` reply in both cases; no panic. |
| 54 | Omarchy socket | 200 concurrent connections from the same uid, each sent a command | All 200 served correctly; no cap, no starvation (matches the static review: peers are same-uid only by design). |
| 55 | Hour-long soak | Mixed: pre-auth churn from many `127.0.0.x` sources + an authenticated flooding client (rate-limited to avoid re-triggering Finding 2 and swamping every other measurement) + hook-endpoint idle/malformed abuse, ~61 min, RSS/fd/thread sampled every 10s | See **Resource limits** below. |

## Finding 1 (Medium): the shared pre-auth pool is still exhaustible, now needing ≥4 distinct sources

**Where.** `crates/acd-core/src/server.rs`: `AppState.preauth` is a single
`Arc<Semaphore>` of 8 permits, shared by every TCP-origin connection with no
per-source accounting of its own. The per-source cap that fixed core#1 lives
one layer down, in `tls::Admission` (`crates/acd-core/src/tls.rs`,
`MAX_UNAUTHENTICATED_PER_SOURCE = 2`), and governs the *admission* pool (32
total) — it does bound how many connections from one source can even reach
the WS-upgrade stage, but it does not give the smaller `preauth` semaphore
any memory of where a connection came from once it's there.

**Attacker.** The core#1 attacker model (a): another device on the tailnet.
The fix raises the bar from "any one such device" to "four such devices" —
a materially higher bar for a lone compromised machine, but well within
reach of an attacker who already controls a few of the user's paired
devices, or of several distinct tailnet peers.

**Steps (reproducer: `tests/redteam/preauth_distributed.py`).**
```
python3 -I preauth_distributed.py --dir <cfg> --port <ws-port> \
    --n-sources 4 --per-source 2 --finish-tls
```
Each of 4 distinct `127.0.0.x` addresses opens 2 TLS connections, finishes
the WebSocket upgrade, and sends no `hello`, refreshing them as the server's
5s `hello_timeout` closes each one.

**Result, isolated (dedicated throwaway daemon, no ambient load).**
| sources × per-source | held | legit client during attack |
|---|---|---|
| 2 × 2 | 4 | connects normally |
| 3 × 2 | 6 | connects normally |
| 4 × 2 | **8** | **`HTTP/1.1 503 Service Unavailable`** |

The local Unix socket (`local_preauth`, a separate pool — the actual fix
for core#1's "it even locks out the local socket" half) stayed reachable
throughout every run at every source count. Releasing the attacker's
sockets restores legitimate access immediately; no lasting state.

**Impact.** A remote attacker who can present ≥4 distinct source addresses
can still deny new TLS clients to the daemon (existing sessions are
unaffected). This is a real reduction in severity from core#1 (needs 4
sources instead of 1, and the local socket is immune), but the underlying
resource — 8 slots, no per-source memory at that layer — is still small
enough that a modestly-resourced attacker clears it easily.

**Minimal fix.** Either move the per-source accounting from
`tls::Admission` down into `AppState.preauth` itself (so a source's slots
there are counted too, not just at admission), or make `preauth`'s capacity
large enough that the Unix-socket-isolation fix's intent — "one peer cannot
hold more than a few of them" (the existing code comment) — actually holds
against *several* peers, e.g. raising it and adding the same `per_source`
pattern already proven in `tls::Admission`.

## Finding 2 (Medium): one authenticated peer can fill the idempotency table and lock out every other client's new commands

**Where.** `crates/acd-core/src/idem.rs`: `IdemCache` is one
`HashMap<(client_id, id), Entry>`, daemon-wide, capacity `IDEM_CAPACITY` =
16,384, shared by every connected client. `crates/acd-core/src/engine.rs`'s
`cacheable` computation (in `Core::handle`) excludes only
`RequestNotReady`/`RequestBusy`/`SourceUnavailable`/`RequestChanged` from
being cached as `Done`; `RequestGone` (returned for a `request.answer`
whose `request_id` doesn't exist — or no longer exists) is *not* excluded,
so it is cached for the full 15-minute `IDEM_TTL`. `ping` cannot be used for
this: `server.rs:468` answers it directly, bypassing the idempotency engine
entirely.

**Attacker.** A token holder (any paired client) — the same minimum bar as
the static review's rejected "idempotency abuse" item, but with a
materially different blast radius (see Impact).

**Steps (reproducer: `tests/redteam/idem_table_flood.py`; unit-level
reproducer: `crates/acd-core/tests/redteam_idem_exhaustion.rs`, run with
`cargo test -p acd-core --test redteam_idem_exhaustion -- --ignored`).**
1. One connection, one `client.id`, sends `request.answer` for 16,500
   distinct, never-existed `request_id`s, each with a distinct command id.
2. Each gets `request_gone` and is cached — confirmed (`16,383 request_gone`
   then `117 bad_request` once the table filled, in the live run against a
   disposable daemon).
3. A **brand-new**, unrelated `client.id`'s **first-ever** command (nothing
   to do with the flood: a fresh connection, a fresh id) gets
   `bad_request: "too many recent commands; retry later"` instead of the
   `request_gone` it would otherwise get.

**Reproduced: yes, live and at the unit level.** Live: 16,500 floods from
one connection filled the table in ~6.4s (including reading every reply);
a subsequent, entirely fresh client's first command was refused. Unit test:
fails in ~4s with `cargo test -p acd-core --test redteam_idem_exhaustion
-- --ignored`, asserting the exact same before/flood/after sequence
directly against `Core::handle`, no network involved.

**Impact.** The static review's "Considered and rejected / Idempotency
abuse" reasoned that a token holder who fills the table "already has full
control", so filling it is not a new capability for *that* attacker. That
reasoning covers self-harm; it does not cover the actual effect observed
here: filling the table also silently breaks `request.answer`/every other
command for **every other paired device and agent** for up to 15 minutes —
including the legitimate user's own other machines, and including
well-behaved agents whose requests the user is trying to answer. One
compromised or buggy paired client (a "full control" attacker over its own
machine, but not over the user's other machines) can deny the
request-answering function of the *entire* daemon to everyone, repeatably,
indefinitely, from outside the blast radius the earlier review described.

**Minimal fix.** Either key capacity per `client_id` (so one client cannot
exhaust more than its fair share of the shared table — the existing
"reservations and results are kept for 15 minutes" design can stay,
per-client), or stop caching `RequestGone` (and audit the other error codes
reachable this cheaply) the way `RequestNotReady`/`RequestBusy`/
`SourceUnavailable`/`RequestChanged` already are, since a request that was
never real in the first place need not be remembered for 15 minutes at all.

## Other observations (not separate findings)

- **`request_busy` lingers after a successful `delivered` answer.** Case 30
  above: re-answering a request immediately (or up to 1s later, the longest
  delay tried) after it was successfully answered gets `request_busy`
  ("another answer is in flight"), not `request_gone`. Reading
  `Registry::resolve_request_with` (which actually removes the request and
  would then yield `request_gone`) shows this is because `outcome:
  "delivered"` is not the same as *resolved* — PROTOCOL.md says so
  explicitly ("Confirmation arrives later as `request.resolved`"), and for
  a hook-sourced `PermissionRequest` that confirmation depends on a
  follow-up hook event (e.g. `PostToolUse`) this session's synthetic
  requests never sent. This is expected, documented behavior, not a
  bypass — no double delivery ever occurred in any trial — but it is worth
  recording because a client that assumes "delivered implies the id is
  immediately free again" would be surprised by it.
- **Token comparison timing**: no detectable difference between
  wrong-length and right-length-wrong-content tokens across 400 interleaved
  samples. Confirms the static review's code-reading conclusion
  dynamically.
- **No WebSocket compression is offered**, at all — confirmed both by
  reading (no `permessage-deflate` code anywhere in `acd-core`/`acd-client`)
  and by requesting it at the upgrade and finding it never echoed back.
  Compression-oracle and zip-bomb-style attacks don't apply.
- **Fragment count is not a resource**: 50,000 one-byte WebSocket fragments
  forming one valid message under the size cap cost 0.23s and no memory
  growth; only total bytes (already capped at 1 MiB) and frame count within
  one message matter, and reassembly is efficient.

## Resource limits: the hour-long soak

Ran `tests/redteam/soak_attack.py` (pre-auth churn from many sources, an
authenticated flooding client sending bulk `ping`/bogus `request.answer`
traffic *without* re-triggering Finding 2 — it deliberately reuses one
command id per connection per 2s cycle, documented in the script, so this
measurement isn't itself contaminated by that separate finding — and
hook-endpoint idle/malformed-POST abuse) against a dedicated daemon for
3700s (~61.7 min), sampling `/proc/<pid>/status` (`VmRSS`, `Threads`) and
`/proc/<pid>/fd` every 10s with `tests/redteam/soak_sample.py`.

**Result (full 3,700s / 61.7 min run, 374 samples; the CSV itself is a
disposable run artifact, not checked in — reproduce it with the two
scripts above).**

| | start (0s) | 1–10 min avg | last ~10 min avg (≥3100s) | end (3730s, 30s after the attack stopped) | min | max |
|---|---|---|---|---|---|---|
| RSS (KB) | 12,228 | 17,264 | 17,636 | 17,764 | 12,228 | 17,996 |
| fds | 14 | — | — | **14** | 14 | 101 (transient, during a separate manual pre-auth-pool test against this same daemon, recovered within the next sample) |
| threads | 5 | — | — | 5–6 | 5 | 6 |

- RSS grows from a cold 12.2 MB to ~17.3 MB in the first 1–10 minutes (initial
  connections, TLS session state, and ~11 agents this session's own earlier
  manual hook-endpoint tests against this daemon left open with
  `permission_hold = 3600`), then stays flat: the last-10-minutes average is
  **2.1% above** the first-10-minutes average after 50 more minutes of
  continuous hostile load (pre-auth churn from many sources, an
  authenticated flooding client, and hook-endpoint idle/malformed-POST
  abuse) — consistent with allocator fragmentation noise, not a leak.
- File descriptors return to exactly the starting count (14) once the
  attack stops; the one spike to 101 happened during a separate,
  deliberate, short-lived pre-auth-pool test run against this same daemon
  mid-soak (`preauth_distributed.py` with `--n-sources 20`) and recovered
  on the very next 10s sample — not a leak either.
- Threads stayed at 5–6 for the entire run (tokio's worker pool; no task
  growth).
- **Zero panics** across the daemon's full log (4,575 lines, `grep -ci
  panic` → 0).
- After the attack stopped, a fresh, legitimate client immediately got a
  normal `welcome`/`snapshot` — the daemon needed no restart or recovery
  time.

## Verification

- `cargo clippy --workspace --all-targets --locked -- -D warnings`: clean.
- `cargo test --workspace --locked --no-fail-fast`, run twice — once on a
  clean `origin/main` checkout (`3404287`, via a disposable `git worktree`)
  and once on this branch, back to back on this same machine so both see
  the same sandbox limits — produced **exactly the same 33 failing test
  names** both times (only the timing numbers inside summary lines
  differ). Every one of them fails for the same reason: `start_daemon`'s
  default (`crates/acd/tests/common/mod.rs`), `listen = ["loopback"]`,
  resolves to both `127.0.0.1` and `::1`
  (`crates/acd/src/bind.rs`), and this sandbox has no IPv6 (`cannot bind
  [::1]:...: Address family not supported by protocol`) — a pre-existing
  environment limitation, not something this session's changes affect.
  This session's own two new test files account for none of the 33: the
  8 `acd-inbox` tests all pass, and `acd-core`'s single new test is
  `#[ignore]`d (it fails on purpose with `--ignored`, documenting Finding
  2, and does not run in a normal `cargo test`).
- This session also ran as root (`id -u` = 0), so the mode-bit/`ps`-sandbox
  tests named in `CLAUDE.md` (`repair_mode_clears_special_bits_and_reports_them`,
  `init_repairs_a_setuid_secret_and_a_loose_t3_token`,
  `the_system_check_recognises_this_process`) are not meaningful here
  either way; they are not in the 33-name failure set above (they pass as
  root, per `CLAUDE.md`'s own note that permission-error tests can
  misbehave under root rather than fail).

## Not tried / not run

- **Client-side findings (core#2, #3, #5, #6, #7)**: this session attacked
  the daemon directly; none of these are reachable without a real client
  (the Mac app, `acd-inbox`, or `acd-client` driven as a library) connected
  *to* this daemon as the thing under attack. The static review already
  reproduced and the team already fixed each of these; not retested.
- **local-F1** (Omarchy panel row-cap): needs Quickshell/Qt rendering,
  unavailable in this sandbox (same limitation the static review hit).
- **local-F2** (T3 token to a loopback squatter) and **local-F4** (herdr
  socket discovery): both need a real T3 server / a reconfigured,
  shared-writable `herdr.extra_sockets`, neither set up in this session's
  daemons. The static review's own reproductions stand; not independently
  re-verified live here.
- **local-F5** (service-file path injection): out of this session's threat
  model (attacking a *running* daemon; `acd service install` is a
  self-injection in a tool invocation, not a wire-reachable attack).
- **TLS parser differential, pin bypass, session-resumption skip**: the
  static review's code-reading already covers these and this session found
  nothing to add; genuinely exercising a parser differential would need a
  crafted certificate this session did not have reason to build.
- **macOS-specific behavior, Keychain, the Tauri app, iOS shell**: this
  sandbox is Linux-only.
- **A real T3 Code or herdr process**: neither is available here; the hook
  adapter (via recorded fixture payloads) was the only producer exercised
  live.
- **fuzz/ and mutation tests**: explicitly owned by another concurrent
  session per the task's scope note; not touched.
