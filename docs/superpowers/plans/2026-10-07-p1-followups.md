# Known gaps and deferred work

Reviewed against `main` on 2026-11-04 (merged through PR #103). Items are
grouped by area. Completed task history is in
[the archived review ledger](../../archive/p1-followups-2026-10.md); phase
plans and live-run records keep the original evidence.

Every entry here is open or an accepted residual. Entries that were fixed are
gone; "Removed as fixed" at the end lists them with the PR. **OWNER-undecided**
marks a decision only the owner can make: those are not accepted residuals.

## Owner decisions still open

- **Same-user local socket starvation.** A process of the same user can hold
  all 8 local pre-auth slots (open `acd.sock`, never send `hello`) until the
  5 s hello timeout, and reconnect. Another UID cannot reach the socket (mode
  0600, `crates/acd/src/local_socket.rs`). Argument for accepting: a process
  running as the user can already read the token and answer requests. Decide
  whether a per-peer-credential cap is wanted. (PR #82.)
- **Four-source pre-auth.** Four or more distinct source IPs (four addresses
  at 2 slots each) can still fill all 8 network pre-auth slots. No status yet.
  A hostile process sharing a source IP with a paired client can take that
  client's 2 slots.
- **Relay lock-screen content and handle revocation.** The relay spec's
  lock-screen content rule and stolen push address and handle revocation are
  open (see the spec's review status).
- **T3 token, local F2.** The T3 bearer token goes to whoever holds the
  loopback T3 port. The review's ticket fix does not close it: the ticket
  request `POST /api/auth/websocket-ticket` itself needs `Authorization:
  Bearer <t3.token>` over plain HTTP to the same port, so a squatter receives
  the long-lived token there instead of on the upgrade. The endpoint is
  confirmed (`docs/research/2026-10-08-t3-gate3.md` §1,
  `tests/fixtures/t3/http_websocket_ticket_200.json`). A real fix has to prove
  who holds the port first, for example the listener's owner uid from
  `/proc/net/tcp` on Linux, or a narrower and shorter-lived T3 token.
  Source: `docs/research/2026-11-03-security-review-local.md`.
- **Show idle is not saved.** Persisting it changes shared client behaviour;
  decide for both clients (see Omarchy).
- **Pairing a literal non-Tailscale address** says "That is not an acd pairing
  line" on Omarchy. A specific message needs a new wire code, which is a
  protocol decision (see Omarchy).

## Core, protocol and client

- **Oversized frame before `welcome` (open; PR #105 fixes it).** After an
  established session drops, a peer can send a frame over 1 MiB before the
  next `welcome`. `next_server()` in `crates/acd-client/src/conn.rs` maps that
  capacity error to `End::Offline`, so the following connection still sends
  the old resume cursor. Memory stays bounded; only the documented resync does
  not happen on that path. An oversized frame after `welcome` is already a
  resync (`serve`, `conn.rs`).
- **Core 3, 7 and 8 limits (as built).** The client limits frames to 1 MiB,
  1000 agents and 64 KiB per agent; each breach, an oversized frame included,
  ends as a resync without the resume cursor. The daemon registry is fed by
  local adapters, not by peers, so it has no new cap. Pairing accepts ASCII
  hostnames only; IDNA names must be paired as punycode.
- **Core 4 logging.** `hello` outcomes are logged at 10 lines per 10 s per
  kind: peer address, protocol, outcome and reason. Peer free text is never
  logged; client id and name appear only as byte length plus an 8-hex SHA-256
  prefix. Lines over the limit are counted into `suppressed` on the next line.
  TLS handshake failures and pre-auth slot refusals are not logged.
- **Core 5 notifications.** The client's notified set is bounded (256) and
  notifications are limited to 5 per 10 s per machine; a removed machine's
  rate-limit entry is dropped. Left open: it still notifies for a request
  whose agent is not in the snapshot, because the daemon sends
  `request.opened` before the agent update (tested in
  `hook_only_agents_show_with_a_request_and_notify_as_before`); the rate limit
  is the bound on that lure. A request first seen in a reconnect snapshot
  stays silent (#81).
- **TLS listener `ended()` is single-shot.** A second call returns a future
  that never resolves, and `TlsListener::accept` now waits forever on a dead
  accept task. The daemon calls `ended()` once, before the listener moves, so
  it cannot wedge today. Other `TlsListener::new` callers (tests) never select
  on it. Make the signal cloneable (`watch`) or document it on `accept`.
- **Startup path-check ordering.** `check_path(hook.sock)` runs before the
  hook endpoint bind; no test pins which error wins when the hook port is
  taken and `hook.sock` is invalid. An existing socket path is probed twice
  (check, then bind), up to about 2 s per path.
- **Import check.** `endpoint::refuses_literal` (shared by both clients)
  refuses only canonical literal addresses. Decimal (`3232235786`), hex
  (`0xC0A8010A`) and zone-scoped (`fe80::1%lo0`) forms pass import and are
  refused at connect by `endpoint::vet`. `endpoint.rs` pins this, so a change
  here must update that test and this entry.
- **Optional hardening:** `tailscale whois` node validation.
- **Review residual, Task 4.** `KeysTargets::remove` retires ids even when no
  live target exists, so callers must not remove before setting; watch for a
  self-deadlock if a P3 holder re-enters `deliver` for the same agent.
- **Review residual, Task 5.** No test pins `lost()` clearing `closed`.
- **Review residual, Task 7.** Strong RTL characters in command lines are not
  escaped (natural RTL letters reorder the display), and herdr or T3 details
  may start lines with spaces. Escape strong RTL characters or document the
  residual.

## Security review residuals (2026-11-03 reviews)

Findings still open or partly open (`docs/research/2026-11-03-security-review-core.md`
and `-local.md`). Core 1 to 8 and local F1, F3, F4 and F5 are fixed; local F2
is open (see owner decisions).

- **F3 residual (hook listeners, #87).** A slow body after the token check is
  bounded only by the 5 s `body_timeout`. There is no per-peer connection cap,
  so a peer can still fill the 128 slots for up to 10 s at a time by sending
  nothing, and the slot is taken before accept. How it works: the hook
  listeners are served through hyper's own `http1::Builder::header_read_timeout`
  (`serve_capped`, hyper 1.12 with `hyper_util::rt::TokioTimer`), 10 s. The
  deadline starts at accept, covers a head that never completes, is cleared by
  hyper when it has parsed a head, and starts again after each response, so an
  idle keep-alive connection is closed too. acd scans no request byte, so it
  cannot disagree with hyper about where a head ends. A held permission
  request is not cut: hyper reads no head while a request is in flight.
- **F4 not covered (herdr discovery, #86).** Discovery and every herdr
  reconnect require the socket to be a socket owned by the effective uid and
  every ancestor directory (checked root-first) to be owned by root or us and
  not group/other-writable, unless sticky and the next component is ours
  (`acd_securefs::check_socket_path`). Not covered: a same-user process, and
  the instant between the check and the connect (closing it would need a
  peer-credential check on the connection).
- **F5 (#86).** Control characters are rejected in the home, executable and
  config-dir paths; other characters were already escaped.
- **Whole-or-read-only (#83), accepted.** Characters such as U+034F, U+115F,
  U+3164 and U+FE0F pass the T3 and hooks escaper unchanged, so "whole
  command" is not a universal visible-character encoding. Pinned by
  `characters_outside_the_escaped_classes_pass_unchanged` (hooks) and
  `a_combining_grapheme_joiner_in_a_command_passes_unchanged` (T3); a change
  to the escaper must update them on purpose.
- **Whole-or-read-only (#83), accepted.** Question text and option labels
  that the adapters cut earlier (T3 in `t3/map.rs`, hooks in `hooks/mapping.rs`)
  stay answerable. Only permission commands follow the rule at the adapter.
- **Whole-or-read-only (#83), accepted.** Read-only for an oversized inbox
  view is a presentation guarantee. `Inbox::pick` in `acd-client` checks the
  original request, so a direct socket caller can still answer a card the view
  showed cut.

## herdr

- Narrow Codex panes (about 50 columns or less) can wrap the final option
  hint `(esc)` onto a separate row. `scan_options` stops before finding the
  options, so the request falls back to read-only `blocked_unparsed`. This
  parser regression is separate from unsupported multi-row commands; see
  [live check 12](../../research/2026-10-09-p3-live.md).
- The idle poll timer wakes every 1 s with nothing due.
- `clean_display` joins words across stripped tabs and newlines.
- herdr alone produces read-only blocked-screen cards; structured option
  cards need hooks or T3. herdr can send follow-ups to idle or done agents.
- herdr's own SSH machines could reach remote panes without a per-machine acd;
  acd would still be needed on each machine for the hook endpoint.
- A Codex agent's title showed `<environment_context>`. The herdr title
  fallback now skips that context (`herdr/map.rs`). Not confirmed on a live
  Codex run, so the adapter that first set it is unconfirmed. The T3 title
  path is unchanged.
- Binding commands that wrap or span several rows (D26). They get no screen
  binding and no keys target. Live check 12 recorded them; a width-aware rule
  still needs a separate review.
- `dialog_identity` still keeps `Tip:` lines to avoid resize ambiguity;
  rotating tips remain an unverified case.
- **Restart with a Claude dialog on screen.** After an acd restart, a dialog
  still on screen comes back as `blocked_unparsed`, read-only, because the
  hook payload M1 needs is gone (spec §12 "requests still on screen reappear"
  holds, but they are not answerable).
- **Esc before Claude draws the dialog.** A terminal Esc that arrives before
  the held dialog is drawn leaves acd without a capture; the request resolves
  `uncertain` after 30 s instead of `terminal`. Only reproducible by a script
  pressing Esc about 65 ms after the hook.

## Hooks and service

- With a literal Tailscale IP in `listen` and no Tailscale CLI, startup
  refuses the address before emitting the missing-CLI warning. The warning is
  also gated on the `tailscale` keyword instead of `bind::needs_tailscale_probe`;
  this diagnostic gap remains unresolved.
- An already-known rival Codex pane can start showing the same command without
  revoking the original request's advertised `keys` readiness. Delivery
  rechecks the rival and safely returns `request_not_ready`, but the client
  offers an answer until the user clicks. This differs from the spec's
  read-only rule; see [live check 5](../../research/2026-10-09-p3-live.md).
- Hook-token retirement deliberately considers every live Claude registry
  session, including ignored or unrelated sessions that never used acd. These
  can retain the previous token until its 30-day expiry, while
  `status_message` says they "still use the previous token". The retention is
  conservative; the diagnostic does not prove actual token use.
- Rotation status can remain silent for the first registry scan period
  (observed about 20–30 s), then report "waiting for a complete registry
  scan". This is a cosmetic reporting delay.
- Hook-config reload depends on the settings source: the live run saw a
  project-local symlink reload, while `--settings <file>` did not reload even
  after `/clear`. The observation was inferred from pending counts, not
  captured headers; user-level settings reload was not verified there.
- A held permission declined with terminal Esc resolved as `terminal` after
  1.7 s in that run, rather than immediately.
- `PostToolUseFailure` is installed and counted as "the tool ran", but its
  payload was unverified in the original gates. `PermissionDenied` evidence is
  not installed or checked.
- Codex in the shared managed daemon: `agent_session` is missing and the hook
  environment is wrong, so only a single Codex pane in a cwd can be bound.
  Upstream fix; restarting the managed daemon is unverified.
- A command-hook shim for Claude, to remove the loopback port-squatting
  exposure of the `http` hooks (ruling R7, post-v1). Needs gate 2 re-tested
  for command hooks.
- Free text in `AskUserQuestion` on the keys path, multi-select and
  multi-question answers (spec §11 item 1, §2).
- Live checks 10–14 have been run ([P3 results](../../research/2026-10-09-p3-live.md)).
  Codex review check 11 was partial, wrapped commands remain unsupported, and
  parallel Claude permissions were observed one at a time.
- **First-seen hidden Codex subagents.** Their session id is only in a thread
  stream, so a T3-run Codex subagent never claims its session. Its hook events
  reach acd but create no agent without a herdr binding (spec §6.5).
- Task 9: a stale registry entry whose pid was reused by another process keeps
  the scan incomplete (conservative).
- Task 20: `repair_mode` on a file the owner cannot read (mode 0o000 or 0o200)
  fails with a plain permission error; give a clearer message.
- Task 21: no failure injection for the writer stage; the `--force`
  generation-1 fallback reuses a generation number (no old token is accepted by
  it; watch the retention history, Task 26).
- Task 22: surplus reply bytes in a later read are ignored; the shim's
  `catch_unwind` wrapper has no test.
- Task 24: the Git check is path-based (it gates the write, it does not bind
  it); the gap between canonicalizing and opening the directory lands on a
  user-owned directory; the temp file can be `git add`ed in a short window;
  compare-then-rename has no compare-and-swap; Codex targets get no Git check.
- Task 25: a Codex journal written before Task 25 loads as Claude (branch-only,
  pre-release).

## T3

- **`project.removed`.** The adapter uses an inferred `projectId` field; live
  runs could not produce the event (`t3 project remove` returned
  `invalid_command`), so the payload remains unconfirmed.
- **Stale T3 requests on subagent threads.** On minos, two subagent threads
  keep a `pendingRuntimeRequest` long after the user considers them answered.
  Hidden by default since #19; find out whether T3 or acd is wrong.
- **Codex and other providers' free text.** Typed T3 answers are live-verified
  for Claude only. Codex questions stay option-only unless T3 sends
  `allowCustomAnswer: true`; "OpenCode sets false" is unverified.
- **T3 keepalive.** No paused-clock test for the busy cap; the slow-decode test
  does not reach `block_in_place`; a reader busy past the cap closes with "T3
  did not answer a ping" (a distinct reason would help); `write_timeout` covers
  a whole frame, not write progress.
- **T3 cursor retention.** A busy root thread re-pulls a ~24 MB snapshot after
  each `release()`; keep the cursor for Working-class releases.

## Mac app

- Clearing the clipboard after pairing (0.8.1) is not yet checked on a device.
  With Universal Clipboard the line may remain on the Mac it was copied from. A
  Keychain prompt during import leaves a window in which a newer clipboard
  value would be cleared as well.
- Ad-hoc signed rebuilds can prompt for Keychain access. `just app-install`
  supports `APPLE_SIGNING_IDENTITY` for a stable identity; signing and
  distribution are not automated releases.
- Test the hotkey failed-registration path; it requires a live shortcut manager.
- `mark_seen` has no test outside a runtime.
- Concurrent `set_hotkey` calls can race.
- Notification permission is requested at launch and its state shows in
  Settings (`notify.rs`, `Settings.svelte`). Not verified on a bundled build:
  check that the macOS prompt appears, that the state updates after it, and
  that a notification is delivered once allowed.
- **Panel frame.** No unit test for `save_now`; the order of the
  screen-parameters notification and macOS's refit moves needs a runtime check
  with a display unplugged.
- **Countdowns.** Turning idle agents off cancels a countdown on an Idle card
  that the view would now keep; the Done header count includes an out-of-filter
  card kept for its countdown; no panel test for J/K and Esc on a counting Done
  row while Done is collapsed.

## iOS

Foreground only (`apps/ios`, #40 and #41); no push, no relay. Live checks on a
device are in the signed-release step: the clipboard clear after pairing, the
Keychain class replacement, a release build on a device, and the Online state
right after a relaunch.

## Omarchy inbox

- **Manual check of the whole-card guard (#80), not done.** The QML checks
  match text and cannot prove wiring. Real proof is the manual Omarchy check
  in the PR body.
- **Pairing outside Tailscale says the wrong thing.** A pairing line whose
  literal address is outside Tailscale and loopback is refused at import with
  `bad_pairing` (`MachineError::NotTailscale` maps to it). Omarchy then shows
  "That is not an acd pairing line", which is misleading. A specific message
  needs a new wire code, which is a protocol decision, so it is not added.
  `acd-inbox` tests pin the mapping.
- **Unavailable keyring reads as damaged.** With no Secret Service running at
  prefill, `Cached::Unavailable` maps to `Unreadable`: the chip says "Pair
  again", `acd-inbox status` says the secret is damaged, and re-pairing fails.
  The spec does not define this state.
- **Unknown write leaves an orphan.** If the write of a new machine's secret
  ends `Unknown`, the keyring item may stay under a UUID no machine uses.
- **Show idle is not saved.** `Inbox::set_show_idle` changes memory only (the
  Mac app behaves the same), so the Omarchy toggle resets when the service
  restarts. Persisting it changes shared client behaviour; decide for both
  clients.
- **Uninstall and a binary install did not write.** Uninstall now keeps the
  binary unless it found its own unit, but an identical binary that was
  already in place is still removed with it.
- **Uninstall provenance.** `acd-inbox uninstall` disables the plugin and
  removes its named files without checking they came from install; a foreign
  directory under the same id is not protected (the install side refuses
  foreign units and symlinks).
- **Notifications across restarts.** If `org.freedesktop.Notifications`
  restarts while requests are open, a close for a saved id goes to the new
  owner and may close an unrelated notification. On service shutdown the
  tracked notifications are not closed, so a restart can leave a stale one
  beside the new.
- **Keyring: an individually locked item.** `unlock` only unlocks the
  collection; with the collection unlocked and one item locked the "Unlock
  keyring" action cannot recover it. Search for the locked items and unlock
  their paths too.
- **Locked after a failed mutation.** A re-pair or removal that gets `Locked`
  leaves a previously cached `Cached::Value`, so the settings still say `ok`
  and the panel does not offer "Unlock keyring".
- **Settings vs the view of a slow edit.** `pair`, `remove_machine`,
  `rename_machine` and `set_show` wake the view before the settings snapshot is
  built, so a panel can briefly join a new view with the old settings.
- **Preference write inside admission.** `set_notifications` saves
  `service.json` while `Service::admit` holds the connection gate; on stalled
  storage a close cannot take the gate to cancel countdowns. Move it to the
  slow-command path.
- **Installer stderr.** The reader pushes every chunk into an unbounded channel
  until the child exits; keep a rolling tail in the reader instead.
- **Rename racing remove** returns `settings_failed`, not `unknown_machine`.
- **Keyring deadline.** Mutating Secret Service calls have no deadline; check
  whether zbus applies a reply timeout, or a hung service blocks the worker
  until restart.
- **One owner per panel.** Each `Service.qml` is its own connection and owner.
  If Omarchy creates a bar widget per monitor, Esc in one panel does not undo a
  countdown started from another's.
- `loginctl enable-linger toni` keeps acd running while logged out (optional).
- The 2026-10-12 live check covers the install, an approval with countdown and
  Esc, a notification and a shell restart. It excludes pairing a remote machine
  with keyring lock and unlock recovery, and the typing-while-another-agent-updates
  focus cases; unit and QML-model tests cover those
  ([record](../../research/2026-10-12-omarchy-plugin-api.md)).

## Voice

Step 2 (from the final review):

- **Probe vs safety paths.** The diagnostic probe records on its own engine:
  lock or voice-off do not cancel it (it ends after 4 s), and it shows no tray
  dot. A busy microphone (`VB_ERR_BUSY`) reads as "Speech recognition is not
  available".
- **Pump stalls.** `dispatch` runs `trigger::refresh` and the tap restart (up
  to about 3 s) inline, so the event pump can stall behind a permission answer.
- **Settings.** Download-speech-model gives no progress and does not report a
  failed install (the `ok` of `VB_EV_ASSETS` is dropped in `dispatch`); the
  first rejected status read hides the permission section.
- **Press cancels a countdown, then listening fails.** `Executor::listen_failed`
  clears `trigger_cancelled`, so the HUD shows only the microphone or speech
  error, not that the countdown was cancelled (nothing was sent).
- **Confirm nested under Listening.** If the user starts recording after a
  Confirm HUD was emitted but before `voiceShown` arrived, the unacknowledged
  Confirm is carried inside `Listening` (`over`), the frontend does not
  acknowledge it, and the display watcher invalidates it; the spoken answer is
  then routed as a new command. Fails closed.
- **Stale voice shortcut.** If unregistering the previous voice shortcut
  fails, `hotkey::apply_voice` has already replaced `VOICE_SHORTCUT`, so the
  old chord is routed as the inbox hotkey until restart.
- **Dropped audio chunk** now ends the session as "microphone unavailable" (no
  test: needs the live engine); a dedicated refusal text would be clearer.
- **Plan deviations approved at merge (2026-10-12):** no Space as a hold key
  in the open inbox, English only with the system microphone, no "English until
  installed" fallback.

Steps 3 and 4 (final review, 2026-10-13):

- A saved machine filter is ignored by alerts until the panel first publishes
  its presentation after launch (`machine_filter` defaults to none before
  that), so an alert for a filtered-out machine could be spoken in that first
  moment.
- Swift appends a short interface note to Rust's `INSTRUCTIONS` (plan V2 says
  Rust is the single source); it is fixed text, not agent text.
- A model `select` runs without confirmation (spec §6.1) and can pick any
  visible card; a following fast-path "approve" then acts on the selected card
  with the 2 s countdown and the full command on the HUD. Same risk as
  selecting with the keyboard; noted, not changed.
- Run-to-run: the 19/20 and 19/20 phrase result is two runs on one Mac, small
  sets, typed text.
- The 16-item live check on the signed app has not been run
  ([list](../../research/2026-10-13-voice-model-contract.md)).

## Tests, flakes and the harness

Open test work:

- **Flaky test, macOS CI (seen 2026-11-04).** `repairing_inside_the_undo_window_sends_nothing`
  (`crates/acd-client/tests/inbox.rs`, final `assert_eq!(answers(&f), 1)` after
  a fixed 400 ms sleep) failed once on PR #87, which does not touch
  `acd-client`; a rerun passed. Fix: wait for the answer to arrive. PR #105
  does this; it is not merged. Other tests in that file still sleep for fixed
  times (for example `reconnect_within_undo_window_cancels`).
- **Auth log test (#90).** `crates/acd-core/tests/tls.rs` (`a_hello_naming_itself_after_the_token_does_not_log_the_token`)
  accepts any shared "hello accepted" line, so another test can mask this
  hello timing out, and prints a token substring in its failure message
  (CLAUDE.md: use a static message). It also waits a fixed 200 ms. PR #105
  fixes the first point; it is not merged.
- Task 1: some CLI tests inherit stdout and print throwaway tokens (for
  example `acd init`, `token rotate`); capture the output in the harness.
- Task 8: the brief's tests for the full event queue, disconnected holds and
  churn were defective (a dropped `Held` temporary; closing before the server
  accepted). Hook-endpoint tests must use the `settle` and `hold_and_close`
  helpers in `crates/acd-adapters/tests/hooks_endpoint.rs`.
- Review automated edits for unrelated formatting; keep changes scoped.
- `just e2e`: `run.lock` has two millisecond races, and `just e2e-down`
  ignores the lock. Once the user trusts acd's Codex hooks, the Codex
  scenarios skip themselves on that machine. §14.10's blocked-screen half was
  not testable (no provider for opencode or pi); `screen_mismatch` (§14.4) was
  never observed.
- minos: agents started over SSH cannot use the login keychain; acceptance
  there relies on locally started sessions.

Historical flakes, not reproduced in the 2026-10 audit:

- `crates/acd-adapters/tests/t3_conn.rs`
  `a_peer_that_stops_reading_trips_the_write_timeout` (P4) failed once in a
  full run under heavy load and passes alone; consider widening its 5 s bound
  (still `Duration::from_secs(5)` at line 377).
- `herdr_e2e` failed once in a workspace run and passed on rerun (Task 22).
- Seen failing under full-workspace load on `external-source`; pass alone;
  base not checked: `crates/acd/tests/hooks_cli.rs`
  `a_stalled_acd_adds_at_most_1_s_to_a_lifecycle_hook` and
  `the_shim_gives_up_when_stdin_never_closes`;
  `crates/acd-adapters/tests/hooks_bounds.rs`
  `queued_holds_cannot_delay_a_due_timer_past_one_batch`;
  `crates/acd-adapters/tests/hooks_endpoint.rs`
  `lifecycle_hooks_reply_at_once_without_a_consumer`.
- `crates/acd/tests/hooks_cli.rs` `a_path_that_is_not_a_socket_is_never_replaced`
  failed once on Ubuntu CI (2026-10-12, PR #24, run 38209946246): the "not a
  socket" message was missing and a worker panicked at
  `acd-core/src/tls.rs:430`. It passed on rerun. The daemon may have exited on
  a TLS setup error before the socket check.
- Sandbox-only failures (special mode bits and `ps` are not honoured, and
  permission-error tests fail as root); they pass outside:
  `secrets::tests::repair_mode_clears_special_bits_and_reports_them`,
  `init_repairs_a_setuid_secret_and_a_loose_t3_token`,
  `hooks::claude_sessions::tests::the_system_check_recognises_this_process`.

## Removed as fixed

Checked in code on 2026-11-04.

- Omarchy J/K scroll: `Panel.qml` `revealCard` scrolls the list to the
  selection (#73).
- Notification history on a flap: Omarchy keeps a bounded history
  (`SEEN_HISTORY_MAX`, `crates/acd-inbox/src/notify.rs`, #73, keyed on
  `opened_at` in a later commit). The Mac app and iOS use the shared
  `NotifyGate` in `acd-client` (`seen` evicts only past 256 keys, #81, #90), so
  a request that leaves the view on a disconnect does not notify again. I found
  no test of a flap itself.
- Core 1 (#82), 2 and 6 (#81), 3, 7, 8 (#88), 4 and 5 (#90); local F1 (#80,
  #83), F3 (#87), F4 and F5 (#86).
- Test hardening (#89): `status_change_before_resubscribe_is_caught` accepts
  the new pane's own `agent.get` or the full list; the pre-auth recovery test
  uses a `ServerOptions::hello_gate` seam (default `None`); the Omarchy plugin
  test checks every handler body; T3 and hooks have exact-output escape tests.
- `t3_sync` `hidden_thread_shows_while_it_has_a_request_and_resolves_terminal`
  flake: the test waits for the stream to end
  (`until("the hidden thread's stream to end", …)`).
- The launchd reinstall race (six later reinstalls passed). Registry delay
  check 14 passed (worst 3.06 s against the 5 s grace).

## 1.0 triage

Sorted 2026-10-13 and updated 2026-11-04. Each item is in one bucket. Items
not named here stay in their section as accepted residuals, except items marked
**OWNER-undecided** (see the first section): those need an owner decision
before 1.0.

**Fix before 1.0** (daily use, or wrong output a user would see):

- Omarchy: Show idle is not saved across restarts (decide for both clients).
- Voice: Download-speech-model gives no progress and does not report a failed
  install; stale voice shortcut after a failed unregister.
- Pairing a literal non-Tailscale address says "That is not an acd pairing
  line" on Omarchy. Needs a protocol decision on a new error code.
- The clipboard clear after pairing (0.8.1), the iOS Keychain class
  replacement, a release build on a device and the Online state right after a
  relaunch: live checks on a device, in the signed-release step.
- The oversized pre-`welcome` frame resync (PR #105).
- The test items above that wait on fixed sleeps (PR #105).

**Verify on the 1.0 acceptance run** (implemented, not yet seen live):

- Notification delivery on a bundled build, the Codex title fallback,
  `project.removed` payloads, rotating `Tip:` lines, Codex free text for
  providers other than Claude.
- Hotkey failed-registration path and panel frame after an unplugged display.
- The voice 16-item live check; the manual Omarchy whole-card check (#80).

**After 1.0** (design work, upstream, or rare):

- A command-hook shim for Claude to remove the loopback port-squatting
  exposure of the `http` hooks (ruling R7).
- Free text, multi-select and multi-question answers on the keys path.
- Codex in a shared managed daemon, wrapped multi-row commands, narrow Codex
  panes, `AskUserQuestion` variants, T3 cursor retention and keepalive
  details, stale T3 requests on subagent threads.
- Esc before Claude draws the dialog, and a restart with a dialog on screen
  (read-only after the restart).
- Keyring edge cases on Omarchy (individually locked item, locked after a
  failed mutation, deadline), installer stderr tail, uninstall provenance.
- Voice: pump stalls, nested Confirm under Listening, dropped-chunk wording.
- Harness races in `just e2e`.
