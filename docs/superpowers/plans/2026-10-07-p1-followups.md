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

## P3: hooks and service

- `acd service install`:
  - restart on exit code 75;
  - handle SIGTERM gracefully;
  - make a dead serve task fatal;
  - exit the process when a mutex is poisoned;
  - resolve `tailscale` by absolute path, because launchd's PATH is minimal.
- `check_mode`: do not follow symlinks, and check the file owner. Clean up
  temp files on failure and fsync the directory after the rename.
- `init` repair: check the mode of files it keeps, and reject an empty
  `machine-id`.
- Superseding an `answering` Claude request when the next permission
  arrives before `PostToolUse` (see spec §6.2 attribution).
- Spec §11 gates 2, 4, 5 and 7 block P3.
- Implement `Evidence`: correlation with PostToolUse, Stop and
  UserPromptSubmit.
  - Use only evidence for `target.request_id` that arrived after
    `sent_at`.
  - Never reuse a verdict across request ids.
  - Answer within `evidence_timeout`.
- After opening each keys-answerable request, register a `KeysTarget` with
  `request_id = core.request_id_for(agent_id, &key)`. Targets of resolved
  requests are dropped by the P2 target watch.
- Build keys targets only with `KeysTarget::from_screen` (its fields are
  sealed). Register only after `core.agent(agent_id)` is Some.
- Option keys may only be "1".."9" or "esc", never Enter. The guard
  digest blanks the cursor, so it cannot tell which option Enter picks.
- Never decide an approval from `Dialog::text` or `Dialog::identity`. Forged
  rule lines in command text can shorten the text, and the identity folds
  whitespace. The guard digest (full sha256 of the whole screen through the
  footer) is the only security-relevant value.
- Never switch a held hook's request to `keys` while the hook is still held.
- When a hook request belongs to the dialog already shown as a
  `blocked_unparsed` fallback, upgrade it in place (`upgrade_from` =
  `herdr::fallback::key(..)`), never supersede it; otherwise the pane can
  show no request until the screen changes. After an upgrade, the fallback
  only emits `DialogGone` and never resolves the request.
- If a dialog with the same identity reappears within about 2 polls after a
  terminal answer, the old occurrence stays open (answers get
  `screen_mismatch`). Re-register with `from_screen` when the hook reports a
  new request. Identity is whitespace-folded (region text and labels with
  their continuation lines), so a dialog that differs from the old one only
  in whitespace or line breaks also counts as the same identity.
- A pane id that is closed and re-detected before its `agent.get` keeps the
  old agent: its agent id, pending request and key state carry over, and an
  old structured request resolves `terminal` through the watch instead of
  `lost`. A kind change under the same pane id behaves the same way. Track
  `Hint::Closed` per pane and emit `AgentGone` before re-adding; also track
  `terminal_id` / `agent_session.value` per pane and treat a change as a new
  agent.
- Spec §6.4 counts a pane whose `state_change_seq` rose without an event as
  a missed change. The reconciler only compares states, and only logs it.
  The periodic list applies every pane anyway, so this affects the log only.
- A dialog is recognized only when its footer is the last non-empty screen
  line. If a future Claude or Codex version draws a status line below the
  footer, update `screen::find_dialog`. It fails closed until then.
- **Cross-path lease.** The P2 lease check covers `Answering` status, the
  answer path and the presence of the target, and it runs after connect and
  before the write.
  - If P3 can activate a hook path or open a structured request for an
    agent while a keys delivery for that agent is between its lease check
    and its write, P3 must hold off until `deliver` returns. Use a
    per-agent async mutex shared with the herdr adapter.
  - Otherwise P3 records why the window cannot occur.
- **Stray-keystroke reason** (spec §6.2). Before any keys request goes live,
  add an optional `reason` to `request.resolved` (for example
  `"stray_keystroke"`). Today it reports only `by: uncertain`. This touches
  `acd-protocol` and its schema, so coordinate it with P5.
- Bind Codex sessions with `agent_session.value`, which `AgentInfo` now
  carries.

## P5: Mac app

- Add `serde(other)` to `Source`, `AgentKind`, `RequestStatus`,
  `ResolvedBy` and `SourceState`, if the Tauri backend reuses the Rust
  types.
- Express the one-of rules in the schema (`request.answer` takes
  `option_id` xor `text`; a result carries `ok` with `outcome` or `error`).
  Unknown variants currently serialize as `"unknown"`, and timestamps have
  no `format: date-time`.
- Clients must validate that a MagicDNS name resolves into the Tailscale
  ranges. TCP clients have no way to authenticate the server: decide this
  at the protocol level before P5 freezes the client. Local tools use
  `acd.sock`.
- Authenticated sessions per token are uncapped, and the idempotency table
  cap (16384 entries) is shared by all clients.

## Process

- There is no git remote yet, so CI (`.github/workflows/ci.yml`) has never
  run.
- `cargo fmt` has not been applied to the workspace.
