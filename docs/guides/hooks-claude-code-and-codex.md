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

**Claude Code** (`~/.claude/settings.json`, or `$CLAUDE_CONFIG_DIR`): HTTP
hooks for `UserPromptSubmit`, `PermissionRequest`, `PreToolUse`,
`PostToolUse`, `PostToolUseFailure`, `Stop` and `SessionEnd`. Each posts to
`http://127.0.0.1:<hook_port>/hook/claude/<event>?marker=acd-managed:v1`
with an `X-Acd-Token` header. `hook_port` comes from `config.toml`. The
`PermissionRequest` hook is held up to `claude.permission_hold` seconds
(3600 by default) so you can answer from the inbox. The hook also sends the
herdr pane and socket so acd can match the agent to a pane.

**Codex** (`~/.codex/hooks.json`, or `$CODEX_HOME`): command hooks that run
`acd hook codex <event> --config-dir <dir>`. This is the hook shim. It reads
the payload on stdin, sends it over the owner-only socket
`<config dir>/hook.sock` (never TCP), and prints the reply. Its budget is
800 ms for `PermissionRequest` and 750 ms for other events. On any failure it
prints nothing and exits 0, so Codex shows its own dialog. Codex may ask you
to review the hooks the first time it runs them.

If acd is down, slow or undecided, the agent's own dialog still works.

## Trust

- The Claude hook token sits in plain text in `settings.json`, in the
  `X-Acd-Token` header. The file is the hook's credential. Do not commit it:
  if the file is tracked by Git, install refuses ("is tracked by Git, and the
  hook token would be committed. Untrack it, or pass --allow-tracked"). If
  Git cannot say, use `--allow-unknown-git`.
- Claude's HTTP hooks cannot authenticate the daemon. While acd is not
  running, another local user or process can take the hook port, read the
  token and payloads, and approve Claude permission prompts. `acd` prints
  this when you install. Use it on personal, trusted machines only.
- The hook token is different from the client token (`client.token`, used
  by apps). Rotate them separately.

## Rotate the hook token

```sh
acd token rotate hook
```

It rewrites the token in the Claude settings files acd manages and prints
`Updated <path>` for each. The previous token stays valid until every
Claude session started before the rotation has ended or used the new one,
at most 30 days. If a file cannot be updated, the command fails with `hook
token rotation incomplete`: fix the file and run `acd hooks install claude`.
Do not change `hook_port` after installing hooks.

## Remove

```sh
acd hooks uninstall claude
acd hooks uninstall codex
```

This removes only the entries acd manages (marked `acd-managed:v1`) and
leaves your other hooks alone. If acd created the file, the file is removed:
"Removed <path> (acd created it)".

Problems: [troubleshooting.md](troubleshooting.md).

Verified: `acd hooks install claude`, `install codex`, `status` and
`uninstall` for both, in a scratch `HOME`: the event list, the header, the
codex command, the printed notices and the file removal above. Not
verified: a real Claude Code or Codex session, the 3600-second hold, the
Codex review prompt, `--allow-tracked` in a real Git repository, and `acd
token rotate hook` against live Claude sessions.
