# Security

agent-caller is pre-release software (0.9.0). It is meant for a personal,
trusted set of machines connected by Tailscale.

## Threat model

- **What is protected.** The ability to answer an agent's permission request
  or send it text. Whoever can do that can make an agent run commands.
- **Network.** `acd` binds only to loopback and to its own Tailscale
  addresses. TCP uses TLS 1.3 with a pinned certificate, so a paired client
  talks only to the daemon it paired with. The client token (32 random bytes)
  is checked in constant time. Tokens and keys sit in owner-only files
  (mode 0600 in a 0700 directory) and are never logged.
- **Local tools.** The Unix socket `acd.sock` is mode 0600. A process of the
  same user can already read the token, so same-user processes are not a
  boundary.
- **Fail closed.** If `acd` cannot prove which agent and screen an answer
  belongs to, it does not deliver it. A permission command that an adapter had to
  cut is read-only. If `acd` is down, the agent's own terminal dialog works.
- **Hostile peers.** Frames, agent state, text and in-flight commands are
  bounded. Unauthenticated connections are capped in total (32), per source
  IP (2) and per pool (8). Browser origins are refused by default.
- **Known limits.**
  - Claude's HTTP hooks cannot authenticate the daemon. A local user who
    binds the hook port while `acd` is stopped could answer a Claude
    permission (spec §9, ruling R7).
  - The T3 token travels over plain `ws://` and HTTP to the T3 port. Whoever
    holds that port receives it (open, owner decision).
  - A same-user process can hold the local pre-auth slots, and four or more
    source IPs can fill the network pool (open, owner decisions).
  - Several items need a device or a platform that has not been run: see the
    [known gaps](docs/superpowers/plans/2026-10-07-p1-followups.md).
- **Out of scope.** A compromised machine or account, a compromised Tailscale
  tailnet, and Windows and Android (type-checked only). There is no relay or
  push yet.

Reviews: [core](docs/research/2026-11-03-security-review-core.md) and
[local](docs/research/2026-11-03-security-review-local.md).

## Report a problem

Use GitHub's private vulnerability reporting: Security tab of the repository,
"Report a vulnerability". Do not open a public issue for a vulnerability.
Include the version, the steps and what an attacker gains. This is a one-person
project, so replies can take time.
