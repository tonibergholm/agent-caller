# Security review: protocol, core, client (findings only)

Date: 2026-11-03. Base: `main` at `108034c`. Read-only review: no source file
changed.

## Summary

| # | Severity | Finding | Attacker | Reproduced |
|---|---|---|---|---|
| 1 | Medium | 8 idle pre-auth sockets (no token, no pin) lock every new client out of TCP **and the local socket** | a | yes |
| 2 | Medium | After a daemon restart the client raises no notification for request ids it has seen in an earlier epoch (`r1`, `r2`, …) | none needed (a hostile peer can also use it) | yes |
| 3 | Medium | The client accepts frames up to 16 MiB (64 MiB per message) and keeps unbounded agent state; one hostile peer can exhaust the app for all machines | b | yes |
| 4 | Low–Medium | acd logs no connection or auth event, so use of a leaked token is invisible | d, a | yes (grep) |
| 5 | Low–Medium | A hostile peer can fire unlimited OS notifications and grow an unbounded dedupe set | b | yes |
| 6 | Low–Medium | Close code 4408 makes the client reconnect with no delay (about 150 reconnects/s) | b, or an overloaded daemon | yes |
| 7 | Low | Pairing host accepts control characters, bidi overrides, NUL and 100 KB | d | yes |
| 8 | Low | `agent.send` text uses a weaker character rule than typed answers | token holder | yes |

No unauthenticated access, token or pin bypass, TLS downgrade, double
delivery, stale-revision delivery, wire-reachable panic or secret or T3 text
in a log line was found. See "Considered and rejected".

## Scope and method

In scope: `acd-protocol`, `acd-core`, `acd-client`, `acd-app-core`, plus
`acd-securefs` and the listener wiring in `crates/acd/src/daemon.rs` where a
finding depends on them.

Attackers: (a) another device on the tailnet, (b) a malicious or compromised
paired acd peer, (c) a local unprivileged user, (d) someone who sees a
pairing line.

I read the code against `PROTOCOL.md`, the design spec and the follow-ups
list, and reproduced candidates with throwaway tests in a scratch copy of the
repo (never committed). Items already recorded in
`docs/superpowers/plans/2026-10-07-p1-followups.md` are not repeated.

---

## 1. Pre-auth slot exhaustion locks out TLS and local clients (Medium)

**Where.**
- `crates/acd-core/src/server.rs:95` creates one `preauth` semaphore (8
  permits) per `router()` call. `server.rs:121` takes a permit at the
  upgrade. It is released after a valid `hello` (`server.rs:226`) or after the
  5 s `hello_timeout` (`server.rs:176`).
- `crates/acd/src/daemon.rs:668` and `daemon.rs:712` serve **clones of the
  same router** on every TCP listener and on `acd.sock`. The comment above
  `daemon.rs:703` says the sharing is deliberate ("the same router (and so the
  same limits)"), so that the limits are daemon-wide. All of them therefore
  share the same 8 permits.
- `crates/acd-core/src/tls.rs:403-404` and `tls.rs:208-217`: 32 admission
  permits (daemon-wide) and 16 concurrent handshakes, with no per-source
  limit. Admission is taken at TCP accept, before any byte is read.

**Attacker.** Any tailnet device (a). No token, no pin: the server never
checks the pin, only clients do.

**Steps.**
1. Open 8 TLS connections, finish the WebSocket upgrade, send nothing.
2. Reconnect each one every 5 s (about 1.6 connections/s in total).

**Impact.** Every legitimate new connection gets HTTP 503. That includes the
owner's local clients on `acd.sock` (`acd status`, the Mac app's local
machine), because the pool is shared. Sharing was meant to bound total load;
it was not meant to let a remote peer starve the local socket, and nothing in
PROTOCOL.md §1 or the follow-ups says it can. Existing sessions keep working,
but any reconnect (network blip, token rotation, daemon restart) cannot get
back in while the attack runs. A cheaper variant needs no TLS: 16 idle TCP
sockets fill the handshake slots for 5 s each, and a legitimate TLS handshake
gets EOF.

**Reproduced: yes.** Scratch test `zz_preauth.rs`, router served on a TLS
listener and, as a stand-in for `acd.sock`, on a plain listener:
- 8 upgraded sockets without `hello` → legit TLS client: `HTTP error: 503`;
  legit client on the plain (local) listener: `HTTP error: 503`. 6 s later the
  plain client connects, so the attacker must keep refilling.
- 16 idle TCP sockets → legit TLS handshake fails with `tls handshake eof`.

**Minimal fix.**
- Give the Unix socket its own `preauth` and `inflight_total` semaphores.
- Cap pre-auth connections per source IP (2 is enough) and, when the pool is
  full, evict the oldest unauthenticated connection instead of refusing the
  new one.
- Test for the fix: the repro above, inverted (a legit client connects while 8
  sockets are held from one IP, and while 8 are held on TCP, a local-socket
  client still connects).

## 2. No notification for request ids seen in an earlier epoch (Medium)

**Where.**
- `crates/acd-core/src/registry.rs:604-605`: request ids are `r1, r2, …` and
  the counter restarts at 0 with each daemon.
- `crates/acd-client/src/inbox.rs:932`: the client notifies only if
  `inbox.notified.insert((machine_id, req.id))` is new. `notified`
  (`inbox.rs:138`) is never cleared or pruned, and the key has no epoch. The
  machine id is stable across restarts (PROTOCOL.md §6, `Machine.id`).

**Attacker.** None needed. After any acd restart (reboot, upgrade, exit 75 on
a Tailscale address change) the next *K* requests raise no notification, where
*K* is the number of requests the previous daemon issued while the app was
running. A hostile or buggy peer that fakes this machine's id cannot reach
another machine's set, so this is a correctness failure with a security
consequence, not a remote exploit.

**Steps.**
1. App is connected to machine M; M's daemon issues `r1`; the user is notified.
2. M restarts (new epoch, same machine id).
3. The new daemon opens a different request, again `r1`.

**Impact.** The permission request is on screen in the panel but raises no
notification or alert, so an agent can wait on a dialog for as long as the
user is not looking at the panel. It fails silent. This is the "request-id
confusion" the review targeted; the answer path itself is safe (see rejected).

**Reproduced: yes.** Scratch test `zz_notify_restart.rs`: a peer with the same
machine id sends `request.opened` `r1` in epoch `e1`, closes, then sends a
different `r1` in epoch `e2`. The app emits 1 notification for the two
requests.

**Minimal fix.** Key the set by `(machine_id, epoch, request id)` or clear it
on `EpochChange::New`, and bound it (LRU of a few thousand). Test: the repro
must yield 2 notifications.

## 3. A hostile peer can exhaust the client's memory (Medium)

**Where.**
- `crates/acd-client/src/conn.rs:163` and `conn.rs:219` call
  `tokio_tungstenite::client_async` with no `WebSocketConfig`. The defaults
  are 16 MiB per frame and 64 MiB per message. PROTOCOL.md §1 sets a 1 MiB
  limit, but only the server enforces it (`server.rs:125-126`).
- `crates/acd-client/src/session.rs:115-118, 148-150`: `agents` is a
  `BTreeMap` with no cap on count or field sizes.
- `crates/acd-client/src/conn.rs:422`: every applied event clones the whole
  `Session` into the watch channel, so cost per event grows with total state.

**Attacker.** (b) a paired peer: a compromised machine, or one whose operator
is not the user. The pin proves the peer is the machine that was paired, not
that it behaves.

**Steps.** After `welcome` and an empty snapshot, send `agent.updated` frames
with distinct agent ids and a 15 MiB `title` each, as often as wanted.

**Impact.** The app holds all of it and clones all of it on each event. One
bad machine takes down the whole app, including approvals for the good
machines, and the iOS app much sooner. Fails closed (nothing is answered
wrongly) but it is a denial of service from the weakest paired machine.

**Reproduced: yes.** Scratch test `zz_hostile_peer.rs`: a pinned TLS peer
sends 8 `agent.updated` frames with 15 MiB titles. The client reaches `Online`
holding 8 agents and 120 MiB of titles; peak process RSS 426 MB (client and
peer share the test process). A single 40 MiB frame is rejected by the 16 MiB
frame default, which confirms the limit that applies.

**Minimal fix.** Set `max_message_size` and `max_frame_size` to 1 MiB on the
client config. Cap agents per machine (for example 2,000) and field lengths
when applying a snapshot or event; on violation, drop the connection and
resync. Test: the repro with a 2 MiB frame must end in a resync, not in state.

## 4. acd records nothing about who connects (Low–Medium)

**Where.** `crates/acd-core/src/server.rs` (no `tracing` call in `session`,
`Auth::check` or the rotation path), `engine.rs` (only lines 60, 359, 405,
431 log). `grep -rn "TraceLayer\|ConnectInfo\|peer_addr\|client_id" crates/acd/src`
finds nothing, so the daemon does not log it either. `tls::Admit`
(`tls.rs:219-250`) carries no peer address, so the server never sees one.

**Attacker.** (d) sees a pairing line (terminal scrollback, a clipboard
manager or Universal Clipboard history, a chat paste), then (a) connects with
the token from another tailnet device.

**Steps.** Connect with the token (no pin needed), then `request.answer` and
`agent.send`. Nothing is logged for the connection, for a rejected `hello`, or
for a new `client.id`.

**Impact.** The token is the only credential and has no scope: answer
(approve any permission) plus `agent.send` (type into any agent) is command
execution on the developer's machines. That model is by design (see
"Considered and rejected"). The gap is detection: the owner gets no signal
that a line leaked or was used, and `client.id` is self-asserted, so it cannot
be an audit trail. The follow-ups cover clipboard clearing and optional
`tailscale whois` hardening, not detection.

**Reproduced: yes.** Token-only access: the existing harness
`authenticate_ok` (`crates/acd-core/tests/tls.rs:343`) gets `welcome` with the
token and an accept-any certificate verifier. No logs: by the grep above.

**Minimal fix.** Plumb the peer address into `Admit`. Log at `info` (never the
token or any text): peer address, `client.id`, client name and version for
each accepted `hello`, and a rate-limited `warn` for each rejected one. Test:
capture logs on a good and a bad `hello`; assert the address and `client.id`
appear and the token never does.

## 5. A hostile peer can flood notifications and grow the dedupe set (Low–Medium)

**Where.** `crates/acd-client/src/inbox.rs:914-940`. Every `request.opened`
event raises an `InboxEvent::RequestOpened`, whether or not the agent or
request exists in the snapshot (`agent_title` falls back to `req.agent_id`,
lines 928-930). The set at `inbox.rs:138, 932` is never pruned (see #2). The
Mac app turns each event into an OS notification
(`app/src-tauri/src/notify.rs:75-88`, one `spawn_blocking` each).

**Attacker.** (b).

**Steps.** After `welcome` and a snapshot, send `request.opened` events with
new ids and an agent id that is not in the snapshot.

**Impact.** Notification spam with peer-chosen title text (a convincing
"approve now" lure), memory growth, and blocking-pool pressure (the macOS
backend can block up to 2 s per notification). The follow-ups record the
missing history only for repeat notifications on reconnect, not this flood.

**Reproduced: yes.** Scratch test `zz_notify_flood.rs`: a paired loopback
peer sends 3,000 `request.opened` frames for the non-existent agent `ghost`;
1,466 events reached the subscriber within 15 s (the rest were lost to
broadcast lag, not filtered).

**Minimal fix.** Notify only for a request that is `pending` and `open` on an
agent in the applied session, rate-limit notifications per machine (for
example 5 per 10 s), and bound `notified`.

## 6. Close code 4408 makes the client reconnect with no delay (Low–Medium)

**Where.** `crates/acd-client/src/conn.rs:612-621`: `End::TooSlow` waits
`Duration::ZERO` instead of `backoff.next_delay()` as every other end does.
`end_for_close` (`conn.rs:303-310`) maps 4408 to it.

**Attacker.** (b), or an overloaded legitimate daemon: the daemon closes slow
clients with 4408 (`server.rs:164-167, 315-317`), and each reconnect asks for
a fresh snapshot, which is what was too slow.

**Steps.** Accept the connection, send `welcome`, close with code 4408.
Repeat on every connection.

**Impact.** A tight loop of TLS handshake, `hello` carrying the token, and
resend of in-flight commands. Battery and CPU burn on the Mac or phone, and a
self-sustaining reconnect storm against a loaded daemon.

**Reproduced: yes.** Scratch test `zz_reconnect_spin.rs` with production
backoff settings (base 1 s, max 30 s): 442 `hello`+`welcome` cycles in 3 s.

**Minimal fix.** Use `backoff.next_delay()` for `TooSlow` and reset it only
when the session becomes ready. Test: the repro must show at most 3 cycles in
3 s.

## 7. Pairing host is not validated beyond a few characters (Low)

**Where.** `crates/acd-protocol/src/pairing.rs:91-96` rejects only whitespace
and `/ @ ? # [ ]`. `crates/acd-client/src/inbox.rs:109-118` derives the
default machine name from the host without sanitising.

**Attacker.** (d) hands the user a crafted line.

**Steps.** The user imports a line such as `acd://\u{202e}moc.live:47800?pin=…#…`.

**Impact.** Spoofed or garbled machine name in cards and notifications, and a
huge string in `settings.json`. The connect path still refuses any address that
does not resolve to a Tailscale or loopback address (`endpoint::vet`), so
there is no network exposure. Limited because the user must import the line.

**Reproduced: yes.** Scratch test `zz_pairing_edges.rs`: these hosts parse and
round-trip: `\u{202e}moc.live`, `ho\0st`, `host\x7f`, a 100,000-byte host,
`127.0.0.1\u{200b}`, full-width letters, `a:b`.

**Minimal fix.** Reject control, bidi and separator characters (reuse
`acd_protocol::text`), require ASCII or IDNA hostnames, and cap the host at 253
bytes.

## 8. `agent.send` uses a weaker character rule than typed answers (Low)

**Where.** `crates/acd-core/src/registry.rs:709-713, 784`: `agent.send`
refuses control characters only. Typed answers also refuse bidi overrides and
line/paragraph separators (`registry.rs:752-756`, `acd_protocol::text`).
`text.rs:1-3` states that every side should refuse the same text.

**Attacker.** A token holder, or a buggy client.

**Steps.** Send `agent.send` with `text: "ls\u{202e}gnihtemos"`.

**Impact.** Bidi-override text reaches an agent prompt and can hide the real
content of a command in the agent's transcript or in review UIs. Needs the
token, so this is hardening, not an access bypass.

**Reproduced: yes.** Scratch test `zz_send_bidi.rs`: `core.handle` returns
`ok=true` and the adapter receives the text; `has_forbidden_chars` on the same
text returns `true`.

**Minimal fix.** Call `has_forbidden_chars` in `check_send`. Test: the repro
must return `bad_request`.

---

## Considered and rejected

- **A leaked pairing line gives full control.** The line carries the only
  credential, has no expiry, is not single-use, and rotation revokes every
  client. That is the documented trust model (PROTOCOL.md §1, Tokens), so it
  is not a defect on its own; #4 covers the detection gap. A per-device token
  from a short-lived pairing code would be the real fix.
- **Timing of token comparison.** `Auth::check` (`server.rs:47-54`) uses
  `ct_eq`; it leaks only the length, and valid tokens are fixed at 43 bytes.
  Pin comparison (`pairing.rs:13-18`) is constant time and canonical.
- **Token brute force.** 256-bit token; no lockout needed.
- **TLS downgrade or pin bypass.** Server and client are TLS 1.3 only
  (`tls.rs:181`, `client/tls.rs:62`). The verifier checks the SPKI pin and
  verifies the handshake signature with the real algorithms. A new
  `ClientConfig` is built per connection (`conn.rs:198`), so session
  resumption cannot skip the check. The pin is verified before `hello`, so
  the token is never sent to an unpinned server.
- **Parser differential** between `x509-parser` (pin) and webpki (signature
  key). No concrete certificate found; theoretical.
- **Idempotency abuse.** Keys are `(client.id, id)` plus a payload hash
  (`idem.rs:49-67`); a different payload gets `duplicate_mismatch`; a retry
  while running waits; `NotDelivered` and `request_changed` release the id.
  A token holder can fill the 16,384-entry table or reuse another client's
  `client.id`, but a token holder already has full control.
- **Replay after the 15-minute TTL.** The client stops resending after 14
  minutes (`stale_inflight`, `client/lib.rs:49`) and request ids are unique
  within an epoch, so a late replay gets `request_gone`.
- **Request-id reuse on the answer path.** Ids restart at `r1` (see #2), but
  the client revokes in-flight commands on a new epoch
  (`commands.rs:235-238`), cannot send until the snapshot is applied, and
  rechecks request id and revision on the card's own machine
  (`inbox.rs:1006-1018`). Putting the epoch in `request.answer` would be
  defence in depth.
- **Stale revision.** `claim` checks `revision` when present
  (`registry.rs:738`). Omitting it is the documented old-peer behaviour, and
  the client always sends it when `answer.revision` is advertised. A peer that
  hides the capability only weakens its own requests.
- **Machine-id confusion in routing.** Routing uses the local machine key,
  never the server's `machine.id` (`inbox.rs:991-1018`). `view.rs:366-383`
  hides a machine whose id duplicates an earlier one. A hostile peer listed
  first could hide a later machine, but only if it knows that machine's id,
  which it does not see.
- **Wire-reachable panics.** None found in `acd-core`, `acd-protocol` or
  `acd-client` source: no byte-index slicing of wire text, the `expect`s are
  on adapter-side invariants, serde_json caps nesting, and `AbandonGuard`
  tolerates mutex poison.
- **Server frame and queue limits.** 1 MiB frames, 128-byte ids, 64 KiB text,
  32 and 256 in-flight commands and the 1,000-event queue are all enforced
  (`server.rs:74-77, 125-126, 315`; `engine.rs:463-483`).
- **Token rotation.** Sessions and detached commands are revoked (tests
  `revocation_tests`). The old token stays valid for up to 2 s (documented).
- **Local socket planting (c).** `read_local_token` verifies the config dir
  (0700, owner, no symlink) before the socket is used (`conn.rs:152-160`,
  `local.rs:54-57`). Ancestor directories are not checked; this matters only
  if `$HOME` is group- or world-writable.
- **Loopback port squatting (c).** A rogue listener fails the pin check.
- **Pairing parse.** Trailing data, duplicate `pin=`, extra `#`, port 0,
  userinfo and brackets are rejected, and the v4-mapped IPv6 form is vetted
  (`endpoint.rs:33-36`). Non-canonical literals are already recorded.
- **Clipboard.** Clear-after-import is recorded in the follow-ups.
  `import_and_clear` leaves the line after a failed import by design.
- **`Debug` on secrets.** `Credential` and `MachineConfig`
  (`conn.rs:24-37`) derive `Debug` and would print the token, but no call
  site formats them today (grep of `acd-client`, `acd-app-core`, `acd-inbox`,
  `app/src-tauri`). `MachineSecret` and `Pairing` already redact. Worth
  redacting before someone adds a `{:?}`.
- **Message text or secrets in logs.** `acd-core` logs ids and adapter reason
  strings only. The reasons passed to `engine.rs:359, 405` are `&'static str`
  constants for T3 (`acd-adapters/src/t3/outcome.rs:28-81`) and fixed text or
  an error code for herdr (`herdr/keys.rs:697-715`, `herdr/mod.rs:133`). The
  client logs only an error code and a fixed reason (`conn.rs:416, 601`).

## Not checked

- Adapters (`acd-adapters`) beyond the log-reason strings, the hook endpoint,
  `hook.token`, the external producer API, and `crates/acd` (daemon bind
  rules, `acd pair` output, TLS key file permissions, service install).
- The Tauri shell: how the clipboard is read and cleared, rendering of peer
  text in Svelte (XSS, bidi), the iOS shell, and the voice crates.
- Live behaviour: real Tailscale ACLs, MagicDNS, macOS Keychain prompts and
  access control, Universal Clipboard, iOS memory limits.
- Fuzzing, load tests of the pre-auth limits beyond the repro, and dependency
  advisories (`cargo audit`).
- macOS builds; all reproductions ran on Linux as root.

The throwaway tests (`zz_preauth.rs`, `zz_hostile_peer.rs`,
`zz_notify_restart.rs`, `zz_notify_flood.rs`, `zz_reconnect_spin.rs`,
`zz_pairing_edges.rs`, `zz_send_bidi.rs`) lived in a scratch copy and are not
part of this change.
