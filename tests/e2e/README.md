# Live end-to-end harness (`just e2e`)

Real Claude Code, Codex and a scratch T3 server against a scratch `acd`, driven through a scratch herdr
session. Spec §13 lists the checks; §14 the acceptance criteria. Results of the first run:
`docs/research/2026-10-11-e2e-run.md`.

Run it only from an interactive session: it launches real agents that make model calls. Models: Claude
scenarios run `claude --model haiku`; Codex scenarios `codex -m gpt-5.6-terra` (reasoning effort low);
`t3drive.py` launches T3 threads on `claude-sonnet-5-5` (instance `claudeAgent`, `--model` overrides).

Prerequisites: `just`, `jq` >= 1.6, `curl`, `shasum`, `sandbox-exec` (macOS), `python3` (stdlib only), `herdr` 0.9.x
(its headless server; no tmux), `claude`, `codex`, `t3`, and a built `acd` (`cargo build --release -p acd --locked`).

## Preconditions

- **Your own acd hooks in `~/.codex/hooks.json` must be untrusted (or disabled).** Codex isolation from your real
  acd rests on that: the scratch project's hooks are trusted, yours are not, so Codex runs only the scratch ones.
  Check with `python3 tests/e2e/codex_hooks.py /tmp/ace2e/proj` (read-only; starts no session, runs no hook) and
  look for `untrusted`/`modified` on every `acd hook codex` row outside `project`. Your other hooks (own
  scripts, plugins) may stay trusted. `codex-trust.sh add` runs the same check (`codex_hooks.py --verify`) and
  rolls back with exit 1 if an acd hook outside the scratch project would run; the Codex scenarios are then
  skipped. Once you trust acd's hooks for everyday use, the Codex scenarios stay skipped on that machine: Codex
  runs its own Seatbelt sandbox, so it cannot be wrapped in the `sandbox-exec` deny profile the Claude runs use.
- At Codex's "Hooks need review" prompt the scripts press **Esc** (continue without trusting). That covers only
  your untrusted hooks; the scratch ones stay active.

```
just e2e                         # build, then tests/e2e/run-all.sh: up, all scenarios, down
just e2e-up                      # bring the scratch environment up
just e2e-one 40-claude-live      # one scenario
just e2e-down
```

`just e2e` runs `run-all.sh`, which traps EXIT, INT and TERM and always calls `down.sh` (agents and scratch herdr
server first, then logger, acd, T3, then the Codex config restore). It exits non-zero if any `FAIL` line is
present, a step failed, or teardown/restore failed. `up.sh` clears `/tmp/ace2e/results` at the start of every run.
If `codex-trust.sh add` fails, the Codex scenarios (30, 31) are skipped and the run is marked failed.

Knobs (environment variables): `E2E_CLAUDE_HOLD=<s>` (up.sh: sets `[claude] permission_hold`), `P` (Codex pane,
default `w1:p1`), `Q` (Claude pane, default `w2:p1`), `ESC_DELAY=<s>` (60-esc-diag: wait after the dialog is seen
before pressing Esc), `E2E_CLIENT_PORT` / `E2E_HOOK_PORT` / `E2E_T3_PORT` (never 47800, 47801, 3773), `ACD_BIN`,
`RUST_LOG`. `E2E_ROOT` is fixed at `/tmp/ace2e`; the scripts refuse to run if it is a symlink or not owned by you.

## Recovery after an interrupted run

`run-all.sh` cleans up on Ctrl-C, but a killed terminal, `kill -9` or a reboot skips the trap. Then:

1. `tests/e2e/down.sh` (safe to run twice; verifies each pid against its command line before killing it, kills
   the T3 process group, reports leftover processes and listeners).
2. `tests/e2e/codex-trust.sh status`. If it says the trust entries are ACTIVE, run `tests/e2e/codex-trust.sh restore`.
   The backup is kept in `~/Library/Caches/ace2e/` (not `/tmp`, so it survives a reboot): `config.before.toml`,
   its sha256, `config.block` (the exact appended bytes) and the post-add sha256.
3. Restore picks the safe way by itself: unchanged since `add` -> copies the backup back; changed but still ends
   with the saved block -> cuts exactly that block off and keeps your edits; otherwise it refuses, prints a diff
   against the backup, keeps the backup and exits non-zero. Then edit `~/.codex/config.toml` by hand: remove
   `[projects."/private/tmp/ace2e/proj"]` and the `[hooks.state."/private/tmp/ace2e/proj/..."]` tables.
4. `add` refuses while a backup exists, and refuses when `config.toml` already holds the scratch project entry
   without a backup (`status` warns about that case).

## What is isolated

Everything lives under `/tmp/ace2e`. Nothing touches the user's acd (launchd job, `~/.config/acd`, ports 47800
and 47801), default herdr session or desktop T3 (port 3773, `~/.t3`). Ports used: hook 52955, client 52956, T3 52957.

- **acd:** fresh `acd init` in `/tmp/ace2e/acd`, moved ports, T3 off (on only for `50-t3.sh`, pointing at the
  scratch T3). Run with `HOME=/tmp/ace2e/home` and `HERDR_SOCKET_PATH` set to the scratch herdr socket.
- **herdr:** session `ace2e` on herdr's own headless server (`herdr --session ace2e server`, detached, `env -i`,
  `HOME=/tmp/ace2e/home`, pid in `herdr.pid`; stopped with `herdr server stop` through the scratch socket, pid
  fallback after a command-line check). `up.sh` creates two workspaces: `w1` (Codex, `P`) and `w2` (Claude, `Q`).
  Every `hd` call refuses when the scratch socket is missing. The scratch socket must NOT sit under the real `~/.config/herdr/sessions`: the user's acd scans that directory
  and would adopt the scratch panes. No `agent.focus` is ever called.
- **Claude:** hooks come from `acd hooks install claude` into a scratch `CLAUDE_CONFIG_DIR`, loaded with
  `--settings`. The real `~/.claude` is still needed for login, and the user's own settings (with http hooks to the
  user's acd on 47801) still load: `--setting-sources project,local` and `allowedHttpHookUrls` do NOT exclude them
  (see the run doc). So the Claude process runs under `sandbox-exec` with a profile that denies outbound connects to tcp
  47801 and 47800 and to `~/.config/acd/acd.sock` and `hook.sock`, and file writes to `~/.config/acd`,
  `~/.claude/settings.json`, `~/.codex/hooks.json` and `~/.codex/config.toml`. The user's hooks then fail with
  `ECONNREFUSED 127.0.0.1:47801`, visible on screen.
- **Codex:** the user's `~/.codex/hooks.json` entries for acd are untrusted, so Codex does not run them (check
  with `codex_hooks.py <cwd>`, which uses `codex app-server` `hooks/list`; it runs no hook and starts no session).
  The scratch project's hooks need trust, which `-c` cannot grant, so `codex-trust.sh add` appends project-trust and
  hook-trust entries for `/private/tmp/ace2e/proj` to `~/.codex/config.toml` after a backup. `down.sh` (or
  `codex-trust.sh restore`) takes them out again; it copies the backup back only if the file is unchanged since
  `add` (sha256 of the current file equals the one recorded after the append), otherwise it removes just the saved
  block, or refuses (see Recovery). The sha256 check on a copy proves the copy, not that nothing else changed
  meanwhile; that is what the post-add sha comparison is for. The scripts press Esc at Codex's "Hooks need
  review" prompt (see Preconditions). Codex panes are closed and the herdr server stopped before the restore.
- **T3:** `t3 serve --base-dir /tmp/ace2e/t3home` on 127.0.0.1:52957 in its own process group (killed as a group by
  `down.sh`), `--base-dir` passed explicitly to `t3 auth session issue` and `t3 project add` with `T3CODE_HOME` as a
  second guard, token scoped to `orchestration:read` and `orchestration:operate`. The server cannot run under `sandbox-exec`
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
| `run-all.sh` | body of `just e2e`: up, scenarios, trap-protected down, exit status |
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
| `scenarios/60-esc-diag.sh` | diagnosis (not part of `just e2e`): Esc on a held Claude dialog, with and without a foreign hook; `Q`, `ESC_DELAY` |
| `scenarios/61-codex-deny-capture.sh` | capture (not part of `just e2e`): Codex pane after No / Esc / `n`; writes fixture candidates to `/tmp/ace2e/fixtures`, copy by hand into `tests/fixtures/herdr`; `P` |

Scenarios append `PASS`/`FAIL` lines to `/tmp/ace2e/results/<scenario>.txt`; `/tmp/ace2e/inbox.jsonl` holds every
acd/1 message with timestamps and `/tmp/ace2e/acd.log` the debug log.

## Manual / not covered

- Spec §14.1 (remote Tailscale machine) and anything needing the Mac app panel (P5 checklist).
- §14.10 blocked third-agent screen: herdr shows `opencode` and `pi` as `other` with their state, but no provider
  is configured to make them block. `herdr pane report-agent` did not change the state of a shell pane.
- `just e2e` as one chain has not been run end to end; the scenarios were run one by one. The herdr headless server
  and `run-all.sh` trap are checked only by dry tests (stubs), not by a live run.
- Panes are driven by sending keys; timings use 100 ms polling of the screen, so they carry that error plus
  model latency (seconds).
