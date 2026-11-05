# Relay and push: design spec

Date: 2026-11-03, revised 2026-11-04
Status: **implementation-ready** for the relay (P4) and the sender state
machine (part of P5). The review of 2026-11-03 (Sol, gpt-6.1-sol: accept with
changes) is folded into the sections below; see "Change log". Three owner
decisions are **PROVISIONAL** (next section).
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

## Provisional owner decisions

The owner has not decided these. The most conservative choice is built in,
each as one named constant or config key, so changing it is a one-line
change. The relay constants live in `crates/acd-relay/src/provisional.rs`.

| # | Question | Provisional choice | Where to change it |
|---|---|---|---|
| P-A | May a locked phone show request content (machine name, permission title)? | **No.** The push carries a fixed generic title and body. The app never puts machine, agent or request text into a notification. Content is shown only inside the unlocked inbox. | `WAKE_TITLE`, `WAKE_BODY` in `provisional.rs`; §2.4 |
| P-B | Relay-side revocation of one handle? | **None.** Accept the 30-day abuse window plus the device-side limits of §4.6. Sealing-key rotation revokes all handles at once. | `HANDLE_LIFETIME_SECS` in `provisional.rs`; §4.5 |
| P-C | A stolen push address | **Documented limit.** An address alone cannot send (needs the push credentials) and cannot mint a handle (needs the challenge code, which is pushed to the address' owner). A handle is sealed and bound to platform, app and address. | §4.4 |

## Change log

- 2026-11-04: review rulings folded in (this section replaces "Review
  status"). Changes by section:
  - §2.3, §5.4, §7: transient failures retry on later wakes with bounded
    backoff; only an invalid registration disables. The wake is no longer
    suppressed by an open session (a session does not prove the app is in
    the foreground).
  - §2.4, §4.6: a wake gives no route to the machines; with Tailscale off
    the alert is generic. Device-side limits (fetch cooldown, concurrent
    connections, deadlines, notification replacement) have numbers.
  - §4.3: the phishing-text limit under relay compromise is stated plainly.
  - §4.5, §4.8, §5.3: no per-handle revocation (P-B). `acd pair` does not
    rotate the token, so re-pair has no push effect.
  - §5.3: the registration challenge is fully specified (code entropy,
    attempt limit, atomic single use, outstanding cap, binding, and the
    `t:"challenge"` push shape that resolves "`v` only" versus `c`). Handle
    format, relay JSON API, error codes and retry classes added.
  - §5.1: `push` capability absence cannot tell an old daemon from an
    unconfigured one, so clients show one message for both. Statuses are
    `ok | expired | invalid | relay_error`.
  - §6: the V1 fallback gives seconds in `onMessageReceived`, so extra
    fetching needs WorkManager; `INVALID_ARGUMENT` is not a dead address;
    APNs may keep returning success after a reinstall.
  - §4.2, §8, §12: metadata claims narrowed: self-hosting changes who sees
    the metadata, not whether it exists.
  - §9: test matrix maps each rule to a test name. §10: D9 resolved without
    a new dependency (`ring`). §11: phase order (P3 before freezing P1/P2;
    flood and challenge-abuse tests before P4/P5 deploy; P6 needs a minimal
    registration and fetch; APNs optional until its probes pass). §3.3's
    "P6" now reads P8.

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
  under the old token (PROTOCOL.md lines 70-76). `acd pair` only reads and
  prints the existing client token; it does not rotate it
  (`crates/acd/src/pair.rs:20-28`).
- `config.toml` ignores unknown keys (`crates/acd/src/config.rs:9`), so a
  typo in a new `[push]` section would silently do nothing. §5.2 handles it.
- The lock file already holds `ring` 0.17.14 (the rustls provider). It
  offers ChaCha20-Poly1305, AES-256-GCM, HKDF, HMAC, `SystemRandom` and RSA
  PKCS#1 signing, which the relay needs (§5.3, D9). No XChaCha20-Poly1305
  or AES-GCM crate is in the lock.
- No Android client exists. It is roadmap step 4. Step 3 therefore needs a
  small Android probe app to verify FCM (§11, phase P3).
- iOS v1 is foreground only. The spike lists "no notifications and no
  background refresh" as a risk (iOS spike, line 289).

## 1. Goals and non-goals

**Goals**

1. A phone that is locked or has the app closed learns that some agent
   may need the user, within seconds in the normal case.
2. The relay never sees request content: no agent, machine, project, path,
   command, title, option, answer or count of requests. It does see
   metadata (§4.2).
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
- Request content on a locked phone (P-A, provisional).
- Relay-side revocation of a single handle (P-B, provisional).
- Content in the push payload, encrypted or not. Encrypted content was
  considered and rejected: the push service and relay would still learn
  sizes and timing, and a decryption key on the phone's push path is a
  second secret to manage. The device fetches content from `acd`.
- Reaching `acd` without Tailscale. The fetch after a wake uses the
  existing acd/1 path. A non-Tailscale path is a separate spec. With
  Tailscale off the phone gets the generic alert and cannot fetch or
  answer; the user turns the VPN on and opens the app. That is the
  supported behaviour. A rich background fetch is optional and conditional
  on the tailnet being reachable (§2.4).
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
- the sealing keys for handles (§5.3);
- in-memory state, all with fixed caps: rate-limit counters, pending
  registration challenges (§8).

It has no database and no persistent state other than its config and key
files. A restart forgets counters and pending challenges and keeps every
issued handle valid (handles are sealed, not stored).

The relay speaks plain HTTP on one listener and expects TLS to be terminated
in front of it (a reverse proxy, or a tailnet where the transport is already
encrypted). It binds to loopback by default and refuses a non-loopback
address unless the config says so (§5.7). Behind a proxy the source IP is
the proxy's, so per-IP limits need `trusted_proxy` (§4.6).

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
  privacy notice, and a way to ban abusers. Without relay-side revocation
  (P-B) the ban tool is sealing-key rotation, which revokes everybody. Whether a privacy notice or
  data-protection registration is legally required was not checked.
  Recommendation: no hosted default for 1.0 (D1).

The relay can also listen on a Tailscale address only. Then `acd` reaches
it over the tailnet, and only the relay talks to the internet. The relay
host then sees tailnet addresses instead of the machines' public IPs, and
`acd` keeps its "no internet egress" property (D2). The metadata still
exists; only who can see it changes (§4.2).

### 2.2 Registration (device → relay → device → acd)

1. The app gets a push address from FCM (later APNs).
2. The app asks the relay for a challenge, `POST /v1/handles` (§5.3). The
   relay pushes a one-time code to that address (data-only message
   `{"v":"1","t":"challenge","c":"<code>"}`) and answers `202 {challenge_id}`.
   The relay keeps no address; it keeps only a binding hash (§5.3, §8).
3. The app reads the code from the push, then calls
   `POST /v1/handles/confirm` with the challenge id, the code and the same
   platform, app, address kind and address. The relay checks the code and
   the binding, then returns `{handle, expires_at}`.
4. The app sends the handle to every paired machine whose `welcome`
   advertises the `push` capability, with `push.register` over acd/1
   (§5.1).
5. `acd` stores the registration in `push.json` in the config directory,
   mode 0600, keyed by `client.id`.

The app repeats steps 2-4 when the push address changes, when the handle
is within 7 days of expiry, and after pairing a machine. It does this on
foreground connect; there is no background registration.

The app reads the `c` field of a push only while it has a registration in
flight (started in the last 120 s), only for a message with `t == "challenge"`,
and only echoes it to the relay URL it called. A wake push never has `c`
and the app reads only `v` from it (§2.4).

`acd` never sees the push address. The relay never sees which machine or
agent a handle is for.

### 2.3 Wake (acd → relay → push service)

`acd` sends a wake to each registered device when, and only when:

- a `request.opened` change is published (any kind, answerable or not),
  and
- the registration is not disabled (`expired` or `invalid`) and its
  transient-failure backoff has elapsed (§5.4), and
- the per-device debounce allows it: at most one wake per device per 30 s.
  A request that opens inside the window schedules one trailing wake at
  the window's end. Nothing else is queued.

An open acd/1 session with the device's `client.id` does **not** suppress
the wake. A session does not prove the app is in the foreground (a
backgrounded app can keep a socket for a while, and the daemon cannot tell),
so suppressing on it could swallow the only alert. The app instead
suppresses the notification display itself while it is in the foreground.
The cost is a redundant wake, bounded by the debounce.

Not a trigger in phase 1: an agent becoming `done`, a resolution, a
follow-up. (Open: D7.)

`acd` posts `{v, handle}` to the relay over HTTPS (or over Tailscale, D2).
It uses the response only to update the registration's status (§5.4). It
never reads anything else from the relay, and nothing the relay returns
changes the registry, a request or a token.

### 2.4 Device after a wake

1. The OS shows a notification built from the fixed text in the payload
   (`WAKE_TITLE`, `WAKE_BODY`; P-A). On Android it is a v2
   `RemoteNotification`; on iOS an alert (§6).
2. The app may then run a **bounded fetch round** (limits in §4.6): on
   Android from a WorkManager job (the messaging service's
   `onMessageReceived` gets only seconds on the V1 shape, so it only
   enqueues the job); on iOS from a Notification Service Extension within
   its budget. The round connects to the paired machines over acd/1 exactly
   as the foreground app does: Tailscale address check, SPKI pin, client
   token. It reads the snapshot and closes.
3. **The notification text never gains content** (P-A, provisional). The
   round may only (a) cancel the notification if every reachable machine
   reports nothing pending (best effort; not verified on either platform),
   and (b) refresh the in-app inbox state. It never writes a machine name,
   agent, title or count into a notification. The unlocked inbox shows
   them.
4. If no machine is reachable (Tailscale off, machine asleep) the
   notification stays as it is. That is the supported behaviour. The app
   offers "Open the inbox to check" once it is opened.
5. Tapping opens the inbox root. The inbox answers through the normal
   acd/1 path. There are no notification actions in phase 1.

The payload is never authority for anything. The app ignores every wake
payload field except `v`.

## 3. The wake message

### 3.1 acd → relay

```json
{"v": 1, "handle": "<base64url, 1 to 512 bytes>"}
```

That is the whole body. No machine id, request id, timestamp, count, kind,
priority or text. The relay's request type has exactly these two fields and
refuses any other (`deny_unknown_fields`), so a free-text field cannot be
added by accident; a test pins the field set (§9). `acd` sends it with a
5 s timeout and does not follow redirects.

### 3.2 relay → FCM (HTTP v1, `POST https://fcm.googleapis.com/v1/projects/{project}/messages:send`)

Wake:

```json
{
  "message": {
    "token": "<push address from the opened handle>",
    "data": {"v": "1"},
    "androidV2": {
      "collapseKey": "acd-wake",
      "ttl": "3600s",
      "remoteNotification": {
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

`title` and `body` are the constants `WAKE_TITLE` and `WAKE_BODY` (P-A). The
body has no `mutableContent`: under P-A the app never rewrites the text, so
the field is not needed.

Challenge (§5.3), data only, no notification:

```json
{
  "message": {
    "token": "<push address from the registration request>",
    "data": {"v": "1", "t": "challenge", "c": "<code>"},
    "androidV2": {"ttl": "60s", "backgroundSync": {}}
  }
}
```

(The `backgroundSync` shape is the one the discovery document defines for
non-urgent data. If the P3 probe shows it is delayed past the challenge TTL,
D5 decides the alternative.)

- If the handle holds a Firebase Installation ID instead of a registration
  token, the relay sets `fid` in place of `token`. The discovery document
  (revision 20261023) marks `token` deprecated in favor of `fid`. The
  handle stores the kind (§5.3).
- If the `androidV2` path is not usable (D5), the fallback is the v1 shape:
  `android: {priority: "HIGH", collapseKey: "acd-wake", ttl: "3600s"}` with
  `data: {"v": "1"}` only, and the app posts a local notification itself
  with the fixed text (this also removes the relay's control over the
  visible text, §4.3, at the price of a less reliable background start).

### 3.3 relay → APNs (later, phase P8)

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

The FCM wake body above is under 600 bytes; the APNs body is under 150
bytes.
APNs limits the payload to 4096 bytes (sending-requests page). The FCM
limit was not read from an official page in this session (§6.4).

### 3.5 Deliberately absent

| Absent | Why |
|---|---|
| Machine id, name, address | The relay and push service would learn which machine is active. The device checks every paired machine. (D3) |
| Request id, agent, kind, title, count | Content. |
| A per-wake nonce or sequence | Replay of a wake only makes the device run a bounded fetch round (§4.5, §4.6). A nonce adds state and gives the relay nothing to check. |
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

What it sees per wake: the source IP of each `acd` (the egress IP of the
machine, or the tailnet address if the relay is on the tailnet; or only the
proxy's, behind one), the time, the handle, and after opening it the push
address, platform and app id. Over time it learns: how many devices a person
has, how many machines wake them, when the person's agents ask for input (a
work-hours profile), and bursts (an agent stuck in a loop).

It does not see: content, which machine (beyond its IP), which agent, or
whether the user answered.

Running your own relay or putting it on the tailnet changes **who** can see
this metadata. It does not change **whether it exists**: the push service
(Google or Apple) sees the timing of every wake regardless, and whoever
operates the relay host sees the rest. Debounce caps the rate at one per
30 s per device. Not proposed: random delay or dummy wakes. They cost
latency or battery and a self-hosting owner gains nothing from them. Listed
in §12.

### 4.3 Relay compromise

An attacker who controls the relay (or only the FCM credentials) gets the
push credentials, the sealing keys and in-memory state. They can:

- **Send any notification text to every device registered with this relay
  and to any device whose push address they know, including phishing text
  ("Tap to approve the deploy").** This cannot be prevented by the design:
  the OS renders a notification payload without running app code, so the
  relay (or anyone with the credentials) controls the visible text on the
  lock screen. It is a **residual risk**, not a mitigated one. What limits
  the damage: the app has no notification actions, categories, links,
  images or inherited actions (audited per release, §9); tapping opens the
  inbox root, which shows `acd`'s data only, and answering needs the client
  token over pinned TLS. The app must tell the user once that notification
  text is not authoritative and that the app never asks to approve from a
  notification. The v1 data-only shape (D5(b)) would remove the relay's
  control of the visible text, because the app would post its own fixed
  text; it is not the default because background delivery is less reliable.
- decrypt handles they see and learn push addresses.
- drop or delay wakes (denial of alerts).
- answer `acd` with a false `gone` or `invalid_handle`, so `acd` disables
  the registration (denial of alerts until the device re-registers; visible
  in `acd push list` and `acd status`). A false 5xx only causes backoff.

They cannot: read requests, reach `acd` (the relay never connects to
`acd`; it has no client token, no pin, and no tailnet path unless the
relay host is in the tailnet; see §12 item 3), answer or approve anything.

Recovery: rotate the FCM service-account key and the APNs key in the
Firebase and Apple consoles, rotate the sealing key (every handle becomes
invalid; devices re-register on next foreground), redeploy.

### 4.4 Stolen push address

FCM and APNs require the project's credentials to send, so an address alone
lets nobody send. An address alone cannot be turned into a handle either:
the relay sends the challenge code **to** the address, and the confirm call
needs the code. The relay rate-limits per push address (§4.6), whichever
handle names it. **Documented limit (P-C, provisional):** someone who both
holds the address and can read pushes sent to it is the device. Nothing in
this design binds a handle to a particular acd or phone beyond platform, app
id and address, which are sealed inside it.

### 4.5 Stolen handle; replay

A handle is a capability to wake one device, nothing more. It lives in
`push.json` (0600) on each machine and in the app's secure storage. It is
never returned over acd/1 (`push.list` returns device ids and status only).

A thief can send wakes, limited by the relay's per-handle and per-address
limits. Each wake makes the device run at most one bounded fetch round,
subject to the device-side limits of §4.6. Replay of a captured wake
request (TLS prevents capture on the wire) or of a push has the same
effect. A compromised relay skips its own limits, so the device-side limits
are the ones that count against it.

Handles expire after 30 days (`HANDLE_LIFETIME_SECS`). There is no
revocation of one handle at the relay (P-B, provisional): the accepted
abuse window is 30 days, bounded by the limits below. The device can still
cut a handle off by deleting its push token or installation (new address),
so every old handle points at a dead address. Sealing-key rotation revokes
all handles at once.

### 4.6 Spam and wake flooding

Relay limits. Windows are fixed windows that start at a key's first hit.

| Source | Limit |
|---|---|
| A busy agent in `acd` | 1 wake per device per 30 s, one trailing wake, no queue. |
| `POST /v1/wake`, per handle | 10 per minute, 120 per hour. |
| `POST /v1/wake`, per push address | Same numbers, shared across handles (checked after the handle opens). |
| `POST /v1/wake`, per source IP | 600 per hour. |
| `POST /v1/wake`, global | 6000 per hour. |
| `POST /v1/handles`, per source IP | 10 per hour. |
| `POST /v1/handles`, per push address | 5 per hour. |
| `POST /v1/handles`, global | 600 per hour. |
| `POST /v1/handles/confirm`, per source IP | 60 per hour. |
| Pending challenges | 256 in total, 2 per binding (§5.3). |
| Sends in flight to the push service | 64; more get 503 `busy`. |
| Request size and time | Body at most 2 KiB, handle at most 512 bytes. The body must arrive within 5 s of the routed request. A send to the push service has one 10 s deadline covering the wait for the OAuth token lock, a token refresh and the push exchange (`SEND_DEADLINE`); past it the answer is 503 `unavailable` and the in-flight slot is freed. |
| Connections | At most 1024 open at once (`MAX_CONNECTIONS`); `accept` waits for a slot, so the rest queue in the kernel. A connection must send a complete request head within 10 s of accept and within 10 s of the end of each response (hyper's `header_read_timeout`), so idle, keep-alive and half-sent connections are closed and free their slot. |

**Proxy expectation.** The relay's own limits are a backstop, not the front
line. The supported deployment puts a reverse proxy (or a tailnet listener)
in front that terminates TLS, enforces its own client header and body
timeouts and connection limits, and talks to the relay over loopback. The
connection cap and head deadline above exist so that a relay reached without
such a proxy, or through one that forwards idle connections, still cannot be
made to hold unbounded descriptors and tasks.

Every rate-limit map has a fixed key cap (4096 per map). A full map sweeps
expired entries first. If it is still full, a new key is **not** inserted
and does not evict anyone (evicting would let an attacker reset their own
counters); it is counted against one shared overflow bucket with the same
limits, so a flood of distinct keys cannot grow memory and cannot reset
existing counters. A background sweep also runs every 30 s (`SWEEP_INTERVAL`),
whatever the fill level: it drops counters whose windows have ended and
challenges past their 120 s, and never touches a counter while *either*
its minute or its hour window is still open (the minute window can outlast
the hour window when it starts in the hour's last minute; dropping the
counter then would hand its budget back). It ends with the server (§8). The global limit is the backstop for all of them. Map
keys are HMACs under a per-boot random key, never the IP, handle or address.
IPv6 sources are keyed by their /64. Behind a proxy the source IP is read
from `X-Forwarded-For` only when the TCP peer equals the configured
`trusted_proxy` (default off); the rightmost entry is used. If the header is
absent or malformed from the trusted proxy, the proxy's own address is used,
which collapses the limit into one bucket (conservative).

Over a limit the relay answers `429 {"error":"rate_limited"}` with a
`Retry-After` header and sends nothing. The numbers are starting values for
review, not measured.

**Device-side limits** (the part that holds when the relay is hostile or
the handle is stolen; numbers are starting values, tested in P7, §9.2):

| Limit | Value |
|---|---|
| Fetch cooldown, device-wide | At most one wake-triggered fetch round per 60 s. A wake inside the cooldown is coalesced into one trailing round at its end. |
| Rounds per hour | At most 30. Past that a wake only shows the notification and opens no connection. |
| Concurrent connections in a round | At most 4. Machines are visited in batches. |
| Per-machine deadline | 8 s for connect, hello and snapshot together. |
| Total round deadline | 20 s (inside the iOS extension budget); the round is cut off, not extended. |
| Notification replacement | One fixed tag and id (`acd-wake`). A wake replaces, never adds. |
| Payload fields read | `v` only (and `t`, `c` for a challenge the app itself started, §2.2). |
| Retries | None. A failed round waits for the next wake or the user. |

Apple asks for no more than "two or three per hour" for background pushes
(background updates page). This design uses alert pushes on iOS for that
reason (§6.2).

### 4.7 A malicious relay trying to cause actions

The only data that flows from the relay into the system:

1. the HTTP status and the `error` code (at most 1 KiB, never logged) of a
   wake call, which `acd` maps to a registration status (§5.4), and
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
| `acd pair` | No effect on push. It prints the existing client token and does not rotate it (`pair.rs:20-28`). Only `acd token rotate client` changes the token. |
| `acd tls rotate` | No effect on push. |
| `acd push remove <device>` (new CLI) | Deletes one registration. |
| `push.unregister` | Deletes the caller's own `client.id` registration. |
| FCM address changes (`onNewToken`) | App mints a new handle and re-registers with each reachable machine. Unreachable machines keep the old handle until the relay reports `gone`. |
| Handle expiry | Relay answers `gone`; `acd` marks the registration `expired` and stops sending. |
| Reinstall of the app | FCM gives a new address; the old one may take a while to report unregistered. APNs may keep returning success for a removed app for some time (S6 does not promise otherwise). Wakes can therefore be accepted and go nowhere until the app re-registers; the old registration is replaced by the app's `push.register` under the same `client.id`, or expires after 30 days. |
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
  refused (`bad_request`). `acd` sends to the configured URL only, over
  HTTPS, with redirects off.
- F2a. A malformed or unknown-keyed `[push]` section, an `http://` relay URL
  (except loopback in tests), or a `relay_pin` this build cannot verify,
  turns push off. The daemon still starts, makes no outbound connection and
  advertises no `push` capability (§5.2).
- F3. If `push.json` is missing, malformed, or has loose modes, `acd`
  starts without push, reports `push: degraded` in `acd status`, and does
  not rewrite the file until a valid registration arrives. It does not
  refuse to start: push is optional, agents must not hang (v1 invariant
  "fail open" applies to agents, not to push).
- F4. If the relay TLS certificate does not validate against the trusted
  roots (or, on a tailnet relay, the configured pin), no wake is sent.
- F5. The app never shows text from a machine it cannot verify (pin
  mismatch, `IdentityChanged`); the notification stays fixed (P-A) and the
  inbox marks the machine unverified.
- F6. The relay refuses to start without its keys file (mode 0600), a
  non-empty `allowed_apps`, valid push credentials and a known-keys config;
  it never starts "open" (§5.7).

## 5. acd and acd/1 changes (proposal)

These are proposals for review. PROTOCOL.md is not edited by this spec.

### 5.1 New capability and commands

Capability `push`, advertised in `welcome` only when `[push]` resolved to an
enabled configuration (§5.2).

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
  Result: `{id, ok: true, push: {registered: bool, status: "ok"|"expired"|
  "invalid"|"relay_error", last_wake_at: timestamp|null}}`. This adds an
  optional `push` field to `result`. Old clients ignore it. `expired` and
  `invalid` mean the registration is disabled (§5.4); `relay_error` means
  the last wake failed transiently and wakes continue with backoff.
- All three go through the existing idempotency table and in-flight caps
  (PROTOCOL.md §4). A retried `push.register` with the same id and payload
  returns the cached result.
- `push.*` never returns a handle or another device's state.

*Implemented (acd side, PROTOCOL.md §5.1), differences from the text above:*
`push.status` leaves `status` out when `registered` is false (there is no
state to report). A daemon with push off answers `push.*` with
`not_supported` (a pre-push daemon still answers `bad_request`, so clients
treat both as "push not available"). `relay` is compared with the configured
URL as normalized (a trailing `/` removed), not with the raw config text.
`handle` must be 1 to 512 bytes of base64url, else `bad_request`. A write
failure of `push.json` is `source_unavailable` and changes nothing.

Logs: `client.id`, status and relay HTTP status. Never the handle.

### 5.2 Configuration (acd)

```toml
[push]
relay = "https://relay.example.net"   # absent: push off, no egress
# relay_pin = "sha256:..."            # tailnet relay without a public cert
```

*Implemented:* the key that works today is `ca_file`, not `relay_pin`:

```toml
[push]
relay = "https://relay.example.net"
ca_file = "/etc/ssl/certs/ca-certificates.crt"   # optional
```

`ca_file` is a PEM file; the relay's certificate must chain to a certificate
in it, and nothing else is trusted (the sender has no built-in roots). It
can be a CA bundle or the relay's own certificate, which covers the tailnet
case without a hash verifier. Without it acd uses the first existing of
`/etc/ssl/certs/ca-certificates.crt` and `/etc/ssl/cert.pem`; if none exists
push is off with a reason. A `ca_file` that is not a string, cannot be read or
holds no certificate turns push off, as does `ca_file` without `relay`.
`relay_pin` still turns push off. See `docs/push.md`.

Default: no `[push]` section, push off, no `push` capability, no outbound
connection. The section is read separately from the rest of `config.toml`
so that a mistake in it cannot stop the daemon, and **fails closed**: push
is off, with a reason for `acd status`, when

- the section is not a table, or `relay` is not a string,
- it has any key other than `relay` and `relay_pin` (a typo must not look
  like "configured"),
- `relay` is not an `https://` URL with a host and no userinfo, query or
  fragment (a loopback `http://` URL is accepted only by test builds of the
  sender, never by this resolver), or
- `relay_pin` is set. The pinned verifier is not implemented in the first
  sender (P5a); setting it disables push instead of silently using the
  system roots.

`acd push list` prints `client.id`, platform, status, last wake time.
`acd push remove <client-id>` and `acd push test <client-id>` (sends one
wake) are local CLI commands over `acd.sock`.

*Implemented:* `acd push list` only, and it reads `push.json` directly (it
never prints a handle). `remove` and `test` are not implemented: over
`acd.sock` they need admin commands on acd/1, which is a wider wire change
than §5.1, and the daemon is the only writer of `push.json`. A device removes
itself with `push.unregister`. `acd status` gains a `push:` line (`off`,
`off (reason)`, `on, N device(s)`, `degraded (reason)`) computed from the
config and `push.json`, not from the running daemon.

*Implemented, F3:* with a valid `[push]` and an unsafe or invalid `push.json`
(loose mode, symlink, bad JSON, unknown field, bad handle, more than 8
devices) the daemon starts with no registrations and still advertises `push`,
but registering is refused (`source_unavailable`) while the file as a whole is
unusable: it is never overwritten, so the operator repairs or removes it by
hand. A single invalid entry is skipped, kept in `push.json.quarantine` at the
next write, and the healthy entries are used. Token rotation (§4.8) clears the
registrations, in memory and on disk. The reason is fixed text
(never file content). `push.json` is deliberately not in the start-up mode
preflight, which would make a loose mode fatal.

*Implemented, sender:* the wake is triggered by the engine publishing
`request.opened` (a hook called under the engine lock that takes no
argument), coalesced per device by the 30 s debounce with one trailing wake.
A burst is handled in one pass. At most 4 sends run at once, one per device
(passes do not overlap), each under the sender's 5 s deadline. A wake outcome
is applied only to the registration generation it was sent for, so a device
that registers again mid-send keeps its fresh state. Wake state (status,
failure streak, backoff, last wake) is persisted next to the handle.

### 5.3 Relay API

All bodies are JSON with `Content-Type: application/json`. Errors are
`{"error": "<code>"}` with the status below and nothing else (no message,
no echo of input). Request bodies are limited to 2 KiB (`413 too_large`).
Unknown fields in any request body are refused (`400 bad_request`).

#### 5.3.1 Registration

```text
POST /v1/handles
  {"platform":"android", "app":"com.example.agentcaller",
   "address_kind":"fcm_token", "address":"<push address>"}
→ 202 {"challenge_id":"<22 chars base64url>"}
```

- `platform`: `android` now; `ios` after P8. Anything else, or a platform the
  relay has no sender for: `400 unsupported_platform`.
- `app`: 1 to 64 bytes, must be in the relay's `allowed_apps`, else
  `400 unsupported_app`.
- `address_kind`: `fcm_token` or `fcm_fid` (android), `apns` (ios).
- `address`: 1 to 255 bytes of `[A-Za-z0-9_:.-]`, else `400 bad_request`.
- The relay generates `code` (16 random bytes from the OS RNG, base64url,
  22 characters: 128 bits) and `challenge_id` (16 random bytes), sends the
  challenge push (§3.2), and stores
  `{code_mac, binding_mac, attempts: 0, expires_at}` under `challenge_id`.
  `code_mac = HMAC-SHA256(boot_key, "code" | code)` and
  `binding_mac = HMAC-SHA256(boot_key, "bind" | platform | 0 | app | 0 |
  address_kind | 0 | address)` with a per-boot random `boot_key`. The code,
  address and handle are never stored.
- Challenge lifetime 120 s (`CHALLENGE_TTL_SECS`); lost on restart.
- Limits (§4.6): per IP 10/h, per address 5/h, global 600/h
  (`429 rate_limited`); at most 256 pending challenges and at most 2 per
  `binding_mac`. When 256 are pending, expired ones are swept first; if
  still full the new request gets `503 busy` and **no pending challenge is
  evicted**. An attacker who fills the table delays other registrations for
  at most 120 s per slot; it cannot read or cancel anyone's challenge.
- If the push service says the address is dead (`gone`), the answer is
  `404 gone` and no challenge is stored. Other send failures: `503
  unavailable`, nothing stored.

```text
POST /v1/handles/confirm
  {"challenge_id":"...", "code":"<22 chars>", "platform":"android",
   "app":"com.example.agentcaller", "address_kind":"fcm_token",
   "address":"<push address>"}
→ 200 {"handle":"<base64url>", "expires_at":"2026-12-04T10:00:00.000Z"}
```

The confirm call resends platform, app, address kind and address. The relay
recomputes `binding_mac` and compares it with the stored one; this is what
ties the handle to platform, app and address, and it is why the relay needs
to store no address. Under one lock, so that a challenge is used at most
once even under concurrent calls:

1. Unknown, expired or already used `challenge_id`: `404 unknown_challenge`.
2. Compare the code and the binding in constant time. If either differs,
   `attempts += 1`; at 3 attempts the challenge is deleted. Answer
   `403 challenge_failed` (the same answer for a wrong code and a wrong
   binding).
3. If both match, delete the challenge, then seal and return the handle.
   A second confirm with the same id gets `404 unknown_challenge`.

Confirm is limited per IP (60/h, `429 rate_limited`). The 128-bit code and
the 3-attempt limit make guessing impractical; the attempt limit exists so
that the code's entropy is not the only barrier.

#### 5.3.2 Wake

```text
POST /v1/wake   {"v":1, "handle":"<base64url, 1 to 512 bytes>"}
→ 204                                      (sent to the push service)
→ 400 {"error":"invalid_handle"}           (malformed, tampered, truncated,
                                            unknown version, or the
                                            sealed app is no longer allowed)
→ 400 {"error":"bad_request"}              (bad JSON, unknown field, v != 1)
→ 404 {"error":"gone"}                     (handle expired, key retired,
                                            or the push service says the
                                            address is dead)
→ 413 {"error":"too_large"}
→ 429 {"error":"rate_limited"}  + Retry-After
→ 503 {"error":"unavailable"} | {"error":"busy"}
```

Order of checks, cheapest first and each before anything is sent: size and
JSON shape (`413 too_large`, or `408 timeout` after the 5 s read deadline);
`v == 1`; global limit; per-IP limit; open the handle (`invalid_handle`,
`gone`, and `invalid_handle` if its app is no longer in `allowed_apps`);
per-handle limit and per-address limit (keyed only now, so garbage handles
cannot fill those maps); in-flight cap; send.

#### 5.3.3 Handle format

A handle is `base64url_no_pad(bytes)`, at most 512 characters:

```text
byte 0        version (0x01)
byte 1        kid (key id, u8)
bytes 2..26   salt (24 random bytes)
bytes 26..    ChaCha20-Poly1305 ciphertext of the payload, then a 16-byte tag
```

Payload (all integers big endian):

```text
issued_at   u32  unix seconds
expires_at  u32  unix seconds
platform    u8   1 android, 2 ios
kind        u8   1 fcm_token, 2 fcm_fid, 3 apns
app_len     u8   1..64
app         bytes
addr_len    u16  1..255
address     bytes
```

The longest payload is 4+4+1+1+1+64+2+255 = 332 bytes, so the longest
handle is 26 + 332 + 16 = 374 bytes, 499 base64url characters, inside the
512-byte limit (a test pins this).

Sealing: the AEAD key is `HKDF-SHA256(ikm = sealing_key[kid], salt = salt,
info = "acd-relay handle v1")`, 32 bytes, **different for every handle**
because the salt is 192 random bits. The nonce is therefore a fixed 12 zero
bytes: a key is used for exactly one message, so a nonce collision cannot
happen (the same property XChaCha20 gives, without a new dependency; D9).
The additional data is `version | kid`, so flipping the kid byte fails
authentication.

Opening: version not 0x01, short input, bad base64url or failed
authentication → `invalid_handle`. A kid with no key in the relay's key file
(rotated out) or `expires_at <= now` → `gone`. The two are distinct on
purpose: `acd` disables the registration on both (§5.4), and the code
tells an operator whether a key was rotated or a handle is forged.

Key rotation: the keys file lists keys by `kid`. The newest `kid` seals;
all listed keys open. To rotate, add a new key and keep the old one for
`HANDLE_LIFETIME_SECS` (30 days), then delete it; removing a key makes its
handles `gone`, which is also the all-handles revocation tool (P-B). `kid`
values are not reused.

Handle lifetime is 30 days (`HANDLE_LIFETIME_SECS`; P-B). The app
re-registers when 7 days remain. The handle also expires after the relay
config drops its `app` from `allowed_apps` (`invalid_handle`).

#### 5.3.4 Other routes

`GET /healthz` returns `200 {"ok":true}`, touches no state and is not rate
limited (it reveals nothing). Everything else is `404 {"error":"not_found"}`.

#### 5.3.5 Push service errors

FCM HTTP v1 errors map as follows. The relay never forwards FCM's body.

| FCM | Relay answer |
|---|---|
| 2xx | 204 (wake) or 202 (challenge) |
| `UNREGISTERED` (in `error.status` or an `errorCode` detail) | `404 gone` |
| `INVALID_ARGUMENT`, `NOT_FOUND` | `503 unavailable`. `INVALID_ARGUMENT` may mean a malformed request on the relay's side (a deploy bug) as well as a bad token, so it must not disable a registration. Logged with the FCM status string only. |
| `UNAVAILABLE`, `INTERNAL`, `QUOTA_EXCEEDED`, `DEADLINE_EXCEEDED`, any 5xx, timeout, connect error | `503 unavailable` |
| `UNAUTHENTICATED`, `PERMISSION_DENIED`, `SENDER_ID_MISMATCH`, OAuth token failure | `503 unavailable`; the log line names the class, never the credential or the response body |
| anything else | `503 unavailable` |

### 5.4 How acd reads relay responses (retry classes)

`acd` does not follow redirects, reads at most 1 KiB of any body and logs
none of it. Classes:

| Relay answer | Class | acd |
|---|---|---|
| 204 | success | `status = ok`, update `last_wake_at`, reset the failure streak. |
| 404 `gone` | **invalid registration** | `status = expired`. Disabled: no more wakes until the device re-registers (`push.register`) or `acd push test` succeeds. |
| 400 `invalid_handle` | **invalid registration** | `status = invalid`. Disabled, same recovery. |
| 429 | drop | Drop this wake. No retry, no state change. |
| 5xx, timeout, connect error, TLS error, redirect, a body or status outside this table, 400 `bad_request`, 413, 401, 403 | **transient** | `status = relay_error`. The registration stays enabled. Backoff below. |

Only an invalid registration disables. A relay that is down for a day
costs the alerts of that day (§12 item 16) and recovers by itself.

Backoff: after the n-th consecutive transient failure, wakes to that
registration are skipped (dropped, no queue) until `10 s * 2^(n-1)`
(capped at 900 s) has passed since the failure. The first wake after the
window sends. One success resets the streak. The 30 s debounce is
independent of the backoff. There is no in-wake retry.

`acd` never sends wakes to a registration in `expired` or `invalid` until
the device re-registers or `acd push test` succeeds.

### 5.5 Backwards compatibility

| Pair | Behavior |
|---|---|
| 0.9.0 client, new daemon | Sees an extra `push` capability, ignores it (PROTOCOL.md §2 "Clients ignore unknown"). Never registers. No change. |
| New client, daemon without the `push` capability | Cannot tell a 0.9.0 daemon from a new daemon with push off or misconfigured: capability absence is the same signal. The app shows one message: "Alerts are not available on this machine (push is off or acd needs an update)". It does not send `push.*` (PROTOCOL.md §5). |
| A client that sends `push.register` anyway to 0.9.0 | `bad_request` result (unknown type, `server.rs:342-354`). Session stays open. |

The protocol stays `acd/1`. The JSON schema and `examples/` gain the three
commands and the optional `result.push` field (PROTOCOL.md §7).

### 5.6 What changes in acd/1 (exact list, for review)

Nothing in this PR changes the acd/1 wire contract. P1 adds, and only P1:
the `push` capability string; client messages `push.register`,
`push.unregister`, `push.status` as in §5.1; and the optional `result.push`
object. No existing message, field or error code changes.

### 5.7 Relay configuration and start-up (fail closed)

```toml
listen = "127.0.0.1:8787"          # default; non-loopback needs allow_non_loopback
# allow_non_loopback = false
allowed_apps = ["com.example.agentcaller"]   # required, non-empty
sealing_keys_file = "/etc/acd-relay/sealing-keys.json"   # mode 0600
# trusted_proxy = "127.0.0.1"      # honour X-Forwarded-For only from this peer

[fcm]
project_id = "my-project"
credentials_file = "/etc/acd-relay/fcm-service-account.json"   # mode 0600
# ca_file = "/etc/ssl/certs/ca-certificates.crt"
```

Keys file: `{"keys":[{"kid":1,"key":"<base64url of 32 bytes>"}]}`. The relay
exits non-zero, before listening, when: the config has an unknown key; the
keys or credentials file is missing, unreadable, not mode 0600, or invalid;
`allowed_apps` is empty; `listen` is not loopback and `allow_non_loopback` is
not true; or the CA file cannot be read. The FCM client reads credentials from
the file path only; the key and tokens are never logged and never appear in
an error string. FCM access tokens are cached until 60 s before expiry and
refreshed single-flight under the token lock. A refresh that fails, or is
cut off by the send deadline (§4.6), is remembered for 5 s
(`REFRESH_BACKOFF_SECS`) counted from the moment it failed or was cut off,
not from when it started, so a refresh that stalled until the deadline still
holds off the next one for the full 5 s: sends in that time fail at once with 503
`unavailable` instead of each starting another OAuth request. Start-up also
fails when the credentials file's `token_uri` is empty, not `https`, has no
host or carries userinfo.

The relay speaks plain HTTP and expects the proxy described in §4.6; it
serves at most 1024 connections and closes any that does not send a
complete request head within 10 s.

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
  received on the device" (S1). Under P-A the app never rewrites the text, so
  this design does not set it. How the app would receive control, and within
  what time, is **unverified** (the Firebase client docs were not readable).
- **Time to work after a wake.** On the V1 data-message fallback the app's
  `onMessageReceived` gets only a few seconds, so anything longer than
  enqueueing work (a fetch round over several machines) must run from
  WorkManager. Whether WorkManager starts promptly from a RemoteNotification
  wake on a restricted bucket is a P3/P6 measurement, not assumed.
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
  error codes (`UNREGISTERED`, `INVALID_ARGUMENT`): **unverified**. The relay
  maps only `UNREGISTERED` to `gone` (`NOT_FOUND` can also mean a wrong
  project in the relay's own config, and must not disable registrations). `INVALID_ARGUMENT` is not a dead address: it can also mean the
  relay's own request is malformed, so it maps to `503 unavailable` and never
  disables a registration (§5.3.5).
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
  token" (S6). The relay maps 410 to `gone`. Do not expect the 410 right
  after a reinstall: APNs may keep accepting pushes for a token whose app
  was removed until it learns of it, so a success does not prove delivery.
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
| Relay down | The wake fails transiently. `acd` keeps the registration enabled, shows `push: relay_error`, and skips wakes inside an exponential backoff (10 s doubling, cap 900 s). No alert reaches the phone for wakes that fail or are skipped. | The first wake after the backoff window that reaches a live relay succeeds and resets the streak. No re-registration needed. The foreground inbox is unaffected. |
| Relay up, push service down | Relay answers 503. Same as above. | Same. |
| Relay says `gone` or `invalid_handle` (expired, key retired, forged, or a hostile relay) | `acd` disables that registration (`expired` or `invalid`). | The device re-registers on next foreground connect, or `acd push test` succeeds. |
| Push delayed | Device may alert late. The extension or service fetches the current state, so a late alert shows current content or "Nothing needs you now". | None needed. |
| Push dropped | No alert. | Next request's wake. Best effort; users are told alerts can be missed. |
| Device offline | Push service stores the latest wake per collapse key until TTL (1 h). | Delivered on reconnect if inside TTL. |
| Device online, Tailscale off | The notification keeps its fixed generic text; the fetch round finds no machine and does nothing. This is the supported behaviour (§1). | The user turns Tailscale on and opens the app. |
| `acd` offline (sleep, crash) | No wakes from it. Other machines still wake. A device that wakes for another machine shows this one as unreachable only in the app, not in the notification. | After restart, a request that adapters observe again opens as a new request and wakes again (§12 item 9). |
| `acd` restarts mid-debounce | Debounce state is lost; at most one extra wake. | None. |
| Handle expired, phone not opened for 30 days | `gone`, registration `expired`. | Next foreground connect re-registers. |
| Wakes flood a device (stolen handle, hostile relay) | The device-side limits of §4.6 cap fetch rounds, connections and deadlines. | None needed; the notification is replaced, not stacked. |
| Sealing key lost or retired | All its handles answer `gone`. | Devices re-register on foreground. |
| Relay restart | Counters and pending challenges are forgotten; handles stay valid. A registration in flight fails with `unknown_challenge` and is retried by the app. | None. |
| Clock skew on `acd` | `expires_at` is informational on `acd`; the relay decides expiry. | None. |

## 8. Privacy: what the relay stores

| Data | Where | How long |
|---|---|---|
| Push credentials, sealing keys | Relay config, files mode 0600 | Until rotated. A retired sealing key stays in the file for 30 days so its handles still open, then is deleted. |
| Handles, push addresses | Not stored. Opened in memory per call. | Request duration. |
| Pending challenges `{challenge_id → code_mac, binding_mac, attempts, expires_at}` (MACs under a per-boot key; no code, no address) | Memory, at most 256 | 120 s, or until used or burned. Expired entries are dropped by a sweep every 30 s, not only when the table is full. |
| Rate-limit counters keyed by HMAC(handle), HMAC(address), HMAC(source IP /64 or /32) under a per-boot key | Memory, at most 4096 keys per map plus one overflow bucket each | Fixed windows of 1 minute and 1 hour (there is no day window). Entries whose windows (both) have ended are dropped by a sweep every 30 s, not only when a map is full. Lost on restart. |
| Logs | stdout / journal | Fields: time, route, status, platform, 8 hex chars of HMAC(handle) (per-boot key). Never handles, addresses, codes, IPs, bodies, credentials. A ceiling applied after the `RUST_LOG` filter drops every event above `warn` from the targets `axum`, `hyper`, `hyper_util`, `h2`, `rustls` and `tokio_rustls` and their submodules, and no `RUST_LOG` directive, however specific (`rustls::client::hs=debug`), can raise it; at `trace` `axum::serve` logs every peer address and rustls can log the address of an OAuth endpoint (found by `logs_never_contain_secrets`; the ceiling is tested by `log_ceiling`). The guarantee covers those targets only: the relay's own code logs only the fields above, and a crate not in the list is not capped. Retention is the host's journal setting; recommend 7 days. |

Metadata does exist even though the relay stores almost nothing: while a
request is being handled the process sees the source IP, the handle and the
opened push address, and the platform's own logs, the reverse proxy's access
log, and Google or Apple record what they record. Self-hosting changes who
holds this metadata, not whether it exists (§4.2). What Google and Apple
store is outside this project's control and not checked here.

`acd` stores in `push.json`: `client.id`, platform, handle, `expires_at`,
status, `last_wake_at`, and the backoff streak. Deleted on client token
rotation (§4.8).

The app stores: the handle and the relay URL, in the same secure storage
as the client token.

## 9. Test plan

Each rule below maps to a named test. "This PR" rows exist in the tree and
each fix-type test fails without the behaviour it checks (the revert proofs
are listed in the PR description). "Later" rows belong to the named phase.
Tests of the relay use an in-process fake push service and an in-process
fake clock; none needs a phone or the internet. Test names are the Rust
function names.

### 9.1 Without a phone (CI, Linux and macOS)

**Relay: handles** (`crates/acd-relay/src/handle.rs`, this PR)

| Rule | Test |
|---|---|
| Seal then open returns the sealed fields | `seal_open_round_trip` |
| Same input seals to different handles (fresh salt) | `sealing_twice_gives_different_handles` |
| Tampered ciphertext → `invalid_handle` | `tampered_ciphertext_is_invalid_handle` |
| Truncated → `invalid_handle` | `truncated_handle_is_invalid_handle` |
| Bad base64url → `invalid_handle` | `base64_garbage_is_invalid_handle` |
| Unknown version → `invalid_handle` | `unknown_version_is_invalid_handle` |
| Unknown or retired kid → `gone` | `unknown_kid_is_gone` |
| Expired → `gone` | `expired_handle_is_gone` |
| The kid is authenticated (flipping it to another valid kid fails) | `kid_is_authenticated` |
| Rotation: new key seals, old key still opens | `rotated_in_key_seals_and_old_key_still_opens` |
| Longest address and app id give a handle of at most 512 bytes | `longest_handle_fits_512_bytes` |

**Relay: limits and bounded state** (`src/limits.rs`, this PR)

| Rule | Test |
|---|---|
| A window blocks after the limit and resets after it ends | `window_blocks_after_limit_and_resets` |
| Key flood never grows a map past its cap | `map_never_exceeds_cap_under_key_flood` |
| A full map does not evict (and so does not reset) existing counters | `full_map_does_not_evict_existing_counters` |
| Keys that do not fit share one overflow bucket with the same limits | `overflow_bucket_limits_keys_that_do_not_fit` |

**Relay: challenges** (`src/challenge.rs`, this PR)

| Rule | Test |
|---|---|
| Code is 128 bits (22 base64url characters) and unique | `code_is_128_bits_and_unique` |
| Wrong code counts; the third burns the challenge | `wrong_code_counts_and_third_burns` |
| Atomic single use: concurrent confirms with the right code, exactly one wins | `correct_code_is_single_use_under_concurrency` |
| Expired challenge is unknown | `expired_challenge_is_unknown` |
| Binding to platform, app, kind and address; a mismatch counts as an attempt | `binding_mismatch_is_refused_and_counts` |
| Outstanding cap refuses new challenges and keeps the pending ones | `outstanding_cap_refuses_new_and_keeps_pending` |
| At most 2 per binding | `per_binding_cap` |

**Relay: wire and push bodies** (`src/wire.rs`, `src/fcm.rs`, this PR)

| Rule | Test |
|---|---|
| The wake request type has exactly `v` and `handle`, no free-text field (schema property set) | `wake_request_has_exactly_v_and_handle` |
| An extra `text` field is refused | `wake_request_rejects_extra_text_field` |
| A non-integer `v` never parses | `wake_request_rejects_wrong_version` |
| FCM errors: only `UNREGISTERED` is `gone` | `only_unregistered_maps_to_gone` |
| Golden FCM wake body; fixed title and body are the P-A constants | `fcm_wake_body_is_golden` |
| Golden FCM challenge body | `fcm_challenge_body_is_golden` |
| The FCM bodies carry exactly the listed keys, no handle, no extra data | `fcm_bodies_have_exactly_the_listed_keys` |

**Relay: HTTP API** (`crates/acd-relay/tests/api.rs`, this PR)

| Rule | Test |
|---|---|
| A valid wake sends exactly one push and answers 204 | `wake_sends_one_push_and_returns_204` |
| Registration end to end, then wake | `registration_end_to_end_then_wake` |
| Tampered handle: 400 and nothing sent | `tampered_handle_is_400_and_sends_nothing` |
| Expired handle: 404 `gone` and nothing sent | `expired_handle_is_404_gone_and_sends_nothing` |
| Push says unregistered → 404 `gone` | `push_gone_maps_to_404_gone` |
| `INVALID_ARGUMENT` is not `gone` | `push_invalid_argument_does_not_map_to_gone` |
| Push outage → 503 | `push_unavailable_maps_to_503` |
| `v != 1` and extra fields are 400 | `wake_with_wrong_version_or_extra_field_is_400` |
| Per-handle limit | `wake_rate_limited_per_handle` |
| Per-address limit across two handles of one address | `wake_rate_limited_per_address_across_two_handles` |
| Per-IP limit | `wake_rate_limited_per_ip` |
| Global limit | `wake_rate_limited_globally` |
| Replay of one captured wake is capped by the handle limit | `replayed_wake_is_capped_by_the_handle_limit` |
| Flood of distinct IPs and addresses: maps stay under their caps and the global limit still holds | `flood_of_distinct_ips_and_addresses_stays_bounded` |
| In-flight sends are capped | `concurrent_sends_are_capped` |
| `X-Forwarded-For` only from the trusted proxy | `xff_is_honoured_only_from_the_trusted_proxy` |
| Challenge reuse refused | `challenge_reuse_is_refused` |
| Challenge exhaustion: 503, no eviction | `challenge_exhaustion_returns_503_and_keeps_pending` |
| Confirm with another address is refused | `binding_mismatch_on_confirm_is_refused` |
| Unsupported app or platform: 400 | `unsupported_app_and_platform_are_400` |
| Address charset and length | `address_charset_and_length_are_enforced` |
| Oversized body: 413 | `oversized_body_is_413` |
| Unknown route 404, `/healthz` 200 | `unknown_route_404_and_healthz` |
| No handle, address, code, IP or credential in logs | `logs_never_contain_secrets` |

**Relay: start-up** (`src/config.rs`, this PR)

| Rule | Test |
|---|---|
| Unknown config key refused | `unknown_key_is_refused` |
| Missing, or not 0600, keys file refused | `missing_or_loose_keys_file_is_refused` |
| Empty `allowed_apps` refused | `empty_allowed_apps_is_refused` |
| Non-loopback `listen` needs the opt-in | `non_loopback_listen_needs_opt_in` |
| A valid config loads | `valid_config_loads` |

**Relay: real FCM client** (`crates/acd-relay/tests/fcm_client.rs`, this PR; fake OAuth and FCM endpoints in the test)

| Rule | Test |
|---|---|
| OAuth token first, then send with it | `fcm_client_gets_a_token_then_sends` |
| Token cached until near expiry | `fcm_client_caches_the_token` |
| `UNREGISTERED` → gone | `fcm_client_maps_unregistered_to_gone` |
| No credential in any error string | `fcm_client_errors_never_contain_credentials` |
| Credentials file must be private | `fcm_credentials_file_must_be_private` |
| `token_uri` must be non-empty `https` with a host and no userinfo; the error does not echo it | `token_uri_must_be_a_usable_https_uri` |
| A refusing OAuth endpoint is asked once, again only after the 5 s backoff | `failed_token_refresh_is_not_retried_until_the_backoff_ends` |
| 64 concurrent sends against a stalled OAuth endpoint all end within `SEND_DEADLINE` after one refresh (tokio paused time, with a relay clock that follows tokio time; the per-exchange timeout is set far above it); sends inside the 5 s backoff after the deadline start no refresh, and the first one after it does | `concurrent_sends_against_a_stalled_oauth_endpoint_all_end_by_the_deadline` |

**Relay: connections and sweep** (`crates/acd-relay/tests/connections.rs`, this PR)

| Rule | Test |
|---|---|
| An idle connection is still open one millisecond before the head deadline and closed at it (in-memory pipe, paused tokio time, the relay's own `connection_builder`) | `an_idle_connection_is_closed_at_the_head_deadline` |
| A half-sent request head: same | `an_incomplete_request_head_is_closed_at_the_head_deadline` |
| A keep-alive connection idle after a response: same, measured from the response | `a_keep_alive_connection_that_goes_idle_after_a_response_is_closed` |
| A connection over the cap is served only after a slot frees | `a_connection_over_the_cap_is_served_only_after_a_slot_frees` |
| Expired counters and challenges are swept while the maps are far below capacity; live ones are kept | `expired_entries_are_swept_while_the_maps_are_below_capacity` |
| A sweep never gives a live counter's budget back | `a_sweep_never_resets_a_live_counter` |
| A counter is kept while its minute window is live even after its hour window ended (periodic and full-map sweeps) | `limits::tests::a_sweep_keeps_a_counter_whose_minute_window_outlives_the_hour_window`, `limits::tests::the_full_map_sweep_keeps_a_counter_whose_minute_window_is_live` |
| The sweeper ends with the server | `the_sweeper_ends_with_serve` |

**acd: sender state machine** (`crates/acd-relay/tests/wake_client.rs`, feature `client`, against a fake relay; this PR)

| Rule | Test |
|---|---|
| 204 → `ok`, `last_wake_at` set | `success_marks_ok` |
| `gone` disables until re-registered | `gone_disables_until_reregistered` |
| `invalid_handle` disables | `invalid_handle_disables` |
| 429 drops the wake, no state change | `rate_limited_drops_without_state_change` |
| Transient outage, then recovery without re-registration | `transient_outage_then_recovery` |
| Backoff doubles and caps at 900 s; skipped wakes send nothing | `backoff_doubles_and_caps` |
| Only an invalid registration disables, an outage never does | `outage_never_disables_but_invalid_registration_does` |
| Hostile bodies (huge, invalid UTF-8, `answer`/`approve` fields) change nothing but the status class | `hostile_bodies_change_nothing_but_the_status` |
| Redirects are not followed | `redirects_are_not_followed` |
| Disabled config opens no connection | `disabled_config_opens_no_connection` |

**acd: configuration** (`crates/acd/src/config.rs`, this PR)

| Rule | Test |
|---|---|
| No `[push]` → off | `push_is_off_by_default` |
| Unknown key inside `[push]` → off | `push_unknown_key_turns_push_off` |
| `http://` URL → off | `push_http_url_turns_push_off` |
| `relay_pin` → off until a verifier exists | `push_pin_turns_push_off_until_a_verifier_exists` |
| A malformed `[push]` does not break the rest of the config | `malformed_push_section_does_not_break_the_config` |
| A valid `https://` URL enables | `push_valid_https_url_enables` |

**Later (P1, P2, P5b: not in this PR).** These need the wire types, `push.json`
and the engine hook. Names are the tests to write there:

| Rule | Test |
|---|---|
| Canary: a marker in title, detail, options, cwd, project and agent title is absent from every byte the fake relay receives and from logs at every level | `push_canary_marker_never_reaches_the_relay` |
| Wake on `request.opened` and on nothing else | `wake_only_on_request_opened` |
| An open session does not suppress the wake | `open_session_does_not_suppress_wake` |
| Debounce: 5 requests in 2 s give 1 immediate and 1 trailing wake | `debounce_gives_one_immediate_and_one_trailing_wake` |
| `push.register` with another relay URL: `bad_request`, no connection to that URL | `push_register_with_other_relay_is_refused_without_connecting` |
| `acd token rotate client` deletes registrations | `token_rotation_deletes_push_registrations` |
| Hostile relay answers leave registry and idempotency table unchanged | `hostile_relay_answers_do_not_touch_the_registry` |
| No `[push]`: `welcome` identical to 0.9.0's | `welcome_is_unchanged_without_push` |
| `push.json` malformed or mode 0644: daemon starts, push degraded, file untouched | `loose_push_json_degrades_push_and_is_left_alone` |
| Schema and `examples/` round-trip; a 0.9.0 client fixture ignores `push` | `push_commands_round_trip_and_old_client_ignores_capability` |

### 9.2 Needs Android (emulator with Google Play, then a real phone)

Challenge push and wake delivery, rewrite and kill/doze behaviour, and the
device-side limits of §4.6. Named so that P7 has a target:

| # | Test |
|---|---|
| 1 | Probe app receives the challenge push and the wake (`android_receives_challenge_and_wake`). |
| 2 | The wake notification keeps the fixed text; nothing from `acd` is written into it (`android_notification_text_stays_generic`). |
| 3 | Wake with the app killed; with the app force-stopped (expected: not delivered; record) (`android_wake_after_kill_and_force_stop`). |
| 4 | Doze: `adb shell dumpsys deviceidle force-idle`, then a wake; latency (`android_wake_in_doze`). |
| 5 | Real phone only: overnight idle, battery saver, restricted bucket (`android_overnight_and_restricted_bucket`). |
| 6 | Collapse: wakes from two machines inside 1 s show one notification (`android_wakes_collapse_to_one_notification`). |
| 7 | Token refresh: clear app data, the new address re-registers, the old handle gets `gone` (`android_token_refresh_reregisters`). |
| 8 | Fetch cooldown: 10 wakes in 30 s start one round, plus one trailing round (`android_fetch_cooldown`). |
| 9 | At most 4 concurrent connections and the 8 s and 20 s deadlines hold with unreachable machines (`android_fetch_round_bounds`). |
| 10 | More than 30 wakes in an hour open no further connections (`android_fetch_round_hourly_cap`). |
| 11 | The app ignores every payload field except `v`; a payload with `c` and no pending registration is ignored; no click action, category, link or image is set (`android_payload_is_ignored_except_v`). |

### 9.3 Needs an iPhone and the paid membership (roadmap step 5)

24. Alert push with `mutable-content` reaches the extension.
25. The extension reaches `acd` over Tailscale with the app closed.
26. Keychain sharing between app and extension.
27. Rounds stay inside the 30 s budget; the fallback is the unchanged fixed text.
28. 410 after reinstall (may be delayed); relay maps to `gone`.

### 9.4 Optional, needs a Firebase project and credentials (not CI)

`validateOnly: true` sends against real FCM to check the bodies of §3.2 are
accepted (S1).

## 10. Open decisions

| # | Decision | Options | Recommendation |
|---|---|---|---|
| D1 | Hosted default relay | (a) none for 1.0, owner self-hosts; (b) maintainer runs a public relay for the published app; (c) both, public relay opt-in | **(a)**. 1.0 has one user. (b) is a service with uptime, abuse and privacy duties; decide when there is a published Android or iOS build. |
| D2 | Where `acd` reaches the relay | (a) public HTTPS with system roots; (b) tailnet only, with a pin; (c) either, config decides | **(c)**, docs recommend (b). (b) keeps `acd` free of internet egress and shows the relay host tailnet addresses instead of public IPs. The metadata still exists (§4.2). The pin verifier is not in P5a; until it lands, `relay_pin` disables push (§5.2). |
| D3 | Machine hint in the wake | (a) none, device checks all machines; (b) a random per-registration tag | **(a)**. A handful of machines; no stable pseudonym in the payload. |
| D4 | Proof that the device holds the push address before a handle is issued | (a) challenge push; (b) none, rely on rate limits; (c) Play Integrity / App Attest | **(a)**. One extra push per registration; (c) is heavier and platform-specific. |
| D5 | Android message shape | (a) `androidV2.remoteNotification` with fixed text; (b) v1 `HIGH` data message, app posts its own local notification with fixed text; (c) both, chosen per app version | **(a)** for delivery reliability. Under P-A nothing is rewritten, so the P3 probe now decides only whether (a) is delivered promptly. (b) removes the relay's control of the visible text (§4.3) at the price of a less reliable background start; choose it if phishing text matters more than latency. |
| D6 | Notification actions (Approve/Deny on the lock screen) | (a) none; (b) later, only after the app fetched the request from `acd` and only for non-`confirm` options; (c) never | **(a)** now, revisit as **(b)** in a separate spec. The relay must never supply the action. |
| D7 | Wake triggers | (a) `request.opened` only; (b) also `done`; (c) user-chosen | **(a)** for phase 1. |
| D8 | iOS interruption level | (a) default `active`; (b) `time-sensitive` (needs an entitlement, not checked) | **(a)**. |
| D9 | Handle cipher | (a) XChaCha20-Poly1305 (new dependency); (b) AES-256-GCM, random 96-bit nonce; (c) ChaCha20-Poly1305 from `ring` (already in the lock) with a per-handle key from HKDF over a 192-bit random salt | **Resolved: (c).** No new dependency to audit, and because every handle has its own key the nonce is a constant and a collision cannot occur (the property XChaCha gives). With (b) and one key, 96-bit random nonces would collide with probability about n²/2⁹⁷; at 10⁶ handles that is 2⁻⁵⁸, acceptable, but (c) removes the question. |
| D10 | Relay as a separate binary or inside `acd` | (a) separate `acd-relay` crate and binary; (b) `acd relay` subcommand | **(a)**. Keeps push credentials and the FCM/APNs code out of the daemon. `acd` still gains an HTTPS client to call the relay. |
| D11 | Direct mode: `acd` calls FCM itself, no relay | (a) no; (b) yes, for single-machine owners | **(a)**. Push credentials would sit on every machine. |
| D12 | **PROVISIONAL (P-A).** May a locked phone show request content? | (a) no, fixed generic text; (b) opt-in enrichment | **(a)** built in; owner undecided. |
| D13 | **PROVISIONAL (P-B).** Relay-side revocation of one handle | (a) none, 30-day window plus device limits; (b) a relay deny-list keyed by HMAC(handle) | **(a)** built in; owner undecided. (b) needs persistent state on the relay. |
| D14 | **PROVISIONAL (P-C).** Stolen push address | (a) documented limit; (b) device attestation (Play Integrity, App Attest) | **(a)** built in; owner undecided. |

## 11. Phases (small PRs, Android first)

Each phase is one PR unless noted, with tests per CLAUDE.md.

| Phase | Content | Needs a phone |
|---|---|---|
| P0 | This spec, then the adversarial review and a revision. | No |
| P1 | Protocol: `push` capability, `push.register`/`unregister`/`status` types, `result.push`, schema and examples, PROTOCOL.md section. No behavior. **Freeze after the P3 probe** (the registration contract depends on what the probe shows). | No |
| P2 | acd: `[push]` config, `push.json` store (securefs, 0600), commands, deletion on client token rotation, `acd push list/remove`. Still no outbound calls (a trait `WakeSink` with a no-op). | No |
| P3 | Android probe app (throwaway, like the iOS spike): get FCM address, receive data and `RemoteNotification` pushes, check the fixed text and timing (no rewrite under P-A). Sent by hand with `curl` and an OAuth token. Research note in `docs/research/`. Decides D5. | Emulator, then phone |
| P4 | **Implemented in this PR.** `crates/acd-relay`: handles, challenge, FCM sender, rate limits, logs. Fake FCM in tests. Deployment notes for self-hosting. Includes the flood and challenge-abuse tests; **no deployment before they pass**. | No |
| P5a | **Implemented in this PR** (sender in `acd-relay`, feature `client`). acd: `[push]` config (default off), the sender and its retry and disable rules, against a fake relay. No engine wiring. The HTTP client dependency is reviewed with this PR. | No |
| P5b | acd: wiring (triggers, debounce, `push.json`, `acd push test`), canary tests, the P1 types. **Flood and challenge-abuse tests must pass before this is deployed.** | No |
| P6 | Live Android check end to end: real `acd` on two machines, real relay, probe app; §9.2 tests 1-7. Needs a **minimal registration and fetch implementation** in the probe, not only receiving pushes. Research note. | Phone |
| P7 | Android client registration and post-wake fetch in the step 4 Android app, with the device-side limits of §4.6; §9.2 tests 8-11. | Phone |
| P8 | APNs sender in the relay; iOS extension and keychain sharing. After the membership (roadmap step 5). §9.3. **APNs stays optional until its routing, Keychain and extension probes pass.** | iPhone |

Order matters: P3 runs before P1 and P2 freeze the registration contract.
P1-P2 add no egress; P5a is the first PR where `acd` carries an HTTP client
and P5b is the first where it talks to the internet, so they get their own
review.

## 12. What could go wrong

1. **The notification text is a phishing surface.** Whoever holds the
   push credentials controls the text, and no design choice of ours changes
   that for a notification message. A compromised relay can show "Tap to
   approve deploy" on every device. Only "no actions, tap opens the real
   inbox, tell the user once" limits it (§4.3). D5(b) removes it at a cost.
2. **Delivery may not work as assumed on Android.** If the P3 probe shows
   the RemoteNotification shape is delayed or dropped, D5's fallback (v1
   high-priority data) may be deprioritized by FCM if the app ever fails to
   show a notification (unverified).
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
   person works and how much their agents block, and Google or Apple see
   the timing regardless. Tailnet relay or self-hosting changes who sees it,
   not whether it exists.
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
    registration, while the app is in the foreground. A delayed challenge
    past its 120 s lifetime fails with `unknown_challenge`; the app retries.
16. **An alert during a relay outage is lost.** Retries happen on later
    wakes (§5.4), not for the failed one. A single request that opens during
    an outage and never has a successor sends no alert after recovery. A
    bounded retry timer would fix it; not done, to keep the sender simple.
    Residual.
17. **A hostile relay can disable registrations** with a false `gone` or
    `invalid_handle` (§4.3). Denial of alerts only, visible in `acd status`.
18. **Abuse window.** Without relay-side revocation (P-B) a stolen handle is
    usable for up to 30 days, bounded by the relay and device limits.
19. **Fixed-window limits** allow up to twice the limit across a window
    boundary. Acceptable for the limits' purpose.
20. **Overflow bucket.** A flood of distinct sources makes all unseen sources
    share one bucket, so legitimate new sources get 429 for the flood's
    duration. Existing sources are unaffected.

## 13. Not verified in this session

- Every Firebase documentation page (`firebase.google.com` was blocked):
  data vs notification semantics in the client SDKs, payload size limit,
  deprioritization of high-priority messages, token staleness and refresh,
  error codes. Facts used here come from S1-S3 instead.
- How Android delivers a `mutableContent` notification to the app and how
  long the app has to rewrite it (no longer needed under P-A).
- Whether push requires the paid Apple membership, the iOS extension's
  ability to reach Tailscale and share the keychain, the filtering
  entitlement, the `time-sensitive` entitlement.
- Legal duties of a hosted relay.
- Tauri 2 support for FCM on Android (plugin availability).
- Real FCM and OAuth behaviour of the relay's client: it is tested only
  against in-process fakes of the documented request and response shapes.
