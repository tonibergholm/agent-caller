# agent-caller

[![CI](https://github.com/tonibergholm/agent-caller/actions/workflows/ci.yml/badge.svg)](https://github.com/tonibergholm/agent-caller/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

An inbox for coding agents. When Claude Code asks for a permission, Codex
waits on a question, or an agent in herdr or T3 Code finishes, the request
shows up in one place, on whichever machine the agent runs. You answer it
there, with a 2-second undo, without hunting for the terminal.

agent-caller has two parts:

- **`acd`**, a Rust daemon that runs on every machine with agents. It
  collects requests from Claude Code and Codex hooks, from herdr, and from
  T3 Code, and serves them over a documented WebSocket protocol, `acd/1`.
- **Agent Caller**, a macOS menu bar app built with Tauri. It connects to
  the local daemon and to remote daemons over Tailscale.

The app is the reference client. Any other client (a CLI, a phone app)
can speak `acd/1` to the same daemon. The protocol is in
[`crates/acd-protocol/PROTOCOL.md`](crates/acd-protocol/PROTOCOL.md).

> **Status: pre-release (0.1.0).** There are no binary releases yet, and
> the Mac app is unsigned. You build both from source. Expect breaking
> changes to the config and protocol before 1.0.

## What it does

- Answers permission prompts and single-question, single-select
  questions. Other question shapes are shown read-only.
- Sends free-text follow-ups to idle or finished agents.
- Offers "Yes, always" for Claude `Bash` permissions, showing the exact
  rule it will save.
- Shows any other agent that herdr detects, with its state and a screen
  preview when it is blocked. These agents are read-only.
- Reaches remote machines over Tailscale, with a pinned TLS certificate
  per machine.

| Source | Agents | How acd answers |
|---|---|---|
| Claude Code hooks | Claude Code | Replies to the held `PermissionRequest` hook |
| Codex hooks | Codex | Sends keys through herdr when the agent runs in herdr |
| herdr | Claude Code, Codex, others read-only | Sends one option key after checking the dialog on screen |
| T3 Code | T3 threads | T3 RPC commands |

## How it stays safe

The daemon sits between you and agents that can run commands, so a few
rules are fixed:

- **Fail open.** If acd is down, slow or undecided, the agent's own
  terminal dialog still works. acd never makes an agent hang.
- **Fail closed on delivery.** If acd cannot prove which agent and which
  dialog an answer belongs to, it does not deliver it. Before it sends a
  key through herdr, it re-reads the screen and compares a fingerprint of
  the dialog.
- **No free-form input.** acd sends a single option key for a matching
  dialog, or a follow-up that herdr accepts only when the agent is not
  blocked. Nothing else.
- **First answer wins.** Answering in the terminal and in the app at the
  same time is safe. The late answer gets "Answered elsewhere".
- **Narrow network exposure.** acd listens only on loopback and on its
  own Tailscale addresses. TCP connections use TLS 1.3 with a pinned
  certificate. Tokens are 32 random bytes in owner-only files and never
  reach the logs.

Known limit: Claude's HTTP hooks cannot authenticate the daemon, so
another local user who binds the hook port while acd is stopped could
answer a Claude permission. v1 targets a personal, trusted machine. See
spec §9, "Hook endpoint trust", for details.

## Requirements

- Rust 1.90 or newer.
- macOS or Linux for the daemon. Service install uses launchd on macOS and
  a systemd user unit on Linux.
- macOS and Node.js for the app.
- [Tailscale](https://tailscale.com) for remote machines.
- Optional sources: [herdr](https://github.com/herdrdev/herdr)
  0.9.3 or newer (below 0.10), and T3 Code.

## Install the daemon

On each machine that runs agents:

```sh
git clone https://github.com/tonibergholm/agent-caller.git
cd agent-caller
cargo install --path crates/acd --locked

acd init                # config, tokens, TLS key in ~/.config/acd/
acd service install     # start at login (launchd or systemd --user)
acd status              # sources, agents and open requests
```

Use `acd run` instead of `acd service install` to run it in the
foreground. `acd service install --print` shows the service file without
writing it.

### Connect the sources

**Claude Code and Codex.** Install the hooks once per machine:

```sh
acd hooks install claude
acd hooks install codex
acd hooks status
```

Start new agent sessions afterwards. Running Claude sessions keep the
hook config they started with. `acd hooks uninstall claude|codex` removes
exactly the entries acd recorded at install and leaves your other hooks
alone.

**herdr.** No setup. acd finds `~/.config/herdr/herdr.sock`,
`$HERDR_SOCKET_PATH` and session sockets under
`~/.config/herdr/sessions/`. Add other sockets with `herdr.extra_sockets`
in the config.

**T3 Code.** Issue a token and set the server URL:

```sh
(umask 077; t3 auth session issue \
  --scope orchestration:read --scope orchestration:operate \
  > ~/.config/acd/t3.token)
```

Then set `t3.url` in `~/.config/acd/config.toml`. The URL must be a
loopback or Tailscale address, because the token travels over plain
`ws://`.

## Install the app (macOS)

```sh
cd app
npm ci
npm run tauri build -- --bundles app
open src-tauri/target/release/bundle/macos
```

Move `Agent Caller.app` to `/Applications` and open it. It lives in the
menu bar and adds the local machine on its own.

To add a remote machine, run this on that machine and copy the line it
prints:

```sh
acd pair --host <machine>.<tailnet>.ts.net
```

Then click **Add from clipboard** in the app's settings. The line carries the client token and the certificate pin. The
app stores tokens in the Keychain. After `acd tls rotate`, pair again.

### Using the app

The menu bar icon shows the most urgent state (amber: needs you, blue:
working, green: done) and how many agents need you. Press
**Control+Option+Space** for the panel. Focus stays in the app you were
using.

| Key | Action |
|---|---|
| `J` / `K` | Move between cards |
| `1`–`9` | Pick an option |
| `R` | Reply to an idle or finished agent |
| `E` | Mark the agent seen |
| `Enter` | Confirm |
| `Esc` | Undo during the countdown, or close the panel |

Every answer waits 2 seconds before acd sends it. `Esc` or a click in that
window cancels it.

## Configuration

`~/.config/acd/config.toml`. Set `ACD_CONFIG_DIR` to use another
directory.

| Key | Meaning | Default |
|---|---|---|
| `machine_name` | Name shown in the app | host name |
| `port` | `acd/1` WebSocket port | 47800 |
| `listen` | Addresses to bind: loopback and the machine's own Tailscale addresses only | `["loopback", "tailscale"]` |
| `hook_port` | Claude hook listener on `127.0.0.1`. Do not change it after installing hooks. | random, chosen by `acd init` |
| `allowed_origins` | Browser origins allowed to connect | none |
| `claude.permission_hold` | Seconds to hold a Claude permission hook | 3600 |
| `ignored_entrypoints` | Claude entry points to ignore | `["sdk-ts"]` |
| `herdr.enabled`, `herdr.extra_sockets` | herdr source | enabled |
| `t3.enabled`, `t3.url` | T3 source. Needs `t3.url` and `t3.token`. | enabled, no URL |

Other commands:

- `acd token rotate client` replaces the client token. Pair the apps again.
- `acd token rotate hook` replaces the hook token. Running Claude
  sessions keep working with the old one until it is retired.
- `acd tls rotate` replaces the TLS certificate.

## Repository layout

| Path | Contents |
|---|---|
| `crates/acd-protocol` | `acd/1` wire types, JSON schema, pairing strings, `PROTOCOL.md` |
| `crates/acd-core` | Request engine, agent registry, server, TLS, idempotency |
| `crates/acd-adapters` | Sources: `hooks/` (Claude Code, Codex), `herdr/`, `t3/` |
| `crates/acd-client` | Client library used by the app |
| `crates/acd-securefs` | Owner-only file access |
| `crates/acd` | The `acd` binary: daemon, CLI, hook shim, service install |
| `app/` | Svelte frontend and Tauri backend (`app/src-tauri`) |
| `tests/` | Recorded fixtures and the live end-to-end harness |
| `docs/` | Design spec, plans, research and live check records |

The design spec,
[`docs/superpowers/specs/2026-10-07-agent-inbox-design.md`](docs/superpowers/specs/2026-10-07-agent-inbox-design.md),
is authoritative. Known gaps and deferred work are in
[`docs/superpowers/plans/2026-10-07-p1-followups.md`](docs/superpowers/plans/2026-10-07-p1-followups.md).

## Development

```sh
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings

cd app && npm ci && npm run verify   # macOS only
```

CI runs the workspace on Ubuntu and macOS and the app on macOS.

`just e2e` runs real Claude Code, Codex and T3 agents against a scratch
daemon under `/tmp/ace2e`. It makes model calls, so run it only from an
interactive session. Read [`tests/e2e/README.md`](tests/e2e/README.md)
first.

Contributor rules are in [`CLAUDE.md`](CLAUDE.md). The short version:
every behavior change needs a test, a fix needs a test that fails without
it, and the workspace is not run through `cargo fmt` yet.

## Not in v1

Voice input, a phone client and push notifications, a self-hosted relay,
starting new agents, transcript browsing, multi-select answers, and
desktop clients for Windows and Linux. The protocol advertises
`capabilities` so these can arrive without breaking clients.

## License

[MIT](LICENSE)
