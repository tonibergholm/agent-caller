# Security for users

agent-caller is pre-release software for a personal, trusted set of machines
on one tailnet. The threat model is in [SECURITY.md](../../SECURITY.md). This
page says what that means for you.

## What is at stake

Whoever can answer an agent's request can make that agent run commands. So the
things to protect are the credentials that let a client answer.

## The credentials

| Secret | Where | Protects |
|---|---|---|
| Client token (`client.token`) | `~/.config/acd/`, mode 0600; in every pairing line; in the Keychain, keyring or Android Keystore file on clients | Answering, replying and reading requests over `acd/1`. 32 random bytes, compared in constant time. |
| Pairing line (`acd://...#token`) | Printed by `acd pair`; clipboard | The client token plus the pin. Treat it as the token. |
| Pin (`?pin=sha256:...`) | In the pairing line; derived from `tls.pem` | Tells the client which server certificate to accept. A client refuses any other certificate. The pin is not a secret, but it only helps when you got it from the right machine. |
| TLS key (`tls.pem`) | `~/.config/acd/`, mode 0600 | The daemon's identity. Whoever holds it can pose as that daemon. |
| Hook token (`hook-tokens.json`, and the `X-Acd-Token` header in `~/.claude/settings.json`) | Local machine | The Claude hook endpoint on `127.0.0.1`. |
| T3 token (`t3.token`) | `~/.config/acd/`, mode 0600 | acd's access to your T3 server. It travels over plain `ws://` to `localhost`, loopback or a Tailscale IP. |

`acd` listens only on loopback and its own Tailscale addresses. TCP uses
TLS 1.3. Tokens and keys never go to the logs. `acd.sock` is mode 0600. A
process running as your user can read your files, so it is not a boundary.

## If a token leaks

Client token or pairing line (shared by mistake, in a screenshot, in a chat):

```sh
acd token rotate client
acd pair --host <machine>.<tailnet>.ts.net
```

Running daemons notice within about 2 seconds and close every client that
used the old token. Remove the machine in each client and add the new line.

If the TLS key may be exposed (someone read `~/.config/acd/`), do both:

```sh
acd tls rotate
acd token rotate client
```

then pair every client again. A client that sees a different certificate than
its pin shows "Server identity changed. Re-pair this machine." and does not
connect. Do not re-pair until you know why the certificate changed.

Hook token (for example `settings.json` was committed or shared):

```sh
acd token rotate hook
```

Rotation alone does not revoke the leaked token. The daemon keeps the previous
token valid for Claude sessions that started before the rotation, until they
exit or use the new token, at most 30 days. To cut the leaked token off now,
rotate a second time, which drops it:

```sh
acd token rotate hook --force
```

Restart or `/clear` Claude sessions that still fail with a retired token.
Then remove the leaked file from the repository history. See
[hooks-claude-code-and-codex.md](hooks-claude-code-and-codex.md). `acd hooks
install` refuses to write into a Git-tracked file unless you pass
`--allow-tracked`.

T3 token: revoke it in T3, issue a new one, and save it to
`~/.config/acd/t3.token` with mode 0600.

A lost phone: rotate the client token. The iOS Keychain item is this-device-only
and not backed up, but rotating is the sure fix.

## Habits that help

- Pair only over Tailscale. Use Tailscale ACLs to limit who can reach port
  47800.
- Do not paste a pairing line into chats, tickets or screenshots.
- Use the 2-second undo; read the card's detail. acd shows the whole command
  or says to answer in the terminal.
- Do not put hooks in a file you commit.
- Keep `allowed_origins` empty unless you run a browser client you trust.

## Known limits

- Claude's HTTP hooks cannot authenticate the daemon. While `acd` is not
  running, another local user or process can bind the hook port, read the hook
  token and payloads, and approve Claude permission prompts. Use acd on
  machines where only you log in.
- A same-user process can fill the local pre-auth slots, four source IPs can
  fill the network pool, and one paired client can fill the idempotency
  table. These are open: see the [known gaps](../superpowers/plans/2026-10-07-p1-followups.md).
- A compromised machine, account or tailnet is out of scope.
- If acd is down, the agent's own dialog still works. If acd cannot prove which
  agent and screen an answer belongs to, it does not deliver it.

## The relay (not built)

There is no relay or push today. The iOS and Android apps show requests only
while open. The design in the
[relay spec](../superpowers/specs/2026-11-03-relay-push-design.md) is a draft
reviewed once, not on main. Per that draft:

- The relay would carry a content-free wake-up. It would not see request
  content: no agent, machine, project, path, command, title, option, answer
  or count.
- It could not answer, approve or send anything. Every action still needs the
  client token over pinned TLS to `acd`, which the relay never has.
- It would see metadata: the public IP of each `acd`, the time, the push
  handle, and, once it opens the handle, the push address and platform. Over time that reveals how many devices you have, how many
  machines wake them, and when your agents ask for input. It could drop wakes.
- Push would be off unless you set a relay URL in `config.toml`. You could
  run the relay yourself. Apple or Google would be involved for push delivery
  and would learn push addresses and timing, not content.

Treat this as a plan, not a promise, until it ships.

## Report a problem

Open a GitHub issue titled "Security: contact request" with no details. Do not
post a vulnerability in public. See [SECURITY.md](../../SECURITY.md).
