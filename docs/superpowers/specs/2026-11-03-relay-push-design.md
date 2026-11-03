# Relay and push: design spec

Date: 2026-11-03
Status: draft, reviewed once by Sol (gpt-6.1-sol, 2026-11-03): **accept with
changes**. Not implementation-ready: apply the changes in "Review status"
below first. No code yet.
Roadmap: [Road to 1.0, step 3](../plans/2026-10-07-roadmap.md) ("A relay
that carries a wake-up, never request content, and push delivery").
Reads with: [the v1 design spec](2026-10-07-agent-inbox-design.md), the
[acd/1 protocol guide](../../../crates/acd-protocol/PROTOCOL.md), the
[iOS spike](../../research/2026-10-12-ios-spike.md).

Words in this document:

- **Device**: a phone running an agent-caller client (Android first, iOS
  later).
- **Push service**: Firebase Cloud Messaging (FCM) or the Apple Push
  Notification service (APNs).
- **Push address**: the token the push service gives the app on the device
  (FCM registration token or Firebase Installation ID; APNs device token).
- **Relay**: the new service that holds the push credentials and turns a
  wake request from `acd` into a push-service call.
- **Handle**: an opaque blob the relay issues to a device. It seals the
  push address. `acd` stores handles, never push addresses.
- **Wake**: one content-free push to one device.

## Review status (2026-11-03)

An independent review found no mechanism by which the relay alone can read
request content or approve anything. It also found six blocking problems and
one missing decision. Until they are folded into the sections named, treat
those sections as superseded by this list.

Blocking, with the ruling for each:

1. **Transient outage silences alerts for good (§5.4, §7).** After two failed
   sends the registration becomes `relay_error` and wakes stop until
   re-registration, which contradicts "recovers when the relay returns". Retry
   transient failures on later wakes with bounded backoff; disable only on an
   invalid registration.
2. **A wake gives no route to the machines (§1, §2.4, §7, §9).** With
   Tailscale off the phone gets the alert and cannot fetch or answer. Generic
   alert plus manual VPN is the supported behaviour; rich background fetch is
   optional and conditional.
3. **"Fixed notification text" is false under relay compromise (§2.4, §4.3,
   §4.7).** A compromised relay controls the text and can phish ("Tap to
   approve deploy"). It still cannot approve anything. State this as a limit;
   audit click actions, categories, links, images and inherited actions.
4. **Replay and wake-flooding need device-side limits (§3.5, §4.5-4.6).** A
   stolen handle allows 120 wakes per hour, each possibly connecting to every
   paired machine, and a compromised relay skips its own limits. Add a
   device-wide fetch cooldown, a bound on concurrent connections, total
   deadlines and notification replacement.
5. **No individual handle revocation (§4.5, §4.8, §5.3).** Ruling: accept a
   30-day abuse window plus the device-side limits from 4, unless the owner
   asks for relay-side revocation.
6. **The registration challenge is underspecified (§4.4, §5.3, §8-9).** Define
   code entropy, confirmation attempt limits, atomic single use, a cap on
   outstanding challenges, and binding to platform, app and address; IP-only
   issuance limits allow distributed flooding. Resolve "reads only `v`" versus
   the registration's required `c` field.

Factual corrections: `acd pair` prints the existing client token and does not
rotate it (`crates/acd/src/pair.rs`); "no push capability" cannot tell an old
daemon from a new unconfigured one by capability presence alone; the V1
fallback gives only seconds in `onMessageReceived`, so extra fetching needs
WorkManager; do not map every `INVALID_ARGUMENT` to a dead address; do not
expect immediate token invalidation after reinstall (APNs may keep returning
success); the metadata-privacy claims overstate: self-hosting changes who sees
the metadata, not whether it exists; session presence does not prove the app
is in the foreground.

**Missing decision for the owner (undecided):** may a locked phone show
request content (machine name, permission title) in the notification? The
reviewer and the author recommend generic text only by default, with opt-in
enrichment.

Phase order: move P3 (the Android probe) before freezing the P1 and P2
registration contract; add flood protection and challenge-abuse tests before
P4 and P5 are deployed; P6 needs a minimal registration and fetch
implementation; keep APNs optional until its routing, Keychain and extension
probes pass. §3.3's reference to "P6" should read P8.

## 0. Facts this design starts from

Read from the current tree (commit `108034c`, version 0.9.0):

- Clients reach `acd` only over loopback, the Unix socket, or Tailscale
  addresses, with TLS 1.3 and an SPKI pin (PROTOCOL.md lines 7-30).
- There is one client token per daemon. Every client presents the same
  token in `hello` (`crates/acd-core/src/server.rs:22-55`, check at
  `:205`). There is no per-device credential.
- `hello.client.id` is self-declared and not authenticated
  (`server.rs:180-219`). The app makes one UUID per install and uses it for
  every machine (`crates/acd-client/src/settings.rs:37`, `:214`).
- An unknown client message `type` fails to deserialize and gets a
  `bad_request` result (`server.rs:342-354`). It does not close the
  session.
- Capabilities are a fixed list:
  `["answer", "send", "mark_seen", "answer.revision"]`
  (`crates/acd-protocol/src/lib.rs:14`), sent in `welcome`
  (`crates/acd-core/src/engine.rs:155-162`).
- `request.opened` is produced by the registry
  (`crates/acd-core/src/registry.rs:114`, `:640`) and published by the
  engine (`engine.rs:455`).
- `CommandResult.outcome` has one variant, `Delivered`
  (`crates/acd-protocol/src/messages.rs:150-160`).
- The workspace has no HTTP client dependency (root `Cargo.toml` lines
  14-60: axum server, tokio-tungstenite, rustls, no reqwest or hyper
  client). **acd makes no outbound internet connection today.** The relay
  adds the first one.
- `acd token rotate client` closes sessions and cancels commands authorized
  under the old token (PROTOCOL.md lines 70-76).
- No Android client exists. It is roadmap step 4. Step 3 therefore needs a
  small Android probe app to verify FCM (§11, phase P3).
- iOS v1 is foreground only. The spike lists "no notifications and no
  background refresh" as a risk (iOS spike, line 289).

## 1. Goals and non-goals

**Goals**

1. A phone that is locked or has the app closed learns that some agent
   may need the user, within seconds in the normal case.
2. The relay never sees request content: no agent, machine, project, path,
   command, title, option, answer or count of requests.
3. The relay, the push services and anyone holding a handle cannot answer,
   approve, send or mark anything. Every action still needs the client
   token over pinned TLS to `acd`, as today.
4. The owner can run the relay themselves, from this repository.
5. Push is optional. With no relay configured, `acd` behaves exactly as
   0.9.0 and makes no outbound connection.
6. 0.9.0 clients and daemons keep working against new ones.

**Non-goals**

- Answering from the notification (action buttons). Phase 1 opens the
  inbox; the user answers there. See D6.
- Content in the push payload, encrypted or not. Encrypted content was
  considered and rejected: the push service and relay would still learn
  sizes and timing, and a decryption key on the phone's push path is a
  second secret to manage. The device fetches content from `acd`.
- Reaching `acd` without Tailscale. The fetch after a wake uses the
  existing acd/1 path. A non-Tailscale path is a separate spec.
- Per-device client tokens. Wanted (see §4.9), but its own change.
- Desktop push. The Mac and Omarchy clients already hold live sessions.
- Guaranteed delivery. Push is best effort on both platforms (§6).

## 2. Architecture

```
 acd (machine A) ─┐  HTTPS POST /v1/wake {handle}            FCM HTTP v1
 acd (machine B) ─┼──────────────────────────────► relay ─────────────► device
                  │  (only if [push] is configured)          or APNs     │
                  │                                                      │ wake (no content)
                  │◄─────────────── acd/1 over Tailscale, pinned TLS ────┘
                  │                 hello + client token, snapshot
```

### 2.1 Who runs the relay

The relay is a small open-source HTTP service in this repository
(proposed crate `crates/acd-relay`, binary `acd-relay`). It holds:

- the push credentials: an FCM service-account key (the HTTP v1 send
  method needs OAuth scope `firebase.messaging`), and later an APNs
  token-signing key (`.p8`);
- a sealing key for handles;
- in-memory rate-limit counters.

It has no database.

**Push credentials bind the relay to an app build.** FCM sends only to apps
in the Firebase project whose credentials sign the request; APNs sends only
to the bundle ID of the team whose key signs the JWT (APNs: "the team ID
and associated bundle IDs" are related to a connection on the first push,
token-based connection page). So:

- **Self-hosted relay** = the owner's own Firebase project and Apple team,
  and an app built with that project's config. This is the 1.0 setup: the
  maintainer builds the app and runs the relay.
- **Third parties who install a published app** cannot use their own relay
  unless they rebuild the app with their own push project. They would use
  the publisher's relay.
- **A hosted default** means: the publisher runs a public relay for every
  install of the published app; it sees metadata for every user (§4.2,
  §8); it is an open target for spam and must enforce limits per handle,
  per push address and per source IP; it needs uptime, key custody, a
  privacy notice, and a way to ban abusers. Whether a privacy notice or
  data-protection registration is legally required was not checked.
  Recommendation: no hosted default for 1.0 (D1).

The relay can also listen on a Tailscale address only. Then `acd` reaches
it over the tailnet, and only the relay talks to the internet. This hides
the machines' public IPs from the relay host's provider and lets `acd`
keep its "no internet egress" property (D2).

### 2.2 Registration (device → relay → device → acd)

1. The app gets a push address from FCM (later APNs).
2. The app asks the relay for a handle:
   `POST /v1/handles` (§5.3). The relay proves the device holds the
   address (D4, recommended: a challenge push), then returns
   `{handle, expires_at}`.
3. The app sends the handle to every paired machine whose `welcome`
   advertises the `push` capability, with `push.register` over acd/1
   (§5.1).
4. `acd` stores the registration in `push.json` in the config directory,
   mode 0600, keyed by `client.id`.

The app repeats steps 2-3 when the push address changes, when the handle
is within 7 days of expiry, and after pairing a machine. It does this on
foreground connect; there is no background registration.

`acd` never sees the push address. The relay never sees which machine or
agent a handle is for.

### 2.3 Wake (acd → relay → push service)

`acd` sends a wake to each registered device when, and only when:

- a `request.opened` change is published (any kind, answerable or not), and
- no acd/1 session with that `client.id` is open on this daemon (the app is
  in the foreground and already sees it), and
- the per-device debounce allows it: at most one wake per device per 30 s.
  A request that opens inside the window schedules one trailing wake at
  the window's end. Nothing else is queued.

Not a trigger in phase 1: an agent becoming `done`, a resolution, a
follow-up. (Open: D7.)

`acd` posts `{handle}` to the relay over HTTPS (or over Tailscale, D2). It
uses the response only to update the registration's status (§5.4). It
never reads anything else from the relay, and nothing the relay returns
changes the registry, a request or a token.

### 2.4 Device after a wake

1. The OS delivers a notification built from fixed text in the payload
   ("An agent may need you"). On Android it is a v2 `RemoteNotification`
   with `mutableContent: true`; on iOS an alert with `mutable-content: 1`
   handled by a Notification Service Extension (§6).
2. The app code (Android messaging service; iOS extension) has about 30 s
   (iOS; Android bound not verified). It connects to every paired machine
   over acd/1 exactly as the foreground app does: Tailscale address check,
   SPKI pin, client token. It reads the snapshot and closes.
3. It rewrites the notification from that snapshot: for example "Claude on
   minos: Bash approval". If nothing is pending, it says "Nothing needs you
   now" (whether a mutable notification can be removed instead is not
   verified on either platform).
4. If no machine is reachable, the text stays generic and adds "Open the
   inbox to check". It never shows anything from the payload except the
   fixed text.
5. Tapping opens the inbox. The inbox answers through the normal acd/1
   path. There are no notification actions in phase 1.

The payload is never authority for anything. The app ignores every
payload field except `v`.

## 3. The wake message

### 3.1 acd → relay

```json
{"v": 1, "handle": "<opaque, ≤ 512 bytes>"}
```

That is the whole body. No machine id, request id, timestamp, count, kind,
priority or text. `acd` sends it with a 5 s timeout.

### 3.2 relay → FCM (HTTP v1, `POST https://fcm.googleapis.com/v1/projects/{project}/messages:send`)

```json
{
  "message": {
    "token": "<push address from the opened handle>",
    "data": {"v": "1"},
    "androidV2": {
      "collapseKey": "acd-wake",
      "ttl": "3600s",
      "remoteNotification": {
        "mutableContent": true,
        "notification": {
          "title": "Agent inbox",
          "body": "An agent may need you",
          "tag": "acd-wake",
          "channelId": "acd_wake",
          "visibility": "PRIVATE"
        }
      }
    }
  }
}
```

- If the handle holds a Firebase Installation ID instead of a registration
  token, the relay sets `fid` in place of `token`. The discovery document
  (revision 20261023) marks `token` deprecated in favor of `fid`. The
  handle stores the kind (§5.3).
- If the `androidV2` path is not usable (D5), the fallback is the v1 shape:
  `android: {priority: "HIGH", collapseKey: "acd-wake", ttl: "3600s"}` with
  `data: {"v": "1"}` only, and the app posts a local notification itself.

### 3.3 relay → APNs (later, phase P6)

```
:path            /3/device/<device token>
apns-push-type   alert
apns-priority    10
apns-topic       <bundle id>
apns-collapse-id acd-wake
apns-expiration  <now + 3600>
body             {"aps": {"alert": {"loc-key": "ACD_WAKE_BODY"},
                          "mutable-content": 1, "thread-id": "acd"}, "v": 1}
```

`loc-key` makes the phone show a string from the app bundle, so the text
does not travel. `interruption-level` is left at its default (D8).

### 3.4 Size

The FCM body above is under 600 bytes; the APNs body is under 150 bytes.
APNs limits the payload to 4096 bytes (sending-requests page). The FCM
limit was not read from an official page in this session (§6.4).

### 3.5 Deliberately absent

| Absent | Why |
|---|---|
| Machine id, name, address | The relay and push service would learn which machine is active. The device checks every paired machine. (D3) |
| Request id, agent, kind, title, count | Content. |
| A per-wake nonce or sequence | Replay of a wake is harmless: it only makes the device fetch the truth (§4.5). A nonce adds state and gives the relay nothing to check. |
| A timestamp | The push service records one anyway; the device trusts `acd`'s `opened_at`. |
| Any token or secret | The wake must be safe to leak. |
| Any instruction or action | The relay must not be able to say "do X". |

The collapse key and notification tag are one constant, so a burst of wakes
from several machines replaces one notification (FCM allows at most 4
collapse keys at a time; discovery document `AndroidConfig.collapseKey`).

## 4. Trust model and threat model

### 4.1 Trust boundaries

| Party | Today | After this change |
|---|---|---|
| `acd` | Trusted. | Trusted. Now also makes outbound HTTPS calls to one configured relay URL. |
| Client token holder | Can answer anything. | Same, plus can register and unregister push for any `client.id`. |
| Tailscale | Transport and addressing. | Same. Also carries the post-wake fetch. |
| Relay | Did not exist. | Untrusted for integrity and content. Trusted only to deliver wakes; can drop them. |
| FCM / APNs (Google, Apple) | Not involved. | Untrusted for content. See timing and push addresses. |

**Invariant R1.** No message from the relay or a push service can cause
`acd` or a client to deliver an answer, send text, mark seen, or change a
request. Answers need the client token over acd/1, which the relay never
has.

**Invariant R2.** No request content reaches the relay or a push service,
including in logs and error bodies.

**Invariant R3.** Push is off unless the machine owner configures a relay
URL in `config.toml`. A client cannot choose where `acd` sends HTTP
requests.

### 4.2 Relay operator reads metadata

What it sees per wake: the source IP of each `acd` (public egress IP,
which reveals rough location and ISP, unless D2's tailnet option is used),
the time, the handle, and after opening it the push address and platform.
Over time it learns: how many devices a person has, how many machines wake
them, when the person's agents ask for input (a work-hours profile), and
bursts (an agent stuck in a loop).

It does not see: content, which machine (beyond IP), which agent, or
whether the user answered.

Mitigations: run your own relay (D1); relay on the tailnet (D2); debounce
caps the rate at one per 30 s per device. Not proposed: random delay or
dummy wakes. They cost latency or battery and a self-hosting owner gains
nothing from them. Listed in §12.

### 4.3 Relay compromise

An attacker who controls the relay gets the push credentials, the sealing
key and in-memory state. They can:

- send any notification with any text to every device of the app that
  registered with this relay, and any device whose push address they know.
  This includes phishing text ("Tap to approve"). **Residual risk.**
  Mitigations: the app shows only fixed text and rewrites it from `acd`;
  there are no notification actions; tapping only opens the inbox, which
  shows `acd`'s data. If the rewrite fails, the OS shows the attacker's
  text, but a tap still leads only to the real inbox.
- decrypt handles they see and learn push addresses.
- drop or delay wakes (denial of alerts).
- send false `gone` responses so `acd` marks registrations stale (denial of
  alerts, visible in `acd push list` and `acd status`).

They cannot: read requests, reach `acd` (the relay never connects to
`acd`; it has no client token, no pin, and no tailnet path unless the
relay host is in the tailnet; see §12 item 3), answer or approve anything.

Recovery: rotate the FCM service-account key and the APNs key in the
Firebase and Apple consoles, rotate the sealing key (every handle becomes
invalid; devices re-register on next foreground), redeploy.

### 4.4 Stolen push address

FCM and APNs require the project's credentials to send, so an address alone
lets nobody send. With an open relay, an address could be turned into a
handle; the challenge push (D4) stops that, because the minting call
succeeds only if the caller can read a push sent to that address. The relay
also rate-limits per push address (§4.6), whichever handle names it.

### 4.5 Stolen handle; replay

A handle is a capability to wake one device, nothing more. It lives in
`push.json` (0600) on each machine and in the app's secure storage. It is
never returned over acd/1 (`push.list` returns device ids and status only).

A thief can send wakes, limited by the relay's per-handle and per-address
limits. Each wake makes the device fetch the truth and show "Nothing needs
you now". Replay of a captured wake request (TLS prevents capture on the
wire) or of a push has the same effect.

Handles expire after 30 days. Earlier revocation: the device deletes its
push token or installation (new address), so every old handle points at a
dead address. Relay sealing-key rotation revokes all handles at once.

### 4.6 Spam and wake flooding

| Source | Limit |
|---|---|
| A busy agent in `acd` | 1 wake per device per 30 s, one trailing wake, no queue. |
| Any caller, per handle | Relay: 10 per minute, 120 per hour. |
| Any caller, per push address | Relay: same numbers, shared across handles. |
| Per source IP | Relay: 600 per hour. `POST /v1/handles`: 10 per hour. |
| Request size | Relay: body ≤ 2 KiB, handle ≤ 512 bytes, 5 s read deadline. |

Over a limit the relay answers 429 with no body and sends nothing. The
numbers are starting values for review, not measured.

Apple asks for no more than "two or three per hour" for background pushes
(background updates page). This design uses alert pushes on iOS for that
reason (§6.2).

### 4.7 A malicious relay trying to cause actions

The only data that flows from the relay into the system:

1. the HTTP status of a wake call, which `acd` maps to a registration
   status (§5.4), and
2. the push payload on the device, of which the app reads only `v`.

Neither path reaches the answer code. Tests in §9 assert this by feeding
hostile relay responses and payloads and checking that the registry and
the outbound acd/1 command stream do not change.

The app must not deep-link from payload data. A tap opens the inbox root,
not a request named by the payload.

### 4.8 Token rotation and revocation

| Event | Effect |
|---|---|
| `acd token rotate client` | Deletes every push registration on that machine, in the same step that closes sessions. A device that lost its token must not keep getting wakes. Devices with the new token re-register on connect. |
| Re-pair (`acd pair` with a new token) | Same as rotation. |
| `acd tls rotate` | No effect on push. |
| `acd push remove <device>` (new CLI) | Deletes one registration. |
| `push.unregister` | Deletes the caller's own `client.id` registration. |
| FCM address changes (`onNewToken`) | App mints a new handle and re-registers with each reachable machine. Unreachable machines keep the old handle until the relay reports `gone`. |
| Handle expiry | Relay answers `gone`; `acd` marks the registration expired and stops sending. |
| Lost phone | Rotate the client token on every machine (the phone holds it). Registrations go with it. |

### 4.9 What the shared client token means here

Any holder of the client token can register a handle under any `client.id`
and overwrite another device's registration (so that device stops getting
wakes), or remove it. They can already answer every request, so this adds
no new power over agents. It adds one: denial of alerts to another device.
Per-device tokens would fix it and are out of scope (§1).

### 4.10 Fail-closed rules

Consistent with CLAUDE.md "If the daemon cannot prove which agent and
screen an answer belongs to, it does not deliver the answer":

- F1. A wake never changes request state. Answers go only through the
  existing claim path.
- F2. A registration whose relay URL differs from the configured one is
  refused (`bad_request`). `acd` sends to the configured URL only.
- F3. If `push.json` is missing, malformed, or has loose modes, `acd`
  starts without push, reports `push: degraded` in `acd status`, and does
  not rewrite the file until a valid registration arrives. It does not
  refuse to start: push is optional, agents must not hang (v1 invariant
  "fail open" applies to agents, not to push).
- F4. If the relay TLS certificate does not validate against the system
  roots (or, on a tailnet relay, the configured pin), no wake is sent.
- F5. The app shows the generic text when it cannot verify a machine (pin
  mismatch, `IdentityChanged`), never stale or payload text.

## 5. acd and acd/1 changes (proposal)

These are proposals for review. PROTOCOL.md is not edited by this spec.

### 5.1 New capability and commands

Capability `push`, advertised in `welcome` only when `[push] relay` is set.

```text
push.register    {id, relay, handle, platform: "android"|"ios", expires_at}
push.unregister  {id}
push.status      {id}
```

- `push.register` registers the session's `hello.client.id`. There is no
  separate device id field, so a session cannot register another
  `client.id` except by opening a session under it (§4.9).
  - `relay` must equal the configured URL byte for byte (F2), else
    `bad_request` with message `relay mismatch`.
  - `handle` 1-512 bytes of base64url; `expires_at` RFC 3339. `acd` does
    not parse the handle.
  - At most 8 registrations per daemon. The ninth gets `bad_request`
    (`too many push devices`). An existing `client.id` replaces its own.
  - Result: `{id, ok: true}` with no `outcome`.
- `push.unregister` deletes the caller's registration. Idempotent: `ok`
  even if none exists.
- `push.status` returns the caller's registration state for display.
  Result: `{id, ok: true, push: {registered: bool, status: "ok"|"stale"|
  "expired"|"relay_error", last_wake_at: timestamp|null}}`. This adds an
  optional `push` field to `result`. Old clients ignore it.
- All three go through the existing idempotency table and in-flight caps
  (PROTOCOL.md §4). A retried `push.register` with the same id and payload
  returns the cached result.
- `push.*` never returns a handle or another device's state.

Logs: `client.id`, status and relay HTTP status. Never the handle.

### 5.2 Configuration

```toml
[push]
relay = "https://relay.example.net"   # absent: push off, no egress
# relay_pin = "sha256:..."            # optional; required for a tailnet relay without a public cert
```

`acd push list` prints `client.id`, platform, status, last wake time.
`acd push remove <client-id>` and `acd push test <client-id>` (sends one
wake) are local CLI commands over `acd.sock`.

### 5.3 Relay API

```text
POST /v1/handles        {platform: "android"|"ios", app: "<package or bundle id>",
                         address_kind: "fcm_token"|"fcm_fid"|"apns",
                         address: "<push address>"}
 → 202 {challenge_id}                  // relay sends a push with data {v:"1", c:"<code>"}
POST /v1/handles/confirm {challenge_id, code}
 → 200 {handle, expires_at}
POST /v1/wake           {v: 1, handle}
 → 204 | 404 {"error":"gone"} | 429 | 400 | 503
```

The challenge push is a data-only message, not a visible notification, so
it is seen only by the running app at registration time. Handle contents,
sealed with AEAD (XChaCha20-Poly1305 or AES-256-GCM, D9) under the relay's
key, with the key id in clear:

```text
{kid, platform, app, address_kind, address, issued_at, expires_at}
```

### 5.4 How acd reads relay responses

| Relay answer | acd |
|---|---|
| 204 | `status = ok`, update `last_wake_at`. |
| 404 `gone` | `status = expired` (handle expired or push address dead). Stop sending until re-registered. |
| 429 | Drop this wake. No retry. |
| 400 | `status = relay_error`. Stop sending to this registration. |
| 5xx, timeout, TLS error | Retry once after 10 s, then drop. `status = relay_error` until the next success. |
| Anything else, any body | Treated as 5xx. The body is not read beyond 1 KiB and not logged. |

`acd` never sends wakes to a registration in `expired` or `relay_error`
until the device re-registers or `acd push test` succeeds.

### 5.5 Backwards compatibility

| Pair | Behavior |
|---|---|
| 0.9.0 client, new daemon | Sees an extra `push` capability, ignores it (PROTOCOL.md §2 "Clients ignore unknown"). Never registers. No change. |
| New client, 0.9.0 daemon | No `push` capability, so the client does not send `push.*` (PROTOCOL.md §5). The app shows "Alerts not available on this machine (update acd)". |
| New client, new daemon without `[push]` | No capability. App shows "Alerts are off on this machine". |
| A client that sends `push.register` anyway to 0.9.0 | `bad_request` result (unknown type, `server.rs:342-354`). Session stays open. |

The protocol stays `acd/1`. The JSON schema and `examples/` gain the three
commands and the optional `result.push` field (PROTOCOL.md §7).

## 6. FCM and APNs specifics

Sources read on 2026-11-03. `firebase.google.com` was blocked from this
session's network, so the FCM concept pages could not be read. FCM facts
below come from the official FCM REST discovery document and the Android
developer pages. Everything from the Firebase pages is marked unverified.

| # | Source | Read |
|---|---|---|
| S1 | FCM REST discovery document, `https://fcm.googleapis.com/$discovery/rest?version=v1` (revision 20261023) | 2026-11-03 |
| S2 | Android, "Optimize for Doze and App Standby", `https://developer.android.com/training/monitoring-device-state/doze-standby` (last updated 2026-10-22) | 2026-11-03 |
| S3 | Android, "App Standby Buckets", `https://developer.android.com/topic/performance/appstandby` (last updated 2026-10-28) | 2026-11-03 |
| S4 | Apple, "Pushing background updates to your App", `https://developer.apple.com/documentation/usernotifications/pushing-background-updates-to-your-app` | 2026-11-03 |
| S5 | Apple, "Sending notification requests to APNs", `https://developer.apple.com/documentation/usernotifications/sending-notification-requests-to-apns` | 2026-11-03 |
| S6 | Apple, "Handling notification responses from APNs", `https://developer.apple.com/documentation/usernotifications/handling-notification-responses-from-apns` | 2026-11-03 |
| S7 | Apple, "Establishing a token-based connection to APNs", `https://developer.apple.com/documentation/usernotifications/establishing-a-token-based-connection-to-apns` | 2026-11-03 |
| S8 | Apple, "Modifying content in newly delivered notifications", `https://developer.apple.com/documentation/usernotifications/modifying-content-in-newly-delivered-notifications` | 2026-11-03 |
| S9 | Apple, "Generating a remote notification", `https://developer.apple.com/documentation/usernotifications/generating-a-remote-notification` | 2026-11-03 |

Apple pages were read through their JSON data endpoints
(`developer.apple.com/tutorials/data/documentation/...json`), which serve
the same text.

### 6.1 Android (FCM)

- **Message kinds.** S2 and S3 say FCM "v1 ... used high priority messages
  whereas v2 uses RemoteNotification for user facing, time-sensitive
  notifications and BackgroundSync for non-urgent messages". S1 defines
  `androidV2` with `remoteNotification` ("delivered immediately and are not
  delayed by battery optimizations") and `backgroundSync` ("may be delayed
  or batched"). `androidV2` and `android` are mutually exclusive (S1).
- **Mutable content.** `AndroidRemoteNotification.mutableContent`: "the
  notification content can be updated by the app after the notification is
  received on the device" (S1). How the app receives control to update it,
  and within what time, is **unverified** (the Firebase client docs were
  not readable).
- **Notification identity.** `AndroidNotificationV2.tag` replaces an
  existing notification with the same `(tag, id)` (S1).
- **Priority (v1).** `HIGH` "attempts to deliver ... immediately, allowing
  the FCM service to wake a sleeping device"; `NORMAL` messages "won't
  open network connections on a sleeping device" (S1, enum descriptions).
- **Doze and buckets.** S2: "FCM is optimized to work with Doze and App
  Standby idle modes"; "use RemoteNotification messages to wake the app and
  access the network" for messaging apps. S3: "If the app doesn't show a
  notification upon receiving a RemoteNotification ... the user can't
  interact with the app and thus promote it to the active bucket." This is
  why the design always shows a notification.
- **Deprioritizing high-priority data messages that show nothing**:
  widely described in Firebase docs. **Unverified** here.
- **Collapse and TTL.** `collapseKey`: "only the last message gets sent
  when delivery can be resumed. A maximum of 4 different collapse keys is
  allowed at any given time." `ttl`: max and default 4 weeks; `0` means
  send now (S1).
- **Payload size limit** (4096 bytes is commonly cited): **unverified**.
- **Token lifecycle.** S1 marks `Message.token` "Deprecated: Use `fid`
  instead" and says the field also accepts a Firebase Installation ID during
  the transition. Token refresh rules (`onNewToken`), staleness windows and
  error codes (`UNREGISTERED`, `INVALID_ARGUMENT`): **unverified**; the relay
  maps any "not registered" class of error to `gone`.
- **Testing aid.** `SendMessageRequest.validateOnly`: "testing the request
  without actually delivering the message" (S1). Scopes for send:
  `firebase.messaging` or `cloud-platform` (S1).
- **Testing Doze.** `adb shell dumpsys deviceidle force-idle` and `unforce`
  (S2).

### 6.2 iOS (APNs), later

- **Background pushes are unsuitable for alerts.** "The system treats
  background notifications as low priority ... doesn't guarantee their
  delivery ... don't try to send more than two or three per hour." They
  require `apns-push-type: background` and `apns-priority: 5`. If the app
  is force-quit, "the system discards the held notification" (S4).
- **So iOS uses an alert push with `mutable-content`** and a Notification
  Service Extension. The extension gets "about 30 seconds" and runs only
  for notifications "configured in the system to display an alert"; if
  alerts are disabled for the app, it does not run (S8).
- **Headers.** `apns-push-type` must "accurately reflect the contents";
  `apns-priority` 10 sends immediately, 5 by power, 1 without waking;
  `apns-collapse-id` at most 64 bytes; `apns-expiration` 0 means one try,
  nonzero means store and retry until then; payload at most 4096 bytes (S5).
- **Storage.** "APNs stores only one notification per bundle ID" and may
  store "for 30 days or less" (S4).
- **Provider token.** Refresh "no more than once every 20 minutes and no
  less than once every 60 minutes"; an `iat` older than one hour gets
  `ExpiredProviderToken` (403) (S7).
- **Responses.** 410: "The device token is no longer active for the
  topic", with a `timestamp`; 429: "too many requests for the same device
  token" (S6). The relay maps 410 to `gone`.
- **Unverified for iOS:** whether the extension can reach a Tailscale
  address while the main app is closed; whether it can run `acd-client`
  (Rust) within its memory limit; keychain sharing of the client token
  between app and extension (the spike found no entitlements, line 144);
  whether a notification can be suppressed when nothing is pending (an
  entitlement for filtering exists in Apple's docs, not read here);
  whether push needs the paid membership (spike line 206 says "treat as
  paid").

## 7. Failure and recovery

| Failure | What happens | Recovery |
|---|---|---|
| Relay down | `acd` retries once after 10 s, then drops the wake. `acd status` shows `push: relay_error`. No alert reaches the phone. | Next wake after the relay returns. The foreground inbox is unaffected. |
| Relay up, push service down | Relay answers 503. Same as above. | Same. |
| Push delayed | Device may alert late. The extension or service fetches the current state, so a late alert shows current content or "Nothing needs you now". | None needed. |
| Push dropped | No alert. | Next request's wake. Best effort; users are told alerts can be missed. |
| Device offline | Push service stores the latest wake per collapse key until TTL (1 h). | Delivered on reconnect if inside TTL. |
| Device online, Tailscale off | Notification keeps generic text plus "Open the inbox to check". | The user opens the app (and Tailscale). |
| `acd` offline (sleep, crash) | No wakes from it. Other machines still wake. A device that wakes for another machine shows this one as unreachable only in the app, not in the notification. | After restart, a request that adapters observe again opens as a new request and wakes again (§12 item 9). |
| `acd` restarts mid-debounce | Debounce state is lost; at most one extra wake. | None. |
| Handle expired, phone not opened for 30 days | `gone`, registration `expired`. | Next foreground connect re-registers. |
| Sealing key lost | All handles invalid. | Devices re-register on foreground. |
| Clock skew on `acd` | `expires_at` is informational on `acd`; the relay decides expiry. | None. |

## 8. Privacy: what the relay stores

| Data | Where | How long |
|---|---|---|
| Push credentials, sealing keys | Relay config, files mode 0600 | Until rotated. Old sealing key kept 30 days for verification, then deleted. |
| Handles, push addresses | Not stored. Opened in memory per call. | Request duration. |
| Pending challenges `{challenge_id → HMAC(code), address hash}` | Memory | 10 minutes. |
| Rate-limit counters keyed by HMAC(handle), HMAC(address), source IP | Memory | 1 hour sliding window. Lost on restart. |
| Logs | stdout / journal | Fields: time, route, status, platform, 8 hex chars of HMAC(handle). Never handles, addresses, IPs, bodies. Retention is the host's journal setting; recommend 7 days. |

Google and Apple store what their services store; that is outside this
project's control and not checked here.

`acd` stores in `push.json`: `client.id`, platform, handle, `expires_at`,
status, `last_wake_at`. Deleted on client token rotation (§4.8).

The app stores: the handle and the relay URL, in the same secure storage
as the client token.

## 9. Test plan

### 9.1 Without a phone (CI, Linux and macOS)

acd (`crates/acd-core`, `crates/acd`), with a fake relay (an axum server
in the test, port from `free_port` per CLAUDE.md):

1. **Canary.** Open requests whose title, detail, options, cwd, project and
   agent title contain a unique marker; assert the marker is absent from
   every byte the fake relay receives, and from logs at every level.
2. A wake is sent on `request.opened` and on nothing else.
3. No wake while a session with that `client.id` is open; a wake after it
   closes.
4. Debounce: 5 requests in 2 s give 1 immediate and 1 trailing wake.
5. `push.register` with a different relay URL gets `bad_request`; nothing
   is sent to that URL (the test runs a second listener and asserts zero
   connections).
6. `acd token rotate client` deletes registrations; the next request sends
   no wake.
7. Relay responses per §5.4, including hostile bodies (huge, invalid
   UTF-8, JSON with `answer`/`approve` fields). Assert registry state and
   the idempotency table are unchanged and no adapter delivery happens.
8. Daemon without `[push]`: no `push` capability; no outbound connection
   (the fake relay sees none); `welcome` is identical to 0.9.0's.
9. `push.json` malformed or mode 0644: daemon starts, push degraded, file
   untouched.
10. Protocol: schema and `examples/` round-trip for the three commands and
    `result.push`; a 0.9.0 client fixture ignores `push` capability.

Relay (`crates/acd-relay`):

11. Seal/open round-trip; tampered, truncated, wrong `kid`, expired handles
    are refused with `gone` or 400 and send nothing.
12. Golden JSON for the FCM body (§3.2) and the APNs request (§3.3); a test
    that the outbound body has exactly the listed keys.
13. Rate limits per handle, per address (two handles, one address), per IP.
14. Challenge: a wrong code, an expired challenge, and a replayed confirm
    are refused.
15. FCM error mapping with a fake FCM endpoint.
16. Optional, needs a Firebase project and credentials, not CI:
    `validateOnly: true` sends against real FCM to check the body is
    accepted (S1).

### 9.2 Needs Android (emulator with Google Play, then a real phone)

17. Probe app receives the challenge push and the wake.
18. `RemoteNotification` with `mutableContent` is shown and rewritten by the
    app from a real `acd` over Tailscale. Measure time to rewrite.
19. App killed (swiped away), then a wake. App force-stopped (Settings),
    then a wake (expected: not delivered; record).
20. Doze: `adb shell dumpsys deviceidle force-idle`, then a wake; latency.
21. Real phone only: overnight idle, battery saver on, and the restricted
    bucket. Emulator results do not count for these.
22. Collapse: wakes from two machines inside 1 s show one notification.
23. Token refresh: clear app data, confirm the new address re-registers and
    the old handle gets `gone`.

### 9.3 Needs an iPhone and the paid membership (roadmap step 5)

24. Alert push with `mutable-content` reaches the extension.
25. The extension reaches `acd` over Tailscale with the app closed.
26. Keychain sharing between app and extension.
27. Rewrite within 30 s; fallback text when it times out.
28. 410 after reinstall; relay maps to `gone`.

## 10. Open decisions

| # | Decision | Options | Recommendation |
|---|---|---|---|
| D1 | Hosted default relay | (a) none for 1.0, owner self-hosts; (b) maintainer runs a public relay for the published app; (c) both, public relay opt-in | **(a)**. 1.0 has one user. (b) is a service with uptime, abuse and privacy duties; decide when there is a published Android or iOS build. |
| D2 | Where `acd` reaches the relay | (a) public HTTPS with system roots; (b) tailnet only, with a pin; (c) either, config decides | **(c)**, docs recommend (b). (b) keeps `acd` free of internet egress and hides machine IPs. |
| D3 | Machine hint in the wake | (a) none, device checks all machines; (b) a random per-registration tag | **(a)**. A handful of machines; no stable pseudonym in the payload. |
| D4 | Proof that the device holds the push address before a handle is issued | (a) challenge push; (b) none, rely on rate limits; (c) Play Integrity / App Attest | **(a)**. One extra push per registration; (c) is heavier and platform-specific. |
| D5 | Android message shape | (a) `androidV2.remoteNotification` with `mutableContent`; (b) v1 `HIGH` data message, app posts a local notification; (c) both, chosen per app version | **(a)**, fall back to (b) only if the P3 probe shows (a) cannot be rewritten before display. |
| D6 | Notification actions (Approve/Deny on the lock screen) | (a) none; (b) later, only after the app fetched the request from `acd` and only for non-`confirm` options; (c) never | **(a)** now, revisit as **(b)** in a separate spec. The relay must never supply the action. |
| D7 | Wake triggers | (a) `request.opened` only; (b) also `done`; (c) user-chosen | **(a)** for phase 1. |
| D8 | iOS interruption level | (a) default `active`; (b) `time-sensitive` (needs an entitlement, not checked) | **(a)**. |
| D9 | Handle cipher | (a) XChaCha20-Poly1305; (b) AES-256-GCM with random 96-bit nonce | **(a)**: random 192-bit nonces remove nonce-reuse risk. Needs a new dependency; review it. |
| D10 | Relay as a separate binary or inside `acd` | (a) separate `acd-relay` crate and binary; (b) `acd relay` subcommand | **(a)**. Keeps push credentials and the FCM/APNs code out of the daemon. `acd` still gains an HTTPS client to call the relay. |
| D11 | Direct mode: `acd` calls FCM itself, no relay | (a) no; (b) yes, for single-machine owners | **(a)**. Push credentials would sit on every machine. |

## 11. Phases (small PRs, Android first)

Each phase is one PR unless noted, with tests per CLAUDE.md.

| Phase | Content | Needs a phone |
|---|---|---|
| P0 | This spec, then the adversarial review and a revision. | No |
| P1 | Protocol: `push` capability, `push.register`/`unregister`/`status` types, `result.push`, schema and examples, PROTOCOL.md section. No behavior. | No |
| P2 | acd: `[push]` config, `push.json` store (securefs, 0600), commands, deletion on client token rotation, `acd push list/remove`. Still no outbound calls (a trait `WakeSink` with a no-op). | No |
| P3 | Android probe app (throwaway, like the iOS spike): get FCM address, receive data and `RemoteNotification` pushes, rewrite with `mutableContent`. Sent by hand with `curl` and an OAuth token. Research note in `docs/research/`. Decides D5. | Emulator, then phone |
| P4 | `crates/acd-relay`: handles, challenge, FCM sender, rate limits, logs. Fake FCM in tests. Deployment notes for self-hosting. | No |
| P5 | acd: HTTPS wake sender (new HTTP client dependency, reviewed on its own), triggers, debounce, suppression, response mapping, `acd push test`, canary tests. | No |
| P6 | Live Android check end to end: real `acd` on two machines, real relay, probe app; §9.2 tests 17-23. Research note. | Phone |
| P7 | Android client registration and post-wake fetch in the step 4 Android app. | Phone |
| P8 | APNs sender in the relay; iOS extension and keychain sharing. After the membership (roadmap step 5). §9.3. | iPhone |

Order matters: P1-P2 add no egress; P5 is the first PR where `acd` talks
to the internet, so it gets its own review.

## 12. What could go wrong

1. **The fixed notification text is a phishing surface.** Whoever holds the
   push credentials controls the text. A compromised relay can show "Tap
   to approve deploy" on every device. Only "no actions, tap opens the real
   inbox" limits it (§4.3).
2. **Mutable content may not work as assumed on Android.** If the app
   cannot rewrite the notification before the user sees it, every alert is
   generic. D5's fallback (v1 high-priority data) may be deprioritized by
   FCM if the app ever fails to show a notification (unverified).
3. **The relay host inside the tailnet.** D2(b) puts the relay on the
   tailnet. If Tailscale ACLs let it reach `acd`'s port, a compromised relay
   reaches the TLS port. It still needs the pin's private key and the
   client token, but the exposure grows. Tailnet ACLs must allow
   `acd → relay` only.
4. **First internet egress from `acd`.** A new HTTP client brings TLS root
   handling, redirects, proxies and DNS. Redirects must be off; proxy
   environment variables must be ignored or explicit; the URL is fixed by
   config. A mistake turns `acd` into an SSRF tool for anyone who can edit
   `config.toml` (who already owns the machine) — or for a client, if F2 is
   ever weakened.
5. **Shared client token** lets any paired client silence another device's
   alerts (§4.9). Users may not expect that.
6. **Metadata profile.** Even a well-behaved hosted relay learns when a
   person works and how much their agents block. Tailnet relay or
   self-hosting is the only real fix.
7. **iOS extension constraints.** The extension must run Rust TLS, the pin
   check and Tailscale routing in a short-lived process with a memory
   limit. If it cannot, iOS alerts stay generic, and the iOS value of this
   feature drops.
8. **Silent loss.** Push is best effort; dropped wakes leave a request
   unseen. Users may trust the phone too much. The app must say "alerts can
   be missed" once, plainly.
9. **Duplicate wakes after an `acd` restart.** If adapters re-open a still
   pending request as a new request after restart (v1 rules on restart are
   in spec §5.1 and follow-ups), the phone alerts again. Bounded by
   debounce; not harmful, but noisy.
10. **Rate-limit numbers are guesses.** Too low drops real alerts during a
    burst across machines; too high lets a stolen handle annoy.
11. **FCM API churn.** The discovery document already deprecates `token` for
    `fid` and adds `androidV2`. The relay must follow FCM changes; a
    self-hoster who does not update loses alerts.
12. **Deep links.** A future change that opens "the request named in the
    push" would make the payload an input to the answer UI. Keep the tap
    target at the inbox root (§4.7).
13. **Handle in backups.** `push.json` and the app's storage may land in
    machine or phone backups. A leaked handle only wakes, but the review
    should confirm the app stores it where the client token is stored.
14. **Debounce hides a second, more urgent request** for up to 30 s. The
    trailing wake covers it; the latency remains.
15. **Challenge push consumes the app's goodwill.** A data-only challenge
    is the kind of push Android may deprioritize. It happens once per
    registration, while the app is in the foreground.

## 13. Not verified in this session

- Every Firebase documentation page (`firebase.google.com` was blocked):
  data vs notification semantics in the client SDKs, payload size limit,
  deprioritization of high-priority messages, token staleness and refresh,
  error codes. Facts used here come from S1-S3 instead.
- How Android delivers a `mutableContent` notification to the app and how
  long the app has to rewrite it.
- Whether push requires the paid Apple membership, the iOS extension's
  ability to reach Tailscale and share the keychain, the filtering
  entitlement, the `time-sensitive` entitlement.
- Legal duties of a hosted relay.
- Tauri 2 support for FCM on Android (plugin availability).
