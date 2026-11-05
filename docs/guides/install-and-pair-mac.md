# Install and pair on a Mac

You end up with the `acd` daemon running at login, the Agent Caller menu bar
app showing its requests, and (optionally) a remote machine paired.

You need macOS 13 or newer, Rust 1.90 or newer, Node.js 24.15 or newer and
[`just`](https://github.com/casey/just). Voice needs macOS 26.

## 1. Install the daemon

Either use a release (see the README, "Install a release") or build from
source:

```sh
git clone https://github.com/tonibergholm/agent-caller.git
cd agent-caller
cargo install --path crates/acd --locked
```

Then set it up:

```sh
acd init                # creates ~/.config/acd/
acd service install     # launchd agent, starts at login
acd status              # sources, agents, open requests
```

`acd init` prints the machine id and the client token once. The files are
`client.token`, `hook-tokens.json`, `machine-id`, `config.toml` and `tls.pem`
in `~/.config/acd/`, all mode 0600 in a 0700 directory. Set `ACD_CONFIG_DIR`
to use another directory.

`acd service install --print` shows the launchd file without writing it. To
run in the foreground instead, use `acd run`. The daemon reads its config at
start: after you edit `config.toml`, run `acd service install` again.

## 2. Install the app

From the repository root:

```sh
just app-install
```

This builds the app, replaces `/Applications/Agent Caller.app` and opens it.
The build is ad-hoc signed unless `APPLE_SIGNING_IDENTITY` is set, so macOS
may ask you to confirm the first launch ([release.md](../release.md)) and
may ask for Keychain access again after a rebuild.

The app lives in the menu bar and adds the local machine by itself. It talks
to the daemon over `~/.config/acd/acd.sock` and reads `client.token` from the
same directory.

Press **Control+Option+Space** to open the panel. Pick an option with `1`
to `9`. The app waits 2 seconds before it sends, and `Esc` cancels. The
README lists all keys.

Notifications: the app asks for permission at launch. If none appear, enable
Agent Caller in System Settings → Notifications.

## 3. Connect the agents

Do [hooks-claude-code-and-codex.md](hooks-claude-code-and-codex.md) for Claude
Code and Codex. herdr and T3 Code: [herdr-and-t3.md](herdr-and-t3.md).

## 4. Pair another machine (optional)

See [pair-a-remote-machine.md](pair-a-remote-machine.md). In the app, open
settings and click **Add from clipboard**. The app keeps the remote token in
the Keychain.

## 5. Voice (optional)

Voice is off. Turn on **Voice control** in the app's settings. The app asks
for Microphone, Speech Recognition and Input Monitoring (the last one only
for the hold key).

- Hold **Right Option**, speak, release. Or press **Control+Option+V** to
  start and again to stop. Both can be changed in settings.
- Say "approve", "deny", "yes always", "option 2", "cancel", "next" or
  "previous". These need no model.
- Other phrases need Apple Intelligence (macOS 26). If it is off, settings
  and the HUD say so and the fixed phrases still work.
- Only English. A recording stops at 60 seconds.
- A spoken answer uses the same path as a typed one, with the 2-second undo.
  Some answers (for example "yes always") are confirmed on the HUD first.

If the hold key does nothing, check System Settings → Privacy & Security →
Input Monitoring. The toggle shortcut does not need it.

Problems: [troubleshooting.md](troubleshooting.md).
