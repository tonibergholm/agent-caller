# Security review: local attack surface and answer delivery

Date: 2026-11-03. Base: `main` at `108034c`. Read-only review: no source file was changed.
Scope: `acd-adapters` (hooks endpoint, herdr, T3, producer API), `acd-securefs`, `acd`
(secrets, token stores, hook install, service install, shim, sockets, CLI), `acd-inbox`
and the Omarchy plugin it installs (`clients/omarchy/`, shipped by `acd-inbox install`
through `PLUGIN_FILES`).

Attackers considered: (a) another local user, (b) a same-user process with no authority
(npm script, editor extension), (c) the coding agent itself, (d) a producer of text shown
on a card.

## Summary

| # | Finding | Severity | Attacker | Reproduced |
|---|---|---|---|---|
| F1 | Omarchy panel hides everything after row 6 of a permission card, while the daemon still offers Yes | Medium | (d), (c) | Data level yes; render not run |
| F2 | The T3 bearer token (30-day, `operate`) is sent to whoever holds the T3 loopback port | Medium | (a), (b) | Yes (client side) |
| F3 | Any local user can starve the hook listeners with idle connections; no read timeout | Low to Medium (availability) | (a), (b) | Yes |
| F4 | herdr socket discovery checks no owner or mode; acd sends keys and follow-up text to what it finds | Low | (a), (b) | Yes (discovery) |
| F5 | systemd unit and plist are built from unescaped paths (newline injects directives) | Low | self only | Yes |

Five items. Nothing found that bypasses the key guard, the producer id rules, the
file-mode checks or the stale-socket rules. Rejected candidates are listed at the end.

## Method

Read the code on the paths above, then reproduced candidates with throwaway tests in a
scratch copy of the repo (`/tmp/.../scratchpad/repo`, not committed, not in this PR).
The sandbox ran as uid 0, so tests that expect a permission error are not meaningful
here. I did not run the full workspace suite; I ran only my own reproductions and the
`acd` binary built from the scratch copy. Time was spent reading, so some files were
sampled (see "Not checked").

---

## F1. Omarchy panel hides the tail of a card the user can approve

- **Where:** `clients/omarchy/Panel.qml:337` (`maximumLineCount: 6` on the detail
  `Label`, text is `tag + "  " + pending.detail`, `wrapMode: Text.Wrap`, no `elide`, no
  "more" control) and `Panel.qml:346` (same cap on the question text). Daemon side:
  `crates/acd-adapters/src/hooks/mapping.rs:78-110` builds the detail (one row per command
  line: `$ ` for the first, `> ` for each following line) and `:130-180` offers
  allow/deny unless the detail exceeds 4000 characters.
- **Attacker:** (d) prompt injection in whatever the agent runs, or (c) an agent that
  wants approval.
- **Steps:**
  1. The agent asks permission for a Bash command with six harmless rows and the real
     action on row 7:
     `echo build ok` + five empty lines + `rm -rf ~` (a `curl … | sh` works the same).
  2. acd builds the card. Detail is `["$ echo build ok", "> ", "> ", "> ", "> ", "> ", "> rm -rf ~"]`.
     Options `allow` and `deny` are offered, `ready=true`, `answer_path=Hook`.
  3. The Omarchy panel draws at most 6 rows. Row 7 is cut with no ellipsis. The
     user sees only the harmless rows and presses Yes (or Enter).
- **Impact:** The approval screen no longer shows what will run. This defeats the rule
  that a card must show the whole command or be read-only (`mapping.rs:170-174`,
  "a cut detail hides part of the command"). The daemon applies that rule at 4000
  characters; the panel applies a stricter, silent limit of 6 rows. The Mac app does not
  have this cap (`app/src/components/Card.svelte:62-78` renders every line, `:376-395`
  styles them as wrapped blocks), so the gap is specific to Omarchy. This is not covered
  by the follow-ups: Task 7 there is about RTL characters and leading spaces, not row
  count.
- **Reproduced:** Data level, yes. `claude_permission()` on that input returns the detail
  above with both options and `ready=true`. The QML render was **not** run (no
  Quickshell or Qt here); the cut follows from `maximumLineCount` with `ElideNone`, and
  I read it, did not see it.
- **Minimal fix:** In the panel, treat a `Label` whose `truncated` property is true as
  not fully shown and make the options unpickable (show "Too long to show here, answer
  in the terminal"). Or drop the cap on permission details and let the card scroll.
  Alternatively have `acd-inbox` mark a card read-only when the detail has more than N
  rows.

## F2. T3 bearer token goes to whoever holds the T3 port

- **Where:** `crates/acd-adapters/src/t3/conn.rs:379-381` puts `Authorization: Bearer
  <t3.token>` on the upgrade over plain `ws://`. The reconciler dials the configured
  port on its own, re-reading `t3.token` every loop, with a backoff of at most 30 s
  (`t3/reconciler.rs:217-254`, token read at `:227`). The URL check (`conn.rs:176-201`)
  accepts `localhost`, any loopback address and any `100.64.0.0/10` address.
- **Attacker:** (a) another local user, or (b) a sandboxed process that can bind loopback
  but not read `~/.config/acd`.
- **Steps:**
  1. T3 Code is stopped or restarting, so its port is free (acd keeps retrying).
  2. The attacker binds `127.0.0.1:<t3 port>` (any user may bind a port above 1023).
  3. Within 30 s acd connects and sends the token. The attacker reads it from the
     request headers.
  4. When T3 is back up, the attacker connects to it from any local uid with the stolen
     token.
- **Impact:** Spec §6.4 (lines 1214-1221) says the token is a 30-day session issued with
  `orchestration:read` and `orchestration:operate`. That is a replayable credential that
  can drive the user's T3 sessions, not a one-shot answer. This is the same class as the
  accepted R7 (a squatter on a loopback port gets a token) but it is not recorded for T3:
  the spec mentions the squat only for the Claude hook port and for `acd.sock`
  (lines 1474-1481, 1819-1835), and says the plain-`ws` token is acceptable only because
  the host is loopback or Tailscale. A stolen T3 token is worse than a stolen hook
  token: it is long-lived and works against a third-party server that acd does not
  control.
- **Reproduced:** Yes, client side. A listener on a free port captured
  `authorization: Bearer T3-SECRET-TOKEN-123` after `ws_url()` accepted the URL and
  `connect()` ran. I did not run a real T3 server, so the replay step (4) is from the
  spec's description of the token.
- **Minimal fix:** Use the 5-minute ticket (`POST /api/auth/websocket-ticket`, mentioned
  in the spec and unused) instead of sending the long-lived token to the socket. Or
  issue the T3 token with the narrowest scope and a short life. Also, `100.64.0.0/10` is
  wider than Tailscale (it is the carrier-grade NAT range); the bind code asks the local
  Tailscale daemon, the T3 check does not.

## F3. Hook listeners can be starved by idle connections

- **Where:** `crates/acd-adapters/src/hooks/endpoint.rs:597-603` (`Capped::accept` takes
  one of 128 slots *before* it accepts, and the slot lives until hyper drops the IO).
  `crates/acd/src/daemon.rs:588-606` serves it with plain `axum::serve`: no header-read
  timeout, no idle timeout. The 5 s `body_timeout` (`endpoint.rs:190`) only applies after
  the token check and only to the body.
- **Attacker:** (a) any local user on the TCP loopback hook port (no token needed, the
  port is visible in `/proc/net/tcp`), or (b) any same-user process on `hook.sock`
  (which also carries the producer API).
- **Steps:** Open 128 TCP connections to the hook port and send nothing. Keep them open.
- **Impact:** Every later hook call connects through the kernel backlog and then waits.
  Claude's lifecycle hooks have a 1 s timeout (`hooks_install/entries.rs:8-17`), so each
  one costs up to 1 s (the existing test
  `a_stalled_acd_adds_at_most_1_s_to_a_lifecycle_hook` pins that bound; I did not time
  Claude itself). `PermissionRequest` hooks time out after the hold + 10 s, but the
  terminal dialog stays usable while a hook is held
  (`docs/research/2026-10-09-p3-gates-claude.md:60`), so the user is not locked out. The
  inbox simply stops receiving requests, silently. Codex's shim gives up and prints
  nothing. No integrity impact.
- **Not R7:** R7 is squatting while acd is down. This works while acd is running.
  The spec says v1 "does not protect against hostile local users" (line 1835), but that
  sentence sits in the server-authentication paragraph; resource exhaustion by
  unauthenticated connections is not discussed. The acd/1 TLS listener does bound its
  pre-authentication stages (PROTOCOL §1); the hook listener does not.
- **Reproduced:** Yes. A 4-slot listener (same `capped` + `axum::serve` setup as the
  daemon). Control run with no idle sockets: the token-bearing POST returns `200 OK`.
  With 4 idle sockets held for **40 s** (longer than hyper's usual 30 s header timeout):
  no response before the 4 s client timeout, so the connections are not reaped.
- **Minimal fix:** Install a header-read and idle timeout (hyper `header_read_timeout`
  with a `TokioTimer`, or a per-connection `tokio::time::timeout` around the IO), and
  take the slot after the first request byte or after the headers are complete. Optionally
  cap connections per peer for TCP.

## F4. herdr discovery trusts any socket it can connect to

- **Where:** `crates/acd-adapters/src/herdr/discovery.rs:83-91` (`is_live`: is a
  socket, accepts a connect) and `:37-70` (`candidates`). No owner, mode or parent
  directory check, unlike the Codex shim (`crates/acd/src/hook_shim.rs:146-225`) and the
  Claude registry (`claude_sessions.rs:260`).
- **Attacker:** (a) another user, only if the socket path is in a place they can write,
  or (b) a same-user process.
- **Steps:** Candidates are `HERDR_SOCKET_PATH` in acd's environment,
  `~/.config/herdr/herdr.sock`, `~/.config/herdr/sessions/*/herdr.sock` and
  `herdr.extra_sockets` in `config.toml`. With the defaults all are under the user's home,
  so another user cannot plant one. If a user points `HERDR_SOCKET_PATH` or
  `extra_sockets` into `/tmp` or another shared directory (the herdr docs allow the
  override), another user can create a socket there first. acd then reads that socket's
  "screens", shows cards from them, and sends it `agent.send_keys` (`1`-`9`/`esc` only)
  and any follow-up text the user types (`herdr/client.rs:241`).
- **Impact:** Spoofed cards and a leak of follow-up text typed in the inbox. Keystrokes
  go to the planted server, not to a real pane, so there is no way to answer a real
  agent from here. Low, because it needs an unusual configuration or same-user access.
- **Reproduced:** Yes, discovery only. A socket owned by uid 12345 in a mode 0777
  directory: `is_live` returns true and `candidates` lists it. I did not run a fake herdr
  server through the whole reconciler.
- **Minimal fix:** For each candidate require the socket to be owned by the effective
  uid and every parent directory to pass the same ancestor rule as `hook_shim::resolve`;
  refuse (and log the path) otherwise.

## F5. Service files built from unescaped paths

- **Where:** `crates/acd/src/service.rs:251-267` (`unit_quote` escapes `\`, `"`, `%`, `$`
  but not newline) and `:206-245` (plist uses `xml()`, which is correct for element
  text).
- **Attacker:** Only whoever controls `ACD_CONFIG_DIR`, `--config-dir` or argv[0] when
  `acd service install` runs. That is the user. No privilege boundary is crossed.
- **Steps:** Run `acd init` and `acd service install --print` with `ACD_CONFIG_DIR` set to
  `<dir>/y\nExecStartPre=/bin/touch /tmp/PWNED`.
- **Impact:** The generated unit gets a new `ExecStartPre=` line. Reported because the
  task asked for it; it is self-injection. It matters only if some wrapper (an
  installer, a dotfiles tool) passes an untrusted path.
- **Reproduced:** Yes, with the built `acd`:

  ```
  Environment="ACD_CONFIG_DIR=…/y
  ExecStartPre=/bin/touch /tmp/PWNED"
  ```
- **Minimal fix:** Reject control characters (including `\n`, `\r`) in the exe and
  config paths before writing either file.

---

## Considered and rejected

- **Agent approves its own prompt (c); same-user process answers (b).** A same-uid
  process can read `client.token` (0600, same uid) and answer on `acd.sock`; read
  `hook.token` and act on `hook.sock` or as any producer; or connect to the Omarchy
  socket, where the peer check is uid equality only (`acd-inbox/src/startup.rs:125-133`)
  and the view broadcast gives it every card id. This is the documented boundary: spec
  lines 1819-1835 ("v1 does not protect against … same-uid processes") and PROTOCOL §8
  Trust. Not re-reported. Whether Claude and Codex sandboxes can reach these sockets was
  not checked.
- **Producer id traversal.** `gate()` runs `valid_producer_id` before any use
  (`external/http.rs:60-81`). Probed `..`, `%2e%2e`, `a%2Fb`, `a%00b`, `A`, `-a`, space, overlong
  UTF-8, 33 characters: all 422 with a fixed message, `a/../b` 404. Agent ids are
  `external:<pid>:<key>` and a pid cannot contain `:`, so no collision across pids.
- **Keystroke injection through herdr.** Keys are only `1`-`9` or `esc`
  (`herdr/keys.rs` `is_guardable_key`, `check_keys`, `KeysTargets::set`). Free text on
  the keys path returns `NotSupported` (`keys.rs`, `deliver`). The guard compares the
  whole-screen digest, requires an untruncated read, and re-reads under the per-agent
  lock (`keys.rs` guard block); the lease is rechecked after connecting. Follow-up text
  goes through `check_prompt_text` (`text.rs:13-27`: no controls, no bidi, 16 KiB).
- **Wrong pane, or a screen that changed.** Pane incarnation is bound to the target
  (`for_incarnation`, `lease`); Claude hook answers return on the held connection, so
  they cannot go to another pane; a forged hook event cannot create a keys target
  because the target is built from the real screen and `DialogMatch` requires the
  payload command to equal the on-screen row (`hooks/dialogs.rs:43-93`). The
  remaining windows (rival Codex pane, lease check to key write) are in the follow-ups.
- **Terminal control or newlines in card text.** `escape_text` shows controls, bidi and
  invisible characters as escapes, and prefixes command rows so lines cannot forge
  `Reason:` or key rows (`mapping.rs:47-110`); other adapters use
  `clean_display`/`clean_screen` (`text.rs:36-52`). The panel labels use
  `Text.PlainText` (`Panel.qml:291`), so no rich text or links. RTL residual: Task 7.
- **Symlink and TOCTOU on token and secret files.** `securefs` opens with
  `O_NOFOLLOW|O_NONBLOCK` and verifies owner, type and mode on the descriptor
  (`acd-securefs/src/lib.rs:74-150`). Writes stage in an `openat` directory fd with
  `O_EXCL|O_NOFOLLOW` and `renameat` (`acd/src/secrets.rs:154-290`). The parent
  directory is not checked by securefs, but it is the 0700 user-owned config directory,
  so only the same user could swap it. The hook installer refuses symlinks and
  writes Claude files at exactly 0600 (`hooks_install/mod.rs:229-260`, `:330-333`, `:446`).
- **Hook endpoint spoofing, stale socket takeover.** Constant-time token check
  (`endpoint.rs` `check_at`). `hook.sock` is replaced only if it is a socket owned by the
  euid whose connect gives `ECONNREFUSED`, under `acd.lock`, in a 0700 directory
  (`local_socket.rs:110-133`). The Omarchy socket does the same under its lock in a 0700
  runtime directory (`startup.rs:71-110`). The shim checks every ancestor root-first
  before connecting (`hook_shim.rs:146-225`). The bind-then-chmod gap is covered by the
  0700 parent.
- **Unbounded input.** Hook body 1 MiB, 512 events, 16 MiB queued, 64 holds, 5 s body
  timeout (`endpoint.rs:178-195`); producer body 256 KiB (`external/http.rs`); Omarchy
  lines 4 MiB with no buffering past the cap (`acd-inbox/src/server.rs:515-540`). Omarchy has no cap on the
  number of connections, but peers are same-uid only.
- **Unit/plist injection by an attacker.** See F5: the only input is the invoking user's
  own environment. Debug-only `ACD_TEST_RUN_AT` (`hooks_install/mod.rs:347-357`) runs
  `sh -c` but is compiled out of release builds; `just` builds release.
- **`git ls-files` in a hostile repo.** `hooks_install/git.rs:35` runs git in the target
  directory. Exploiting it needs a hostile `.git/config` in a repository the user owns;
  a clone does not carry one.
- **Command injection in the Codex hook entry.** exe and config dir are POSIX
  single-quoted and parsed back with `sh_words` (`hooks_install/entries.rs:53-100`).
- **Token or text in logs.** Hook payloads are logged only at debug, redacted and capped
  at 4 KiB (`mapping.rs:455-462`), which the spec accepts (PROTOCOL §1 Logs). The T3
  sites I read log fingerprints and redacted values (`t3/conn.rs:495`,
  `t3/reconciler.rs:436`, `:660`); I did not audit every `tracing` call in the crate.
- **Spoofed producer cards.** A holder of the hook token can post cards with any title.
  By design (PROTOCOL §8 Trust).
- **Known and recorded, not re-reported:** R7 (Claude hook port squat while acd is
  down), rival Codex pane readiness, the lease-to-key window, RTL characters (Task 7),
  Git path check (Task 24), idle poll timer, hook-token retirement behaviour.

## Not checked

- macOS: launchd plist behaviour, Keychain, the Tauri app. `Card.svelte` was read, not run.
- Live herdr, T3 Code, Codex, Claude Code, Tailscale; so the F2 replay and the F4
  end-to-end path were not exercised against real servers.
- The Omarchy QML render (no Quickshell or Qt in the sandbox), so F1's cut is from the
  source, not from a screenshot.
- Whether Claude or Codex sandboxes can reach `acd.sock`, `hook.sock`, the Omarchy
  socket or the loopback ports.
- Race conditions that need a racing harness (installer rename windows, socket
  bind/chmod), judged from the code only.
- `acd-core` (the acd/1 server), `acd-client` and the Secret Service keyring backend
  (`acd-inbox/src/linux/keyring.rs`, `secrets.rs`) were out of scope or only skimmed.
- The full workspace test suite was not run; the sandbox ran as uid 0, so the mode-bit
  and permission tests named in `CLAUDE.md` would not be meaningful.
