# Troubleshooting

Every message below is copied from the code. Start with `acd status` on the
machine that runs the agent, and `acd hooks status` if hooks are involved.

How to read a machine's state: the panel, the settings list and the dot
tooltip show `<kind>` or `<kind>: <reason>`. Kinds that are normal are
`online`, `connecting` and `syncing`. The others raise a banner.

## Machine states

Reconnect timing: after a drop the client waits 1 s, then 2, 4, 8, 16, and
30 s at most, doubling each time. Reaching `online` resets it. **Retry** in
settings reconnects at once. A connect attempt gives up after 8 s; an idle
connection is pinged after 15 s and dropped after 10 s more without reply.

| State and text | Cause | Fix |
|---|---|---|
| `offline: acd is not running on this machine` | The local `acd.sock` is missing or refuses. | `acd service install`, or `acd run`. |
| `offline: connection refused. Is acd running on that machine?` | The remote host answers but nothing listens on the port. | Start `acd` there; check `port` in its `config.toml`. |
| `offline: cannot reach that machine. Is Tailscale on?` | No route. | Start Tailscale on both machines. |
| `offline: connection timed out` | No answer in 8 s. | Same; check the firewall and that the host is online. |
| `offline: handshake timed out`, `write timed out`, `no reply to ping`, `closed`, `dropped by request` | The link broke or the peer stalled. | Wait for the retry. If it repeats, check the network and `acd` logs. |
| `offline: cannot resolve <host>` | The name does not resolve. | Use the machine's Tailscale name or IP. |
| `offline: <host> resolves to [...]; none is a Tailscale or loopback address` | The client refuses non-Tailscale addresses. | Re-pair with `acd pair --host <tailscale name or IP>`. |
| `offline: HTTP error: 503 Service Unavailable` | Pre-auth refusal, see below. | Wait, close extra clients. |
| `auth failed: The token was rejected. Re-pair this machine.` | The client token changed (`acd token rotate client`, close code 4401) or the line was wrong. | Run `acd pair`, remove the machine, add the new line. |
| `auth failed: acd rejected client.token; re-reading it` | The local app's token file changed. | Wait: it re-reads the file. |
| `identity changed: Server identity changed. Re-pair this machine.` | The certificate does not match the pin (`acd tls rotate`, a reinstall, or another host at that address). Retried every 30 s, never trusted. | If you did it, `acd pair` and re-add. If not, do not re-pair: see [security-for-users.md](security-for-users.md). |
| `incompatible: unsupported protocol` (or `unsupported protocol <x>`) | Close code 4400: client and `acd` do not share `acd/1`. | Update the older side. Press Retry afterwards. |
| `local unavailable: <reason>` | The local socket or token is unusable (next table). | Fix the file named. |
| `local unavailable: the local acd.sock endpoint is unavailable on this platform; pair a remote machine instead` | Windows and Android have no local endpoint. | Pair a remote machine. |

### Local file errors (`local unavailable`)

| Text | Fix |
|---|---|
| `acd is not set up on this Mac (<path> is missing)` | `acd init`. |
| `<path> is a symlink; refusing it`, `... is not a regular file or directory as expected`, `... is not owned by you` | Replace it with a real file you own. |
| `<path> has mode <m>, which other users can read. Fix with: chmod <want> <path>` | Run the chmod it prints (0700 for the directory, 0600 for `client.token`). |
| `<path> does not hold a valid token` | `acd token rotate client`. |

## Close codes and 4408

| Code | Meaning | What the client does |
|---|---|---|
| 4400 | Unsupported protocol. | Shows `incompatible`, waits for Retry. |
| 4401 | No valid `hello`, invalid token, or `token rotated`. | Shows `auth failed`. A paired remote waits for Retry; the local app retries with backoff. |
| 4408 | `client too slow`: the client queued over 1000 events. | Shows `connecting`, reconnects with the backoff above, resumes where it left off, or gets a fresh snapshot if it fell too far behind. If it repeats, the client or link is too slow. |
| anything else | Normal close. | `offline: closed`, then backoff. |

## Pre-auth refusals (HTTP 503)

The daemon limits connections that have not authenticated yet: 32 in total,
2 per source IP, 8 past the upgrade on the network pool (the last 2 slots only
for a source that holds none), and 8 on the local socket. A third connection
from one IP is closed before TLS and looks like a reset or EOF. A full pool
answers the upgrade with HTTP 503. `hello` must arrive within 5 s and the
whole login within 10 s.

Fix: wait a few seconds; close extra tabs, apps or scripts that connect from
the same machine. A stuck client from the same IP holds a slot until it times
out. A request that carries a browser `Origin` header gets HTTP 403 unless
the origin is in `allowed_origins` (empty by default).

## Pairing lines

| Text | Cause |
|---|---|
| `That is not an acd pairing line. Run \`acd pair\` on the machine and copy its output.` | The text is not `acd://<host>:<port>?pin=sha256:<43 chars>#<token>`: bad scheme, host, port, pin or token. Copy the whole line. |
| `The clipboard does not hold an acd pairing line.` | The clipboard is empty. |
| `That address is not a Tailscale or loopback address. Use the machine's Tailscale name or address.` | The host in the line is not on the tailnet. Use `acd pair --host`. |
| `Pair this machine: run \`acd pair\` there, copy the line, then Add from clipboard` | The machine is listed but has no saved secret. |
| `stored credentials unreadable` / `The stored credentials for this machine cannot be read. Remove it and add it again to re-pair.` | The Keychain item is damaged. Remove and re-add. |
| `Could not read machine credentials`, `Could not save machine credentials`, `Could not read saved machine credentials` | The Keychain or keyring refused. Unlock it and retry. |
| `acd: could not find a Tailscale host; pass --host` | `acd pair` could not ask Tailscale. Pass `--host`. |
| `acd: pass --host with a non-empty host` | `--host ""`. |
| `acd: invalid pairing host` | The host is not a valid name or IP. |
| `acd: <dir> not initialized; run \`acd init\`` | Run `acd init`. |

## Answer notices on a card

| Notice | Meaning |
|---|---|
| Answered elsewhere | Someone answered first (terminal, another client). Nothing to do. |
| Answered in the terminal | You answered in the terminal. |
| Check the terminal | acd cannot tell the outcome (uncertain, lost). Look at the agent. |
| A stray keystroke may be in the input box | A key was sent and may have landed in the agent's input. Check and clear it. |
| The source rejected the answer | The producer refused it. |
| Offline. Nothing was sent. | The machine is not `online`. |
| The request changed. Nothing was sent. | The request was updated; review it and answer again. |
| Not ready yet. Enlarge the pane to answer here. | The dialog is not confirmed on screen yet. Enlarge the herdr pane, wait. |
| The terminal shows a different dialog. Nothing was sent. | The screen check failed (`screen_mismatch`). Answer in the terminal. |
| Another answer is in progress | Another claim is in flight. |
| Answer in terminal / Answer in T3 / Answer in <producer> | The card is read-only (no-answer state). See [herdr-and-t3.md](herdr-and-t3.md). |
| The agent is gone | The agent exited. |
| The agent is busy. Try again when it is idle. | A reply to an agent that is working. |
| The source is unavailable. Nothing was sent. | The adapter (hooks, herdr, T3) is disconnected. Check `acd status`. |
| Not supported for this agent | The action is not offered for this source. |
| Hidden. Nothing was sent. | The agent is hidden by a filter. |
| Could not send: <reason>. | Any other daemon error (`bad_request`, `duplicate_mismatch`, `unauthorized`). |

Daemon text does not appear in these notices; only the code does. One exception: an external producer's own message (at most 500 bytes) replaces the generic notice on a resolved card.

## Source states (`acd status`)

| Source | State and message | Fix |
|---|---|---|
| herdr | `unconfigured` "no herdr socket found" | Start herdr, or set `herdr.extra_sockets`. |
| herdr | `degraded` `<socket>: <reason>` | Check that herdr is 0.9.3 or newer (below 0.10). |
| T3 | `unconfigured` "set t3.url in config.toml", token errors | See the table in [herdr-and-t3.md](herdr-and-t3.md). |
| hooks | no agents appear | `acd hooks status`; start new agent sessions. |

If Tailscale is missing, the daemon logs `Tailscale CLI not found; Tailscale
addresses unavailable` and listens on loopback only.

## Hooks

| Text | Fix |
|---|---|
| `<path> is tracked by Git, and the hook token would be committed. Untrack it, or pass --allow-tracked` | Untrack the file. |
| `cannot tell whether <path> is tracked by Git (...). Fix Git, or pass --allow-unknown-git` | Fix Git, or pass the flag. |
| `hook token rotation incomplete: fix the file(s) and run \`acd hooks install claude\`` | Fix the file named, then reinstall. |
| An agent shows its own dialog and nothing in the inbox | By design when acd is down, slow or undecided. Check `acd status`. |

## Omarchy

| Text | Fix |
|---|---|
| Agent inbox unreachable (bar icon) | `acd-inbox status`. |
| The inbox service is not running. Start it with: systemctl --user start acd-inbox | Run it. |
| The inbox service did not understand that. Update the plugin or the service. | Run `acd-inbox install` again, then `omarchy-restart-shell`. |
| The keyring is locked. Unlock it and try again. / The keyring could not be used. / The keyring is not responding. Press Retry on the machine once it answers. / Keyring state unknown. Retry | Unlock the Secret Service keyring (press **Unlock keyring**), then **Retry**. |
| This machine is not paired. Paste its pairing line. | Add the line in settings. |
| Answer this one in the terminal. / Too long to show here. Answer it in the terminal. / Scroll the whole card into view to answer it, or answer in the terminal. | Read-only or partly hidden card. |
| That agent is gone. / The agent is hidden now. Nothing was sent. / The request changed. Your answer was not sent. / The request changed. Review it again. / That was already sent. | Re-read the card; nothing was delivered. |
| Type something first. / That is too long to send. / The text has characters that cannot be sent. | Fix the text (answers: 16 KiB; replies: 64 KiB). |
| Names are 1 to 64 characters. / Could not save the settings. | Fix the name; retry. |
| `another acd-inbox is running (<path> is locked)` | One service per user. |
| `<path>: the runtime directory must be a directory you own, mode 0700, and not a symlink` | Fix `XDG_RUNTIME_DIR`. |
| `XDG_RUNTIME_DIR is not set; acd-inbox runs inside a user session` | Run it from a user session. |
| `<unit> exists and was not written by acd-inbox; remove it first` | Move the file, run install again. |
| `omarchy plugin enable ... failed` | Run `omarchy plugin enable tonibergholm.agentcaller` by hand. |

## Android

| Text | Fix |
|---|---|
| The saved pairing cannot be read here. Remove this machine and pair it again. | Remove and re-pair. |
| The phone's secure key was lost and has been replaced. These machines must be paired again: <names>. | Pair them again. |
| The phone's secure storage could not be reset. Try again later. | Retry. |

## Mac voice

Shown on the HUD or in settings: "Microphone access is off for Agent Caller."
and "Speech Recognition is off for Agent Caller." (System Settings → Privacy &
Security); "The hold key needs Input Monitoring." (grant it, or use the toggle
shortcut); "Quit and reopen Agent Caller to use the hold key."; "The English
speech model is not installed. Download it in Settings."; "Recording stopped
after 60 seconds. Nothing was sent."; "Apple Intelligence is off. Simple
commands still work." and its variants (free-form phrases need macOS 26 and
Apple Intelligence; "approve", "deny", "yes always", "option N", "cancel",
"next", "previous" always work); "Not sure which agent. Nothing was sent."
(say the agent or machine name); "That agent is filtered out. Say its
machine."; "Could not register Control+Alt+M: in use. The previous shortcut
stays active." (pick another shortcut).

Verified: the strings are from `crates/acd-client` (`conn.rs`, `view.rs`,
`inbox.rs`, `local.rs`), `crates/acd-protocol` (`pairing.rs`, `messages.rs`,
`PROTOCOL.md`), `crates/acd-core/src/server.rs`, `clients/omarchy/model.mjs`,
`crates/acd-inbox`, `apps/android/src`, `app/src`; `acd pair`, `acd token
rotate`, `acd tls rotate` and `acd hooks` output was run;
Not verified: no
state was provoked against a running daemon (`acd run` cannot bind IPv6 in
this sandbox), so the fixes are reasoned from the code, not tried; also not verified: the exact `HTTP error: 503`
wording (it is the WebSocket library's text for the 503 that tests assert),
and the macOS voice and Omarchy strings in a running app.
