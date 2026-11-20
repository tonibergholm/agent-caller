# Security

agent-caller is pre-release software (0.9.0) for a personal, trusted set of
machines connected by Tailscale.

## Threat model

- **Protected:** the ability to answer an agent's permission request or send
  it text. Whoever has it can make an agent run commands.
- **Network:** `acd` binds to loopback and its own Tailscale addresses only.
  TCP uses TLS 1.3 with a pinned certificate. The client token (32 random
  bytes) is compared in constant time. Tokens and keys sit in owner-only files
  (0600, directory 0700) and are never logged.
- **Local:** `acd.sock` is mode 0600. A same-user process can read the token,
  so it is not a boundary.
- **Fail closed:** if `acd` cannot prove which agent and screen an answer
  belongs to, it does not deliver it. A permission command that an adapter cut
  is read-only. If `acd` is down, the agent's own dialog still works.
- **Hooks:** Claude and Codex hooks run the `acd hook` shim, which talks to
  `hook.sock` only after it checks the socket's owner, mode and parent
  directories. No hook token is written to agent settings files. Older
  installs used Claude HTTP hooks on a loopback port; `acd status`
  warns about them until `acd hooks install claude` replaces them, and acd
  keeps that port bound for Claude sessions started before the reinstall.
  `acd token rotate hook --revoke` ends the old hook token at once.
- **T3:** before acd sends the T3 token to a T3 server on this machine
  (loopback or one of its own addresses), it checks that the server end of
  that connection belongs to the same user (`/proc/net/tcp` on Linux,
  `lsof` on macOS) and refuses otherwise.
- **Answers and text:** an answer must carry the request revision the client
  saw, and follow-up text must carry the agent incarnation it was written
  for. acd refuses follow-up text while the agent's screen shows a dialog.
  Clients clean peer text of control and bidi characters, and treat an allow
  option whose label does not read as an approval as unanswerable.
- **Hostile peers:** frames, agent state, text and in-flight commands are
  bounded. Unauthenticated connections are capped (32 total, 2 per source IP,
  8 per pool), and every loopback address counts as one source. Browser
  origins are refused by default.
- **Push (optional):** with `[push] relay` set, acd sends the relay a
  sealed device handle each time a request opens. The relay learns the
  daemon's IP address and the timing of requests, never request text,
  tokens or agent names, and can stop a device's wakes. Registrations are
  bound to the client token and are dropped when it rotates. No first-party
  client registers for push yet. See [docs/push.md](docs/push.md).
- **Releases:** release archives carry a GitHub build provenance
  attestation. `scripts/install.sh` verifies it when `gh` is installed and
  signed in, and says so when it could not.
- **Known limits:** five network sources can still fill the pre-auth pool,
  and a same-user process can fill the local pre-auth slots. A T3 server run
  as another user or in a container on loopback is refused. The T3 token goes
  over plain `ws://` to a Tailscale address, and 100.64.0.0/10 is also
  carrier-grade NAT, so check `t3.url`. See the
  [known gaps](docs/superpowers/plans/2026-10-07-p1-followups.md).
- **Out of scope:** a compromised machine, account or tailnet. Windows and
  Android are type-checked only.

Reviews: [core](docs/research/2026-11-03-security-review-core.md),
[local](docs/research/2026-11-03-security-review-local.md),
[red team](docs/research/2026-11-04-red-team-acd.md).

## Report a problem

Open a GitHub issue titled "Security: contact request" with no details, and
the maintainer will arrange a private channel. Do not post the vulnerability
in public. GitHub private vulnerability reporting is off for this repository.
