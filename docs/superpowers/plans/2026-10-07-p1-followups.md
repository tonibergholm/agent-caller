# P1 follow-ups: deferred items for later plans

These items came out of the P1 task, final and security reviews. They were
deferred on purpose. Each later plan must pick up the items listed under
its name.

## P2: herdr adapter (done)

- [done: Task 3 and Task 8] Reject Unicode format characters (bidi
  overrides, U+2028 and U+2029) and text over 16 KiB before `agent.prompt`
  (spec §6.4).
- [done: Task 3 and Task 6] Enforce the §5 limits (`raw_screen` at most 40
  lines, `last_message` at most 500 characters) in the adapter.
  `last_message` has no herdr source in P2; the helper exists for P3
  `Stop`.
- [done: Tasks 8 and 9] Adapter contract (`acd-core/src/adapter.rs`):
  - deliveries finish within 10 s and are cancellation-safe;
  - attribution follows spec §5.1, "delivered and outcome consistent";
  - emit an agent upsert after a resolution that changes the agent's state.
- [done: Task 9, `Hint::Pane`] After a resolution, agent state stays
  `needs_you` until the adapter sends an upsert. Supersede emits two
  `AgentUpdated` events, with a transient `pending: null` between them.
- [done: Task 1] Check whether an adapter exists before claiming, to avoid
  the answering-to-open churn when it doesn't.
- [done: Task 1] Remove dead code in `Core::execute` (the Hello and Ping
  arms).
- [done: Task 1] Add registry tests: a Lost replay, `RequestPath` while not
  open, and an agent that is removed and then re-added.

## P2 deferred

- The idle poll timer wakes every 1 s with nothing due.
- `clean_display` joins words across stripped tabs and newlines.

## P4: T3 adapter (done)

- Closed the adapter contract for `t3`, including §5 caps and display
  cleaning for T3 text, and added the `text::redact_log` helper.

## P3: hooks and service

Status, 2026-10-10: each bullet is tagged with the task of
`docs/superpowers/plans/2026-10-09-p3-hooks.md` that resolved it. Tasks 1
to 31 are approved in the SDD ledger; Task 32 (this file, the spec and the
live checks) is in progress. The leftovers are under "P3 deferred" below.

- [done: Task 4 (set-once targets) and Task 6 (targets bound to a pane
  incarnation); D8] Answer revisions (docs/superpowers/plans/2026-10-08-p5-answer-revision.md):
  a herdr keys target must stay immutable for the life of an occurrence,
  or be bound to the revision or digest that was claimed. Today the
  target is fetched after the claim from a replaceable map
  (crates/acd-adapters/src/herdr/keys.rs), so the revision check alone
  does not cover it.

- [done: Tasks 10, 11 and 16 (map, re-evaluation, T3 takeover forgets hub
  expectations), 18 (revoking live requests on every owner transition)] P3 ownership rule 1: read `T3Adapter::sessions()` (`SessionMap`). For each
  Claude session, if `SessionMap::owner(session_id)` is `Some`, drop the
  hook-side and discovery state for that session. Re-evaluate sessions whose
  ownership is unknown (spec §6.2 rule 4) on every `SessionMap::changes()`
  tick. The map publishes ownership when shell and detail agree and the
  native id is set, the first claim for a session id wins, and ownership is
  invalidated only by identity evidence.
- [still open: T3, out of scope for P3; needs a live T3] `project.removed`
  uses an inferred `projectId` field; the live runs could not
  produce one (`t3 project remove` returned `invalid_command`), so it is still
  unconfirmed.
- [done: P4, D21] T3 decode-error degradation clears after 10 minutes without new errors.
- [done: Task 7] Reuse `text::redact_log` for hook payload logging so secret patterns stay
  consistent.

- `acd service install` [done: Task 28 (the service), Task 29 (exit codes)]:
  - restart on exit code 75 [done: Tasks 28, 29; launchd
    `SuccessfulExit = false`, systemd `Restart=on-failure`];
  - handle SIGTERM gracefully [done: Task 29; held hook requests get an
    empty reply before `run` returns: Task 30];
  - make a dead serve task fatal [done: Task 29 (exit 1); the hook
    listeners joining the supervised set: Task 30];
  - exit the process when a mutex is poisoned [done: Task 29 (panic exits
    70)];
  - resolve `tailscale` by absolute path, because launchd's PATH is minimal
    [done: Task 29, `FALLBACK_CLIS`].
- [done: Task 20] `check_mode`: do not follow symlinks, and check the file owner. Clean up
  temp files on failure and fsync the directory after the rename.
- [done: Task 20] `init` repair: check the mode of files it keeps, and reject an empty
  `machine-id`.
- [done: Task 14 (O1, D10)] Superseding an `answering` Claude request when the next permission
  arrives before `PostToolUse` (see spec §6.2 attribution).
- [done: gates confirmed in a6e08dd; spec updated in Task 32 Step 2;
  live checks 10 to 14 remain open, 14 is a release gate] Spec §11 gates 2,
  4, 5 and 7 block P3.
- [done: Tasks 12 and 16] Implement `Evidence`: correlation with PostToolUse, Stop and
  UserPromptSubmit.
  - Use only evidence for `target.request_id` that arrived after
    `sent_at`.
  - Never reuse a verdict across request ids.
  - Answer within `evidence_timeout`.
- [done: Tasks 15 and 16 (Claude), Task 17 (Codex)] After
  opening each keys-answerable request, register a `KeysTarget` with
  `request_id = core.request_id_for(agent_id, &key)`. Targets of resolved
  requests are dropped by the P2 target watch.
- [done: Tasks 15 and 16 (Claude), Task 17 (Codex)] Build keys
  targets only with `KeysTarget::from_screen` (its fields are
  sealed). Register only after `core.agent(agent_id)` is Some.
- [done: Tasks 4, 15, 16 (Claude), Task 17 (Codex)] Option keys may only be
  "1".."9" or "esc", never Enter. The guard
  digest blanks the cursor, so it cannot tell which option Enter picks.
- [done: Task 13 (dialog match contract M1 to M5, raw rows only)] Never
  decide an approval from `Dialog::text` or `Dialog::identity`. Forged
  rule lines in command text can shorten the text, and the identity folds
  whitespace. The guard digest (full sha256 of the whole screen through the
  footer) is the only security-relevant value.
- [done: Task 15] Never switch a held hook's request to `keys` while the hook is still held.
- [done: Tasks 2, 14, 16 (Claude), Task 17 (Codex)] When a hook
  request belongs to the dialog already shown as a
  `blocked_unparsed` fallback, upgrade it in place (`upgrade_from` =
  `herdr::fallback::key(..)`), never supersede it; otherwise the pane can
  show no request until the screen changes. After an upgrade, the fallback
  only emits `DialogGone` and never resolves the request. Use the key the
  fallback recorded when it opened, not one recomputed from a later screen:
  the fallback keeps its open-time key across redraws.
- [done: Task 5] A malformed `agent.get` reply still drops the herdr connection (agents go
  `unknown`, then reconnect). Decode it leniently, as `agent.list` now is.
- [open: live check 10, Task 32 Step 6] Dialog identity keeps `Tip:` lines (so a wrapped tip doesn't look like a
  new dialog). If a Claude tip ever changes while the same dialog stays up,
  the watch would publish a false `terminal` and a confirmation could count
  the dialog closed. Check tip behaviour live before keys requests go live,
  and drop tip lines from the identity if they rotate.
- [done: Task 5] The target watch removes the target and emits `DialogGoneById` before
  `RequestEndedIfOpen`. A claim landing in that window leaves the request
  with no target: the delivery fails its lease and the request reopens with
  no watcher. Reorder (end first, then remove) or re-check under one step.
- [done: Tasks 14 to 16 (Claude: O1 to O3, a new request id and a fresh
  target), Task 17 (Codex)] If a dialog with the same identity
  reappears within about 2 polls after a
  terminal answer, the old occurrence stays open (answers get
  `screen_mismatch`). Re-register with `from_screen` when the hook reports a
  new request. Identity is whitespace-folded (region text and labels with
  their continuation lines), so a dialog that differs from the old one only
  in whitespace or line breaks also counts as the same identity.
- [done: Task 5 (pane incarnation, identity change)] A pane id that is
  closed and re-detected before its `agent.get` keeps the
  old agent: its agent id, pending request and key state carry over, and an
  old structured request resolves `terminal` through the watch instead of
  `lost`. A kind change under the same pane id behaves the same way. Track
  `Hint::Closed` per pane and emit `AgentGone` before re-adding; also track
  `terminal_id` / `agent_session.value` per pane and treat a change as a new
  agent.
- [decided: log-only, D19] Spec §6.4 counts a pane whose
  `state_change_seq` rose without an event as
  a missed change. The reconciler only compares states, and only logs it.
  The periodic list applies every pane anyway, so this affects the log only.
- [decided: stays fail-closed, D20] A dialog is recognized only when its
  footer is the last non-empty screen
  line. If a future Claude or Codex version draws a status line below the
  footer, update `screen::find_dialog`. It fails closed until then.
- [done: Tasks 4 and 6 (D7, per-agent delivery lock; lock order R11 in
  Task 15)] **Cross-path lease.** The P2 lease check covers `Answering` status, the
  answer path and the presence of the target, and it runs after connect and
  before the write.
  - If P3 can activate a hook path or open a structured request for an
    agent while a keys delivery for that agent is between its lease check
    and its write, P3 must hold off until `deliver` returns. Use a
    per-agent async mutex shared with the herdr adapter.
  - Otherwise P3 records why the window cannot occur.
- [done: Tasks 3 and 4] **Stray-keystroke reason** (spec §6.2). Before any keys request goes live,
  add an optional `reason` to `request.resolved` (for example
  `"stray_keystroke"`). Today it reports only `by: uncertain`. This touches
  `acd-protocol` and its schema, so coordinate it with P5.
- [done: Tasks 5 and 10 (`codex_bind`), Task 17 (the caller with mandatory
  screen checks and the T3 ownership check, C7/R12 at delivery)] Bind Codex sessions with `agent_session.value`, which `AgentInfo` now
  carries.
- [done: Task 3] When adding `reason` to `request.resolved`, map it in
  `acd_client::view::resolution_notice`. `stray_keystroke` maps to "A stray
  keystroke may be in the input box". Regenerate the bindings.

### P3 deferred

From the gates and the plan (`docs/superpowers/plans/2026-10-09-p3-hooks.md`,
"Out of scope"):

- `PostToolUseFailure` is installed and counted as "the tool ran", but its
  payload is unverified. `PermissionDenied` evidence is not installed or
  checked.
- Codex in the shared managed daemon: `agent_session` is missing and the hook
  environment is wrong, so only a single Codex pane in a cwd can be bound.
  Upstream fix; restarting the managed daemon is unverified.
- Binding commands that wrap or span several rows (D26). They get no screen
  binding and no keys target. Needs live check 12 and a separate review of a
  width-aware rule.
- A command-hook shim for Claude, to remove the loopback port-squatting
  exposure of the `http` hooks (ruling R7, post-v1). Needs gate 2 re-tested
  for command hooks.
- Free text in `AskUserQuestion` on the keys path, multi-select and
  multi-question answers (spec §11 item 1, §2).

Open live checks (Task 32 Step 6): 10 (`Tip:` lines), 11 (Codex hook review),
12 (wrapped commands), 13 (parallel `PermissionRequest`), 14 (registry entry
delay, **release gate**; `REGISTRY_GRACE_MS` is an assumption until then).

Amendment A1 mitigations for the Claude `http` hooks, which no plan task
owned, are done in Task 30: `acd init` picks a random free `hook_port` for a
new config (existing configs keep theirs), `acd status` and
`acd hooks status` warn, best effort, when another process holds the hook port
while acd is not running, and the Claude install prints the trust note. The
spec (§9) describes them.

Carries closed:

- Task 30: hook listeners are in the daemon's `servers` JoinSet (loss is
  fatal); held hook requests get an empty reply before `run` returns on
  SIGTERM; `rt.shutdown_timeout` bounds the exit; the endpoint `Credential`
  has a redacting `Debug`.
- Task 31 (R13): the in-memory hook credentials reload from the store within
  2 s, retirement revokes in memory at once, an expired previous token is
  rejected per request, the daemon clears its `on_current` set when the store
  is reset or regenerated, and the retirement caller passes complete scans.
- Task 17 (C7, R12): Codex callers run the T3 ownership check before
  `codex_bind` and apply the screen checks for `Session` and `OnlyInCwd`
  bindings; delivery re-reads every rival under the delivery lock. In the
  recorded misbound fixture all panes share one cwd, so the cwd check cannot
  reject the app-server pane; the screen checks are the only guard.
- Task 19 (A8): global caps on the history and session maps, and path
  resolution in at most 4 bounded workers with a 3 s freshness window
  (unknown fails closed), so a stalled mount cannot block the reconciler.
  Residual: a Claude header binding refreshes only on the next valid hook.
  Minor: a hooks-only agent's state stays frozen while a migration waits for
  the target agent.

Still open:

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

Flakes and environment:

- `crates/acd-adapters/tests/t3_conn.rs`
  `a_peer_that_stops_reading_trips_the_write_timeout` (P4) failed once in a
  full run under heavy load and passes alone; consider widening its 5 s
  bound.
- `herdr_e2e` failed once in a workspace run and passed on rerun (seen in
  Task 22).
- `secrets::tests::repair_mode_clears_special_bits_and_reports_them`,
  `init_repairs_a_setuid_secret_and_a_loose_t3_token` and
  `hooks::claude_sessions::tests::the_system_check_recognises_this_process`
  fail only inside the Codex sandbox (special bits and `ps` are not honoured
  there). They pass outside it.
- Luna dispatches reformat unrelated files; revert at commit time.

## P5: Mac app

- T3 requests use option ids `accept`, `acceptForSession`, `decline` and
  `o1`…`oN`. Clients must treat option ids as opaque.

- Add `serde(other)` to `Source`, `AgentKind`, `RequestStatus`,
  `ResolvedBy` and `SourceState`, if the Tauri backend reuses the Rust
  types. **Done, Task 1.**
- Express the one-of rules in the schema (`request.answer` takes
  `option_id` xor `text`; a result carries `ok` with `outcome` or `error`).
  Unknown variants currently serialize as `"unknown"`, and timestamps have
  no `format: date-time`. **Done, Task 1; null cases included.**
- Clients must validate that a MagicDNS name resolves into the Tailscale
  ranges. TCP clients have no way to authenticate the server: decide this
  at the protocol level before P5 freezes the client. Local tools use
  `acd.sock`. **Done, Tasks 2–3: pinned TLS (D1), by controller ruling
  after Sol round 1. Tasks 1–3 PR review: Round 3 approved; findings
  resolved (see P5 plan review log).**
- Authenticated sessions per token are uncapped, and the idempotency table
  cap (16384 entries) is shared by all clients. **Accepted, D13.**

### P5 deferred

- Optional `tailscale whois` node validation hardening.
- Keychain prompts after rebuilds of unsigned builds. No prompt appeared
  in the 2026-10-09 checklist run.
- The panel is centered; its position is not remembered.
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
- herdr alone produces only "Answer in terminal" cards; option cards and
  replies need T3 or P3 hooks. Say so in the setup docs.
- On a remote Mac, the macOS firewall prompts for the unsigned `acd`
  binary on first incoming connection. Say so in the setup docs.
- With an explicit Tailscale IP in `listen` and no Tailscale CLI, acd
  refuses the address without saying the CLI is missing. Key the startup
  warnings on `needs_tailscale_probe`, not the `tailscale` keyword.
- After Cancel in the inline remove confirmation, focus falls to the page
  body. Return it to the restored Remove button.

## Process

- There is no git remote yet, so CI (`.github/workflows/ci.yml`) has never
  run.
- `cargo fmt` has not been applied to the workspace.
