# Live end-to-end harness (`just e2e`)

Real Claude Code, Codex and a scratch T3 server against a scratch `acd`, driven through a scratch herdr
session. Spec §13 lists the checks; §14 the acceptance criteria. Results of the first run:
`docs/research/2026-10-11-e2e-run.md`.

Run it only from an interactive session: it launches real agents (haiku, gpt-5.6-terra) that make model calls.
Needs `just`, `tmux`, `jq`, `python3` (stdlib only), `herdr` 0.9.x, `claude`, `codex`, `t3`.

```
just e2e                         # build, up, all scenarios, down
just e2e-up                      # bring the scratch environment up
just e2e-one 40-claude-live      # one scenario
just e2e-down
```

## What is isolated

Everything lives under `/tmp/ace2e`. Nothing touches the user's acd (launchd job, `~/.config/acd`, ports 47800
and 47801), default herdr session or desktop T3 (port 3773, `~/.t3`). Ports used: hook 52955, client 52956, T3 52957.

- **acd:** fresh `acd init` in `/tmp/ace2e/acd`, moved ports, T3 off (on only for `50-t3.sh`, pointing at the
  scratch T3). Run with `HOME=/tmp/ace2e/home` and `HERDR_SOCKET_PATH` set to the scratch herdr socket.
- **herdr:** session `ace2e` in its own tmux server (`tmux -L ace2e`), started with `HOME=/tmp/ace2e/home`.
  The scratch socket must NOT sit under the real `~/.config/herdr/sessions`: the user's acd scans that directory
  and would adopt the scratch panes. No `agent.focus` is ever called.
- **Claude:** hooks come from `acd hooks install claude` into a scratch `CLAUDE_CONFIG_DIR`, loaded with
  `--settings`. The real `~/.claude` is still needed for login, and the user's own settings (with http hooks to the
  user's acd on 47801) still load: `--setting-sources project,local` and `allowedHttpHookUrls` do NOT exclude them
  (see the run doc). So the Claude process runs under `sandbox-exec` with `(deny network-outbound (remote tcp
  "*:47801"))` (and 47800). The user's hooks then fail with `ECONNREFUSED 127.0.0.1:47801`, visible on screen.
- **Codex:** the user's `~/.codex/hooks.json` entries for acd are untrusted, so Codex does not run them (check
  with `codex_hooks.py <cwd>`, which uses `codex app-server` `hooks/list`; it runs no hook and starts no session).
  The scratch project's hooks need trust, which `-c` cannot grant, so `codex-trust.sh add` appends project-trust and
  hook-trust entries for `/private/tmp/ace2e/proj` to `~/.codex/config.toml` after a backup, and `down.sh`
  (or `codex-trust.sh restore`) puts the file back and checks the sha256. Choose "Continue without trusting" at
  the Codex review prompt: it covers only the user's untrusted hooks, the scratch ones stay active.
- **T3:** `t3 serve --base-dir /tmp/ace2e/t3home` on 127.0.0.1:52957, `T3CODE_HOME` set for every CLI call, token
  scoped to `orchestration:read` and `orchestration:operate`. The server cannot run under `sandbox-exec`
  (`spawn EPERM`), so only its Claude provider is: `settings.json` points `providerInstances.claudeAgent.binaryPath`
  at a wrapper that runs claude under the same deny profile. T3's Codex provider is disabled.
- Tokens are never printed; `inbox.py` logs `<redacted>` for the hello token. `down.sh` deletes the scratch config
  dirs, T3 home and the `t3.token` copy.

Side effects that remain: Claude writes a trust entry for `/private/tmp/ace2e/proj` into `~/.claude.json`,
transcripts under `~/.claude/projects/`, and a registry entry in `~/.claude/sessions/` while a session runs (the
user's acd reads that registry, so a running test session counts as a live session for token retirement).
Codex writes rollouts under `~/.codex/sessions/`.

## Files

| File | Purpose |
|---|---|
| `up.sh`, `down.sh`, `lib.sh` | environment up/down and shared helpers |
| `inbox.py` | acd/1 client: `log`, `list`, `agents`, `wait-open`, `answer`, `race`, `send` (idempotency key, repeat) |
| `hookpost.py` | synthetic Claude http hook client (not a live Claude) |
| `t3drive.py` | scratch-T3 driver: launch a thread, count messages |
| `codex_hooks.py`, `codex-trust.sh` | Codex hook trust inspection and the reversible trust entries |
| `scenarios/10-hooks-roundtrip.sh` | install x2 then uninstall x1, `jq -S` compare, foreign PermissionRequest hook kept |
| `scenarios/20-synthetic-daemon.sh` | acd down, stalled, hold, late answer, race, expiry, restart at the hook boundary (synthetic) |
| `scenarios/30-codex-live.sh`, `31-codex-resilience.sh` | live Codex: Yes/No, terminal first, race, follow-up x3 with one key; acd down, stalled, restart |
| `scenarios/40-claude-live.sh`, `41-claude-resilience.sh` | live Claude: same, plus question, coexisting hook, Esc; hold expiry |
| `scenarios/50-t3.sh` | T3 approval, single-select question, late answer, follow-up exactly once |

Scenarios append `PASS`/`FAIL` lines to `/tmp/ace2e/results/<scenario>.txt`; `/tmp/ace2e/inbox.jsonl` holds every
acd/1 message with timestamps and `/tmp/ace2e/acd.log` the debug log.

## Manual / not covered

- Spec §14.1 (remote Tailscale machine) and anything needing the Mac app panel (P5 checklist).
- §14.10 blocked third-agent screen: herdr shows `opencode` and `pi` as `other` with their state, but no provider
  is configured to make them block. `herdr pane report-agent` did not change the state of a shell pane.
- `just e2e` as one chain has not been run end to end; the scenarios were run one by one.
- Panes are driven by sending keys; timings use 100 ms polling of the screen, so they carry that error plus
  model latency (seconds).
