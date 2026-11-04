# Mutation testing of the security-critical crates (2026-11-04)

Question: do the tests of the crates that decide authentication, owner
checks, limits, answer routing, idempotency, revision checks, character rules
and frame limits fail when that code is changed? `cargo-mutants` changes one
operator or function body at a time (a mutant) and runs the tests; a mutant
the tests do not notice is *missed*.

This record is evidence, not a plan. Counts are from the runs described below;
nothing here was estimated.

## Tool, command, environment

- `cargo-mutants 27.1.0` (`cargo install cargo-mutants --locked`), Rust 1.97.0.
- Linux 6.18 sandbox, 4 cores, 15 GB, **running as root** (euid 0), `CARGO_INCREMENTAL=0`.
- cargo-mutants copies the tree to `/tmp/cargo-mutants-*` at the start of every run and tests the copy;
  `--in-place` was never used. The `acd-securefs`, `acd-protocol` and `acd-core` runs started from the
  working tree (clean, with only committed test files). The `acd-client`, `acd-voice` and `acd-adapters`
  runs started from `git worktree`s of a pushed commit, so edits made while they ran could not reach them.
- Per crate, from the repository root (`--jobs 2` for most runs, 3 or 4 for some `acd-adapters` runs, 1 for
  the short re-checks):

  ```sh
  cargo mutants -d crates/<crate> --jobs 2 --timeout-multiplier 4 \
    --build-timeout-multiplier 4 --minimum-test-timeout 60 \
    -E '<impl (std::)?(fmt::)?Display for' -o <out-dir>
  ```

  `-E` leaves out `Display` impls (text only). `Debug` impls were **not** excluded:
  several redact tokens and typed text, which is a security property, and they now have tests.
  `acd-client` also used `-e crates/acd-client/src/fake.rs` (the fake server is a test helper).
  `acd-voice` "after" runs also excluded the test helpers `fake::`, `FakeIntentModel`,
  `FakeSpeaker` and `fixtures::`, which cannot be tested through themselves.
- `acd-adapters` was run per file group with `-f crates/acd-adapters/src/<file>.rs` and only the
  relevant test binaries, because the whole adapter suite takes about a minute and the herdr suites are
  out of scope. So "missed" for those files is relative to that subset:
  - hooks (`owner`, `paths`, `tickets`): `--lib`, `hooks_claude`, `hooks_codex`, `hooks_endpoint`
    (not `hooks_bounds`, `hooks_e2e`, `external_api`); the re-run after the new tests added `mutants_adapters`.
  - T3 (`map`, `outcome`, `targets`, `detail`): `--lib`, `t3_adapter`, `t3_conn`, `t3_sync`, `t3_wire`,
    `t3_e2e`; the re-runs after the new tests added `mutants_adapters`.
- The mutants are tested against **the package's own tests** (`cargo test -p <crate>`), not the whole
  workspace. A mutant that only the `acd` binary crate's tests would catch counts as missed here.

### How the "after" numbers were made

- *After* = the *before* outcomes, with every missed or timed-out mutant tested again
  (`cargo mutants --iterate`) at the commit that holds the new tests. Caught and unviable mutants
  are not tested again; unviable mutants do not compile and tell nothing.
- A **timeout** means the test run did not finish in time. Most are an infinite loop or a hang
  caused by the mutant (`+=` -> `*=` in a loop counter, a reply that is never sent). That is a
  detection of a sort (CI would stall), not a clean failure. Because one hanging old test hides a
  new test that fails fast, each timeout was run once more with **only the new tests**
  (`--cargo-test-arg=--lib --cargo-test-arg=--test mutants_core -- -- mutants_ <test names>`).
  Those the new tests kill quickly are counted as caught and shown in the column "of which by new
  tests only". The same kind of re-run was used for the few mutants that were still missed at the end
  (`t3/detail.rs`: 5, `acd-client` `retry_machine`: 1); what it kills counts in that column too. No other
  mutant was reclassified.
- Time is wall clock on a machine that was often oversubscribed (load 10 to 16 on 4 cores, two
  to six jobs at once), so the times only compare orders of magnitude. Under that load some
  timeouts may be false; the ones that matter were checked as above.

## Result

| Crate | Scope | Mutants | Before (caught / missed / unviable / timeout) | After | of which by new tests only | Time before | Time after |
|---|---|---|---|---|---|---|---|
| `acd-securefs` | whole crate | 86 | 58 / 21 / 6 / 1 | 67 / 12 / 6 / 1 | 0 | 1 min 7 s | 1 min 9 s |
| `acd-protocol` | whole crate | 86 | 65 / 8 / 13 / 0 | 71 / 2 / 13 / 0 | 0 | 2 min 3 s | 2 min 12 s |
| `acd-core` | whole crate | 416 | 248 / 41 / 106 / 21 | 301 / 7 / 106 / 2 | 20 | 34 min 53 s | 20 min 25 s, then 6 min 22 s + 2 min 24 s checking timeouts |
| `acd-client` | whole crate except `fake.rs` | 758 | 488 / 113 / 149 / 8 | 577 / 26 / 149 / 6 | 2 | 66 min 4 s + 24 min 45 s (the first run stopped on a full disk at 667 of 758) | 42 min + 54 min 13 s + 38 min 30 s, then 6 min 15 s + 2 min checking some of them with only the new tests |
| `acd-voice` | whole crate | 590 | 418 / 76 / 96 / 0 | 462 / 32 / 96 / 0 | 0 | 38 min 33 s | 6 min 57 s + 3 min 41 s |
| `acd-adapters hooks` | `hooks/owner.rs`, `paths.rs`, `tickets.rs` | 109 | 72 / 8 / 20 / 9 | 87 / 0 / 20 / 2 | 7 | 71 min 2 s | 37 min, then 24 min checking timeouts |
| `acd-adapters t3` | `t3/map.rs`, `outcome.rs`, `targets.rs` | 89 | 71 / 9 / 9 / 0 | 80 / 0 / 9 / 0 | 0 | 63 min | 12 min 19 s |
| `acd-adapters t3 detail` | `t3/detail.rs` | 57 | 20 / 10 / 1 / 2 | 54 / 0 / 3 / 0 | 5 | the first run stopped at 33 of 57 | 49 min 16 s, then 7 min + 6 min re-checking the missed ones with only the new tests |

Columns: caught / missed / unviable / timeout. A mutant counts once, in its final state.
`acd-client` was run in two stages because the first run stopped on a full disk at 667 of 758 mutants;
the remaining 91 were tested by a second run. `acd-voice`'s missed mutants include 25 in test helpers
(see below). For `t3/detail.rs` the "before" run was stopped early, so its "before" covers 33 of the
57 mutants; the "after" run is complete.

Total tests added: 141 (1814 passing on `origin/main` 3f7c7df, 1955 on this branch merged with it, from
`cargo test --workspace --locked --no-fail-fast` in this sandbox; the same 29 tests fail on both, all
in the `acd` binary crate; see the pull request).

**Base.** All mutation runs were made on `origin/main` 3404287 plus the new tests. Main moved afterwards
(for example `acd-core` `idem.rs`, `server.rs` and `registry.rs`, and `acd-securefs`, which gained
Windows ACL code). The branch was merged with main afterwards and the full suite re-run. Only
`acd-securefs` and `acd-protocol` were measured again on the merged code (both with `--jobs 4`, 1.5 and
6 min):

- `acd-protocol`: unchanged, 86 mutants, 71 caught, 2 missed (the two equivalent ones), 13 unviable.
- `acd-securefs`: now 236 mutants (main added Windows code): 128 caught, 100 missed, 7 unviable,
  1 timeout. Of the missed, 82 are in `windows.rs` and 4 in `paths.rs` (`windows_*_dir`), which are
  `cfg(windows)` and cannot be built here; the 12 in `lib.rs` are the ones listed below (their line
  numbers moved by 11); `paths.rs:29:36` (`p.len() < 3` in `is_absolute_windows`) was killed by a test
  added afterwards, `a_windows_drive_root_is_absolute`; `paths.rs:34:20` is equivalent, because the
  length check above it already returned. The Windows code itself has no mutation evidence at all.

The other crates were not measured again; main's changes to them (idempotency, `server.rs`,
`registry.rs`) may have added mutants that the new tests do not cover.

## What was added

New files only, so no other session's test files conflict. Each test names the mutant it kills in
its doc comment.

| File | Covers |
|---|---|
| `crates/acd-securefs/tests/mutants_securefs.rs` | owner-only file checks: `check_private_file`, `open_owned_file`, `read_private` length limit (inclusive), Missing/NotRegular kinds, symlinked directory, other-owner refusal (root only), unprivileged euid in a child process (root only), one `#[ignore]`d known bug |
| `crates/acd-protocol/tests/mutants_protocol.rs` | frame, agent and agent-count limits, token and pin alphabet and length, `Debug` redaction of `Pairing` and `ClientMessage` |
| `crates/acd-core/tests/mutants_core.rs` | id and text limits (inclusive), idempotency TTL and capacity, source status, revision bump per field, claim rules, `FallbackEnded` fail-closed rules, migration, origin allow-list, `client.id` limit, PEM bundle rules |
| `crates/acd-core/src/mutants_server.rs`, `mutants_tls.rs` | rotation revocation (`run_command`, `revoked`), log limiter, TLS deadline for unauthenticated connections, `close_notify`, accept-task abort |
| `crates/acd-client/tests/mutants_client.rs`, `mutants_client_env.rs` | `pick` / typed answer / reply refusals and limits, voice-path reply guard, countdown timestamps, pairing fail-closed, rename / remove / retry, session limits (64 KiB agent, 1000 agents), settings validation, `MachineSecret` redaction, `mark_seen`, hiding, config dir |
| `crates/acd-client/src/mutants_{actions,commands,conn,inbox,view}.rs` | `check_reason` rules, retry and reconnect windows, close-code and handshake mapping, `wait_retry`, notification history |
| `crates/acd-voice/tests/mutants_voice.rs` | executor: spoken cancel, `display_failed`, stale-result numbering, `named_send_only`, typed-answer binding; target resolution; model reply limits and agent/request agreement; candidate filtering; `Debug` redaction |
| `crates/acd-adapters/tests/mutants_adapters.rs` | Claude header-to-pane binding, `same_dir_with`, `SessionOwners`, T3 thread detail (projection validity, skipped entries, retention, pruning), T3 `kind`, `request_obs`, `classify`, `Targets` |
| `crates/acd-adapters/src/hooks/mutants_{tickets,paths}.rs` | read slots and the path cache (freshness boundary, size bound) |

Where the code under test is private, a short `#[cfg(test)] #[path = "mutants_*.rs"] mod mutants_*;`
block (four lines) was appended to the module (`server.rs`, `tls.rs`, `actions.rs`, `commands.rs`, `conn.rs`, `inbox.rs`,
`view.rs`, `hooks/tickets.rs`, `hooks/paths.rs`). No other production line changed. `crates/acd-adapters/Cargo.toml` gained one dev-dependency
line (`tokio` with `test-util`) for the cache-freshness test, which needs a paused clock.

Two new tests in `mutants_securefs.rs` need root (one chowns a file to another user, one runs the
checks in a child process as uid 65534) and return early otherwise. CI runs unprivileged, so there
they are no kill for `euid -> 0`; every other securefs test that creates a file of its own is. The
whole file was also run once as uid 65534 (`setpriv --reuid=65534`, `TMPDIR=/var/tmp/...`) and passes.

## Mutants left, and why

"Equivalent" means no test can tell the mutant from the original because the behaviour is the
same. "Not worth a test" means the behaviour differs but is not a security decision.

### `acd-securefs` (12 missed, 1 timeout)

| Mutant | Why |
|---|---|
| `lib.rs:98:38`, `98:57` `\|` -> `^` (open flags) | Equivalent: `O_NOFOLLOW`, `O_NONBLOCK` and `O_CLOEXEC` are distinct bits, so `^` equals `\|`. |
| `lib.rs:98:57` `\|` -> `&` (timeout) | The flags become 0 and opening a FIFO blocks forever. Detected by the hang of the existing FIFO test. |
| `lib.rs:104:11` `fd < 0` -> `<=` | Differs only when `open` returns descriptor 0, that is with stdin closed. Not worth a test. |
| `lib.rs:110:18` match guard -> `true` | Only changes which reason a failed open reports (`Owner`, `Symlink` or `Io`). Both refuse. |
| `lib.rs:203`, `208`, `214`, `219` (x3), `292`, `297` | The `cfg(not(owner_checks))` stubs for Windows and Android. They are not compiled on Linux, so cargo-mutants cannot test them; the in-crate test `every_check_refuses_even_for_a_good_file` (Windows and Android only) covers them there. |

### `acd-protocol` (2 missed)

`pairing.rs:26:26` (`decode_pin`) and `36:42` (`valid_token`) both turn the second half of
"decodes to 32 bytes **and** re-encodes to the same text" into an `||`. 43 base64url characters
always decode to 32 bytes and the decoder already rejects non-zero trailing bits, so the second
check never differs. Equivalent. (The tests for a wrong alphabet and for trailing bits pass under
both versions on purpose.)

### `acd-core` (7 missed, 2 timeouts)

| Mutant | Why |
|---|---|
| `engine.rs:55`, `56` (`apply_bounded` arms `RequestSeen`, `AgentMigrated`) | Equivalent: `Registry::upsert_agent` is the only place an agent enters the registry, and that arm is checked, so an over-long agent id cannot be known to the other two paths. Defence in depth. |
| `log.rs:45:88` `seq + 1` -> `seq * 1` | Equivalent: the ring is empty only when `seq` is 0, where both give the same answer for every cursor. |
| `registry.rs:573:29` `generation > 0` -> `>= 0` | Equivalent: key states are created only by `claim_key`, which increments first, so a generation is never 0. |
| `tls.rs:526` `Debug for TlsListener` | Prints the local address only. Not worth a test. |
| `testing.rs:40` (x2), `28`, `29` (timeouts) | Test helper `FakeAdapter`. |

Twenty of the timeouts are now killed quickly by the new tests; the two left are the helper mutants
above. The old suite hangs on those twenty, which is why they showed as timeouts.

### `acd-client` (26 missed, 6 timeouts)

Equivalent or redundant:

| Mutant | Why |
|---|---|
| `conn.rs:221` `\|\|` -> `&&` | Both operands test the same property of the same error text. |
| `conn.rs:330` (`Message::Ping` arm), `335` (`Message::Close` arm) | tungstenite queues the pong itself; without the arm the next read ends with the same "closed" error. |
| `conn.rs:534`, `543` | The revocation of commands sent under an old credential runs before and after the connection opens; either one alone is enough while the credential is stable. |
| `inbox.rs:819` | `start` already limits the list to machines without a handle. |
| `inbox.rs:866` `>` -> `>=` | Compares two `Instant`s for equality; unobservable. |
| `settings.rs:140` | Same as `acd-protocol`'s `decode_canonical_32` pair. |
| `settings.rs:278` match guard -> `true` | A path that cannot be read as a file then fails in `save` with the same error. |
| `local.rs:11` | `cfg(not(owner_checks))` stub. |
| `inbox.rs:421` | Test hook `edit_waiters_for_test`. |

Timing, wording and feedback, not decisions (the check that stops a stale action is `check_reason`
at fire time, which the existing and new tests kill):

| Mutant | What it changes |
|---|---|
| `conn.rs:418` | Far-future fallback for the idle-ping timer. |
| `inbox.rs:639`, `989`, `1021`, `1108` | `now + notice_ttl` -> `now - notice_ttl`: how long a notice stays. |
| `inbox.rs:1015`, `1019` | `not_sent` / `set_notice` -> no-op: the notice text. |
| `inbox.rs:1095`, `1096`, `1099` | `end_machine`: the "offline, nothing was sent" notice appears later, at fire time, instead of at once. |
| `inbox.rs:919` | Which hidden idle cards the voice listing carries. |
| `inbox.rs:1164` | The detail label of an undo countdown. |
| `inbox.rs:519`, `612` | Cancelling a machine's handle after a failed secret recovery; not observable from outside. |
| `inbox.rs:1395` | The wake-up in `cancel_owned`; other wake-ups cover it. |

Timeouts (6): `actions.rs:166`, `184`, `203` (`+=` -> `*=` in a loop counter) and `inbox.rs:157:32`
(`>` -> `<` in a `while`) loop forever; `inbox.rs:381` (`emit` -> no-op) and `inbox.rs:1360`
(`cancel` -> `true`) make an existing test wait for an event that never comes. All are detected by
the hang only. `conn.rs:385` (the `Error` arm of `handshake`) is killed fast by the new test.

### `acd-voice` (32 missed)

| Mutant | Why |
|---|---|
| `listen.rs:371-372` (4), `router/model.rs:146-170` (10), `snapshot.rs:333`, `speak.rs:209-228` (10) | Test helpers (`FakeSpeechInput`, `FakeIntentModel`, `FakeSpeaker`, `fixtures::working`): 25 mutants. |
| `plan.rs:209`, `210` match guards `!which_one` / `which_one` -> `true` | Equivalent: `confirm` and `choose` check the state again and return `None` in the wrong one. |
| `plan.rs:303`, `426` `+=` -> `*=` | Equivalent: `begin` increments the utterance number again, so a stale result still never matches. (`-=` is killed.) |
| `router/mod.rs:106` `Routed::tag` | A log tag only. |
| `router/target.rs:175` `>` -> `>=` | Equivalent: narrowing a single hit by project keeps the same hit. |

### `acd-adapters`

`hooks/owner.rs`, `paths.rs`, `tickets.rs`: all 8 missed mutants are killed. Two timeouts remain:
`owner.rs:25` (`pane_facts` -> `vec![]`; building a `PaneRef` needs a herdr connection, so no new
test; the old suite hangs) and `tickets.rs:29:12` (`delete !` in `ReadSlots::admit`; the new
`admit_numbers_reads_and_remembers_refused_ones` asserts a refused read gets no number and
should fail it at once, but the restricted re-run that proves it did not reach this mutant).
Seven timeouts were re-run with only the new tests and are killed.

`t3/map.rs`, `outcome.rs`, `targets.rs`: all 9 missed mutants are killed.

`t3/detail.rs`: the full run left 4 missed mutants and 1 timeout (the question arm and the provider-thread
limit in `prune`, and `valid_projection`). Tests added afterwards kill all of them; none is left.

## Not done

- `acd-adapters`: only `hooks/{owner,paths,tickets}.rs` and `t3/{map,outcome,targets,detail}.rs`
  (see the result table for the counts) were run. Not run: `hooks/state.rs` (447 mutants),
  `hooks/{endpoint,wire,mapping,evidence,claude_sessions,dialogs,mod}.rs`, `t3/{wire,conn,reconciler,
  sessions,deferred_ends,mod}.rs`. These carry most of the hook-token and answer-routing logic of the
  adapters, so this is the largest open gap; at the observed rate (about one mutant a minute on this
  machine) they need several hours. `herdr/` was out of scope. A first attempt on `hooks/{owner,paths,tickets,wire,
  endpoint,mod}.rs` was abandoned for time (about one mutant per minute under load); its 17 outcomes
  are not used.
- `acd-voice-client` and `acd` (the binary crate) were not in scope.
- Per-package testing: a mutant that only a test in another crate would catch is reported as missed.
- No live check (herdr, T3 Code, Tailscale, macOS Keychain) was possible or intended.
- macOS-only paths were not run; the new tests avoid macOS-specific behaviour but were run on Linux only.

## Findings

No mutant revealed a defect in what the code does for the user. One reported reason is wrong:
`check_private_dir` on a symlinked directory refuses (correct) but reports `NotRegular`
instead of `Symlink`, because on Linux `open(O_NOFOLLOW | O_DIRECTORY)` fails with ENOTDIR, not ELOOP.
It is recorded as the `#[ignore]`d test `a_symlinked_directory_reports_a_symlink` in
`mutants_securefs.rs` and not fixed here.
