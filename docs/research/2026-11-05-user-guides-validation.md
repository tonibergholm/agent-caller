# User guides validation, 2026-11-05

What was checked for `docs/guides/` (PR #112) and what was not. Sandbox: Linux, no IPv6 (`acd run` cannot bind), no Mac, Xcode, NDK, phone or Tailscale.

## herdr-and-t3.md

Verified: the table of what is answerable and the status messages are read
from `PROTOCOL.md` (section 6), the design spec (section 6.2) and the
adapter source (`t3/reconciler.rs`, `t3/conn.rs`, `herdr/discovery.rs`); the
card labels from `app/src/components/Card.svelte` and
`crates/acd-client/src/view.rs`;
Not verified: any live herdr or T3 session;
the recorded live checks are in `docs/research/` (herdr gate 6, T3 gate 3).

## hooks-claude-code-and-codex.md

Verified: `acd hooks install claude`, `install codex`, `status` and
`uninstall` for both, in a scratch `HOME`: the event list, the header, the
codex command, the printed notices and the file removal above;
Not
verified: a real Claude Code or Codex session, the 3600-second hold, the
Codex review prompt, `--allow-tracked` in a real Git repository, and `acd
token rotate hook` against live Claude sessions.

## install-and-pair-mac.md

Verified: `cargo build -p acd`; `acd init`, `acd service install --print`
(systemd form), `acd service install --help`, and every file name and mode
above, on Linux. The command names, keys and voice text come from the README
and the app source;
Not verified: `acd service install` as a launchd agent,
`just app-install`, the app, notifications and voice (no Mac in this
session); `acd run` and `acd status` (the sandbox has no IPv6 and the daemon
refuses to bind).

## ios-and-android.md

Verified: the scope and limits, from `apps/ios/README.md`, `docs/android.md`
and `README.md`;
Not verified: every build and install command above (no
Mac, Xcode, NDK or phone in this session), the Android app on any device,
and `adb install`. The iOS README records a debug build checked on one
iPhone on 2026-10-13; a release build was not.

## pair-a-remote-machine.md

Verified: `acd init`; `acd pair --host mini.tail1234.ts.net` (the line has
the form shown); `acd pair` with no Tailscale CLI (error text above);
`acd token rotate client`, `acd tls rotate` (output text above). Client
messages are copied from `crates/acd-client` source;
Not verified: a real
pairing over Tailscale, the 30-second identity retry and the 4401 close
against a running daemon (the sandbox cannot bind IPv6, so `acd run` exits;
the behaviour is from `conn.rs`, `daemon.rs` and `server.rs`, which have
their own tests), `acd pair` host discovery with a real Tailscale CLI.

## security-for-users.md

Verified: file names and modes (`acd init`), `acd token rotate client`,
`acd token rotate hook`, `acd tls rotate`, `acd hooks install claude` (the
token in `settings.json`, the tracked-file refusal text), `acd pair`. The
rest is from `SECURITY.md`, `PROTOCOL.md` and the relay spec;
Not verified:
rotation against live clients (the daemon cannot bind IPv6 in this sandbox),
Tailscale ACL advice (general practice, not tested), and anything about the
relay, which does not exist in this tree.

## troubleshooting.md

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

## install-omarchy.md

Verified: `cargo build -p acd-inbox`; `acd-inbox --help`; `acd-inbox status`
with no service (prints `service: not running`, `plugin: missing`);
`acd service install --print`. Panel text comes from
`clients/omarchy/model.mjs` and `Panel.qml`;
Not verified: `acd-inbox
install` and `uninstall` (they need `systemctl --user` and `omarchy`), the
shell plugin, notifications, the keybinding.

