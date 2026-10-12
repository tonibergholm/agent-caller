# Omarchy Inbox Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `acd-inbox`, a systemd user service that runs the `acd-client` inbox and serves it over a local socket, and the Omarchy shell plugin `tonibergholm.agentcaller` that shows it, with Secret Service storage for remote secrets, freedesktop notifications and an installer.

**Architecture:** `acd-client` gains action owners, typed machine and secret errors, secret status and handle recreation, and stops calling the `SecretStore` under its inbox lock. A new crate, `crates/acd-inbox`, holds the socket protocol, the startup contract, a per-connection outbox, the server, the command table, the secret worker, the notifier and the installer. The protocol, server, commands, worker and notifier logic build and test on Linux and macOS. Only the Secret Service and D-Bus notification backends, and `serve` itself, are Linux-only. The plugin in `clients/omarchy/` is thin QML over `model.mjs`, a pure JavaScript module tested with `node --test`.

**Tech Stack:** Rust 2024 (workspace `rust-version = "1.90"`), tokio, serde, serde_json, clap 4, tracing, async-trait; Linux only: zbus 5.19.0 (`tokio` feature), secret-service 5.2.0 (`rt-tokio-crypto-rust`). QML on Quickshell (Omarchy 4 shell). Node 24 for `node --test`. Docker `rust:1-bookworm` (`linux/amd64`) for the Omarchy build.

**Spec:** `docs/superpowers/specs/2026-10-12-omarchy-inbox-design.md` at `7994c25` (authoritative; §13 records the review decisions). Task 4 amends its §5 for oversized views, as the orchestrator decided.

**Revision:** this plan answers Sol's review of `034d8d0` (`/tmp/sol-omarchy/findings3.md`); see "Review answers" below. Also `docs/superpowers/specs/2026-10-07-agent-inbox-design.md` §12 (undo and disconnect rule).

## Spec issues found

These are places where the spec and the code, or two spec sections, disagree, or where the spec cannot be followed literally. The plan's handling is stated; neither the spec nor the code is silently changed to match.

- **S1 and S2 are resolved in the spec** (`7994c25`): §5.4 now says settings entries join `InboxView.machines` by key, and §8 no longer claims the Mac app's notification wording. The plan follows both (D7, D15).
- **S3. `MachineEntryView.secret` reaches the app.** Adding a field breaks the exhaustive destructure in `app/src-tauri/src/pairing.rs` (`view_has_only_public_fields`). With a normal field it also changes the ts-rs bindings (`bindings_are_current`) and the TypeScript literals in `app/src/preview.ts`, `app/src/lib/fixtures.ts` and `app/src/components/Settings.test.ts`. The plan marks the field `ts(skip)` (D5), so the bindings and TypeScript do not change, and adds `secret: _` to the one Rust destructure. That is an `app/` change: Task 3 needs `npm run verify` on macOS (pending for CI when run on Linux).
- **S4. Outcomes have no card.** §5.3's `outcome` carries `"card"`, but `acd_client::voice::ActionEvent` has only `action_id`. Every `emit` call site has the `Action` in hand, so Task 1 adds `card_key` to `ActionEvent` (additive; voice reads only `action_id` and `outcome`).
- **S5. `node --test clients/omarchy/tests/` fails on Node 24** (checked: a directory argument is treated as a test file and fails). The glob form passes. The plan uses `node --test 'clients/omarchy/tests/*.test.mjs'`.
- **S6. Mac wording on Linux.** `Settings::fresh()` names the local machine "This Mac", and `NOTICE_NOT_PAIRED` says "Add from clipboard". `acd-inbox` writes its first `settings.json` with the host's name (D18). The panel never shows `NOTICE_NOT_PAIRED`: a machine chip with no handle takes its text from `secret` status (Task 17).
- **S7. `model.js` cannot be both a QML library and a Node module.** A QML `.js` library (`.pragma library`) cannot `export`, and Node cannot load `.pragma`. The plan names the file `model.mjs` (an ECMAScript module, which Qt 6 QML imports with `import "model.mjs" as Model`). Task 16 checks this on the Omarchy machine; Task 17 has the fallback.
- **S8. The Omarchy plugin API is not in this repository.** `qs.Ui` `Panel`, `Commons.Color`, `Style`, `ShellIpc`, the manifest schema, `omarchy-shell shell rescanPlugins`, `omarchy plugin enable`, and the Hyprland bindings file are named in the spec but not documented here. Quickshell's own IPC type is `IpcHandler` (`Quickshell.Io`). Task 16, a local task that runs before any QML is written, reads the installed `omarchy.tailscale` plugin and runs a probe plugin. It records the manifest, `Panel`, `ShellIpc`, `Socket` and `SplitParser` behaviour, the JS host globals, notification actions, and plugin enable and placement. Tasks 17 to 20 use those recorded names, and Task 21 only validates.
- **S9. No `owners` module.** §4.1 lists `owners` ("which connection owns which action"). With owners in `acd-client` (§4.3), each action carries its owner, and a connection's owner is `ActionOwner(connection id)`. No separate table is needed (D1). The module layout differs from §4.1 in two more places. The Secret Service code is `SecretServiceBackend` in `linux::keyring`, behind the `SecretBackend` trait that `secrets` defines (with `SecretWorker` and `WorkerStore`), not a `SecretServiceStore` in `secrets`. The D-Bus notification code is `linux::dbus`, behind `notify::NotifyBackend`. This keeps every Linux-only dependency in one gated module (D13, D14, D26).
- **S10. `AlreadyCounting` has no code.** §5.2's list omits it; a second view picking a counting card is a real case (Review Focus 2). The plan adds `already_counting` (D8).

## Decisions

Task text refers to these by number.

- **D1. Owners.** `acd_client::actions::ActionOwner(pub u64)`, with `ActionOwner::DEFAULT = ActionOwner(0)`. `pick`, `answer_text`, `reply`, the guarded variants, `cancel` and `escape` keep their signatures and use `DEFAULT`; `escape()` still cancels the newest action of any owner, so the Mac app and voice behave as before. New: `pick_owned`, `answer_text_owned`, `reply_owned`, `cancel_owned(owner) -> usize`, `escape_owned(owner) -> bool`. `acd-inbox` numbers connections from 1 and uses `ActionOwner(id)`.
- **D2. `ActionEvent` gains `card_key`** (S4). `Inbox::emit` takes the `&Action`.
- **D3. An edit lock.** `Inbox` gains an `EditLock` (a mutex plus a waiter count for tests). It is held for the whole of every change to machines or to the saved settings: `pair_machine`, `rename_machine`, `remove_machine_checked`, `reconnect_machine`, `set_hotkey`, `set_show_subagents` and `set_show_hook_only`. So no writer can save over another's result (a rename during a re-pairing is kept; a settings write during a removal cannot bring the machine back), and `remove_machine_checked` needs no corrective save. `SecretStore` calls may run under it, never under `inner`. Timers, `cancel`, `escape`, views, `machines()` and `settings()` never take it.
- **D4. Typed errors.**
  - `SecretError` stays `pub struct SecretError(pub String)` (the Mac Keychain code builds it).
  - It gains `SecretError::locked()`, `SecretError::timeout()`, `SecretError::unknown()` and `kind() -> SecretErrorKind { Locked, Timeout, Unknown, Other }`. The three constructors use fixed texts that no Keychain message equals. `Unknown` means a change to the item may or may not have happened (D36).
  - `acd_client::inbox::MachineError` is the typed error of `pair_machine`, `remove_machine_checked` and `reconnect_machine`. Its `Display` reproduces today's strings exactly, so `import_pairing` and `remove_machine` (now thin wrappers) return the same texts.
- **D5. `MachineEntryView.secret: SecretStatus` is `#[cfg_attr(feature = "ts", ts(skip))]`.** The Mac bindings do not change (S3). `SecretStatus` is `ok | locked | missing | unreadable`. `SecretStore` gains a default method `status(key)`, derived from `get`; a `Locked`-kind error gives `locked`. `pairing` keeps its values: `locked` maps to `unreadable` there, as today any read error does.
- **D6. `reconnect_machine(key)`.** A machine with a handle gets `retry_now()` (a no-op while online). A machine without one has its secret read outside `inner`, then a handle is created and watched. Errors: `UnknownMachine`, `NotPaired` (no secret), `SecretRead(e)` (locked, timeout, unreadable). `retry_machine` stays as it is for the Mac app.
- **D7. Wire shapes.**
  - `view` is `{"type":"view","view":<InboxView>,"omitted":{"done":n,"working":m}}`. Nesting avoids a clash with the `machines` key of `settings`; `omitted` is always present (D9).
  - `settings` is flat, as §5.4 shows, without connection status; if it ever had to be cut, `omitted_machines` counts the entries left out.
  - `pick`, `answer_text` and `reply` reply `{"type":"ok","id":…,"result":"<action id>"}`.
  - `cancel` and `escape` reply with `result` `true` or `false`.
  - `pair` replies `{"machine":"<key>","name":"<name>"}`.
  - Every other success replies `"result":null`.
- **D8. Error codes**, all fixed snake_case strings:
  - Picks and answers: `stale`, `unknown_card`, `not_answerable`, `unknown_option`, `already_counting`, `bad_text`, `cannot_reply`, `cannot_mark_seen`, `hidden`.
  - Machines: `unknown_machine`, `not_paired`, `bad_name`, `bad_pairing`.
  - Keyring and settings: `secret_store_locked`, `secret_store_timeout` (also for an unconfirmed change, D36), `secret_store_failed`, `settings_failed`.
  - Protocol: `bad_request` (a field is missing or has the wrong type), `duplicate_id`, `unsupported`.
  - `PickError::ConnectionChanged` (guarded paths only) maps to `stale`.
- **D9. Oversized views** (orchestrator decision, 2026-10-12; Task 4 amends spec §5). `protocol::view_line(view, max)` returns a line under `max` bytes, newline included, or `None`.
  1. The whole view, if it fits.
  2. Agent text cut, longest first, down to no less than 256 characters plus the marker `"…[truncated]"`. A binary search finds the largest common cap that fits. Keys that hold ids, enums or times are never cut: `id`, `machine_id`, `agent_id`, `source`, `kind`, `state`, `status`, `answer_path`, `effect`, `state_since`, `opened_at`, `role`. Cuts fall on character boundaries.
  3. Then Done and idle cards are left out, then Working and unknown ones, never a card with a countdown. They are counted in `omitted`, and the panel shows "N not shown".
  4. Needs-you cards are never left out. If they alone do not fit, their text is cut further, down to 32 characters per field.
  5. If even that does not fit, `None`. The service then sends `{"type":"view_overflow","needs_you":n}` instead of a view (orchestrator decision, 2026-10-12). The panel shows an explicit error state with the count until a view fits again.
  `settings_line` is bounded the same way, leaving machines out from the end.
- **D10. Envelope.**
  - A line must be a JSON object with a string `type` and a string `id` of 1 to 64 bytes. Anything else is malformed and closes the connection (§11).
  - The service parses the envelope first and dispatches on `type` second, so an unknown `type` with a valid id gets `error unsupported` and the connection stays open.
  - A missing or mistyped field gets `error bad_request`.
  - Each connection remembers its last 1024 ids; a repeat among them gets `error duplicate_id` and nothing runs.
- **D11. Slow commands run off the reader.**
  - `pair`, `remove_machine`, `retry_machine` and `unlock_secrets` can wait on the keyring (up to 5 s, or 120 s for unlock). `rename_machine` and `set_show` can wait on the edit lock (D3). They run on `spawn_blocking` and reply when done.
  - `retry_machine` first refreshes that machine's cached secret, so a cached keyring error is retried only when asked (D14).
  - Everything else runs inline in the reader task, under the admission lock (D31).
  - So the reader always sees EOF promptly, and a disconnect cancels countdowns at once (Review Focus 1).
- **D12. Outbox order.**
  - Each connection first gets `hello`, then `settings`, then `view`, in that order. Nothing else goes out before them.
  - After that the writer takes turns over four classes: replies, outcomes (a pending `outcomes_lost` first), the newest `settings`, the newest `view`. It resumes after the class it served last, so no class starves another.
  - Outcomes queue up to 256. On overflow the queue is cleared and one `outcomes_lost` is due; upstream `Lagged` does the same on every connection.
  - Replies queue up to 1024. A view that leaves more unread is closed (`reply_overflow`).
- **D13. The secret worker.**
  - One `std::thread` owns the backend and runs a current-thread tokio runtime with the async `secret-service` API. The blocking API is not used: with zbus's `tokio` feature it runs its own static runtime and panics inside ours.
  - Every job carries the caller's deadline: 5 s for read, write and delete, 120 s for unlock.
  - The worker is strictly serial. A job that reaches the front of the queue after its deadline is answered `Expired` and never runs. So a write queued behind a prompt cannot happen after its caller reported a timeout.
  - A started read or unlock runs only until its deadline; its future is then dropped, which changes nothing in the keyring.
  - A started write or delete is never abandoned: the worker awaits it to completion (orchestrator decision, 2026-10-12). Its caller still stops waiting at the deadline and gets `Ambiguous`, which becomes `secret_store_timeout` with the machine `unknown` (D36). Every later job runs after the change settled.
  - A hung D-Bus write or delete holds the worker. Every queued job then expires, or its caller times out: everything fails closed, statuses are `unknown`, and the panel says the keyring is not responding.
  - Reads check `Collection::is_locked` and `Item::is_locked` and return `Locked` without prompting. A lock check cannot stop the service from asking for a prompt on a write or delete (the Secret Service spec allows one per operation), so writes and deletes call `CreateItem` and `Item.Delete` directly and dismiss any returned prompt, reporting `Locked`. Only `unlock` runs a prompt. The fake models this with `prompts`.
  - secret-service 5.2 cannot dismiss its prompt. After an unlock time-out the prompt can stay on screen. A late unlock only unlocks the collection, and the next refresh picks that up; a late dismiss changes nothing.
- **D14. `WorkerStore`, not `CachedSecrets`.**
  - Per key, the cache holds the outcome of the last read: a value, `Locked`, `Unavailable`, `TimedOut`, or `Unknown` (a change whose caller stopped waiting, or a re-pairing that could not commit). A cached error is answered from memory.
  - Only `refresh(keys)`, on unlock and on `retry_machine`, reads a key again.
  - `prefill(keys)` runs on the blocking pool before `Inbox::start`. So `start`, `Service::new` and every settings snapshot are served from memory; no async thread waits on the keyring.
  - A read of a key with no entry (a machine being paired) goes to the worker. That only happens in `pair_machine`, which runs on the blocking pool.
  - An `Ambiguous` write or delete makes the key `Unknown` at once; nothing is read back (D36).
  - Read results are published under per-key generations (D37).
  - `Unavailable` shows as `secret: unreadable`; `TimedOut` and `Unknown` as `secret: unknown`.
- **D15. Notification text.**
  - Summary: `"<agent title> on <machine> needs you"`. Body: the request label, using the panel's rule (`toolName`): `"bash approval"`, `"approval"`, `"question"`, `"blocked"` or `"request"`.
  - Agent text is cleaned (whitespace, control, bidi and separator characters become single spaces) and cut to 60 characters with `…`.
- **D16. Notification bookkeeping.**
  - Notifications are keyed by `(card, request id, revision)`. A revision bump closes the old one and shows a new one.
  - A request is considered once, when it first appears: if a panel is open, notifications are off, or the card has a countdown (the user's own), it is marked seen and never notified later.
  - When a key leaves the view, its notification is closed.
- **D17. Notification action trust.**
  - An `ActionInvoked` signal opens a card only if all of these hold: the id is one this service created; the signal's sender is the current owner of `org.freedesktop.Notifications`; and that owner is the one that was current when the notification was shown.
  - After a notification-server restart, old ids are ignored (Review Focus 5).
- **D18. First run.** If `settings.json` does not exist, `acd-inbox` writes `Settings::fresh()` with the local machine renamed to the host's first DNS label (else "This computer", at most 64 characters), then calls `Settings::load_or_init`.
- **D19. `model.mjs`** (S7). Node tests use the glob command (S5). CI runs them on the Ubuntu leg.
- **D20. Line framing.** QML's JavaScript host has no `TextEncoder` or `TextDecoder`, and the plugin uses neither.
  - `Service.qml` relies on `SplitParser` (`splitMarker: "\n"`), which splits bytes before decoding and hands over whole lines as strings. Task 16 confirms this with a line that ends in a split 4-byte character.
  - `Model.LineReader.push(line + "\n")` enforces the 4 MiB limit with a pure-JS `utf8Length` and copes with a line given in pieces.
  - The spec's "split UTF-8" test is therefore Task 16's probe, plus node tests for split surrogate pairs and for loading `model.mjs` with no `TextEncoder`.
- **D21. Plugin files are embedded** in the binary with `include_str!`, so `acd-inbox install` on the Omarchy machine needs no checkout.
- **D22. Keybinding.**
  - Line: `bindd = SUPER ALT, A, Agent inbox, exec, omarchy-shell tonibergholm.agentcaller toggle`.
  - File: `~/.config/hypr/bindings.conf`. Task 16 confirms the file name.
  - It is appended only after an explicit `y`, with a timestamped backup. A non-terminal stdin counts as no.
- **D23. Runtime directory and lock file.** The directory: `acd_securefs::check_private_dir(dir, 0o700)`, a no-follow open, owned by the euid, mode within 0700. The lock file: opened `O_NOFOLLOW`, then its descriptor is checked as `crates/acd/src/writer_lock.rs:17` does (a regular file owned by the euid; a looser mode is repaired to 0600).
- **D24. Peer check.** Uses `tokio::net::UnixStream::peer_cred()`, which is `SO_PEERCRED` on Linux and `getpeereid` on macOS (tokio `src/net/unix/ucred.rs`). `nix` is not needed. The pure rule is `startup::peer_allowed(peer_uid, own_uid)`; an unreadable credential fails closed.
- **D25. `acd-inbox serve` runs on Linux only.** On other systems it exits with an error. Its wiring (`serve::start_service`) is tested on both platforms with fakes.
- **D26. Crates.**
  - `zbus = { version = "5.19.0", default-features = false, features = ["tokio"] }`. Without `default-features = false`, zbus's default `async-io` feature would turn on for every crate that shares it.
  - `secret-service = { version = "5.2.0", features = ["rt-tokio-crypto-rust"] }`, used with `EncryptionType::Dh`; it brings the same zbus 5.
  - Not `oo7`: its current release is `0.7.0-beta`.
  - Both go under `[target.'cfg(target_os = "linux")'.dependencies]`. Versions were checked with `cargo search` and `cargo info` on 2026-10-12.
- **D27. Install restarts the service.** `systemctl --user enable --now` does not restart a running service, so `install` follows it with `systemctl --user restart acd-inbox.service` to pick up a new binary.
- **D28. A notification server that fails** (connect at start, or any `Notify` call) turns notifications off for the session, and the rest of that batch is dropped. The failure is logged by class. The bar count still updates.
- **D29. Closing a connection** (EOF, read error, oversize or malformed line, write timeout or error, reply overflow) goes through one idempotent `Service::close`. Under the admission lock (D31) it marks the connection closed, calls `cancel_owned`, and clears the connection's panel-open flag, so a crashed panel cannot suppress notifications forever.
- **D30. Always-allow options in the panel.** An `allow_always` option shows its rule (`description`) on the card. If it has no description, the panel does not let it be picked, which is stricter than the Mac panel. Options outside `selectable_options` (unknown effect) and options on a card that is not `answerable` (read-only) are never pickable.
- **D31. Admission is serialized with close.** Each connection has one admission lock.
  - Every inline command (one that may create an action or set the panel flag) runs under it, after checking that the connection is still open. `close` takes the same lock to mark the connection closed and call `cancel_owned`.
  - So an action is either created before the close, and the close cancels it, or it is refused. A `panel(open)` that arrives late is refused too.
  - Lock order: the admission lock, then the inbox's `inner` or `panels`.
  - Tests park a command inside the lock with a hook and check that the close waits (Task 8).
- **D32. Ordered publication.**
  - Connections, the newest settings line and the newest view line live under one registry lock.
  - Views have one publisher, the broadcaster task. Settings publications carry a generation, taken before the snapshot is computed, and an older one is dropped.
  - A new connection is seeded under the same lock, so it never gets an older line after a newer one.
- **D33. Logging that `RUST_LOG` cannot open.**
  - `serve::init_logging_to(env, writer)` installs `EnvFilter(env or "info")` AND a fixed deny-list as two filter layers, plus the `log` bridge.
  - The deny-list: `tungstenite` and `tokio_tungstenite` off; `rustls`, `tokio_rustls`, `hyper`, `hyper_util`, `h2`, `axum`, `zbus`, `zvariant`, `secret_service`, `mio` and `tokio` at most `warn`.
  - Production calls it with `RUST_LOG`. The capture test calls the same function at `trace` and pushes secret-bearing `log` and `tracing` records through the dependencies' targets.
- **D34. Typed answers and keys in QML.**
  - Opening a typed-answer editor records the card, request id and revision (`Model.answerEditor`). The answer goes to that request. When the view no longer shows it (`answerEditorValid`), the editor closes with "The request changed. Your answer was not sent." and the text is dropped.
  - Every key, in the panel or in a text field, goes through `Model.onKey`, with the field's kind and its `inputMethodComposing`.
- **D35. Drafts outside delegates** (Sol re-review, adopted). Repeater delegates are recreated whenever a new view replaces the model array, so text fields cannot own their text.
  - Typed-answer, reply and rename texts live in `Model.Drafts`, held by the panel. Keys: `answer:<card>:<request>:<revision>`, `reply:<card>:<request or ->` and `rename:<machine>`.
  - Each field reads its draft on creation and writes it on every change. A draft is cleared on a successful send, and pruned when its card or machine is gone or its request changed.
  - The pairing field is never drafted.
- **D36. Unconfirmed keyring changes** (orchestrator decisions, 2026-10-12).
  - The serial worker always awaits a started write or delete to completion (D13).
  - A caller that stops waiting at its deadline gets `secret_store_timeout`, and the machine's secret becomes `SecretStatus::Unknown`.
  - acd-client stops the machine's handle and ends its countdowns: fail closed, with no connection on uncertain credentials. Nothing rolls back on a guess.
  - Recovery is a successful read (the user's `retry_machine`) or a re-pairing. A retry's read runs only after the change settled, because the worker is serial, so a successful read is authoritative and clears `unknown`. A change cannot land after that read.
  - A hung D-Bus call keeps every machine it blocks at `unknown`: queued jobs expire, and nothing reconnects.
  - **Re-pairing from `unknown`.** An `unknown` previous secret counts as "nothing restorable": pairing skips that read's result, writes the new secret, and commits. If the commit fails, it calls `SecretStore::mark_unknown`, leaves the status `unknown` and the handle stopped, and guesses no rollback.
  - The panel chip says "Keyring state unknown. Retry"; the error text for `secret_store_timeout` says the keyring is not responding.
  - The fakes model a slow change (`Hang::UntilReleased`), a hung one (`Hang::Forever`) and failing reads (`fail_reads`). Tests:
    - Task 11: `a_write_past_its_deadline_settles_before_the_next_job_runs`, `a_hung_write_makes_every_later_job_fail_without_running`.
    - Task 12: `a_timed_out_write_leaves_the_key_unknown_until_a_read_after_it_settles`, `a_hung_write_fails_everything_closed_and_nothing_reconnects`, `an_unconfirmed_rollback_is_reported_and_never_guessed`.
    - Task 3: `re_pairing_from_unknown_writes_the_new_secret_and_connects`, `re_pairing_from_unknown_that_cannot_commit_stays_unknown_and_stopped`.
- **D37. Per-key generations** (orchestrator decision, 2026-10-12).
  - Every write or delete raises the key's generation when it starts. These run only under acd-client's edit lock (pair, remove).
  - A read (prefill, refresh, miss) remembers the generation it started under. It publishes its result only if that generation is still current, compared and stored under the cache lock.
  - A refresh that read old credentials before a re-pairing therefore cannot overwrite the new ones. A hook between the worker's reply and publication makes this testable with a barrier (Task 12).
- **D38. Writes use a plain Secret Service session.** secret-service 5.2 keeps its encrypted (DH) session private, and its `create_item` would run prompts. So `write_in` opens its own "plain" session for `CreateItem`.
  - The secret crosses the user's session bus unencrypted. Only processes of the same user can see it, and they can already read the keyring.
  - Reads keep the crate's DH session.
  - Accepted by the orchestrator 2026-10-12: while the keyring is unlocked, any same-user process can already ask the Secret Service for the secret, so an encrypted bus session would add no protection here.
  - If that trade-off is not acceptable, the alternative is to implement DH (hkdf, aes, cbc) in `linux::keyring`. That is a separate change.

## Review answers

Sol reviewed `034d8d0` (`/tmp/sol-omarchy/findings3.md`). The blocking findings and where each is resolved:

1. **Disconnect vs a buffered command.** D31: admission and close share one lock. Tests are in Task 8 (`a_close_during_a_pick_cancels_the_new_action`, `a_command_after_close_is_refused`, `a_late_panel_open_cannot_stick_after_close`).
2. **Timed-out worker jobs running later.** D13 (deadlines, `Expired`, a serial worker that awaits started changes) and D36. Tests are in Task 11 (`a_job_queued_past_its_deadline_never_runs`, `a_write_past_its_deadline_settles_before_the_next_job_runs`) and Task 12 (see D36).
3. **Keyring waits on async threads.** D14: errors are cached and refreshed only when asked; prefill runs on the blocking pool. Tests are in Task 12 (`errors_are_cached_until_refreshed`, `startup_after_prefill_never_waits_on_the_keyring`, `retry_reads_a_cached_failure_again`).
4. **Machine edits racing settings writes.** D3: the edit lock covers every machine and settings writer. Tests are in Task 2 (`a_rename_during_a_re_pairing_is_kept`, `a_settings_write_during_a_removal_never_brings_the_machine_back`).
5. **Missed `Action` literal and Clippy's argument limit.** Task 1 lists every literal (including `crates/acd-voice-client/tests/port.rs:129`) with a grep step, and passes an `Origin` struct instead of more arguments.
6. **Logging.** D33, and the Task 15 test that uses the production init with the `log` bridge.
7. **QML assumptions.** D20 (no `TextEncoder` or `TextDecoder`; `SplitParser` strings), D34 (typed answers bound to their request; IME and field state in key dispatch), and Task 16 (installed-shell discovery before any QML).

Non-blocking points adopted:
- Bounded replies and remembered ids, and fair writer turns (D10, D12).
- Ordered publication (D32).
- A notification batch stops at the first failure (D28).
- No Secret Service call that can prompt outside `unlock`, and the undismissable prompt is documented (D13).
- The lock file is validated (D23).
- Race tests force both orderings and assert exactly one terminal outcome, with no fixed sleeps; blocked fakes are released by guards (Tasks 1, 2, 8, 10).
- Tasks 8, 10 and 15 are split.

Sol re-reviewed `f95da10` (`/tmp/sol-omarchy/findings4.md`). The orchestrator decided the two blockers; the plan implements them:

1. **Ambiguous mutations without safe recovery.** D36: an unconfirmed write or delete makes the secret `unknown`. The machine's handle is stopped, nothing rolls back on a guess, and only a successful read or a re-pairing recovers. Tests:
   - Task 3: `an_unconfirmed_secret_change_stops_the_machine_until_a_read_succeeds`.
   - Task 11 and Task 12: the serial-worker tests listed under D36 (revised after Sol's final check: in-flight changes are awaited, never abandoned).
2. **Refresh overwriting newer credentials.** D37: per-key generations with compare-and-publish. Task 12 tests it with a barrier: `a_refresh_that_read_old_credentials_never_overwrites_a_re_pairing`.

Important points adopted:
- **Prompts on create and delete.** These are now refused: the prompt is dismissed and the change reported `Locked` (D13, D38). The "only unlock prompts" claim is corrected. Tests: Task 11 `a_change_that_needs_a_prompt_is_dismissed_and_reported_locked`, Task 12 `a_change_that_needs_a_prompt_is_refused_as_locked`, Task 14 `only_the_no_prompt_path_lets_a_change_through`.
- **Drafts.** They are kept outside the delegates (D35). Tests: Task 17 "drafts survive an unrelated new snapshot", Task 19 checks.
- **Structural overflow.** It sends `view_overflow` with the Needs-you count, and the panel shows an error state (D9). Tests: Task 4 `needs_you_text_is_cut_further_but_no_request_is_dropped`, Task 9 `a_view_that_cannot_fit_is_reported_and_recovers`.

## Global Constraints

Copied from the spec; every task includes them.

- Socket and lock: "`$XDG_RUNTIME_DIR/acd-inbox.sock` and `acd-inbox.lock`." "Bind, then set mode 0600." "`$XDG_RUNTIME_DIR` must exist, be owned by the user, mode 0700, and not be a symlink."
- "A line over 4 MiB from a view closes that connection." "The service keeps its own lines under 4 MiB."
- "each connection has a bounded queue of 256." "On overflow the service sends `{"type":"outcomes_lost"}` once, drops the queue."
- "a view that does not read for 30 s is disconnected."
- Secret worker timeouts: "read, write, delete 5 s; unlock 120 s".
- Open action: "`omarchy-shell tonibergholm.agentcaller openCard <key>` as an argv (no shell), with a 5 s timeout and its output discarded."
- `hello`: `{"type":"hello","protocol":1,"service":"<version>"}`.
- Command ids: "(string, ≤ 64 bytes, unique per connection)".
- Plugin id `tonibergholm.agentcaller`; manifest `kinds: ["bar-widget"]`, `defaultSection: "right"`, category "AI".
- "`ShellIpc` target `tonibergholm.agentcaller` with `open`, `close`, `toggle`, and `openCard <key>`."
- Keybinding: "Super+Alt+A → `omarchy-shell tonibergholm.agentcaller toggle`"; "asks first, backs up the file, and changes nothing without a yes."
- Unit: "`PartOf=graphical-session.target`, `After=graphical-session.target`, `WantedBy=graphical-session.target`", "`Restart=on-failure`, `RestartSec=2`", `ExecStart=` "`~/.local/bin/acd-inbox serve`".
- Secret items: "attributes `service=agent-caller`, `machine=<key>`, in the default collection."
- "Logging follows CLAUDE.md: agent and message text never reach logs; tags, lengths and hashes only. Raw socket lines are never logged. Secret Service and D-Bus errors are logged by class, not by message text."
- "The service never sends a token or pin to a view. The pairing string goes from view to service only; it is never logged or echoed."
- "The pairing field is a secret: it is cleared after every attempt, never logged, never written to the QML console, and an error never repeats it."
- "The Mac app keeps its behaviour. Shared `acd-client` changes must not change it; its tests stay green."
- Repository rules (`CLAUDE.md`):
  - Do not run `cargo fmt`; keep diffs to the lines you need.
  - Every behaviour change has a test; a test for a fix fails without it.
  - Secrets and tokens never go to stdout in tests.
  - Fail closed.
- Ready means `cargo test --workspace --locked` and `cargo clippy --workspace --all-targets --locked -- -D warnings` pass. An `app/` change also needs `cd app && npm ci && npm run verify` (macOS; on Linux report it pending for CI). `clients/omarchy` changes need `node --test 'clients/omarchy/tests/*.test.mjs'`.
- Linux-only dependencies live under `[target.'cfg(target_os = "linux")'.dependencies]`. A task that adds a dependency runs one build without `--locked` to update `Cargo.lock`, commits the lock file, and runs every later command with `--locked`.

## Review Focus

1. **A view disconnects while a slow command is in flight, or between reading a command and running it.** The view sends `unlock_secrets` (a keyring prompt can wait 120 s) or `pair`, or a buffered `pick`, then the panel reloads. Its countdowns must end `cancelled` at once and nothing is sent. Pinned in Task 8 (`a_close_during_a_pick_cancels_the_new_action`), Task 10 (`a_disconnect_during_a_slow_command_still_cancels_countdowns`) and Task 12 (`a_cancel_is_answered_while_an_unlock_prompt_waits`).
2. **A second view picks a card that is already counting.** The bar and a TUI show the same card. The second pick gets `already_counting`, and the first countdown is untouched. Pinned in Task 10 (`a_second_view_cannot_pick_a_card_that_is_counting`).
3. **An oversized view.** A 5 MiB `raw_screen`, or many cards that together pass 4 MiB. The line stays under 4 MiB, text is cut first, then Done and Working cards are left out with counts, and no Needs-you card is ever dropped. Pinned in Task 4 (`a_5_mib_screen_is_cut_with_a_marker_and_the_request_survives`, `done_cards_go_first_then_working_cards_and_requests_never`, `needs_you_text_is_cut_further_but_no_request_is_dropped`, `a_card_with_a_countdown_is_never_omitted`).
4. **A locked keyring at start.** `build_view` reports a machine without a handle as `auth_failed` with the "Pair this machine…" text. The panel must say "Keyring locked" and offer "Unlock keyring", not "not paired". Pinned in Task 3 (`a_locked_secret_is_shown_as_locked_and_reconnect_creates_the_handle`), Task 12 (`a_locked_keyring_at_start_unlocks_and_reconnects_without_restart`) and Task 17 (`a locked keyring is not shown as not paired`).
5. **A notification action after the notification server restarts.** An old notification id must not open a card when the sender is a new owner, or when the owner changed since the notification was shown. Pinned in Task 13 (`actions_from_a_new_notification_server_are_ignored`).

## File Structure

`acd-client` (modified):

| File | Change |
|---|---|
| `crates/acd-client/src/actions.rs` | `ActionOwner`; `Action.owner`; `Actions::cancel_latest_of`, `cancel_owned`. |
| `crates/acd-client/src/voice.rs` | `ActionEvent.card_key`. |
| `crates/acd-client/src/settings.rs` | `SecretErrorKind`, `SecretError::{locked, timeout, kind}`, `SecretStatus`, `SecretStore::status`. |
| `crates/acd-client/src/view.rs` | `MachineEntryView.secret` (ts skip). |
| `crates/acd-client/src/inbox.rs` | Owned operations (`Origin`), `MachineError`, `EditLock` over every machine and settings writer, secrets outside `inner`, `pair_machine`, `remove_machine_checked`, `reconnect_machine`. |
| `crates/acd-client/tests/inbox.rs` | New tests at the end. |
| `app/src-tauri/src/pairing.rs` | `secret: _` in the test destructure. |
| `crates/acd-voice-client/tests/port.rs` | `owner` in its `Action` literal. |

New crate `crates/acd-inbox`:

| File | Responsibility |
|---|---|
| `Cargo.toml` | Manifest; Linux-only zbus and secret-service; `testing` feature exposes `secrets::fake`. |
| `src/lib.rs` | Module list. |
| `src/protocol.rs` | Wire constants, error codes, outcome mapping, line builders, bounded views and settings (D7 to D9). |
| `src/prefs.rs` | Paths, `service.json` prefs, first-run settings, the `settings` snapshot. |
| `src/startup.rs` | §4.4 socket startup and the peer check. |
| `src/outbox.rs` | Per-connection queue: greeting order, coalescing, fair turns, bounded outcomes and replies (D12). |
| `src/server.rs` | `Service`, `SecretControl`, admission and `close` (D29, D31), registry and ordered publication (D32), accept, reader and writer tasks. |
| `src/commands.rs` | Envelope (D10) and the command table (D11). |
| `src/secrets.rs` | `SecretBackend`, a serial `SecretWorker` with deadlines, `WorkerStore` with cached outcomes and generations, `fake` (D13, D14, D36, D37). |
| `src/notify.rs` | `Reconciler`, `Notifier`, `NotifyBackend`, `ShellOpener` (D15 to D17). |
| `src/linux/mod.rs`, `src/linux/keyring.rs`, `src/linux/dbus.rs` | Secret Service and D-Bus notification backends. |
| `src/serve.rs` | `start_service`, safe logging (D33), Linux `run`. |
| `src/install.rs` | install, uninstall, status, keybinding offer. |
| `src/main.rs` | CLI. |
| `tests/common/mod.rs` | Rig: fake daemon, inbox, service on a temp socket, a socket `View` client. |
| `tests/{startup,admission,server,commands,worker,secrets,notify,serve,logs,install}.rs` | Integration tests. |

Plugin `clients/omarchy/`:

| File | Responsibility |
|---|---|
| `manifest.json` | Plugin manifest. |
| `model.mjs` | Pure view logic in host-neutral JS: line reader, sections, chips, keys, option rows, typed-answer editor, validation, error texts, outcome tracking. |
| `Service.qml` | Socket client, reconnect, request/reply. |
| `BarWidget.qml` | Glyph and count, IPC target, owns the panel. |
| `Panel.qml` | Inbox and settings views. |
| `tests/model.test.mjs`, `tests/host.test.mjs`, `tests/plugin_files.test.mjs` | Node tests. |

Also: `.github/workflows/ci.yml` (node step), `justfile` (Docker build), `docs/research/2026-10-12-omarchy-plugin-api.md` (Task 16), `docs/research/2026-10-12-omarchy-live.md` (Task 22), and spec §5 (Task 4).

## Task order and execution

| # | Task | Execution |
|---|---|---|
| 1 | acd-client: action owners and outcome cards | cloud-ok |
| 2 | acd-client: secrets outside the inbox lock, edit lock, typed machine errors | cloud-ok |
| 3 | acd-client: secret status and `reconnect_machine` | cloud-ok (app check pending for macOS CI) |
| 4 | acd-inbox crate, protocol, bounded views (spec §5 amendment) | cloud-ok |
| 5 | Paths, prefs and the settings snapshot | cloud-ok |
| 6 | Socket startup, lock validation and peer check | cloud-ok |
| 7 | Outbox | cloud-ok |
| 8 | Connections: admission, remembered ids and close | cloud-ok |
| 9 | Server: sockets, readers, writers and broadcasts | cloud-ok |
| 10 | Commands | cloud-ok |
| 11 | Secret worker: deadlines and ambiguous writes | cloud-ok |
| 12 | Cached secret store, reconciliation and unlock | cloud-ok |
| 13 | Notifier | cloud-ok |
| 14 | Linux backends: Secret Service and D-Bus notifications | cloud-ok (Linux session) |
| 15 | `acd-inbox serve` and safe logging | cloud-ok |
| 16 | Omarchy shell API discovery | local |
| 17 | `model.mjs` and node tests | cloud-ok |
| 18 | Plugin manifest, socket client and bar widget | cloud-ok (validated in Task 21) |
| 19 | Plugin panel | cloud-ok (validated in Task 21) |
| 20 | install, uninstall, status | cloud-ok |
| 21 | Build in Docker, install and validate on omarchy | local |
| 22 | Live checks on omarchy | local |

Dependencies:
- Tasks 1 to 3 are sequential.
- Tasks 4 to 15 build in order on them.
- Task 16 is local and can run any time before Task 17, in parallel with Tasks 1 to 15.
- Tasks 17 to 19 need Task 16's note.
- Task 20 needs 17 to 19, because it embeds the plugin files.
- Tasks 21 and 22 come last.

---
### Task 1: acd-client: action owners and outcome cards

**Execution:** cloud-ok

**Files:**
- Modify: `crates/acd-client/src/actions.rs`: `Action` (108 to 118), `Actions` impl, the test helper `action` (284)
- Modify: `crates/acd-voice-client/tests/port.rs:129` (the one `Action` literal outside acd-client)
- Modify: `crates/acd-client/src/voice.rs`: `ActionEvent` (53 to 57)
- Modify: `crates/acd-client/src/inbox.rs` (line numbers from `f18b06f`):
  - imports 1 to 14
  - `emit` 187 to 189 and its 15 call sites
  - `pick_with` 797, `answer_text_with` 874, `reply_with` 937
  - the public wrappers 995 to 1025
  - `cancel` and `escape` 1026 to 1043
- Test: `crates/acd-client/src/actions.rs` (unit), `crates/acd-client/tests/inbox.rs` (append)

**Interfaces:**
- Consumes: nothing new.
- Produces (in `acd_client::actions`):
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)] pub struct ActionOwner(pub u64)` with `pub const DEFAULT: ActionOwner = ActionOwner(0)`.
  - `Action.owner: ActionOwner`.
  - `Actions::cancel_latest_of(&mut self, owner: ActionOwner) -> Option<Action>`.
  - `Actions::cancel_owned(&mut self, owner: ActionOwner) -> Vec<Action>`.
- Produces (in `acd_client::voice`): `ActionEvent { action_id: String, card_key: String, outcome: ActionOutcome }`.
- Produces (on `Inbox`), each with the same rules as its unowned form:
  - `pick_owned(self: &Arc<Self>, owner: ActionOwner, card_key: &str, request_id: &str, option_id: &str, revision: u64) -> Result<String, PickError>`
  - `answer_text_owned(self: &Arc<Self>, owner: ActionOwner, card_key: &str, request_id: &str, revision: u64, text: String) -> Result<String, PickError>`
  - `reply_owned(self: &Arc<Self>, owner: ActionOwner, card_key: &str, text: String) -> Result<String, PickError>`
  - `cancel_owned(&self, owner: ActionOwner) -> usize`: cancels every unsent action of `owner`, each ending `Cancelled`.
  - `escape_owned(&self, owner: ActionOwner) -> bool`: cancels `owner`'s newest.
- Unchanged: the signatures and behaviour of `pick`, `answer_text`, `reply`, the `_guarded` forms, `cancel` and `escape` (D1). `escape()` still takes the newest action of any owner.

- [ ] **Step 1: Write the failing unit test**

In `crates/acd-client/src/actions.rs`, in `mod tests`, add `owner: ActionOwner::DEFAULT,` to the `Action { … }` literal in `fn action` (after `id: id.into(),`), and append:

```rust
    #[test]
    fn owners_cancel_only_their_own() {
        let t = Instant::now();
        let mut a = Actions::default();
        let mut x = action("x", "m/a", 1, t);
        x.owner = ActionOwner(1);
        let mut y = action("y", "m/b", 1, t);
        y.owner = ActionOwner(2);
        let mut z = action("z", "m/c", 1, t);
        z.owner = ActionOwner(1);
        a.add(x).unwrap();
        a.add(y).unwrap();
        a.add(z).unwrap();
        assert_eq!(a.cancel_latest_of(ActionOwner(2)).unwrap().id, "y");
        assert!(a.cancel_latest_of(ActionOwner(2)).is_none());
        let ids: Vec<String> = a.cancel_owned(ActionOwner(1)).into_iter().map(|x| x.id).collect();
        assert_eq!(ids, ["x", "z"]);
        assert!(a.views().is_empty());
    }
```

- [ ] **Step 2: Write the failing integration tests**

Append to `crates/acd-client/tests/inbox.rs` (it already imports `acd_client::{fake::*, inbox::*, settings::*, view::*, voice::*}`; add `use acd_client::actions::ActionOwner;` below line 2). The race tests force each ordering instead of hoping for one, and a sentinel action proves no second terminal outcome follows; no test sleeps for a fixed time.

```rust
fn second_pending_agent() -> Value {
    let mut a = pending_agent();
    a["id"] = "a2".into();
    a["pending"]["id"] = "r2".into();
    a["pending"]["agent_id"] = "a2".into();
    a
}

/// Two answerable cards, `m1/a1` (request `r1`) and `m1/a2` (request `r2`).
async fn setup_two(timing: acd_client::Timing) -> (FakeServer, FakeConn, Arc<Inbox>) {
    let (f, c, ib) = setup_with(timing).await;
    c.send(serde_json::json!({"type":"agent.updated","epoch":"e1","seq":2,"agent":second_pending_agent()}))
        .await;
    view_until(&ib, "two answerable cards", |v| v.cards.iter().filter(|c| c.answerable).count() == 2).await;
    (f, c, ib)
}

/// Starts and cancels a sentinel action on `m1/a2`, then reads outcomes up
/// to the sentinel's: none of them may belong to `id`.
async fn no_more_outcomes_for(ib: &Arc<Inbox>, rx: &mut tokio::sync::broadcast::Receiver<ActionEvent>, id: &str) {
    let sentinel = ib.pick("m1/a2", "r2", "deny", 1).unwrap();
    assert!(ib.cancel(&sentinel));
    loop {
        let e = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.unwrap().unwrap();
        assert_ne!(e.action_id, id, "a second outcome for one action: {:?}", e.outcome);
        if e.action_id == sentinel {
            return;
        }
    }
}

async fn deliver(f: &FakeServer, c: &FakeConn) {
    let frame = answer_frame(f).await;
    c.send(serde_json::json!({"type":"result","id":frame["id"],"ok":true,"outcome":"delivered"})).await;
}

#[tokio::test]
async fn escape_owned_cancels_only_that_owners_newest() {
    let (_f, _c, ib) = setup_two(held()).await;
    let mut rx = ib.action_outcomes();
    let (one, two) = (ActionOwner(1), ActionOwner(2));
    let a = ib.pick_owned(one, "m1/a1", ANSWER_REQ, "allow", 1).unwrap();
    let b = ib.pick_owned(two, "m1/a2", "r2", "allow", 1).unwrap();
    assert!(ib.escape_owned(one));
    assert_eq!(next_outcome(&mut rx, &a).await, ActionOutcome::Cancelled);
    assert!(!ib.escape_owned(one), "owner one has nothing left; two's countdown is not one's");
    view_until(&ib, "one countdown left", |v| v.actions.len() == 1 && v.actions[0].id == b).await;
    assert!(ib.cancel(&b), "a card's cancel control cancels any owner's action");
    assert_eq!(next_outcome(&mut rx, &b).await, ActionOutcome::Cancelled);
}

#[tokio::test]
async fn cancel_owned_ends_every_countdown_of_that_owner_and_nothing_fires() {
    let (f, _c, ib) = setup_two(held()).await;
    let mut rx = ib.action_outcomes();
    let a = ib.pick_owned(ActionOwner(7), "m1/a1", ANSWER_REQ, "allow", 1).unwrap();
    let b = ib.pick_owned(ActionOwner(7), "m1/a2", "r2", "deny", 1).unwrap();
    assert_eq!(ib.cancel_owned(ActionOwner(7)), 2);
    assert_eq!(ib.cancel_owned(ActionOwner(7)), 0);
    assert_eq!(next_outcome(&mut rx, &a).await, ActionOutcome::Cancelled);
    assert_eq!(next_outcome(&mut rx, &b).await, ActionOutcome::Cancelled);
    ib.expire_countdowns_for_test();
    view_until(&ib, "no countdowns", |v| v.actions.is_empty()).await;
    assert_eq!(answers(&f), 0);
}

#[tokio::test]
async fn escape_without_an_owner_still_takes_the_newest_of_any_owner() {
    let (_f, _c, ib) = setup_two(held()).await;
    ib.pick_owned(ActionOwner(1), "m1/a1", ANSWER_REQ, "allow", 1).unwrap();
    let newest = ib.pick_owned(ActionOwner(2), "m1/a2", "r2", "allow", 1).unwrap();
    let mut rx = ib.action_outcomes();
    assert!(ib.escape());
    assert_eq!(next_outcome(&mut rx, &newest).await, ActionOutcome::Cancelled);
}

#[tokio::test]
async fn outcomes_name_the_card() {
    let (_f, _c, ib) = setup_with(held()).await;
    let mut rx = ib.action_outcomes();
    let id = ib.pick("m1/a1", ANSWER_REQ, "allow", 1).unwrap();
    assert!(ib.cancel(&id));
    let event = rx.recv().await.unwrap();
    assert_eq!((event.action_id.as_str(), event.card_key.as_str()), (id.as_str(), "m1/a1"));
}

#[tokio::test]
async fn escape_owned_before_expiry_cancels_once_and_sends_nothing() {
    let (f, _c, ib) = setup_two(held()).await;
    let mut rx = ib.action_outcomes();
    let owner = ActionOwner(3);
    let id = ib.pick_owned(owner, "m1/a1", ANSWER_REQ, "allow", 1).unwrap();
    assert!(ib.escape_owned(owner));
    ib.expire_countdowns_for_test();
    assert_eq!(next_outcome(&mut rx, &id).await, ActionOutcome::Cancelled);
    no_more_outcomes_for(&ib, &mut rx, &id).await;
    assert_eq!(answers(&f), 0);
}

#[tokio::test]
async fn escape_owned_after_expiry_is_refused_and_the_send_ends_once() {
    let (f, c, ib) = setup_two(held()).await;
    let mut rx = ib.action_outcomes();
    let owner = ActionOwner(3);
    let id = ib.pick_owned(owner, "m1/a1", ANSWER_REQ, "allow", 1).unwrap();
    ib.expire_countdowns_for_test();
    assert_eq!(next_outcome(&mut rx, &id).await, ActionOutcome::Fired);
    assert!(!ib.escape_owned(owner), "too late once fired");
    deliver(&f, &c).await;
    assert_eq!(next_outcome(&mut rx, &id).await, ActionOutcome::Delivered);
    no_more_outcomes_for(&ib, &mut rx, &id).await;
}

#[tokio::test]
async fn cancel_owned_before_expiry_cancels_once_and_sends_nothing() {
    let (f, _c, ib) = setup_two(held()).await;
    let mut rx = ib.action_outcomes();
    let owner = ActionOwner(4);
    let id = ib.pick_owned(owner, "m1/a1", ANSWER_REQ, "allow", 1).unwrap();
    assert_eq!(ib.cancel_owned(owner), 1);
    ib.expire_countdowns_for_test();
    assert_eq!(next_outcome(&mut rx, &id).await, ActionOutcome::Cancelled);
    no_more_outcomes_for(&ib, &mut rx, &id).await;
    assert_eq!(answers(&f), 0);
}

#[tokio::test]
async fn cancel_owned_after_expiry_cancels_nothing_and_the_send_ends_once() {
    let (f, c, ib) = setup_two(held()).await;
    let mut rx = ib.action_outcomes();
    let owner = ActionOwner(4);
    let id = ib.pick_owned(owner, "m1/a1", ANSWER_REQ, "allow", 1).unwrap();
    ib.expire_countdowns_for_test();
    assert_eq!(next_outcome(&mut rx, &id).await, ActionOutcome::Fired);
    assert_eq!(ib.cancel_owned(owner), 0, "a disconnect after the countdown cannot stop it");
    deliver(&f, &c).await;
    assert_eq!(next_outcome(&mut rx, &id).await, ActionOutcome::Delivered);
    no_more_outcomes_for(&ib, &mut rx, &id).await;
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p acd-client --locked --lib actions::tests::owners_cancel_only_their_own`
Expected: compile error, `cannot find type ActionOwner`.

Run: `cargo test -p acd-client --locked --test inbox owned`
Expected: compile error, `no method named pick_owned`.

- [ ] **Step 4: Implement owners in `actions.rs`**

Above `#[derive(Clone, Debug)] pub struct Action`, add:

```rust
/// Who started an action (omarchy spec §6). The Mac app and voice use
/// `DEFAULT`; acd-inbox gives each socket connection its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ActionOwner(pub u64);

impl ActionOwner {
    pub const DEFAULT: ActionOwner = ActionOwner(0);
}
```

In `pub struct Action`, after `pub id: String,`, add `pub owner: ActionOwner,`. In `impl Actions`, after `cancel_latest`, add:

```rust
    /// The newest unsent action of `owner`.
    pub fn cancel_latest_of(&mut self, owner: ActionOwner) -> Option<Action> {
        let i = self.items.iter().rposition(|a| a.owner == owner)?;
        Some(self.items.remove(i))
    }
    /// Every unsent action of `owner`, oldest first.
    pub fn cancel_owned(&mut self, owner: ActionOwner) -> Vec<Action> {
        self.remove_where(|a| a.owner == owner)
    }
```

- [ ] **Step 5: Add the card to `ActionEvent`**

In `crates/acd-client/src/voice.rs`:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionEvent {
    pub action_id: String,
    /// The card the action was started on (omarchy spec §5.3).
    pub card_key: String,
    pub outcome: ActionOutcome,
}
```

- [ ] **Step 6: Thread owners through `inbox.rs`**

Change the import on line 3 to `actions::{Action, ActionKind, ActionOwner, Actions, Semantics, TextSemantics, check_reason, notice_for},`.

Replace `emit`:

```rust
    fn emit(&self, action: &Action, outcome: ActionOutcome) {
        let _ = self.outcomes_tx.send(ActionEvent {
            action_id: action.id.clone(),
            card_key: action.card_key.clone(),
            outcome,
        });
    }
```

Update all 15 call sites, which pass `&x.id`, to pass `&x`:

```bash
perl -pi -e 's/emit\(&(\w+)\.id, /emit(&$1, /g' crates/acd-client/src/inbox.rs
grep -n 'emit(&' crates/acd-client/src/inbox.rs   # every match now passes an Action, none ends in .id
```

Find every `Action` literal first; each needs the new field:

```bash
grep -rnE '(^|[^A-Za-z])Action \{' crates app/src-tauri/src | grep -v 'struct Action'
```

Expected at `f18b06f`:
- `crates/acd-client/src/actions.rs:285`, the test helper; Step 1 adds `owner`.
- `crates/acd-client/src/inbox.rs:851`, `916` and `973`, in `pick_with`, `answer_text_with` and `reply_with`.
- `crates/acd-voice-client/tests/port.rs:129`. Add `owner: acd_client::actions::ActionOwner::DEFAULT,` after `id: "act".into(),`.

The grep must show no other site. If it does, add `owner` there too.

So the three builders stay within Clippy's argument limit, they take one `Origin` instead of a growing list. Add above `impl Inbox`:

```rust
/// Who asks for an action, and what it was planned on (voice guards).
#[derive(Clone, Copy)]
struct Origin<'a> {
    owner: ActionOwner,
    expect: Option<&'a Guard>,
}

impl Origin<'_> {
    const KEYS: Origin<'static> = Origin { owner: ActionOwner::DEFAULT, expect: None };
    fn owned(owner: ActionOwner) -> Origin<'static> {
        Origin { owner, expect: None }
    }
}
```

The signatures become:

```rust
    fn pick_with(self: &Arc<Self>, origin: Origin<'_>, card_key: &str, request_id: &str, option_id: &str, revision: u64) -> Result<String, PickError> {
    fn answer_text_with(self: &Arc<Self>, origin: Origin<'_>, card_key: &str, request_id: &str, revision: u64, text: String) -> Result<String, PickError> {
    fn reply_with(self: &Arc<Self>, origin: Origin<'_>, card_key: &str, text: String) -> Result<String, PickError> {
```

In their bodies:
- Replace `expect` with `origin.expect`. It appears in the `guard(&inner.settings, &state, card_key, expect)?` calls and in `reply_with`'s `match expect`.
- Add `owner: origin.owner,` after `id: id.clone(),` in each `Action { … }` literal.

Replace the public wrappers (995 to 1025) and `cancel`/`escape` (1026 to 1043) with:

```rust
    pub fn pick(self: &Arc<Self>, card_key: &str, request_id: &str, option_id: &str, revision: u64) -> Result<String, PickError> {
        self.pick_with(Origin::KEYS, card_key, request_id, option_id, revision)
    }
    /// `pick` for one owner (omarchy spec §6).
    pub fn pick_owned(self: &Arc<Self>, owner: ActionOwner, card_key: &str, request_id: &str, option_id: &str, revision: u64) -> Result<String, PickError> {
        self.pick_with(Origin::owned(owner), card_key, request_id, option_id, revision)
    }
    /// `pick` on the request `guard` names, refused unless connection,
    /// agent and request still match (voice spec §5.6).
    pub fn pick_guarded(self: &Arc<Self>, card_key: &str, option_id: &str, guard: &Guard) -> Result<String, PickError> {
        let Some((request_id, revision)) = guard.request.clone() else {
            return Err(PickError::StaleRequest);
        };
        self.pick_with(Origin { owner: ActionOwner::DEFAULT, expect: Some(guard) }, card_key, &request_id, option_id, revision)
    }
    /// A typed answer to a free-text request, with the same undo countdown as
    /// `pick`. The text is trimmed; blank, oversize (16 KiB) or control-char
    /// text is `BadAnswerText`.
    pub fn answer_text(self: &Arc<Self>, card_key: &str, request_id: &str, revision: u64, text: String) -> Result<String, PickError> {
        self.answer_text_with(Origin::KEYS, card_key, request_id, revision, text)
    }
    /// `answer_text` for one owner.
    pub fn answer_text_owned(self: &Arc<Self>, owner: ActionOwner, card_key: &str, request_id: &str, revision: u64, text: String) -> Result<String, PickError> {
        self.answer_text_with(Origin::owned(owner), card_key, request_id, revision, text)
    }
    pub fn answer_text_guarded(self: &Arc<Self>, card_key: &str, text: String, guard: &Guard) -> Result<String, PickError> {
        let Some((request_id, revision)) = guard.request.clone() else {
            return Err(PickError::StaleRequest);
        };
        self.answer_text_with(Origin { owner: ActionOwner::DEFAULT, expect: Some(guard) }, card_key, &request_id, revision, text)
    }
    pub fn reply(self: &Arc<Self>, card_key: &str, text: String) -> Result<String, PickError> {
        self.reply_with(Origin::KEYS, card_key, text)
    }
    /// `reply` for one owner.
    pub fn reply_owned(self: &Arc<Self>, owner: ActionOwner, card_key: &str, text: String) -> Result<String, PickError> {
        self.reply_with(Origin::owned(owner), card_key, text)
    }
    /// `reply`, refused unless connection, agent and request (including
    /// "no request") still match what the step was planned on.
    pub fn reply_guarded(self: &Arc<Self>, card_key: &str, text: String, guard: &Guard) -> Result<String, PickError> {
        self.reply_with(Origin { owner: ActionOwner::DEFAULT, expect: Some(guard) }, card_key, text)
    }
    pub fn cancel(&self, id: &str) -> bool {
        let removed = self.inner.lock().unwrap().actions.cancel(id);
        let Some(action) = removed else {
            return false;
        };
        self.emit(&action, ActionOutcome::Cancelled);
        self.wake();
        true
    }
    /// Cancels the newest unsent action of any owner (the Mac panel's Esc).
    pub fn escape(&self) -> bool {
        let removed = self.inner.lock().unwrap().actions.cancel_latest();
        let Some(action) = removed else {
            return false;
        };
        self.emit(&action, ActionOutcome::Cancelled);
        self.wake();
        true
    }
    /// Cancels `owner`'s newest unsent action (Esc in one view).
    pub fn escape_owned(&self, owner: ActionOwner) -> bool {
        let removed = self.inner.lock().unwrap().actions.cancel_latest_of(owner);
        let Some(action) = removed else {
            return false;
        };
        self.emit(&action, ActionOutcome::Cancelled);
        self.wake();
        true
    }
    /// Cancels every unsent action of `owner` (a view disconnected); each
    /// ends `Cancelled`. Returns how many.
    pub fn cancel_owned(&self, owner: ActionOwner) -> usize {
        let removed = self.inner.lock().unwrap().actions.cancel_owned(owner);
        for action in &removed {
            self.emit(action, ActionOutcome::Cancelled);
        }
        if !removed.is_empty() {
            self.wake();
        }
        removed.len()
    }
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test -p acd-client --locked`
Expected: PASS, including every existing test in `tests/inbox.rs` and `tests/bindings.rs` (the bindings do not change).

Run: `cargo test -p acd-voice-client --locked`
Expected: PASS (its `tests/port.rs` literal now has `owner`).

- [ ] **Step 8: Lint and commit**

Run: `cargo clippy --workspace --all-targets --locked -- -D warnings`
Expected: no warnings.

```bash
git add crates/acd-client/src/actions.rs crates/acd-client/src/voice.rs crates/acd-client/src/inbox.rs crates/acd-client/tests/inbox.rs crates/acd-voice-client/tests/port.rs
git commit -m "acd-client: action owners, owned cancel and escape, card in outcomes"
```

---

### Task 2: acd-client: secrets outside the inbox lock, edit lock, typed machine errors

**Execution:** cloud-ok

**Files:**
- Modify: `crates/acd-client/src/inbox.rs`:
  - imports
  - `Inbox` struct and `new` (108 to 172)
  - `machines` (221 to 250)
  - `import_pairing` (251 to 326)
  - `remove_machine` (355 to 406)
  - `start` (488 to 539)
  - `spawn_for` (1165 to 1207)
- Test: `crates/acd-client/tests/inbox.rs` (append)

**Interfaces:**
- Consumes: Task 1's `emit(&Action, …)`.
- Produces (in `acd_client::inbox`):

```rust
#[derive(Debug, thiserror::Error)]
pub enum MachineError {
    #[error("Unknown machine")] UnknownMachine,
    #[error("That is not an acd pairing line. Run `acd pair` on the machine and copy its output.")] BadPairing,
    #[error("This machine is not paired")] NotPaired,
    #[error("Could not read machine credentials")] SecretRead(SecretError),
    #[error("Could not save machine credentials")] SecretWrite(SecretError),
    #[error("{0}")] SecretDelete(SecretError),
    #[error("{0}")] Settings(String),
    #[error("Machine pairing recovery failed")] PairRecoveryFailed,
    #[error("Machine removal recovery failed")] RemoveRecoveryFailed,
}
```

- Produces (on `Inbox`):
  - `pair_machine(self: &Arc<Self>, line: &str) -> Result<MachineEntry, MachineError>`.
  - `remove_machine_checked(&self, key: &str) -> Result<(), MachineError>`.
  - Under `feature = "testing"`: `edit_waiters_for_test(&self) -> usize`.
  - `import_pairing` and `remove_machine` become wrappers that return `e.to_string()`: the same texts as today.
- Rule (§4.3, D3): no `SecretStore` call runs while `inner` is locked; that covers `machines`, `pair_machine`, `remove_machine_checked` and `start`. Every machine or settings-file change (pair, rename, remove, reconnect, `set_hotkey`, `set_show_subagents`, `set_show_hook_only`) holds the edit lock for its whole length, so no writer saves over another's result. The pairing secret is stored first and the machine committed second. A failed commit deletes a new machine's secret or restores a re-paired machine's previous one.

- [ ] **Step 1: Write the failing tests**

Append to `crates/acd-client/tests/inbox.rs`. Every blocked store call is released by a guard's `Drop`, so a failing assertion cannot leave a thread blocked and hang runtime teardown. "The second caller is waiting" is observed through `Inbox::edit_waiters_for_test()`, not a sleep.

```rust
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};

/// A secret store whose reads, writes or deletes wait while held, as a
/// keyring prompt does.
#[derive(Default)]
struct Gate {
    inner: MemorySecrets,
    hold_get: AtomicBool,
    hold_set: AtomicBool,
    hold_delete: AtomicBool,
    entered: AtomicUsize,
}

/// Holds one of the gate's operations until dropped, panics included.
struct Hold<'a>(&'a AtomicBool);

impl Drop for Hold<'_> {
    fn drop(&mut self) {
        self.0.store(false, SeqCst);
    }
}

impl Gate {
    fn hold<'a>(&'a self, which: &'a AtomicBool) -> Hold<'a> {
        which.store(true, SeqCst);
        Hold(which)
    }
    fn wait(&self, hold: &AtomicBool) {
        if hold.load(SeqCst) {
            self.entered.fetch_add(1, SeqCst);
            while hold.load(SeqCst) {
                std::thread::yield_now();
            }
        }
    }
}

impl SecretStore for Gate {
    fn get(&self, key: &str) -> Result<Option<String>, SecretError> {
        self.wait(&self.hold_get);
        self.inner.get(key)
    }
    fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
        self.wait(&self.hold_set);
        self.inner.set(key, value)
    }
    fn delete(&self, key: &str) -> Result<(), SecretError> {
        self.wait(&self.hold_delete);
        self.inner.delete(key)
    }
}

/// Polls `cond` (no fixed sleep) for up to 5 s.
async fn until(what: &str, cond: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !cond() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

fn box_secret(f: &FakeServer) -> String {
    MachineSecret { token: common::TOKEN.into(), pin: f.pin() }.to_json()
}

fn box_settings(f: &FakeServer) -> Settings {
    Settings {
        client_id: settings_client_id().into(),
        hotkey: DEFAULT_HOTKEY.into(),
        show_idle: false,
        show_subagents: false,
        show_hook_only: false,
        machines: vec![MachineEntry {
            key: "m1".into(),
            name: "Box".into(),
            endpoint: EndpointConfig::Tcp { host: "127.0.0.1".into(), port: f.port() },
        }],
    }
}

fn options(timing: acd_client::Timing) -> InboxOptions {
    InboxOptions { timing, settings_path: None, client_name: "t".into(), client_version: "0".into() }
}

/// `setup_with` over a `Gate` store, optionally with a settings file.
async fn setup_gated(
    timing: acd_client::Timing,
    settings_path: Option<std::path::PathBuf>,
) -> (FakeServer, FakeConn, Arc<Inbox>, Arc<Gate>) {
    let f = FakeServer::start(common::TOKEN, Identity::Paired).await;
    let gate = Arc::new(Gate::default());
    gate.inner.set("m1", &box_secret(&f)).unwrap();
    let ib = Inbox::new(box_settings(&f), gate.clone(), InboxOptions { settings_path, ..options(timing) });
    ib.start();
    let c = f.next_connection().await;
    c.send(serde_json::json!({"type":"snapshot","epoch":"e1","seq":1,"agents":[pending_agent()]})).await;
    view_until(&ib, "answerable", |v| v.cards.iter().any(|c| c.answerable)).await;
    (f, c, ib, gate)
}

/// A pairing line for a second machine on the fake daemon's port.
fn localhost_line(f: &FakeServer) -> String {
    format!("acd://localhost:{}?pin={}#{}", f.port(), f.pin(), common::TOKEN)
}

/// The same machine as `m1`, re-paired.
fn m1_line(f: &FakeServer) -> String {
    format!("acd://127.0.0.1:{}?pin={}#{}", f.port(), f.pin(), common::TOKEN)
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::time::timeout(Duration::from_secs(2), tokio::task::spawn_blocking(f))
        .await
        .expect("the call waited for the secret store")
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_blocked_secret_write_never_stops_a_cancel_or_a_listing() {
    let (f, _c, ib, gate) = setup_gated(held(), None).await;
    let id = ib.pick("m1/a1", ANSWER_REQ, "allow", 1).unwrap();
    let pairing = {
        let _hold = gate.hold(&gate.hold_set);
        let pairing = {
            let (ib, line) = (ib.clone(), localhost_line(&f));
            tokio::task::spawn_blocking(move || ib.pair_machine(&line).map(|e| e.key))
        };
        until("the secret write", || gate.entered.load(SeqCst) == 1).await;
        let ib2 = ib.clone();
        assert!(blocking(move || ib2.cancel(&id)).await);
        let ib2 = ib.clone();
        assert_eq!(blocking(move || ib2.machines().len()).await, 1, "not committed before its secret is stored");
        pairing
    };
    assert!(pairing.await.unwrap().is_ok());
    assert_eq!(ib.settings().machines.len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_countdown_fires_while_a_secret_write_is_blocked() {
    let (f, _c, ib, gate) = setup_gated(common::fast(), None).await;
    let pairing = {
        let _hold = gate.hold(&gate.hold_set);
        let pairing = {
            let (ib, line) = (ib.clone(), localhost_line(&f));
            tokio::task::spawn_blocking(move || ib.pair_machine(&line).map(|e| e.key))
        };
        until("the secret write", || gate.entered.load(SeqCst) == 1).await;
        ib.pick("m1/a1", ANSWER_REQ, "allow", 1).unwrap();
        answer_frame(&f).await;
        pairing
    };
    pairing.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_blocked_secret_read_during_removal_never_stops_a_cancel() {
    let (_f, _c, ib, gate) = setup_gated(held(), None).await;
    let id = ib.pick("m1/a1", ANSWER_REQ, "allow", 1).unwrap();
    let removal = {
        let _hold = gate.hold(&gate.hold_get);
        let removal = {
            let ib = ib.clone();
            tokio::task::spawn_blocking(move || ib.remove_machine_checked("m1"))
        };
        until("the secret read", || gate.entered.load(SeqCst) == 1).await;
        let ib2 = ib.clone();
        assert!(blocking(move || ib2.cancel(&id)).await);
        removal
    };
    removal.await.unwrap().unwrap();
    assert!(ib.settings().machines.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_reads_secrets_outside_the_inbox_lock() {
    let f = FakeServer::start(common::TOKEN, Identity::Paired).await;
    let gate = Arc::new(Gate::default());
    gate.inner.set("m1", &box_secret(&f)).unwrap();
    let ib = Inbox::new(box_settings(&f), gate.clone(), options(common::fast()));
    let starting = {
        let _hold = gate.hold(&gate.hold_get);
        let starting = {
            let ib = ib.clone();
            tokio::task::spawn_blocking(move || ib.start())
        };
        until("start's secret read", || gate.entered.load(SeqCst) == 1).await;
        let ib2 = ib.clone();
        assert_eq!(blocking(move || ib2.settings()).await.machines.len(), 1);
        starting
    };
    starting.await.unwrap();
    f.next_connection().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_pairings_of_one_host_leave_one_machine() {
    let f = FakeServer::start(common::TOKEN, Identity::Paired).await;
    let gate = Arc::new(Gate::default());
    let ib = empty_inbox(gate.clone(), None);
    let line = m1_line(&f);
    let (first, second) = {
        let _hold = gate.hold(&gate.hold_set);
        let first = {
            let (ib, line) = (ib.clone(), line.clone());
            tokio::task::spawn_blocking(move || ib.pair_machine(&line).map(|e| e.key))
        };
        until("the first secret write", || gate.entered.load(SeqCst) == 1).await;
        let second = {
            let (ib, line) = (ib.clone(), line.clone());
            tokio::task::spawn_blocking(move || ib.pair_machine(&line).map(|e| e.key))
        };
        until("the second pairing to wait", || ib.edit_waiters_for_test() == 1).await;
        assert_eq!(gate.entered.load(SeqCst), 1, "the second pairing has not reached the store");
        (first, second)
    };
    let (a, b) = (first.await.unwrap().unwrap(), second.await.unwrap().unwrap());
    assert_eq!(a, b, "the second pairing re-pairs the machine the first one added");
    assert_eq!(ib.settings().machines.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rename_during_a_re_pairing_is_kept() {
    let (f, _c, ib, gate) = setup_gated(held(), None).await;
    let (pairing, rename) = {
        let _hold = gate.hold(&gate.hold_set);
        let pairing = {
            let (ib, line) = (ib.clone(), m1_line(&f));
            tokio::task::spawn_blocking(move || ib.pair_machine(&line).map(|e| e.key))
        };
        until("the re-pairing's secret write", || gate.entered.load(SeqCst) == 1).await;
        let rename = {
            let ib = ib.clone();
            tokio::task::spawn_blocking(move || ib.rename_machine("m1", "Renamed"))
        };
        until("the rename to wait for the edit lock", || ib.edit_waiters_for_test() == 1).await;
        assert_eq!(ib.settings().machines[0].name, "Box");
        (pairing, rename)
    };
    assert_eq!(pairing.await.unwrap().unwrap(), "m1");
    rename.await.unwrap().unwrap();
    assert_eq!(ib.settings().machines[0].name, "Renamed");
    // Task 3: ib.reconnect_machine("m1").unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_settings_write_during_a_removal_never_brings_the_machine_back() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("settings.json");
    let (_f, _c, ib, gate) = setup_gated(held(), Some(path.clone())).await;
    let (removal, toggle) = {
        let _hold = gate.hold(&gate.hold_delete);
        let removal = {
            let ib = ib.clone();
            tokio::task::spawn_blocking(move || ib.remove_machine_checked("m1"))
        };
        until("the secret delete", || gate.entered.load(SeqCst) == 1).await;
        let toggle = {
            let ib = ib.clone();
            tokio::task::spawn_blocking(move || ib.set_show_subagents(true))
        };
        until("the settings write to wait for the edit lock", || ib.edit_waiters_for_test() == 1).await;
        (removal, toggle)
    };
    removal.await.unwrap().unwrap();
    toggle.await.unwrap().unwrap();
    let saved: Settings = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(saved.machines.is_empty(), "the removed machine came back in the file");
    assert!(saved.show_subagents);
    assert_eq!(ib.settings(), saved);
}

/// Records every store call; keys only, never values.
#[derive(Default)]
struct Recording {
    inner: MemorySecrets,
    ops: std::sync::Mutex<Vec<(&'static str, String)>>,
}

impl SecretStore for Recording {
    fn get(&self, key: &str) -> Result<Option<String>, SecretError> {
        self.ops.lock().unwrap().push(("get", key.into()));
        self.inner.get(key)
    }
    fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
        self.ops.lock().unwrap().push(("set", key.into()));
        self.inner.set(key, value)
    }
    fn delete(&self, key: &str) -> Result<(), SecretError> {
        self.ops.lock().unwrap().push(("delete", key.into()));
        self.inner.delete(key)
    }
}

#[tokio::test]
async fn a_failed_commit_deletes_a_new_machines_secret() {
    let f = FakeServer::start(common::TOKEN, Identity::Paired).await;
    let d = tempfile::tempdir().unwrap();
    let blocker = d.path().join("file");
    std::fs::write(&blocker, "block").unwrap();
    let store = Arc::new(Recording::default());
    let ib = empty_inbox(store.clone(), Some(blocker.join("settings.json")));
    assert!(matches!(ib.pair_machine(&m1_line(&f)), Err(MachineError::Settings(_))));
    let ops = store.ops.lock().unwrap().clone();
    let kinds: Vec<&str> = ops.iter().map(|(op, _)| *op).collect();
    assert_eq!(kinds, ["get", "set", "delete"]);
    assert!(ops.iter().all(|(_, key)| key == &ops[0].1));
    assert_eq!(store.inner.get(&ops[0].1).unwrap(), None);
    assert!(ib.settings().machines.is_empty());
}

#[tokio::test]
async fn machine_errors_are_typed_and_keep_the_old_texts() {
    let ib = empty_inbox(Arc::new(MemorySecrets::default()), None);
    let bad = ib.pair_machine("hello #tok").unwrap_err();
    assert!(matches!(bad, MachineError::BadPairing));
    assert_eq!(ib.import_pairing("hello #tok").unwrap_err(), bad.to_string());
    assert_eq!(
        bad.to_string(),
        "That is not an acd pairing line. Run `acd pair` on the machine and copy its output."
    );
    assert!(matches!(ib.remove_machine_checked("nope"), Err(MachineError::UnknownMachine)));
    assert_eq!(ib.remove_machine("nope").unwrap_err(), "Unknown machine");
}
```

`a_rename_during_a_re_pairing_is_kept` calls `reconnect_machine`, which Task 3 adds. Until then, leave that line commented with `// Task 3:` and enable it in Task 3 Step 2.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p acd-client --locked --test inbox -- secret machine start_reads concurrent a_failed_commit rename settings_write`
Expected: compile errors, `no method named pair_machine`, `no method named edit_waiters_for_test` and `cannot find type MachineError`.

- [ ] **Step 3: Add `MachineError`, the edit lock and the split spawn helpers**

In `inbox.rs`:

1. Change the settings import to `settings::{EndpointConfig, MachineEntry, MachineSecret, SecretError, SecretStore, Settings},`.
2. Add `MachineError` from the Interfaces block above `struct Machine`, with this doc comment:

   ```rust
   /// Why a machine operation failed. `Display` keeps the texts the Mac app
   /// shows today.
   ```

3. Above `struct Machine`, add the edit lock:

   ```rust
   /// Serializes every change to machines or to the saved settings: pair,
   /// rename, remove, reconnect and each settings write (plan D3).
   /// SecretStore calls may run under it; they never run under `inner`.
   /// Timers, cancels, views, `machines()` and `settings()` never take it.
   #[derive(Default)]
   struct EditLock {
       lock: Mutex<()>,
       waiting: AtomicUsize,
   }

   impl EditLock {
       fn lock(&self) -> std::sync::MutexGuard<'_, ()> {
           self.waiting.fetch_add(1, Ordering::SeqCst);
           let guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
           self.waiting.fetch_sub(1, Ordering::SeqCst);
           guard
       }
   }
   ```

   Add `AtomicUsize` to the `std::sync::atomic` import. In `pub struct Inbox`, after `inner: Mutex<Inner>,`, add `edits: EditLock,`; in `Inbox::new`, add `edits: EditLock::default(),`. Add the test hook next to `expire_countdowns_for_test`:

   ```rust
       /// Test hook: callers blocked on the edit lock right now.
       #[cfg(feature = "testing")]
       pub fn edit_waiters_for_test(&self) -> usize {
           self.edits.waiting.load(Ordering::SeqCst)
       }
   ```

Replace `spawn_for` (1165 to 1207) with:

```rust
/// Where and how to connect to `entry`. Reads the SecretStore for a TCP
/// machine, so never call it while holding `inner`.
fn connect_info(entry: &MachineEntry, secrets: &Arc<dyn SecretStore>) -> Option<(Endpoint, conn::Credential)> {
    match &entry.endpoint {
        EndpointConfig::Local => {
            let dir = crate::local::default_config_dir()?;
            Some((Endpoint::Local { config_dir: dir.clone() }, conn::Credential::LocalFile(dir)))
        }
        EndpointConfig::Tcp { host, port } => {
            let raw = secrets.get(&entry.key).ok()??;
            let secret = MachineSecret::from_json(&raw).ok()?;
            Some(tcp_info(host, *port, &secret))
        }
    }
}

fn tcp_info(host: &str, port: u16, secret: &MachineSecret) -> (Endpoint, conn::Credential) {
    (
        Endpoint::Tcp { host: host.to_owned(), port, pin: secret.pin.clone() },
        conn::Credential::Token(secret.token.clone()),
    )
}

fn spawn_with(
    entry: &MachineEntry,
    (endpoint, credential): (Endpoint, conn::Credential),
    opts: &InboxOptions,
    instance: u64,
    client_id: &str,
) -> MachineHandle {
    conn::spawn_machine(conn::MachineConfig {
        key: entry.key.clone(),
        instance,
        endpoint,
        credential,
        client: acd_protocol::ClientInfo {
            id: client_id.to_owned(),
            name: opts.client_name.clone(),
            version: opts.client_version.clone(),
        },
        timing: opts.timing.clone(),
    })
}
```

- [ ] **Step 4: Move every secret call out of `inner`**

Replace `machines` with:

```rust
    pub fn machines(&self) -> Vec<MachineEntryView> {
        // The SecretStore is never called under `inner` (omarchy spec §4.3).
        let entries = self.inner.lock().unwrap().settings.machines.clone();
        entries
            .iter()
            .map(|entry| {
                let pairing = match &entry.endpoint {
                    EndpointConfig::Local => PairingState::Paired,
                    EndpointConfig::Tcp { .. } => match self.secrets.get(&entry.key) {
                        Ok(None) => PairingState::NotPaired,
                        Err(_) => PairingState::Unreadable,
                        Ok(Some(raw)) => {
                            if MachineSecret::from_json(&raw).is_ok() {
                                PairingState::Paired
                            } else {
                                PairingState::Unreadable
                            }
                        }
                    },
                };
                MachineEntryView {
                    key: entry.key.clone(),
                    name: entry.name.clone(),
                    endpoint: entry.endpoint.clone(),
                    pairing,
                }
            })
            .collect()
    }
```

Replace `import_pairing` with:

```rust
    pub fn import_pairing(self: &Arc<Self>, line: &str) -> Result<MachineEntry, String> {
        self.pair_machine(line).map_err(|e| e.to_string())
    }

    /// Pairs a machine from an `acd pair` line. The secret is stored first,
    /// outside `inner`; the machine is then committed under it. A failed
    /// commit deletes a new machine's secret or restores a re-paired
    /// machine's previous one (omarchy spec §4.3, §9).
    pub fn pair_machine(self: &Arc<Self>, line: &str) -> Result<MachineEntry, MachineError> {
        let pairing = Pairing::parse(line).map_err(|_| MachineError::BadPairing)?;
        let _edit = self.edits.lock();
        let existing = self
            .inner
            .lock()
            .unwrap()
            .settings
            .machines
            .iter()
            .find(|m| matches!(&m.endpoint, EndpointConfig::Tcp { host, port } if host == &pairing.host && *port == pairing.port))
            .cloned();
        let entry = existing.unwrap_or_else(|| MachineEntry {
            key: uuid::Uuid::new_v4().to_string(),
            name: default_machine_name(&pairing.host),
            endpoint: EndpointConfig::Tcp { host: pairing.host.clone(), port: pairing.port },
        });
        let secret = MachineSecret { token: pairing.token, pin: pairing.pin };
        let previous_secret = self.secrets.get(&entry.key).map_err(MachineError::SecretRead)?;
        self.secrets
            .set(&entry.key, &secret.to_json())
            .map_err(MachineError::SecretWrite)?;
        let info = tcp_info(&pairing.host, pairing.port, &secret);
        let mut inner = self.inner.lock().unwrap();
        let mut candidate = inner.settings.clone();
        candidate.machines.retain(|m| m.key != entry.key);
        candidate.machines.push(entry.clone());
        if let Some(path) = &self.opts.settings_path
            && let Err(error) = candidate.save(path)
        {
            drop(inner);
            let recovery = match previous_secret {
                Some(value) => self.secrets.set(&entry.key, &value),
                None => self.secrets.delete(&entry.key),
            };
            if recovery.is_err() {
                let mut inner = self.inner.lock().unwrap();
                if let Some(machine) = inner.machines.iter_mut().find(|m| m.entry.key == entry.key)
                    && let Some(handle) = machine.handle.take()
                {
                    handle.cancel();
                }
                eprintln!("machine pairing recovery failed");
                return Err(MachineError::PairRecoveryFailed);
            }
            return Err(MachineError::Settings(error.to_string()));
        }
        inner.settings = candidate;
        if let Some(old) = inner.machines.iter().position(|m| m.entry.key == entry.key) {
            for action in inner.actions.end_machine(&entry.key) {
                self.emit(&action, ActionOutcome::NotSent { reason: NotSentReason::Offline });
                inner.notices.insert(
                    action.card_key,
                    (
                        notice("Offline. Nothing was sent.", NoticeTone::Warning),
                        Instant::now() + self.opts.timing.notice_ttl,
                    ),
                );
            }
            if let Some(handle) = inner.machines[old].handle.take() {
                handle.cancel();
            }
            inner.machines.remove(old);
        }
        let instance = self.next_instance.fetch_add(1, Ordering::Relaxed);
        let client_id = inner.settings.client_id.clone();
        let handle = spawn_with(&entry, info, &self.opts, instance, &client_id);
        inner.machines.push(Machine { entry: entry.clone(), handle: Some(handle.clone()) });
        drop(inner);
        self.watch_machine(entry.key.clone(), handle);
        self.wake();
        Ok(entry)
    }
```

Replace `remove_machine` with:

```rust
    pub fn remove_machine(&self, key: &str) -> Result<(), String> {
        self.remove_machine_checked(key).map_err(|e| e.to_string())
    }

    /// Removes a machine and its secret. SecretStore calls run outside
    /// `inner`; the settings file is written first, the secret deleted
    /// second, and the file restored if the delete fails.
    pub fn remove_machine_checked(&self, key: &str) -> Result<(), MachineError> {
        let _edit = self.edits.lock();
        if !self.inner.lock().unwrap().settings.machines.iter().any(|m| m.key == key) {
            return Err(MachineError::UnknownMachine);
        }
        self.secrets.get(key).map_err(MachineError::SecretRead)?;
        {
            let inner = self.inner.lock().unwrap();
            let mut candidate = inner.settings.clone();
            candidate.machines.retain(|m| m.key != key);
            if let Some(path) = &self.opts.settings_path {
                candidate.save(path).map_err(|e| MachineError::Settings(e.to_string()))?;
            }
        }
        if let Err(error) = self.secrets.delete(key) {
            let mut inner = self.inner.lock().unwrap();
            // `inner.settings` still lists the machine: it is the old file.
            if let Some(path) = &self.opts.settings_path
                && inner.settings.save(path).is_err()
            {
                if let Some(machine) = inner.machines.iter_mut().find(|m| m.entry.key == key)
                    && let Some(handle) = machine.handle.take()
                {
                    handle.cancel();
                }
                eprintln!("machine removal recovery failed");
                return Err(MachineError::RemoveRecoveryFailed);
            }
            return Err(MachineError::SecretDelete(error));
        }
        // The file already matches: every settings writer holds `edits`, so
        // none can have saved this machine back since (plan D3).
        let mut inner = self.inner.lock().unwrap();
        inner.settings.machines.retain(|m| m.key != key);
        for machine in inner.machines.iter().filter(|m| m.entry.key == key) {
            if let Some(handle) = &machine.handle {
                handle.cancel();
            }
        }
        inner.machines.retain(|m| m.entry.key != key);
        for action in inner.actions.end_machine(key) {
            self.emit(&action, ActionOutcome::NotSent { reason: NotSentReason::Offline });
            inner.notices.insert(
                action.card_key,
                (
                    notice("Offline. Nothing was sent.", NoticeTone::Warning),
                    Instant::now() + self.opts.timing.notice_ttl,
                ),
            );
        }
        drop(inner);
        self.wake();
        Ok(())
    }
```

Replace the first block of `start` (from `let handles = {` to the `for (key, handle) in handles` loop) with:

```rust
        let pending: Vec<MachineEntry> = {
            let inner = self.inner.lock().unwrap();
            if self.started.swap(true, Ordering::AcqRel) {
                return;
            }
            inner.machines.iter().filter(|m| m.handle.is_none()).map(|m| m.entry.clone()).collect()
        };
        // Secrets are read outside `inner` (omarchy spec §4.3).
        let infos: Vec<(MachineEntry, Option<(Endpoint, conn::Credential)>)> = pending
            .into_iter()
            .map(|entry| {
                let info = connect_info(&entry, &self.secrets);
                (entry, info)
            })
            .collect();
        let handles = {
            let mut inner = self.inner.lock().unwrap();
            let client_id = inner.settings.client_id.clone();
            for (entry, info) in infos {
                let Some(info) = info else { continue };
                let Some(machine) = inner.machines.iter_mut().find(|m| m.entry == entry && m.handle.is_none()) else {
                    continue;
                };
                let instance = self.next_instance.fetch_add(1, Ordering::Relaxed);
                machine.handle = Some(spawn_with(&entry, info, &self.opts, instance, &client_id));
            }
            inner
                .machines
                .iter()
                .filter_map(|m| m.handle.clone().map(|h| (m.entry.key.clone(), h)))
                .collect::<Vec<_>>()
        };
        for (key, handle) in handles {
            self.watch_machine(key, handle);
        }
```

Leave the timer loop below it unchanged.

- [ ] **Step 4b: Put every other settings writer under the edit lock**

Each of `rename_machine`, `set_hotkey`, `set_show_subagents` and `set_show_hook_only` saves the settings file. Make the first line of each body:

```rust
        let _edit = self.edits.lock();
```

None of them calls the SecretStore, so a Mac caller waits at most for one pairing's keyring call. `set_show_idle` saves nothing and stays as it is. acd-inbox runs these commands off the socket reader (Task 10, D11), so a wait never delays a disconnect.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p acd-client --locked`
Expected: PASS. The existing tests keep passing unmodified, among them `save_failure_preserves_import_rename_remove_and_hotkey_state`, `import_reports_failed_credential_recovery_without_leaking_details`, `remove_reports_failed_settings_recovery_without_leaking_details`, `secret_backend_errors_never_echo_pairing_credentials` and `start_after_import_keeps_the_imported_connection_handle`.

- [ ] **Step 6: Lint and commit**

Run: `cargo clippy --workspace --all-targets --locked -- -D warnings`
Expected: no warnings.

```bash
git add crates/acd-client/src/inbox.rs crates/acd-client/tests/inbox.rs
git commit -m "acd-client: no secret store calls under the inbox lock; typed machine errors"
```

---

### Task 3: acd-client: secret status and `reconnect_machine`

**Execution:** cloud-ok. This task edits `app/src-tauri/src/pairing.rs`: on Linux, report `npm run verify` as pending for macOS CI.

**Files:**
- Modify: `crates/acd-client/src/settings.rs`: `SecretError` (61 to 63), the `SecretStore` trait (243 to 247), tests
- Modify: `crates/acd-client/src/view.rs`: imports, `MachineEntryView` (166 to 172), `from_entry` (179 to 192), `machine_entry_views` (194 to 223)
- Modify: `crates/acd-client/src/inbox.rs`: `machines`, new `reconnect_machine`
- Modify: `app/src-tauri/src/pairing.rs:24-31` (test destructure)
- Test: `crates/acd-client/src/settings.rs` (unit), `crates/acd-client/tests/inbox.rs` (append)

**Interfaces:**
- Consumes: Task 2's `MachineError`, `connect_info`, `tcp_info`, `spawn_with`, `edits`.
- Produces (in `acd_client::settings`):
  - `pub const SECRET_LOCKED: &str`, `pub const SECRET_TIMEOUT: &str` and `pub const SECRET_UNKNOWN: &str`.
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum SecretErrorKind { Locked, Timeout, Unknown, Other }`.
  - `impl SecretError { pub fn locked() -> Self; pub fn timeout() -> Self; pub fn kind(&self) -> SecretErrorKind }`.
  - `#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)] #[serde(rename_all = "snake_case")] pub enum SecretStatus { Ok, Locked, Missing, Unreadable, Unknown }`.
  - `trait SecretStore { … fn status(&self, key: &str) -> SecretStatus { /* default from get */ } }`.
- Produces (in `acd_client::settings`): `SecretStore::mark_unknown(&self, key)`, a default no-op.
- Produces (in `acd_client::view`): `MachineEntryView.secret: SecretStatus`, skipped by ts-rs (D5); JSON `"secret":"ok"|"locked"|"missing"|"unreadable"`.
- Produces (on `Inbox`): `reconnect_machine(self: &Arc<Self>, key: &str) -> Result<(), MachineError>` (D6).

- [ ] **Step 1: Write the failing unit test**

Append to `mod tests` in `crates/acd-client/src/settings.rs`:

```rust
    #[test]
    fn secret_errors_carry_a_kind_and_status_follows_get() {
        assert_eq!(SecretError::locked().kind(), SecretErrorKind::Locked);
        assert_eq!(SecretError::timeout().kind(), SecretErrorKind::Timeout);
        assert_eq!(SecretError("denied".into()).kind(), SecretErrorKind::Other);
        let m = MemorySecrets::default();
        assert_eq!(m.status("k"), SecretStatus::Missing);
        m.set("k", "not-json").unwrap();
        assert_eq!(m.status("k"), SecretStatus::Unreadable);
        let ok = MachineSecret {
            token: "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8".into(),
            pin: "sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
        };
        m.set("k", &ok.to_json()).unwrap();
        assert_eq!(m.status("k"), SecretStatus::Ok);
        struct Locked;
        impl SecretStore for Locked {
            fn get(&self, _: &str) -> Result<Option<String>, SecretError> {
                Err(SecretError::locked())
            }
            fn set(&self, _: &str, _: &str) -> Result<(), SecretError> {
                Err(SecretError::locked())
            }
            fn delete(&self, _: &str) -> Result<(), SecretError> {
                Err(SecretError::locked())
            }
        }
        assert_eq!(Locked.status("k"), SecretStatus::Locked);
        assert_eq!(CachedSecrets::new(Locked).status("k"), SecretStatus::Locked);
        assert_eq!(serde_json::to_value(SecretStatus::Unreadable).unwrap(), "unreadable");
        assert_eq!(SecretError::unknown().kind(), SecretErrorKind::Unknown);
        assert_eq!(serde_json::to_value(SecretStatus::Unknown).unwrap(), "unknown");
    }
```

- [ ] **Step 2: Write the failing integration tests**

Append to `crates/acd-client/tests/inbox.rs`:

```rust
/// A keyring that refuses every call while locked.
struct Lockable {
    inner: MemorySecrets,
    unlocked: AtomicBool,
}

impl SecretStore for Lockable {
    fn get(&self, key: &str) -> Result<Option<String>, SecretError> {
        if !self.unlocked.load(SeqCst) {
            return Err(SecretError::locked());
        }
        self.inner.get(key)
    }
    fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
        if !self.unlocked.load(SeqCst) {
            return Err(SecretError::locked());
        }
        self.inner.set(key, value)
    }
    fn delete(&self, key: &str) -> Result<(), SecretError> {
        if !self.unlocked.load(SeqCst) {
            return Err(SecretError::locked());
        }
        self.inner.delete(key)
    }
}

#[tokio::test]
async fn a_locked_secret_is_shown_as_locked_and_reconnect_creates_the_handle() {
    let f = FakeServer::start(common::TOKEN, Identity::Paired).await;
    let store = Arc::new(Lockable { inner: MemorySecrets::default(), unlocked: AtomicBool::new(false) });
    store.inner.set("m1", &box_secret(&f)).unwrap();
    let ib = Inbox::new(box_settings(&f), store.clone(), options(common::fast()));
    ib.start();
    let m = ib.machines().remove(0);
    assert_eq!((m.secret, m.pairing), (SecretStatus::Locked, PairingState::Unreadable));
    assert!(ib.conn_stamp("m1").is_none(), "no handle without a secret");
    assert!(matches!(
        ib.reconnect_machine("m1"),
        Err(MachineError::SecretRead(e)) if e.kind() == SecretErrorKind::Locked
    ));
    store.unlocked.store(true, SeqCst);
    ib.reconnect_machine("m1").unwrap();
    let c = f.next_connection().await;
    c.send(serde_json::json!({"type":"snapshot","epoch":"e1","seq":1,"agents":[pending_agent()]})).await;
    view_until(&ib, "answerable after reconnect", |v| v.cards.iter().any(|c| c.answerable)).await;
    assert_eq!(ib.machines()[0].secret, SecretStatus::Ok);
}

#[tokio::test]
async fn reconnect_keeps_an_online_handle_and_rejects_unknown_or_unpaired_machines() {
    let (f, _c, ib) = setup().await;
    let before = ib.conn_stamp("m1");
    ib.reconnect_machine("m1").unwrap();
    assert_eq!(ib.conn_stamp("m1"), before, "an online handle is kept, not rebuilt");
    assert!(matches!(ib.reconnect_machine("nope"), Err(MachineError::UnknownMachine)));
    let empty = Inbox::new(box_settings(&f), Arc::new(MemorySecrets::default()), options(common::fast()));
    assert!(matches!(empty.reconnect_machine("m1"), Err(MachineError::NotPaired)));
    assert_eq!(empty.machines()[0].secret, SecretStatus::Missing);
}

/// A keyring whose reads, or writes and deletes, report an unconfirmed
/// state; a successful write makes the item known again.
#[derive(Default)]
struct Uncertain {
    inner: MemorySecrets,
    unknown_reads: AtomicBool,
    unknown_writes: AtomicBool,
    marked: AtomicBool,
    restores: AtomicUsize,
}

impl SecretStore for Uncertain {
    fn get(&self, key: &str) -> Result<Option<String>, SecretError> {
        if self.unknown_reads.load(SeqCst) {
            return Err(SecretError::unknown());
        }
        self.inner.get(key)
    }
    fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
        if self.unknown_writes.load(SeqCst) {
            return Err(SecretError::unknown());
        }
        if self.inner.get(key).unwrap().is_some() && !self.unknown_reads.load(SeqCst) {
            self.restores.fetch_add(1, SeqCst);
        }
        self.unknown_reads.store(false, SeqCst);
        self.inner.set(key, value)
    }
    fn delete(&self, key: &str) -> Result<(), SecretError> {
        if self.unknown_writes.load(SeqCst) {
            return Err(SecretError::unknown());
        }
        self.restores.fetch_add(1, SeqCst);
        self.inner.delete(key)
    }
    fn mark_unknown(&self, _key: &str) {
        self.marked.store(true, SeqCst);
        self.unknown_reads.store(true, SeqCst);
    }
}

async fn uncertain_setup(store: Arc<Uncertain>, settings_path: Option<std::path::PathBuf>) -> (FakeServer, Arc<Inbox>) {
    let f = FakeServer::start(common::TOKEN, Identity::Paired).await;
    store.inner.set("m1", &box_secret(&f)).unwrap();
    let ib = Inbox::new(box_settings(&f), store, InboxOptions { settings_path, ..options(held()) });
    ib.start();
    (f, ib)
}

#[tokio::test]
async fn an_unconfirmed_secret_change_stops_the_machine_until_a_read_succeeds() {
    let store = Arc::new(Uncertain::default());
    let (f, ib) = uncertain_setup(store.clone(), None).await;
    let c = f.next_connection().await;
    c.send(serde_json::json!({"type":"snapshot","epoch":"e1","seq":1,"agents":[pending_agent()]})).await;
    view_until(&ib, "answerable", |v| v.cards.iter().any(|c| c.answerable)).await;
    let mut rx = ib.action_outcomes();
    let id = ib.pick("m1/a1", ANSWER_REQ, "allow", 1).unwrap();
    store.unknown_writes.store(true, SeqCst);
    let line = format!("acd://127.0.0.1:{}?pin={}#{}", f.port(), f.pin(), common::TOKEN);
    let e = ib.pair_machine(&line).unwrap_err();
    assert!(matches!(&e, MachineError::SecretWrite(s) if s.kind() == SecretErrorKind::Unknown), "{e:?}");
    assert!(ib.conn_stamp("m1").is_none(), "no connection with uncertain credentials");
    assert_eq!(next_outcome(&mut rx, &id).await, ActionOutcome::NotSent { reason: NotSentReason::Offline });
    store.unknown_writes.store(false, SeqCst);
    store.unknown_reads.store(true, SeqCst);
    assert_eq!(ib.machines()[0].secret, SecretStatus::Unknown);
    assert!(matches!(ib.reconnect_machine("m1"), Err(MachineError::SecretRead(_))));
    store.unknown_reads.store(false, SeqCst);
    ib.reconnect_machine("m1").unwrap();
    f.next_connection().await;
}

#[tokio::test]
async fn re_pairing_from_unknown_writes_the_new_secret_and_connects() {
    let store = Arc::new(Uncertain::default());
    store.unknown_reads.store(true, SeqCst);
    let (f, ib) = uncertain_setup(store.clone(), None).await;
    assert!(ib.conn_stamp("m1").is_none(), "unknown at start: no handle");
    let line = format!("acd://127.0.0.1:{}?pin={}#{}", f.port(), f.pin(), common::TOKEN);
    assert_eq!(ib.pair_machine(&line).unwrap().key, "m1", "nothing restorable is not an error");
    f.next_connection().await;
    assert_eq!(ib.machines()[0].secret, SecretStatus::Ok);
}

#[tokio::test]
async fn re_pairing_from_unknown_that_cannot_commit_stays_unknown_and_stopped() {
    let d = tempfile::tempdir().unwrap();
    let blocker = d.path().join("file");
    std::fs::write(&blocker, "block").unwrap();
    let store = Arc::new(Uncertain::default());
    store.unknown_reads.store(true, SeqCst);
    let (f, ib) = uncertain_setup(store.clone(), Some(blocker.join("settings.json"))).await;
    let line = format!("acd://127.0.0.1:{}?pin={}#{}", f.port(), f.pin(), common::TOKEN);
    assert!(matches!(ib.pair_machine(&line), Err(MachineError::Settings(_))));
    assert!(store.marked.load(SeqCst), "the store was told the state is unknown");
    assert_eq!(store.restores.load(SeqCst), 0, "no rollback guess: nothing restored or deleted");
    assert!(ib.conn_stamp("m1").is_none());
    assert_eq!(ib.machines()[0].secret, SecretStatus::Unknown);
}

#[test]
fn the_secret_status_is_json_but_not_in_the_mac_bindings() {
    let view = MachineEntryView {
        key: "local".into(),
        name: "Here".into(),
        endpoint: EndpointConfig::Local,
        pairing: PairingState::Paired,
        secret: SecretStatus::Ok,
    };
    assert_eq!(serde_json::to_value(&view).unwrap()["secret"], "ok");
}
```

In `a_rename_during_a_re_pairing_is_kept` (Task 2), enable the line marked `// Task 3:`.

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p acd-client --locked secret`
Expected: compile errors, `no function or associated item named locked`, `cannot find type SecretStatus`, and `no method named reconnect_machine`.

- [ ] **Step 4: Implement the kinds and the status in `settings.rs`**

Below `pub struct SecretError(pub String);` add:

```rust
/// The text of `SecretError::locked()`; no Keychain message equals it.
pub const SECRET_LOCKED: &str = "the keyring is locked";
/// The text of `SecretError::timeout()`.
pub const SECRET_TIMEOUT: &str = "the keyring did not answer in time";
/// The text of `SecretError::unknown()`.
pub const SECRET_UNKNOWN: &str = "the keyring state is unknown";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecretErrorKind {
    Locked,
    Timeout,
    /// A write or delete may or may not have happened (omarchy plan D36).
    Unknown,
    Other,
}

impl SecretError {
    pub fn locked() -> Self {
        SecretError(SECRET_LOCKED.into())
    }
    pub fn timeout() -> Self {
        SecretError(SECRET_TIMEOUT.into())
    }
    pub fn unknown() -> Self {
        SecretError(SECRET_UNKNOWN.into())
    }
    pub fn kind(&self) -> SecretErrorKind {
        match self.0.as_str() {
            SECRET_LOCKED => SecretErrorKind::Locked,
            SECRET_TIMEOUT => SecretErrorKind::Timeout,
            SECRET_UNKNOWN => SecretErrorKind::Unknown,
            _ => SecretErrorKind::Other,
        }
    }
}

/// What a machine's stored secret looks like right now (omarchy spec §4.3):
/// a locked keyring is not "not paired".
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SecretStatus {
    Ok,
    Locked,
    Missing,
    Unreadable,
    /// A change to the item could not be confirmed, or the keyring does not
    /// answer; the machine stays disconnected until a successful read
    /// (retry) or a re-pairing.
    Unknown,
}
```

Replace the trait with:

```rust
pub trait SecretStore: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<String>, SecretError>;
    fn set(&self, key: &str, value: &str) -> Result<(), SecretError>;
    fn delete(&self, key: &str) -> Result<(), SecretError>;
    /// The secret's status, from `get` unless a store knows better.
    fn status(&self, key: &str) -> SecretStatus {
        match self.get(key) {
            Ok(None) => SecretStatus::Missing,
            Ok(Some(raw)) if MachineSecret::from_json(&raw).is_ok() => SecretStatus::Ok,
            Ok(Some(_)) => SecretStatus::Unreadable,
            Err(e) if e.kind() == SecretErrorKind::Locked => SecretStatus::Locked,
            // An unconfirmed change, or a keyring that does not answer (D36).
            Err(e) if matches!(e.kind(), SecretErrorKind::Unknown | SecretErrorKind::Timeout) => SecretStatus::Unknown,
            Err(_) => SecretStatus::Unreadable,
        }
    }
    /// Forget what is known about `key`: its status is `Unknown` until a
    /// successful read. acd-inbox's store implements it; the Keychain store
    /// does not need to (omarchy plan D36).
    fn mark_unknown(&self, _key: &str) {}
}
```

- [ ] **Step 5: Add `secret` to `MachineEntryView`**

In `view.rs`, change the settings import to `settings::{EndpointConfig, MachineEntry, MachineSecret, SecretStatus, SecretStore},` and make the struct:

```rust
#[derive(Serialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MachineEntryView {
    pub key: String,
    pub name: String,
    pub endpoint: EndpointConfig,
    pub pairing: PairingState,
    /// For acd-inbox views (omarchy spec §4.3). Not in the Mac app's
    /// TypeScript bindings, which do not use it (plan D5).
    #[cfg_attr(feature = "ts", ts(skip))]
    pub secret: SecretStatus,
}
```

In `from_entry`, compute both from the same condition:

```rust
    pub fn from_entry(entry: &MachineEntry, secret: Option<&MachineSecret>) -> Self {
        let paired = matches!(entry.endpoint, EndpointConfig::Local) || secret.is_some();
        Self {
            key: entry.key.clone(),
            name: entry.name.clone(),
            endpoint: entry.endpoint.clone(),
            pairing: if paired { PairingState::Paired } else { PairingState::NotPaired },
            secret: if paired { SecretStatus::Ok } else { SecretStatus::Missing },
        }
    }
```

Replace `machine_entry_views` with:

```rust
pub fn machine_entry_views(
    entries: &[MachineEntry],
    secrets: &dyn SecretStore,
) -> Result<Vec<MachineEntryView>, String> {
    entries
        .iter()
        .map(|entry| {
            let (pairing, secret) = match entry.endpoint {
                EndpointConfig::Local => (PairingState::Paired, SecretStatus::Ok),
                EndpointConfig::Tcp { .. } => match secrets.get(&entry.key) {
                    Err(_) => return Err("Could not read saved machine credentials".into()),
                    Ok(None) => (PairingState::NotPaired, SecretStatus::Missing),
                    Ok(Some(raw)) => match MachineSecret::from_json(&raw) {
                        Ok(_) => (PairingState::Paired, SecretStatus::Ok),
                        Err(_) => (PairingState::Unreadable, SecretStatus::Unreadable),
                    },
                },
            };
            Ok(MachineEntryView {
                key: entry.key.clone(),
                name: entry.name.clone(),
                endpoint: entry.endpoint.clone(),
                pairing,
                secret,
            })
        })
        .collect()
}
```

In `inbox.rs`, replace the `.map(|entry| { … })` body of `machines` with:

```rust
            .map(|entry| {
                let secret = match &entry.endpoint {
                    EndpointConfig::Local => SecretStatus::Ok,
                    EndpointConfig::Tcp { .. } => self.secrets.status(&entry.key),
                };
                let pairing = match secret {
                    SecretStatus::Ok => PairingState::Paired,
                    SecretStatus::Missing => PairingState::NotPaired,
                    SecretStatus::Locked | SecretStatus::Unreadable | SecretStatus::Unknown => PairingState::Unreadable,
                };
                MachineEntryView {
                    key: entry.key.clone(),
                    name: entry.name.clone(),
                    endpoint: entry.endpoint.clone(),
                    pairing,
                    secret,
                }
            })
```

and add `SecretStatus` to the `settings::{…}` import.

- [ ] **Step 6: Add `reconnect_machine`**

After `retry_machine` in `inbox.rs`:

```rust
    /// Retries a machine's connection, or creates its handle if it has none,
    /// for example because its secret could not be read at start (omarchy
    /// spec §4.3, plan D6). The secret is read outside `inner`.
    pub fn reconnect_machine(self: &Arc<Self>, key: &str) -> Result<(), MachineError> {
        let _edit = self.edits.lock();
        let (entry, handle) = {
            let inner = self.inner.lock().unwrap();
            let m = inner.machines.iter().find(|m| m.entry.key == key).ok_or(MachineError::UnknownMachine)?;
            (m.entry.clone(), m.handle.clone())
        };
        if let Some(handle) = handle {
            handle.retry_now();
            return Ok(());
        }
        let info = match &entry.endpoint {
            EndpointConfig::Local => connect_info(&entry, &self.secrets).ok_or(MachineError::NotPaired)?,
            EndpointConfig::Tcp { host, port } => {
                let raw = self
                    .secrets
                    .get(&entry.key)
                    .map_err(MachineError::SecretRead)?
                    .ok_or(MachineError::NotPaired)?;
                let secret = MachineSecret::from_json(&raw).map_err(MachineError::SecretRead)?;
                tcp_info(host, *port, &secret)
            }
        };
        let handle = {
            let mut inner = self.inner.lock().unwrap();
            let client_id = inner.settings.client_id.clone();
            let instance = self.next_instance.fetch_add(1, Ordering::Relaxed);
            let Some(machine) = inner.machines.iter_mut().find(|m| m.entry == entry) else {
                return Err(MachineError::UnknownMachine);
            };
            if let Some(existing) = &machine.handle {
                existing.retry_now();
                return Ok(());
            }
            let handle = spawn_with(&entry, info, &self.opts, instance, &client_id);
            machine.handle = Some(handle.clone());
            handle
        };
        self.watch_machine(entry.key.clone(), handle);
        self.wake();
        Ok(())
    }
```

- [ ] **Step 6b: Fail closed on an unconfirmed change (D36)**

Add to `impl Inbox`:

```rust
    /// After a SecretStore error of kind `Unknown`, the machine's credentials
    /// are uncertain: stop its handle and end its countdowns (fail closed).
    /// Only a successful read (`reconnect_machine`) or a re-pairing brings
    /// it back.
    fn fail_closed(&self, key: &str, e: &SecretError) {
        if e.kind() != SecretErrorKind::Unknown {
            return;
        }
        let mut inner = self.inner.lock().unwrap();
        if let Some(machine) = inner.machines.iter_mut().find(|m| m.entry.key == key)
            && let Some(handle) = machine.handle.take()
        {
            handle.cancel();
        }
        for action in inner.actions.end_machine(key) {
            self.emit(&action, ActionOutcome::NotSent { reason: NotSentReason::Offline });
        }
        drop(inner);
        self.wake();
    }
```

Call it on every SecretStore error path that touches a machine's item:
- In `pair_machine`, map the write as `.map_err(|e| { self.fail_closed(&entry.key, &e); MachineError::SecretWrite(e) })?`.
- In `pair_machine`, an `Unknown` previous secret is "nothing restorable" (orchestrator decision, 2026-10-12). Pairing is how the user recovers it, so it skips the restore, writes the new secret and commits. If the commit fails, it does not roll back: it calls `mark_unknown` and leaves the handle stopped. Replace the read of the previous secret, and the recovery match, with:

```rust
        /// What a failed commit puts back (D36).
        enum Restore {
            Previous(Option<String>),
            /// The previous item is unknown: nothing to restore, no guess.
            Nothing,
        }
        let restore = match self.secrets.get(&entry.key) {
            Ok(previous) => Restore::Previous(previous),
            Err(e) if e.kind() == SecretErrorKind::Unknown => Restore::Nothing,
            Err(e) => {
                self.fail_closed(&entry.key, &e);
                return Err(MachineError::SecretRead(e));
            }
        };
```

  In the commit-failure branch:

```rust
            drop(inner);
            let recovery = match restore {
                Restore::Previous(Some(value)) => self.secrets.set(&entry.key, &value),
                Restore::Previous(None) => self.secrets.delete(&entry.key),
                Restore::Nothing => {
                    self.secrets.mark_unknown(&entry.key);
                    self.fail_closed(&entry.key, &SecretError::unknown());
                    return Err(MachineError::Settings(error.to_string()));
                }
            };
```
- In `remove_machine_checked`, call `self.fail_closed(key, &error)` first in the `if let Err(error) = self.secrets.delete(key)` branch.
- The rollback branch already stops the handle when recovery fails.

`reconnect_machine` needs no change: its read returns the `Unknown` error, so it creates no handle until a read succeeds.

- [ ] **Step 7: Update the app's exhaustive destructure**

In `app/src-tauri/src/pairing.rs`, `view_has_only_public_fields`, add `secret: _,` after `pairing: _,`.

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test -p acd-client --locked`
Expected: PASS, including `bindings_are_current` (the bindings are unchanged).

Run: `cargo test --workspace --locked`
Expected: PASS.

On macOS: `cd app && npm ci && npm run verify`
Expected: PASS. On Linux, report it as pending for CI.

- [ ] **Step 9: Lint and commit**

Run: `cargo clippy --workspace --all-targets --locked -- -D warnings`
Expected: no warnings.

```bash
git add crates/acd-client/src/settings.rs crates/acd-client/src/view.rs crates/acd-client/src/inbox.rs crates/acd-client/tests/inbox.rs app/src-tauri/src/pairing.rs
git commit -m "acd-client: secret status per machine and reconnect_machine for a missing handle"
```

---
### Task 4: acd-inbox crate, protocol and bounded views

**Execution:** cloud-ok

**Files:**
- Create: `crates/acd-inbox/Cargo.toml`, `crates/acd-inbox/src/lib.rs`, `crates/acd-inbox/src/protocol.rs`
- Modify: `Cargo.toml` (workspace dependency `acd-inbox`), `Cargo.lock`
- Modify: `docs/superpowers/specs/2026-10-12-omarchy-inbox-design.md` §5, first paragraph (spec amendment, Step 6)
- Test: `crates/acd-inbox/src/protocol.rs` (unit)

**Interfaces:**
- Consumes:
  - `acd_client::view::{InboxView, MachineEntryView}`
  - `acd_client::voice::{ActionEvent, ActionOutcome, NotSentReason}`
  - `acd_client::inbox::PickError`
- Produces (in `acd_inbox::protocol`):
  - `pub const PROTOCOL: u32 = 1; pub const MAX_LINE: usize = 4 * 1024 * 1024; pub const MAX_ID: usize = 64; pub const TRUNCATED: &str = "…[truncated]";`
  - `pub enum ErrorCode` (D8), with `as_str(self) -> &'static str`.
  - `pub fn pick_error_code(e: &PickError) -> ErrorCode`.
  - `pub struct SettingsSnapshot { pub machines: Vec<MachineEntryView>, pub notifications: bool, pub show_idle: bool, pub show_subagents: bool, pub show_hook_only: bool }`. It serializes with `"type":"settings"`.
  - `pub enum OutcomeResult` (tagged `kind`), `pub enum NotSentCode`, and `From` impls from `ActionOutcome` and `NotSentReason`.
  - Line builders (no trailing newline):
    - `hello_line(version: &str) -> String`
    - `settings_line(&SettingsSnapshot, max: usize) -> String` (machines beyond the bound are left out and counted in `omitted_machines`)
    - `view_line(&InboxView, max: usize) -> Option<String>` (D9)
    - `view_overflow_line(needs_you: usize) -> String` and `view_or_overflow_line(&InboxView, max) -> String`: the view, or `{"type":"view_overflow","needs_you":n}` when it cannot fit
  - `pub struct Omitted { pub done: usize, pub working: usize }`, serialized in every `view` line.
    - `outcome_line(&ActionEvent) -> String`
    - `outcomes_lost_line() -> String`
    - `ok_line(id: &str, result: serde_json::Value) -> String`
    - `error_line(id: &str, code: ErrorCode) -> String`

JSON shapes this task fixes:

```json
{"type":"hello","protocol":1,"service":"0.1.0"}
{"type":"settings","machines":[{"key":"m1","name":"Box","endpoint":{"kind":"tcp","host":"h","port":47800},"pairing":"paired","secret":"ok"}],"notifications":true,"show_idle":false,"show_subagents":false,"show_hook_only":false}
{"type":"view","view":{"machines":[],"cards":[],"hidden_idle":0,"show_idle":false,"show_subagents":false,"show_hook_only":false,"tray":{"color":"grey","needs_you":0},"actions":[]},"omitted":{"done":0,"working":0}}
{"type":"outcome","action":"<id>","card":"m1/a1","result":{"kind":"rejected","code":"request_gone"}}
{"type":"outcome","action":"<id>","card":"m1/a1","result":{"kind":"not_sent","reason":"answered_elsewhere"}}
{"type":"outcomes_lost"}
{"type":"ok","id":"c1","result":"<action id>"}
{"type":"error","id":"c1","code":"stale"}
```

- [ ] **Step 1: Create the crate**

`crates/acd-inbox/Cargo.toml`:

```toml
[package]
name = "acd-inbox"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[features]
# Exposes secrets::fake for this crate's integration tests.
testing = []

[dependencies]
acd-client.workspace = true
acd-protocol.workspace = true
acd-securefs.workspace = true
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true
tokio.workspace = true
tracing.workspace = true

[dev-dependencies]
acd-client = { workspace = true, features = ["testing"] }
acd-inbox = { path = ".", features = ["testing"] }
tempfile.workspace = true
```

In the workspace `Cargo.toml` `[workspace.dependencies]`, add `acd-inbox = { path = "crates/acd-inbox" }` after `acd-client`.

`crates/acd-inbox/src/lib.rs`:

```rust
//! acd-inbox: the agent-caller inbox as a local service for Omarchy
//! (omarchy spec §4). Views connect over a Unix socket and speak JSON lines.
pub mod protocol;
```

- [ ] **Step 2: Write the failing tests**

Create `crates/acd-inbox/src/protocol.rs` with only the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use acd_client::view::{CardGroup, CardView, TrayColor, TraySummary};
    use acd_client::voice::{ActionEvent, ActionOutcome, NotSentReason};
    use serde_json::{Value, json};

    fn agent(id: &str, title: &str, screen: Option<&str>) -> acd_protocol::Agent {
        serde_json::from_value(json!({"id":id,"machine_id":"m","source":"herdr","kind":"claude","title":title,"cwd":null,"project":null,"state":"needs_you","state_since":"2026-10-08T00:00:00Z","failed":false,"last_message":null,"can_send":true,"pending":{"id":format!("r-{id}"),"revision":1,"agent_id":id,"kind":"permission","status":"open","answer_path":"hook","title":"Bash","detail":"npm test","options":[{"id":"allow","label":"Yes","description":null,"effect":"allow"}],"questions":null,"free_text":false,"raw_screen":screen,"opened_at":"2026-10-08T00:00:00Z"}})).unwrap()
    }

    fn card(a: acd_protocol::Agent) -> CardView {
        CardView {
            key: format!("m1/{}", a.id),
            machine_key: "m1".into(),
            machine_name: "Box".into(),
            agent: a,
            group: CardGroup::Request,
            live: true,
            offline_since_ms: None,
            answerable: true,
            free_text_answerable: false,
            selectable_options: vec!["allow".into()],
            can_reply: false,
            can_mark_seen: false,
            busy: false,
            notice: None,
            action: None,
        }
    }

    fn view(cards: Vec<CardView>) -> InboxView {
        InboxView {
            machines: vec![],
            cards,
            hidden_idle: 0,
            show_idle: false,
            show_subagents: false,
            show_hook_only: false,
            tray: TraySummary { color: TrayColor::Amber, needs_you: 1 },
            actions: vec![],
        }
    }

    fn parse(line: &str) -> Value {
        serde_json::from_str(line).unwrap()
    }

    #[test]
    fn hello_settings_and_reply_shapes() {
        assert_eq!(parse(&hello_line("0.1.0")), json!({"type":"hello","protocol":1,"service":"0.1.0"}));
        let s = SettingsSnapshot { machines: vec![], notifications: true, show_idle: false, show_subagents: true, show_hook_only: false };
        assert_eq!(
            parse(&settings_line(&s, MAX_LINE)),
            json!({"type":"settings","machines":[],"notifications":true,"show_idle":false,"show_subagents":true,"show_hook_only":false})
        );
        assert_eq!(parse(&ok_line("c1", json!("a-1"))), json!({"type":"ok","id":"c1","result":"a-1"}));
        assert_eq!(parse(&error_line("c2", ErrorCode::Stale)), json!({"type":"error","id":"c2","code":"stale"}));
        assert_eq!(parse(&outcomes_lost_line()), json!({"type":"outcomes_lost"}));
    }

    #[test]
    fn every_error_code_is_its_fixed_string() {
        use ErrorCode::*;
        for code in [
            Stale, UnknownCard, NotAnswerable, UnknownOption, AlreadyCounting, BadText, CannotReply,
            CannotMarkSeen, Hidden, UnknownMachine, NotPaired, BadName, BadPairing, BadRequest,
            SecretStoreLocked, SecretStoreTimeout, SecretStoreFailed, SettingsFailed, DuplicateId, Unsupported,
        ] {
            assert_eq!(serde_json::to_value(code).unwrap(), code.as_str());
        }
        assert_eq!(pick_error_code(&PickError::AlreadyCounting).as_str(), "already_counting");
        assert_eq!(pick_error_code(&PickError::BadAnswerText).as_str(), "bad_text");
        assert_eq!(pick_error_code(&PickError::ConnectionChanged).as_str(), "stale");
    }

    #[test]
    fn outcomes_map_one_to_one() {
        let line = |outcome| {
            parse(&outcome_line(&ActionEvent { action_id: "a".into(), card_key: "m1/a1".into(), outcome }))
        };
        assert_eq!(line(ActionOutcome::Fired), json!({"type":"outcome","action":"a","card":"m1/a1","result":{"kind":"fired"}}));
        assert_eq!(line(ActionOutcome::Delivered)["result"], json!({"kind":"delivered"}));
        assert_eq!(line(ActionOutcome::Uncertain)["result"], json!({"kind":"uncertain"}));
        assert_eq!(line(ActionOutcome::Cancelled)["result"], json!({"kind":"cancelled"}));
        assert_eq!(
            line(ActionOutcome::Rejected { code: acd_protocol::ErrorCode::RequestGone })["result"],
            json!({"kind":"rejected","code":"request_gone"})
        );
        for (reason, text) in [
            (NotSentReason::Offline, "offline"),
            (NotSentReason::NotSupported, "not_supported"),
            (NotSentReason::AgentGone, "agent_gone"),
            (NotSentReason::AnsweredElsewhere, "answered_elsewhere"),
            (NotSentReason::Changed, "changed"),
            (NotSentReason::AgentBusy, "agent_busy"),
            (NotSentReason::Hidden, "hidden"),
        ] {
            assert_eq!(line(ActionOutcome::NotSent { reason })["result"], json!({"kind":"not_sent","reason":text}));
        }
    }

    /// A Done (or Working) card without a request.
    fn quiet(id: &str, state: &str, group: CardGroup) -> CardView {
        let mut a = agent(id, "quiet agent", None);
        a.pending = None;
        a.state = serde_json::from_value(json!(state)).unwrap();
        CardView { group, answerable: false, selectable_options: vec![], ..card(a) }
    }

    fn line(v: &InboxView, max: usize) -> Value {
        let l = view_line(v, max).expect("fits");
        assert!(l.len() < max, "{} bytes for a {max} limit", l.len());
        parse(&l)
    }

    fn keys(out: &Value) -> Vec<String> {
        out["view"]["cards"].as_array().unwrap().iter().map(|c| c["key"].as_str().unwrap().to_owned()).collect()
    }

    #[test]
    fn a_small_view_is_sent_whole() {
        let v = view(vec![card(agent("a1", "fix tests", None))]);
        assert_eq!(
            line(&v, MAX_LINE),
            json!({"type":"view","view":serde_json::to_value(&v).unwrap(),"omitted":{"done":0,"working":0}})
        );
    }

    #[test]
    fn a_5_mib_screen_is_cut_with_a_marker_and_the_request_survives() {
        let screen = "é".repeat(5 * 1024 * 1024 / 2 + 7);
        let v = view(vec![card(agent("a1", "fix tests", Some(&screen)))]);
        let out = line(&v, MAX_LINE);
        let pending = &out["view"]["cards"][0]["agent"]["pending"];
        assert!(pending["raw_screen"].as_str().unwrap().ends_with(TRUNCATED));
        assert_eq!(pending["id"], "r-a1");
        assert_eq!(out["view"]["cards"][0]["agent"]["title"], "fix tests", "short text is not cut");
        assert_eq!(out["omitted"], json!({"done":0,"working":0}));
    }

    #[test]
    fn text_is_cut_before_any_card_is_omitted() {
        let title = "t".repeat(3000);
        let cards: Vec<CardView> = (0..200).map(|i| card(agent(&format!("a{i}"), &title, None))).collect();
        let v = view(cards);
        let at_256 = {
            let mut short = v.clone();
            for c in &mut short.cards {
                c.agent.title = format!("{}{TRUNCATED}", "t".repeat(256));
            }
            view_line(&short, usize::MAX).unwrap().len()
        };
        let out = line(&v, at_256 + 1);
        assert_eq!(keys(&out).len(), 200);
        assert!(out["view"]["cards"][199]["agent"]["title"].as_str().unwrap().ends_with(TRUNCATED));
    }

    #[test]
    fn done_cards_go_first_then_working_cards_and_requests_never() {
        let mut cards = vec![card(agent("r1", "needs you", None))];
        cards.extend((0..100).map(|i| quiet(&format!("w{i}"), "working", CardGroup::Working)));
        cards.extend((0..100).map(|i| quiet(&format!("d{i}"), "done", CardGroup::Done)));
        let v = view(cards);
        let size = |keep: &dyn Fn(&CardView) -> bool| {
            let mut x = v.clone();
            x.cards.retain(|c| keep(c));
            view_line(&x, usize::MAX).unwrap().len()
        };
        let without_done = size(&|c| c.group != CardGroup::Done);
        let requests_only = size(&|c| c.group == CardGroup::Request);
        let out = line(&v, without_done + 32);
        assert_eq!(out["omitted"], json!({"done":100,"working":0}));
        assert!(keys(&out).iter().all(|k| !k.starts_with("m1/d")));
        let out = line(&v, requests_only + 32);
        assert_eq!(out["omitted"], json!({"done":100,"working":100}));
        assert_eq!(keys(&out), ["m1/r1"]);
    }

    #[test]
    fn a_card_with_a_countdown_is_never_omitted() {
        let mut counting = quiet("d0", "done", CardGroup::Done);
        counting.action = Some(acd_client::view::ActionView {
            id: "x".into(),
            card_key: "m1/d0".into(),
            label: "Reply".into(),
            detail: None,
            fires_at_ms: 0.,
        });
        let mut cards = vec![card(agent("r1", "needs you", None)), counting];
        cards.extend((1..100).map(|i| quiet(&format!("d{i}"), "done", CardGroup::Done)));
        let v = view(cards);
        let mut two = v.clone();
        two.cards.truncate(2);
        let out = line(&v, view_line(&two, usize::MAX).unwrap().len() + 32);
        assert_eq!(keys(&out), ["m1/r1", "m1/d0"]);
        assert_eq!(out["omitted"]["done"], 99);
    }

    #[test]
    fn needs_you_text_is_cut_further_but_no_request_is_dropped() {
        let cards: Vec<CardView> = (0..50).map(|i| card(agent(&format!("a{i}"), &"t".repeat(1000), None))).collect();
        let v = view(cards);
        let mut at_100 = v.clone();
        for c in &mut at_100.cards {
            c.agent.title = format!("{}{TRUNCATED}", "t".repeat(100));
        }
        let out = line(&v, view_line(&at_100, usize::MAX).unwrap().len() + 1);
        assert_eq!(keys(&out).len(), 50);
        let title = out["view"]["cards"][0]["agent"]["title"].as_str().unwrap();
        assert!(title.ends_with(TRUNCATED) && title.chars().count() <= 100 + TRUNCATED.chars().count());
        assert!(view_line(&v, 200).is_none(), "nothing is sent rather than dropping a request");
        assert_eq!(parse(&view_or_overflow_line(&v, 200)), json!({"type":"view_overflow","needs_you":50}));
    }

    #[test]
    fn ids_and_enums_are_never_cut() {
        let long_id = "x".repeat(500);
        let v = view(vec![card(agent(&long_id, &"t".repeat(5000), None))]);
        let out = line(&v, 4000);
        assert_eq!(out["view"]["cards"][0]["agent"]["id"].as_str().unwrap(), long_id);
        assert_eq!(out["view"]["cards"][0]["agent"]["state"], "needs_you");
    }

    #[test]
    fn settings_are_bounded_too() {
        use acd_client::settings::{EndpointConfig, SecretStatus};
        use acd_client::view::{MachineEntryView, PairingState};
        let machines = (0..100)
            .map(|i| MachineEntryView {
                key: format!("{i:08}-0000-4000-8000-000000000000"),
                name: "n".repeat(64),
                endpoint: EndpointConfig::Tcp { host: "h".repeat(200), port: 1 },
                pairing: PairingState::Paired,
                secret: SecretStatus::Ok,
            })
            .collect();
        let s = SettingsSnapshot { machines, notifications: true, show_idle: false, show_subagents: false, show_hook_only: false };
        let whole = settings_line(&s, usize::MAX);
        assert!(parse(&whole).get("omitted_machines").is_none());
        let cut = settings_line(&s, whole.len() / 2);
        assert!(cut.len() < whole.len() / 2);
        let out = parse(&cut);
        let kept = out["machines"].as_array().unwrap().len();
        assert_eq!(kept + out["omitted_machines"].as_u64().unwrap() as usize, 100);
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p acd-inbox --lib`
(no `--locked` here: adding the crate updates `Cargo.lock`)
Expected: compile errors, `cannot find function hello_line`, and others.

- [ ] **Step 4: Implement `protocol.rs`**

Put this above the test module:

```rust
//! The socket protocol (omarchy spec §5): JSON, one object per line, UTF-8.
use acd_client::{
    inbox::PickError,
    view::{CardGroup, InboxView, MachineEntryView},
    voice::{ActionEvent, ActionOutcome, NotSentReason},
};
use serde::Serialize;
use serde_json::{Value, json};

pub const PROTOCOL: u32 = 1;
/// The longest line either side may send, newline excluded.
pub const MAX_LINE: usize = 4 * 1024 * 1024;
/// The longest command id.
pub const MAX_ID: usize = 64;
/// Appended to agent text the service cut so a view fits in a line.
pub const TRUNCATED: &str = "…[truncated]";

/// Fixed reply codes (plan D8). Views map them to text; the service never
/// sends an error's message.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Stale,
    UnknownCard,
    NotAnswerable,
    UnknownOption,
    AlreadyCounting,
    BadText,
    CannotReply,
    CannotMarkSeen,
    Hidden,
    UnknownMachine,
    NotPaired,
    BadName,
    BadPairing,
    BadRequest,
    SecretStoreLocked,
    SecretStoreTimeout,
    SecretStoreFailed,
    SettingsFailed,
    DuplicateId,
    Unsupported,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::Stale => "stale",
            ErrorCode::UnknownCard => "unknown_card",
            ErrorCode::NotAnswerable => "not_answerable",
            ErrorCode::UnknownOption => "unknown_option",
            ErrorCode::AlreadyCounting => "already_counting",
            ErrorCode::BadText => "bad_text",
            ErrorCode::CannotReply => "cannot_reply",
            ErrorCode::CannotMarkSeen => "cannot_mark_seen",
            ErrorCode::Hidden => "hidden",
            ErrorCode::UnknownMachine => "unknown_machine",
            ErrorCode::NotPaired => "not_paired",
            ErrorCode::BadName => "bad_name",
            ErrorCode::BadPairing => "bad_pairing",
            ErrorCode::BadRequest => "bad_request",
            ErrorCode::SecretStoreLocked => "secret_store_locked",
            ErrorCode::SecretStoreTimeout => "secret_store_timeout",
            ErrorCode::SecretStoreFailed => "secret_store_failed",
            ErrorCode::SettingsFailed => "settings_failed",
            ErrorCode::DuplicateId => "duplicate_id",
            ErrorCode::Unsupported => "unsupported",
        }
    }
}

pub fn pick_error_code(e: &PickError) -> ErrorCode {
    match e {
        PickError::UnknownCard => ErrorCode::UnknownCard,
        PickError::NotAnswerable => ErrorCode::NotAnswerable,
        PickError::StaleRequest | PickError::ConnectionChanged => ErrorCode::Stale,
        PickError::UnknownOption => ErrorCode::UnknownOption,
        PickError::AlreadyCounting => ErrorCode::AlreadyCounting,
        PickError::BadText | PickError::BadAnswerText => ErrorCode::BadText,
        PickError::CannotReply => ErrorCode::CannotReply,
        PickError::Hidden => ErrorCode::Hidden,
    }
}

/// The `settings` message (omarchy spec §5.4, plan S1). No tokens or pins:
/// `MachineEntryView` has none.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename = "settings")]
pub struct SettingsSnapshot {
    pub machines: Vec<MachineEntryView>,
    pub notifications: bool,
    pub show_idle: bool,
    pub show_subagents: bool,
    pub show_hook_only: bool,
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NotSentCode {
    Offline,
    NotSupported,
    AgentGone,
    AnsweredElsewhere,
    Changed,
    AgentBusy,
    Hidden,
}

impl From<NotSentReason> for NotSentCode {
    fn from(r: NotSentReason) -> Self {
        match r {
            NotSentReason::Offline => NotSentCode::Offline,
            NotSentReason::NotSupported => NotSentCode::NotSupported,
            NotSentReason::AgentGone => NotSentCode::AgentGone,
            NotSentReason::AnsweredElsewhere => NotSentCode::AnsweredElsewhere,
            NotSentReason::Changed => NotSentCode::Changed,
            NotSentReason::AgentBusy => NotSentCode::AgentBusy,
            NotSentReason::Hidden => NotSentCode::Hidden,
        }
    }
}

/// `R` in an `outcome` message (omarchy spec §5.3).
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutcomeResult {
    Fired,
    Delivered,
    Rejected { code: acd_protocol::ErrorCode },
    Uncertain,
    Cancelled,
    NotSent { reason: NotSentCode },
}

impl From<ActionOutcome> for OutcomeResult {
    fn from(o: ActionOutcome) -> Self {
        match o {
            ActionOutcome::Fired => OutcomeResult::Fired,
            ActionOutcome::Delivered => OutcomeResult::Delivered,
            ActionOutcome::Rejected { code } => OutcomeResult::Rejected { code },
            ActionOutcome::Uncertain => OutcomeResult::Uncertain,
            ActionOutcome::Cancelled => OutcomeResult::Cancelled,
            ActionOutcome::NotSent { reason } => OutcomeResult::NotSent { reason: reason.into() },
        }
    }
}

pub fn hello_line(version: &str) -> String {
    json!({"type": "hello", "protocol": PROTOCOL, "service": version}).to_string()
}

/// A `settings` line under `max` bytes. Machine entries are small (names
/// at most 64 characters), so the cut below is for robustness: entries are
/// left out from the end and counted in `omitted_machines`.
pub fn settings_line(s: &SettingsSnapshot, max: usize) -> String {
    let whole = serde_json::to_string(s).expect("serializable");
    if whole.len() < max {
        return whole;
    }
    let mut cut = s.clone();
    let mut omitted = 0usize;
    loop {
        cut.machines.pop();
        omitted += 1;
        let mut v = serde_json::to_value(&cut).expect("serializable");
        v["omitted_machines"] = omitted.into();
        let line = v.to_string();
        if line.len() < max || cut.machines.is_empty() {
            tracing::warn!(omitted, "settings too large; machines left out");
            return line;
        }
    }
}

pub fn outcome_line(e: &ActionEvent) -> String {
    json!({
        "type": "outcome",
        "action": e.action_id,
        "card": e.card_key,
        "result": OutcomeResult::from(e.outcome),
    })
    .to_string()
}

/// Sent instead of a view that cannot fit even with Needs-you cards only
/// (plan D9): the panel shows an error state with the count.
pub fn view_overflow_line(needs_you: usize) -> String {
    json!({"type": "view_overflow", "needs_you": needs_you}).to_string()
}

/// The view line, or the overflow line when the view cannot fit.
pub fn view_or_overflow_line(view: &InboxView, max: usize) -> String {
    view_line(view, max).unwrap_or_else(|| {
        view_overflow_line(view.cards.iter().filter(|c| c.group == CardGroup::Request).count())
    })
}

pub fn outcomes_lost_line() -> String {
    json!({"type": "outcomes_lost"}).to_string()
}

pub fn ok_line(id: &str, result: Value) -> String {
    json!({"type": "ok", "id": id, "result": result}).to_string()
}

pub fn error_line(id: &str, code: ErrorCode) -> String {
    json!({"type": "error", "id": id, "code": code}).to_string()
}

/// Keys inside an agent whose strings are ids, enums or times: never cut.
const KEEP: &[&str] = &[
    "id", "machine_id", "agent_id", "source", "kind", "state", "status", "answer_path", "effect",
    "state_since", "opened_at", "role",
];
/// Agent text is cut to no less than this before any card is left out.
const FIRST_CAP: usize = 256;
/// The shortest agent text field is ever cut to.
const MIN_CAP: usize = 32;

/// Cards a `view` line left out to fit (plan D9). Always present.
#[derive(Serialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Omitted {
    pub done: usize,
    pub working: usize,
}

/// A `view` line under `max` bytes, newline included (plan D9, spec §5
/// as amended in Step 6):
/// 1. the whole view;
/// 2. agent text cut, longest first, to no less than `FIRST_CAP`;
/// 3. then Done and idle cards left out, then Working and unknown ones,
///    except cards with a countdown, counted in `omitted`;
/// 4. then the remaining (Needs-you) text cut down to `MIN_CAP`.
///
/// `None` when even that does not fit: the caller sends `view_overflow`
/// instead. Needs-you cards are never left out.
pub fn view_line(view: &InboxView, max: usize) -> Option<String> {
    let encode = |v: &InboxView, o: Omitted| json!({"type": "view", "view": v, "omitted": o});
    let whole = encode(view, Omitted::default()).to_string();
    if whole.len() < max {
        return Some(whole);
    }
    if let Some(s) = fit_text(&encode(view, Omitted::default()), FIRST_CAP, max) {
        return Some(s);
    }
    let mut v = view.clone();
    let mut omitted = Omitted::default();
    let stages: [(&[CardGroup], bool); 2] =
        [(&[CardGroup::Done, CardGroup::Idle], true), (&[CardGroup::Working, CardGroup::Unknown], false)];
    for (groups, done) in stages {
        let before = v.cards.len();
        v.cards.retain(|c| !groups.contains(&c.group) || c.action.is_some());
        let n = before - v.cards.len();
        if done {
            omitted.done = n;
        } else {
            omitted.working = n;
        }
        if let Some(s) = fit_text(&encode(&v, omitted), FIRST_CAP, max) {
            tracing::warn!(done = omitted.done, working = omitted.working, "view too large; cards left out");
            return Some(s);
        }
    }
    let s = fit_text(&encode(&v, omitted), MIN_CAP, max);
    if s.is_none() {
        tracing::error!(cards = v.cards.len(), "view too large even with requests only; not sent");
    }
    s
}

/// The largest cap, at least `floor`, at which `v` fits under `max`.
fn fit_text(v: &Value, floor: usize, max: usize) -> Option<String> {
    let fits = |cap: usize| {
        let mut c = v.clone();
        cap_text(&mut c, cap);
        let s = c.to_string();
        (s.len() < max).then_some(s)
    };
    let mut best = fits(floor)?;
    let (mut lo, mut hi) = (floor, longest_text(v).max(floor));
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        match fits(mid) {
            Some(s) => {
                lo = mid;
                best = s;
            }
            None => hi = mid - 1,
        }
    }
    Some(best)
}

fn texts_mut<'a>(v: &'a mut Value, out: &mut Vec<&'a mut String>) {
    match v {
        Value::String(s) => out.push(s),
        Value::Array(items) => {
            for item in items {
                texts_mut(item, out);
            }
        }
        Value::Object(map) => {
            for (key, value) in map.iter_mut() {
                if !KEEP.contains(&key.as_str()) {
                    texts_mut(value, out);
                }
            }
        }
        _ => {}
    }
}

/// Every cuttable string under `view.cards[*].agent`.
fn agent_texts(v: &mut Value) -> Vec<&mut String> {
    let mut out = Vec::new();
    if let Some(cards) = v["view"]["cards"].as_array_mut() {
        for card in cards {
            if let Some(agent) = card.get_mut("agent") {
                texts_mut(agent, &mut out);
            }
        }
    }
    out
}

fn cap_text(v: &mut Value, cap: usize) {
    for s in agent_texts(v) {
        if s.chars().count() > cap {
            let mut cut: String = s.chars().take(cap).collect();
            cut.push_str(TRUNCATED);
            *s = cut;
        }
    }
}

fn longest_text(v: &Value) -> usize {
    let mut copy = v.clone();
    agent_texts(&mut copy).iter().map(|s| s.chars().count()).max().unwrap_or(0)
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p acd-inbox --lib` (this updates `Cargo.lock` once)
Run: `cargo test -p acd-inbox --locked --lib`
Expected: PASS.

- [ ] **Step 6: Amend the spec**

The orchestrator decided (2026-10-12) how oversized views behave. In `docs/superpowers/specs/2026-10-12-omarchy-inbox-design.md` §5, replace the paragraph's last sentence ("The service keeps its own lines under 4 MiB: agent text fields in a view are truncated by the service (with a marker) until the view fits, and the panel shows the marker.") with:

```markdown
The service keeps its own lines under 4 MiB. To fit a view it first
truncates agent text fields (with a marker the panel shows); then leaves
out Done cards, then Working cards (never a card with a countdown), and
reports them as `"omitted":{"done":n,"working":m}` so the panel shows
"N not shown"; Needs-you cards are never left out, and if they alone do
not fit, their text is truncated further down to a minimum per field.
If even that does not fit, the service sends
`{"type":"view_overflow","needs_you":n}` instead of a view and the panel
shows an error state with the count until a view fits again.
`settings` is bounded the same way (machines left out are counted in
`omitted_machines`).
```

Add to §13 under the re-review paragraph: "Oversized views (2026-10-12, orchestrator): text first, then Done, then Working cards with counts; Needs-you cards are never left out."

- [ ] **Step 7: Lint and commit**

Run: `cargo clippy --workspace --all-targets --locked -- -D warnings`
Expected: no warnings.

```bash
git add Cargo.toml Cargo.lock crates/acd-inbox docs/superpowers/specs/2026-10-12-omarchy-inbox-design.md
git commit -m "acd-inbox: crate, wire types, error codes and bounded views and settings"
```

---

### Task 5: Paths, prefs and the settings snapshot

**Execution:** cloud-ok

**Files:**
- Create: `crates/acd-inbox/src/prefs.rs`
- Modify: `crates/acd-inbox/src/lib.rs` (add `pub mod prefs;`), `crates/acd-inbox/Cargo.toml` (`gethostname.workspace = true`), `Cargo.lock`
- Test: `crates/acd-inbox/src/prefs.rs` (unit)

**Interfaces:**
- Consumes: `protocol::SettingsSnapshot`; `Inbox::{settings, machines}` (Task 3).
- Produces (in `acd_inbox::prefs`):
  - `pub const SOCKET_NAME: &str = "acd-inbox.sock"; pub const LOCK_NAME: &str = "acd-inbox.lock";`
  - `pub struct Paths { pub config_dir: PathBuf, pub settings: PathBuf, pub service: PathBuf, pub runtime_dir: PathBuf, pub socket: PathBuf, pub lock: PathBuf }`
  - `Paths::new(config_home: &Path, runtime_dir: &Path) -> Paths` and `Paths::from_env() -> Result<Paths, PathsError>`.
  - `#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)] pub struct Prefs { pub notifications: bool }`, with `Default` (on), `Prefs::load(&Path) -> Prefs` and `Prefs::save(&self, &Path) -> std::io::Result<()>`.
  - `pub fn local_name(hostname: &str) -> String` (D18).
  - `pub fn init_settings(path: &Path, local_name: &str) -> Result<Settings, SettingsError>`.
  - `pub fn snapshot(inbox: &Inbox, prefs: Prefs) -> SettingsSnapshot`.

- [ ] **Step 1: Write the failing tests**

Create `crates/acd-inbox/src/prefs.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use acd_client::{
        Timing,
        inbox::InboxOptions,
        settings::{EndpointConfig, MachineEntry, MachineSecret, MemorySecrets, SecretStore},
    };
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;

    const TOKEN: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8";
    const PIN: &str = "sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

    #[test]
    fn paths_follow_the_spec_layout() {
        let p = Paths::new(Path::new("/home/u/.config"), Path::new("/run/user/1000"));
        assert_eq!(p.settings, Path::new("/home/u/.config/acd-inbox/settings.json"));
        assert_eq!(p.service, Path::new("/home/u/.config/acd-inbox/service.json"));
        assert_eq!(p.socket, Path::new("/run/user/1000/acd-inbox.sock"));
        assert_eq!(p.lock, Path::new("/run/user/1000/acd-inbox.lock"));
    }

    #[test]
    fn prefs_default_on_survive_a_round_trip_and_are_private() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("acd-inbox/service.json");
        assert_eq!(Prefs::load(&path), Prefs { notifications: true });
        Prefs { notifications: false }.save(&path).unwrap();
        assert_eq!(Prefs::load(&path), Prefs { notifications: false });
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(Prefs::load(&path), Prefs::default());
    }

    #[test]
    fn first_run_names_the_local_machine_after_this_host() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("acd-inbox/settings.json");
        let s = init_settings(&path, &local_name("omarchy.tail1234.ts.net")).unwrap();
        assert_eq!(s.machines[0].name, "omarchy");
        let again = init_settings(&path, "other").unwrap();
        assert_eq!(again.machines[0].name, "omarchy", "an existing file is kept");
        assert_eq!(local_name(""), "This computer");
        assert_eq!(local_name(&"h".repeat(100)).chars().count(), 64);
    }

    #[test]
    fn the_snapshot_has_status_and_toggles_but_no_token_or_pin() {
        let secrets = Arc::new(MemorySecrets::default());
        secrets.set("m1", &MachineSecret { token: TOKEN.into(), pin: PIN.into() }.to_json()).unwrap();
        let settings = Settings {
            show_subagents: true,
            machines: vec![MachineEntry {
                key: "m1".into(),
                name: "Box".into(),
                endpoint: EndpointConfig::Tcp { host: "box".into(), port: 47800 },
            }],
            ..Settings::fresh()
        };
        let inbox = acd_client::inbox::Inbox::new(
            settings,
            secrets,
            InboxOptions { timing: Timing::default(), settings_path: None, client_name: "t".into(), client_version: "0".into() },
        );
        let s = snapshot(&inbox, Prefs { notifications: false });
        let line = crate::protocol::settings_line(&s, crate::protocol::MAX_LINE);
        assert!(!line.contains(TOKEN) && !line.contains("sha256:"), "secrets in the settings line");
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["machines"][0]["secret"], "ok");
        assert_eq!(v["notifications"], false);
        assert_eq!(v["show_subagents"], true);
        assert!(v["machines"][0].get("status").is_none(), "connection status stays in the view (S1)");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Add `pub mod prefs;` to `lib.rs` and `gethostname.workspace = true` to `[dependencies]`.
Run: `cargo test -p acd-inbox --lib prefs` (updates `Cargo.lock`)
Expected: compile errors, `cannot find struct Paths`, and others.

- [ ] **Step 3: Implement `prefs.rs`**

Above the tests:

```rust
//! Files the service keeps (omarchy spec §4.1) and the `settings` snapshot
//! (§5.4).
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use acd_client::{
    inbox::Inbox,
    settings::{LOCAL_KEY, Settings, SettingsError},
};
use serde::{Deserialize, Serialize};

use crate::protocol::SettingsSnapshot;

pub const SOCKET_NAME: &str = "acd-inbox.sock";
pub const LOCK_NAME: &str = "acd-inbox.lock";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub settings: PathBuf,
    pub service: PathBuf,
    pub runtime_dir: PathBuf,
    pub socket: PathBuf,
    pub lock: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum PathsError {
    #[error("XDG_RUNTIME_DIR is not set; acd-inbox runs inside a user session")]
    NoRuntimeDir,
    #[error("neither XDG_CONFIG_HOME nor HOME is set")]
    NoConfigHome,
}

impl Paths {
    pub fn new(config_home: &Path, runtime_dir: &Path) -> Paths {
        let config_dir = config_home.join("acd-inbox");
        Paths {
            settings: config_dir.join("settings.json"),
            service: config_dir.join("service.json"),
            config_dir,
            runtime_dir: runtime_dir.to_path_buf(),
            socket: runtime_dir.join(SOCKET_NAME),
            lock: runtime_dir.join(LOCK_NAME),
        }
    }

    /// `$XDG_CONFIG_HOME` (else `~/.config`) and `$XDG_RUNTIME_DIR`.
    pub fn from_env() -> Result<Paths, PathsError> {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .filter(|v| !v.is_empty())
            .ok_or(PathsError::NoRuntimeDir)?;
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .ok_or(PathsError::NoConfigHome)?;
        Ok(Paths::new(&config, Path::new(&runtime)))
    }
}

/// `service.json` (omarchy spec §4.1). No secrets.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Prefs {
    #[serde(default = "on")]
    pub notifications: bool,
}

fn on() -> bool {
    true
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs { notifications: true }
    }
}

impl Prefs {
    /// A missing or unreadable file gives the defaults.
    pub fn load(path: &Path) -> Prefs {
        match std::fs::read(path) {
            Ok(raw) => serde_json::from_slice(&raw).unwrap_or_else(|_| {
                tracing::warn!("service.json is unreadable; using defaults");
                Prefs::default()
            }),
            Err(_) => Prefs::default(),
        }
    }

    /// Atomic write, mode 0600, parent directory 0700.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let tmp = path.with_extension("json.tmp");
        let _ = std::fs::remove_file(&tmp);
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        f.write_all(&serde_json::to_vec_pretty(self).expect("serializable"))?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    }
}

/// This host's first DNS label, at most 64 characters; else "This computer"
/// (plan D18).
pub fn local_name(hostname: &str) -> String {
    let label = hostname.split('.').next().unwrap_or("").trim();
    if label.is_empty() {
        return "This computer".into();
    }
    label.chars().take(64).collect()
}

/// Loads `settings.json`. On first run it writes one whose local machine is
/// named `local_name`, not "This Mac" (plan S6).
pub fn init_settings(path: &Path, local_name: &str) -> Result<Settings, SettingsError> {
    if std::fs::symlink_metadata(path).is_err() {
        let mut fresh = Settings::fresh();
        if let Some(local) = fresh.machines.iter_mut().find(|m| m.key == LOCAL_KEY) {
            local.name = local_name.to_owned();
        }
        fresh.save(path)?;
    }
    Settings::load_or_init(path)
}

/// The `settings` message. Calls `Inbox::machines`, which asks the
/// SecretStore for each machine's status: run it off the async workers.
pub fn snapshot(inbox: &Inbox, prefs: Prefs) -> SettingsSnapshot {
    let s = inbox.settings();
    SettingsSnapshot {
        machines: inbox.machines(),
        notifications: prefs.notifications,
        show_idle: s.show_idle,
        show_subagents: s.show_subagents,
        show_hook_only: s.show_hook_only,
    }
}
```

`gethostname` is used in Task 15 (`local_name(&gethostname::gethostname().to_string_lossy())`). The dependency goes in now so this task updates the lock file once.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p acd-inbox --locked --lib prefs`
Expected: PASS.

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy --workspace --all-targets --locked -- -D warnings`
Expected: no warnings.

```bash
git add Cargo.lock crates/acd-inbox
git commit -m "acd-inbox: paths, service prefs, first-run settings and the settings snapshot"
```

---

### Task 6: Socket startup, lock validation and peer check

**Execution:** cloud-ok

**Files:**
- Create: `crates/acd-inbox/src/startup.rs`, `crates/acd-inbox/tests/startup.rs`
- Modify: `crates/acd-inbox/src/lib.rs` (`pub mod startup;`), `crates/acd-inbox/Cargo.toml` (`libc.workspace = true`), `Cargo.lock`

**Interfaces:**
- Consumes: `acd_securefs::{check_private_dir, euid}`.
- Produces (in `acd_inbox::startup`):
  - `pub enum StartupError { RuntimeDir(PathBuf), AlreadyRunning(PathBuf), NotASocket(PathBuf), Io(PathBuf, std::io::Error) }`.
  - `pub fn check_runtime_dir(dir: &Path) -> Result<(), StartupError>`.
  - `pub struct LockFile` and `pub fn take_lock(path: &Path) -> Result<LockFile, StartupError>`. The opened descriptor is validated like `crates/acd/src/writer_lock.rs:17`: a regular file owned by the euid, with a looser mode repaired to 0600.
  - `pub fn clear_socket_path(lock: &LockFile, path: &Path) -> Result<(), StartupError>`.
  - `pub struct OwnedSocket`, which removes its path on drop while it is still the same inode.
  - `pub struct Listening { pub listener: tokio::net::UnixListener, pub socket: OwnedSocket, pub lock: LockFile }`.
  - `pub fn listen(runtime_dir: &Path, socket: &Path, lock: &Path) -> Result<Listening, StartupError>`; must be called inside a tokio runtime.
  - `pub fn peer_allowed(peer_uid: u32, own_uid: u32) -> bool`.
  - `pub fn peer_ok(stream: &tokio::net::UnixStream) -> bool` (D24).

- [ ] **Step 1: Write the failing tests**

`crates/acd-inbox/tests/startup.rs`:

```rust
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};

use acd_inbox::startup::{self, StartupError};

/// A private runtime dir. Canonical, because on macOS /var is a symlink.
fn runtime() -> (tempfile::TempDir, PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let p = std::fs::canonicalize(d.path()).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
    (d, p)
}

fn listen(dir: &Path) -> Result<startup::Listening, StartupError> {
    startup::listen(dir, &dir.join("acd-inbox.sock"), &dir.join("acd-inbox.lock"))
}

#[tokio::test]
async fn an_unsafe_runtime_dir_is_refused() {
    let (_d, dir) = runtime();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(listen(&dir), Err(StartupError::RuntimeDir(_))));
    let (_d2, real) = runtime();
    let (_d3, holder) = runtime();
    let link = holder.join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert!(matches!(startup::check_runtime_dir(&link), Err(StartupError::RuntimeDir(_))));
    assert!(startup::check_runtime_dir(&real).is_ok());
}

#[tokio::test]
async fn a_second_instance_exits() {
    let (_d, dir) = runtime();
    let _first = listen(&dir).unwrap();
    assert!(matches!(listen(&dir), Err(StartupError::AlreadyRunning(_))));
}

#[tokio::test]
async fn a_regular_file_or_a_symlink_at_the_socket_path_is_never_deleted() {
    let (_d, dir) = runtime();
    let path = dir.join("acd-inbox.sock");
    std::fs::write(&path, "keep me").unwrap();
    assert!(matches!(listen(&dir), Err(StartupError::NotASocket(_))));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep me");
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(dir.join("elsewhere"), &path).unwrap();
    assert!(matches!(listen(&dir), Err(StartupError::NotASocket(_))));
    assert!(std::fs::symlink_metadata(&path).unwrap().file_type().is_symlink());
}

#[tokio::test]
async fn a_stale_socket_is_replaced_and_the_new_one_is_0600() {
    let (_d, dir) = runtime();
    let path = dir.join("acd-inbox.sock");
    drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
    assert!(std::fs::symlink_metadata(&path).unwrap().file_type().is_socket());
    let l = listen(&dir).unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    let client = tokio::net::UnixStream::connect(&path).await.unwrap();
    let (server, _) = l.listener.accept().await.unwrap();
    assert!(startup::peer_ok(&server), "the same user passes");
    drop(client);
    drop(l);
    assert!(std::fs::symlink_metadata(&path).is_err(), "dropping the listener removes its socket");
}

#[test]
fn another_uid_is_refused() {
    assert!(startup::peer_allowed(1000, 1000));
    assert!(!startup::peer_allowed(0, 1000));
    assert!(!startup::peer_allowed(1001, 1000));
}

#[tokio::test]
async fn a_loose_lock_file_is_repaired_and_a_directory_refused() {
    let (_d, dir) = runtime();
    let lock = dir.join("acd-inbox.lock");
    std::fs::write(&lock, "").unwrap();
    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o666)).unwrap();
    let held = startup::take_lock(&lock).unwrap();
    assert_eq!(std::fs::metadata(&lock).unwrap().permissions().mode() & 0o7777, 0o600);
    drop(held);
    let (_d2, dir2) = runtime();
    std::fs::create_dir(dir2.join("acd-inbox.lock")).unwrap();
    assert!(matches!(listen(&dir2), Err(StartupError::Io(..))));
}

#[tokio::test]
async fn a_symlinked_lock_path_is_refused() {
    let (_d, dir) = runtime();
    std::os::unix::fs::symlink(dir.join("target"), dir.join("acd-inbox.lock")).unwrap();
    assert!(matches!(listen(&dir), Err(StartupError::Io(..))));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Add `pub mod startup;` to `lib.rs` and `libc.workspace = true` to `[dependencies]`.
Run: `cargo test -p acd-inbox --test startup` (updates `Cargo.lock`)
Expected: compile error, `could not find startup`.

- [ ] **Step 3: Implement `startup.rs`**

```rust
//! Socket startup (omarchy spec §4.4) and the peer check (§5.1).
use std::fs::{File, OpenOptions};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum StartupError {
    #[error("{0}: the runtime directory must be a directory you own, mode 0700, and not a symlink")]
    RuntimeDir(PathBuf),
    #[error("another acd-inbox is running ({0} is locked)")]
    AlreadyRunning(PathBuf),
    #[error("{0} exists and is not a socket; refusing to remove it")]
    NotASocket(PathBuf),
    #[error("{0}: {1}")]
    Io(PathBuf, std::io::Error),
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> StartupError + '_ {
    move |e| StartupError::Io(path.to_path_buf(), e)
}

/// Owned by the euid, mode within 0700, not a symlink (plan D23).
pub fn check_runtime_dir(dir: &Path) -> Result<(), StartupError> {
    acd_securefs::check_private_dir(dir, 0o700).map_err(|_| StartupError::RuntimeDir(dir.to_path_buf()))
}

/// Held for the service's whole life; released when the process exits.
pub struct LockFile(#[allow(dead_code)] File);

/// `O_NOFOLLOW`, mode 0600, exclusive `flock` without waiting.
pub fn take_lock(path: &Path) -> Result<LockFile, StartupError> {
    let f = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(io(path))?;
    validate_lock(&f, path)?;
    // SAFETY: a valid open descriptor; flock has no other preconditions.
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::EWOULDBLOCK) {
            return Err(StartupError::AlreadyRunning(path.to_path_buf()));
        }
        return Err(StartupError::Io(path.to_path_buf(), e));
    }
    Ok(LockFile(f))
}

/// Checks the opened descriptor, not the path, as `crates/acd/src/writer_lock.rs`
/// does: a regular file owned by the euid. A looser mode is repaired.
fn validate_lock(f: &File, path: &Path) -> Result<(), StartupError> {
    let m = f.metadata().map_err(io(path))?;
    if !m.file_type().is_file() || m.uid() != acd_securefs::euid() {
        return Err(StartupError::Io(
            path.to_path_buf(),
            std::io::Error::other("the lock file must be a regular file you own"),
        ));
    }
    if m.mode() & 0o7777 != 0o600 {
        f.set_permissions(std::fs::Permissions::from_mode(0o600)).map_err(io(path))?;
    }
    Ok(())
}

/// With the lock held: a socket at `path` is removed. Any other file type,
/// or a symlink, is left alone and refused.
pub fn clear_socket_path(_lock: &LockFile, path: &Path) -> Result<(), StartupError> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(StartupError::Io(path.to_path_buf(), e)),
        Ok(m) if m.file_type().is_socket() => std::fs::remove_file(path).map_err(io(path)),
        Ok(_) => Err(StartupError::NotASocket(path.to_path_buf())),
    }
}

/// The socket this process bound. Dropping it removes the path only while
/// it still names the same inode.
pub struct OwnedSocket {
    path: PathBuf,
    dev: u64,
    ino: u64,
}

impl Drop for OwnedSocket {
    fn drop(&mut self) {
        if let Ok(m) = std::fs::symlink_metadata(&self.path)
            && m.file_type().is_socket()
            && (m.dev(), m.ino()) == (self.dev, self.ino)
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

pub struct Listening {
    pub listener: tokio::net::UnixListener,
    pub socket: OwnedSocket,
    pub lock: LockFile,
}

/// §4.4 in order: check the runtime dir, take the lock, clear a stale
/// socket, bind, then set mode 0600. Call inside a tokio runtime.
pub fn listen(runtime_dir: &Path, socket: &Path, lock: &Path) -> Result<Listening, StartupError> {
    check_runtime_dir(runtime_dir)?;
    let lock = take_lock(lock)?;
    clear_socket_path(&lock, socket)?;
    let std_listener = std::os::unix::net::UnixListener::bind(socket).map_err(io(socket))?;
    let m = std::fs::symlink_metadata(socket).map_err(io(socket))?;
    let owned = OwnedSocket { path: socket.to_path_buf(), dev: m.dev(), ino: m.ino() };
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600)).map_err(io(socket))?;
    std_listener.set_nonblocking(true).map_err(io(socket))?;
    let listener = tokio::net::UnixListener::from_std(std_listener).map_err(io(socket))?;
    Ok(Listening { listener, socket: owned, lock })
}

pub fn peer_allowed(peer_uid: u32, own_uid: u32) -> bool {
    peer_uid == own_uid
}

/// The peer runs as this user. Fails closed when its credentials cannot be
/// read (plan D24).
pub fn peer_ok(stream: &tokio::net::UnixStream) -> bool {
    stream.peer_cred().is_ok_and(|c| peer_allowed(c.uid(), acd_securefs::euid()))
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p acd-inbox --locked --test startup`
Expected: PASS. Run as a normal user: `check_private_dir` refuses directories that root owns.

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy --workspace --all-targets --locked -- -D warnings`
Expected: no warnings.

```bash
git add Cargo.lock crates/acd-inbox
git commit -m "acd-inbox: socket startup contract and peer check"
```

---

### Task 7: Outbox

**Execution:** cloud-ok

**Files:**
- Create: `crates/acd-inbox/src/outbox.rs`
- Modify: `crates/acd-inbox/src/lib.rs` (`pub mod outbox;`)
- Test: `crates/acd-inbox/src/outbox.rs` (unit)

**Interfaces:**
- Consumes: `protocol::outcomes_lost_line`.
- Produces (in `acd_inbox::outbox`), following D12:
  - `pub const OUTCOME_QUEUE: usize = 256; pub const REPLY_QUEUE: usize = 1024;`
  - `pub struct Outbox` with:
    - `new(hello: String, outcome_cap: usize, reply_cap: usize) -> Outbox`
    - `reply(&self, line: String) -> bool`: false means the reply queue is full; the caller closes the connection (a view that sends faster than it reads)
    - `outcome(&self, line: String)`
    - `outcomes_lost(&self)`
    - `set_settings(&self, line: String)`
    - `set_view(&self, line: String)`
    - `close(&self)`
    - `is_closed(&self) -> bool`
    - `async fn next(&self) -> Option<String>`, which returns `None` once closed
- Order (D12): `hello`, `settings` and `view` first, in that order. After that the writer takes turns over four classes (replies, outcomes with a pending `outcomes_lost` first, settings, view), resuming after the class it served last, so no class starves another.

- [ ] **Step 1: Write the failing tests**

Create `crates/acd-inbox/src/outbox.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    async fn next(o: &Outbox) -> String {
        tokio::time::timeout(Duration::from_secs(1), o.next()).await.expect("a line is due").expect("open")
    }

    /// Nothing is due: `next` stays pending. Checked with a poll, not a wait.
    fn nothing_due(o: &Outbox) {
        assert_eq!(o.take(), None);
    }

    async fn greeted(outcome_cap: usize, reply_cap: usize) -> Outbox {
        let o = Outbox::new("hello".into(), outcome_cap, reply_cap);
        o.set_settings("s0".into());
        o.set_view("v0".into());
        for want in ["hello", "s0", "v0"] {
            assert_eq!(next(&o).await, want);
        }
        o
    }

    #[tokio::test]
    async fn hello_settings_and_view_come_first_in_that_order() {
        let o = Outbox::new("hello".into(), OUTCOME_QUEUE, REPLY_QUEUE);
        o.set_view("v1".into());
        o.outcome("o1".into());
        assert!(o.reply("r1".into()));
        assert_eq!(next(&o).await, "hello");
        nothing_due(&o);
        o.set_settings("s1".into());
        assert_eq!(next(&o).await, "s1");
        assert_eq!(next(&o).await, "v1");
        let mut rest = vec![next(&o).await, next(&o).await];
        rest.sort();
        assert_eq!(rest, ["o1", "r1"]);
    }

    #[tokio::test]
    async fn views_and_settings_coalesce_to_the_newest() {
        let o = greeted(OUTCOME_QUEUE, REPLY_QUEUE).await;
        o.set_view("a".into());
        o.set_settings("x".into());
        o.set_view("b".into());
        o.set_settings("y".into());
        let mut got = vec![next(&o).await, next(&o).await];
        got.sort();
        assert_eq!(got, ["b", "y"]);
        nothing_due(&o);
    }

    #[tokio::test]
    async fn classes_take_turns_so_replies_cannot_starve_outcomes() {
        let o = greeted(OUTCOME_QUEUE, REPLY_QUEUE).await;
        for i in 1..=3 {
            assert!(o.reply(format!("r{i}")));
            o.outcome(format!("o{i}"));
        }
        o.set_settings("s".into());
        o.set_view("v".into());
        let got: Vec<String> = [(); 8].iter().map(|_| o.take().unwrap().unwrap()).collect();
        assert_eq!(got, ["r1", "o1", "s", "v", "r2", "o2", "r3", "o3"]);
    }

    #[tokio::test]
    async fn outcomes_are_never_coalesced_and_overflow_sends_one_loss() {
        let o = greeted(3, REPLY_QUEUE).await;
        o.outcome("o1".into());
        o.outcome("o2".into());
        assert_eq!(next(&o).await, "o1");
        assert_eq!(next(&o).await, "o2");
        for i in 0..5 {
            o.outcome(format!("x{i}"));
        }
        assert_eq!(next(&o).await, crate::protocol::outcomes_lost_line());
        assert_eq!(next(&o).await, "x4", "outcomes after the overflow queue again");
        nothing_due(&o);
    }

    #[tokio::test]
    async fn an_upstream_lag_drops_the_queue_and_reports_once() {
        let o = greeted(OUTCOME_QUEUE, REPLY_QUEUE).await;
        o.outcome("o1".into());
        o.outcomes_lost();
        o.outcomes_lost();
        assert_eq!(next(&o).await, crate::protocol::outcomes_lost_line());
        nothing_due(&o);
    }

    #[tokio::test]
    async fn a_full_reply_queue_refuses_more() {
        let o = greeted(OUTCOME_QUEUE, 2).await;
        assert!(o.reply("r1".into()));
        assert!(o.reply("r2".into()));
        assert!(!o.reply("r3".into()));
    }

    #[tokio::test]
    async fn closing_ends_the_stream() {
        let o = greeted(OUTCOME_QUEUE, REPLY_QUEUE).await;
        assert!(o.reply("r".into()));
        o.close();
        assert!(o.is_closed());
        assert_eq!(o.next().await, None);
        assert!(o.reply("ignored".into()), "a reply to a closed view is dropped, not an overflow");
        assert_eq!(o.next().await, None);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Add `pub mod outbox;` to `lib.rs`.
Run: `cargo test -p acd-inbox --locked --lib outbox`
Expected: compile error, `cannot find struct Outbox`.

- [ ] **Step 3: Implement `outbox.rs`**

```rust
//! One connection's outgoing lines (omarchy spec §5.1, plan D12): hello,
//! settings and view first; then replies, outcomes (bounded, never
//! coalesced) and the newest settings and view (coalesced), taking turns.
use std::collections::VecDeque;
use std::sync::Mutex;

use tokio::sync::Notify;

use crate::protocol;

pub const OUTCOME_QUEUE: usize = 256;
/// Replies a view may have unread before it is disconnected.
pub const REPLY_QUEUE: usize = 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Hello,
    Settings,
    View,
    Open,
}

struct State {
    phase: Phase,
    hello: Option<String>,
    replies: VecDeque<String>,
    outcomes: VecDeque<String>,
    lost: bool,
    settings: Option<String>,
    view: Option<String>,
    /// The class served last: 0 replies, 1 outcomes, 2 settings, 3 view.
    last: usize,
    closed: bool,
}

pub struct Outbox {
    state: Mutex<State>,
    ready: Notify,
    outcome_cap: usize,
    reply_cap: usize,
}

impl Outbox {
    pub fn new(hello: String, outcome_cap: usize, reply_cap: usize) -> Outbox {
        Outbox {
            state: Mutex::new(State {
                phase: Phase::Hello,
                hello: Some(hello),
                replies: VecDeque::new(),
                outcomes: VecDeque::new(),
                lost: false,
                settings: None,
                view: None,
                last: 3,
                closed: false,
            }),
            ready: Notify::new(),
            outcome_cap,
            reply_cap,
        }
    }

    fn update(&self, f: impl FnOnce(&mut State)) {
        let mut s = self.state.lock().unwrap();
        if s.closed {
            return;
        }
        f(&mut s);
        drop(s);
        self.ready.notify_one();
    }

    /// Queues a reply. False when the queue is full: the caller closes the
    /// connection. A closed outbox drops the reply and returns true.
    pub fn reply(&self, line: String) -> bool {
        let mut s = self.state.lock().unwrap();
        if s.closed {
            return true;
        }
        if s.replies.len() >= self.reply_cap {
            return false;
        }
        s.replies.push_back(line);
        drop(s);
        self.ready.notify_one();
        true
    }

    /// Queues an outcome. A full queue is dropped and one `outcomes_lost`
    /// becomes due instead (omarchy spec §5.1).
    pub fn outcome(&self, line: String) {
        let cap = self.outcome_cap;
        self.update(|s| {
            if s.outcomes.len() >= cap {
                s.outcomes.clear();
                s.lost = true;
            } else {
                s.outcomes.push_back(line);
            }
        });
    }

    /// The service's own outcome subscription lagged.
    pub fn outcomes_lost(&self) {
        self.update(|s| {
            s.outcomes.clear();
            s.lost = true;
        });
    }

    pub fn set_settings(&self, line: String) {
        self.update(|s| s.settings = Some(line));
    }

    pub fn set_view(&self, line: String) {
        self.update(|s| s.view = Some(line));
    }

    pub fn close(&self) {
        self.state.lock().unwrap().closed = true;
        self.ready.notify_one();
    }

    pub fn is_closed(&self) -> bool {
        self.state.lock().unwrap().closed
    }

    /// The next line to write, waiting until one is due; `None` once closed.
    pub async fn next(&self) -> Option<String> {
        loop {
            let notified = self.ready.notified();
            if let Some(next) = self.take() {
                return next;
            }
            notified.await;
        }
    }

    /// `Some(None)`: closed. `None`: nothing due yet.
    fn take(&self) -> Option<Option<String>> {
        let mut s = self.state.lock().unwrap();
        if s.closed {
            return Some(None);
        }
        match s.phase {
            Phase::Hello => {
                s.phase = Phase::Settings;
                return Some(s.hello.take());
            }
            Phase::Settings => {
                let line = s.settings.take()?;
                s.phase = Phase::View;
                return Some(Some(line));
            }
            Phase::View => {
                let line = s.view.take()?;
                s.phase = Phase::Open;
                return Some(Some(line));
            }
            Phase::Open => {}
        }
        for step in 1..=4 {
            let class = (s.last + step) % 4;
            let line = match class {
                0 => s.replies.pop_front(),
                1 if s.lost => {
                    s.lost = false;
                    Some(protocol::outcomes_lost_line())
                }
                1 => s.outcomes.pop_front(),
                2 => s.settings.take(),
                _ => s.view.take(),
            };
            if let Some(line) = line {
                s.last = class;
                return Some(Some(line));
            }
        }
        None
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p acd-inbox --locked --lib outbox`
Expected: PASS.

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy --workspace --all-targets --locked -- -D warnings`
Expected: no warnings.

```bash
git add crates/acd-inbox
git commit -m "acd-inbox: per-connection outbox with greeting order, fair turns and bounded queues"
```

---

### Task 8: Connections: admission, remembered ids and close

**Execution:** cloud-ok

**Files:**
- Create: `crates/acd-inbox/src/server.rs` (the `Service` core; Task 9 adds the socket side), `crates/acd-inbox/src/commands.rs` (envelope; Task 10 adds the table)
- Create: `crates/acd-inbox/tests/common/mod.rs`, `crates/acd-inbox/tests/admission.rs`
- Modify: `crates/acd-inbox/src/lib.rs` (`pub mod commands; pub mod server;`)

**Interfaces:**
- Consumes:
  - Task 1: `Inbox::{cancel_owned, pick_owned}` and `ActionOwner`.
  - Task 4: the line builders.
  - Task 5: `prefs::{Prefs, snapshot}`.
  - Task 7: `outbox::{Outbox, OUTCOME_QUEUE, REPLY_QUEUE}`.
- Produces (in `acd_inbox::server`):
  - `pub trait SecretControl: Send + Sync { fn unlock(&self) -> Result<(), SecretError>; fn refresh(&self, keys: &[String]); }`. Both methods block. `pub struct NoKeyring;` is a control that never locks.
  - `#[derive(Clone, Debug)] pub struct ServerOptions { pub max_line: usize, pub write_timeout: Duration, pub outcome_queue: usize, pub reply_queue: usize, pub remembered_ids: usize, pub version: String }`. `Default` gives 4 MiB, 30 s, 256, 1024, 1024 and the crate version.
  - `pub enum CloseReason { Eof, ReadError, Oversize, Malformed, WriteTimeout, WriteError, ReplyOverflow }`, with `tag()`.
  - `pub struct Conn { pub id: u64, pub owner: ActionOwner, pub outbox: Outbox, .. }`.
  - `pub struct Service { pub inbox: Arc<Inbox>, pub opts: ServerOptions, .. }`, with:
    - `Service::new(inbox: Arc<Inbox>, prefs: Prefs, prefs_path: Option<PathBuf>, secrets: Arc<dyn SecretControl>, opts: ServerOptions) -> Arc<Service>`
    - `panel_open(&self) -> watch::Receiver<bool>`, `prefs(&self) -> watch::Receiver<Prefs>`, `connections(&self) -> usize`
    - `close(&self, conn: &Conn, why: CloseReason)` (D29)
    - `admit<R>(&self, conn: &Conn, f: impl FnOnce() -> R) -> Option<R>` (D31)
    - crate-internal: `register() -> Arc<Conn>`, `set_panel`, `set_notifications`, `reply(&self, conn, line)`, `secrets`
    - under `feature = "testing"`: `connect_for_test(&self) -> Arc<Conn>` (a registered connection without a socket), `set_admission_hook(&self, hook: Arc<dyn Fn() + Send + Sync>)`, and `Conn::gate_waiters_for_test(&self) -> usize`
- Produces (in `acd_inbox::commands`): `pub struct Malformed;` and `pub fn handle(svc: &Arc<Service>, conn: &Arc<Conn>, line: &[u8]) -> Result<(), Malformed>` (D10). In this task `pick`, `answer_text`, `reply` and `panel` are wired, each admitted (D31); every other well-formed command replies `unsupported` until Task 10.

The rule this task pins (D31): a connection has one admission lock. Each command that may create an action or set the panel flag runs under it, after checking that the connection is still open. `close` takes the same lock to mark the connection closed and call `cancel_owned`. So an action is either created before the close, and the close cancels it, or it is refused. A `panel(open)` that arrives after the close is refused too.

- [ ] **Step 1: Write the test rig (inbox part)**

`crates/acd-inbox/tests/common/mod.rs` (Task 9 appends the socket part):

```rust
#![allow(dead_code)]
use std::sync::Arc;
use std::time::Duration;

use acd_client::{
    Timing,
    conn::MachineStatus,
    fake::{FakeConn, FakeServer, Identity},
    inbox::{Inbox, InboxOptions},
    settings::{EndpointConfig, MachineEntry, MachineSecret, MemorySecrets, SecretStore, Settings},
    view::InboxView,
};
use serde_json::{Value, json};

pub const TOKEN: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8";

pub fn fast() -> Timing {
    let ms = Duration::from_millis;
    Timing {
        backoff_base: ms(20),
        backoff_max: ms(100),
        identity_retry: ms(200),
        idle_ping: ms(500),
        ping_timeout: ms(300),
        undo: ms(200),
        not_ready_interval: ms(50),
        connect_total: ms(3000),
        ..Timing::default()
    }
}

/// Countdowns that never end on their own; tests end them with
/// `expire_countdowns_for_test`.
pub fn held() -> Timing {
    Timing { undo: Duration::from_secs(3600), ..fast() }
}

/// An agent with an open permission request that also takes a typed answer.
pub fn pending_agent(id: &str, request: &str) -> Value {
    json!({"id":id,"machine_id":"m","source":"herdr","kind":"claude","title":"agent","cwd":null,"project":null,"state":"needs_you","state_since":"2026-10-08T00:00:00.000Z","failed":false,"last_message":null,"can_send":true,"pending":{"id":request,"revision":1,"agent_id":id,"kind":"permission","status":"open","answer_path":"hook","title":"Bash","detail":"npm test","options":[{"id":"allow","label":"Yes","description":null,"effect":"allow"},{"id":"deny","label":"No","description":null,"effect":"deny"}],"questions":null,"free_text":true,"raw_screen":null,"opened_at":"2026-10-08T00:00:00.000Z"}})
}

pub fn box_secret(f: &FakeServer) -> String {
    MachineSecret { token: TOKEN.into(), pin: f.pin() }.to_json()
}

pub fn box_settings(f: &FakeServer) -> Settings {
    Settings {
        machines: vec![MachineEntry {
            key: "m1".into(),
            name: "Box".into(),
            endpoint: EndpointConfig::Tcp { host: "127.0.0.1".into(), port: f.port() },
        }],
        ..Settings::fresh()
    }
}

/// A fake daemon and a started inbox over `store` (default: memory with
/// m1's secret). With `agents`, waits for the snapshot to show.
pub async fn inbox_rig(
    agents: Option<Vec<Value>>,
    timing: Timing,
    store: Option<Box<dyn FnOnce(&FakeServer) -> Arc<dyn SecretStore> + Send>>,
) -> (FakeServer, Option<FakeConn>, Arc<Inbox>) {
    let f = FakeServer::start(TOKEN, Identity::Paired).await;
    let store = match store {
        Some(make) => make(&f),
        None => {
            let s = Arc::new(MemorySecrets::default());
            s.set("m1", &box_secret(&f)).unwrap();
            s as Arc<dyn SecretStore>
        }
    };
    let inbox = Inbox::new(
        box_settings(&f),
        store,
        InboxOptions { timing, settings_path: None, client_name: "t".into(), client_version: "0".into() },
    );
    inbox.start();
    let Some(agents) = agents else { return (f, None, inbox) };
    let c = f.next_connection().await;
    c.send(json!({"type":"snapshot","epoch":"e1","seq":1,"agents":agents})).await;
    let want = agents.len();
    view_until(&inbox, move |v| v.cards.len() >= want && v.machines.iter().any(|m| m.status == MachineStatus::Online)).await;
    (f, Some(c), inbox)
}

pub async fn view_until(ib: &Inbox, pred: impl Fn(&InboxView) -> bool) {
    let mut rx = ib.view();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if pred(&rx.borrow()) {
                break;
            }
            rx.changed().await.unwrap();
        }
    })
    .await
    .expect("the view never matched");
}

/// Polls `cond` (no fixed sleep) for up to 5 s.
pub async fn until(what: &str, cond: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !cond() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

/// The first frame of this type the fake daemon received.
pub async fn frame_of_type(f: &FakeServer, ty: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(v) = f.received().into_iter().find(|v| v["type"] == ty) {
                return v;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no {ty} frame"))
}

pub fn answers(f: &FakeServer) -> usize {
    f.received().iter().filter(|v| v["type"] == "request.answer").count()
}
```

- [ ] **Step 2: Write the failing tests**

`crates/acd-inbox/tests/admission.rs`:

```rust
mod common;

use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::Arc;

use acd_client::voice::ActionOutcome;
use acd_inbox::commands;
use acd_inbox::prefs::Prefs;
use acd_inbox::server::{CloseReason, NoKeyring, ServerOptions, Service};
use common::*;

async fn service(timing: acd_client::Timing) -> (acd_client::fake::FakeServer, Arc<acd_client::inbox::Inbox>, Arc<Service>) {
    let (f, _c, inbox) = inbox_rig(Some(vec![pending_agent("a1", "r1")]), timing, None).await;
    let svc = Service::new(inbox.clone(), Prefs::default(), None, Arc::new(NoKeyring), ServerOptions::default());
    (f, inbox, svc)
}

const PICK: &[u8] = br#"{"type":"pick","id":"p1","card":"m1/a1","request":"r1","option":"allow","revision":1}"#;

/// The hook parks every admitted command inside the admission lock until
/// the `Release` guard is dropped, panics included, so a failed assertion
/// never leaves a thread parked.
fn park(svc: &Service) -> (Arc<AtomicBool>, Release) {
    let inside = Arc::new(AtomicBool::new(false));
    let released = Arc::new(AtomicBool::new(false));
    let (i, r) = (inside.clone(), released.clone());
    svc.set_admission_hook(Arc::new(move || {
        i.store(true, SeqCst);
        while !r.load(SeqCst) {
            std::thread::yield_now();
        }
    }));
    (inside, Release(released))
}

struct Release(Arc<AtomicBool>);

impl Drop for Release {
    fn drop(&mut self) {
        self.0.store(true, SeqCst);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_close_during_a_pick_cancels_the_new_action() {
    let (f, inbox, svc) = service(held()).await;
    let mut rx = inbox.action_outcomes();
    let conn = svc.connect_for_test();
    let (inside, release) = park(&svc);
    let picking = {
        let (svc, conn) = (svc.clone(), conn.clone());
        tokio::task::spawn_blocking(move || commands::handle(&svc, &conn, PICK).unwrap())
    };
    until("the pick inside the admission lock", || inside.load(SeqCst)).await;
    let closing = {
        let (svc, conn) = (svc.clone(), conn.clone());
        tokio::task::spawn_blocking(move || svc.close(&conn, CloseReason::Eof))
    };
    until("the close to wait for the admission lock", || conn.gate_waiters_for_test() == 1).await;
    drop(release);
    picking.await.unwrap();
    closing.await.unwrap();
    let event = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await.unwrap().unwrap();
    assert_eq!(event.outcome, ActionOutcome::Cancelled, "the action created before the close was cancelled by it");
    inbox.expire_countdowns_for_test();
    view_until(&inbox, |v| v.actions.is_empty()).await;
    assert_eq!(answers(&f), 0);
}

#[tokio::test]
async fn a_command_after_close_is_refused() {
    let (_f, inbox, svc) = service(held()).await;
    let conn = svc.connect_for_test();
    svc.close(&conn, CloseReason::Eof);
    commands::handle(&svc, &conn, PICK).unwrap();
    commands::handle(&svc, &conn, br#"{"type":"panel","id":"p2","open":true}"#).unwrap();
    assert!(inbox.stamped_view().view.actions.is_empty() && !*svc.panel_open().borrow());
    view_until(&inbox, |v| v.actions.is_empty()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_late_panel_open_cannot_stick_after_close() {
    let (_f, _inbox, svc) = service(held()).await;
    let conn = svc.connect_for_test();
    let (inside, release) = park(&svc);
    let opening = {
        let (svc, conn) = (svc.clone(), conn.clone());
        tokio::task::spawn_blocking(move || {
            commands::handle(&svc, &conn, br#"{"type":"panel","id":"o1","open":true}"#).unwrap()
        })
    };
    until("panel(open) inside the admission lock", || inside.load(SeqCst)).await;
    let closing = {
        let (svc, conn) = (svc.clone(), conn.clone());
        tokio::task::spawn_blocking(move || svc.close(&conn, CloseReason::Eof))
    };
    until("the close to wait", || conn.gate_waiters_for_test() == 1).await;
    drop(release);
    opening.await.unwrap();
    closing.await.unwrap();
    assert!(!*svc.panel_open().borrow(), "the panel flag of a closed view was left set");
}

#[tokio::test]
async fn ids_are_remembered_up_to_a_bound() {
    let (_f, inbox, _svc) = service(held()).await;
    let svc = Service::new(
        inbox.clone(),
        Prefs::default(),
        None,
        Arc::new(NoKeyring),
        ServerOptions { remembered_ids: 2, ..Default::default() },
    );
    let conn = svc.connect_for_test();
    for id in ["a", "b", "a", "c", "a"] {
        commands::handle(&svc, &conn, format!(r#"{{"type":"escape","id":"{id}"}}"#).as_bytes()).unwrap();
    }
    let codes: Vec<String> = std::iter::from_fn(|| conn.outbox.try_next_for_test())
        .skip(3) // hello, settings, view
        .map(|l| serde_json::from_str::<serde_json::Value>(&l).unwrap()["code"].as_str().unwrap_or("").to_owned())
        .collect();
    assert_eq!(codes, ["unsupported", "unsupported", "duplicate_id", "unsupported", "unsupported"],
        "a repeat within the last 2 ids is refused; an older one is forgotten");
}
```

`ids_are_remembered_up_to_a_bound` needs `Outbox::try_next_for_test(&self) -> Option<String>` (`#[cfg(any(test, feature = "testing"))]`, returning `self.take().flatten()`); add it to `outbox.rs` in this task. `escape` replies `unsupported` until Task 10, which updates this test's expected codes to `ok`.

- [ ] **Step 3: Run the tests to verify they fail**

Add `pub mod commands; pub mod server;` to `lib.rs`.
Run: `cargo test -p acd-inbox --locked --test admission`
Expected: compile error, `could not find server in acd_inbox`.

- [ ] **Step 4: Implement the `Service` core in `server.rs`**

```rust
//! The service (omarchy spec §5): connections, admission and close. Task 9
//! adds the socket side.
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use acd_client::{actions::ActionOwner, inbox::Inbox, settings::SecretError};
use tokio::sync::watch;

use crate::outbox::{OUTCOME_QUEUE, Outbox, REPLY_QUEUE};
use crate::prefs::{self, Prefs};
use crate::protocol;

/// What the server needs from the keyring (§9). Both calls block: run them
/// on `spawn_blocking`.
pub trait SecretControl: Send + Sync {
    /// Unlocks the keyring; may wait for the user's prompt.
    fn unlock(&self) -> Result<(), SecretError>;
    /// Reads `keys` from the keyring again, replacing cached values and
    /// cached errors (plan D14).
    fn refresh(&self, keys: &[String]);
}

/// A keyring that is never locked: tests, and stores without one.
pub struct NoKeyring;

impl SecretControl for NoKeyring {
    fn unlock(&self) -> Result<(), SecretError> {
        Ok(())
    }
    fn refresh(&self, _: &[String]) {}
}

#[derive(Clone, Debug)]
pub struct ServerOptions {
    pub max_line: usize,
    pub write_timeout: Duration,
    pub outcome_queue: usize,
    pub reply_queue: usize,
    /// Command ids remembered per connection for duplicate detection.
    pub remembered_ids: usize,
    pub version: String,
}

impl Default for ServerOptions {
    fn default() -> Self {
        ServerOptions {
            max_line: protocol::MAX_LINE,
            write_timeout: Duration::from_secs(30),
            outcome_queue: OUTCOME_QUEUE,
            reply_queue: REPLY_QUEUE,
            remembered_ids: 1024,
            version: env!("CARGO_PKG_VERSION").into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseReason {
    Eof,
    ReadError,
    Oversize,
    Malformed,
    WriteTimeout,
    WriteError,
    ReplyOverflow,
}

impl CloseReason {
    pub fn tag(self) -> &'static str {
        match self {
            CloseReason::Eof => "eof",
            CloseReason::ReadError => "read_error",
            CloseReason::Oversize => "oversize",
            CloseReason::Malformed => "malformed",
            CloseReason::WriteTimeout => "write_timeout",
            CloseReason::WriteError => "write_error",
            CloseReason::ReplyOverflow => "reply_overflow",
        }
    }
}

/// The last `cap` command ids of a connection (plan D10).
pub(crate) struct RecentIds {
    order: VecDeque<String>,
    set: HashSet<String>,
    cap: usize,
}

impl RecentIds {
    fn new(cap: usize) -> Self {
        RecentIds { order: VecDeque::new(), set: HashSet::new(), cap }
    }
    /// False when `id` is among the remembered ones.
    pub(crate) fn insert(&mut self, id: &str) -> bool {
        if self.set.contains(id) {
            return false;
        }
        if self.order.len() >= self.cap
            && let Some(old) = self.order.pop_front()
        {
            self.set.remove(&old);
        }
        self.order.push_back(id.to_owned());
        self.set.insert(id.to_owned());
        true
    }
}

struct ConnState {
    closed: bool,
    ids: RecentIds,
}

/// One view connection.
pub struct Conn {
    pub id: u64,
    pub owner: ActionOwner,
    pub outbox: Outbox,
    /// The admission lock (plan D31).
    gate: Mutex<ConnState>,
    waiting: AtomicUsize,
    pub(crate) shut: watch::Sender<bool>,
}

impl Conn {
    fn lock(&self) -> std::sync::MutexGuard<'_, ConnState> {
        self.waiting.fetch_add(1, Ordering::SeqCst);
        let g = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        self.waiting.fetch_sub(1, Ordering::SeqCst);
        g
    }

    /// False when the id was seen among the remembered ones. Refused (and
    /// so true) once closed: nothing is replied then anyway.
    pub(crate) fn remember(&self, id: &str) -> bool {
        self.lock().ids.insert(id)
    }

    #[cfg(feature = "testing")]
    pub fn gate_waiters_for_test(&self) -> usize {
        self.waiting.load(Ordering::SeqCst)
    }
}

/// Connections and the newest published lines, under one lock, so a new
/// connection is seeded and a broadcast reaches it without a gap or an
/// older line overwriting a newer one (plan D32).
pub(crate) struct Registry {
    pub(crate) conns: HashMap<u64, Arc<Conn>>,
    pub(crate) settings: (u64, String),
    pub(crate) view: String,
}

pub struct Service {
    pub inbox: Arc<Inbox>,
    pub(crate) secrets: Arc<dyn SecretControl>,
    prefs_tx: watch::Sender<Prefs>,
    prefs_path: Option<PathBuf>,
    pub(crate) registry: Mutex<Registry>,
    pub(crate) settings_gen: AtomicU64,
    next_conn: AtomicU64,
    panels: Mutex<HashSet<u64>>,
    panel_tx: watch::Sender<bool>,
    #[cfg(feature = "testing")]
    hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    pub opts: ServerOptions,
}

impl Service {
    pub fn new(
        inbox: Arc<Inbox>,
        prefs: Prefs,
        prefs_path: Option<PathBuf>,
        secrets: Arc<dyn SecretControl>,
        opts: ServerOptions,
    ) -> Arc<Service> {
        let settings = protocol::settings_line(&prefs::snapshot(&inbox, prefs), opts.max_line);
        let view = protocol::view_or_overflow_line(&inbox.view().borrow().clone(), opts.max_line);
        Arc::new(Service {
            inbox,
            secrets,
            prefs_tx: watch::Sender::new(prefs),
            prefs_path,
            registry: Mutex::new(Registry { conns: HashMap::new(), settings: (0, settings), view }),
            settings_gen: AtomicU64::new(0),
            next_conn: AtomicU64::new(1),
            panels: Mutex::new(HashSet::new()),
            panel_tx: watch::Sender::new(false),
            #[cfg(feature = "testing")]
            hook: Mutex::new(None),
            opts,
        })
    }

    /// True while any view reports its panel open (§8).
    pub fn panel_open(&self) -> watch::Receiver<bool> {
        self.panel_tx.subscribe()
    }

    pub fn prefs(&self) -> watch::Receiver<Prefs> {
        self.prefs_tx.subscribe()
    }

    pub fn connections(&self) -> usize {
        self.registry.lock().unwrap().conns.len()
    }

    /// A new connection, registered (and so subscribed to outcomes, §5.3)
    /// and seeded with `hello`, the newest `settings` and the newest `view`
    /// under the registry lock.
    pub(crate) fn register(&self) -> Arc<Conn> {
        let id = self.next_conn.fetch_add(1, Ordering::Relaxed);
        let conn = Arc::new(Conn {
            id,
            owner: ActionOwner(id),
            outbox: Outbox::new(protocol::hello_line(&self.opts.version), self.opts.outcome_queue, self.opts.reply_queue),
            gate: Mutex::new(ConnState { closed: false, ids: RecentIds::new(self.opts.remembered_ids) }),
            waiting: AtomicUsize::new(0),
            shut: watch::Sender::new(false),
        });
        let mut reg = self.registry.lock().unwrap();
        conn.outbox.set_settings(reg.settings.1.clone());
        conn.outbox.set_view(reg.view.clone());
        reg.conns.insert(id, conn.clone());
        conn
    }

    #[cfg(feature = "testing")]
    pub fn connect_for_test(&self) -> Arc<Conn> {
        self.register()
    }

    #[cfg(feature = "testing")]
    pub fn set_admission_hook(&self, hook: Arc<dyn Fn() + Send + Sync>) {
        *self.hook.lock().unwrap() = Some(hook);
    }

    /// Runs `f` under the connection's admission lock if it is still open
    /// (plan D31). `f` must not call `close` or `admit`.
    pub fn admit<R>(&self, conn: &Conn, f: impl FnOnce() -> R) -> Option<R> {
        let state = conn.lock();
        if state.closed {
            return None;
        }
        #[cfg(feature = "testing")]
        if let Some(hook) = self.hook.lock().unwrap().clone() {
            hook();
        }
        let r = f();
        drop(state);
        Some(r)
    }

    /// Queues a reply; a view with too many unread replies is closed.
    pub(crate) fn reply(&self, conn: &Conn, line: String) {
        if !conn.outbox.reply(line) {
            self.close(conn, CloseReason::ReplyOverflow);
        }
    }

    /// Called only inside `admit`.
    pub(crate) fn set_panel(&self, conn: u64, open: bool) {
        let mut panels = self.panels.lock().unwrap();
        if open {
            panels.insert(conn);
        } else {
            panels.remove(&conn);
        }
        let any = !panels.is_empty();
        self.panel_tx.send_if_modified(|v| std::mem::replace(v, any) != any);
    }

    pub(crate) fn set_notifications(&self, on: bool) -> std::io::Result<()> {
        let mut p = *self.prefs_tx.borrow();
        p.notifications = on;
        if let Some(path) = &self.prefs_path {
            p.save(path)?;
        }
        self.prefs_tx.send_replace(p);
        Ok(())
    }

    /// Ends a connection, once (plan D29, D31). Under the admission lock it
    /// is marked closed, its unsent countdowns are cancelled (§6) and its
    /// panel flag is cleared; then it leaves the registry.
    pub fn close(&self, conn: &Conn, why: CloseReason) {
        let cancelled = {
            let mut state = conn.lock();
            if state.closed {
                return;
            }
            state.closed = true;
            let cancelled = self.inbox.cancel_owned(conn.owner);
            self.set_panel(conn.id, false);
            cancelled
        };
        self.registry.lock().unwrap().conns.remove(&conn.id);
        conn.outbox.close();
        conn.shut.send_replace(true);
        tracing::info!(conn = conn.id, reason = why.tag(), cancelled, "connection closed");
    }
}
```

Lock order: a connection's admission lock, then the inbox's `inner` (inside `pick_owned` and `cancel_owned`) or `panels`. Nothing takes them the other way round.

- [ ] **Step 5: Implement the envelope in `commands.rs`**

```rust
//! Commands from views (omarchy spec §5.2, plan D10, D11, D31).
use std::sync::Arc;

use acd_client::inbox::PickError;
use serde_json::{Map, Value};

use crate::protocol::{self, ErrorCode};
use crate::server::{Conn, Service};

/// The line is not a JSON object with a string `type` and a string `id` of
/// 1 to 64 bytes. The connection closes (§11).
#[derive(Debug)]
pub struct Malformed;

/// The command types this service knows. A view's `type` text is logged
/// only when it is one of these; anything else as "unknown".
const KNOWN: &[&str] = &[
    "pick", "answer_text", "reply", "cancel", "escape", "mark_seen", "set_show", "pair", "rename_machine",
    "remove_machine", "retry_machine", "unlock_secrets", "set_notifications", "panel",
];

pub fn handle(svc: &Arc<Service>, conn: &Arc<Conn>, line: &[u8]) -> Result<(), Malformed> {
    let Ok(Value::Object(m)) = serde_json::from_slice::<Value>(line) else {
        return Err(Malformed);
    };
    let (Some(ty), Some(id)) = (m.get("type").and_then(Value::as_str), m.get("id").and_then(Value::as_str)) else {
        return Err(Malformed);
    };
    if id.is_empty() || id.len() > protocol::MAX_ID {
        return Err(Malformed);
    }
    let tag = KNOWN.iter().copied().find(|k| *k == ty).unwrap_or("unknown");
    tracing::debug!(conn = conn.id, command = tag, "command");
    if !conn.remember(id) {
        svc.reply(conn, protocol::error_line(id, ErrorCode::DuplicateId));
        return Ok(());
    }
    dispatch(svc, conn, tag, id, &m);
    Ok(())
}

pub(crate) fn line_for(id: &str, r: Result<Value, ErrorCode>) -> String {
    match r {
        Ok(v) => protocol::ok_line(id, v),
        Err(c) => protocol::error_line(id, c),
    }
}

pub(crate) fn s<'a>(m: &'a Map<String, Value>, k: &str) -> Result<&'a str, ErrorCode> {
    m.get(k).and_then(Value::as_str).ok_or(ErrorCode::BadRequest)
}

pub(crate) fn n(m: &Map<String, Value>, k: &str) -> Result<u64, ErrorCode> {
    m.get(k).and_then(Value::as_u64).ok_or(ErrorCode::BadRequest)
}

pub(crate) fn b(m: &Map<String, Value>, k: &str) -> Result<bool, ErrorCode> {
    m.get(k).and_then(Value::as_bool).ok_or(ErrorCode::BadRequest)
}

fn pick_err(e: PickError) -> ErrorCode {
    protocol::pick_error_code(&e)
}

/// Inline commands run under the admission lock; a closed connection runs
/// nothing and gets no reply (plan D31).
fn dispatch(svc: &Arc<Service>, conn: &Arc<Conn>, tag: &'static str, id: &str, m: &Map<String, Value>) {
    let Some(r) = svc.admit(conn, || admitted(svc, conn, tag, m)) else { return };
    svc.reply(conn, line_for(id, r));
}

fn admitted(svc: &Arc<Service>, conn: &Conn, tag: &str, m: &Map<String, Value>) -> Result<Value, ErrorCode> {
    let ib = &svc.inbox;
    match tag {
        "pick" => ib
            .pick_owned(conn.owner, s(m, "card")?, s(m, "request")?, s(m, "option")?, n(m, "revision")?)
            .map(Value::String)
            .map_err(pick_err),
        "answer_text" => ib
            .answer_text_owned(conn.owner, s(m, "card")?, s(m, "request")?, n(m, "revision")?, s(m, "text")?.to_owned())
            .map(Value::String)
            .map_err(pick_err),
        "reply" => ib
            .reply_owned(conn.owner, s(m, "card")?, s(m, "text")?.to_owned())
            .map(Value::String)
            .map_err(pick_err),
        "panel" => {
            svc.set_panel(conn.id, b(m, "open")?);
            Ok(Value::Null)
        }
        _ => Err(ErrorCode::Unsupported),
    }
}
```

In `outbox.rs`, add the test accessor:

```rust
    /// Test hook: the next due line without waiting.
    #[cfg(any(test, feature = "testing"))]
    pub fn try_next_for_test(&self) -> Option<String> {
        self.take().flatten()
    }
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p acd-inbox --locked --test admission`
Expected: PASS. Then remove the admission lock from `admit` and `close` as a check: `a_close_during_a_pick_cancels_the_new_action` and `a_late_panel_open_cannot_stick_after_close` must fail. Restore the lock.

- [ ] **Step 7: Lint and commit**

Run: `cargo clippy --workspace --all-targets --locked -- -D warnings`
Expected: no warnings.

```bash
git add crates/acd-inbox
git commit -m "acd-inbox: connection admission serialized with close, bounded ids"
```

---

### Task 9: Server: sockets, readers, writers and broadcasts

**Execution:** cloud-ok

**Files:**
- Modify: `crates/acd-inbox/src/server.rs` (socket side)
- Modify: `crates/acd-inbox/tests/common/mod.rs` (socket rig and the `View` client)
- Create: `crates/acd-inbox/tests/server.rs`

**Interfaces:**
- Consumes: Task 6's `startup::peer_ok`; Task 8's `Service` core, `Registry`, `register`, `close`, `reply`; Task 4's `view_line` (returns `Option`) and `settings_line`.
- Produces (on `Service`):
  - `spawn(self: &Arc<Self>, listener: UnixListener) -> JoinHandle<()>`.
  - `route_outcomes(self: Arc<Self>, rx: broadcast::Receiver<ActionEvent>)` (async).
  - `publish_settings(self: &Arc<Self>)` (async) and `settings_changed(self: &Arc<Self>)`.
- Produces (in `acd_inbox::server`): `pub async fn read_line<R: AsyncBufRead + Unpin>(r: &mut R, buf: &mut Vec<u8>, max: usize) -> Result<bool, LineError>` and `pub enum LineError { TooLong, Io }`.
- Publication (D32):
  - Views have one owner, the broadcaster task. It encodes each new `InboxView` once and stores and queues it under the registry lock. A `None` from `view_line` keeps the previous line.
  - Settings carry a generation, taken before the snapshot is computed. A publication older than the stored one is dropped.
  - A new connection is seeded from the registry under the same lock, so it can never get an older line after a newer one.

- [ ] **Step 1: Extend the test rig**

Append to `crates/acd-inbox/tests/common/mod.rs`:

```rust
use std::path::{Path, PathBuf};

use acd_inbox::{
    prefs::Prefs,
    server::{NoKeyring, SecretControl, ServerOptions, Service},
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

pub type MakeSecrets = Box<dyn FnOnce(&FakeServer) -> (Arc<dyn SecretStore>, Arc<dyn SecretControl>) + Send>;

pub struct RigOptions {
    pub agents: Vec<Value>,
    pub timing: Timing,
    pub server: ServerOptions,
    pub control: Arc<dyn SecretControl>,
    /// Builds the inbox's store and the service's keyring control; default:
    /// a `MemorySecrets` holding m1's secret, and `control`.
    pub secrets: Option<MakeSecrets>,
    /// Wait for the fake daemon connection and send `agents`.
    pub connect: bool,
}

impl Default for RigOptions {
    fn default() -> Self {
        RigOptions {
            agents: vec![pending_agent("a1", "r1")],
            timing: fast(),
            server: ServerOptions::default(),
            control: Arc::new(NoKeyring),
            secrets: None,
            connect: true,
        }
    }
}

pub struct Rig {
    pub f: FakeServer,
    pub c: Option<FakeConn>,
    pub inbox: Arc<Inbox>,
    pub svc: Arc<Service>,
    pub socket: PathBuf,
    pub server: tokio::task::JoinHandle<()>,
    _dir: tempfile::TempDir,
}

pub async fn rig(agents: Vec<Value>, timing: Timing) -> Rig {
    rig_with(RigOptions { agents, timing, ..Default::default() }).await
}

pub async fn rig_with(o: RigOptions) -> Rig {
    type MakeStore = Box<dyn FnOnce(&FakeServer) -> Arc<dyn SecretStore> + Send>;
    let slot: Arc<std::sync::Mutex<Option<Arc<dyn SecretControl>>>> = Arc::new(std::sync::Mutex::new(None));
    let make: Option<MakeStore> = o.secrets.map(|make| {
        let out = slot.clone();
        Box::new(move |f: &FakeServer| {
            let (store, control) = make(f);
            *out.lock().unwrap() = Some(control);
            store
        }) as MakeStore
    });
    let (f, c, inbox) = inbox_rig(o.connect.then_some(o.agents), o.timing, make).await;
    let control = slot.lock().unwrap().take().unwrap_or(o.control);
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("s.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let svc = Service::new(inbox.clone(), Prefs::default(), None, control, o.server);
    let server = svc.spawn(listener);
    Rig { f, c, inbox, svc, socket, server, _dir: dir }
}

/// A view: a socket client that speaks the JSON-lines protocol.
pub struct View {
    r: BufReader<OwnedReadHalf>,
    w: OwnedWriteHalf,
    n: u64,
}

impl View {
    pub async fn connect(path: &Path) -> View {
        let (r, w) = UnixStream::connect(path).await.unwrap().into_split();
        View { r: BufReader::new(r), w, n: 0 }
    }

    /// The next message; `None` once the service closed the connection.
    pub async fn recv(&mut self) -> Option<Value> {
        let mut line = String::new();
        let n = tokio::time::timeout(Duration::from_secs(5), self.r.read_line(&mut line))
            .await
            .expect("no message within 5 s")
            .ok()?;
        if n == 0 {
            return None;
        }
        Some(serde_json::from_str(&line).expect("one JSON object per line"))
    }

    pub async fn greeting(&mut self) -> (Value, Value, Value) {
        let h = self.recv().await.unwrap();
        let s = self.recv().await.unwrap();
        let v = self.recv().await.unwrap();
        assert_eq!(
            (h["type"].as_str(), s["type"].as_str(), v["type"].as_str()),
            (Some("hello"), Some("settings"), Some("view"))
        );
        (h, s, v)
    }

    pub async fn recv_until(&mut self, pred: impl Fn(&Value) -> bool) -> Value {
        loop {
            let m = self.recv().await.expect("the service closed the connection");
            if pred(&m) {
                return m;
            }
        }
    }

    pub async fn send(&mut self, v: &Value) {
        let mut bytes = serde_json::to_vec(v).unwrap();
        bytes.push(b'\n');
        self.w.write_all(&bytes).await.unwrap();
    }

    pub async fn send_raw(&mut self, bytes: &[u8]) {
        let _ = self.w.write_all(bytes).await;
    }

    /// Sends `v` with a fresh id; returns the `ok` or `error` reply to it.
    pub async fn cmd(&mut self, mut v: Value) -> Value {
        self.n += 1;
        let id = format!("c{}", self.n);
        v["id"] = id.clone().into();
        self.send(&v).await;
        self.recv_until(|m| m["id"] == id.as_str() && (m["type"] == "ok" || m["type"] == "error")).await
    }

    /// Reads until the service closes the connection.
    pub async fn closed(&mut self) {
        while self.recv().await.is_some() {}
    }
}
```

- [ ] **Step 2: Write the failing tests**

`crates/acd-inbox/tests/server.rs`:

```rust
mod common;

use std::time::Duration;

use acd_client::{
    actions::ActionOwner,
    voice::{ActionEvent, ActionOutcome},
};
use acd_inbox::server::ServerOptions;
use common::*;
use serde_json::json;

#[tokio::test]
async fn a_view_gets_hello_settings_and_view_in_order() {
    let rig = rig(vec![pending_agent("a1", "r1")], fast()).await;
    let mut v = View::connect(&rig.socket).await;
    let (hello, settings, view) = v.greeting().await;
    assert_eq!(hello, json!({"type":"hello","protocol":1,"service":env!("CARGO_PKG_VERSION")}));
    assert_eq!(settings["machines"][0]["key"], "m1");
    assert_eq!(settings["machines"][0]["secret"], "ok");
    assert_eq!(settings["notifications"], true);
    assert_eq!(view["view"]["cards"][0]["key"], "m1/a1");
    assert_eq!(view["omitted"], json!({"done":0,"working":0}));
}

#[tokio::test]
async fn outcomes_reach_every_view_with_their_card() {
    let rig = rig(vec![pending_agent("a1", "r1")], held()).await;
    let mut a = View::connect(&rig.socket).await;
    let mut b = View::connect(&rig.socket).await;
    a.greeting().await;
    b.greeting().await;
    let id = rig.inbox.pick("m1/a1", "r1", "allow", 1).unwrap();
    assert!(rig.inbox.cancel(&id));
    for v in [&mut a, &mut b] {
        let o = v.recv_until(|m| m["type"] == "outcome").await;
        assert_eq!(o, json!({"type":"outcome","action":id,"card":"m1/a1","result":{"kind":"cancelled"}}));
    }
}

#[tokio::test]
async fn a_disconnect_cancels_that_views_countdowns_only() {
    let rig = rig(vec![pending_agent("a1", "r1"), pending_agent("a2", "r2")], held()).await;
    let mut a = View::connect(&rig.socket).await;
    a.greeting().await; // connection 1
    let mut b = View::connect(&rig.socket).await;
    b.greeting().await; // connection 2
    let mine = rig.inbox.pick_owned(ActionOwner(1), "m1/a1", "r1", "allow", 1).unwrap();
    let theirs = rig.inbox.pick_owned(ActionOwner(2), "m1/a2", "r2", "allow", 1).unwrap();
    drop(a);
    let o = b.recv_until(|m| m["type"] == "outcome").await;
    assert_eq!(o["action"], mine.as_str());
    assert_eq!(o["result"]["kind"], "cancelled");
    view_until(&rig.inbox, |v| v.actions.len() == 1 && v.actions[0].id == theirs).await;
    rig.inbox.expire_countdowns_for_test();
    let fired = b.recv_until(|m| m["type"] == "outcome" && m["action"] == theirs.as_str()).await;
    assert_eq!(fired["result"]["kind"], "fired");
    let frame = frame_of_type(&rig.f, "request.answer").await;
    assert_eq!(frame["request_id"], "r2");
    assert_eq!(answers(&rig.f), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_view_that_stops_reading_is_disconnected_and_its_countdowns_cancelled() {
    let mut big = pending_agent("a1", "r1");
    big["pending"]["raw_screen"] = "x".repeat(2 * 1024 * 1024).into();
    let rig = rig_with(RigOptions {
        agents: vec![big],
        timing: held(),
        server: ServerOptions { write_timeout: Duration::from_millis(300), ..Default::default() },
        ..Default::default()
    })
    .await;
    let mut watcher = View::connect(&rig.socket).await;
    watcher.greeting().await; // connection 1 reads everything
    let stalled = tokio::net::UnixStream::connect(&rig.socket).await.unwrap(); // connection 2 never reads
    until("the second connection", || rig.svc.connections() == 2).await;
    let id = rig.inbox.pick_owned(ActionOwner(2), "m1/a1", "r1", "allow", 1).unwrap();
    let o = watcher.recv_until(|m| m["type"] == "outcome" && m["action"] == id.as_str()).await;
    assert_eq!(o["result"]["kind"], "cancelled");
    assert_eq!(rig.svc.connections(), 1);
    drop(stalled);
}

#[tokio::test]
async fn an_oversized_line_closes_the_connection() {
    let rig = rig_with(RigOptions {
        server: ServerOptions { max_line: 1024, ..Default::default() },
        ..Default::default()
    })
    .await;
    let mut v = View::connect(&rig.socket).await;
    v.greeting().await;
    v.send_raw(&[b'a'; 2000]).await;
    v.closed().await;
}

#[tokio::test]
async fn malformed_lines_and_bad_ids_close_the_connection() {
    let rig = rig(vec![pending_agent("a1", "r1")], fast()).await;
    for bad in [
        b"not json\n".to_vec(),
        b"[1,2]\n".to_vec(),
        b"{\"type\":\"escape\"}\n".to_vec(),
        format!("{{\"type\":\"escape\",\"id\":\"{}\"}}\n", "x".repeat(65)).into_bytes(),
        b"{\"type\":7,\"id\":\"a\"}\n".to_vec(),
    ] {
        let mut v = View::connect(&rig.socket).await;
        v.greeting().await;
        v.send_raw(&bad).await;
        v.closed().await;
    }
}

#[tokio::test]
async fn an_unknown_type_is_unsupported_and_a_repeated_id_is_refused() {
    let rig = rig(vec![pending_agent("a1", "r1")], fast()).await;
    let mut v = View::connect(&rig.socket).await;
    v.greeting().await;
    v.send(&json!({"type":"from_the_future","id":"x1"})).await;
    let r = v.recv_until(|m| m["id"] == "x1").await;
    assert_eq!(r, json!({"type":"error","id":"x1","code":"unsupported"}));
    v.send(&json!({"type":"from_the_future","id":"x1"})).await;
    let r = v.recv_until(|m| m["id"] == "x1").await;
    assert_eq!(r["code"], "duplicate_id");
}

#[tokio::test]
async fn a_view_that_cannot_fit_is_reported_and_recovers() {
    let agents: Vec<serde_json::Value> = (1..=5)
        .map(|i| {
            let mut a = pending_agent(&format!("a{i}"), &format!("r{i}"));
            a["title"] = "t".repeat(400).into();
            a
        })
        .collect();
    let rig = rig_with(RigOptions {
        agents,
        server: ServerOptions { max_line: 3000, ..Default::default() },
        ..Default::default()
    })
    .await;
    let mut v = View::connect(&rig.socket).await;
    assert_eq!(v.recv().await.unwrap()["type"], "hello");
    assert_eq!(v.recv().await.unwrap()["type"], "settings");
    assert_eq!(v.recv().await.unwrap(), json!({"type":"view_overflow","needs_you":5}));
    let c = rig.c.as_ref().unwrap();
    for (seq, id) in [(2, "a2"), (3, "a3"), (4, "a4"), (5, "a5")] {
        c.send(json!({"type":"agent.removed","epoch":"e1","seq":seq,"agent_id":id})).await;
    }
    let view = v.recv_until(|m| m["type"] == "view").await;
    assert_eq!(view["view"]["cards"].as_array().unwrap().len(), 1, "the inbox shows again once it fits");
}

#[tokio::test]
async fn a_lagged_outcome_stream_tells_every_view() {
    let rig = rig(vec![pending_agent("a1", "r1")], fast()).await;
    let mut a = View::connect(&rig.socket).await;
    let mut b = View::connect(&rig.socket).await;
    a.greeting().await;
    b.greeting().await;
    let (tx, rx) = tokio::sync::broadcast::channel(1);
    for i in 0..3 {
        tx.send(ActionEvent { action_id: format!("x{i}"), card_key: "m1/a1".into(), outcome: ActionOutcome::Cancelled })
            .unwrap();
    }
    tokio::spawn(rig.svc.clone().route_outcomes(rx));
    for v in [&mut a, &mut b] {
        v.recv_until(|m| m["type"] == "outcomes_lost").await;
        let next = v.recv_until(|m| m["type"] == "outcome").await;
        assert_eq!(next["action"], "x2");
    }
}
```

Unit tests at the end of `server.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use acd_client::{inbox::InboxOptions, settings::{MemorySecrets, Settings}};



    #[tokio::test]
    async fn lines_are_split_capped_and_a_final_partial_line_is_dropped() {
        let mut r = BufReader::new(&b"one\ntwo\nthree"[..]);
        let mut buf = Vec::new();
        assert_eq!(read_line(&mut r, &mut buf, 16).await, Ok(true));
        assert_eq!(buf, b"one");
        assert_eq!(read_line(&mut r, &mut buf, 16).await, Ok(true));
        assert_eq!(buf, b"two");
        assert_eq!(read_line(&mut r, &mut buf, 16).await, Ok(false));
        let mut r = BufReader::new(&b"12345\n"[..]);
        assert_eq!(read_line(&mut r, &mut buf, 5).await, Ok(true), "exactly max fits");
        let mut r = BufReader::new(&b"123456\n"[..]);
        assert_eq!(read_line(&mut r, &mut buf, 5).await, Err(LineError::TooLong));
        let mut r = BufReader::with_capacity(2, &b"123456"[..]);
        assert_eq!(read_line(&mut r, &mut buf, 5).await, Err(LineError::TooLong), "without a newline too");
    }

    #[test]
    fn an_older_settings_publication_never_overwrites_a_newer_one() {
        let inbox = Inbox::new(
            Settings { machines: vec![], ..Settings::fresh() },
            Arc::new(MemorySecrets::default()),
            InboxOptions { timing: Default::default(), settings_path: None, client_name: "t".into(), client_version: "0".into() },
        );
        let svc = Service::new(inbox, Prefs::default(), None, Arc::new(NoKeyring), ServerOptions::default());
        let conn = svc.register();
        svc.store_settings(2, "new".into());
        svc.store_settings(1, "old".into());
        assert_eq!(svc.registry.lock().unwrap().settings, (2, "new".to_owned()));
        let lines: Vec<String> = std::iter::from_fn(|| conn.outbox.try_next_for_test()).collect();
        assert!(lines.contains(&"new".to_owned()) && !lines.contains(&"old".to_owned()));
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p acd-inbox --locked --test server`
Expected: compile error, `no method named spawn`.

- [ ] **Step 4: Implement the socket side in `server.rs`**

Add to the imports:

```rust
use acd_client::voice::ActionEvent;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast;

use crate::{commands, startup};
```

Add to `impl Service`:

```rust
    /// Serves `listener`. The view and outcome broadcasters stop when the
    /// returned task is aborted.
    pub fn spawn(self: &Arc<Self>, listener: UnixListener) -> tokio::task::JoinHandle<()> {
        let me = self.clone();
        let outcomes = self.inbox.action_outcomes();
        tokio::spawn(async move {
            let mut tasks = tokio::task::JoinSet::new();
            tasks.spawn(me.clone().broadcast_views());
            tasks.spawn(me.clone().route_outcomes(outcomes));
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => me.accept(stream),
                    Err(e) => {
                        tracing::warn!(kind = ?e.kind(), "accept failed");
                        tokio::task::yield_now().await;
                    }
                }
            }
        })
    }

    /// Recomputes `settings` off the async workers and publishes it unless
    /// a newer publication got there first (plan D32).
    pub async fn publish_settings(self: &Arc<Self>) {
        let generation = self.settings_gen.fetch_add(1, Ordering::SeqCst) + 1;
        let me = self.clone();
        let Ok(line) = tokio::task::spawn_blocking(move || {
            let p = *me.prefs_tx.borrow();
            protocol::settings_line(&prefs::snapshot(&me.inbox, p), me.opts.max_line)
        })
        .await
        else {
            return;
        };
        self.store_settings(generation, line);
    }

    pub(crate) fn store_settings(&self, generation: u64, line: String) {
        let mut reg = self.registry.lock().unwrap();
        if generation <= reg.settings.0 {
            return;
        }
        reg.settings = (generation, line.clone());
        for c in reg.conns.values() {
            c.outbox.set_settings(line.clone());
        }
    }

    /// `publish_settings` in the background.
    pub(crate) fn settings_changed(self: &Arc<Self>) {
        let me = self.clone();
        tokio::spawn(async move { me.publish_settings().await });
    }

    /// The only publisher of views (plan D32).
    async fn broadcast_views(self: Arc<Self>) {
        let mut rx = self.inbox.view();
        loop {
            let view = rx.borrow_and_update().clone();
            // A view that cannot fit becomes `view_overflow`, which replaces
            // the previous view: the panel never shows a stale inbox (D9).
            let line = protocol::view_or_overflow_line(&view, self.opts.max_line);
            let mut reg = self.registry.lock().unwrap();
            reg.view = line.clone();
            for c in reg.conns.values() {
                c.outbox.set_view(line.clone());
            }
            drop(reg);
            if rx.changed().await.is_err() {
                return;
            }
        }
    }

    /// Sends each outcome to every view; a lag sends `outcomes_lost` to
    /// every view (§5.1). `spawn` runs it on the inbox's stream.
    pub async fn route_outcomes(self: Arc<Self>, mut rx: broadcast::Receiver<ActionEvent>) {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let line = protocol::outcome_line(&event);
                    for c in self.registry.lock().unwrap().conns.values() {
                        c.outbox.outcome(line.clone());
                    }
                }
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    tracing::warn!(missed, "outcome stream lagged");
                    for c in self.registry.lock().unwrap().conns.values() {
                        c.outbox.outcomes_lost();
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    }

    fn accept(self: &Arc<Self>, stream: UnixStream) {
        if !startup::peer_ok(&stream) {
            tracing::warn!("connection refused: the peer is another user");
            return;
        }
        let conn = self.register();
        tracing::info!(conn = conn.id, "connection accepted");
        let (r, w) = stream.into_split();
        tokio::spawn(self.clone().write_loop(conn.clone(), w));
        tokio::spawn(self.clone().read_loop(conn, r));
    }

    async fn write_loop(self: Arc<Self>, conn: Arc<Conn>, mut w: OwnedWriteHalf) {
        while let Some(line) = conn.outbox.next().await {
            let mut bytes = line.into_bytes();
            bytes.push(b'\n');
            match tokio::time::timeout(self.opts.write_timeout, w.write_all(&bytes)).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => return self.close(&conn, CloseReason::WriteError),
                Err(_) => return self.close(&conn, CloseReason::WriteTimeout),
            }
        }
        let _ = w.shutdown().await;
    }

    async fn read_loop(self: Arc<Self>, conn: Arc<Conn>, r: OwnedReadHalf) {
        let mut reader = BufReader::new(r);
        let mut line = Vec::new();
        let mut shut = conn.shut.subscribe();
        loop {
            let read = tokio::select! {
                r = read_line(&mut reader, &mut line, self.opts.max_line) => r,
                _ = shut.wait_for(|closed| *closed) => return,
            };
            let why = match read {
                Ok(true) => match commands::handle(&self, &conn, &line) {
                    Ok(()) => continue,
                    Err(commands::Malformed) => CloseReason::Malformed,
                },
                Ok(false) => CloseReason::Eof,
                Err(LineError::TooLong) => CloseReason::Oversize,
                Err(LineError::Io) => CloseReason::ReadError,
            };
            if why != CloseReason::Eof {
                // Tag and length only: raw lines are never logged (§4.1).
                tracing::warn!(conn = conn.id, reason = why.tag(), len = line.len(), "line rejected");
            }
            return self.close(&conn, why);
        }
    }
```

Add after `impl Service`:

```rust
#[derive(Debug, PartialEq, Eq)]
pub enum LineError {
    TooLong,
    Io,
}

/// Reads one line, newline excluded, into `buf`. `Ok(false)` at end of
/// input; a final line without a newline is dropped. A line longer than
/// `max` is `TooLong`, and nothing past `max` is buffered.
pub async fn read_line<R: AsyncBufRead + Unpin>(r: &mut R, buf: &mut Vec<u8>, max: usize) -> Result<bool, LineError> {
    buf.clear();
    loop {
        let available = r.fill_buf().await.map_err(|_| LineError::Io)?;
        if available.is_empty() {
            return Ok(false);
        }
        if let Some(i) = available.iter().position(|&b| b == b'\n') {
            if buf.len() + i > max {
                return Err(LineError::TooLong);
            }
            buf.extend_from_slice(&available[..i]);
            r.consume(i + 1);
            return Ok(true);
        }
        let n = available.len();
        if buf.len() + n > max {
            return Err(LineError::TooLong);
        }
        buf.extend_from_slice(available);
        r.consume(n);
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p acd-inbox --locked --test server --test admission`
Run: `cargo test -p acd-inbox --locked --lib server`
Expected: PASS.

- [ ] **Step 6: Lint and commit**

Run: `cargo clippy --workspace --all-targets --locked -- -D warnings`
Expected: no warnings.

```bash
git add crates/acd-inbox
git commit -m "acd-inbox: socket server with ordered publication, limits and disconnect cancellation"
```

---

### Task 10: Commands

**Execution:** cloud-ok

**Files:**
- Modify: `crates/acd-inbox/src/commands.rs` (replace `dispatch` and `admitted`)
- Modify: `crates/acd-inbox/tests/admission.rs` (expected codes)
- Create: `crates/acd-inbox/tests/commands.rs`

**Interfaces:**
- Consumes:
  - Task 1: `Inbox::{pick_owned, answer_text_owned, reply_owned, cancel, escape_owned}`.
  - `Inbox::{mark_seen, set_show_idle, set_show_subagents, set_show_hook_only, rename_machine, settings, machines}`.
  - Tasks 2 and 3: `pair_machine`, `remove_machine_checked`, `reconnect_machine`, `MachineError`, `SecretErrorKind`, `SecretStatus`.
  - Task 8: `Service::{admit, reply, set_panel, set_notifications, secrets}` and `commands::{line_for, s, n, b}`; Task 9: `Service::{settings_changed, publish_settings}`.
- Produces (in `acd_inbox::commands`):
  - `pub fn machine_error_code(e: &MachineError) -> ErrorCode`
  - `pub fn secret_code(e: &SecretError) -> ErrorCode`
  - The command table of §5.2, with these replies (D7, D8, D11):

| Command | Fields | Reply `result` | Runs |
|---|---|---|---|
| `pick` | `card`, `request`, `option`, `revision` | action id | inline |
| `answer_text` | `card`, `request`, `revision`, `text` | action id | inline |
| `reply` | `card`, `text` | action id | inline |
| `cancel` | `action` | `true`/`false` | inline |
| `escape` | none | `true`/`false` | inline |
| `mark_seen` | `card` | `null` | inline |
| `set_show` | `what` (`idle`, `subagents`, `hook_only`), `on` | `null` | blocking pool (waits for the edit lock) |
| `rename_machine` | `machine`, `name` | `null` | blocking pool (waits for the edit lock) |
| `set_notifications` | `on` | `null` | inline |
| `panel` | `open` | `null` | inline |
| `pair` | `pairing` | `{"machine","name"}` | blocking pool |
| `remove_machine` | `machine` | `null` | blocking pool |
| `retry_machine` | `machine` | `null` | blocking pool (refreshes the machine's cached secret first) |
| `unlock_secrets` | none | `null` | blocking pool |

Every command that changes settings, machines or the notification switch publishes a new `settings` snapshot afterwards.

- [ ] **Step 1: Write the failing tests**

`crates/acd-inbox/tests/commands.rs`:

```rust
mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::time::Duration;

use acd_client::settings::SecretError;
use acd_inbox::server::SecretControl;
use common::*;
use serde_json::{Value, json};

fn code(r: Value) -> String {
    assert_eq!(r["type"], "error", "{r}");
    r["code"].as_str().unwrap().to_owned()
}

fn action_id(r: Value) -> String {
    assert_eq!(r["type"], "ok", "{r}");
    r["result"].as_str().unwrap().to_owned()
}

async fn greeted(rig: &Rig) -> View {
    let mut v = View::connect(&rig.socket).await;
    v.greeting().await;
    v
}

fn pick(card: &str, request: &str, option: &str) -> Value {
    json!({"type":"pick","card":card,"request":request,"option":option,"revision":1})
}

#[tokio::test]
async fn pick_returns_the_action_id_and_the_countdown_fires() {
    let rig = rig(vec![pending_agent("a1", "r1")], fast()).await;
    let mut v = greeted(&rig).await;
    let id = action_id(v.cmd(pick("m1/a1", "r1", "allow")).await);
    let fired = v.recv_until(|m| m["type"] == "outcome" && m["action"] == id.as_str()).await;
    assert_eq!(fired["card"], "m1/a1");
    assert_eq!(fired["result"], json!({"kind":"fired"}));
    frame_of_type(&rig.f, "request.answer").await;
}

#[tokio::test]
async fn stale_unknown_and_bad_input_get_their_codes() {
    let rig = rig(vec![pending_agent("a1", "r1")], held()).await;
    let mut v = greeted(&rig).await;
    let stale = json!({"type":"pick","card":"m1/a1","request":"r1","option":"allow","revision":2});
    assert_eq!(code(v.cmd(stale).await), "stale");
    assert_eq!(code(v.cmd(pick("m1/zz", "r1", "allow")).await), "unknown_card");
    assert_eq!(code(v.cmd(pick("m1/a1", "r1", "nope")).await), "unknown_option");
    assert_eq!(code(v.cmd(json!({"type":"pick","card":"m1/a1"})).await), "bad_request");
    let blank = json!({"type":"answer_text","card":"m1/a1","request":"r1","revision":1,"text":"   "});
    assert_eq!(code(v.cmd(blank).await), "bad_text");
    assert_eq!(code(v.cmd(json!({"type":"reply","card":"m1/a1","text":""})).await), "bad_text");
    assert_eq!(code(v.cmd(json!({"type":"mark_seen","card":"m1/a1"})).await), "cannot_mark_seen");
    assert_eq!(code(v.cmd(json!({"type":"rename_machine","machine":"nope","name":"x"})).await), "unknown_machine");
    assert_eq!(code(v.cmd(json!({"type":"rename_machine","machine":"m1","name":"  "})).await), "bad_name");
    assert_eq!(code(v.cmd(json!({"type":"set_show","what":"everything","on":true})).await), "bad_request");
    assert_eq!(code(v.cmd(json!({"type":"retry_machine","machine":"nope"})).await), "unknown_machine");
    assert_eq!(v.cmd(json!({"type":"escape"})).await["result"], false, "nothing was counting");
}

#[tokio::test]
async fn a_repeated_id_does_nothing() {
    let rig = rig(vec![pending_agent("a1", "r1")], held()).await;
    let mut v = greeted(&rig).await;
    v.send(&json!({"type":"pick","id":"same","card":"m1/a1","request":"r1","option":"allow","revision":1})).await;
    let first = v.recv_until(|m| m["id"] == "same").await;
    let id = action_id(first);
    v.send(&json!({"type":"cancel","id":"same","action":id})).await;
    let second = v.recv_until(|m| m["id"] == "same").await;
    assert_eq!(second["code"], "duplicate_id");
    view_until(&rig.inbox, |view| view.actions.len() == 1 && view.actions[0].id == id).await;
}

#[tokio::test]
async fn esc_is_per_view_and_the_cancel_control_cancels_any_views_action() {
    let rig = rig(vec![pending_agent("a1", "r1"), pending_agent("a2", "r2")], held()).await;
    let mut a = greeted(&rig).await;
    let mut b = greeted(&rig).await;
    let ida = action_id(a.cmd(pick("m1/a1", "r1", "allow")).await);
    let idb = action_id(b.cmd(pick("m1/a2", "r2", "allow")).await);
    assert_eq!(a.cmd(json!({"type":"escape"})).await["result"], true);
    let o = b.recv_until(|m| m["type"] == "outcome" && m["action"] == ida.as_str()).await;
    assert_eq!(o["result"]["kind"], "cancelled");
    assert_eq!(a.cmd(json!({"type":"escape"})).await["result"], false, "b's countdown is not a's");
    assert_eq!(a.cmd(json!({"type":"cancel","action":idb})).await["result"], true);
    let o = b.recv_until(|m| m["type"] == "outcome" && m["action"] == idb.as_str()).await;
    assert_eq!(o["result"]["kind"], "cancelled");
}

#[tokio::test]
async fn a_second_view_cannot_pick_a_card_that_is_counting() {
    let rig = rig(vec![pending_agent("a1", "r1")], held()).await;
    let mut a = greeted(&rig).await;
    let mut b = greeted(&rig).await;
    let ida = action_id(a.cmd(pick("m1/a1", "r1", "allow")).await);
    assert_eq!(code(b.cmd(pick("m1/a1", "r1", "deny")).await), "already_counting");
    view_until(&rig.inbox, |view| view.actions.len() == 1 && view.actions[0].id == ida).await;
}

#[tokio::test]
async fn the_panel_flag_follows_the_view_and_clears_on_disconnect() {
    let rig = rig(vec![pending_agent("a1", "r1")], fast()).await;
    let mut open = rig.svc.panel_open();
    let mut a = greeted(&rig).await;
    assert_eq!(a.cmd(json!({"type":"panel","open":true})).await["type"], "ok");
    tokio::time::timeout(Duration::from_secs(2), open.wait_for(|o| *o)).await.unwrap().unwrap();
    drop(a);
    tokio::time::timeout(Duration::from_secs(2), open.wait_for(|o| !*o)).await.unwrap().unwrap();
}

#[tokio::test]
async fn settings_follow_show_toggles_and_the_notification_switch() {
    let rig = rig(vec![pending_agent("a1", "r1")], fast()).await;
    let mut v = greeted(&rig).await;
    assert_eq!(v.cmd(json!({"type":"set_show","what":"subagents","on":true})).await["type"], "ok");
    v.recv_until(|m| m["type"] == "settings" && m["show_subagents"] == true).await;
    assert_eq!(v.cmd(json!({"type":"set_notifications","on":false})).await["type"], "ok");
    v.recv_until(|m| m["type"] == "settings" && m["notifications"] == false).await;
    assert!(!rig.svc.prefs().borrow().notifications);
}

#[tokio::test]
async fn a_bad_pairing_string_is_never_echoed() {
    let rig = rig(vec![pending_agent("a1", "r1")], fast()).await;
    let mut v = greeted(&rig).await;
    let secretish = format!("acd://host:0?pin=sha256:XYZ#{TOKEN}");
    let r = v.cmd(json!({"type":"pair","pairing":secretish})).await;
    assert_eq!(r["code"], "bad_pairing");
    assert!(!r.to_string().contains(TOKEN), "the reply repeats the pairing string");
}

/// An unlock that waits until released, as a keyring prompt does.
#[derive(Default)]
struct SlowUnlock {
    entered: AtomicBool,
    release: AtomicBool,
}

impl SecretControl for SlowUnlock {
    fn unlock(&self) -> Result<(), SecretError> {
        self.entered.store(true, SeqCst);
        while !self.release.load(SeqCst) {
            std::thread::yield_now();
        }
        Ok(())
    }
    fn refresh(&self, _: &[String]) {}
}

/// Releases the unlock on drop, panics included.
struct Unblock(Arc<SlowUnlock>);

impl Drop for Unblock {
    fn drop(&mut self) {
        self.0.release.store(true, SeqCst);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_disconnect_during_a_slow_command_still_cancels_countdowns() {
    let control = Arc::new(SlowUnlock::default());
    let unblock = Unblock(control.clone());
    let rig = rig_with(RigOptions { timing: held(), control: control.clone(), ..Default::default() }).await;
    let mut a = greeted(&rig).await;
    let mut watcher = greeted(&rig).await;
    let id = action_id(a.cmd(pick("m1/a1", "r1", "allow")).await);
    a.send(&json!({"type":"unlock_secrets","id":"u1"})).await;
    until("the unlock to start", || control.entered.load(SeqCst)).await;
    drop(a);
    let o = watcher.recv_until(|m| m["type"] == "outcome" && m["action"] == id.as_str()).await;
    assert_eq!(o["result"]["kind"], "cancelled");
    drop(unblock);
    rig.inbox.expire_countdowns_for_test();
    view_until(&rig.inbox, |v| v.actions.is_empty()).await;
    assert_eq!(answers(&rig.f), 0);
}

#[tokio::test]
async fn rename_and_remove_publish_settings() {
    let rig = rig(vec![pending_agent("a1", "r1")], fast()).await;
    let mut v = greeted(&rig).await;
    assert_eq!(v.cmd(json!({"type":"rename_machine","machine":"m1","name":"Dev box"})).await["type"], "ok");
    v.recv_until(|m| m["type"] == "settings" && m["machines"][0]["name"] == "Dev box").await;
    assert_eq!(v.cmd(json!({"type":"remove_machine","machine":"m1"})).await["type"], "ok");
    v.recv_until(|m| m["type"] == "settings" && m["machines"].as_array().unwrap().is_empty()).await;
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p acd-inbox --locked --test commands`
Expected: FAIL. Only `pick`, `answer_text`, `reply` and `panel` are wired (Task 8), so `stale_unknown_and_bad_input_get_their_codes` gets `unsupported` for `mark_seen`, and the settings and slow-command tests fail.

- [ ] **Step 3: Implement the table**

In `commands.rs`, extend the imports:

```rust
use acd_client::{
    inbox::MachineError,
    settings::{EndpointConfig, SecretError, SecretErrorKind, SecretStatus},
};
use serde_json::json;
```

Replace `dispatch` and `admitted` with:

```rust
/// Inline commands run under the admission lock (plan D31); slow ones, which
/// may wait on the keyring or the edit lock, on the blocking pool (D11).
fn dispatch(svc: &Arc<Service>, conn: &Arc<Conn>, tag: &'static str, id: &str, m: &Map<String, Value>) {
    match tag {
        "pair" | "remove_machine" | "retry_machine" | "unlock_secrets" | "rename_machine" | "set_show" => {
            slow(svc, conn, tag, id, m.clone())
        }
        "unknown" => svc.reply(conn, protocol::error_line(id, ErrorCode::Unsupported)),
        _ => {
            let Some(r) = svc.admit(conn, || admitted(svc, conn, tag, m)) else { return };
            svc.reply(conn, line_for(id, r));
        }
    }
}

fn admitted(svc: &Arc<Service>, conn: &Conn, tag: &str, m: &Map<String, Value>) -> Result<Value, ErrorCode> {
    let ib = &svc.inbox;
    match tag {
        "pick" => ib
            .pick_owned(conn.owner, s(m, "card")?, s(m, "request")?, s(m, "option")?, n(m, "revision")?)
            .map(Value::String)
            .map_err(pick_err),
        "answer_text" => ib
            .answer_text_owned(conn.owner, s(m, "card")?, s(m, "request")?, n(m, "revision")?, s(m, "text")?.to_owned())
            .map(Value::String)
            .map_err(pick_err),
        "reply" => ib
            .reply_owned(conn.owner, s(m, "card")?, s(m, "text")?.to_owned())
            .map(Value::String)
            .map_err(pick_err),
        "cancel" => Ok(Value::Bool(ib.cancel(s(m, "action")?))),
        "escape" => Ok(Value::Bool(ib.escape_owned(conn.owner))),
        "mark_seen" => ib.mark_seen(s(m, "card")?).map(|()| Value::Null).map_err(|e| match e {
            PickError::CannotReply => ErrorCode::CannotMarkSeen,
            e => pick_err(e),
        }),
        "set_notifications" => {
            svc.set_notifications(b(m, "on")?).map_err(|_| ErrorCode::SettingsFailed)?;
            svc.settings_changed();
            Ok(Value::Null)
        }
        "panel" => {
            svc.set_panel(conn.id, b(m, "open")?);
            Ok(Value::Null)
        }
        _ => Err(ErrorCode::Unsupported),
    }
}

/// Runs off the reader, so a disconnect is seen at once (plan D11). The
/// reply is dropped if the view left meanwhile.
fn slow(svc: &Arc<Service>, conn: &Arc<Conn>, tag: &'static str, id: &str, m: Map<String, Value>) {
    let (svc, conn, id) = (svc.clone(), conn.clone(), id.to_owned());
    tokio::spawn(async move {
        let worker = svc.clone();
        let r = tokio::task::spawn_blocking(move || run_slow(&worker, tag, &m))
            .await
            .unwrap_or(Err(ErrorCode::SecretStoreFailed));
        svc.reply(&conn, line_for(&id, r));
        svc.publish_settings().await;
    });
}

fn run_slow(svc: &Arc<Service>, tag: &str, m: &Map<String, Value>) -> Result<Value, ErrorCode> {
    let ib = &svc.inbox;
    match tag {
        "pair" => {
            let entry = ib.pair_machine(s(m, "pairing")?).map_err(|e| machine_error_code(&e))?;
            Ok(json!({"machine": entry.key, "name": entry.name}))
        }
        "remove_machine" => ib
            .remove_machine_checked(s(m, "machine")?)
            .map(|()| Value::Null)
            .map_err(|e| machine_error_code(&e)),
        "retry_machine" => {
            let key = s(m, "machine")?;
            // A cached keyring error is retried only when asked (plan D14).
            svc.secrets.refresh(&[key.to_owned()]);
            ib.reconnect_machine(key).map(|()| Value::Null).map_err(|e| machine_error_code(&e))
        }
        "unlock_secrets" => unlock(svc).map(|()| Value::Null),
        "rename_machine" => {
            let (key, name) = (s(m, "machine")?, s(m, "name")?);
            let mut entry = ib
                .settings()
                .machines
                .into_iter()
                .find(|e| e.key == key)
                .ok_or(ErrorCode::UnknownMachine)?;
            entry.name = name.to_owned();
            entry.validate().map_err(|_| ErrorCode::BadName)?;
            ib.rename_machine(key, name).map(|()| Value::Null).map_err(|_| ErrorCode::SettingsFailed)
        }
        "set_show" => {
            let on = b(m, "on")?;
            match s(m, "what")? {
                "idle" => ib.set_show_idle(on),
                "subagents" => ib.set_show_subagents(on).map_err(|_| ErrorCode::SettingsFailed)?,
                "hook_only" => ib.set_show_hook_only(on).map_err(|_| ErrorCode::SettingsFailed)?,
                _ => return Err(ErrorCode::BadRequest),
            }
            Ok(Value::Null)
        }
        _ => Err(ErrorCode::Unsupported),
    }
}

/// §9: unlock, re-read the affected secrets, reconnect those machines. A
/// dismissed or timed-out prompt changes nothing.
fn unlock(svc: &Arc<Service>) -> Result<(), ErrorCode> {
    let affected: Vec<String> = svc
        .inbox
        .machines()
        .into_iter()
        .filter(|m| matches!(m.endpoint, EndpointConfig::Tcp { .. }) && m.secret != SecretStatus::Ok)
        .map(|m| m.key)
        .collect();
    svc.secrets.unlock().map_err(|e| secret_code(&e))?;
    svc.secrets.refresh(&affected);
    for key in &affected {
        if let Err(e) = svc.inbox.reconnect_machine(key) {
            tracing::info!(code = machine_error_code(&e).as_str(), "machine not reconnected after unlock");
        }
    }
    Ok(())
}

pub fn secret_code(e: &SecretError) -> ErrorCode {
    match e.kind() {
        SecretErrorKind::Locked => ErrorCode::SecretStoreLocked,
        // An unconfirmed change is reported as a timeout; the machine shows
        // `secret: unknown` (D36).
        SecretErrorKind::Timeout | SecretErrorKind::Unknown => ErrorCode::SecretStoreTimeout,
        SecretErrorKind::Other => ErrorCode::SecretStoreFailed,
    }
}

pub fn machine_error_code(e: &MachineError) -> ErrorCode {
    match e {
        MachineError::UnknownMachine => ErrorCode::UnknownMachine,
        MachineError::BadPairing => ErrorCode::BadPairing,
        MachineError::NotPaired => ErrorCode::NotPaired,
        MachineError::SecretRead(s) | MachineError::SecretWrite(s) | MachineError::SecretDelete(s) => secret_code(s),
        MachineError::Settings(_) => ErrorCode::SettingsFailed,
        MachineError::PairRecoveryFailed | MachineError::RemoveRecoveryFailed => ErrorCode::SecretStoreFailed,
    }
}
```

`set_show` with `what: "idle"` saves nothing, but goes the same way to keep one path. Every slow command publishes settings when done; `set_notifications` publishes inline.

In `tests/admission.rs`, `ids_are_remembered_up_to_a_bound` now gets `ok` for each admitted `escape`: change its expected codes to `["", "", "duplicate_id", "", ""]` (an `ok` line has no `code`).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p acd-inbox --locked`
Expected: PASS, including Task 9's `an_unknown_type_is_unsupported_and_a_repeated_id_is_refused` and Task 8's admission tests.

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy --workspace --all-targets --locked -- -D warnings`
Expected: no warnings.

```bash
git add crates/acd-inbox
git commit -m "acd-inbox: command table with owned actions, error codes and slow commands off the reader"
```

---
### Task 11: Secret worker: deadlines and ambiguous writes

**Execution:** cloud-ok

**Files:**
- Create: `crates/acd-inbox/src/secrets.rs` (worker and fake; Task 12 adds the store), `crates/acd-inbox/tests/worker.rs`
- Modify: `crates/acd-inbox/src/lib.rs` (`pub mod secrets;`), `crates/acd-inbox/Cargo.toml` (`async-trait.workspace = true`), `Cargo.lock`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces (in `acd_inbox::secrets`):
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum BackendError { Locked, Dismissed, Timeout, Unavailable, Failed }`, with `tag()`.
  - `#[async_trait] pub trait SecretBackend: Send + 'static`, with `read`, `write`, `delete` and `unlock`. Only `unlock` may prompt.
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum WorkerError { Backend(BackendError), Expired, Ambiguous }`:
    - `Expired`: the deadline passed before the operation started, so it never ran.
    - `Ambiguous`: a write or delete started and did not finish in time; it may still take effect.
  - `#[derive(Clone, Copy, Debug)] pub struct WorkerTimeouts { pub op: Duration, pub unlock: Duration }`, with `WorkerTimeouts::LIVE` = 5 s and 120 s.
  - `pub struct SecretWorker` with:
    - `spawn<B: SecretBackend>(backend: B, timeouts: WorkerTimeouts) -> SecretWorker`
    - `read(&self, key) -> Result<Option<String>, WorkerError>`
    - `write(&self, key, value) -> Result<(), WorkerError>`
    - `delete(&self, key) -> Result<(), WorkerError>`
    - `unlock(&self) -> Result<(), WorkerError>`
    All four block, at most until their deadline plus 1 s.
    - Under `cfg(any(test, feature = "testing"))`: `pending_for_test(&self) -> usize`, the jobs sent and not yet started.
  - Under `cfg(any(test, feature = "testing"))`: `pub mod fake` with:
    - `FakeKeyring { pub items, pub locked, pub unavailable, pub fail_reads, pub prompts, pub released, pub unlock: UnlockBehaviour, pub hang_writes: Hang, pub hang_deletes: Hang, pub calls: Vec<String> }`
    - `pub enum UnlockBehaviour { Succeed, Dismiss, Hang }`
    - `pub enum Hang { No, UntilReleased, Forever }`, for writes and deletes. `UntilReleased` waits for `released`, then applies the change: a slow D-Bus call. `Forever` never returns: a hung D-Bus call. `fail_reads` makes reads fail. `prompts` makes writes and deletes answer with a prompt, which the backend dismisses (`dismiss <op> <key>`) and reports as `Locked`.
    - `#[derive(Clone, Default)] pub struct FakeBackend(pub Arc<Mutex<FakeKeyring>>)`
- Rules (D13):
  - The worker is strictly serial: one job at a time, in order.
  - Every job carries the caller's deadline. A job that reaches the front of the queue after its deadline is answered `Expired` and never runs.
  - A started read or unlock runs only until its deadline; dropping it changes nothing in the keyring.
  - A started write or delete is never abandoned: the worker awaits it to completion (orchestrator decision, 2026-10-12). Every later job therefore runs after the change settled, so a later successful read is authoritative.
  - A caller that stops waiting for a write or delete gets `Ambiguous`: the change is still running, and its outcome is unknown to that caller. Task 12 turns this into `SecretStatus::Unknown` (D36).
  - A hung write or delete holds the worker, so every queued job expires or its caller times out. Everything fails closed, and no machine reconnects.

- [ ] **Step 1: Write the failing tests**

`crates/acd-inbox/tests/worker.rs`:

```rust
use std::time::{Duration, Instant};

use acd_inbox::secrets::fake::{FakeBackend, Hang, UnlockBehaviour};
use acd_inbox::secrets::{BackendError, SecretWorker, WorkerError, WorkerTimeouts};

const QUICK: WorkerTimeouts = WorkerTimeouts { op: Duration::from_millis(200), unlock: Duration::from_millis(600) };

fn calls(b: &FakeBackend) -> Vec<String> {
    b.0.lock().unwrap().calls.clone()
}

#[test]
fn operations_reach_the_backend() {
    let b = FakeBackend::default();
    let w = SecretWorker::spawn(b.clone(), QUICK);
    w.write("k", "v").unwrap();
    assert_eq!(w.read("k").unwrap().as_deref(), Some("v"));
    w.delete("k").unwrap();
    assert_eq!(w.read("k").unwrap(), None);
    assert_eq!(calls(&b), ["write k", "read k", "delete k", "read k"]);
}

#[test]
fn a_job_queued_past_its_deadline_never_runs() {
    let b = FakeBackend::default();
    b.0.lock().unwrap().unlock = UnlockBehaviour::Hang;
    let w = std::sync::Arc::new(SecretWorker::spawn(b.clone(), QUICK));
    let unlocking = {
        let w = w.clone();
        std::thread::spawn(move || w.unlock())
    };
    while !calls(&b).contains(&"unlock".to_owned()) {
        std::thread::yield_now();
    }
    let started = Instant::now();
    assert_eq!(w.write("k", "v"), Err(WorkerError::Expired), "queued behind the prompt past its deadline");
    assert!(started.elapsed() < QUICK.unlock + Duration::from_secs(1));
    assert_eq!(unlocking.join().unwrap(), Err(WorkerError::Backend(BackendError::Timeout)));
    assert!(!calls(&b).iter().any(|c| c.starts_with("write")), "the expired write never ran");
    assert!(b.0.lock().unwrap().items.is_empty());
}

/// Lets an `UntilReleased` change finish, on drop too.
struct Release(FakeBackend);

impl Drop for Release {
    fn drop(&mut self) {
        self.0.0.lock().unwrap().released = true;
    }
}

#[test]
fn a_write_past_its_deadline_settles_before_the_next_job_runs() {
    let b = FakeBackend::default();
    b.0.lock().unwrap().hang_writes = Hang::UntilReleased;
    let release = Release(b.clone());
    let w = std::sync::Arc::new(SecretWorker::spawn(b.clone(), WorkerTimeouts { op: Duration::from_millis(300), ..QUICK }));
    assert_eq!(w.write("k", "v"), Err(WorkerError::Ambiguous), "the caller stopped waiting; the write still runs");
    let reading = {
        let w = w.clone();
        std::thread::spawn(move || w.read("k"))
    };
    while w.pending_for_test() == 0 {
        std::thread::yield_now();
    }
    drop(release);
    assert_eq!(reading.join().unwrap().unwrap().as_deref(), Some("v"), "the read ran after the write settled");
    assert_eq!(calls(&b), ["write k", "read k"]);
}

#[test]
fn a_hung_write_makes_every_later_job_fail_without_running() {
    let b = FakeBackend::default();
    b.0.lock().unwrap().hang_writes = Hang::Forever;
    let w = SecretWorker::spawn(b.clone(), QUICK);
    assert_eq!(w.write("k", "v"), Err(WorkerError::Ambiguous));
    assert_eq!(w.read("k"), Err(WorkerError::Backend(BackendError::Timeout)));
    assert_eq!(w.delete("k"), Err(WorkerError::Ambiguous));
    assert_eq!(calls(&b), ["write k"], "nothing ran behind the hung write");
}

#[test]
fn a_change_that_needs_a_prompt_is_dismissed_and_reported_locked() {
    let b = FakeBackend::default();
    b.0.lock().unwrap().prompts = true;
    let w = SecretWorker::spawn(b.clone(), QUICK);
    assert_eq!(w.write("k", "v"), Err(WorkerError::Backend(BackendError::Locked)));
    assert_eq!(w.delete("k"), Err(WorkerError::Backend(BackendError::Locked)));
    assert!(calls(&b).contains(&"dismiss write k".to_owned()) && calls(&b).contains(&"dismiss delete k".to_owned()));
    assert!(b.0.lock().unwrap().items.is_empty());
}

#[test]
fn a_slow_unlock_is_a_plain_timeout_and_reads_never_prompt() {
    let b = FakeBackend::default();
    b.0.lock().unwrap().unlock = UnlockBehaviour::Hang;
    let w = SecretWorker::spawn(b.clone(), QUICK);
    assert_eq!(w.unlock(), Err(WorkerError::Backend(BackendError::Timeout)));
    b.0.lock().unwrap().unlock = UnlockBehaviour::Dismiss;
    assert_eq!(w.unlock(), Err(WorkerError::Backend(BackendError::Dismissed)));
    b.0.lock().unwrap().locked = true;
    assert_eq!(w.read("k"), Err(WorkerError::Backend(BackendError::Locked)));
    assert_eq!(calls(&b).iter().filter(|c| *c == "unlock").count(), 2, "the locked read did not prompt");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Add `pub mod secrets;` to `lib.rs` and `async-trait.workspace = true` to `[dependencies]`.
Run: `cargo test -p acd-inbox --test worker` (updates `Cargo.lock`)
Expected: compile error, `could not find secrets in acd_inbox`.

- [ ] **Step 3: Implement the worker in `secrets.rs`**

```rust
//! The secret worker (omarchy spec §9, plan D13): one thread owns the
//! keyring connection; every job carries a deadline.
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendError {
    /// The collection or item is locked; nothing was prompted.
    Locked,
    /// The user dismissed the unlock prompt.
    Dismissed,
    Timeout,
    /// No Secret Service on the session bus.
    Unavailable,
    Failed,
}

impl BackendError {
    /// A class for logs; never the D-Bus message.
    pub fn tag(self) -> &'static str {
        match self {
            BackendError::Locked => "locked",
            BackendError::Dismissed => "dismissed",
            BackendError::Timeout => "timeout",
            BackendError::Unavailable => "unavailable",
            BackendError::Failed => "failed",
        }
    }
}

/// A keyring. Implementations must not prompt except in `unlock`.
#[async_trait::async_trait]
pub trait SecretBackend: Send + 'static {
    async fn read(&mut self, key: &str) -> Result<Option<String>, BackendError>;
    async fn write(&mut self, key: &str, value: &str) -> Result<(), BackendError>;
    async fn delete(&mut self, key: &str) -> Result<(), BackendError>;
    async fn unlock(&mut self) -> Result<(), BackendError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkerError {
    Backend(BackendError),
    /// The deadline passed before the job started: it never ran.
    Expired,
    /// A write or delete started and was cut off: it may have applied.
    Ambiguous,
}

impl WorkerError {
    pub fn tag(self) -> &'static str {
        match self {
            WorkerError::Backend(e) => e.tag(),
            WorkerError::Expired => "expired",
            WorkerError::Ambiguous => "ambiguous",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct WorkerTimeouts {
    pub op: Duration,
    pub unlock: Duration,
}

impl WorkerTimeouts {
    /// §9: read, write, delete 5 s; unlock 120 s.
    pub const LIVE: WorkerTimeouts = WorkerTimeouts { op: Duration::from_secs(5), unlock: Duration::from_secs(120) };
}

enum Op {
    Read(String),
    Write(String, String),
    Delete(String),
    Unlock,
}

impl Op {
    fn mutates(&self) -> bool {
        matches!(self, Op::Write(..) | Op::Delete(_))
    }
}

struct Job {
    op: Op,
    deadline: Instant,
    reply: std_mpsc::SyncSender<Result<Option<String>, WorkerError>>,
}

pub struct SecretWorker {
    tx: tokio::sync::mpsc::UnboundedSender<Job>,
    timeouts: WorkerTimeouts,
    /// Jobs sent and not yet started.
    pending: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl SecretWorker {
    /// Starts the worker thread; it owns `backend` for the process's life.
    pub fn spawn<B: SecretBackend>(mut backend: B, timeouts: WorkerTimeouts) -> SecretWorker {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Job>();
        let pending = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let started = pending.clone();
        std::thread::Builder::new()
            .name("acd-inbox-secrets".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("the secret worker's runtime");
                rt.block_on(async move {
                    while let Some(job) = rx.recv().await {
                        started.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                        let now = Instant::now();
                        if now >= job.deadline {
                            let _ = job.reply.send(Err(WorkerError::Expired));
                            continue;
                        }
                        let mutates = job.op.mutates();
                        let run = async {
                            match job.op {
                                Op::Read(k) => backend.read(&k).await,
                                Op::Write(k, v) => backend.write(&k, &v).await.map(|()| None),
                                Op::Delete(k) => backend.delete(&k).await.map(|()| None),
                                Op::Unlock => backend.unlock().await.map(|()| None),
                            }
                        };
                        // A started write or delete runs to completion, so no
                        // change can land after a later read (plan D36). A read
                        // or unlock stops at the caller's deadline.
                        let result = if mutates {
                            run.await.map_err(WorkerError::Backend)
                        } else {
                            match tokio::time::timeout(job.deadline - now, run).await {
                                Ok(r) => r.map_err(WorkerError::Backend),
                                Err(_) => Err(WorkerError::Backend(BackendError::Timeout)),
                            }
                        };
                        let _ = job.reply.send(result);
                    }
                });
            })
            .expect("spawn the secret worker thread");
        SecretWorker { tx, timeouts, pending }
    }

    /// Test hook: jobs sent and not yet started.
    #[cfg(any(test, feature = "testing"))]
    pub fn pending_for_test(&self) -> usize {
        self.pending.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn call(&self, op: Op, limit: Duration) -> Result<Option<String>, WorkerError> {
        let mutates = op.mutates();
        let deadline = Instant::now() + limit;
        let (reply, rx) = std_mpsc::sync_channel(1);
        self.pending.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.tx
            .send(Job { op, deadline, reply })
            .map_err(|_| WorkerError::Backend(BackendError::Unavailable))?;
        match rx.recv_timeout(limit + Duration::from_secs(1)) {
            Ok(r) => r,
            // A write or delete still running (or queued behind one): its
            // outcome is unknown to this caller (plan D36).
            Err(_) if mutates => Err(WorkerError::Ambiguous),
            Err(_) => Err(WorkerError::Backend(BackendError::Timeout)),
        }
    }

    pub fn read(&self, key: &str) -> Result<Option<String>, WorkerError> {
        self.call(Op::Read(key.to_owned()), self.timeouts.op)
    }

    pub fn write(&self, key: &str, value: &str) -> Result<(), WorkerError> {
        self.call(Op::Write(key.to_owned(), value.to_owned()), self.timeouts.op).map(|_| ())
    }

    pub fn delete(&self, key: &str) -> Result<(), WorkerError> {
        self.call(Op::Delete(key.to_owned()), self.timeouts.op).map(|_| ())
    }

    pub fn unlock(&self) -> Result<(), WorkerError> {
        self.call(Op::Unlock, self.timeouts.unlock).map(|_| ())
    }
}

#[cfg(any(test, feature = "testing"))]
pub mod fake {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use super::{BackendError, SecretBackend};

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub enum UnlockBehaviour {
        #[default]
        Succeed,
        Dismiss,
        Hang,
    }

    /// How writes and deletes misbehave.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub enum Hang {
        #[default]
        No,
        /// Waits for `released`, then applies: a slow D-Bus call.
        UntilReleased,
        /// Never returns: a hung D-Bus call.
        Forever,
    }

    /// An in-memory keyring. `calls` records operations and keys, never values.
    #[derive(Default)]
    pub struct FakeKeyring {
        pub items: HashMap<String, String>,
        pub locked: bool,
        pub unavailable: bool,
        /// Reads fail with `Unavailable`; writes and deletes do not.
        pub fail_reads: bool,
        /// Writes and deletes answer with a prompt, which is dismissed.
        pub prompts: bool,
        pub unlock: UnlockBehaviour,
        pub hang_writes: Hang,
        pub hang_deletes: Hang,
        /// Lets `UntilReleased` changes finish.
        pub released: bool,
        pub calls: Vec<String>,
    }

    #[derive(Clone, Default)]
    pub struct FakeBackend(pub Arc<Mutex<FakeKeyring>>);

    impl FakeBackend {
        /// Records the call; the error the keyring's state forces, if any;
        /// else how a write (`deleting == false`) or delete misbehaves.
        fn enter(&self, call: String, deleting: bool) -> Result<Hang, BackendError> {
            let mut k = self.0.lock().unwrap();
            k.calls.push(call);
            if k.unavailable {
                return Err(BackendError::Unavailable);
            }
            if k.locked {
                return Err(BackendError::Locked);
            }
            if k.prompts {
                // What the real backend does with a returned prompt (D13).
                let call = k.calls.last().cloned().unwrap_or_default();
                k.calls.push(format!("dismiss {call}"));
                return Err(BackendError::Locked);
            }
            Ok(if deleting { k.hang_deletes } else { k.hang_writes })
        }

        async fn mutate(&self, hang: Hang, f: impl FnOnce(&mut HashMap<String, String>)) {
            match hang {
                Hang::No => {}
                Hang::Forever => std::future::pending::<()>().await,
                Hang::UntilReleased => {
                    while !self.0.lock().unwrap().released {
                        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                    }
                }
            }
            f(&mut self.0.lock().unwrap().items);
        }
    }

    #[async_trait::async_trait]
    impl SecretBackend for FakeBackend {
        async fn read(&mut self, key: &str) -> Result<Option<String>, BackendError> {
            let fail = {
                let mut k = self.0.lock().unwrap();
                k.calls.push(format!("read {key}"));
                k.fail_reads || k.unavailable
            };
            if fail {
                return Err(BackendError::Unavailable);
            }
            if self.0.lock().unwrap().locked {
                return Err(BackendError::Locked);
            }
            Ok(self.0.lock().unwrap().items.get(key).cloned())
        }

        async fn write(&mut self, key: &str, value: &str) -> Result<(), BackendError> {
            let hang = self.enter(format!("write {key}"), false)?;
            let (key, value) = (key.to_owned(), value.to_owned());
            self.mutate(hang, move |items| {
                items.insert(key, value);
            })
            .await;
            Ok(())
        }

        async fn delete(&mut self, key: &str) -> Result<(), BackendError> {
            let hang = self.enter(format!("delete {key}"), true)?;
            let key = key.to_owned();
            self.mutate(hang, move |items| {
                items.remove(&key);
            })
            .await;
            Ok(())
        }

        async fn unlock(&mut self) -> Result<(), BackendError> {
            let behaviour = {
                let mut k = self.0.lock().unwrap();
                k.calls.push("unlock".into());
                k.unlock
            };
            match behaviour {
                UnlockBehaviour::Succeed => {
                    self.0.lock().unwrap().locked = false;
                    Ok(())
                }
                UnlockBehaviour::Dismiss => Err(BackendError::Dismissed),
                UnlockBehaviour::Hang => std::future::pending().await,
            }
        }
    }
}
```

No mutex guard is held across an `.await`: `enter` and each `items` access lock and release within one statement.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p acd-inbox --locked --test worker`
Expected: PASS.

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy --workspace --all-targets --locked -- -D warnings`
Expected: no warnings.

```bash
git add Cargo.lock crates/acd-inbox
git commit -m "acd-inbox: secret worker with job deadlines and ambiguous-write detection"
```

---

### Task 12: Cached secret store, reconciliation and unlock

**Execution:** cloud-ok

**Files:**
- Modify: `crates/acd-inbox/src/secrets.rs` (`WorkerStore`)
- Create: `crates/acd-inbox/tests/secrets.rs`
- Modify: `docs/superpowers/specs/2026-10-12-omarchy-inbox-design.md` §4.3, §9, §13 (spec amendment, Step 4b)

**Interfaces:**
- Consumes: Task 11's `SecretWorker`, `WorkerError` and `fake`; Task 8's `SecretControl`; `acd_client::settings::{SecretError, SecretStore}`.
- Produces (in `acd_inbox::secrets`): `pub struct WorkerStore`, with `new(worker: SecretWorker)` and `prefill(&self, keys: &[String])`, and under `cfg(any(test, feature = "testing"))` `set_read_hook(&self, hook: Arc<dyn Fn() + Send + Sync>)` and `pending_for_test(&self) -> usize`. It implements `SecretStore` (its `status` is the trait default, so `Unknown` reads as `SecretStatus::Unknown`) and `SecretControl`.
- Rules (D14):
  - The cache holds an outcome per key: a value, `Locked`, `Unavailable`, `TimedOut` or `Unknown`, where `Unknown` is a change whose caller stopped waiting, or a re-pairing that could not commit (D36).
  - A cached error is returned without calling the worker. Only `refresh` (on unlock and on `retry_machine`) reads a key again.
  - `prefill` runs on the blocking pool before `Inbox::start` (Task 15). Every key in `settings.json` then has an entry, so `start`, `Service::new` and every settings snapshot are served from memory.
  - A read for a key with no entry (a machine being paired) goes to the worker; that only happens in `pair_machine`, which runs on the blocking pool.
  - An `Ambiguous` write or delete (the caller stopped waiting; the serial worker still runs it to completion) makes the key `Unknown`, and the call fails with `SecretError::unknown()`. acd-client then stops the machine's handle (Task 3, D36). Nothing is read back and no rollback guesses. The next read runs only after the change settled, so a successful `refresh` (`retry_machine`) is authoritative and clears `Unknown`.
  - `mark_unknown(key)` (acd-client calls it when a re-pairing from `Unknown` cannot commit) publishes `Unknown`.
  - Generations (D37): each key's generation goes up at the start of every write or delete, and those run only under acd-client's edit lock. A read publishes its result only if its generation is still current, compared under the cache lock. So a refresh that read the old credentials cannot overwrite a re-pairing's new ones.

- [ ] **Step 1: Write the failing tests**

`crates/acd-inbox/tests/secrets.rs`:

```rust
mod common;

use std::sync::Arc;
use std::time::Duration;

use acd_client::fake::FakeServer;
use acd_client::inbox::{Inbox, InboxOptions, MachineError};
use acd_client::settings::{SecretErrorKind, SecretStatus, SecretStore, Settings};
use acd_inbox::prefs::Prefs;
use acd_inbox::secrets::fake::{FakeBackend, Hang, UnlockBehaviour};
use acd_inbox::secrets::{SecretWorker, WorkerStore, WorkerTimeouts};
use acd_inbox::server::{SecretControl, ServerOptions, Service};
use common::*;
use serde_json::json;

const QUICK: WorkerTimeouts = WorkerTimeouts { op: Duration::from_millis(200), unlock: Duration::from_millis(300) };

fn store(backend: &FakeBackend, timeouts: WorkerTimeouts) -> Arc<WorkerStore> {
    Arc::new(WorkerStore::new(SecretWorker::spawn(backend.clone(), timeouts)))
}

fn calls(b: &FakeBackend) -> usize {
    b.0.lock().unwrap().calls.len()
}

#[test]
fn reads_are_cached_and_writes_go_through() {
    let backend = FakeBackend::default();
    backend.0.lock().unwrap().items.insert("k".into(), "v".into());
    let s = store(&backend, WorkerTimeouts::LIVE);
    s.prefill(&["k".into()]);
    for _ in 0..3 {
        assert_eq!(s.get("k").unwrap().as_deref(), Some("v"));
    }
    s.set("k", "w").unwrap();
    assert_eq!(s.get("k").unwrap().as_deref(), Some("w"));
    s.delete("k").unwrap();
    assert_eq!(s.get("k").unwrap(), None);
    assert_eq!(backend.0.lock().unwrap().calls, ["read k", "write k", "delete k"]);
}

#[test]
fn errors_are_cached_until_refreshed() {
    for (setup, kind) in [
        ((true, false), SecretErrorKind::Locked),
        ((false, true), SecretErrorKind::Other),
    ] {
        let backend = FakeBackend::default();
        {
            let mut k = backend.0.lock().unwrap();
            k.items.insert("k".into(), "v".into());
            (k.locked, k.unavailable) = setup;
        }
        let s = store(&backend, WorkerTimeouts::LIVE);
        s.prefill(&["k".into()]);
        let before = calls(&backend);
        assert_eq!(s.get("k").unwrap_err().kind(), kind);
        let _ = s.status("k");
        assert_eq!(calls(&backend), before, "a cached error does not go back to the keyring");
        {
            let mut k = backend.0.lock().unwrap();
            (k.locked, k.unavailable) = (false, false);
        }
        assert_eq!(s.get("k").unwrap_err().kind(), kind, "still cached");
        s.refresh(&["k".into()]);
        assert_eq!(s.get("k").unwrap().as_deref(), Some("v"));
    }
}

#[test]
fn a_locked_keyring_at_start_is_noted_and_never_prompted() {
    let backend = FakeBackend::default();
    {
        let mut k = backend.0.lock().unwrap();
        k.items.insert("k".into(), "v".into());
        k.locked = true;
    }
    let s = store(&backend, WorkerTimeouts::LIVE);
    s.prefill(&["k".into()]);
    assert_eq!(s.status("k"), SecretStatus::Locked);
    assert!(!backend.0.lock().unwrap().calls.iter().any(|c| c == "unlock"));
}

/// Lets an `UntilReleased` change finish, on drop too.
struct Release(FakeBackend);

impl Drop for Release {
    fn drop(&mut self) {
        self.0.0.lock().unwrap().released = true;
    }
}

#[test]
fn a_timed_out_write_leaves_the_key_unknown_until_a_read_after_it_settles() {
    let backend = FakeBackend::default();
    backend.0.lock().unwrap().hang_writes = Hang::UntilReleased;
    let release = Release(backend.clone());
    let s = store(&backend, WorkerTimeouts { op: Duration::from_millis(300), ..QUICK });
    assert_eq!(s.set("k", "v").unwrap_err().kind(), SecretErrorKind::Unknown);
    assert_eq!(s.status("k"), SecretStatus::Unknown);
    let retrying = {
        let s = s.clone();
        std::thread::spawn(move || s.refresh(&["k".into()]))
    };
    while s.pending_for_test() == 0 {
        std::thread::yield_now();
    }
    drop(release);
    retrying.join().unwrap();
    assert_eq!(s.get("k").unwrap().as_deref(), Some("v"), "the retry read the settled write");
    assert_eq!(s.status("k"), SecretStatus::Ok);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hung_write_fails_everything_closed_and_nothing_reconnects() {
    let f = FakeServer::start(TOKEN, acd_client::fake::Identity::Paired).await;
    let backend = FakeBackend::default();
    backend.0.lock().unwrap().items.insert("m1".into(), box_secret(&f));
    let s = store(&backend, QUICK);
    let prefill = s.clone();
    tokio::task::spawn_blocking(move || prefill.prefill(&["m1".into()])).await.unwrap();
    let inbox = Inbox::new(
        box_settings(&f),
        s.clone(),
        InboxOptions { timing: fast(), settings_path: None, client_name: "t".into(), client_version: "0".into() },
    );
    inbox.start();
    f.next_connection().await;
    backend.0.lock().unwrap().hang_writes = Hang::Forever;
    let line = format!("acd://127.0.0.1:{}?pin={}#{}", f.port(), f.pin(), TOKEN);
    let ib = inbox.clone();
    let result = tokio::task::spawn_blocking(move || ib.pair_machine(&line)).await.unwrap();
    assert!(matches!(result, Err(MachineError::SecretWrite(ref e)) if e.kind() == SecretErrorKind::Unknown));
    assert!(inbox.conn_stamp("m1").is_none(), "fail closed");
    let (s2, ib) = (s.clone(), inbox.clone());
    let retried = tokio::task::spawn_blocking(move || {
        s2.refresh(&["m1".into()]);
        ib.reconnect_machine("m1")
    })
    .await
    .unwrap();
    assert!(retried.is_err(), "the retry's read cannot run behind the hung write");
    assert!(inbox.conn_stamp("m1").is_none());
    assert_eq!(inbox.machines()[0].secret, SecretStatus::Unknown);
}

#[test]
fn mark_unknown_makes_the_key_unknown() {
    let backend = FakeBackend::default();
    backend.0.lock().unwrap().items.insert("k".into(), "v".into());
    let s = store(&backend, QUICK);
    s.prefill(&["k".into()]);
    s.mark_unknown("k");
    assert_eq!(s.status("k"), SecretStatus::Unknown);
    s.refresh(&["k".into()]);
    assert_eq!(s.status("k"), SecretStatus::Unreadable, "\"v\" is not a machine secret, but it was read");
}

#[test]
fn a_refresh_that_read_old_credentials_never_overwrites_a_re_pairing() {
    use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
    let backend = FakeBackend::default();
    backend.0.lock().unwrap().items.insert("m1".into(), "old".into());
    let s = store(&backend, WorkerTimeouts::LIVE);
    s.prefill(&["m1".into()]);
    let (inside, released) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
    let (i, r) = (inside.clone(), released.clone());
    s.set_read_hook(Arc::new(move || {
        i.store(true, SeqCst);
        while !r.load(SeqCst) {
            std::thread::yield_now();
        }
    }));
    struct Release(Arc<AtomicBool>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.store(true, SeqCst);
        }
    }
    let release = Release(released);
    let refreshing = {
        let s = s.clone();
        std::thread::spawn(move || s.refresh(&["m1".into()]))
    };
    while !inside.load(SeqCst) {
        std::thread::yield_now();
    }
    s.set("m1", "new").unwrap();
    drop(release);
    refreshing.join().unwrap();
    assert_eq!(s.get("m1").unwrap().as_deref(), Some("new"), "the refresh's old value was dropped");
}

#[test]
fn a_change_that_needs_a_prompt_is_refused_as_locked() {
    let backend = FakeBackend::default();
    backend.0.lock().unwrap().prompts = true;
    let s = store(&backend, QUICK);
    assert_eq!(s.set("k", "v").unwrap_err().kind(), SecretErrorKind::Locked);
    assert!(backend.0.lock().unwrap().items.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unconfirmed_rollback_is_reported_and_never_guessed() {
    let f = FakeServer::start(TOKEN, acd_client::fake::Identity::Paired).await;
    let d = tempfile::tempdir().unwrap();
    let blocker = d.path().join("file");
    std::fs::write(&blocker, "block").unwrap();
    // The write succeeds, the settings save fails, the rollback delete hangs.
    let backend = FakeBackend::default();
    backend.0.lock().unwrap().hang_deletes = Hang::Forever;
    let s = store(&backend, QUICK);
    let inbox = Inbox::new(
        Settings { machines: vec![], ..Settings::fresh() },
        s,
        InboxOptions { timing: fast(), settings_path: Some(blocker.join("settings.json")), client_name: "t".into(), client_version: "0".into() },
    );
    let line = format!("acd://127.0.0.1:{}?pin={}#{}", f.port(), f.pin(), TOKEN);
    let result = tokio::task::spawn_blocking(move || inbox.pair_machine(&line)).await.unwrap();
    assert!(matches!(result, Err(MachineError::PairRecoveryFailed)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_after_prefill_never_waits_on_the_keyring() {
    let backend = FakeBackend::default();
    backend.0.lock().unwrap().unavailable = true;
    let s = store(&backend, WorkerTimeouts::LIVE);
    let f = FakeServer::start(TOKEN, acd_client::fake::Identity::Paired).await;
    let keys = vec!["m1".to_owned()];
    let prefill = s.clone();
    tokio::task::spawn_blocking(move || prefill.prefill(&keys)).await.unwrap();
    let before = calls(&backend);
    let inbox = Inbox::new(
        box_settings(&f),
        s.clone(),
        InboxOptions { timing: fast(), settings_path: None, client_name: "t".into(), client_version: "0".into() },
    );
    inbox.start();
    let svc = Service::new(inbox.clone(), Prefs::default(), None, s.clone(), ServerOptions::default());
    svc.publish_settings().await;
    assert_eq!(inbox.machines()[0].secret, SecretStatus::Unreadable);
    assert_eq!(calls(&backend), before, "start and the settings snapshot used the cache only");
}

/// A rig whose inbox reads a `WorkerStore` over a locked fake keyring that
/// holds m1's secret.
async fn locked_rig(behaviour: UnlockBehaviour, timeouts: WorkerTimeouts) -> (Rig, FakeBackend) {
    let backend = FakeBackend::default();
    let b = backend.clone();
    let rig = rig_with(RigOptions {
        timing: held(),
        connect: false,
        secrets: Some(Box::new(move |f: &FakeServer| {
            {
                let mut k = b.0.lock().unwrap();
                k.items.insert("m1".into(), box_secret(f));
                k.locked = true;
                k.unlock = behaviour;
            }
            let s = store(&b, timeouts);
            s.prefill(&["m1".into()]);
            (s.clone() as Arc<dyn SecretStore>, s as Arc<dyn SecretControl>)
        })),
        ..Default::default()
    })
    .await;
    (rig, backend)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_locked_keyring_at_start_unlocks_and_reconnects_without_restart() {
    let (rig, _backend) = locked_rig(UnlockBehaviour::Succeed, WorkerTimeouts::LIVE).await;
    let mut v = View::connect(&rig.socket).await;
    let (_, settings, _) = v.greeting().await;
    assert_eq!(settings["machines"][0]["secret"], "locked");
    assert!(rig.inbox.conn_stamp("m1").is_none(), "no connection without the secret");
    assert_eq!(v.cmd(json!({"type":"unlock_secrets"})).await["type"], "ok");
    let c = rig.f.next_connection().await;
    c.send(json!({"type":"snapshot","epoch":"e1","seq":1,"agents":[pending_agent("a1","r1")]})).await;
    v.recv_until(|m| m["type"] == "settings" && m["machines"][0]["secret"] == "ok").await;
    v.recv_until(|m| m["type"] == "view" && m["view"]["cards"][0]["answerable"] == true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dismissed_or_slow_prompt_changes_nothing() {
    let (rig, _b) = locked_rig(UnlockBehaviour::Dismiss, QUICK).await;
    let mut v = View::connect(&rig.socket).await;
    v.greeting().await;
    assert_eq!(v.cmd(json!({"type":"unlock_secrets"})).await["code"], "secret_store_locked");
    let (rig2, _b2) = locked_rig(UnlockBehaviour::Hang, QUICK).await;
    let mut w = View::connect(&rig2.socket).await;
    w.greeting().await;
    assert_eq!(w.cmd(json!({"type":"unlock_secrets"})).await["code"], "secret_store_timeout");
    assert!(rig.inbox.conn_stamp("m1").is_none() && rig2.inbox.conn_stamp("m1").is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancel_is_answered_while_an_unlock_prompt_waits() {
    let backend = FakeBackend::default();
    let b = backend.clone();
    let rig = rig_with(RigOptions {
        timing: held(),
        secrets: Some(Box::new(move |f: &FakeServer| {
            {
                let mut k = b.0.lock().unwrap();
                k.items.insert("m1".into(), box_secret(f));
                k.unlock = UnlockBehaviour::Hang;
            }
            // A short unlock limit: runtime teardown waits for the blocked thread.
            let s = store(&b, WorkerTimeouts { op: Duration::from_secs(5), unlock: Duration::from_secs(2) });
            s.prefill(&["m1".into()]);
            (s.clone() as Arc<dyn SecretStore>, s as Arc<dyn SecretControl>)
        })),
        ..Default::default()
    })
    .await;
    let mut v = View::connect(&rig.socket).await;
    v.greeting().await;
    let r = v.cmd(json!({"type":"pick","card":"m1/a1","request":"r1","option":"allow","revision":1})).await;
    let id = r["result"].as_str().unwrap().to_owned();
    v.send(&json!({"type":"unlock_secrets","id":"u1"})).await;
    let cancel = tokio::time::timeout(Duration::from_secs(1), v.cmd(json!({"type":"cancel","action":id})))
        .await
        .expect("cancel waited for the keyring prompt");
    assert_eq!(cancel["result"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retry_reads_a_cached_failure_again() {
    let backend = FakeBackend::default();
    let b = backend.clone();
    let rig = rig_with(RigOptions {
        timing: held(),
        connect: false,
        secrets: Some(Box::new(move |f: &FakeServer| {
            {
                let mut k = b.0.lock().unwrap();
                k.items.insert("m1".into(), box_secret(f));
                k.unavailable = true;
            }
            let s = store(&b, QUICK);
            s.prefill(&["m1".into()]);
            (s.clone() as Arc<dyn SecretStore>, s as Arc<dyn SecretControl>)
        })),
        ..Default::default()
    })
    .await;
    backend.0.lock().unwrap().unavailable = false;
    let mut v = View::connect(&rig.socket).await;
    v.greeting().await;
    assert_eq!(v.cmd(json!({"type":"retry_machine","machine":"m1"})).await["type"], "ok");
    rig.f.next_connection().await;
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p acd-inbox --locked --test secrets`
Expected: compile error, `cannot find struct WorkerStore`. After implementing, remove the generation check from `publish` as a check: `a_refresh_that_read_old_credentials_never_overwrites_a_re_pairing` must fail. Restore it.

- [ ] **Step 3: Implement `WorkerStore`**

Append to `secrets.rs` (above `pub mod fake`):

```rust
use std::collections::HashMap;
use std::sync::Mutex;

use acd_client::settings::{SecretError, SecretStore};

use crate::server::SecretControl;

/// What the store knows about a key (plan D14, D36).
#[derive(Clone)]
enum Cached {
    Value(Option<String>),
    Locked,
    Unavailable,
    TimedOut,
    /// A write or delete whose caller stopped waiting, or a re-pairing that
    /// could not commit. Cleared only by a successful read (`refresh`,
    /// which runs after the change settled) or a confirmed change.
    Unknown,
}

impl Cached {
    fn of(e: WorkerError) -> Cached {
        match e {
            WorkerError::Backend(BackendError::Locked | BackendError::Dismissed) => Cached::Locked,
            WorkerError::Backend(BackendError::Unavailable | BackendError::Failed) => Cached::Unavailable,
            WorkerError::Backend(BackendError::Timeout) | WorkerError::Expired => Cached::TimedOut,
            WorkerError::Ambiguous => Cached::Unknown,
        }
    }

    fn error(&self) -> Option<SecretError> {
        match self {
            Cached::Value(_) => None,
            Cached::Locked => Some(SecretError::locked()),
            Cached::TimedOut => Some(SecretError::timeout()),
            Cached::Unknown => Some(SecretError::unknown()),
            Cached::Unavailable => Some(SecretError("keyring unavailable".into())),
        }
    }
}

/// Entries and their generations (plan D37). A key's generation goes up at
/// the start of every write or delete; a read publishes its result only if
/// the generation it started under is still current.
#[derive(Default)]
struct Cache {
    entries: HashMap<String, Cached>,
    generations: HashMap<String, u64>,
}

/// The inbox's `SecretStore`: a per-key cache in front of the worker.
pub struct WorkerStore {
    worker: SecretWorker,
    cache: Mutex<Cache>,
    #[cfg(any(test, feature = "testing"))]
    read_hook: Mutex<Option<std::sync::Arc<dyn Fn() + Send + Sync>>>,
}

impl WorkerStore {
    pub fn new(worker: SecretWorker) -> WorkerStore {
        WorkerStore {
            worker,
            cache: Mutex::new(Cache::default()),
            #[cfg(any(test, feature = "testing"))]
            read_hook: Mutex::new(None),
        }
    }

    /// Test hook: runs after a read returns from the worker and before its
    /// result is published.
    #[cfg(any(test, feature = "testing"))]
    pub fn set_read_hook(&self, hook: std::sync::Arc<dyn Fn() + Send + Sync>) {
        *self.read_hook.lock().unwrap() = Some(hook);
    }

    /// Reads `keys` now and caches each outcome, errors included. Run on
    /// the blocking pool before `Inbox::start`. Never prompts (§9).
    pub fn prefill(&self, keys: &[String]) {
        for key in keys {
            let _ = self.read_through(key);
        }
    }

    fn generation(&self, key: &str) -> u64 {
        self.cache.lock().unwrap().generations.get(key).copied().unwrap_or(0)
    }

    /// Starts a change: a read already in flight for `key` will not publish.
    fn bump(&self, key: &str) -> u64 {
        let mut c = self.cache.lock().unwrap();
        let g = c.generations.entry(key.to_owned()).or_insert(0);
        *g += 1;
        *g
    }

    /// Compare-and-publish under the cache lock (plan D37). False when a
    /// change started since `generation` was taken.
    fn publish(&self, key: &str, generation: u64, entry: Cached) -> bool {
        let mut c = self.cache.lock().unwrap();
        if c.generations.get(key).copied().unwrap_or(0) != generation {
            return false;
        }
        c.entries.insert(key.to_owned(), entry);
        true
    }

    fn read_through(&self, key: &str) -> Result<Option<String>, SecretError> {
        let generation = self.generation(key);
        let result = self.worker.read(key);
        #[cfg(any(test, feature = "testing"))]
        if let Some(hook) = self.read_hook.lock().unwrap().clone() {
            hook();
        }
        let entry = match &result {
            Ok(value) => Cached::Value(value.clone()),
            Err(e) => {
                tracing::warn!(error = e.tag(), "keyring read failed");
                Cached::of(*e)
            }
        };
        if !self.publish(key, generation, entry.clone()) {
            tracing::debug!("a keyring read finished after a change; its result was dropped");
            // Answer from what the change left in the cache.
            let current = self.cache.lock().unwrap().entries.get(key).cloned();
            return match current {
                Some(Cached::Value(v)) => Ok(v),
                Some(other) => Err(other.error().expect("an error state")),
                None => Err(SecretError::unknown()),
            };
        }
        match entry {
            Cached::Value(v) => Ok(v),
            other => Err(other.error().expect("an error state")),
        }
    }

    /// Test hook: jobs waiting for the worker.
    #[cfg(any(test, feature = "testing"))]
    pub fn pending_for_test(&self) -> usize {
        self.worker.pending_for_test()
    }

    fn after_change(&self, key: &str, generation: u64, want: Option<&str>, r: Result<(), WorkerError>) -> Result<(), SecretError> {
        match r {
            Ok(()) => {
                self.publish(key, generation, Cached::Value(want.map(str::to_owned)));
                Ok(())
            }
            // The change is still running in the serial worker (plan D36):
            // unknown until a later read, which can only run after it.
            Err(WorkerError::Ambiguous) => {
                tracing::warn!("keyring change unconfirmed; the machine's secret is unknown");
                self.publish(key, generation, Cached::Unknown);
                Err(SecretError::unknown())
            }
            // Expired or refused: nothing changed. The cached entry stands.
            Err(e) => {
                tracing::warn!(error = e.tag(), "keyring change failed");
                Err(Cached::of(e).error().expect("an error state"))
            }
        }
    }
}

impl SecretStore for WorkerStore {
    fn get(&self, key: &str) -> Result<Option<String>, SecretError> {
        let hit = self.cache.lock().unwrap().entries.get(key).cloned();
        match hit {
            Some(Cached::Value(v)) => Ok(v),
            Some(other) => Err(other.error().expect("an error state")),
            None => self.read_through(key),
        }
    }

    /// Called only under acd-client's edit lock, so changes to one key never
    /// overlap.
    fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
        let generation = self.bump(key);
        let r = self.worker.write(key, value);
        self.after_change(key, generation, Some(value), r)
    }

    fn delete(&self, key: &str) -> Result<(), SecretError> {
        let generation = self.bump(key);
        let r = self.worker.delete(key);
        self.after_change(key, generation, None, r)
    }

    fn mark_unknown(&self, key: &str) {
        let generation = self.bump(key);
        self.publish(key, generation, Cached::Unknown);
    }
}

impl SecretControl for WorkerStore {
    fn unlock(&self) -> Result<(), SecretError> {
        self.worker.unlock().map_err(|e| {
            tracing::info!(error = e.tag(), "keyring unlock failed");
            Cached::of(e).error().expect("an error state")
        })
    }

    /// Reads `keys` again; the authoritative way out of `Unknown` and of a
    /// cached error. Entries are replaced only through `publish`.
    fn refresh(&self, keys: &[String]) {
        self.prefill(keys);
    }
}
```

Move the new `use` lines to the top of `secrets.rs`, next to the existing imports.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p acd-inbox --locked --test secrets --test worker`
Expected: PASS.

- [ ] **Step 4b: Amend the spec**

The orchestrator decided (2026-10-12) how unconfirmed keyring changes behave. In `docs/superpowers/specs/2026-10-12-omarchy-inbox-design.md`:
- In §4.3 "Secret status per machine", change `ok | locked | missing | unreadable` to `ok | locked | missing | unreadable | unknown`.
- Append to the §9 secret-worker bullet:

```markdown
The worker is serial and never abandons a started write or delete: a
caller that stops waiting gets `secret_store_timeout`, the machine's
secret becomes `unknown`, and its connection stops (fail closed). Only a
successful read (Retry) or a re-pairing recovers it; an `unknown` previous
secret is not restored when a re-pairing fails, and nothing is guessed.
```

- Add to §13: "Unconfirmed keyring changes (2026-10-12, orchestrator): serial worker, `unknown` status, recovery by Retry or re-pairing."

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy --workspace --all-targets --locked -- -D warnings`
Expected: no warnings.

```bash
git add crates/acd-inbox docs/superpowers/specs/2026-10-12-omarchy-inbox-design.md
git commit -m "acd-inbox: cached secret store with error states, unknown after unconfirmed changes, unlock and retry"
```

---

### Task 13: Notifier

**Execution:** cloud-ok

**Files:**
- Create: `crates/acd-inbox/src/notify.rs`, `crates/acd-inbox/tests/notify.rs`
- Modify: `crates/acd-inbox/src/lib.rs` (`pub mod notify;`)

**Interfaces:**
- Consumes: `acd_client::view::{CardView, InboxView}`; `acd_protocol::{RequestKind, RequestStatus}`; Task 5's `prefs::Prefs`.
- Produces (in `acd_inbox::notify`):
  - `pub const OPEN_ACTION: &str = "open"; pub const PLUGIN_ID: &str = "tonibergholm.agentcaller"; pub const OPEN_TIMEOUT: Duration = Duration::from_secs(5);`
  - `#[derive(Clone, Debug, PartialEq, Eq, Hash)] pub struct NoteKey { pub card: String, pub request: String, pub revision: u64 }`
  - `#[derive(Clone, Debug, PartialEq)] pub enum NoteCmd { Show { key: NoteKey, summary: String, body: String }, Close { key: NoteKey } }`
  - `#[derive(Default)] pub struct Reconciler`, with `reconcile(&mut self, view: &InboxView, enabled: bool, panel_open: bool) -> Vec<NoteCmd>` (D16).
  - Text helpers: `pub fn clean(s: &str, max: usize) -> String`, `pub fn request_label(kind: RequestKind, title: &str) -> String`, `pub fn text(card: &CardView) -> Option<(String, String)>` (D15).
  - `#[async_trait] pub trait NotifyBackend: Send + Sync + 'static`, with:
    - `async fn show(&self, summary: &str, body: &str, open_action: bool) -> Result<u32, NotifyError>`
    - `async fn close(&self, id: u32)`
    - `async fn owner(&self) -> Option<String>`
  - `pub enum NotifyError { Unavailable, Failed }`.
  - `#[derive(Clone, Debug, PartialEq, Eq)] pub struct ActionSignal { pub id: u32, pub action: String, pub sender: String }`.
  - `pub trait Opener: Send + Sync + 'static { fn open(&self, card: &str); }`.
  - `pub struct ShellOpener { pub shell: PathBuf, pub timeout: Duration }`, with:
    - `find(path_var: Option<&OsStr>, fallback: &Path) -> Option<ShellOpener>`
    - `command(&self, card: &str) -> tokio::process::Command`
    - `async fn run(&self, card: &str) -> OpenResult`, where `pub enum OpenResult { Exited, TimedOut, Failed }`
  - `pub struct Notifier`, with:
    - `Notifier::new(backend: Arc<dyn NotifyBackend>, opener: Option<Arc<dyn Opener>>) -> Arc<Notifier>`
    - `async fn apply(&self, view: &InboxView, enabled: bool, panel_open: bool)`
    - `async fn on_action(&self, signal: &ActionSignal) -> bool` (D17)
    - `async fn run(self: Arc<Self>, views: watch::Receiver<InboxView>, prefs: watch::Receiver<Prefs>, panel: watch::Receiver<bool>, actions: mpsc::Receiver<ActionSignal>)`

- [ ] **Step 1: Write the failing tests**

`crates/acd-inbox/tests/notify.rs`:

```rust
use std::sync::{Arc, Mutex};
use std::time::Duration;

use acd_client::view::{CardGroup, CardView, InboxView, TrayColor, TraySummary};
use acd_inbox::notify::*;
use acd_protocol::RequestKind;
use serde_json::json;

fn card(id: &str, revision: u64, countdown: bool) -> CardView {
    let agent = serde_json::from_value(json!({"id":id,"machine_id":"m","source":"herdr","kind":"claude","title":"api\u{202E} refactor\n","cwd":null,"project":null,"state":"needs_you","state_since":"2026-10-08T00:00:00Z","failed":false,"last_message":null,"can_send":true,"pending":{"id":format!("r-{id}"),"revision":revision,"agent_id":id,"kind":"permission","status":"open","answer_path":"hook","title":"Bash","detail":"rm -rf SECRET-DETAIL","options":[],"questions":null,"free_text":false,"raw_screen":null,"opened_at":"2026-10-08T00:00:00Z"}})).unwrap();
    CardView {
        key: format!("m1/{id}"),
        machine_key: "m1".into(),
        machine_name: "Box".into(),
        agent,
        group: CardGroup::Request,
        live: true,
        offline_since_ms: None,
        answerable: true,
        free_text_answerable: false,
        selectable_options: vec![],
        can_reply: false,
        can_mark_seen: false,
        busy: false,
        notice: None,
        action: countdown.then(|| acd_client::view::ActionView {
            id: "x".into(),
            card_key: format!("m1/{id}"),
            label: "Yes".into(),
            detail: None,
            fires_at_ms: 0.,
        }),
    }
}

fn view(cards: Vec<CardView>) -> InboxView {
    InboxView {
        machines: vec![],
        cards,
        hidden_idle: 0,
        show_idle: false,
        show_subagents: false,
        show_hook_only: false,
        tray: TraySummary { color: TrayColor::Amber, needs_you: 1 },
        actions: vec![],
    }
}

fn key(id: &str, revision: u64) -> NoteKey {
    NoteKey { card: format!("m1/{id}"), request: format!("r-{id}"), revision }
}

fn shown(cmds: &[NoteCmd]) -> Vec<NoteKey> {
    cmds.iter().filter_map(|c| if let NoteCmd::Show { key, .. } = c { Some(key.clone()) } else { None }).collect()
}

#[test]
fn one_notification_per_request_revision_and_none_for_repeats() {
    let mut r = Reconciler::default();
    assert_eq!(shown(&r.reconcile(&view(vec![card("a1", 1, false)]), true, false)), [key("a1", 1)]);
    assert!(r.reconcile(&view(vec![card("a1", 1, false)]), true, false).is_empty());
    let bumped = r.reconcile(&view(vec![card("a1", 2, false)]), true, false);
    assert!(bumped.contains(&NoteCmd::Close { key: key("a1", 1) }));
    assert_eq!(shown(&bumped), [key("a1", 2)]);
    assert_eq!(r.reconcile(&view(vec![]), true, false), [NoteCmd::Close { key: key("a1", 2) }]);
}

#[test]
fn none_while_a_panel_is_open_off_or_counting_and_not_later_either() {
    let mut r = Reconciler::default();
    assert!(r.reconcile(&view(vec![card("a1", 1, false)]), true, true).is_empty());
    assert!(r.reconcile(&view(vec![card("a1", 1, false)]), true, false).is_empty(), "seen while the panel was open");
    assert!(r.reconcile(&view(vec![card("a1", 1, false), card("a2", 1, true)]), true, false).is_empty(), "own countdown");
    assert!(r.reconcile(&view(vec![card("a1", 1, false), card("a2", 1, false), card("a3", 1, false)]), false, false).is_empty());
}

#[test]
fn the_text_is_templated_cleaned_and_holds_no_detail() {
    let (summary, body) = text(&card("a1", 1, false)).unwrap();
    assert_eq!(summary, "api refactor on Box needs you");
    assert_eq!(body, "bash approval");
    assert!(!format!("{summary}{body}").contains("SECRET"));
    assert_eq!(request_label(RequestKind::Permission, "Run npm test?"), "approval");
    assert_eq!(request_label(RequestKind::Question, "x"), "question");
    assert_eq!(request_label(RequestKind::BlockedUnparsed, "x"), "blocked");
    assert_eq!(clean(&"y".repeat(100), 60).chars().count(), 60);
}

#[derive(Default)]
struct FakeNotify {
    next: Mutex<u32>,
    shows: Mutex<Vec<(u32, String, bool)>>,
    closes: Mutex<Vec<u32>>,
    owner: Mutex<Option<String>>,
    fail: Mutex<bool>,
    attempts: Mutex<u32>,
}

#[async_trait::async_trait]
impl NotifyBackend for FakeNotify {
    async fn show(&self, summary: &str, _body: &str, open_action: bool) -> Result<u32, NotifyError> {
        *self.attempts.lock().unwrap() += 1;
        if *self.fail.lock().unwrap() {
            return Err(NotifyError::Unavailable);
        }
        let mut n = self.next.lock().unwrap();
        *n += 1;
        self.shows.lock().unwrap().push((*n, summary.into(), open_action));
        Ok(*n)
    }
    async fn close(&self, id: u32) {
        self.closes.lock().unwrap().push(id);
    }
    async fn owner(&self) -> Option<String> {
        self.owner.lock().unwrap().clone()
    }
}

#[derive(Default)]
struct Opened(Mutex<Vec<String>>);

impl Opener for Opened {
    fn open(&self, card: &str) {
        self.0.lock().unwrap().push(card.into());
    }
}

fn fake() -> (Arc<FakeNotify>, Arc<Opened>, Arc<Notifier>) {
    let backend = Arc::new(FakeNotify::default());
    *backend.owner.lock().unwrap() = Some(":1.10".into());
    let opened = Arc::new(Opened::default());
    let n = Notifier::new(backend.clone(), Some(opened.clone()));
    (backend, opened, n)
}

#[tokio::test]
async fn the_open_action_opens_its_card_and_resolving_closes_it() {
    let (backend, opened, n) = fake();
    n.apply(&view(vec![card("a1", 1, false)]), true, false).await;
    let (id, _, with_open) = backend.shows.lock().unwrap()[0].clone();
    assert!(with_open);
    assert!(n.on_action(&ActionSignal { id, action: OPEN_ACTION.into(), sender: ":1.10".into() }).await);
    assert_eq!(*opened.0.lock().unwrap(), ["m1/a1"]);
    n.apply(&view(vec![]), true, false).await;
    assert_eq!(*backend.closes.lock().unwrap(), [id]);
}

#[tokio::test]
async fn actions_from_unknown_ids_or_senders_are_ignored() {
    let (backend, opened, n) = fake();
    n.apply(&view(vec![card("a1", 1, false)]), true, false).await;
    let id = backend.shows.lock().unwrap()[0].0;
    assert!(!n.on_action(&ActionSignal { id: id + 7, action: OPEN_ACTION.into(), sender: ":1.10".into() }).await);
    assert!(!n.on_action(&ActionSignal { id, action: OPEN_ACTION.into(), sender: ":1.99".into() }).await);
    assert!(!n.on_action(&ActionSignal { id, action: "default".into(), sender: ":1.10".into() }).await);
    assert!(opened.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn actions_from_a_new_notification_server_are_ignored() {
    let (backend, opened, n) = fake();
    n.apply(&view(vec![card("a1", 1, false)]), true, false).await;
    let id = backend.shows.lock().unwrap()[0].0;
    *backend.owner.lock().unwrap() = Some(":1.42".into());
    assert!(!n.on_action(&ActionSignal { id, action: OPEN_ACTION.into(), sender: ":1.42".into() }).await);
    assert!(opened.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_failing_server_turns_notifications_off_for_the_session() {
    let (backend, _opened, n) = fake();
    *backend.fail.lock().unwrap() = true;
    n.apply(&view(vec![card("a1", 1, false), card("a2", 1, false)]), true, false).await;
    assert_eq!(*backend.attempts.lock().unwrap(), 1, "the batch stops at the first failure (D28)");
    *backend.fail.lock().unwrap() = false;
    n.apply(&view(vec![card("a1", 1, false), card("a2", 1, false), card("a3", 1, false)]), true, false).await;
    assert!(backend.shows.lock().unwrap().is_empty());
    assert_eq!(*backend.attempts.lock().unwrap(), 1);
}

fn script(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("omarchy-shell");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[tokio::test]
async fn the_opener_runs_an_argv_without_a_shell_and_times_out() {
    let d = tempfile::tempdir().unwrap();
    let out = d.path().join("args");
    let shell = script(d.path(), &format!("printf '%s|' \"$@\" > '{}'", out.display()));
    let opener = ShellOpener { shell, timeout: Duration::from_secs(5) };
    assert_eq!(opener.run("m1/a b;touch x").await, OpenResult::Exited);
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "tonibergholm.agentcaller|openCard|m1/a b;touch x|");
    assert!(!d.path().join("x").exists());
    let slow = ShellOpener { shell: script(d.path(), "sleep 30"), timeout: Duration::from_millis(200) };
    let started = std::time::Instant::now();
    assert_eq!(slow.run("m1/a").await, OpenResult::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn omarchy_shell_is_found_on_path_else_in_the_fallback_dir() {
    let d = tempfile::tempdir().unwrap();
    let empty = tempfile::tempdir().unwrap();
    script(d.path(), "true");
    let found = ShellOpener::find(Some(d.path().as_os_str()), empty.path()).unwrap();
    assert_eq!(found.shell, d.path().join("omarchy-shell"));
    let fallback = ShellOpener::find(Some(empty.path().as_os_str()), d.path()).unwrap();
    assert_eq!(fallback.shell, d.path().join("omarchy-shell"));
    assert!(ShellOpener::find(Some(empty.path().as_os_str()), empty.path()).is_none());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Add `pub mod notify;` to `lib.rs`.
Run: `cargo test -p acd-inbox --locked --test notify`
Expected: compile error, `could not find notify in acd_inbox`.

- [ ] **Step 3: Implement `notify.rs`**

```rust
//! Notifications (omarchy spec §8, plan D15 to D17). Views are reconciled into
//! show and close commands; the D-Bus side sits behind `NotifyBackend`.
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use acd_client::view::{CardView, InboxView};
use acd_protocol::{RequestKind, RequestStatus};
use tokio::sync::{mpsc, watch};

use crate::prefs::Prefs;

pub const OPEN_ACTION: &str = "open";
pub const PLUGIN_ID: &str = "tonibergholm.agentcaller";
pub const OPEN_TIMEOUT: Duration = Duration::from_secs(5);
const TITLE_MAX: usize = 60;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct NoteKey {
    pub card: String,
    pub request: String,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum NoteCmd {
    Show { key: NoteKey, summary: String, body: String },
    Close { key: NoteKey },
}

/// Which requests were considered and which have a notification on screen.
#[derive(Default)]
pub struct Reconciler {
    seen: HashSet<NoteKey>,
    shown: HashSet<NoteKey>,
}

impl Reconciler {
    /// A request is considered once, when it first appears (plan D16).
    pub fn reconcile(&mut self, view: &InboxView, enabled: bool, panel_open: bool) -> Vec<NoteCmd> {
        let current: HashMap<NoteKey, &CardView> = view
            .cards
            .iter()
            .filter_map(|c| {
                let p = c.agent.pending.as_ref()?;
                (p.status == RequestStatus::Open)
                    .then(|| (NoteKey { card: c.key.clone(), request: p.id.clone(), revision: p.revision }, c))
            })
            .collect();
        let mut out = Vec::new();
        let gone: Vec<NoteKey> = self.shown.iter().filter(|k| !current.contains_key(*k)).cloned().collect();
        for key in gone {
            self.shown.remove(&key);
            out.push(NoteCmd::Close { key });
        }
        self.seen.retain(|k| current.contains_key(k));
        let mut fresh: Vec<(&NoteKey, &&CardView)> = current.iter().filter(|(k, _)| !self.seen.contains(*k)).collect();
        fresh.sort_by(|a, b| a.0.card.cmp(&b.0.card));
        for (key, card) in fresh {
            self.seen.insert(key.clone());
            if !enabled || panel_open || card.action.is_some() {
                continue;
            }
            if let Some((summary, body)) = text(card) {
                self.shown.insert(key.clone());
                out.push(NoteCmd::Show { key: key.clone(), summary, body });
            }
        }
        out
    }
}

/// Agent text made safe to show: whitespace, control, bidi and separator
/// characters become single spaces, the ends are trimmed, and it is cut to
/// `max` characters ending in "…".
pub fn clean(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut gap = false;
    for c in s.chars() {
        if c.is_whitespace() || acd_protocol::text::is_forbidden_control(c) || acd_protocol::text::is_forbidden_format(c) {
            gap = !out.is_empty();
            continue;
        }
        if gap {
            out.push(' ');
            gap = false;
        }
        out.push(c);
    }
    if out.chars().count() <= max {
        return out;
    }
    let mut cut: String = out.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// The panel's type tag in lower case (`panel.ts` `typeTag`).
pub fn request_label(kind: RequestKind, title: &str) -> String {
    let tool = {
        let mut chars = title.chars();
        chars.next().is_some_and(|c| c.is_ascii_alphabetic())
            && title.len() <= 40
            && chars.all(|c| c.is_ascii_alphanumeric() || "_.:-".contains(c))
    };
    match kind {
        RequestKind::Permission if tool => format!("{} approval", title.to_lowercase()),
        RequestKind::Permission => "approval".into(),
        RequestKind::Question => "question".into(),
        RequestKind::BlockedUnparsed => "blocked".into(),
        RequestKind::Unknown => "request".into(),
    }
}

/// Summary and body (plan D15). Never the request detail or screen.
pub fn text(card: &CardView) -> Option<(String, String)> {
    let p = card.agent.pending.as_ref()?;
    Some((
        format!("{} on {} needs you", clean(&card.agent.title, TITLE_MAX), clean(&card.machine_name, TITLE_MAX)),
        request_label(p.kind, &p.title),
    ))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotifyError {
    Unavailable,
    Failed,
}

#[async_trait::async_trait]
pub trait NotifyBackend: Send + Sync + 'static {
    async fn show(&self, summary: &str, body: &str, open_action: bool) -> Result<u32, NotifyError>;
    async fn close(&self, id: u32);
    /// The unique name that owns `org.freedesktop.Notifications` now.
    async fn owner(&self) -> Option<String>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionSignal {
    pub id: u32,
    pub action: String,
    pub sender: String,
}

pub trait Opener: Send + Sync + 'static {
    fn open(&self, card: &str);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenResult {
    Exited,
    TimedOut,
    Failed,
}

/// Runs `omarchy-shell tonibergholm.agentcaller openCard <key>` (§8).
pub struct ShellOpener {
    pub shell: PathBuf,
    pub timeout: Duration,
}

impl ShellOpener {
    /// `omarchy-shell` on `path_var`, else in `fallback` (`/usr/bin`, §4.2).
    pub fn find(path_var: Option<&OsStr>, fallback: &Path) -> Option<ShellOpener> {
        let on_path = path_var.and_then(|p| std::env::split_paths(p).map(|d| d.join("omarchy-shell")).find(|c| c.is_file()));
        let shell = on_path.or_else(|| Some(fallback.join("omarchy-shell")).filter(|c| c.is_file()))?;
        Some(ShellOpener { shell, timeout: OPEN_TIMEOUT })
    }

    /// An argv, no shell; output discarded; killed if dropped.
    pub fn command(&self, card: &str) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new(&self.shell);
        cmd.arg(PLUGIN_ID)
            .arg("openCard")
            .arg(card)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        cmd
    }

    pub async fn run(&self, card: &str) -> OpenResult {
        let Ok(mut child) = self.command(card).spawn() else {
            tracing::warn!("open action could not start omarchy-shell");
            return OpenResult::Failed;
        };
        match tokio::time::timeout(self.timeout, child.wait()).await {
            Ok(_) => OpenResult::Exited,
            Err(_) => {
                let _ = child.kill().await;
                tracing::warn!("open action timed out");
                OpenResult::TimedOut
            }
        }
    }
}

impl Opener for ShellOpener {
    fn open(&self, card: &str) {
        let me = ShellOpener { shell: self.shell.clone(), timeout: self.timeout };
        let card = card.to_owned();
        tokio::spawn(async move {
            me.run(&card).await;
        });
    }
}

struct State {
    reconciler: Reconciler,
    ids: HashMap<NoteKey, u32>,
    /// Notification id → card and the server owner that showed it.
    by_id: HashMap<u32, (String, Option<String>)>,
    available: bool,
}

pub struct Notifier {
    backend: Arc<dyn NotifyBackend>,
    opener: Option<Arc<dyn Opener>>,
    state: Mutex<State>,
}

impl Notifier {
    pub fn new(backend: Arc<dyn NotifyBackend>, opener: Option<Arc<dyn Opener>>) -> Arc<Notifier> {
        Arc::new(Notifier {
            backend,
            opener,
            state: Mutex::new(State {
                reconciler: Reconciler::default(),
                ids: HashMap::new(),
                by_id: HashMap::new(),
                available: true,
            }),
        })
    }

    /// One reconcile pass; shows and closes notifications.
    pub async fn apply(&self, view: &InboxView, enabled: bool, panel_open: bool) {
        let cmds = {
            let mut s = self.state.lock().unwrap();
            let on = enabled && s.available;
            s.reconciler.reconcile(view, on, panel_open)
        };
        let owner = if cmds.iter().any(|c| matches!(c, NoteCmd::Show { .. })) { self.backend.owner().await } else { None };
        for cmd in cmds {
            match cmd {
                NoteCmd::Show { key, summary, body } => {
                    match self.backend.show(&summary, &body, self.opener.is_some()).await {
                        Ok(id) => {
                            let mut s = self.state.lock().unwrap();
                            s.by_id.insert(id, (key.card.clone(), owner.clone()));
                            s.ids.insert(key, id);
                        }
                        Err(e) => {
                            // D28: off for the session; the rest of this batch is dropped.
                            tracing::warn!(error = ?e, "notification failed; notifications are off for this session");
                            self.state.lock().unwrap().available = false;
                            return;
                        }
                    }
                }
                NoteCmd::Close { key } => {
                    let id = {
                        let mut s = self.state.lock().unwrap();
                        let id = s.ids.remove(&key);
                        if let Some(id) = id {
                            s.by_id.remove(&id);
                        }
                        id
                    };
                    if let Some(id) = id {
                        self.backend.close(id).await;
                    }
                }
            }
        }
    }

    /// Opens the card only for an id this notifier created, sent by the
    /// current owner of the notification name, unchanged since then.
    pub async fn on_action(&self, signal: &ActionSignal) -> bool {
        if signal.action != OPEN_ACTION {
            return false;
        }
        let Some(opener) = &self.opener else { return false };
        let created = self.state.lock().unwrap().by_id.get(&signal.id).cloned();
        let Some((card, owner_then)) = created else { return false };
        let now = self.backend.owner().await;
        if now.is_none() || now != owner_then || now.as_deref() != Some(signal.sender.as_str()) {
            tracing::warn!("notification action from an unexpected sender ignored");
            return false;
        }
        opener.open(&card);
        true
    }

    /// Runs until the view channel closes.
    pub async fn run(
        self: Arc<Self>,
        mut views: watch::Receiver<InboxView>,
        mut prefs: watch::Receiver<Prefs>,
        mut panel: watch::Receiver<bool>,
        mut actions: mpsc::Receiver<ActionSignal>,
    ) {
        loop {
            let view = views.borrow_and_update().clone();
            let enabled = prefs.borrow_and_update().notifications;
            let open = *panel.borrow_and_update();
            self.apply(&view, enabled, open).await;
            tokio::select! {
                r = views.changed() => if r.is_err() { return },
                r = prefs.changed() => if r.is_err() { return },
                r = panel.changed() => if r.is_err() { return },
                Some(signal) = actions.recv() => { self.on_action(&signal).await; }
            }
        }
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p acd-inbox --locked --test notify`
Expected: PASS.

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy --workspace --all-targets --locked -- -D warnings`
Expected: no warnings.

```bash
git add Cargo.lock crates/acd-inbox
git commit -m "acd-inbox: notification reconciliation, trusted open action and argv opener"
```

---
### Task 14: Linux backends: Secret Service and D-Bus notifications

**Execution:** cloud-ok, in a Linux session. The modules compile only on Linux; on macOS this task can only confirm that the workspace still builds without them. The live behaviour is checked in Task 22.

**Files:**
- Create: `crates/acd-inbox/src/linux/mod.rs`, `crates/acd-inbox/src/linux/keyring.rs`, `crates/acd-inbox/src/linux/dbus.rs`
- Modify: `crates/acd-inbox/src/lib.rs` (`#[cfg(target_os = "linux")] pub mod linux;`), `crates/acd-inbox/Cargo.toml`, `Cargo.lock`

**Interfaces:**
- Consumes: Task 11's `secrets::{SecretBackend, BackendError}`; Task 13's `notify::{NotifyBackend, NotifyError, ActionSignal, OPEN_ACTION}`.
- Produces (in `acd_inbox::linux::keyring`):
  - `pub struct SecretServiceBackend`, with `new() -> Self`; it implements `SecretBackend`.
  - `pub fn attributes(key: &str) -> HashMap<&str, &str>` and `pub fn map_error(e: &secret_service::Error) -> BackendError`.
  - `pub fn needs_prompt(prompt: &str) -> bool`. Writes and deletes dismiss a returned prompt and report `Locked` (D13).
- Produces (in `acd_inbox::linux::dbus`):
  - `pub struct DbusNotify`, with `connect() -> zbus::Result<(DbusNotify, mpsc::Receiver<ActionSignal>)>` (async); it implements `NotifyBackend`.
  - `pub fn actions(open: bool) -> Vec<&'static str>` and `pub fn error_class(e: &zbus::Error) -> &'static str`.

- [ ] **Step 1: Add the dependencies**

In `crates/acd-inbox/Cargo.toml` (D26):

```toml
[target.'cfg(target_os = "linux")'.dependencies]
futures-util.workspace = true
secret-service = { version = "5.2.0", features = ["rt-tokio-crypto-rust"] }
zbus = { version = "5.19.0", default-features = false, features = ["tokio"] }
```

Run: `cargo check -p acd-inbox` (no `--locked`: this resolves and records secret-service 5.2.0 and zbus 5.19.0 in `Cargo.lock`). `Cargo.lock` lists target-specific packages on every host, so the lock written here also works for `--locked` on macOS.

Run: `grep -A1 -E '^name = "(zbus|secret-service)"$' Cargo.lock`
Expected: `version = "5.19.0"` for zbus and `version = "5.2.0"` for secret-service. If cargo picked a newer patch release, pin it with `cargo update -p zbus --precise 5.19.0` and `cargo update -p secret-service --precise 5.2.0`.

Run: `cargo tree -p acd-inbox -e features -i zbus | grep -c async-io`
Expected: `0`. zbus must not pull in async-io.

- [ ] **Step 2: Write the failing tests**

`crates/acd-inbox/src/linux/mod.rs`:

```rust
//! Linux-only backends: the Secret Service (omarchy spec §9) and D-Bus
//! notifications (§8). The logic they plug into is tested on every platform
//! with fakes.
pub mod dbus;
pub mod keyring;
```

At the end of `keyring.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_are_found_by_service_and_machine() {
        let a = attributes("11111111-1111-4111-8111-111111111111");
        assert_eq!(a.get("service"), Some(&"agent-caller"));
        assert_eq!(a.get("machine"), Some(&"11111111-1111-4111-8111-111111111111"));
        assert_eq!(a.len(), 2);
    }

    #[test]
    fn only_the_no_prompt_path_lets_a_change_through() {
        assert!(!needs_prompt("/"));
        assert!(needs_prompt("/org/freedesktop/secrets/prompt/u1"));
    }

    #[test]
    fn errors_map_to_classes() {
        assert_eq!(map_error(&secret_service::Error::Locked), BackendError::Locked);
        assert_eq!(map_error(&secret_service::Error::Prompt), BackendError::Dismissed);
        assert_eq!(map_error(&secret_service::Error::Unavailable), BackendError::Unavailable);
        assert_eq!(map_error(&secret_service::Error::PromptDisconnected), BackendError::Unavailable);
        assert_eq!(map_error(&secret_service::Error::NoResult), BackendError::Failed);
    }

    /// Needs a desktop session bus with an unlocked keyring and a Rust
    /// toolchain (omarchy has none; Task 22 check 4 covers the keyring
    /// there): `cargo test -p acd-inbox --locked --lib -- --ignored keyring_round_trip`
    #[tokio::test]
    #[ignore = "touches the user's keyring"]
    async fn keyring_round_trip() {
        let mut b = SecretServiceBackend::new();
        let key = "acd-inbox-test-00000000-0000-4000-8000-000000000000";
        b.write(key, "value").await.unwrap();
        assert_eq!(b.read(key).await.unwrap().as_deref(), Some("value"));
        b.delete(key).await.unwrap();
        assert_eq!(b.read(key).await.unwrap(), None);
    }
}
```

At the end of `dbus.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_open_action_is_offered_only_with_omarchy_shell() {
        assert_eq!(actions(true), [OPEN_ACTION, "Open"]);
        assert!(actions(false).is_empty());
    }

    #[test]
    fn errors_are_logged_by_class() {
        assert_eq!(error_class(&zbus::Error::InterfaceNotFound), "other");
        let io = zbus::Error::InputOutput(std::sync::Arc::new(std::io::Error::other("SECRET")));
        assert_eq!(error_class(&io), "io");
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Add `#[cfg(target_os = "linux")] pub mod linux;` to `lib.rs`.
Run: `cargo test -p acd-inbox --locked --lib linux`
Expected: compile errors, `cannot find function attributes`, and others.

- [ ] **Step 4: Implement `keyring.rs`**

Above its tests:

```rust
//! Remote secrets in the Secret Service (omarchy spec §9): items in the
//! default collection with attributes `service=agent-caller`,
//! `machine=<key>`. Reads check the collection and item lock first.
//! Writes and deletes call `CreateItem` and `Item.Delete` directly and
//! dismiss any prompt the service returns (reported `Locked`). Only
//! `unlock` runs a prompt (plan D13).
use std::collections::HashMap;

use secret_service::{EncryptionType, Error, SecretService};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::secrets::{BackendError, SecretBackend};

pub const SERVICE_ATTRIBUTE: &str = "agent-caller";
const CONTENT_TYPE: &str = "text/plain";

pub fn attributes(key: &str) -> HashMap<&str, &str> {
    HashMap::from([("service", SERVICE_ATTRIBUTE), ("machine", key)])
}

/// A class for logs and replies; the D-Bus message is never kept.
pub fn map_error(e: &Error) -> BackendError {
    match e {
        Error::Locked => BackendError::Locked,
        Error::Prompt => BackendError::Dismissed,
        Error::Unavailable | Error::PromptDisconnected => BackendError::Unavailable,
        _ => BackendError::Failed,
    }
}

fn class(e: Error) -> BackendError {
    map_error(&e)
}

#[derive(Default)]
pub struct SecretServiceBackend {
    ss: Option<SecretService<'static>>,
}

impl SecretServiceBackend {
    pub fn new() -> Self {
        Self::default()
    }

    async fn service(&mut self) -> Result<&SecretService<'static>, BackendError> {
        if self.ss.is_none() {
            self.ss = Some(SecretService::connect(EncryptionType::Dh).await.map_err(class)?);
        }
        Ok(self.ss.as_ref().expect("connected above"))
    }

    /// Drops a broken connection so the next call reconnects.
    fn settle<T>(&mut self, r: Result<T, BackendError>) -> Result<T, BackendError> {
        if matches!(r, Err(BackendError::Unavailable | BackendError::Failed)) {
            self.ss = None;
        }
        r
    }
}

async fn read_in(ss: &SecretService<'_>, key: &str) -> Result<Option<String>, BackendError> {
    let col = ss.get_default_collection().await.map_err(class)?;
    if col.is_locked().await.map_err(class)? {
        return Err(BackendError::Locked);
    }
    let items = col.search_items(attributes(key)).await.map_err(class)?;
    let Some(item) = items.first() else { return Ok(None) };
    if item.is_locked().await.map_err(class)? {
        return Err(BackendError::Locked);
    }
    let bytes = item.get_secret().await.map_err(class)?;
    String::from_utf8(bytes).map(Some).map_err(|_| BackendError::Failed)
}

const SECRETS: &str = "org.freedesktop.secrets";

/// Whether a Secret Service reply asks for a prompt: any path but "/".
pub fn needs_prompt(prompt: &str) -> bool {
    prompt != "/"
}

fn bus(e: zbus::Error) -> BackendError {
    match e {
        zbus::Error::InputOutput(_) | zbus::Error::Connection(..) => BackendError::Unavailable,
        _ => BackendError::Failed,
    }
}

/// A returned prompt is never run: it is dismissed, and the change is
/// reported as `Locked` (plan D13). The user unlocks from the panel.
async fn refuse_prompt(conn: &zbus::Connection, prompt: &OwnedObjectPath) -> Result<(), BackendError> {
    if !needs_prompt(prompt.as_str()) {
        return Ok(());
    }
    let _ = conn
        .call_method(Some(SECRETS), prompt.as_str(), Some("org.freedesktop.Secret.Prompt"), "Dismiss", &())
        .await;
    Err(BackendError::Locked)
}

/// `CreateItem` called directly, so its prompt can be refused; the crate's
/// `create_item` would run it. The secret travels in a "plain" session
/// (plan D38): secret-service 5.2 keeps its encrypted session private.
async fn write_in(ss: &SecretService<'_>, key: &str, value: &str) -> Result<(), BackendError> {
    let col = ss.get_default_collection().await.map_err(class)?;
    if col.is_locked().await.map_err(class)? {
        return Err(BackendError::Locked);
    }
    let conn = zbus::Connection::session().await.map_err(bus)?;
    let reply = conn
        .call_method(
            Some(SECRETS),
            "/org/freedesktop/secrets",
            Some("org.freedesktop.Secret.Service"),
            "OpenSession",
            &("plain", Value::from("")),
        )
        .await
        .map_err(bus)?;
    let (_, session): (OwnedValue, OwnedObjectPath) = reply.body().deserialize().map_err(|_| BackendError::Failed)?;
    let mut props: HashMap<&str, Value<'_>> = HashMap::new();
    props.insert("org.freedesktop.Secret.Item.Label", Value::from(format!("agent-caller machine {key}")));
    props.insert("org.freedesktop.Secret.Item.Attributes", Value::from(attributes(key)));
    let secret = (session.clone(), Vec::<u8>::new(), value.as_bytes().to_vec(), CONTENT_TYPE);
    let result = conn
        .call_method(
            Some(SECRETS),
            col.collection_path.as_str(),
            Some("org.freedesktop.Secret.Collection"),
            "CreateItem",
            &(props, secret, true),
        )
        .await;
    let _ = conn
        .call_method(Some(SECRETS), session.as_str(), Some("org.freedesktop.Secret.Session"), "Close", &())
        .await;
    let (_item, prompt): (OwnedObjectPath, OwnedObjectPath) =
        result.map_err(bus)?.body().deserialize().map_err(|_| BackendError::Failed)?;
    refuse_prompt(&conn, &prompt).await
}

/// `Item.Delete` called directly, so its prompt can be refused.
async fn delete_in(ss: &SecretService<'_>, key: &str) -> Result<(), BackendError> {
    let col = ss.get_default_collection().await.map_err(class)?;
    if col.is_locked().await.map_err(class)? {
        return Err(BackendError::Locked);
    }
    let conn = zbus::Connection::session().await.map_err(bus)?;
    for item in col.search_items(attributes(key)).await.map_err(class)? {
        let reply = conn
            .call_method(Some(SECRETS), item.item_path.as_str(), Some("org.freedesktop.Secret.Item"), "Delete", &())
            .await
            .map_err(bus)?;
        let prompt: OwnedObjectPath = reply.body().deserialize().map_err(|_| BackendError::Failed)?;
        refuse_prompt(&conn, &prompt).await?;
    }
    Ok(())
}

/// The only call that may prompt (§9). secret-service 5.2 has no way to
/// dismiss its prompt: when the worker's 120 s limit drops this future, the
/// prompt stays on screen until the user answers it. A late "unlock" then
/// unlocks the collection and the next `refresh` or `retry_machine` reads
/// the secrets; a late dismiss changes nothing.
async fn unlock_in(ss: &SecretService<'_>) -> Result<(), BackendError> {
    let col = ss.get_default_collection().await.map_err(class)?;
    if col.is_locked().await.map_err(class)? {
        col.unlock().await.map_err(class)?;
    }
    Ok(())
}

#[async_trait::async_trait]
impl SecretBackend for SecretServiceBackend {
    async fn read(&mut self, key: &str) -> Result<Option<String>, BackendError> {
        let r = match self.service().await {
            Ok(ss) => read_in(ss, key).await,
            Err(e) => Err(e),
        };
        self.settle(r)
    }

    async fn write(&mut self, key: &str, value: &str) -> Result<(), BackendError> {
        let r = match self.service().await {
            Ok(ss) => write_in(ss, key, value).await,
            Err(e) => Err(e),
        };
        self.settle(r)
    }

    async fn delete(&mut self, key: &str) -> Result<(), BackendError> {
        let r = match self.service().await {
            Ok(ss) => delete_in(ss, key).await,
            Err(e) => Err(e),
        };
        self.settle(r)
    }

    async fn unlock(&mut self) -> Result<(), BackendError> {
        let r = match self.service().await {
            Ok(ss) => unlock_in(ss).await,
            Err(e) => Err(e),
        };
        self.settle(r)
    }
}
```

- [ ] **Step 5: Implement `dbus.rs`**

Above its tests:

```rust
//! Notifications through `org.freedesktop.Notifications` (omarchy spec §8).
use std::collections::HashMap;

use futures_util::StreamExt;
use tokio::sync::mpsc;
use zbus::{Connection, names::BusName, proxy, zvariant::Value};

use crate::notify::{ActionSignal, NotifyBackend, NotifyError, OPEN_ACTION};

pub const NOTIFICATIONS: &str = "org.freedesktop.Notifications";
pub const APP_NAME: &str = "Agent Caller";

#[proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
trait Notifications {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: HashMap<&str, Value<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;

    fn close_notification(&self, id: u32) -> zbus::Result<()>;

    #[zbus(signal)]
    fn action_invoked(&self, id: u32, action_key: String) -> zbus::Result<()>;
}

/// The actions array: "Open" only when omarchy-shell was found (§4.2).
pub fn actions(open: bool) -> Vec<&'static str> {
    if open { vec![OPEN_ACTION, "Open"] } else { vec![] }
}

/// A class for logs; never the D-Bus message.
pub fn error_class(e: &zbus::Error) -> &'static str {
    match e {
        zbus::Error::MethodError(..) => "method_error",
        zbus::Error::FDO(_) => "fdo",
        zbus::Error::InputOutput(_) | zbus::Error::Connection(..) => "io",
        _ => "other",
    }
}

pub struct DbusNotify {
    conn: Connection,
    proxy: NotificationsProxy<'static>,
}

impl DbusNotify {
    /// Connects to the session bus and forwards every `ActionInvoked`
    /// signal with its sender; `Notifier::on_action` decides what to trust.
    pub async fn connect() -> zbus::Result<(DbusNotify, mpsc::Receiver<ActionSignal>)> {
        let conn = Connection::session().await?;
        let proxy = NotificationsProxy::new(&conn).await?;
        let mut stream = proxy.receive_action_invoked().await?;
        let (tx, rx) = mpsc::channel(16);
        tokio::spawn(async move {
            while let Some(signal) = stream.next().await {
                let sender = signal.message().header().sender().map(|s| s.to_string()).unwrap_or_default();
                let Ok(args) = signal.args() else { continue };
                let action = ActionSignal { id: args.id, action: args.action_key.clone(), sender };
                if tx.send(action).await.is_err() {
                    return;
                }
            }
        });
        Ok((DbusNotify { conn, proxy }, rx))
    }
}

#[async_trait::async_trait]
impl NotifyBackend for DbusNotify {
    async fn show(&self, summary: &str, body: &str, open_action: bool) -> Result<u32, NotifyError> {
        self.proxy
            .notify(APP_NAME, 0, "", summary, body, &actions(open_action), HashMap::new(), -1)
            .await
            .map_err(|e| {
                tracing::warn!(class = error_class(&e), "notify failed");
                NotifyError::Unavailable
            })
    }

    async fn close(&self, id: u32) {
        if let Err(e) = self.proxy.close_notification(id).await {
            tracing::debug!(class = error_class(&e), "close notification failed");
        }
    }

    async fn owner(&self) -> Option<String> {
        let dbus = zbus::fdo::DBusProxy::new(&self.conn).await.ok()?;
        let name = BusName::try_from(NOTIFICATIONS).ok()?;
        dbus.get_name_owner(name).await.ok().map(|o| o.to_string())
    }
}
```

- [ ] **Step 6: Run the tests to verify they pass**

Run (Linux): `cargo test -p acd-inbox --locked --lib linux`
Expected: PASS, with 1 ignored (`keyring_round_trip`).

Run (Linux): `cargo test --workspace --locked`
Expected: PASS.

On macOS, `cargo build -p acd-inbox --locked` must still pass: the module is gated.

- [ ] **Step 7: Lint and commit**

Run: `cargo clippy --workspace --all-targets --locked -- -D warnings`
Expected: no warnings (Linux and macOS).

```bash
git add Cargo.lock crates/acd-inbox
git commit -m "acd-inbox: Secret Service and D-Bus notification backends (Linux)"
```

---

### Task 15: `acd-inbox serve` and safe logging

**Execution:** cloud-ok

**Files:**
- Create: `crates/acd-inbox/src/serve.rs`, `crates/acd-inbox/src/main.rs`, `crates/acd-inbox/tests/serve.rs`, `crates/acd-inbox/tests/logs.rs`
- Modify: `crates/acd-inbox/src/lib.rs` (`pub mod serve;`), `crates/acd-inbox/Cargo.toml` (`anyhow`, `clap`, `tracing-subscriber`), `Cargo.lock`

**Interfaces:**
- Consumes:
  - Task 5: `prefs::{Paths, Prefs, init_settings, local_name}`.
  - Task 6: `startup::{listen, Listening, OwnedSocket, LockFile}`.
  - Tasks 8 and 9: `server::{Service, ServerOptions, SecretControl}`.
  - Tasks 11 and 12: `secrets::{SecretWorker, WorkerStore, WorkerTimeouts}`.
  - Task 13: `notify::{Notifier, ShellOpener, Opener}`.
  - Task 14: `linux::{keyring::SecretServiceBackend, dbus::DbusNotify}`.
- Produces (in `acd_inbox::serve`):
  - `pub struct Running { pub service: Arc<Service>, pub inbox: Arc<Inbox>, pub server: JoinHandle<()>, pub socket: OwnedSocket, pub lock: LockFile }`.
  - `pub async fn start_service(paths: &Paths, store: Arc<dyn SecretStore>, control: Arc<dyn SecretControl>, timing: Timing, local_name: &str, prepare: impl FnOnce(Vec<String>) + Send + 'static) -> anyhow::Result<Running>`. It runs §4.4, loads `settings.json`, runs `prepare(remote keys)` on the blocking pool (the Linux entry point prefills the `WorkerStore` there), then starts the inbox and the server.
  - `pub fn remote_keys(settings: &Settings) -> Vec<String>`.
  - Logging (D33):
    - `pub fn init_logging_to<W>(env: Option<&str>, writer: W) -> Result<(), TryInitError>`, where `W: for<'a> MakeWriter<'a> + Send + Sync + 'static`. It installs the global subscriber and the `log` bridge.
    - `env` (`RUST_LOG`) sets levels, with `info` as the default.
    - A fixed deny-list, ANDed after `env` as a second filter layer, caps the dependencies whatever `env` says: `tungstenite` and `tokio_tungstenite` off; `rustls`, `tokio_rustls`, `hyper`, `hyper_util`, `h2`, `axum`, `zbus`, `zvariant`, `secret_service`, `mio` and `tokio` at most `warn`.
    - acd-client logs no agent text at any level (its only `tracing` calls are `conn.rs:392`, an error code, and `conn.rs:577`, a static reason).
  - `pub fn init_logging()`: `init_logging_to(RUST_LOG, stderr)`, which journald collects. The capture test calls the same function.
  - `pub fn run() -> anyhow::Result<()>`: the `serve` command (D25).
- Produces: the binary `acd-inbox` with subcommand `serve`. Task 20 adds `install`, `uninstall` and `status`.

- [ ] **Step 1: Write the failing tests**

`crates/acd-inbox/tests/serve.rs`:

```rust
mod common;

use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};

use acd_client::settings::MemorySecrets;
use acd_inbox::{prefs::Paths, serve::start_service, server::NoKeyring};
use common::*;

#[tokio::test]
async fn start_service_listens_privately_and_names_the_local_machine() {
    let acd = tempfile::tempdir().unwrap();
    // SAFETY: set before any other thread of this test binary reads the environment.
    unsafe { std::env::set_var("ACD_CONFIG_DIR", acd.path()) };
    let config = tempfile::tempdir().unwrap();
    let run = tempfile::tempdir().unwrap();
    let runtime = std::fs::canonicalize(run.path()).unwrap();
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
    let paths = Paths::new(config.path(), &runtime);
    let prepared = Arc::new(Mutex::new(None));
    let p = prepared.clone();
    let running = start_service(&paths, Arc::new(MemorySecrets::default()), Arc::new(NoKeyring), fast(), "omarchy", move |keys| {
        *p.lock().unwrap() = Some(keys);
    })
    .await
    .unwrap();
    assert_eq!(*prepared.lock().unwrap(), Some(vec![]), "a fresh install has no remote machines");
    assert_eq!(std::fs::metadata(&paths.socket).unwrap().permissions().mode() & 0o777, 0o600);
    let mut v = View::connect(&paths.socket).await;
    let (_, settings, _) = v.greeting().await;
    assert_eq!(settings["machines"][0]["name"], "omarchy");
    let second = start_service(&paths, Arc::new(MemorySecrets::default()), Arc::new(NoKeyring), fast(), "omarchy", |_| {}).await;
    assert!(second.err().unwrap().to_string().contains("another acd-inbox is running"));
    running.server.abort();
    drop(running);
    assert!(std::fs::symlink_metadata(&paths.socket).is_err(), "the socket goes with the service");
}
```

`crates/acd-inbox/tests/logs.rs`:

```rust
mod common;

use std::sync::{Arc, Mutex};

use common::*;
use serde_json::json;

#[derive(Clone, Default)]
struct Buffer(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Spec §4.1 and CLAUDE.md: agent text, typed text, pairing strings, tokens
/// and raw socket lines never reach the logs. It installs the production
/// logging (`init_logging_to`, with the `log` bridge) once, at `trace`, so
/// `RUST_LOG` is shown unable to open the dependencies; this file holds
/// one test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_text_secrets_and_raw_lines_never_reach_the_logs() {
    let buffer = Buffer::default();
    let writer = buffer.clone();
    acd_inbox::serve::init_logging_to(Some("trace"), move || writer.clone()).unwrap();
    // What the dependencies would print at trace through the `log` bridge
    // and through tracing: frames carry tokens and agent text.
    log::trace!(target: "tungstenite::protocol", "SECRET-FRAME {TOKEN}");
    log::warn!(target: "tokio_tungstenite::compat", "SECRET-WARN");
    tracing::debug!(target: "rustls::conn", "SECRET-TLS");
    tracing::trace!(target: "zbus::connection", "SECRET-DBUS");

    let mut agent = pending_agent("a1", "r1");
    agent["title"] = "SECRET-TITLE".into();
    agent["pending"]["detail"] = "SECRET-DETAIL".into();
    agent["pending"]["raw_screen"] = "SECRET-SCREEN".into();
    let rig = rig(vec![agent], held()).await;
    let mut v = View::connect(&rig.socket).await;
    v.greeting().await;
    v.cmd(json!({"type":"reply","card":"m1/a1","text":"SECRET-REPLY"})).await;
    v.cmd(json!({"type":"answer_text","card":"m1/a1","request":"r1","revision":1,"text":"SECRET-ANSWER"})).await;
    v.cmd(json!({"type":"escape"})).await;
    v.cmd(json!({"type":"pair","pairing":format!("acd://h:1?pin=SECRET-PIN#{TOKEN}")})).await;
    let good = format!("acd://localhost:{}?pin={}#{}", rig.f.port(), rig.f.pin(), TOKEN);
    assert_eq!(v.cmd(json!({"type":"pair","pairing":good})).await["type"], "ok");
    v.cmd(json!({"type":"SECRET-TYPE"})).await;
    v.send_raw(b"{\"type\":\"pick\",\"id\":\"SECRET-ID-0123456789012345678901234567890123456789012345678901234567890123\"}\n").await;
    v.closed().await;
    let mut w = View::connect(&rig.socket).await;
    w.greeting().await;
    w.send_raw(b"SECRET-NOT-JSON\n").await;
    w.closed().await;
    let logs = || String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
    until("both close lines", || logs().matches("connection closed").count() >= 2).await;

    let logs = logs();
    assert!(logs.contains("connection accepted"), "the capture works");
    assert!(logs.contains("line rejected"), "rejections are logged by tag");
    assert!(logs.contains("command"), "trace is on for acd-inbox itself");
    assert!(!logs.contains("SECRET"), "agent, user or dependency text reached the logs");
    assert!(!logs.contains(TOKEN), "a token reached the logs");
    assert!(!logs.contains(&rig.f.pin()), "a pin reached the logs");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Add `pub mod serve;` to `lib.rs`, and to `[dependencies]`: `anyhow.workspace = true`, `clap.workspace = true`, `tracing-subscriber.workspace = true`; to `[dev-dependencies]`: `log = "0.4.34"` (already in `Cargo.lock` through rustls).
Run: `cargo test -p acd-inbox --test serve --test logs` (updates `Cargo.lock`)
Expected: compile error, `cannot find function start_service`.

- [ ] **Step 3: Implement `serve.rs`**

```rust
//! `acd-inbox serve`: startup wiring (omarchy spec §4) and logging.
use std::sync::Arc;

use acd_client::{
    Timing,
    inbox::{Inbox, InboxOptions},
    settings::{EndpointConfig, SecretStore, Settings},
};
use anyhow::{Context, Result};
use tracing_subscriber::filter::{EnvFilter, LevelFilter, Targets};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::{SubscriberInitExt, TryInitError};

use crate::prefs::{self, Paths, Prefs};
use crate::server::{SecretControl, ServerOptions, Service};
use crate::startup::{self, LockFile, OwnedSocket};

/// A started service. Dropping it removes the socket (the server task keeps
/// the listener until it is aborted or the process exits).
pub struct Running {
    pub service: Arc<Service>,
    pub inbox: Arc<Inbox>,
    pub server: tokio::task::JoinHandle<()>,
    pub socket: OwnedSocket,
    pub lock: LockFile,
}

pub fn remote_keys(settings: &Settings) -> Vec<String> {
    settings
        .machines
        .iter()
        .filter(|m| matches!(m.endpoint, EndpointConfig::Tcp { .. }))
        .map(|m| m.key.clone())
        .collect()
}

/// §4.4 startup, then settings, `prepare(remote keys)` on the blocking pool,
/// the inbox and the server. Call inside a tokio runtime.
pub async fn start_service(
    paths: &Paths,
    store: Arc<dyn SecretStore>,
    control: Arc<dyn SecretControl>,
    timing: Timing,
    local_name: &str,
    prepare: impl FnOnce(Vec<String>) + Send + 'static,
) -> Result<Running> {
    let startup::Listening { listener, socket, lock } = startup::listen(&paths.runtime_dir, &paths.socket, &paths.lock)?;
    let settings = prefs::init_settings(&paths.settings, local_name).context("loading settings.json")?;
    let keys = remote_keys(&settings);
    tokio::task::spawn_blocking(move || prepare(keys)).await?;
    let prefs = Prefs::load(&paths.service);
    let inbox = Inbox::new(
        settings,
        store,
        InboxOptions {
            timing,
            settings_path: Some(paths.settings.clone()),
            client_name: "acd-inbox".into(),
            client_version: env!("CARGO_PKG_VERSION").into(),
        },
    );
    inbox.start();
    let service = Service::new(inbox.clone(), prefs, Some(paths.service.clone()), control, ServerOptions::default());
    let server = service.spawn(listener);
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "acd-inbox serving");
    Ok(Running { service, inbox, server, socket, lock })
}

/// Dependencies whose logs can carry frames, tokens or agent text, capped
/// whatever `RUST_LOG` says (plan D33).
const DENY: &[(&str, LevelFilter)] = &[
    ("tungstenite", LevelFilter::OFF),
    ("tokio_tungstenite", LevelFilter::OFF),
    ("rustls", LevelFilter::WARN),
    ("tokio_rustls", LevelFilter::WARN),
    ("hyper", LevelFilter::WARN),
    ("hyper_util", LevelFilter::WARN),
    ("h2", LevelFilter::WARN),
    ("axum", LevelFilter::WARN),
    ("zbus", LevelFilter::WARN),
    ("zvariant", LevelFilter::WARN),
    ("secret_service", LevelFilter::WARN),
    ("mio", LevelFilter::WARN),
    ("tokio", LevelFilter::WARN),
];

/// The deny-list as a filter that passes everything else.
pub fn deny_list() -> Targets {
    DENY.iter().fold(Targets::new().with_default(LevelFilter::TRACE), |t, (target, level)| t.with_target(*target, *level))
}

/// The production logging: `env` (`RUST_LOG`, default `info`) AND the
/// deny-list, both as filter layers, so an event must pass both; plus the
/// `log` bridge, which `try_init` installs.
pub fn init_logging_to<W>(env: Option<&str>, writer: W) -> Result<(), TryInitError>
where
    W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
    let env = env.and_then(|e| EnvFilter::try_new(e).ok()).unwrap_or_else(|| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(env)
        .with(deny_list())
        .with(tracing_subscriber::fmt::layer().with_ansi(false).with_target(false).with_writer(writer))
        .try_init()
}

/// Logs to stderr (journald for the unit).
pub fn init_logging() {
    let env = std::env::var("RUST_LOG").ok();
    if let Err(e) = init_logging_to(env.as_deref(), std::io::stderr) {
        eprintln!("acd-inbox: logging is already set up: {e}");
    }
}

/// `acd-inbox serve` (plan D25).
pub fn run() -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        linux_run()
    }
    #[cfg(not(target_os = "linux"))]
    {
        anyhow::bail!("acd-inbox serve runs on Linux only")
    }
}

#[cfg(target_os = "linux")]
fn linux_run() -> Result<()> {
    use crate::linux::{dbus::DbusNotify, keyring::SecretServiceBackend};
    use crate::notify::{Notifier, Opener, ShellOpener};
    use crate::secrets::{SecretWorker, WorkerStore, WorkerTimeouts};

    init_logging();
    let paths = Paths::from_env()?;
    let local = prefs::local_name(&gethostname::gethostname().to_string_lossy());
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async move {
        let store = Arc::new(WorkerStore::new(SecretWorker::spawn(SecretServiceBackend::new(), WorkerTimeouts::LIVE)));
        let prefill = store.clone();
        let running = start_service(&paths, store.clone(), store, Timing::default(), &local, move |keys| {
            prefill.prefill(&keys)
        })
        .await?;
        match DbusNotify::connect().await {
            Ok((backend, actions)) => {
                let opener = ShellOpener::find(std::env::var_os("PATH").as_deref(), std::path::Path::new("/usr/bin"))
                    .map(|o| Arc::new(o) as Arc<dyn Opener>);
                if opener.is_none() {
                    tracing::info!("omarchy-shell not found; notifications have no Open action");
                }
                let notifier = Notifier::new(Arc::new(backend), opener);
                tokio::spawn(notifier.run(
                    running.inbox.view(),
                    running.service.prefs(),
                    running.service.panel_open(),
                    actions,
                ));
            }
            Err(e) => tracing::warn!(
                class = crate::linux::dbus::error_class(&e),
                "no notification server; notifications are off for this session"
            ),
        }
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            _ = term.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
        tracing::info!("acd-inbox stopping");
        running.server.abort();
        drop(running);
        anyhow::Ok(())
    })
}
```

`crates/acd-inbox/src/main.rs`:

```rust
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "acd-inbox", version, about = "The agent-caller inbox service for Omarchy")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the service (the systemd user unit starts this).
    Serve,
}

fn main() -> ExitCode {
    let result = match Cli::parse().command {
        Command::Serve => acd_inbox::serve::run(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("acd-inbox: {e:#}");
            ExitCode::FAILURE
        }
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p acd-inbox --locked --test serve --test logs`
Expected: PASS.

Run: `cargo run -p acd-inbox --locked -- --help`
Expected: usage lists `serve`.

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy --workspace --all-targets --locked -- -D warnings`
Expected: no warnings.

```bash
git add Cargo.lock crates/acd-inbox
git commit -m "acd-inbox: serve wiring and logging that RUST_LOG cannot open for dependencies"
```

---
### Task 16: Omarchy shell API discovery

**Execution:** local. It runs on the omarchy machine with the user. It can run any time before Task 17, in parallel with Tasks 1 to 15; Tasks 17 to 21 depend on its findings.

**Files:**
- Create: `docs/research/2026-10-12-omarchy-plugin-api.md`
- Create (scratch, not committed): a probe plugin under `~/.config/omarchy/plugins/tonibergholm.probe/` on omarchy

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: a research note that records, with file paths and quoted lines, every installed-shell fact the plugin relies on. Tasks 17 to 21 use the recorded names as written. Where a fact differs from this plan's assumption (listed in Step 3), the note says what to change, and the controller amends Tasks 17 to 21 before they start.

- [ ] **Step 1: Find the shell, the CLI and a reference plugin**

On omarchy (`ssh omarchy`):

```bash
omarchy-shell --help; omarchy plugin --help
command -v omarchy-plugin-validate qmllint qmllint6 qs quickshell
qs --version 2>/dev/null || quickshell --version
find / -name manifest.json -path '*omarchy*' 2>/dev/null | xargs grep -l '"omarchy.tailscale"'
ls ~/.config/hypr/
busctl --user call org.freedesktop.Notifications /org/freedesktop/Notifications org.freedesktop.Notifications GetCapabilities
busctl --user status org.freedesktop.Notifications | grep -E '^(Comm|Exe)='
```

Read the `manifest.json` and every `.qml` file of `omarchy.tailscale`.

- [ ] **Step 2: Run a probe plugin**

Install this probe as plugin `tonibergholm.probe`, with a manifest copied from `omarchy.tailscale` and the id changed. It prints only to its own log; it holds no secrets.

`Probe.qml`:

```qml
import QtQuick
import Quickshell
import Quickshell.Io
import "probe.mjs" as P

Item {
    Component.onCompleted: {
        console.log("probe: mjs import", P.answer());
        console.log("probe: TextEncoder", typeof TextEncoder, "TextDecoder", typeof TextDecoder);
        console.log("probe: XDG_RUNTIME_DIR", Quickshell.env("XDG_RUNTIME_DIR") !== "");
    }

    Socket {
        id: probe
        path: Quickshell.env("XDG_RUNTIME_DIR") + "/acd-inbox-probe.sock"
        connected: true
        onConnectedChanged: console.log("probe: connected", connected)
        onError: e => console.log("probe: error", e)
        parser: SplitParser {
            splitMarker: "\n"
            onRead: data => console.log("probe: line", JSON.stringify(data), data.length)
        }
    }
}
```

`probe.mjs`:

```js
export function answer() {
  return 42;
}
```

Serve the probe socket from omarchy with a line that ends in a split 4-byte character, written in two chunks:

```bash
python3 - <<'PY'
import os, socket, time
path = os.path.join(os.environ["XDG_RUNTIME_DIR"], "acd-inbox-probe.sock")
if os.path.exists(path): os.remove(path)
s = socket.socket(socket.AF_UNIX); s.bind(path); s.listen(1)
c, _ = s.accept()
line = '{"t":"é😀"}\n'.encode()
c.sendall(line[:9]); time.sleep(0.5); c.sendall(line[9:] + b'{"t":"two"}\n')
time.sleep(1); c.close()
PY
```

Read the probe's output in the shell's log (`journalctl --user -b | grep 'probe:'`, or the path `omarchy-shell` reports).

- [ ] **Step 3: Record the findings**

Write `docs/research/2026-10-12-omarchy-plugin-api.md`. For each item, give the fact, the quoted source line, and the plan's assumption with "holds" or "differs: <what to change>".

| # | Question | Plan's assumption (Tasks 17 to 21) |
|---|---|---|
| 1 | Manifest keys; how a bar widget and its panel are declared | `id`, `name`, `version`, `description`, `author`, `category`, `kinds: ["bar-widget"]`, `defaultSection`, `barWidget: "BarWidget.qml"` |
| 2 | Imports and names: `qs.Ui` `Panel` (open, close, visibility API), `qs.Commons` `Color` (urgent, accent, dim, text, surface, surfaceActive) and `Style` (spacing, spacingSmall, radius) | as named, qualified imports `qs.Ui as Ui`, `qs.Commons as Commons` |
| 3 | IPC: `ShellIpc` or Quickshell `IpcHandler`; how a target and functions are declared; how `omarchy-shell <target> <fn> <arg>` maps | `IpcHandler { target: "tonibergholm.agentcaller"; function openCard(key: string): void }` |
| 4 | `.mjs` import from a plugin | works (`import "model.mjs" as Model`) |
| 5 | JS host globals: `TextEncoder`, `TextDecoder` | not relied on: `model.mjs` uses neither |
| 6 | `Socket`: `connected`, `connectedChanged`, `error`, `write`, `flush`; reconnect by toggling `connected` | as named |
| 7 | `SplitParser`: splits bytes before decoding (the split `😀` arrives whole); one `read` per line, newline stripped | holds; the plugin does no byte handling (D20) |
| 8 | Key events: `Keys.onPressed` on a `TextField` sees letters before the field; `inputMethodComposing` is available | as used in Task 19 |
| 9 | Notification server: name, and whether `GetCapabilities` lists `actions` | lists `actions` |
| 10 | Plugin lifecycle: `omarchy-shell shell rescanPlugins`, `omarchy plugin enable <id>` and `disable <id>`, placement by `defaultSection` | as named |
| 11 | The Hyprland bindings file and its line format | `~/.config/hypr/bindings.conf`, `bindd = SUPER ALT, A, Agent inbox, exec, …` |
| 12 | `omarchy-plugin-validate` and `qmllint` invocation (import paths) | used in Task 21 |

If item 4 differs, the plan's fallback is:
1. `model.mjs` becomes `model.js`, with `.pragma library` as its first line and the `export ` keywords removed.
2. The QML imports become `import "model.js" as Model`.
3. `tests/model.test.mjs` loads it with `node:vm`, as Task 17 Step 4b shows.

If item 7 differs (raw chunks), Service.qml must buffer bytes. Stop and report to the orchestrator; there is no `TextDecoder` to fall back on.

- [ ] **Step 4: Remove the probe and commit the note**

```bash
ssh omarchy 'omarchy plugin disable tonibergholm.probe; rm -rf ~/.config/omarchy/plugins/tonibergholm.probe; omarchy-shell shell rescanPlugins'
git add docs/research/2026-10-12-omarchy-plugin-api.md
git commit -m "Research: the Omarchy shell plugin API as installed"
```

---

### Task 17: `model.mjs` and node tests

**Execution:** cloud-ok. Starts after Task 16's research note; if the note changes item 4 or 5, apply its fallback here.

**Files:**
- Create: `clients/omarchy/model.mjs`, `clients/omarchy/tests/model.test.mjs`, `clients/omarchy/tests/host.test.mjs`
- Modify: `.github/workflows/ci.yml` (a node step on the Ubuntu leg)

**Interfaces:**
- Consumes: the wire shapes of Task 4 (D7), the error codes (D8), and `InboxView` and `CardView` as the Mac bindings describe them (`app/src/lib/bindings/*.ts`). `settings.machines[*].secret` comes from Task 3.
- Produces (ES module `clients/omarchy/model.mjs`; the QML of Tasks 18 and 19 imports it as `Model`):
  - Constants `MAX_LINE`, `ANSWER_MAX_BYTES`, `REPLY_MAX_BYTES`, `UNCERTAIN_TEXT`, `QT`.
  - `utf8Length(s)`, pure JS, since the QML host has no `TextEncoder` (D20).
  - `class LineReader { constructor(max = MAX_LINE); push(text) -> {lines, overflow} }` over `SplitParser` output (D20).
  - `parseMessage(line) -> object | null`.
  - `class Ids { constructor(prefix); next() -> string }`.
  - `sections(cards, machine) -> {needs, working, done}`, as `app/src/lib/panel.ts` does.
  - `headerText(view)`, `barState(view, connected) -> {count, tone: "dim"|"urgent"|"down", text}`.
  - `machineChips(view, settings) -> [{key, name, status, label}]` and `keyringLocked(settings) -> bool`.
  - `requestTag(card)`, `cardMeta(card)`, `optionRows(card) -> [{number, id, label, rule, pickable}]` (D30).
  - `countdownFraction(action, nowMs)`.
  - `fromQt(key, text, modifiers, composing, field) -> KeyInput`.
  - `onKey(visibleCards, state, input) -> {state, effect}`, with effects `none | select | pick | focusAnswer | openReply | submitReply | submitAnswer | submitPairing | markSeen | escape`.
  - `validateAnswer(text) -> {ok, text} | {ok: false, reason}` and `validateReply(text)`, with the same rules as `acd-client`.
  - `errorText(code)`, `reasonText(reason)`.
  - `class Tracker { started(actionId, cardKey); outcome(msg); lost() -> cardKeys }`.
  - `answerEditor(card) -> {cardKey, requestId, revision} | null`, `answerEditorValid(editor, cards)` and `EDITOR_CLOSED_TEXT` (D34).
  - `omittedText(omitted)` (D9).
  - `overflowText(needsYou)` (D9), the chip status `unknown` with "Keyring state unknown. Retry" (D36), and `class Drafts { get, set, clear, prune(view, settings) }` with `draftKey.{answer, reply, rename}` (D35).

- [ ] **Step 1: Write the failing tests**

`clients/omarchy/tests/model.test.mjs`:

```js
import test from "node:test";
import assert from "node:assert/strict";
import * as M from "../model.mjs";

const pending = (opts = {}) => ({
  id: "r1", revision: 1, agent_id: "a1", kind: "permission", status: "open", answer_path: "hook",
  title: "Bash", detail: "npm test", free_text: false, raw_screen: null, questions: null,
  opened_at: "2026-10-08T00:00:00Z",
  options: [
    { id: "allow", label: "Yes", description: null, effect: "allow" },
    { id: "always", label: "Yes, always", description: "Bash(npm test:*)", effect: "allow_always" },
    { id: "deny", label: "No", description: null, effect: "deny" },
    { id: "odd", label: "Something", description: null, effect: "unknown" },
  ],
  ...opts,
});

const card = (key, group = "request", extra = {}) => ({
  key, machine_key: key.split("/")[0], machine_name: "Box", group, live: true, offline_since_ms: null,
  answerable: group === "request", free_text_answerable: false, selectable_options: ["allow", "always", "deny"],
  can_reply: true, can_mark_seen: group === "done", busy: false, notice: null, action: null,
  agent: { id: key.split("/")[1], source: "herdr", kind: "claude", title: "agent", project: "api", cwd: "/w/api",
    state: group === "request" ? "needs_you" : group, pending: group === "request" ? pending() : null },
  ...extra,
});

const view = (cards, machines = [{ key: "m1", name: "Box", machine_id: "x", status: { kind: "online" }, is_local: false, duplicate_of: null }]) =>
  ({ machines, cards, hidden_idle: 0, show_idle: false, show_subagents: false, show_hook_only: false,
     tray: { color: "amber", needs_you: cards.filter((c) => c.group === "request").length }, actions: [] });

const key = (k, extra = {}) => ({ key: k, shift: false, ctrl: false, alt: false, meta: false, composing: false, field: null, ...extra });

test("the line reader handles partial lines, several lines per chunk and split characters", () => {
  const r = new M.LineReader();
  assert.deepEqual(r.push('{"a":'), { lines: [], overflow: false });
  assert.deepEqual(r.push('1}\n{"b":2}\n{"c"'), { lines: ['{"a":1}', '{"b":2}'], overflow: false });
  assert.deepEqual(r.push(":3}\n"), { lines: ['{"c":3}'], overflow: false });
  const emoji = "😀";
  assert.deepEqual(r.push(`"${emoji[0]}`), { lines: [], overflow: false });
  assert.deepEqual(r.push(`${emoji[1]}"\n`), { lines: [`"${emoji}"`], overflow: false });
});

test("UTF-8 lengths are counted without TextEncoder", () => {
  assert.equal(M.utf8Length("a"), 1);
  assert.equal(M.utf8Length("é"), 2);
  assert.equal(M.utf8Length("€"), 3);
  assert.equal(M.utf8Length("😀"), 4);
  assert.equal(M.utf8Length("😀"[0]), 3, "a lone surrogate");
  const sample = "aé€😀\u{10FFFF}x";
  assert.equal(M.utf8Length(sample), Buffer.byteLength(sample, "utf8"));
});

test("a line over the limit overflows, counted in UTF-8 bytes", () => {
  const r = new M.LineReader(8);
  assert.equal(r.push("é".repeat(5)).overflow, true, "10 bytes without a newline");
  const s = new M.LineReader(8);
  assert.equal(s.push("12345678\n").overflow, false);
  assert.equal(s.push("123456789\n").overflow, true);
});

test("only JSON objects with a string type are messages", () => {
  assert.deepEqual(M.parseMessage('{"type":"hello","protocol":1}'), { type: "hello", protocol: 1 });
  for (const bad of ["nope", "[]", "null", '{"type":7}', "{}"]) assert.equal(M.parseMessage(bad), null, bad);
});

test("sections follow the Mac panel and a machine filter keeps countdowns", () => {
  const cards = [card("m1/a"), card("m2/b", "working"), card("m1/c", "done"), card("m2/d", "idle"),
    card("m2/e", "request", { action: { id: "x", card_key: "m2/e", label: "Yes", detail: null, fires_at_ms: 0 } })];
  const all = M.sections(cards, null);
  assert.deepEqual(all.needs.map((c) => c.key), ["m1/a", "m2/e"]);
  assert.deepEqual(all.working.map((c) => c.key), ["m2/b"]);
  assert.deepEqual(all.done.map((c) => c.key), ["m1/c", "m2/d"]);
  assert.deepEqual(M.sections(cards, "m1").needs.map((c) => c.key), ["m1/a", "m2/e"]);
  assert.equal(M.headerText(view([card("m1/a"), card("m2/b", "working")])), "Inbox · 1 need you · 1 working");
});

test("the bar is dim, urgent or down", () => {
  assert.deepEqual(M.barState(view([]), true), { count: 0, tone: "dim", text: "" });
  assert.deepEqual(M.barState(view([card("m1/a")]), true), { count: 1, tone: "urgent", text: "1" });
  assert.equal(M.barState(view([card("m1/a")]), false).tone, "down");
  assert.equal(M.barState(null, true).tone, "down");
});

test("a locked keyring is not shown as not paired", () => {
  const v = view([], [{ key: "m1", name: "Box", machine_id: null, status: { kind: "auth_failed", message: "Pair this machine: run `acd pair` there" }, is_local: false, duplicate_of: null }]);
  const settings = { machines: [{ key: "m1", name: "Box", endpoint: { kind: "tcp", host: "h", port: 1 }, pairing: "unreadable", secret: "locked" }] };
  const [chip] = M.machineChips(v, settings);
  assert.equal(chip.status, "locked");
  assert.equal(chip.label, "Keyring locked");
  assert.equal(M.keyringLocked(settings), true);
  settings.machines[0].secret = "missing";
  assert.equal(M.machineChips(v, settings)[0].status, "not_paired");
  settings.machines[0].secret = "unknown";
  assert.equal(M.machineChips(v, settings)[0].label, "Keyring state unknown. Retry");
  settings.machines[0].secret = "ok";
  assert.equal(M.machineChips(v, settings)[0].status, "error");
  assert.equal(M.keyringLocked(settings), false);
});

test("option rows follow the card's flags", () => {
  const rows = M.optionRows(card("m1/a"));
  assert.deepEqual(rows.map((r) => [r.number, r.id, r.pickable, r.rule]), [
    [1, "allow", true, null], [2, "always", true, "Bash(npm test:*)"], [3, "deny", true, null], [4, "odd", false, null],
  ]);
  const noRule = card("m1/a");
  noRule.agent.pending = pending({ options: [{ id: "always", label: "Always", description: null, effect: "allow_always" }] });
  noRule.selectable_options = ["always"];
  assert.equal(M.optionRows(noRule)[0].pickable, false, "an always-allow option without its rule cannot be picked");
  const readOnly = card("m1/a", "request", { answerable: false });
  assert.ok(M.optionRows(readOnly).every((r) => !r.pickable));
});

test("keys move, pick, and open the reply and answer fields by the card's flags", () => {
  const cards = [card("m1/a"), card("m1/b", "done")];
  let st = { selected: "m1/a", replyFor: null };
  assert.deepEqual(M.onKey(cards, st, key("j")).effect, { kind: "select", key: "m1/b" });
  assert.deepEqual(M.onKey(cards, st, key("ArrowDown")).effect, { kind: "select", key: "m1/b" });
  assert.deepEqual(M.onKey(cards, st, key("2")).effect, { kind: "pick", cardKey: "m1/a", requestId: "r1", optionId: "always", revision: 1 });
  assert.equal(M.onKey(cards, st, key("4")).effect.kind, "none", "unknown effect");
  assert.equal(M.onKey(cards, st, key("9")).effect.kind, "none", "no ninth option");
  assert.equal(M.onKey(cards, st, key("t")).effect.kind, "none", "no typed answer for this request");
  const typed = card("m1/a", "request", { free_text_answerable: true });
  assert.deepEqual(M.onKey([typed], st, key("t")).effect, { kind: "focusAnswer", cardKey: "m1/a" });
  assert.deepEqual(M.onKey(cards, st, key("r")).effect, { kind: "openReply", cardKey: "m1/a" });
  assert.equal(M.onKey(cards, st, key("e")).effect.kind, "none");
  st = { selected: "m1/b", replyFor: null };
  assert.deepEqual(M.onKey(cards, st, key("e")).effect, { kind: "markSeen", cardKey: "m1/b" });
  assert.deepEqual(M.onKey(cards, st, key("Escape")).effect, { kind: "escape" });
});

test("in a text field letters and digits are text, Enter submits, and IME or modifiers are never shortcuts", () => {
  const cards = [card("m1/a")];
  const st = { selected: "m1/a", replyFor: "m1/a" };
  const reply = { kind: "reply", cardKey: "m1/a" };
  for (const k of ["j", "1", "r", "e", "t"]) assert.equal(M.onKey(cards, st, key(k, { field: reply })).effect.kind, "none", k);
  assert.deepEqual(M.onKey(cards, st, key("Enter", { field: reply })).effect, { kind: "submitReply", cardKey: "m1/a" });
  assert.equal(M.onKey(cards, st, key("Enter", { field: reply, shift: true })).effect.kind, "none");
  assert.deepEqual(M.onKey(cards, st, key("Enter", { field: { kind: "answer", cardKey: "m1/a" } })).effect, { kind: "submitAnswer", cardKey: "m1/a" });
  assert.deepEqual(M.onKey(cards, st, key("Enter", { field: { kind: "pairing", cardKey: null } })).effect, { kind: "submitPairing" });
  assert.equal(M.onKey(cards, st, key("Enter", { field: reply, composing: true })).effect.kind, "none");
  assert.equal(M.onKey(cards, { selected: "m1/a", replyFor: null }, key("1", { composing: true })).effect.kind, "none");
  assert.equal(M.onKey(cards, { selected: "m1/a", replyFor: null }, key("1", { ctrl: true })).effect.kind, "none");
  assert.deepEqual(M.onKey(cards, st, key("Escape", { field: reply })).effect, { kind: "escape" });
});

test("Qt key events become key inputs", () => {
  assert.deepEqual(M.fromQt(M.QT.Key_Escape, "", 0, false, null).key, "Escape");
  assert.equal(M.fromQt(M.QT.Key_Return, "\r", 0, false, null).key, "Enter");
  assert.equal(M.fromQt(M.QT.Key_Down, "", 0, false, null).key, "ArrowDown");
  const j = M.fromQt(0x4a, "J", M.QT.ShiftModifier, false, null);
  assert.equal(j.key, "J");
  assert.equal(j.shift, true);
  assert.equal(M.fromQt(0x31, "1", M.QT.ControlModifier, false, null).ctrl, true);
  assert.equal(M.fromQt(0x31, "1", 0, true, null).composing, true);
});

test("typed answers and replies are checked as acd-client checks them", () => {
  assert.deepEqual(M.validateAnswer("  yes\r\nplease  "), { ok: true, text: "yes\nplease" });
  assert.deepEqual(M.validateAnswer("   "), { ok: false, reason: "empty" });
  assert.deepEqual(M.validateAnswer("a\u202Eb"), { ok: false, reason: "characters" });
  assert.deepEqual(M.validateAnswer("a\rb"), { ok: false, reason: "characters" });
  assert.deepEqual(M.validateAnswer("a\tb"), { ok: true, text: "a\tb" });
  assert.deepEqual(M.validateAnswer("é".repeat(8193)), { ok: false, reason: "too_long" });
  assert.deepEqual(M.validateReply(" hi "), { ok: true, text: " hi " });
  assert.deepEqual(M.validateReply(" "), { ok: false, reason: "empty" });
  assert.deepEqual(M.validateReply("x".repeat(64 * 1024 + 1)), { ok: false, reason: "too_long" });
});

test("every service error code has text, and unknown codes fall back", () => {
  for (const code of ["stale", "unknown_card", "not_answerable", "unknown_option", "already_counting", "bad_text",
    "cannot_reply", "cannot_mark_seen", "hidden", "unknown_machine", "not_paired", "bad_name", "bad_pairing",
    "bad_request", "secret_store_locked", "secret_store_timeout", "secret_store_failed", "settings_failed",
    "duplicate_id", "unsupported", "unreachable"]) {
    assert.ok(M.errorText(code).length > 0, code);
    assert.notEqual(M.errorText(code), M.errorText("never-heard-of-it"), code);
  }
  assert.equal(M.errorText("stale"), "The request changed. Review it again.");
});

test("after outcomes_lost, this view's unfinished actions are uncertain", () => {
  const t = new M.Tracker();
  t.started("x1", "m1/a");
  t.started("x2", "m1/b");
  t.started("x3", "m1/c");
  t.outcome({ type: "outcome", action: "x1", card: "m1/a", result: { kind: "fired" } });
  t.outcome({ type: "outcome", action: "x2", card: "m1/b", result: { kind: "delivered" } });
  assert.deepEqual(t.lost().sort(), ["m1/a", "m1/c"]);
  assert.deepEqual(t.lost(), []);
  assert.equal(M.UNCERTAIN_TEXT, "Check the terminal");
});

test("ids are unique and short", () => {
  const ids = new M.Ids("bar");
  const a = ids.next();
  const b = ids.next();
  assert.notEqual(a, b);
  assert.ok(a.length <= 64);
});

test("a typed answer stays bound to the request it was opened for", () => {
  const c = card("m1/a", "request", { free_text_answerable: true });
  const editor = M.answerEditor(c);
  assert.deepEqual(editor, { cardKey: "m1/a", requestId: "r1", revision: 1 });
  assert.equal(M.answerEditorValid(editor, [c]), true);
  const bumped = card("m1/a", "request", { free_text_answerable: true });
  bumped.agent.pending = pending({ revision: 2 });
  assert.equal(M.answerEditorValid(editor, [bumped]), false, "a new revision closes the editor");
  const replaced = card("m1/a", "request", { free_text_answerable: true });
  replaced.agent.pending = pending({ id: "r2" });
  assert.equal(M.answerEditorValid(editor, [replaced]), false, "a new request closes the editor");
  assert.equal(M.answerEditorValid(editor, []), false);
  assert.equal(M.answerEditor(card("m1/a")), null, "no editor without a typed-answer request");
});

test("omitted cards are counted for the panel", () => {
  assert.equal(M.omittedText({ done: 0, working: 0 }), "");
  assert.equal(M.omittedText({ done: 3, working: 0 }), "3 done not shown");
  assert.equal(M.omittedText({ done: 3, working: 2 }), "3 done and 2 working not shown");
  assert.equal(M.omittedText(undefined), "");
});

test("drafts survive an unrelated new snapshot and go with their request", () => {
  const drafts = new M.Drafts();
  const c = card("m1/a", "request", { free_text_answerable: true });
  const answerKey = M.draftKey.answer(M.answerEditor(c));
  drafts.set(answerKey, "half an answer");
  drafts.set(M.draftKey.reply(c), "half a reply");
  drafts.set(M.draftKey.rename("m1"), "New na");
  const settings = { machines: [{ key: "m1" }] };
  // An unrelated change: another card appears; the view is a new object.
  const next = view([card("m1/a", "request", { free_text_answerable: true }), card("m1/b", "working")]);
  drafts.prune(next, settings);
  assert.equal(drafts.get(M.draftKey.answer(M.answerEditor(next.cards[0]))), "half an answer");
  assert.equal(drafts.get(M.draftKey.reply(next.cards[0])), "half a reply");
  assert.equal(drafts.get(M.draftKey.rename("m1")), "New na");
  const changed = card("m1/a", "request", { free_text_answerable: true });
  changed.agent.pending = pending({ id: "r2" });
  drafts.prune(view([changed]), { machines: [] });
  assert.equal(drafts.get(answerKey), "", "a new request drops the old answer draft");
  assert.equal(drafts.get(M.draftKey.rename("m1")), "", "a removed machine drops its rename draft");
});

test("an overflowing view has its own text", () => {
  assert.equal(M.overflowText(7), "Too many requests to show here (7 need you). Answer some in the terminal.");
});

test("card meta and the countdown bar", () => {
  const c = card("m1/a");
  assert.deepEqual(M.cardMeta(c), { kind: "Claude", machine: "Box", project: "api", title: "agent", badge: "herdr", tag: "BASH APPROVAL" });
  assert.equal(M.countdownFraction({ fires_at_ms: 2000 }, 1000), 0.5);
  assert.equal(M.countdownFraction({ fires_at_ms: 0 }, 1000), 0);
});
```

`clients/omarchy/tests/host.test.mjs`, which loads the model with no `TextEncoder` or `TextDecoder`, as in QML:

```js
import test from "node:test";
import assert from "node:assert/strict";

// QML's JavaScript host has no TextEncoder or TextDecoder (plan D20).
test("model.mjs works without browser or Node globals", async () => {
  delete globalThis.TextEncoder;
  delete globalThis.TextDecoder;
  const M = await import("../model.mjs");
  const r = new M.LineReader(8);
  assert.deepEqual(r.push("é😀\n"), { lines: ["é😀"], overflow: false });
  assert.equal(r.push("éééée\n").overflow, true);
  assert.deepEqual(M.validateAnswer(" ok "), { ok: true, text: "ok" });
});
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `node --test 'clients/omarchy/tests/*.test.mjs'`
Expected: FAIL, `Cannot find module '…/clients/omarchy/model.mjs'`.

- [ ] **Step 3: Implement `model.mjs`**

```js
// Pure view logic for the Omarchy plugin (omarchy spec §7). No QML or I/O,
// so `node --test` covers it. Service.qml, BarWidget.qml and Panel.qml
// import it as `Model`.

export const MAX_LINE = 4 * 1024 * 1024;
export const ANSWER_MAX_BYTES = 16 * 1024;
export const REPLY_MAX_BYTES = 64 * 1024;
export const UNDO_MS = 2000;
export const UNCERTAIN_TEXT = "Check the terminal";

/**
 * UTF-8 length of a JS string, in pure JS: QML's host has no TextEncoder
 * (plan D20). A lone surrogate counts 3, as its replacement character would.
 */
export function utf8Length(s) {
  let n = 0;
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i);
    if (c < 0x80) n += 1;
    else if (c < 0x800) n += 2;
    else if (c >= 0xd800 && c <= 0xdbff && i + 1 < s.length && (s.charCodeAt(i + 1) & 0xfc00) === 0xdc00) {
      n += 4;
      i += 1;
    } else n += 3;
  }
  return n;
}

// A UTF-16 unit is at least one UTF-8 byte and at most three.
const tooLong = (s, max) => s.length > max || (s.length * 3 > max && utf8Length(s) > max);

/**
 * Joins Quickshell SplitParser output into lines and enforces the 4 MiB
 * limit (plan D20). SplitParser splits bytes before decoding, so text
 * arrives as whole characters; this reader also copes with a line given in
 * pieces. On overflow the caller drops the connection.
 */
export class LineReader {
  constructor(max = MAX_LINE) {
    this.max = max;
    this.buf = "";
  }

  push(text) {
    this.buf += text;
    const lines = [];
    let i;
    while ((i = this.buf.indexOf("\n")) >= 0) {
      const line = this.buf.slice(0, i);
      this.buf = this.buf.slice(i + 1);
      if (tooLong(line, this.max)) return { lines, overflow: true };
      if (line.length > 0) lines.push(line);
    }
    return { lines, overflow: tooLong(this.buf, this.max) };
  }
}

export function parseMessage(line) {
  let v;
  try {
    v = JSON.parse(line);
  } catch {
    return null;
  }
  return v !== null && typeof v === "object" && !Array.isArray(v) && typeof v.type === "string" ? v : null;
}

export class Ids {
  constructor(prefix) {
    this.prefix = prefix;
    this.n = 0;
  }

  next() {
    this.n += 1;
    return `${this.prefix}-${this.n}`;
  }
}

/** The three sections, as app/src/lib/panel.ts: a countdown card stays whatever the machine filter says. */
export function sections(cards, machine) {
  const shown = machine === null ? cards : cards.filter((c) => c.machine_key === machine || c.action !== null);
  return {
    needs: shown.filter((c) => c.group === "request"),
    working: shown.filter((c) => c.group === "working" || c.group === "unknown"),
    done: shown.filter((c) => c.group === "done" || c.group === "idle"),
  };
}

export function headerText(view) {
  const working = view.cards.filter((c) => c.agent.state === "working").length;
  return `Inbox · ${view.tray.needs_you} need you · ${working} working`;
}

export function barState(view, connected) {
  if (!connected || !view) return { count: 0, tone: "down", text: "" };
  const n = view.tray.needs_you;
  return { count: n, tone: n > 0 ? "urgent" : "dim", text: n > 0 ? String(n) : "" };
}

function dotKind(status) {
  switch (status.kind) {
    case "online":
      return "online";
    case "connecting":
    case "syncing":
      return "syncing";
    case "offline":
      return "offline";
    default:
      return "error";
  }
}

const CHIP_TEXT = {
  online: "Online",
  syncing: "Connecting",
  offline: "Offline",
  error: "Error",
  locked: "Keyring locked",
  not_paired: "Not paired",
  unreadable: "Pair again",
  unknown: "Keyring state unknown. Retry",
};

/** Machine chips: connection status from the view, joined by key with secret status from settings (plan S1, S6). */
export function machineChips(view, settings) {
  const entries = new Map((settings?.machines ?? []).map((m) => [m.key, m]));
  return (view?.machines ?? [])
    .filter((m) => m.duplicate_of === null)
    .map((m) => {
      let status = dotKind(m.status);
      const e = entries.get(m.key);
      if (e && e.endpoint.kind === "tcp") {
        if (e.secret === "locked") status = "locked";
        else if (e.secret === "missing") status = "not_paired";
        else if (e.secret === "unreadable") status = "unreadable";
        else if (e.secret === "unknown") status = "unknown";
      }
      return { key: m.key, name: m.name, status, label: CHIP_TEXT[status] };
    });
}

export function keyringLocked(settings) {
  return (settings?.machines ?? []).some((m) => m.secret === "locked");
}

const TOOL_NAME = /^[A-Za-z][A-Za-z0-9_.:-]{0,39}$/;

export function requestTag(card) {
  const p = card.agent.pending;
  if (!p) return "NEEDS YOU";
  switch (p.kind) {
    case "permission":
      return TOOL_NAME.test(p.title) ? `${p.title.toUpperCase()} APPROVAL` : "APPROVAL";
    case "question":
      return "QUESTION";
    case "blocked_unparsed":
      return "BLOCKED";
    default:
      return "REQUEST";
  }
}

const KIND = { claude: "Claude", codex: "Codex" };

export function cardMeta(card) {
  const a = card.agent;
  const project = a.project ?? (a.cwd ? a.cwd.split("/").filter(Boolean).pop() ?? null : null);
  const badge = a.source === "herdr" ? "herdr" : a.source === "t3" ? "T3" : null;
  return { kind: KIND[a.kind] ?? "agent", machine: card.machine_name, project, title: a.title, badge, tag: requestTag(card) };
}

/** Options as the panel lists them (plan D30). */
export function optionRows(card) {
  const p = card.agent.pending;
  if (!p) return [];
  return p.options.map((o, i) => {
    const rule = o.effect === "allow_always" ? (o.description ?? null) : null;
    const pickable =
      card.answerable && i < 9 && card.selectable_options.includes(o.id) && (o.effect !== "allow_always" || rule !== null);
    return { number: i + 1, id: o.id, label: o.label, rule, pickable };
  });
}

/**
 * A typed-answer editor remembers the request it was opened for (spec §7):
 * the answer goes to that request id and revision, never to a later one.
 */
export function answerEditor(card) {
  const p = card.agent.pending;
  if (!card.free_text_answerable || !p) return null;
  return { cardKey: card.key, requestId: p.id, revision: p.revision };
}

/** False once the card is gone, its request changed, or it no longer takes a typed answer: the editor closes. */
export function answerEditorValid(editor, cards) {
  if (!editor) return false;
  const card = cards.find((c) => c.key === editor.cardKey);
  const p = card?.agent.pending;
  return !!card && card.free_text_answerable && !!p && p.id === editor.requestId && p.revision === editor.revision;
}

export const EDITOR_CLOSED_TEXT = "The request changed. Your answer was not sent.";

/** The error state for a view the service could not fit (plan D9). */
export function overflowText(needsYou) {
  return `Too many requests to show here (${needsYou} need you). Answer some in the terminal.`;
}

/** Draft keys (plan D35): a draft belongs to a card and its request, or to a machine. */
export const draftKey = {
  answer: (editor) => `answer:${editor.cardKey}:${editor.requestId}:${editor.revision}`,
  reply: (card) => `reply:${card.key}:${card.agent.pending ? card.agent.pending.id : "-"}`,
  rename: (machineKey) => `rename:${machineKey}`,
};

/**
 * Typed text kept outside QML delegates, which a new view recreates
 * (plan D35). The pairing line is never stored here.
 */
export class Drafts {
  constructor() {
    this.texts = new Map();
  }

  get(key) {
    return this.texts.get(key) ?? "";
  }

  set(key, text) {
    if (text === "") this.texts.delete(key);
    else this.texts.set(key, text);
  }

  clear(key) {
    this.texts.delete(key);
  }

  /** Drops drafts whose card, request or machine is gone. */
  prune(view, settings) {
    const live = new Set();
    for (const c of view?.cards ?? []) {
      live.add(draftKey.reply(c));
      const e = answerEditor(c);
      if (e) live.add(draftKey.answer(e));
    }
    for (const m of settings?.machines ?? []) live.add(draftKey.rename(m.key));
    for (const key of [...this.texts.keys()]) if (!live.has(key)) this.texts.delete(key);
  }
}

/** "N not shown" for the cards a large view left out (plan D9). */
export function omittedText(omitted) {
  const parts = [];
  if (omitted?.done > 0) parts.push(`${omitted.done} done`);
  if (omitted?.working > 0) parts.push(`${omitted.working} working`);
  return parts.length === 0 ? "" : `${parts.join(" and ")} not shown`;
}

export function countdownFraction(action, nowMs) {
  return Math.max(0, Math.min(1, (action.fires_at_ms - nowMs) / UNDO_MS));
}

/** Qt enum values (Qt::Key, Qt::KeyboardModifier). */
export const QT = {
  Key_Escape: 0x01000000,
  Key_Return: 0x01000004,
  Key_Enter: 0x01000005,
  Key_Up: 0x01000013,
  Key_Down: 0x01000015,
  ShiftModifier: 0x02000000,
  ControlModifier: 0x04000000,
  AltModifier: 0x08000000,
  MetaModifier: 0x10000000,
};

/** `field`: null, or {kind: "reply"|"answer"|"pairing", cardKey}. `composing`: the focused field's inputMethodComposing. */
export function fromQt(key, text, modifiers, composing, field) {
  const name =
    key === QT.Key_Escape ? "Escape"
    : key === QT.Key_Return || key === QT.Key_Enter ? "Enter"
    : key === QT.Key_Up ? "ArrowUp"
    : key === QT.Key_Down ? "ArrowDown"
    : text;
  return {
    key: name,
    shift: (modifiers & QT.ShiftModifier) !== 0,
    ctrl: (modifiers & QT.ControlModifier) !== 0,
    alt: (modifiers & QT.AltModifier) !== 0,
    meta: (modifiers & QT.MetaModifier) !== 0,
    composing: !!composing,
    field: field ?? null,
  };
}

const NONE = { kind: "none" };

/** Keys (omarchy spec §7), as app/src/lib/keys.ts, over the cards in display order. */
export function onKey(visible, st, ev) {
  const none = { state: st, effect: NONE };
  if (ev.composing || ev.ctrl || ev.alt || ev.meta) return none;
  if (ev.key === "Escape") return { state: st, effect: { kind: "escape" } };
  if (ev.field) {
    if (ev.key !== "Enter" || ev.shift) return none;
    if (ev.field.kind === "answer" && ev.field.cardKey) return { state: st, effect: { kind: "submitAnswer", cardKey: ev.field.cardKey } };
    if (ev.field.kind === "reply" && st.replyFor) return { state: st, effect: { kind: "submitReply", cardKey: st.replyFor } };
    if (ev.field.kind === "pairing") return { state: st, effect: { kind: "submitPairing" } };
    return none;
  }
  const k = ev.key.length === 1 ? ev.key.toLowerCase() : ev.key;
  const idx = visible.findIndex((c) => c.key === st.selected);
  if (k === "j" || k === "ArrowDown" || k === "k" || k === "ArrowUp") {
    if (visible.length === 0) return none;
    const down = k === "j" || k === "ArrowDown";
    const next = idx < 0 ? 0 : Math.min(visible.length - 1, Math.max(0, idx + (down ? 1 : -1)));
    const selected = visible[next].key;
    return { state: { ...st, selected }, effect: selected === st.selected ? NONE : { kind: "select", key: selected } };
  }
  const card = idx >= 0 ? visible[idx] : undefined;
  if (!card) return none;
  if (/^[1-9]$/.test(k)) {
    const row = optionRows(card).find((r) => r.number === Number(k));
    if (!row || !row.pickable) return none;
    const p = card.agent.pending;
    return { state: st, effect: { kind: "pick", cardKey: card.key, requestId: p.id, optionId: row.id, revision: p.revision } };
  }
  if (k === "t" && card.free_text_answerable) return { state: st, effect: { kind: "focusAnswer", cardKey: card.key } };
  if (k === "r" && card.can_reply) return { state: st, effect: { kind: "openReply", cardKey: card.key } };
  if (k === "e" && card.can_mark_seen) return { state: st, effect: { kind: "markSeen", cardKey: card.key } };
  return none;
}

// acd_protocol::text: controls other than \n and \t, bidi controls, line and paragraph separators.
const FORBIDDEN = /[\u0000-\u0008\u000B-\u001F\u007F-\u009F\u061C\u200E\u200F\u202A-\u202E\u2066-\u2069\u2028\u2029]/u;

/** As Inbox::answer_text: CRLF to LF, trimmed, 1 byte to 16 KiB, no forbidden characters. */
export function validateAnswer(text) {
  const t = text.replace(/\r\n/g, "\n").trim();
  if (!t) return { ok: false, reason: "empty" };
  if (utf8Length(t) > ANSWER_MAX_BYTES) return { ok: false, reason: "too_long" };
  if (FORBIDDEN.test(t)) return { ok: false, reason: "characters" };
  return { ok: true, text: t };
}

/** As Inbox::reply: not blank, at most 64 KiB; sent as typed. */
export function validateReply(text) {
  if (!text.trim()) return { ok: false, reason: "empty" };
  if (utf8Length(text) > REPLY_MAX_BYTES) return { ok: false, reason: "too_long" };
  return { ok: true, text };
}

const REASONS = {
  empty: "Type something first.",
  too_long: "That is too long to send.",
  characters: "The text has characters that cannot be sent.",
};

export function reasonText(reason) {
  return REASONS[reason] ?? "That cannot be sent.";
}

const UPDATE = "The inbox service did not understand that. Update the plugin or the service.";
const ERRORS = {
  stale: "The request changed. Review it again.",
  unknown_card: "That agent is gone.",
  not_answerable: "Answer this one in the terminal.",
  unknown_option: "That option cannot be picked here.",
  already_counting: "A countdown is already running for this card.",
  bad_text: "The text is empty, too long, or has characters that cannot be sent.",
  cannot_reply: "This agent cannot take a reply right now.",
  cannot_mark_seen: "This agent cannot be marked seen.",
  hidden: "The agent is hidden now. Nothing was sent.",
  unknown_machine: "That machine is not in the list.",
  not_paired: "This machine is not paired. Paste its pairing line.",
  bad_name: "Names are 1 to 64 characters.",
  bad_pairing: "That is not an acd pairing line. Run `acd pair` on the machine and copy its output.",
  bad_request: UPDATE,
  secret_store_locked: "The keyring is locked. Unlock it and try again.",
  secret_store_timeout: "The keyring is not responding. Press Retry on the machine once it answers.",
  secret_store_failed: "The keyring could not be used.",
  settings_failed: "Could not save the settings.",
  duplicate_id: "That was already sent.",
  unsupported: UPDATE,
  unreachable: "The inbox service is not running. Start it with: systemctl --user start acd-inbox",
};

export function errorText(code) {
  return ERRORS[code] ?? "Something went wrong.";
}

/** This view's actions without a terminal outcome yet. */
export class Tracker {
  constructor() {
    this.actions = new Map();
  }

  started(actionId, cardKey) {
    this.actions.set(actionId, cardKey);
  }

  outcome(msg) {
    if (msg.result.kind !== "fired") this.actions.delete(msg.action);
  }

  /** outcomes_lost: every action still open is uncertain; returns their cards. */
  lost() {
    const cards = [...new Set(this.actions.values())];
    this.actions.clear();
    return cards;
  }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `node --test 'clients/omarchy/tests/*.test.mjs'`
Expected: PASS, 21 tests in two files.

- [ ] **Step 4b: Only if Task 16 found `.mjs` imports unsupported**

Apply Task 16's fallback: rename `model.mjs` to `model.js`, make its first line `.pragma library`, remove the `export ` keywords (`sed -i '' 's/^export //' clients/omarchy/model.js`), and load it in both test files with `node:vm`. Replace each `import * as M from "../model.mjs";` (and the dynamic import in `host.test.mjs`) with:

```js
import { readFileSync } from "node:fs";
import vm from "node:vm";

const src = readFileSync(new URL("../model.js", import.meta.url), "utf8").replace(/^\.pragma library\n/, "");
const ctx = vm.createContext({});
vm.runInContext(
  `${src}\nglobalThis.M = { MAX_LINE, ANSWER_MAX_BYTES, REPLY_MAX_BYTES, UNDO_MS, UNCERTAIN_TEXT, EDITOR_CLOSED_TEXT, QT, utf8Length, LineReader, parseMessage, Ids, sections, headerText, barState, machineChips, keyringLocked, requestTag, cardMeta, optionRows, answerEditor, answerEditorValid, omittedText, overflowText, draftKey, Drafts, countdownFraction, fromQt, onKey, validateAnswer, validateReply, reasonText, errorText, Tracker };`,
  ctx,
);
const M = ctx.M;
```

The empty context has no `TextEncoder`, as QML has none. Record the choice in this plan's D19.

- [ ] **Step 5: Run the node tests in CI**

In `.github/workflows/ci.yml`, job `test`, after the clippy step, add:

```yaml
      - if: matrix.os == 'ubuntu-latest'
        run: node --test 'clients/omarchy/tests/*.test.mjs'
```

The `ubuntu-latest` image ships Node, so no setup step is needed. The glob form is required (plan S5).

- [ ] **Step 6: Commit**

```bash
git add clients/omarchy/model.mjs clients/omarchy/tests/model.test.mjs clients/omarchy/tests/host.test.mjs .github/workflows/ci.yml
git commit -m "Omarchy plugin: view model with node tests"
```

---
### Task 18: Plugin manifest, socket client and bar widget

**Execution:** cloud-ok for writing the files and the node checks. They cannot load without the Omarchy shell; Task 21 validates them on the machine. Names follow Task 16's research note: where the note says "differs", use the recorded name instead of the one below.

**Files:**
- Create: `clients/omarchy/manifest.json`, `clients/omarchy/Service.qml`, `clients/omarchy/BarWidget.qml`, `clients/omarchy/tests/plugin_files.test.mjs`

**Interfaces:**
- Consumes: Task 17's `model.mjs` (as `Model`); Task 16's research note; the protocol of Tasks 4, 9 and 10.
- Produces:
  - `Service.qml` properties: `view`, `settings`, `omitted`, `overflow` (the `needs_you` of a `view_overflow`, else -1), `connected` and `tracker`.
  - Its signals: `outcome(msg)`, `outcomesLost(cards)` and `ready()`.
  - Its function `send(type, fields, done)`.
  - `BarWidget.qml` owns `Service` and `Panel` (Task 19) and declares the IPC target `tonibergholm.agentcaller`.
- Rules (spec §7):
  - `Commons` colours and sizes only.
  - Every write is followed by `flush()`.
  - No `console.` calls, and no `TextEncoder` or `TextDecoder`: `SplitParser` hands over whole lines as strings (D20).

- [ ] **Step 1: Write the failing checks**

`clients/omarchy/tests/plugin_files.test.mjs`:

```js
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const read = (name) => readFileSync(new URL(`../${name}`, import.meta.url), "utf8");
const QML = ["BarWidget.qml", "Service.qml"]; // Task 19 adds Panel.qml

test("the manifest names the plugin, its kind, section and category", () => {
  const m = JSON.parse(read("manifest.json"));
  assert.equal(m.id, "tonibergholm.agentcaller");
  assert.deepEqual(m.kinds, ["bar-widget"]);
  assert.equal(m.defaultSection, "right");
  assert.equal(m.category, "AI");
});

test("the IPC target has open, close, toggle and openCard", () => {
  const bar = read("BarWidget.qml");
  assert.match(bar, /target:\s*"tonibergholm\.agentcaller"/);
  for (const fn of ["open", "close", "toggle", "openCard"]) assert.match(bar, new RegExp(`function ${fn}\\(`), fn);
});

test("no QML file logs or sets colours of its own", () => {
  for (const f of QML) {
    const src = read(f);
    assert.doesNotMatch(src, /console\./, f);
    assert.doesNotMatch(src, /#[0-9a-fA-F]{3,8}\b/, f);
    assert.doesNotMatch(src, /color:\s*"/, f);
    assert.doesNotMatch(src, /TextEncoder|TextDecoder/, f);
  }
});

test("every write is flushed and every QML file uses model.mjs", () => {
  assert.match(read("Service.qml"), /socket\.write\([^\n]*\);\s*\n\s*socket\.flush\(\);/);
  for (const f of QML) assert.match(read(f), /import "model\.mjs" as Model/, f);
});

test("a view message carries its omitted counts to the panel", () => {
  assert.match(read("Service.qml"), /root\.omitted = msg\.omitted;/);
});
```

- [ ] **Step 2: Run the checks to verify they fail**

Run: `node --test 'clients/omarchy/tests/*.test.mjs'`
Expected: FAIL, `ENOENT … manifest.json`.

- [ ] **Step 3: Write `manifest.json`**

```json
{
  "id": "tonibergholm.agentcaller",
  "name": "Agent Caller",
  "version": "0.1.0",
  "description": "Answer coding agents that need you.",
  "author": "Toni Bergholm",
  "category": "AI",
  "kinds": ["bar-widget"],
  "defaultSection": "right",
  "barWidget": "BarWidget.qml"
}
```

- [ ] **Step 4: Write `Service.qml`**

```qml
// The socket client (omarchy spec §7). Logic and texts live in model.mjs.
import QtQuick
import Quickshell
import Quickshell.Io
import "model.mjs" as Model

Item {
    id: root

    property var view: null
    property var settings: null
    // Cards the service left out of a large view (plan D9).
    property var omitted: ({ done: 0, working: 0 })
    // `needs_you` of a `view_overflow` message; -1 while views fit (plan D9).
    property int overflow: -1
    property bool connected: false
    property var tracker: new Model.Tracker()
    property var ids: new Model.Ids("omarchy")
    property var reader: new Model.LineReader()
    property var waiting: ({})
    property int backoffMs: 500

    signal outcome(var msg)
    signal outcomesLost(var cards)
    signal ready()

    readonly property string socketPath: Quickshell.env("XDG_RUNTIME_DIR") + "/acd-inbox.sock"

    function reconnectSoon() {
        retry.interval = root.backoffMs;
        root.backoffMs = Math.min(root.backoffMs * 2, 10000);
        retry.restart();
    }

    function dropConnection() {
        socket.connected = false;
    }

    function handle(msg) {
        if (msg === null) {
            dropConnection();
            return;
        }
        switch (msg.type) {
        case "hello":
            if (msg.protocol !== 1) dropConnection();
            else root.ready();
            break;
        case "settings":
            root.settings = msg;
            break;
        case "view":
            root.view = msg.view;
            root.omitted = msg.omitted;
            root.overflow = -1;
            break;
        case "view_overflow":
            root.view = null;
            root.overflow = msg.needs_you;
            break;
        case "outcome":
            root.tracker.outcome(msg);
            root.outcome(msg);
            break;
        case "outcomes_lost":
            root.outcomesLost(root.tracker.lost());
            break;
        case "ok":
        case "error": {
            const done = root.waiting[msg.id];
            delete root.waiting[msg.id];
            if (done) done(msg);
            break;
        }
        }
    }

    // Sends a command; `done(reply)` gets its ok or error. A command is never
    // sent again after a reconnect: the user acts again (spec §5.2).
    function send(type, fields, done) {
        if (!root.connected) {
            if (done) done({ type: "error", code: "unreachable" });
            return;
        }
        const id = root.ids.next();
        if (done) root.waiting[id] = done;
        socket.write(JSON.stringify(Object.assign({}, fields, { type: type, id: id })) + "\n");
        socket.flush();
    }

    Socket {
        id: socket
        path: root.socketPath
        connected: true

        onConnectedChanged: {
            root.connected = connected;
            if (connected) {
                root.backoffMs = 500;
                root.reader = new Model.LineReader();
                return;
            }
            root.view = null;
            const pending = root.waiting;
            root.waiting = ({});
            for (const id in pending) pending[id]({ type: "error", code: "unreachable" });
            root.reconnectSoon();
        }

        onError: root.reconnectSoon()

        parser: SplitParser {
            splitMarker: "\n"
            onRead: data => {
                const r = root.reader.push(data + "\n");
                for (const line of r.lines) root.handle(Model.parseMessage(line));
                if (r.overflow) root.dropConnection();
            }
        }
    }

    Timer {
        id: retry
        repeat: false
        onTriggered: {
            socket.connected = false;
            socket.connected = true;
        }
    }
}
```

- [ ] **Step 5: Write `BarWidget.qml`**

```qml
// Bar widget (omarchy spec §7): a glyph and the number of agents that need
// the user. Dim when none, urgent when some, struck through when the
// service is unreachable. Click toggles the panel.
import QtQuick
import Quickshell.Io
import qs.Commons as Commons
import "model.mjs" as Model

Item {
    id: root

    readonly property var bar: Model.barState(service.view, service.connected)

    implicitWidth: row.implicitWidth + Commons.Style.spacing * 2
    implicitHeight: row.implicitHeight

    Service {
        id: service
    }

    Panel {
        id: panel
        service: service
    }

    Row {
        id: row
        anchors.centerIn: parent
        spacing: Commons.Style.spacingSmall

        Text {
            text: "◉"
            font.strikeout: root.bar.tone === "down"
            color: root.bar.tone === "urgent" ? Commons.Color.urgent : Commons.Color.dim
        }

        Text {
            visible: root.bar.count > 0
            text: root.bar.text
            color: Commons.Color.urgent
        }
    }

    MouseArea {
        anchors.fill: parent
        onClicked: panel.toggle()
    }

    // omarchy-shell tonibergholm.agentcaller open|close|toggle|openCard <key>
    IpcHandler {
        target: "tonibergholm.agentcaller"

        function open(): void { panel.open(); }
        function close(): void { panel.close(); }
        function toggle(): void { panel.toggle(); }
        function openCard(key: string): void { panel.openCard(key); }
    }
}
```

- [ ] **Step 6: Run the checks to verify they pass**

Run: `node --test 'clients/omarchy/tests/*.test.mjs'`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add clients/omarchy
git commit -m "Omarchy plugin: manifest, socket client and bar widget"
```

---

### Task 19: Plugin panel

**Execution:** cloud-ok for writing the file and the node checks; Task 21 validates it on the machine. Names follow Task 16's research note.

**Files:**
- Create: `clients/omarchy/Panel.qml`
- Modify: `clients/omarchy/tests/plugin_files.test.mjs`

**Interfaces:**
- Consumes: Task 17's `model.mjs`; Task 18's `Service.qml`.
- Produces: `Panel.qml` with `open()`, `close()`, `toggle()`, `openCard(key)` and `keyEvent(event, field, composing, input)`.
- Rules (spec §7, D34):
  - Every key, in the panel or in a text field, goes through `Model.onKey` with the field's kind and its `inputMethodComposing`. So letters and digits typed into a field stay text, Enter submits, and IME composition is never a shortcut.
  - A typed answer remembers the request id and revision it was opened for and sends those. When the view shows that request changed or went away, the editor closes with "The request changed. Your answer was not sent.", and the text is dropped.
  - "N not shown" appears when the service left cards out. An explicit error state with the Needs-you count appears when it sent `view_overflow`; the next fitting view clears it (D9).
  - Reply, typed-answer and rename texts are kept in `Model.Drafts`, outside the delegates, so an unrelated new view does not erase them (D35). The pairing line is never drafted.
  - The pairing field is masked, cleared before each send, and never logged.

- [ ] **Step 1: Write the failing checks**

In `clients/omarchy/tests/plugin_files.test.mjs`, change the `QML` list to `["BarWidget.qml", "Panel.qml", "Service.qml"]` and append:

```js

test("the pairing field is masked and cleared before the line is sent", () => {
  const panel = read("Panel.qml");
  assert.match(panel, /echoMode:\s*TextInput\.Password/);
  const body = panel.slice(panel.indexOf("function submitPairing"));
  const cleared = body.indexOf('pairingField.text = ""');
  const sent = body.indexOf('service.send("pair"');
  assert.ok(cleared > 0 && sent > cleared, "cleared before sending");
});

test("every text field sends its keys through model.mjs with its IME state", () => {
  const panel = read("Panel.qml");
  const fields = panel.split("TextField {").slice(1);
  assert.equal(fields.length, 4, "reply, answer, machine name, pairing");
  for (const f of fields) {
    assert.match(f.slice(0, f.indexOf("\n            }")), /Keys\.onPressed: event => panel\.keyEvent\(event, \{ kind: "[a-z]+"/);
    assert.match(f, /inputMethodComposing/);
  }
});

test("a typed answer goes to the request its editor was opened for", () => {
  const panel = read("Panel.qml");
  const body = panel.slice(panel.indexOf("function submitAnswer"), panel.indexOf("function keyEvent"));
  assert.match(body, /request: editor\.requestId, revision: editor\.revision/);
  assert.doesNotMatch(body, /pending\.(id|revision)/);
  assert.match(panel, /onViewChanged:[\s\S]*answerEditorValid/);
});

test("drafts live outside the delegates; the pairing line never does", () => {
  const panel = read("Panel.qml");
  assert.match(panel, /property var drafts: new Model\.Drafts\(\)/);
  for (const kind of ["reply", "answer", "rename"]) assert.match(panel, new RegExp(`Model\\.draftKey\\.${kind}\\(`), kind);
  assert.ok((panel.match(/panel\.drafts\.set\(/g) ?? []).length >= 3);
  const pairing = panel.slice(panel.indexOf("id: pairingField"), panel.indexOf("}", panel.indexOf("id: pairingField")));
  assert.doesNotMatch(pairing, /drafts/);
});

test("an overflowing view is shown as an error with its count", () => {
  assert.match(read("Service.qml"), /case "view_overflow":[\s\S]*root\.overflow = msg\.needs_you;/);
  assert.match(read("Panel.qml"), /Model\.overflowText\(panel\.service\.overflow\)/);
});
```

- [ ] **Step 2: Run the checks to verify they fail**

Run: `node --test 'clients/omarchy/tests/*.test.mjs'`
Expected: FAIL, `ENOENT … Panel.qml`.

- [ ] **Step 3: Write `Panel.qml`**

```qml
// The inbox panel (omarchy spec §7). What shows follows InboxView exactly;
// the machine filter is the panel's own. Logic and texts come from model.mjs.
import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import qs.Commons as Commons
import qs.Ui as Ui
import "model.mjs" as Model

Ui.Panel {
    id: panel

    required property var service

    property bool shown: false
    property var keyState: ({ selected: null, replyFor: null })
    // The request a typed answer was opened for (plan D34).
    property var answerEditor: null
    // Typed text outside the delegates, which every new view recreates (plan D35).
    property var drafts: new Model.Drafts()
    property string machineFilter: ""
    property bool settingsOpen: false
    property bool doneOpen: false
    property string message: ""
    property var uncertain: ({})
    property real now: Date.now()

    readonly property var view: service.view
    readonly property var groups: Model.sections(view ? view.cards : [], machineFilter === "" ? null : machineFilter)
    readonly property var visibleCards: groups.needs.concat(groups.working, doneOpen ? groups.done : [])
    readonly property var chips: Model.machineChips(view, service.settings)

    visible: shown
    focus: shown

    function open() {
        shown = true;
        service.send("panel", { open: true });
    }

    function close() {
        shown = false;
        service.send("panel", { open: false });
    }

    function toggle() {
        if (shown) close();
        else open();
    }

    function openCard(key) {
        open();
        settingsOpen = false;
        keyState = { selected: key, replyFor: null };
    }

    function cardFor(key) {
        return (view ? view.cards : []).find(c => c.key === key) ?? null;
    }

    function report(reply) {
        message = reply.type === "error" ? Model.errorText(reply.code) : "";
    }

    function started(cardKey) {
        return reply => {
            report(reply);
            if (reply.type === "ok") service.tracker.started(reply.result, cardKey);
        };
    }

    // `input`: the focused text field, or null.
    function run(effect, input) {
        switch (effect.kind) {
        case "pick":
            service.send("pick", { card: effect.cardKey, request: effect.requestId, option: effect.optionId, revision: effect.revision }, started(effect.cardKey));
            break;
        case "focusAnswer":
            answerEditor = Model.answerEditor(cardFor(effect.cardKey));
            break;
        case "submitReply":
            if (submitReply(effect.cardKey, input.text)) input.text = "";
            break;
        case "submitAnswer":
            if (submitAnswer(input.text)) input.text = "";
            break;
        case "submitPairing":
            submitPairing();
            break;
        case "openReply":
            keyState = Object.assign({}, keyState, { replyFor: effect.cardKey });
            break;
        case "markSeen":
            service.send("mark_seen", { card: effect.cardKey }, report);
            break;
        case "escape":
            // This panel's newest countdown, else close (spec §6).
            service.send("escape", {}, reply => {
                if (reply.type !== "ok" || reply.result === false) close();
            });
            break;
        }
    }

    function submitReply(cardKey, text) {
        const r = Model.validateReply(text);
        if (!r.ok) {
            message = Model.reasonText(r.reason);
            return false;
        }
        service.send("reply", { card: cardKey, text: r.text }, started(cardKey));
        const card = cardFor(cardKey);
        if (card) drafts.clear(Model.draftKey.reply(card));
        keyState = Object.assign({}, keyState, { replyFor: null });
        return true;
    }

    // Sends to the request the editor was opened for, never the card's
    // current one; the service refuses it if that request moved on.
    function submitAnswer(text) {
        const editor = answerEditor;
        if (!Model.answerEditorValid(editor, view ? view.cards : [])) {
            answerEditor = null;
            message = Model.EDITOR_CLOSED_TEXT;
            return false;
        }
        const r = Model.validateAnswer(text);
        if (!r.ok) {
            message = Model.reasonText(r.reason);
            return false;
        }
        service.send("answer_text", { card: editor.cardKey, request: editor.requestId, revision: editor.revision, text: r.text }, started(editor.cardKey));
        drafts.clear(Model.draftKey.answer(editor));
        answerEditor = null;
        return true;
    }

    // Every key goes through model.mjs: `field` says which text field has
    // focus, and `composing` is that field's IME state (spec §7).
    function keyEvent(event, field, composing, input) {
        const r = Model.onKey(visibleCards, keyState, Model.fromQt(event.key, event.text, event.modifiers, composing, field));
        keyState = r.state;
        if (r.effect.kind !== "none") {
            run(r.effect, input);
            event.accepted = true;
        }
    }

    // A changed request closes its typed-answer editor; the text is dropped.
    onViewChanged: {
        if (answerEditor !== null && !Model.answerEditorValid(answerEditor, view ? view.cards : [])) {
            answerEditor = null;
            message = Model.EDITOR_CLOSED_TEXT;
        }
        // No view (disconnected, overflow) is not a reason to drop drafts.
        if (view !== null) drafts.prune(view, service.settings);
    }

    // The pairing line is a secret: cleared before it is sent, after every
    // attempt; never logged; errors never repeat it (spec §7).
    function submitPairing() {
        const line = pairingField.text;
        pairingField.text = "";
        if (line.trim() === "") return;
        service.send("pair", { pairing: line }, report);
    }

    Connections {
        target: panel.service

        function onOutcomesLost(cards) {
            const u = Object.assign({}, panel.uncertain);
            for (const c of cards) u[c] = true;
            panel.uncertain = u;
        }

        // The service forgets a panel's state when the connection drops.
        function onReady() {
            panel.service.send("panel", { open: panel.shown });
        }
    }

    Timer {
        interval: 100
        repeat: true
        running: panel.shown && panel.view !== null && panel.view.actions.length > 0
        onTriggered: panel.now = Date.now()
    }

    Keys.onPressed: event => panel.keyEvent(event, null, false, null)

    component CardItem: Rectangle {
        id: cardItem

        required property var card
        readonly property var meta: Model.cardMeta(card)
        readonly property var rows: Model.optionRows(card)
        readonly property bool selected: panel.keyState.selected === card.key

        width: parent ? parent.width : 0
        implicitHeight: body.implicitHeight + Commons.Style.spacing * 2
        radius: Commons.Style.radius
        color: selected ? Commons.Color.surfaceActive : Commons.Color.surface

        ColumnLayout {
            id: body
            anchors.fill: parent
            anchors.margins: Commons.Style.spacing
            spacing: Commons.Style.spacingSmall

            Text {
                Layout.fillWidth: true
                elide: Text.ElideRight
                color: Commons.Color.dim
                text: [cardItem.meta.kind, cardItem.meta.machine, cardItem.meta.project, cardItem.meta.badge].filter(x => x).join(" · ")
            }

            Text {
                Layout.fillWidth: true
                elide: Text.ElideRight
                color: Commons.Color.text
                text: cardItem.meta.title
            }

            Text {
                visible: cardItem.card.agent.pending !== null
                Layout.fillWidth: true
                wrapMode: Text.Wrap
                maximumLineCount: 6
                color: Commons.Color.text
                text: cardItem.card.agent.pending ? cardItem.meta.tag + "  " + cardItem.card.agent.pending.detail : ""
            }

            Repeater {
                model: cardItem.rows

                delegate: Text {
                    required property var modelData
                    Layout.fillWidth: true
                    wrapMode: Text.Wrap
                    color: modelData.pickable ? Commons.Color.text : Commons.Color.dim
                    text: modelData.number + ". " + modelData.label + (modelData.rule ? "  (" + modelData.rule + ")" : "")

                    MouseArea {
                        anchors.fill: parent
                        enabled: modelData.pickable
                        onClicked: panel.run({ kind: "pick", cardKey: cardItem.card.key, requestId: cardItem.card.agent.pending.id, optionId: modelData.id, revision: cardItem.card.agent.pending.revision }, null)
                    }
                }
            }

            // Countdown: an accent bar with "Esc to undo" and a cancel control
            // that cancels the action whichever view started it (spec §6).
            RowLayout {
                visible: cardItem.card.action !== null
                Layout.fillWidth: true

                Rectangle {
                    Layout.fillWidth: true
                    implicitHeight: Commons.Style.spacingSmall
                    color: Commons.Color.surface

                    Rectangle {
                        height: parent.height
                        width: cardItem.card.action ? parent.width * Model.countdownFraction(cardItem.card.action, panel.now) : 0
                        color: Commons.Color.accent
                    }
                }

                Text {
                    color: Commons.Color.dim
                    text: "Esc to undo"
                }

                Button {
                    text: "Cancel"
                    onClicked: panel.service.send("cancel", { action: cardItem.card.action.id }, panel.report)
                }
            }

            Text {
                visible: cardItem.card.notice !== null || panel.uncertain[cardItem.card.key] === true
                Layout.fillWidth: true
                wrapMode: Text.Wrap
                color: Commons.Color.urgent
                text: cardItem.card.notice ? cardItem.card.notice.text : Model.UNCERTAIN_TEXT
            }

            TextField {
                id: replyField
                readonly property string draft: Model.draftKey.reply(cardItem.card)
                visible: panel.keyState.replyFor === cardItem.card.key
                Layout.fillWidth: true
                placeholderText: "Reply"
                focus: visible
                Component.onCompleted: text = panel.drafts.get(draft)
                onTextChanged: panel.drafts.set(draft, text)
                Keys.onPressed: event => panel.keyEvent(event, { kind: "reply", cardKey: cardItem.card.key }, inputMethodComposing, replyField)
            }

            TextField {
                id: answerField
                readonly property string draft: panel.answerEditor !== null ? Model.draftKey.answer(panel.answerEditor) : ""
                visible: panel.answerEditor !== null && panel.answerEditor.cardKey === cardItem.card.key
                Layout.fillWidth: true
                placeholderText: "Answer"
                focus: visible
                Component.onCompleted: if (visible) text = panel.drafts.get(draft)
                onVisibleChanged: if (visible) text = panel.drafts.get(draft)
                onTextChanged: if (visible && draft !== "") panel.drafts.set(draft, text)
                Keys.onPressed: event => panel.keyEvent(event, { kind: "answer", cardKey: cardItem.card.key }, inputMethodComposing, answerField)
            }
        }

        MouseArea {
            anchors.fill: parent
            z: -1
            onClicked: panel.keyState = Object.assign({}, panel.keyState, { selected: cardItem.card.key })
        }
    }

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: Commons.Style.spacing
        spacing: Commons.Style.spacing

        RowLayout {
            Layout.fillWidth: true

            Text {
                Layout.fillWidth: true
                color: Commons.Color.text
                text: panel.view ? Model.headerText(panel.view) : "Inbox"
            }

            Button {
                text: panel.settingsOpen ? "Inbox" : "Settings"
                onClicked: panel.settingsOpen = !panel.settingsOpen
            }
        }

        Text {
            visible: !panel.service.connected
            Layout.fillWidth: true
            wrapMode: Text.Wrap
            color: Commons.Color.urgent
            text: Model.errorText("unreachable")
        }

        Text {
            visible: panel.message !== ""
            Layout.fillWidth: true
            wrapMode: Text.Wrap
            color: Commons.Color.urgent
            text: panel.message
        }

        // The service could not fit even the Needs-you cards (plan D9).
        Text {
            visible: !panel.settingsOpen && panel.service.overflow >= 0
            Layout.fillWidth: true
            wrapMode: Text.Wrap
            color: Commons.Color.urgent
            text: Model.overflowText(panel.service.overflow)
        }

        Text {
            visible: !panel.settingsOpen && Model.omittedText(panel.service.omitted) !== ""
            Layout.fillWidth: true
            color: Commons.Color.dim
            text: Model.omittedText(panel.service.omitted)
        }

        Flow {
            visible: !panel.settingsOpen
            Layout.fillWidth: true
            spacing: Commons.Style.spacingSmall

            Repeater {
                model: panel.chips

                delegate: Button {
                    required property var modelData
                    text: modelData.name + " · " + modelData.label
                    checkable: true
                    checked: panel.machineFilter === modelData.key
                    onClicked: panel.machineFilter = checked ? modelData.key : ""
                }
            }
        }

        Flickable {
            visible: !panel.settingsOpen
            Layout.fillWidth: true
            Layout.fillHeight: true
            clip: true
            contentHeight: sectionsColumn.implicitHeight

            Column {
                id: sectionsColumn
                width: parent.width
                spacing: Commons.Style.spacing

                Text { visible: panel.groups.needs.length > 0; color: Commons.Color.dim; text: "Needs you" }
                Repeater { model: panel.groups.needs; delegate: CardItem { required property var modelData; card: modelData } }

                Text { visible: panel.groups.working.length > 0; color: Commons.Color.dim; text: "Working" }
                Repeater { model: panel.groups.working; delegate: CardItem { required property var modelData; card: modelData } }

                Text {
                    visible: panel.groups.done.length > 0
                    color: Commons.Color.dim
                    text: (panel.doneOpen ? "▾ " : "▸ ") + "Done (" + panel.groups.done.length + ")"
                    MouseArea { anchors.fill: parent; onClicked: panel.doneOpen = !panel.doneOpen }
                }
                Repeater { model: panel.doneOpen ? panel.groups.done : []; delegate: CardItem { required property var modelData; card: modelData } }
            }
        }

        // Settings (spec §7): machines, the three show toggles, notifications.
        ColumnLayout {
            visible: panel.settingsOpen
            Layout.fillWidth: true
            spacing: Commons.Style.spacing

            Button {
                visible: Model.keyringLocked(panel.service.settings)
                text: "Unlock keyring"
                onClicked: panel.service.send("unlock_secrets", {}, panel.report)
            }

            Repeater {
                model: panel.service.settings ? panel.service.settings.machines : []

                delegate: RowLayout {
                    required property var modelData
                    Layout.fillWidth: true

                    TextField {
                        id: nameField
                        readonly property string draft: Model.draftKey.rename(modelData.key)
                        Layout.fillWidth: true
                        Component.onCompleted: text = panel.drafts.get(draft) || modelData.name
                        onTextChanged: panel.drafts.set(draft, text === modelData.name ? "" : text)
                        Keys.onPressed: event => panel.keyEvent(event, { kind: "rename", cardKey: null }, inputMethodComposing, nameField)
                        onAccepted: {
                            panel.drafts.clear(draft);
                            panel.service.send("rename_machine", { machine: modelData.key, name: text }, panel.report);
                        }
                    }

                    Text {
                        color: Commons.Color.dim
                        text: (panel.chips.find(c => c.key === modelData.key) ?? { label: "" }).label
                    }

                    Button {
                        text: "Retry"
                        onClicked: panel.service.send("retry_machine", { machine: modelData.key }, panel.report)
                    }

                    Button {
                        text: "Remove"
                        enabled: modelData.endpoint.kind === "tcp"
                        onClicked: panel.service.send("remove_machine", { machine: modelData.key }, panel.report)
                    }
                }
            }

            RowLayout {
                Layout.fillWidth: true

                TextField {
                    id: pairingField
                    Layout.fillWidth: true
                    echoMode: TextInput.Password
                    placeholderText: "Paste the line from `acd pair`"
                    Keys.onPressed: event => panel.keyEvent(event, { kind: "pairing", cardKey: null }, inputMethodComposing, pairingField)
                }

                Button {
                    text: "Add"
                    onClicked: panel.submitPairing()
                }
            }

            Switch {
                text: "Show idle agents"
                checked: panel.service.settings ? panel.service.settings.show_idle : false
                onToggled: panel.service.send("set_show", { what: "idle", on: checked }, panel.report)
            }

            Switch {
                text: "Show subagents"
                checked: panel.service.settings ? panel.service.settings.show_subagents : false
                onToggled: panel.service.send("set_show", { what: "subagents", on: checked }, panel.report)
            }

            Switch {
                text: "Show hook-only agents"
                checked: panel.service.settings ? panel.service.settings.show_hook_only : false
                onToggled: panel.service.send("set_show", { what: "hook_only", on: checked }, panel.report)
            }

            Switch {
                text: "Notifications"
                checked: panel.service.settings ? panel.service.settings.notifications : true
                onToggled: panel.service.send("set_notifications", { on: checked }, panel.report)
            }
        }
    }
}
```

- [ ] **Step 4: Run the checks to verify they pass**

Run: `node --test 'clients/omarchy/tests/*.test.mjs'`
Expected: PASS, every test in the three files.

- [ ] **Step 5: Commit**

```bash
git add clients/omarchy
git commit -m "Omarchy plugin: panel with model-driven keys and bound typed answers"
```

---

### Task 20: install, uninstall, status

**Execution:** cloud-ok (a fake runner; nothing touches systemd or the shell). The command names, plugin directory and bindings file below follow Task 16's note; where it says "differs", use the recorded value.

**Files:**
- Create: `crates/acd-inbox/src/install.rs`, `crates/acd-inbox/tests/install.rs`
- Modify: `crates/acd-inbox/src/lib.rs` (`pub mod install;`), `crates/acd-inbox/src/main.rs` (three subcommands)

**Interfaces:**
- Consumes: the plugin files of Tasks 17 to 19 (embedded, D21); Task 16's research note for the shell commands, plugin directory and bindings file; `prefs::Paths` (socket path); the `hello` and `settings` lines (Tasks 4, 9).
- Produces (in `acd_inbox::install`):
  - Constants: `UNIT_NAME = "acd-inbox.service"`, `PLUGIN_ID`, `MARKER`, `KEYBINDING` (D22), `PLUGIN_FILES: &[(&str, &str)]`.
  - `pub trait Runner { fn run(&self, program: &str, args: &[&str]) -> anyhow::Result<()>; }` and `pub struct SystemRunner`.
  - `pub struct Layout { pub home: PathBuf, pub config: PathBuf }`, with `from_env()`, `bin()`, `unit()`, `plugin_dir()`, `bindings()`.
  - `pub fn unit_text(exe: &Path) -> String`.
  - `pub fn install(l: &Layout, exe: &Path, runner: &dyn Runner, ask: &mut dyn FnMut(&str) -> bool, out: &mut dyn Write) -> anyhow::Result<()>`.
  - `pub fn offer_keybinding(path: &Path, ask: &mut dyn FnMut(&str) -> bool, out: &mut dyn Write, stamp: u64) -> anyhow::Result<bool>`.
  - `pub fn uninstall(l: &Layout, runner: &dyn Runner, out: &mut dyn Write) -> anyhow::Result<()>`.
  - `pub struct Probe { pub protocol: u64, pub service: String, pub locked: usize }` and `pub fn probe(socket: &Path, timeout: Duration) -> anyhow::Result<Probe>`.
  - `pub fn status(l: &Layout, socket: &Path, runner: &dyn Runner, out: &mut dyn Write) -> anyhow::Result<()>`.
  - `pub fn run_install() / run_uninstall() / run_status() -> anyhow::Result<()>`, the CLI entry points (Linux only).

- [ ] **Step 1: Write the failing tests**

`crates/acd-inbox/tests/install.rs`:

```rust
mod common;

use std::cell::RefCell;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use acd_inbox::install::*;
use anyhow::{Result, bail};
use common::*;

#[derive(Default)]
struct Recorder {
    calls: RefCell<Vec<String>>,
    fail: Option<&'static str>,
}

impl Runner for Recorder {
    fn run(&self, program: &str, args: &[&str]) -> Result<()> {
        let call = format!("{program} {}", args.join(" "));
        self.calls.borrow_mut().push(call.clone());
        if self.fail.is_some_and(|f| call.contains(f)) {
            bail!("{call} failed");
        }
        Ok(())
    }
}

fn layout(home: &Path) -> Layout {
    Layout { home: home.to_path_buf(), config: home.join(".config") }
}

fn build(dir: &Path) -> std::path::PathBuf {
    let exe = dir.join("acd-inbox-build");
    std::fs::write(&exe, "binary").unwrap();
    exe
}

#[test]
fn the_unit_follows_the_graphical_session_and_quotes_the_path() {
    let unit = unit_text(Path::new("/home/u/50%/a$b/.local/bin/acd-inbox"));
    assert!(unit.contains("ExecStart=\"/home/u/50%%/a$$b/.local/bin/acd-inbox\" serve"));
    for line in [
        "PartOf=graphical-session.target",
        "After=graphical-session.target",
        "WantedBy=graphical-session.target",
        "Restart=on-failure",
        "RestartSec=2",
    ] {
        assert!(unit.lines().any(|l| l == line), "{line}");
    }
    assert!(unit.starts_with(&format!("# {MARKER}")));
}

#[test]
fn install_copies_the_binary_writes_the_unit_and_plugin_and_runs_the_steps_in_order() {
    let home = tempfile::tempdir().unwrap();
    let l = layout(home.path());
    let runner = Recorder::default();
    let mut asked = Vec::new();
    let mut out = Vec::new();
    install(&l, &build(home.path()), &runner, &mut |q| { asked.push(q.to_owned()); false }, &mut out).unwrap();
    assert_eq!(std::fs::read_to_string(l.bin()).unwrap(), "binary");
    assert_eq!(std::fs::metadata(l.bin()).unwrap().permissions().mode() & 0o777, 0o755);
    assert_eq!(std::fs::read_to_string(l.unit()).unwrap(), unit_text(&l.bin()));
    for (name, text) in PLUGIN_FILES {
        assert_eq!(std::fs::read_to_string(l.plugin_dir().join(name)).unwrap(), *text, "{name}");
    }
    assert_eq!(
        *runner.calls.borrow(),
        [
            "systemctl --user daemon-reload",
            "systemctl --user enable --now acd-inbox.service",
            "systemctl --user restart acd-inbox.service",
            "omarchy-shell shell rescanPlugins",
            "omarchy plugin enable tonibergholm.agentcaller",
        ]
    );
    let printed = String::from_utf8(out).unwrap();
    assert!(printed.contains(KEYBINDING));
    assert!(asked.is_empty(), "no bindings file, so nothing to ask");
}

#[test]
fn a_unit_install_did_not_write_is_never_replaced() {
    let home = tempfile::tempdir().unwrap();
    let l = layout(home.path());
    std::fs::create_dir_all(l.unit().parent().unwrap()).unwrap();
    std::fs::write(l.unit(), "[Service]\nExecStart=/mine\n").unwrap();
    let runner = Recorder::default();
    assert!(install(&l, &build(home.path()), &runner, &mut |_| false, &mut Vec::new()).is_err());
    assert_eq!(std::fs::read_to_string(l.unit()).unwrap(), "[Service]\nExecStart=/mine\n");
    assert!(runner.calls.borrow().is_empty());
}

#[test]
fn the_keybinding_changes_nothing_without_a_yes_and_keeps_a_backup() {
    let d = tempfile::tempdir().unwrap();
    let bindings = d.path().join("bindings.conf");
    std::fs::write(&bindings, "bindd = SUPER, RETURN, Terminal, exec, $terminal").unwrap();
    let before = std::fs::read_to_string(&bindings).unwrap();
    assert!(!offer_keybinding(&bindings, &mut |_| false, &mut Vec::new(), 1).unwrap());
    assert_eq!(std::fs::read_to_string(&bindings).unwrap(), before);
    assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1, "no backup without a yes");
    assert!(offer_keybinding(&bindings, &mut |_| true, &mut Vec::new(), 2).unwrap());
    let after = std::fs::read_to_string(&bindings).unwrap();
    assert!(after.starts_with(&before));
    assert_eq!(after.lines().filter(|l| *l == KEYBINDING).count(), 1);
    let backup = d.path().join("bindings.conf.acd-inbox-2.bak");
    assert_eq!(std::fs::read_to_string(backup).unwrap(), before);
    let mut asked = false;
    assert!(!offer_keybinding(&bindings, &mut |_| { asked = true; true }, &mut Vec::new(), 3).unwrap());
    assert!(!asked, "an existing binding is not offered again");
    assert!(!offer_keybinding(&d.path().join("missing.conf"), &mut |_| true, &mut Vec::new(), 4).unwrap());
    assert!(!d.path().join("missing.conf").exists());
}

#[test]
fn uninstall_reverses_install_and_prints_the_binding_to_remove() {
    let home = tempfile::tempdir().unwrap();
    let l = layout(home.path());
    install(&l, &build(home.path()), &Recorder::default(), &mut |_| false, &mut Vec::new()).unwrap();
    let runner = Recorder::default();
    let mut out = Vec::new();
    uninstall(&l, &runner, &mut out).unwrap();
    assert!(!l.bin().exists() && !l.unit().exists() && !l.plugin_dir().exists());
    assert!(runner.calls.borrow().contains(&"systemctl --user disable --now acd-inbox.service".to_owned()));
    assert!(String::from_utf8(out).unwrap().contains(KEYBINDING));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_reports_service_socket_keyring_and_plugin() {
    let rig = rig(vec![pending_agent("a1", "r1")], fast()).await;
    let home = tempfile::tempdir().unwrap();
    let l = layout(home.path());
    let socket = rig.socket.clone();
    let printed = tokio::task::spawn_blocking(move || {
        let mut out = Vec::new();
        status(&l, &socket, &Recorder::default(), &mut out).unwrap();
        String::from_utf8(out).unwrap()
    })
    .await
    .unwrap();
    assert!(printed.contains("service: running"), "{printed}");
    assert!(printed.contains("answers (protocol 1"), "{printed}");
    assert!(printed.contains("keyring: ok"), "{printed}");
    assert!(printed.contains("plugin: missing"), "{printed}");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Add `pub mod install;` to `lib.rs`.
Run: `cargo test -p acd-inbox --locked --test install`
Expected: compile error, `could not find install in acd_inbox`.

- [ ] **Step 3: Implement `install.rs`**

```rust
//! `acd-inbox install | uninstall | status` (omarchy spec §10).
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};

pub const UNIT_NAME: &str = "acd-inbox.service";
pub const PLUGIN_ID: &str = "tonibergholm.agentcaller";
pub const MARKER: &str = "Written by acd-inbox install";
/// Super+Alt+A (spec §10, plan D22).
pub const KEYBINDING: &str = "bindd = SUPER ALT, A, Agent inbox, exec, omarchy-shell tonibergholm.agentcaller toggle";

/// The plugin, embedded so install needs no checkout (plan D21).
pub const PLUGIN_FILES: &[(&str, &str)] = &[
    ("manifest.json", include_str!("../../../clients/omarchy/manifest.json")),
    ("BarWidget.qml", include_str!("../../../clients/omarchy/BarWidget.qml")),
    ("Panel.qml", include_str!("../../../clients/omarchy/Panel.qml")),
    ("Service.qml", include_str!("../../../clients/omarchy/Service.qml")),
    ("model.mjs", include_str!("../../../clients/omarchy/model.mjs")),
];

pub trait Runner {
    fn run(&self, program: &str, args: &[&str]) -> Result<()>;
}

/// Runs a program with its output captured, so probes stay quiet.
pub struct SystemRunner;

impl Runner for SystemRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<()> {
        let out = std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .with_context(|| format!("running {program}"))?;
        if !out.status.success() {
            bail!("{program} {} failed ({})", args.join(" "), out.status);
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct Layout {
    pub home: PathBuf,
    pub config: PathBuf,
}

impl Layout {
    pub fn from_env() -> Result<Layout> {
        let home = std::env::var_os("HOME").map(PathBuf::from).context("HOME is not set")?;
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        Ok(Layout { home, config })
    }
    pub fn bin(&self) -> PathBuf {
        self.home.join(".local/bin/acd-inbox")
    }
    pub fn unit(&self) -> PathBuf {
        self.config.join("systemd/user").join(UNIT_NAME)
    }
    pub fn plugin_dir(&self) -> PathBuf {
        self.config.join("omarchy/plugins").join(PLUGIN_ID)
    }
    pub fn bindings(&self) -> PathBuf {
        self.config.join("hypr/bindings.conf")
    }
}

/// Quoted for systemd: `%` specifiers and `$` expansion escaped.
fn unit_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"").replace('%', "%%").replace('$', "$$"))
}

/// The user unit (spec §4.2).
pub fn unit_text(exe: &Path) -> String {
    format!(
        "# {MARKER}\n[Unit]\nDescription=agent-caller inbox (acd-inbox)\nPartOf=graphical-session.target\nAfter=graphical-session.target\n\n[Service]\nExecStart={} serve\nRestart=on-failure\nRestartSec=2\n\n[Install]\nWantedBy=graphical-session.target\n",
        unit_quote(&exe.to_string_lossy())
    )
}

fn write_file(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let dir = path.parent().context("the path has no parent")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let tmp = path.with_extension("acd-inbox-new");
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))
}

fn ours(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|t| t.contains(MARKER))
}

/// Steps 1 to 3 of spec §10.
pub fn install(
    l: &Layout,
    exe: &Path,
    runner: &dyn Runner,
    ask: &mut dyn FnMut(&str) -> bool,
    out: &mut dyn Write,
) -> Result<()> {
    let bin = l.bin();
    let unit = l.unit();
    if unit.exists() && !ours(&unit) {
        bail!("{} exists and was not written by acd-inbox; remove it first", unit.display());
    }
    let same = std::fs::canonicalize(exe).ok().is_some_and(|e| std::fs::canonicalize(&bin).ok() == Some(e));
    if !same {
        let bytes = std::fs::read(exe).with_context(|| format!("reading {}", exe.display()))?;
        write_file(&bin, &bytes, 0o755)?;
        writeln!(out, "Installed {}", bin.display())?;
    }
    write_file(&unit, unit_text(&bin).as_bytes(), 0o644)?;
    runner.run("systemctl", &["--user", "daemon-reload"])?;
    runner.run("systemctl", &["--user", "enable", "--now", UNIT_NAME])?;
    // enable --now does not restart a running service (plan D27).
    runner.run("systemctl", &["--user", "restart", UNIT_NAME])?;
    writeln!(out, "Started {UNIT_NAME}")?;
    let dir = l.plugin_dir();
    for (name, text) in PLUGIN_FILES {
        write_file(&dir.join(name), text.as_bytes(), 0o644)?;
    }
    runner.run("omarchy-shell", &["shell", "rescanPlugins"])?;
    runner.run("omarchy", &["plugin", "enable", PLUGIN_ID])?;
    writeln!(out, "Installed the plugin in {}", dir.display())?;
    writeln!(out, "Keybinding for Super+Alt+A:\n{KEYBINDING}")?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    offer_keybinding(&l.bindings(), ask, out, stamp)?;
    Ok(())
}

/// Appends the binding only after a yes, keeping a backup (spec §10).
pub fn offer_keybinding(path: &Path, ask: &mut dyn FnMut(&str) -> bool, out: &mut dyn Write, stamp: u64) -> Result<bool> {
    let Ok(existing) = std::fs::read_to_string(path) else {
        writeln!(out, "{} does not exist; add the line above to your Hyprland bindings.", path.display())?;
        return Ok(false);
    };
    if existing.lines().any(|l| l.trim() == KEYBINDING) {
        writeln!(out, "The keybinding is already in {}.", path.display())?;
        return Ok(false);
    }
    if !ask(&format!("Append it to {}? A backup is kept. [y/N] ", path.display())) {
        writeln!(out, "{} was not changed.", path.display())?;
        return Ok(false);
    }
    let name = path.file_name().context("bindings file name")?.to_string_lossy();
    let backup = path.with_file_name(format!("{name}.acd-inbox-{stamp}.bak"));
    std::fs::copy(path, &backup).with_context(|| format!("backing up {}", path.display()))?;
    let mut f = std::fs::OpenOptions::new().append(true).open(path)?;
    let sep = if existing.is_empty() || existing.ends_with('\n') { "" } else { "\n" };
    write!(f, "{sep}\n# agent-caller inbox\n{KEYBINDING}\n")?;
    writeln!(out, "Added. Backup: {}", backup.display())?;
    Ok(true)
}

/// Reverses steps 1 and 2 of install; prints how to remove the binding.
pub fn uninstall(l: &Layout, runner: &dyn Runner, out: &mut dyn Write) -> Result<()> {
    let _ = runner.run("systemctl", &["--user", "disable", "--now", UNIT_NAME]);
    let unit = l.unit();
    if ours(&unit) {
        std::fs::remove_file(&unit)?;
    }
    let _ = runner.run("systemctl", &["--user", "daemon-reload"]);
    let _ = runner.run("omarchy", &["plugin", "disable", PLUGIN_ID]);
    let dir = l.plugin_dir();
    for (name, _) in PLUGIN_FILES {
        let _ = std::fs::remove_file(dir.join(name));
    }
    let _ = std::fs::remove_dir(&dir);
    let _ = runner.run("omarchy-shell", &["shell", "rescanPlugins"]);
    let bin = l.bin();
    if bin.exists() {
        std::fs::remove_file(&bin)?;
    }
    writeln!(out, "Removed {UNIT_NAME}, the plugin and {}.", bin.display())?;
    writeln!(out, "To remove the keybinding, delete this line from {}:\n{KEYBINDING}", l.bindings().display())?;
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub struct Probe {
    pub protocol: u64,
    pub service: String,
    pub locked: usize,
}

/// Connects as a view and reads `hello` and `settings`.
pub fn probe(socket: &Path, timeout: Duration) -> Result<Probe> {
    let s = std::os::unix::net::UnixStream::connect(socket).with_context(|| format!("connecting to {}", socket.display()))?;
    s.set_read_timeout(Some(timeout))?;
    let mut r = BufReader::new(s);
    let mut line = String::new();
    r.read_line(&mut line)?;
    let hello: serde_json::Value = serde_json::from_str(&line).context("reading hello")?;
    line.clear();
    r.read_line(&mut line)?;
    let settings: serde_json::Value = serde_json::from_str(&line).context("reading settings")?;
    let locked = settings["machines"]
        .as_array()
        .map(|m| m.iter().filter(|e| e["secret"] == "locked").count())
        .unwrap_or(0);
    Ok(Probe {
        protocol: hello["protocol"].as_u64().unwrap_or(0),
        service: hello["service"].as_str().unwrap_or("").to_owned(),
        locked,
    })
}

pub fn status(l: &Layout, socket: &Path, runner: &dyn Runner, out: &mut dyn Write) -> Result<()> {
    let active = runner.run("systemctl", &["--user", "is-active", "--quiet", UNIT_NAME]).is_ok();
    writeln!(out, "service: {}", if active { "running" } else { "not running" })?;
    match probe(socket, Duration::from_secs(2)) {
        Ok(p) => {
            writeln!(out, "socket: {} answers (protocol {}, service {})", socket.display(), p.protocol, p.service)?;
            if p.locked == 0 {
                writeln!(out, "keyring: ok")?;
            } else {
                writeln!(out, "keyring: locked ({} machines); unlock it from the panel's settings", p.locked)?;
            }
        }
        Err(_) => {
            writeln!(out, "socket: {} does not answer", socket.display())?;
            writeln!(out, "keyring: unknown")?;
        }
    }
    let plugin = l.plugin_dir().join("manifest.json").exists();
    writeln!(out, "plugin: {}", if plugin { "installed" } else { "missing" })?;
    Ok(())
}

/// A y/N question on the terminal; no terminal means no.
fn ask_on_terminal(question: &str) -> bool {
    // SAFETY: isatty only inspects the descriptor.
    if unsafe { libc::isatty(0) } != 1 {
        return false;
    }
    print!("{question}");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer).is_ok() && matches!(answer.trim(), "y" | "Y" | "yes")
}

fn linux_only(what: &str) -> Result<()> {
    if cfg!(target_os = "linux") { Ok(()) } else { bail!("acd-inbox {what} runs on Linux only") }
}

pub fn run_install() -> Result<()> {
    linux_only("install")?;
    let exe = std::env::current_exe()?;
    install(&Layout::from_env()?, &exe, &SystemRunner, &mut ask_on_terminal, &mut std::io::stdout())
}

pub fn run_uninstall() -> Result<()> {
    linux_only("uninstall")?;
    uninstall(&Layout::from_env()?, &SystemRunner, &mut std::io::stdout())
}

pub fn run_status() -> Result<()> {
    linux_only("status")?;
    let paths = crate::prefs::Paths::from_env()?;
    status(&Layout::from_env()?, &paths.socket, &SystemRunner, &mut std::io::stdout())
}
```

In `main.rs`, extend the subcommands:

```rust
#[derive(Subcommand)]
enum Command {
    /// Run the service (the systemd user unit starts this).
    Serve,
    /// Install the service, the shell plugin and, if you agree, a keybinding.
    Install,
    /// Remove the service and the plugin.
    Uninstall,
    /// Report the service, the socket, the plugin and the keyring.
    Status,
}

fn main() -> ExitCode {
    let result = match Cli::parse().command {
        Command::Serve => acd_inbox::serve::run(),
        Command::Install => acd_inbox::install::run_install(),
        Command::Uninstall => acd_inbox::install::run_uninstall(),
        Command::Status => acd_inbox::install::run_status(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("acd-inbox: {e:#}");
            ExitCode::FAILURE
        }
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p acd-inbox --locked --test install`
Expected: PASS.

Run: `cargo test --workspace --locked`
Expected: PASS.

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy --workspace --all-targets --locked -- -D warnings`
Expected: no warnings.

```bash
git add crates/acd-inbox
git commit -m "acd-inbox: install, uninstall and status with the embedded plugin and keybinding offer"
```

---

### Task 21: Build in Docker, install and validate on omarchy

**Execution:** local. It needs the Mac (Docker) and the omarchy machine, and the user answers the keyring and keybinding prompts. The controller runs it with the user. The shell API was settled in Task 16, so this task builds, installs and validates; it does not redesign.

**Files:**
- Modify: `justfile` (two recipes)
- Modify, only for gaps Step 2 finds: `clients/omarchy/*`, `crates/acd-inbox/src/install.rs`, their tests, and `docs/research/2026-10-12-omarchy-plugin-api.md`

**Interfaces:**
- Consumes: everything above.
- Produces:
  - An installed `acd-inbox` and plugin on omarchy.
  - Validation output from `omarchy-plugin-validate`, `qmllint` and `acd-inbox status`.

- [ ] **Step 1: Add the Docker recipes**

Append to `justfile`:

```just
# Build acd-inbox for Omarchy (x86_64 Linux) in Docker, as acd is built
# (docs/research/2026-10-11-e2e-run.md). Output:
# target/linux-amd64/release/acd-inbox. The Linux target dir is separate
# from the Mac's.
inbox-linux:
    docker run --rm --platform linux/amd64 -v "$PWD":/src -w /src -v acd-inbox-cargo:/usr/local/cargo/registry -e CARGO_TARGET_DIR=/src/target/linux-amd64 rust:1-bookworm cargo build --release --locked -p acd-inbox

# Copy the Linux build to a host and run its installer there.
inbox-install host="omarchy": inbox-linux
    scp target/linux-amd64/release/acd-inbox {{host}}:/tmp/acd-inbox
    ssh -t {{host}} /tmp/acd-inbox install
```

Run: `just inbox-linux && file target/linux-amd64/release/acd-inbox`
Expected: `ELF 64-bit LSB pie executable, x86-64`.

- [ ] **Step 2: Check the files against the research note**

Compare `clients/omarchy/*` and `install.rs` (commands, paths, `KEYBINDING`) with every "differs" item in `docs/research/2026-10-12-omarchy-plugin-api.md`. Each must already be applied (Tasks 17 to 20). A gap found here is a defect in those tasks: fix it in their files, keep their tests passing (`node --test 'clients/omarchy/tests/*.test.mjs'`, `cargo test -p acd-inbox --locked`), and note it in the research note.

- [ ] **Step 3: Install on omarchy**

Run: `just inbox-install omarchy`
Expected:
- The output shows "Installed …/acd-inbox", "Started acd-inbox.service", "Installed the plugin in …" and the keybinding line.
- A `[y/N]` question about the bindings file. The user decides; with `y`, a `.bak` file is next to it.

- [ ] **Step 4: Validate the plugin and the service**

On omarchy:

```bash
omarchy-plugin-validate ~/.config/omarchy/plugins/tonibergholm.agentcaller
qmllint ~/.config/omarchy/plugins/tonibergholm.agentcaller/*.qml   # with the -I paths omarchy.tailscale needs
acd-inbox status
journalctl --user -u acd-inbox -n 50 --no-pager
```

Expected:
- `omarchy-plugin-validate` passes; `qmllint` shows no errors.
- `status` prints `service: running`, `socket: … answers (protocol 1, …)`, `plugin: installed`.
- The journal shows `acd-inbox serving` and connection lines with tags and lengths only.
- The bar shows the glyph; Super+Alt+A (if added) toggles the panel.

Fix and repeat Steps 2 to 4 until these hold. A failure that needs a design change goes to the orchestrator.

- [ ] **Step 5: Commit**

```bash
git add justfile docs/research/2026-10-12-omarchy-plugin-api.md clients/omarchy crates/acd-inbox
git commit -m "Omarchy inbox: Docker build recipe; validated on omarchy"
```

---

### Task 22: Live checks on omarchy

**Execution:** local. It runs on the omarchy machine with a real Claude Code session, and with minos as the remote. The controller runs the steps with the user and records each result.

**Files:**
- Create: `docs/research/2026-10-12-omarchy-live.md`

**Interfaces:**
- Consumes: the installed service and plugin (Task 21); `acd` on omarchy and on minos; herdr with Claude Code on omarchy.
- Produces: a record of the five live checks of spec §12, each with date, commit, steps, observed result and evidence (journal lines, `acd-inbox status`, screenshots named in the note). A failed check is recorded as failed, with the follow-up it needs.

- [ ] **Step 1: Prepare and start the record**

On omarchy: `acd status` shows the daemon running; `acd-inbox status` shows the service running. Start a Claude Code session in herdr in a scratch repo.

Create `docs/research/2026-10-12-omarchy-live.md`:

```markdown
# Omarchy inbox: live checks

Date: <YYYY-MM-DD>. Commit: <sha>. Machines: omarchy (Omarchy 4), minos (macOS, remote).
Build: `just inbox-linux`, installed with `acd-inbox install`.

| # | Check | Result | Evidence |
|---|---|---|---|
| 1 | Answer from the panel; Esc undoes | | |
| 2 | Notification opens the card; none while open | | |
| 3 | Shell restart in a countdown sends nothing; service stop shows | | |
| 4 | Pair minos; keyring after restart; locked keyring unlocks | | |
| 5 | A minos request appears and is answerable | | |

## Notes
```

- [ ] **Step 2: Check 1. Answer a real permission request; Esc undoes**

1. In the Claude session, ask for a command that needs approval (for example "run `ls /` with Bash").
2. Open the panel with Super+Alt+A. Press 1: the countdown bar shows "Esc to undo". Press Esc within 2 s: the bar goes away; the terminal dialog is still open.
3. Press 1 again and wait: the terminal dialog closes as answered.

Expected: the first try sends nothing and the second answers. `journalctl --user -u acd-inbox` shows no agent text.

- [ ] **Step 3: Check 2. A notification opens the panel on the card; none while it is open**

1. With the panel closed, trigger another approval. A notification "… on omarchy needs you" with an "Open" action appears. Click Open: the panel opens on that card.
2. Keep the panel open and trigger another approval. No notification appears.

- [ ] **Step 4: Check 3. Shell restart during a countdown; service stop**

1. Start a countdown (press 1) and within 2 s restart the shell (`omarchy-shell` restart or its keybinding).
   Expected: the terminal dialog is still open, so nothing was sent. After the shell is back, the bar reconnects and the panel shows the request again.
2. Run `systemctl --user stop acd-inbox`.
   Expected: the bar glyph is struck through and the panel says the service is not running and how to start it.
3. Run `systemctl --user start acd-inbox`.
   Expected: the bar recovers within the backoff (at most 10 s).

- [ ] **Step 5: Check 4. Pair minos, the keyring after a restart, and unlock**

1. On minos, run `acd pair` and copy the line. In the panel's settings, paste it and press Add.
   Expected: the field clears; minos appears with status Online.
2. Run `systemctl --user restart acd-inbox`.
   Expected: minos reconnects without re-pairing. `secret-tool search service agent-caller` lists one item with `machine=<key>`.
3. Lock the keyring (`secret-tool lock --collection=login`, or the desktop's lock), then restart the service.
   Expected: the minos chip says "Keyring locked" (not "Not paired"), and settings shows "Unlock keyring".
4. Press "Unlock keyring", then enter the password in the prompt.
   Expected: minos reconnects without a restart or re-pairing.
5. Lock again, restart, press "Unlock keyring" and dismiss the prompt.
   Expected: the panel says the keyring is locked, and nothing else changes.

- [ ] **Step 6: Check 5. A minos request is answerable**

On minos, start Claude Code under herdr and trigger an approval.
Expected: the card appears in the panel's "Needs you" section with machine minos and can be answered with 1.

- [ ] **Step 7: Record and commit**

Fill in the table and notes. Then:

```bash
git add docs/research/2026-10-12-omarchy-live.md
git commit -m "Omarchy inbox: live check record"
```

A failed check blocks merge. Record it, with its evidence, as a follow-up in `docs/superpowers/plans/2026-10-07-p1-followups.md`, and report it to the orchestrator.

