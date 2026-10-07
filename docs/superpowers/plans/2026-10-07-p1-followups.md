# P1 follow-ups: deferred items for later plans

These items came out of the P1 task, final and security reviews. They were
deferred on purpose. Each later plan must pick up the items listed under
its name.

## P2: herdr adapter

- Reject Unicode format characters (bidi overrides, U+2028 and U+2029) and
  text over 16 KiB before `agent.prompt` (spec §6.4).
- Enforce the §5 limits (`raw_screen` at most 40 lines, `last_message` at
  most 500 characters) in the adapter.
- Adapter contract (`acd-core/src/adapter.rs`):
  - deliveries finish within 10 s and are cancellation-safe;
  - attribution follows spec §5.1, "delivered and outcome consistent";
  - emit an agent upsert after a resolution that changes the agent's state.
- After a resolution, agent state stays `needs_you` until the adapter sends
  an upsert. Supersede emits two `AgentUpdated` events, with a transient
  `pending: null` between them.
- Check whether an adapter exists before claiming, to avoid the
  answering-to-open churn when it doesn't.
- Remove dead code in `Core::execute` (the Hello and Ping arms).
- Add registry tests: a Lost replay, `RequestPath` while not open, and an
  agent that is removed and then re-added.

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
