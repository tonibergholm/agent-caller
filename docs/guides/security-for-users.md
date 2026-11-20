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
| Hook token (`hook-tokens.json`; older installs also put it in the `X-Acd-Token` header in `~/.claude/settings.json`) | `~/.config/acd/`, mode 0600 | The hook endpoint. The `acd hook` shim reads it and sends it over `hook.sock`. |
| T3 token (`t3.token`) | `~/.config/acd/`, mode 0600 | acd's access to your T3 server. It travels over plain `ws://` to `localhost`, loopback or a Tailscale IP. For a T3 server on this machine, acd first checks that the server runs as your user. |

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

Hook token (for example `settings.json` was committed or shared). First stop
tracking the file: `acd token rotate hook` and `acd hooks install` both refuse
to touch a Git-tracked settings file, and the refusal happens before the token
changes, so rotating first revokes nothing.

```sh
git rm --cached <path-to-settings.json>
acd token rotate hook --revoke
```

Without `--revoke`, a rotation does not revoke the leaked token: the daemon
keeps the previous token valid for Claude sessions that started before the
rotation, until they exit or use the new token, at most 30 days. `--revoke`
keeps no previous token, so the leaked one stops working at once. Claude
sessions that still use it lose acd until you restart them (or `/clear` them).
If the rotation reports that it could not update every settings file, fix that
and run `acd hooks install claude`. The running daemon picks up the change on
its next refresh, normally within about two seconds. Then remove the leaked
file from the repository history. See
[hooks-claude-code-and-codex.md](hooks-claude-code-and-codex.md).

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

- Older installs used Claude HTTP hooks, which cannot authenticate the
  daemon: while `acd` is not running, another local user can bind the hook
  port, read the hook token and approve Claude permission prompts. `acd
  status` warns while such entries remain. Run `acd hooks install claude` to
  replace them with the shim, restart your Claude sessions, and run `acd
  token rotate hook --revoke` if the settings file was readable by others.
- A same-user process can fill the local pre-auth slots, and five network
  source IPs can fill the network pool. These are open: see the
  [known gaps](../superpowers/plans/2026-10-07-p1-followups.md).
- A compromised machine, account or tailnet is out of scope.
- If acd is down, the agent's own dialog still works. If acd cannot prove which
  agent and screen an answer belongs to, it does not deliver it.

## The relay (optional, off by default)

`acd` can wake devices through a push relay, but only when you set
`[push] relay` in `config.toml`. The relay server (`crates/acd-relay`) is
built from source; releases do not ship it. No first-party client registers
for push yet, so the iOS and Android apps still show requests only while
open. See [docs/push.md](../push.md) and the
[relay spec](../superpowers/specs/2026-11-03-relay-push-design.md).

- A wake carries a sealed device handle and a version number. The relay never
  sees request content: no agent, machine, project, path, command, title,
  option, answer or count.
- It cannot answer, approve or send anything. Every action still needs the
  client token over pinned TLS to `acd`, which the relay never has.
- It sees metadata: the public IP of each `acd`, the time, the push handle,
  and, once it opens the handle, the push address and platform. Over time
  that reveals how many devices you have, how many machines wake them, and
  when your agents ask for input. It can drop wakes, and an answer of
  `invalid_handle` or `gone` stops a device's wakes until it registers again.
- Registrations are bound to the client token. Rotating it drops them, also
  across a restart.
- Apple or Google deliver the push and learn push addresses and timing, not
  content.

## Report a problem

Open a GitHub issue titled "Security: contact request" with no details. Do not
post a vulnerability in public. See [SECURITY.md](../../SECURITY.md).
