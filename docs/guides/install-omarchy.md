# Install on Omarchy (Linux)

You end up with `acd` and the `acd-inbox` user service running, a bar icon
and panel in the Omarchy shell, and desktop notifications for new requests.

You need Omarchy 4 on x86_64 Linux and Rust 1.90 or newer. Tailscale is
needed only for remote machines.

## 1. Install `acd`

Use a release archive (the Linux archives contain `acd` and `acd-inbox`;
`scripts/install.sh` puts them in `~/.local/bin`), or build:

```sh
git clone https://github.com/tonibergholm/agent-caller.git
cd agent-caller
cargo install --path crates/acd --locked
acd init
acd service install      # systemd user unit: ~/.config/systemd/user/acd.service
```

To keep the daemon running after you log out: `loginctl enable-linger $USER`.
Stop it with `systemctl --user disable --now acd.service`.

## 2. Install the inbox

On the machine itself:

```sh
acd-inbox install
```

It installs and starts the user service, installs the shell plugin
(`~/.config/omarchy/plugins/tonibergholm.agentcaller`) and runs
`omarchy plugin enable tonibergholm.agentcaller`. It does not restart the
shell. Run this once so the bar icon appears:

```sh
omarchy-restart-shell
```

From a Mac with Docker you can build and install on a host in one step:
`just inbox-install you@omarchy`.

Check it:

```sh
acd-inbox status     # service, socket, keyring, plugin
```

`acd-inbox uninstall` removes the service and the plugin. It removes the
binary only when it also found the unit it installed. It leaves your
keybinding and prints the line to delete.

## 3. Keybinding (optional)

The installer prints this line and adds it to `~/.config/hypr/bindings.lua`
only if you say yes (and only when it has a terminal):

```lua
o.bind("SUPER + ALT + A", "Agent inbox", "omarchy-shell tonibergholm.agentcaller toggle")
```

## 4. Answer a card

Click the bar icon to open the panel. Cards are grouped under **Needs you**
and **Working**.

- `J`/`K` or Down/Up move between cards. `1` to `9` pick an option and
  start the 2-second countdown; **Esc to undo**. Some options need a
  confirm: press the digit, then Enter.
- `T` focuses the typed answer field of a question that allows text, `R`
  opens a reply to an idle or finished agent, `E` marks the agent seen. Enter
  sends in a text field. Esc closes the panel.
- A card that cannot be answered here says so ("Answer this one in the
  terminal.", or "Scroll the whole card into view to answer it, or answer in
  the terminal."). Answer those in the agent's terminal.
- Settings in the panel: **Show idle agents**, **Show subagents**,
  **Show hook-only agents**, **Notifications**.

New requests raise a desktop notification while the panel is closed.
Turn them off with the **Notifications** toggle.

## 5. Add the Claude Code and Codex hooks

[hooks-claude-code-and-codex.md](hooks-claude-code-and-codex.md). For remote
machines, paste the `acd pair` line into the panel's settings
([pair-a-remote-machine.md](pair-a-remote-machine.md)). Remote tokens go to
the Secret Service keyring; if the keyring is locked the panel shows
"Unlock keyring".

Problems: [troubleshooting.md](troubleshooting.md).

Verified: `cargo build -p acd-inbox`; `acd-inbox --help`; `acd-inbox status`
with no service (prints `service: not running`, `plugin: missing`);
`acd service install --print`. Panel text comes from
`clients/omarchy/model.mjs` and `Panel.qml`;
Not verified: `acd-inbox
install` and `uninstall` (they need `systemctl --user` and `omarchy`), the
shell plugin, notifications, the keybinding.
