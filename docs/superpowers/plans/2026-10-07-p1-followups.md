# Known gaps and deferred work

Reviewed against the implementation on 2026-10-12. P1–P5 are implemented;
completed task history is preserved verbatim in
[the archived review ledger](../../archive/p1-followups-2026-10.md).
Phase plans and live-run records retain the original evidence.

## herdr

- Narrow Codex panes (about 50 columns or less) can wrap the final option
  hint `(esc)` onto a separate row. `scan_options` stops before finding
  the options, so the request falls back to read-only `blocked_unparsed`.
  This parser regression is separate from unsupported multi-row commands;
  see [live check 12](../../research/2026-10-09-p3-live.md).
- The idle poll timer wakes every 1 s with nothing due.
- `clean_display` joins words across stripped tabs and newlines.

## Hooks and service

- With a literal Tailscale IP in `listen` and no Tailscale CLI, startup
  refuses the address before emitting the missing-CLI warning. The warning
  is also gated on the `tailscale` keyword instead of
  `bind::needs_tailscale_probe`; this diagnostic gap remains unresolved.
- An already-known rival Codex pane can start showing the same command
  without revoking the original request's advertised `keys` readiness.
  Delivery rechecks the rival and safely returns `request_not_ready`, but
  the client offers an answer until the user clicks. This differs from the
  spec's read-only rule; see [live check 5](../../research/2026-10-09-p3-live.md).
Additional P3 live-record residuals (recorded 2026-10-10):

- Hook-token retirement deliberately considers every live Claude registry
  session, including ignored or unrelated sessions that never used acd.
  These can retain the previous token until its 30-day expiry, while
  `status_message` says they "still use the previous token". The retention
  is conservative; the diagnostic does not prove actual token use.
- Rotation status can remain silent for the first registry scan period
  (observed about 20–30 s), then report "waiting for a complete registry
  scan". This is a cosmetic reporting delay.
- Hook-config reload depends on the settings source: the live run saw a
  project-local symlink reload, while `--settings <file>` did not reload
  even after `/clear`. The observation was inferred from pending counts,
  not captured headers; user-level settings reload was not verified there.
- A held permission declined with terminal Esc resolved as `terminal`
  after 1.7 s in that run, rather than immediately.

The [P3 consequences and reinstall rerun](../../research/2026-10-09-p3-live.md)
retain the complete evidence. The launchd reinstall race was fixed and
six subsequent reinstalls passed; it is not an open gap.

Remaining limits from the gates and historical P3 plan:

- `PostToolUseFailure` is installed and counted as "the tool ran", but its
  payload was unverified in the original gates. `PermissionDenied` evidence is not installed or
  checked.
- Codex in the shared managed daemon: `agent_session` is missing and the hook
  environment is wrong, so only a single Codex pane in a cwd can be bound.
  Upstream fix; restarting the managed daemon is unverified.
- Binding commands that wrap or span several rows (D26). They get no screen
  binding and no keys target. Live check 12 recorded them; a width-aware rule still needs a
  separate review.
- A command-hook shim for Claude, to remove the loopback port-squatting
  exposure of the `http` hooks (ruling R7, post-v1). Needs gate 2 re-tested
  for command hooks.
- Free text in `AskUserQuestion` on the keys path, multi-select and
  multi-question answers (spec §11 item 1, §2).

Live checks 10–14 have been run; see [the P3 results](../../research/2026-10-09-p3-live.md).
Registry delay check 14 passed (worst 3.06 s against the 5 s grace). Codex
review check 11 was partial, wrapped commands remain unsupported, and
parallel Claude permissions were observed one at a time. `dialog_identity`
still keeps `Tip:` lines to avoid resize ambiguity; rotating tips remain
an unverified case.

### Review residuals

- Task 4: `KeysTargets::remove` retires ids even when no live target exists,
  so callers must not remove before setting; watch for a self-deadlock if a
  P3 holder re-enters `deliver` for the same agent.

Minors and residuals accepted in review:

- Task 1: some CLI tests inherit stdout and print throwaway tokens (for
  example `acd init`, `token rotate`); capture the output in the harness.
- Task 5: no test pins `lost()` clearing `closed`.
- Task 7: strong RTL characters in command lines are not escaped (natural RTL
  letters reorder the display), and herdr or T3 details may start lines with
  spaces. Escape strong RTL characters or document the residual.
- Task 9: a stale registry entry whose pid was reused by another process
  keeps the scan incomplete (conservative).
- Task 20: `repair_mode` on a file the owner cannot read (mode 0o000 or
  0o200) fails with a plain permission error; give a clearer message.
- Task 21: no failure injection for the writer stage; the `--force`
  generation-1 fallback reuses a generation number (no old token is accepted
  by it; watch the retention history, Task 26).
- Task 22: surplus reply bytes in a later read are ignored; the shim's
  `catch_unwind` wrapper has no test.
- Task 24: the Git check is path-based (it gates the write, it does not
  bind it); the gap between canonicalizing and opening the directory lands on
  a user-owned directory; the temp file can be `git add`ed in a short window;
  compare-then-rename has no compare-and-swap; Codex targets get no Git
  check.
- Task 25: a Codex journal written before Task 25 loads as Claude
  (branch-only, pre-release).
- Task 8: the brief's tests for the full event queue, disconnected holds and
  churn were defective (a dropped `Held` temporary; closing before the server
  accepted). Later hook-endpoint tests must use the `settle` and
  `hold_and_close` helpers in `crates/acd-adapters/tests/hooks_endpoint.rs`.

Historical flakes and environment caveats (not reproduced in this audit):

- `crates/acd-adapters/tests/t3_conn.rs`
  `a_peer_that_stops_reading_trips_the_write_timeout` (P4) failed once in a
  full run under heavy load and passes alone; consider widening its 5 s
  bound.
- `herdr_e2e` failed once in a workspace run and passed on rerun (seen in
  Task 22).
- `crates/acd/tests/hooks_cli.rs` `a_path_that_is_not_a_socket_is_never_replaced`
  failed once on Ubuntu CI (2026-10-12, PR #24, run 38209946246): the
  "not a socket" message was missing and a worker panicked at
  `acd-core/src/tls.rs:430`. It passed on rerun. The daemon may have
  exited on a TLS setup error before the socket check.
- `secrets::tests::repair_mode_clears_special_bits_and_reports_them`,
  `init_repairs_a_setuid_secret_and_a_loose_t3_token` and
  `hooks::claude_sessions::tests::the_system_check_recognises_this_process`
  fail only inside the Codex sandbox (special bits and `ps` are not honoured
  there). They pass outside it.
- Review automated edits for unrelated formatting; keep changes scoped.

## Mac app

- Optional `tailscale whois` node validation hardening.
- Ad-hoc signed rebuilds can prompt for Keychain access. `just app-install`
  supports `APPLE_SIGNING_IDENTITY` for a stable identity; signing and
  distribution are not automated releases.
- The `acd pair` output remains in the user's clipboard after import.
- Test the hotkey failed-registration path; it requires a live shortcut manager.
- `mark_seen` has no test outside a runtime.
- Concurrent `set_hotkey` calls can race.

From the 2026-10-09 production-bundle checklist:

- The app never asks for notification permission and cannot tell when
  notifications are off; macOS dropped them silently until the user
  enabled them in System Settings. Request authorization on first run
  (UNUserNotificationCenter) and show the state in Settings.
- A saved hotkey keeps the old default (`Alt+Space`) after the default
  changed to `Control+Alt+Space`. Decide whether to migrate it.
- A pairing line with a literal IP outside the Tailscale ranges is saved
  and only refused at connect time. Refuse it at import.
- A Codex agent's title showed `<environment_context>`: raw context leaks
  into the agent title. Find which adapter sets it.
- herdr alone produces read-only blocked-screen cards; structured option
  cards need hooks or T3. herdr can send follow-ups to idle or done agents.
- After Cancel in the inline remove confirmation, focus falls to the page
  body. Return it to the restored Remove button.


Open items from the live end-to-end run, the v1 acceptance checks and the panel work (PRs #10, #12, #14, #18, #19, #20). Each was found by a run or a review and was judged non-blocking.

## Live-run residuals (P6/P7)

### Daemon

- **T3 `project.removed`.** The adapter uses an inferred `projectId` field;
  live runs could not produce the event (`t3 project remove` returned
  `invalid_command`), so the payload remains unconfirmed.

- **Esc before Claude draws the dialog.** A terminal Esc that arrives before the held dialog is drawn leaves acd without a capture; the request resolves `uncertain` after 30 s instead of `terminal`. Only reproducible by a script pressing Esc about 65 ms after the hook.
- **Restart with a Claude dialog on screen.** After an acd restart, a dialog still on screen comes back as `blocked_unparsed`, read-only, because the hook payload M1 needs is gone (spec §12 "requests still on screen reappear" holds, but they are not answerable).
- **Stale T3 requests on subagent threads.** On minos, two subagent threads keep a `pendingRuntimeRequest` long after the user considers them answered. Hidden by default since #19; find out whether T3 or acd is wrong.
- **First-seen hidden Codex subagents.** Their session id is only in a thread stream, so a T3-run Codex subagent never claims its session. Its hook events reach acd but create no agent without a herdr binding (spec §6.5).
- **Codex and other providers' free text.** Typed T3 answers are live-verified for Claude only. Codex questions stay option-only unless T3 sends `allowCustomAnswer: true`; "OpenCode sets false" is unverified.
- **T3 keepalive.** No paused-clock test for the busy cap; the slow-decode test does not reach `block_in_place`; a reader busy past the cap closes with "T3 did not answer a ping" (a distinct reason would help); `write_timeout` covers a whole frame, not write progress.
- **T3 cursor retention.** A busy root thread re-pulls a ~24 MB snapshot after each `release()`; keep the cursor for Working-class releases.
- **`acd status | head`** panics on a broken pipe.
- **herdr machines.** herdr's own SSH machines could reach remote panes without a per-machine acd; acd would still be needed on each machine for the hook endpoint.

### Mac app

- **Panel frame.** No unit test for `save_now`; the order of the screen-parameters notification and macOS's refit moves needs a runtime check with a display unplugged.
- **Countdowns.** Turning idle agents off cancels a countdown on an Idle card that the view would now keep; the Done header count includes an out-of-filter card kept for its countdown; no panel test for J/K and Esc on a counting Done row while Done is collapsed.

### Harness (`just e2e`)

- `run.lock` has two millisecond races, and `just e2e-down` ignores the lock.
- Once the user trusts acd's Codex hooks, the Codex scenarios skip themselves on that machine.
- §14.10's blocked-screen half was not testable (no provider for opencode or pi); `screen_mismatch` (§14.4) was never observed.

### Machines

- omarchy: `loginctl enable-linger toni` keeps acd running while logged out (optional).
- minos: agents started over SSH cannot use the login keychain; acceptance there relies on locally started sessions.
