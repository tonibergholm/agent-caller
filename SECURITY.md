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
- **Hostile peers:** frames, agent state, text and in-flight commands are
  bounded. Unauthenticated connections are capped (32 total, 2 per source IP,
  8 per pool). Browser origins are refused by default.
- **Known limits:** Claude's HTTP hooks cannot authenticate the daemon (spec
  §9, R7). The T3 token goes to whoever holds the T3 port. A same-user process
  can fill the local pre-auth slots, and four source IPs the network pool. The
  last two are open [owner decisions](docs/superpowers/plans/2026-10-07-p1-followups.md).
- **Out of scope:** a compromised machine, account or tailnet. Windows and
  Android are type-checked only. There is no relay or push yet.

Reviews: [core](docs/research/2026-11-03-security-review-core.md),
[local](docs/research/2026-11-03-security-review-local.md).

## Report a problem

Open a GitHub issue titled "Security: contact request" with no details, and
the maintainer will arrange a private channel. Do not post the vulnerability
in public. GitHub private vulnerability reporting is off for this repository.
