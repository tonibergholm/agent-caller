# Security review: protocol, core, client (findings only)

Date: 2026-11-03. Base: `main` at `108034c`. Read-only review: no source file
changed. Scope: `acd-protocol`, `acd-core`, `acd-client`, `acd-app-core`
(plus `acd-securefs` and the listener wiring in `crates/acd/src/daemon.rs`
where a finding depends on them).

Attackers considered: (a) another device on the tailnet, (b) a malicious or
compromised paired acd peer, (c) a local unprivileged user, (d) someone who
sees a pairing line.

Method: read the code against `PROTOCOL.md`, the design spec and the
follow-ups list. Findings were reproduced with throwaway tests in a scratch
copy of the repo (never committed). Items already recorded in
`2026-10-07-p1-followups.md` are not repeated.

## Summary

| # | Severity | Finding | Attacker | Reproduced |
|---|---|---|---|---|
| 1 | Medium | 8 idle pre-auth sockets (no token, no pin) lock every new client out of TCP **and the local socket** | a | yes |
| 2 | Medium | A seen pairing line is permanent full control, and acd logs no connection or auth event, so misuse is invisible | d, a | yes (access), grep (no logs) |
| 3 | Medium | The client accepts frames up to 16 MiB (64 MiB per message) and keeps unbounded agent state; one hostile peer can exhaust the app for all machines | b | yes |
| 4 | Low–Medium | A hostile peer can fire unlimited OS notifications and grow an unbounded dedupe set | b | yes |
| 5 | Low–Medium | Close code 4408 makes the client reconnect with no delay (about 150 reconnects/s) | b, or an overloaded daemon | yes |
| 6 | Low | Pairing host accepts control characters, bidi overrides, NUL and 100 KB | d | yes |
| 7 | Low | `agent.send` text uses a weaker character rule than typed answers | token holder | no |
| 8 | Info | `Credential` and `MachineConfig` derive `Debug` and print the token | latent | trivial |

No unauthenticated access, token or pin bypass, TLS downgrade, double
delivery, stale-revision delivery, wire-reachable panic or secret in a log
line was found. See "Considered and rejected".

---

## 1. Pre-auth slot exhaustion locks out TLS and local clients (Medium)

**Where.**
- `crates/acd-core/src/server.rs:95` creates one `preauth` semaphore (8
  permits) per `router()` call. `server.rs:121` takes a permit at the
  upgrade; it is released only after a valid `hello` (`server.rs:226`) or
  after the 5 s `hello_timeout` (`server.rs:176`).
- `crates/acd/src/daemon.rs:668` and `daemon.rs:712` serve **clones of the
  same router** on every TCP listener and on `acd.sock`, so all of them share
  those 8 permits.
- `crates/acd-core/src/tls.rs:403-404` and `tls.rs:208-217`: 32 admission
  permits (daemon-wide) and 16 concurrent handshakes, with no per-source
  limit. Admission is taken at TCP accept, before any bytes are read.

**Attacker.** Any tailnet device (a). No token, no pin: the server does not
check the pin, only clients do.

**Steps.**
1. Open 8 TLS connections, finish the WebSocket upgrade, send nothing.
2. Reconnect each one every 5 s (about 1.6 connections/s in total).

**Impact.** Every legitimate new connection gets HTTP 503. This includes the
owner's local clients on `acd.sock` (`acd status`, the Mac app's local
machine), because the semaphore is shared. Existing sessions keep working,
but any reconnect (network blip, token rotation, daemon restart) cannot get
back in while the attack runs. A cheaper variant needs no TLS at all: 16 idle
TCP sockets fill the handshake slots for 5 s each, and a legitimate TLS
handshake gets EOF. The follow-ups record only that the pre-auth stages are
bounded; the shared pool and the lockout are not recorded.

**Reproduced: yes.** Scratch test `zz_preauth.rs` (copy of `tests/tls.rs`
helpers), router served on a TLS listener and, as a stand-in for `acd.sock`,
on a plain listener:
- 8 upgraded sockets without `hello` → legit TLS client: `HTTP error: 503`;
  legit client on the plain (local) listener: `HTTP error: 503`; 6 s later
  the plain client connects, so the attacker must keep refilling.
- 16 idle TCP sockets → legit TLS handshake fails with `tls handshake eof`.

**Minimal fix.**
- Give the Unix socket its own router state, or at least its own `preauth`
  and `inflight_total` semaphores, so a remote attacker cannot starve it.
- Cap pre-auth connections per source IP (2 is enough), and when the pool is
  full evict the oldest unauthenticated connection instead of refusing the new
  one.
- Test for the fix: the repro above, inverted (legit client connects while 8
  sockets are held from one IP).

## 2. A seen pairing line is permanent full control and leaves no trace (Medium)

**Where.**
- `crates/acd-protocol/src/pairing.rs:39-45, 72-109`: the line is
  `acd://host:port?pin=…#token`. The token is the only credential; the pin
  only authenticates the server to the client.
- `crates/acd-core/src/server.rs:24-55`: one shared token for all clients,
  checked on `hello`. Nothing ties it to a device, address or time.
- `crates/acd-core/src/server.rs`, `engine.rs`: no `tracing` call for a
  failed `hello`, a successful `hello`, a new `client.id` or a rotation. The
  only `tracing` calls in `acd-core/src` are in `engine.rs:60, 359, 405, 431`.

**Attacker.** (d) sees the line (terminal scrollback, shoulder surfing, a
clipboard manager or Universal Clipboard history, a chat paste), then (a)
connects from any tailnet device.

**Steps.** Connect to `host:port` with the token. No pin is needed. Send
`request.answer` (approve any permission request) and `agent.send` (type text
into any agent that has `can_send`). Together these are command execution on
the developer's machines.

**Impact.** The line never expires and is not single-use. It stays valid
until someone runs `acd token rotate client`, which cuts off every legitimate
client too. Because acd logs nothing about who connects, the owner has no
signal that the line leaked or was used. `client.id` is self-asserted, so it
cannot serve as an audit trail. The follow-ups cover the clipboard-clear
residual and `tailscale whois` as optional hardening; they do not cover the
absence of any detection, or the scope of the token (answer plus `agent.send`).

**Reproduced.** Access: yes. The existing harness `authenticate_ok`
(`crates/acd-core/tests/tls.rs:343`) connects with only the token and an
accept-any certificate verifier, and gets `welcome`. Missing logs: yes, by
grep of `acd-core/src` (list above).

**Minimal fix.**
- Log at `info` (no token, no text): peer address, `client.id`, client name
  and version for each accepted `hello`, and at `warn` for each rejected one
  (rate-limited).
- Longer term: a short-lived single-use pairing code exchanged for a
  per-device token, so one device can be revoked. Optionally bind the session
  to the peer's Tailscale identity (`tailscale whois`, already on the
  follow-ups).
- Test idea: capture logs on a good and a bad `hello`; assert the peer
  address and `client.id` appear and the token never does.

## 3. A hostile peer can exhaust the client's memory (Medium)

**Where.**
- `crates/acd-client/src/conn.rs:163` and `conn.rs:219` call
  `tokio_tungstenite::client_async` with no `WebSocketConfig`. The defaults
  are 16 MiB per frame and 64 MiB per message. PROTOCOL.md §1 sets a 1 MiB
  limit, but only the server enforces it (`server.rs:125-126`).
- `crates/acd-client/src/session.rs:115-118, 148-150`: `agents` is a
  `BTreeMap` with no cap on count or on field sizes.
- `crates/acd-client/src/conn.rs:422`: every applied event clones the whole
  `Session` into the watch channel, so cost grows with total state per event.

**Attacker.** (b) a paired peer (compromised machine, or a machine whose
operator is not the user). The pin only proves the peer is the machine that
was paired, not that it is well behaved.

**Steps.** After `welcome` and an empty snapshot, send `agent.updated` frames
with distinct agent ids and a 15 MiB `title` each.

**Impact.** The app holds all of it and clones all of it on each event. One
bad machine takes down the whole app (including approvals for good
machines), or the iOS app, which has a much lower memory limit. This fails
closed (nothing is answered wrongly) but it is a denial of service from the
weakest paired machine.

**Reproduced: yes.** Scratch test `zz_hostile_peer.rs`: a pinned TLS peer
sends 8 `agent.updated` frames with 15 MiB titles. The client reaches `Online`
holding 8 agents / 120 MiB of titles; peak process RSS 426 MB (client and
peer share the test process). A single 40 MiB frame is rejected by the 16 MiB
frame default, which confirms the limit that applies.

**Minimal fix.** Set `max_message_size` and `max_frame_size` to 1 MiB (the
documented limit) on the client config. Cap agents per machine (for example
2,000) and per-field lengths when applying a snapshot or event, and drop the
connection with a resync on violation. Test: the repro with a 2 MiB frame
must end in a resync, not in state.

## 4. A hostile peer can flood notifications and grow the dedupe set (Low–Medium)

**Where.** `crates/acd-client/src/inbox.rs:914-940`. Every `request.opened`
event raises an `InboxEvent::RequestOpened`, whether or not the agent or
request exists in the snapshot (`agent_title` falls back to `req.agent_id`,
line 928-930). Dedupe is `inbox.notified` (`inbox.rs:138, 932`), a
`HashSet<(machine_id, request_id)>` that is never pruned. The Mac app turns
each event into an OS notification (`app/src-tauri/src/notify.rs:75-88`, one
`spawn_blocking` each).

**Attacker.** (b).

**Steps.** Send `request.opened` events with new ids and an agent id that is
not in the snapshot.

**Impact.** Notification spam with peer-chosen title text (a convincing
"approve now" lure), memory growth in `notified`, and blocking-pool pressure
(the macOS backend can block up to 2 s per notification). The follow-ups
record that the history should be bounded to stop *repeat* notifications on
reconnect; they do not record this flood.

**Reproduced: yes.** Scratch test `zz_notify_flood.rs`: a paired loopback
peer sends 3,000 `request.opened` frames for the non-existent agent `ghost`;
1,466 `InboxEvent`s were delivered to the subscriber within 15 s (the rest
were lost to the broadcast lag, not filtered).

**Minimal fix.** Notify only for a request that is `pending` on an agent in
the applied session, and only while `open`. Rate-limit notifications per
machine (for example 5 per 10 s) and bound `notified` (LRU, a few thousand).

## 5. Close code 4408 makes the client reconnect with no delay (Low–Medium)

**Where.** `crates/acd-client/src/conn.rs:612-621`: `End::TooSlow` waits
`Duration::ZERO`, and does not use `backoff.next_delay()` like every other
end. `end_for_close` (`conn.rs:303-310`) maps close code 4408 to it.

**Attacker.** (b), or an overloaded legitimate daemon: the daemon closes slow
clients with 4408 (`server.rs:164-167, 315-317`), and each reconnect asks for
a fresh snapshot, which is the thing that was too slow.

**Impact.** A peer that completes `welcome` and then closes with 4408 sends
the client into a tight loop of TLS handshake, `hello` with the token, and
resend of in-flight commands. Battery and CPU burn on the Mac or phone, and
for a legitimate daemon a self-sustaining reconnect storm.

**Reproduced: yes.** Scratch test `zz_reconnect_spin.rs` with production
backoff settings (base 1 s, max 30 s): 442 `hello`+`welcome` cycles in 3 s.

**Minimal fix.** Use `backoff.next_delay()` for `TooSlow` (and reset it only
once the session is `ready`). Test: the repro must show at most 3 cycles in
3 s.

## 6. Pairing host is not validated beyond a few characters (Low)

**Where.** `crates/acd-protocol/src/pairing.rs:91-96` rejects only
whitespace and `/ @ ? # [ ]`. `inbox.rs:109-118` derives the default machine
name from the host with no sanitising.

**Attacker.** (d) hands the user a crafted line.

**Reproduced: yes.** Scratch test `zz_pairing_edges.rs`: these hosts parse
and round-trip: `\u{202e}moc.live` (bidi override), `ho\0st` (NUL), `host\x7f`,
a 100,000-byte host, `127.0.0.1\u{200b}`, full-width letters, `a:b`.

**Impact.** Spoofed or garbled machine name in cards and notifications, and a
huge string in `settings.json`. The connect path still refuses anything that
does not resolve to a Tailscale or loopback address (`endpoint::vet`), so
there is no network exposure. Limited because the user must import the line.

**Minimal fix.** Reject control, bidi and separator characters (reuse
`acd_protocol::text`), require ASCII (or IDNA) hostnames, and cap the host at
253 bytes.

## 7. `agent.send` uses a weaker character rule than typed answers (Low)

**Where.** `crates/acd-core/src/registry.rs:709-713, 784`: `agent.send`
refuses control characters only. Typed answers also refuse bidi overrides and
line/paragraph separators (`registry.rs:752-756` via
`acd_protocol::text::has_forbidden_chars`). `text.rs:1-3` states the intent
that every side refuses the same text.

**Impact.** A token holder (or a buggy client) can put bidi-override text into
an agent prompt, which can hide the real content of a command in the agent's
transcript or in review UIs. Needs the token, so it is hardening, not an
access bypass.

**Reproduced: no** (by code reading). Test idea: `check_send(agent, "a\u{202e}b")`
must return `BadRequest`; it returns `Ok` today.

**Minimal fix.** Call `has_forbidden_chars` in `check_send`.

## 8. `Credential` and `MachineConfig` print the token under `Debug` (Info)

`crates/acd-client/src/conn.rs:24-37`: both derive `Debug`, and
`Credential::Token(String)` is the client token. No call site formats them
today (grep of `acd-client`, `acd-app-core`, `acd-inbox`, `app/src-tauri`),
so nothing leaks now. `MachineSecret` and `Pairing` already redact
(`settings.rs:153-160`, `pairing.rs:47-56`); apply the same here. Test:
`format!("{:?}", Credential::Token("SECRET".into()))` must not contain
`SECRET`.

---

## Considered and rejected

- **Timing of token comparison.** `Auth::check` (`server.rs:47-54`) uses
  `ct_eq`; it leaks only the length, and valid tokens are fixed at 43 bytes.
  Pin comparison (`pairing.rs:13-18`) is constant time and canonical.
- **Token brute force.** 256-bit token; no lockout needed.
- **TLS downgrade or pin bypass.** Server and client are both TLS 1.3 only
  (`tls.rs:181`, `client/tls.rs:62`). The verifier checks the SPKI pin and
  verifies the handshake signature with the real algorithms. A new
  `ClientConfig` is built per connection (`conn.rs:198`), so no session
  resumption skips the check. The pin is verified before `hello`, so the token
  is never sent to an unpinned server.
- **Parser differential** between `x509-parser` (pin) and webpki (signature
  key). No concrete cert found; theoretical.
- **Idempotency abuse.** Keys are `(client.id, id)` plus a payload hash
  (`idem.rs:49-67`); a different payload gets `duplicate_mismatch`; a retry
  while running waits; `NotDelivered` and `request_changed` release the id.
  A token holder can fill the 16,384-entry table or reuse another client's
  `client.id`, but a token holder already has full control.
- **Replay after the 15-minute TTL.** The client stops resending after 14
  minutes (`stale_inflight`, `client/lib.rs:49`), and request ids are unique
  within an epoch, so a late replay gets `request_gone`.
- **Request-id reuse across epochs.** Ids are `r1, r2, …` and restart at
  `r1` (`registry.rs:604-605`). The client revokes in-flight commands on a
  new epoch (`commands.rs:235-238`) and cannot send until the snapshot has
  been applied. Defence in depth would be to put the epoch in
  `request.answer`.
- **Stale revision.** `claim` checks `revision` when it is present
  (`registry.rs:738`); omitting it is the documented old-peer behaviour, and
  the client always sends it when `answer.revision` is advertised.
  Capability downgrade only affects the peer that advertises it.
- **Machine-id confusion.** Routing uses the local machine key, never the
  server's `machine.id` (`inbox.rs:994-1018`; `pick_with` rechecks request id
  and revision on the card's own machine). `view.rs:366-383` hides a machine
  whose id duplicates an earlier one. A hostile peer listed first could hide a
  later machine, but only if it knows that machine's id, which it does not
  see.
- **Wire-reachable panics.** None found in `acd-core`, `acd-protocol` or
  `acd-client` source: no byte-index slicing of wire text, `expect`s are on
  adapter-side invariants, JSON depth is capped by serde_json, and the
  `AbandonGuard` handles mutex poison.
- **Server frame and queue limits.** 1 MiB frames, 128-byte ids, 64 KiB text,
  32 and 256 in-flight commands, 1,000-event queue: all enforced
  (`server.rs:74-77, 125-126, 315`; `engine.rs:463-483`).
- **Token rotation.** Sessions and detached commands are revoked (tests
  `revocation_tests`). The old token stays valid for up to 2 s (documented).
- **Local socket planting (c).** `read_local_token` verifies the config dir
  (0700, owner, no symlink) before the socket is used (`conn.rs:152-160`,
  `local.rs:54-57`). Ancestor directories are not checked; this matters only
  if `$HOME` is group- or world-writable.
- **Loopback port squatting (c).** A rogue listener on the daemon's port
  fails the pin check.
- **Pairing parse.** Trailing data, duplicate `pin=`, extra `#`, port 0,
  userinfo and brackets are rejected; the v4-mapped IPv6 form is vetted
  (`endpoint.rs:33-36`). Non-canonical literals are already recorded.
- **Clipboard.** Clear-after-import is recorded in the follow-ups.
  `import_and_clear` leaves the line on the clipboard after a failed import
  by design.
- **Logs.** `acd-core` logs ids and adapter reasons only. The adapter reason
  strings passed to `engine.rs:359, 405` come from `acd-adapters`, which was
  out of scope; check that T3 error text never reaches them.

## Not checked

- Adapters (`acd-adapters`), the hook endpoint, `hook.token`, the external
  producer API, and `crates/acd` (daemon bind rules, `acd pair` output, TLS
  key file permissions, service install).
- The Tauri shell: how the clipboard is read and cleared, rendering of peer
  text in Svelte (XSS, bidi), the iOS shell, and the voice crates.
- Live behaviour: real Tailscale ACLs, MagicDNS, macOS Keychain prompts and
  access control, Universal Clipboard, iOS memory limits.
- Fuzzing, load tests of the pre-auth limits beyond the repro, and
  dependency advisories (`cargo audit`).
- Windows and macOS builds; all reproductions ran on Linux as root.

Throwaway tests (`zz_preauth.rs`, `zz_hostile_peer.rs`, `zz_notify_flood.rs`,
`zz_reconnect_spin.rs`, `zz_pairing_edges.rs`) lived in a scratch copy and
are not part of this change.
