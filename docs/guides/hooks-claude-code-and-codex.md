# Claude Code and Codex hooks

Hooks are how Claude Code and Codex tell `acd` about permission requests and
questions. Install them once per machine, then start new agent sessions.

## Install

```sh
acd hooks install claude
acd hooks install codex
acd hooks status
```

`acd hooks status` lists the files acd changed and any interrupted command
(`journal: none` when all is well). Running Claude sessions keep the hook
config they started with: restart them or run `/clear`.

## What each install writes

Both agents get command hooks that run the hook shim,
`acd hook <agent> <event> --config-dir <dir>`. The shim reads the payload on
stdin, sends it over the owner-only socket `<config dir>/hook.sock` (never
TCP), and prints the reply. It reads the hook token from the config dir
itself, so no token is written to the agent's settings. Before it sends
anything it checks that the config dir and every directory above it, and the
socket, belong to you and cannot be swapped by another user. On any failure
it prints nothing and exits 0, so the agent shows its own dialog; it never
answers a prompt by itself.

**Claude Code** (`~/.claude/settings.json`, or `$CLAUDE_CONFIG_DIR`): hooks for
`UserPromptSubmit`, `PermissionRequest`, `PreToolUse`, `PostToolUse`,
`PostToolUseFailure`, `Stop` and `SessionEnd`. The shim passes the herdr pane,
the herdr socket and Claude's entrypoint from its environment, so acd can
match the agent to a pane. The `PermissionRequest` hook is held up to
`claude.permission_hold` seconds (3600 by default) so you can answer from the
inbox, and the shim waits as long. Its hook timeout in `settings.json` is
`permission_hold` plus 10 seconds, so run `acd hooks install claude` again
after you change `claude.permission_hold`. The other events have a 1 s
timeout, and the shim gives up after 750 ms.

**Codex** (`~/.codex/hooks.json`, or `$CODEX_HOME`): hooks for `SessionStart`,
`UserPromptSubmit`, `PermissionRequest`, `PostToolUse` and `Stop`. The shim's
budget is 800 ms for `PermissionRequest` and 750 ms for other events. Codex
may ask you to review the hooks the first time it runs them.

If acd is down, slow or undecided, the agent's own dialog still works.

## Upgrading from the HTTP hooks

Earlier versions wrote Claude hooks of type `http`, which posted to
`http://127.0.0.1:<hook_port>/hook/claude/<event>` with an `X-Acd-Token`
header. That put the hook token in plain text in `settings.json`. While acd
was not running, another local user could take the port, read the token and
the payloads, and approve Claude permission prompts.

Run `acd hooks install claude` once. It replaces those entries (the ones marked
`acd-managed:v1`) with the shim commands, removes the token from the file
and makes the file private (mode 0600). `acd hooks status` and `acd status`
warn while a recorded Claude settings file still holds the HTTP hook, and say
if that file is readable by group or others. Until you reinstall, the old
entries keep working: the daemon still listens on `hook_port` for them.
Sessions that started before the reinstall keep the HTTP hooks they read, so
restart them or run `/clear`.

## Trust

- The hook token lives in `hook-tokens.json` in the config dir (mode 0600).
  The shim sends it only over `hook.sock`.
- A Claude settings file written by an older version holds the token in the
  `X-Acd-Token` header. Treat that file as a credential until you reinstall.
  If it is tracked by Git, install refuses ("is tracked by Git, and the
  hook token would be committed. Untrack it, or pass --allow-tracked"). If
  Git cannot say, use `--allow-unknown-git`.
- The hook token is different from the client token (`client.token`, used
  by apps). Rotate them separately.

## Rotate the hook token

```sh
acd token rotate hook
```

The shim reads the current token each time it runs, so rotation needs no
change to the settings of a shim install. It still rewrites the token in any
recorded Claude settings file that holds the old HTTP hook, and prints
`Updated <path>` for each recorded file. The previous token stays valid until every Claude session started
before the rotation has ended or used the new one, at most 30 days. If a file
cannot be updated, the command fails with `hook token rotation incomplete`:
fix the file and run `acd hooks install claude`. If the token may have
leaked, add `--revoke`: the old token stops working at once, and Claude
sessions that still use it lose acd until restarted. Do not change
`hook_port` while a settings file still holds the HTTP hook.

## Remove

```sh
acd hooks uninstall claude
acd hooks uninstall codex
```

This removes only the entries acd manages (marked `acd-managed:v1`) and
leaves your other hooks alone. If acd created the file, the file is removed:
"Removed <path> (acd created it)".

Problems: [troubleshooting.md](troubleshooting.md).
