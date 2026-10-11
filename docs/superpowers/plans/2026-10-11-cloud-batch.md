# Cloud session batch (credit expires 2026-11-05)

Prompts for Claude Code cloud sessions, which run on Linux. Each prompt
is one session, one branch and one PR. Paste the common preamble first,
then the task.

Launch 3 or 4 sessions at a time. Calibrate the cost on the first ones.
Review each PR locally (Opus; Sol for risky ones) before merge. Cloud
sessions cannot call Codex.

## Conflicts with work in progress

P6 is not merged. `p6-e2e` and `p6-t3` touch these files, so wave 1
keeps out of them:

- `crates/acd-adapters/src/herdr/keys.rs`, `herdr/screen.rs`
- `crates/acd-adapters/src/hooks/mapping.rs`, `hooks/state.rs`
- `crates/acd-adapters/src/t3/*` and `crates/acd-adapters/tests/t3_*.rs`
- `crates/acd-adapters/tests/herdr_adapter.rs`
- `crates/acd-client/src/settings.rs`
- `crates/acd/src/hook_shim.rs`, `crates/acd/tests/hooks_cli.rs`
- `app/src-tauri/src/main.rs`, `justfile`
- `docs/superpowers/specs/2026-10-07-agent-inbox-design.md`

Wave 2 starts after P6 merges.

## Common preamble

```text
Read CLAUDE.md first. Work on a new branch from main and open one PR.

Do not edit these files; another branch changes them: herdr/keys.rs,
herdr/screen.rs, hooks/mapping.rs, hooks/state.rs and t3/* in
crates/acd-adapters/src, crates/acd-adapters/tests/t3_*.rs and
herdr_adapter.rs, crates/acd-client/src/settings.rs,
crates/acd/src/hook_shim.rs, crates/acd/tests/hooks_cli.rs,
app/src-tauri/src/main.rs, justfile, and the spec. Do not edit
docs/superpowers/plans/2026-10-07-p1-followups.md either. If your task
needs a change there, say so in the PR description.

Before opening the PR, run:
  cargo test --workspace --locked
  cargo clippy --workspace --all-targets --locked -- -D warnings
Put the results in the PR description. Name any failures that CLAUDE.md
lists as sandbox-only, and do not try to fix those.

The PR description lists: what changed and why, the tests added, the
command results, anything you could not verify on Linux, and new
deferred items with a suggested follow-ups entry.

Task:
```

## Wave 1 (now)

### 1. CLI test hygiene

```text
Follow-ups, P3 deferred, Task 1: some CLI tests in crates/acd/tests/cli.rs
inherit stdout and print throwaway tokens (for example `acd init` and
`token rotate`). Capture the output in the harness so no token reaches
the test log. Assert on the captured output where a test depends on it.

Also look at the free_port race in crates/acd/tests/common/mod.rs:
free_port binds 127.0.0.1:0, drops the listener and returns the port, so
another test can take it before the daemon binds. Decide whether
retry-on-bind-failure in the harness is enough (P3 Task 1 in
docs/superpowers/plans/2026-10-09-p3-hooks.md describes the intended
design). Keep free_port's signature: hooks_cli.rs on another branch uses it.

Owned files: crates/acd/tests/cli.rs, crates/acd/tests/common/mod.rs,
crates/acd/tests/harness.rs.
```

### 2. herdr reconciler: `lost()` test and `herdr_e2e` flake

```text
Follow-ups, P3 deferred, Task 5: no test pins that `lost()` in
crates/acd-adapters/src/herdr/reconciler.rs clears `closed`. Add one
that fails if the `self.closed.clear()` line is removed.

Then look at the herdr_e2e flake (crates/acd-adapters/tests/herdr_e2e.rs
failed once in a workspace run and passed on rerun). Run it in a loop
under load, for example 200 runs while the rest of the workspace tests
run in parallel. If it fails, find the cause from the evidence and fix
it. If it never fails, report the run count and conditions and change
nothing.

Owned files: crates/acd-adapters/src/herdr/reconciler.rs (tests only,
unless the flake fix needs more), crates/acd-adapters/tests/herdr_e2e.rs,
crates/acd-adapters/tests/herdr_sync.rs.
```

### 3. Secrets and the hook token store

```text
Two items from follow-ups, P3 deferred:

Task 20: `repair_mode` in crates/acd/src/secrets.rs fails with a plain
permission error on a file the owner cannot read (mode 0o000 or 0o200).
Give a message that names the file, the mode found and what to do. Test
both modes. These tests fail as root: skip them when `id -u` is 0, and
say so in the test.

Task 21: the writer stage of the hook token store
(crates/acd/src/hook_token.rs) has no failure injection. Add a test seam
that makes the atomic write fail at each stage (temp write, fsync,
rename) and check that the store on disk stays at the previous valid
state and that no old token becomes valid again. Read the Task 21 and
Task 26 sections of docs/superpowers/plans/2026-10-09-p3-hooks.md first.
Check the `--force` generation-1 fallback against the retention history:
report whether it can reuse a generation that is still retained. Do not
change it unless you can show a failing case.

Owned files: crates/acd/src/secrets.rs, crates/acd/src/hook_token.rs,
crates/acd/src/writer_lock.rs.
```

### 4. Tailscale startup warnings

```text
Follow-ups, P5 deferred: with an explicit Tailscale IP in `listen` (for
example "100.101.102.103") and no Tailscale CLI, acd refuses the address
without saying the CLI is missing. In crates/acd/src/daemon.rs the
warnings are keyed on `listen_includes_tailscale` (the literal
"tailscale" entry), while the probe runs on
`bind::needs_tailscale_probe(&cfg.listen)`. Key the warnings on
needs_probe, and word them so they are correct for both the keyword and
an explicit address. If the address is then refused, the error should
say why.

Factor the warning choice into a pure function and unit-test every case:
CLI missing, probe failed, no addresses in range, keyword only, explicit
IP only, and neither.

Owned files: crates/acd/src/daemon.rs (this block only), crates/acd/src/bind.rs.
```

### 5. README and setup guide

```text
The repository has no README. Write README.md and docs/setup.md for a
developer who wants to run acd with their own agents.

README: what the project does, the components (acd daemon, acd/1
protocol, Mac app), status (v1 in progress, personal trusted-host use),
and links to the setup guide, spec and roadmap. Keep it under 80 lines.

docs/setup.md: build and install acd, `acd init`, hooks install for
Claude Code and Codex, the T3 Code and herdr sources, remote machines
over Tailscale with `acd pair` and pinned TLS, and the Mac app. Include
these known limits from the follow-ups doc (P5 deferred):
- herdr alone gives only "Answer in terminal" cards. Option cards and
  replies need T3 or the hooks.
- On a remote Mac, the macOS firewall prompts for the unsigned `acd`
  binary on its first incoming connection.
- The Claude `http` hooks listen on loopback; the port-squatting risk is
  accepted for trusted hosts (spec §9).

Every command and flag must exist: check each one against
crates/acd/src/main.rs (clap definitions) and run `acd --help` and the
subcommand helps. Do not document behavior you cannot find in the code
or the spec.

Owned files: README.md, docs/setup.md.
```

### 6. acd/1 protocol guide for other clients

```text
The project goal includes a reusable daemon that other UIs can use. Write
docs/protocol.md: a client author's guide to acd/1. Source it from spec
§7 (read only; do not edit the spec), crates/acd-protocol (messages.rs,
model.rs, pairing.rs, schema.rs) and crates/acd-client.

Cover: transports (acd.sock locally; pinned TLS over Tailscale), the
pairing string and SPKI pin, the handshake, each message with a JSON
example, idempotency, request lifecycle and statuses, how to answer
(`option_id` xor `text`; option ids are opaque), errors, and forward
compatibility (unknown variants).

Every JSON example must validate against the schema. Add a test (for
example crates/acd-protocol/tests/protocol_doc.rs) that extracts the
fenced JSON examples from docs/protocol.md and validates them, so the
doc cannot drift. Where the spec and the code disagree, follow the code
and list each disagreement in the PR description.

Owned files: docs/protocol.md, crates/acd-protocol/tests/protocol_doc.rs.
```

### 7. Linux test soak

```text
Find flaky tests on Linux. Do not fix anything in this task.

Run `cargo test --workspace --locked` 30 times, then 30 more times with
the CPU loaded (for example `stress-ng --cpu $(nproc)` or a parallel
`cargo build` loop). Record each failure: test name, run number, load
condition and the failure output.

Then run each failing test 200 times alone and 200 times under load.

Write the results to docs/research/2026-10-12-linux-test-soak.md: the
environment (nproc, kernel, rustc, uid), the method, a table of flaky
tests with failure rates, and for each one the suspected cause with file
and line references. Mark causes you have not confirmed as unconfirmed.
Sandbox-only failures listed in CLAUDE.md go in their own section.

Owned files: that one research doc.
```

## Wave 2 (after P6 merges)

- Run `cargo fmt` on the whole workspace, alone, as its own PR. Merge it
  before anything else in this wave.
- Task 22: the shim ignores surplus reply bytes in a later read, and its
  `catch_unwind` wrapper has no test (`crates/acd/src/hook_shim.rs`).
- Task 7: escape strong RTL characters in command lines, or document the
  residual (`crates/acd-adapters/src/hooks/mapping.rs`).
- Task 4: `KeysTargets::remove` retires ids when no live target exists;
  check for self-deadlock if a holder re-enters `deliver`
  (`crates/acd-adapters/src/herdr/keys.rs`).
- `t3_conn.rs` `a_peer_that_stops_reading_trips_the_write_timeout`:
  widen its 5 s bound or fix the cause, using the soak results.
- A Codex agent's title showed `<environment_context>`. Find which
  adapter sets it and sanitize it.

## Keep local

These need macOS, the test machines or live agents:

- Mac app items in P5 deferred: hotkey, panel position, Keychain,
  notification permission, pairing import validation, focus after Cancel.
- `launchctl` service work.
- Live end-to-end runs (`just e2e`), v1 acceptance (spec §14) and the
  open live checks 10 to 14.
- Anything over Tailscale, minos or omarchy.
