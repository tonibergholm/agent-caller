# agent-caller

An open-source inbox for coding agents. A Rust daemon (`acd`) collects
permission requests and questions from Claude Code, Codex, herdr and T3
Code on local and remote machines. Clients answer them over the acd/1
protocol. The reference client is a Tauri menubar app for macOS.

## Layout

- `crates/acd-protocol`: acd/1 wire types, JSON schema, pairing strings.
- `crates/acd-core`: request engine, registry, server, TLS, idempotency.
- `crates/acd-adapters`: sources. `herdr/` (screen and keys), `t3/` (T3
  Code), `hooks/` (Claude Code and Codex hooks).
- `crates/acd-client`: client library used by the app.
- `crates/acd-securefs`: owner-only file access.
- `crates/acd`: the `acd` binary: daemon, CLI, hook shim, service
  install, secrets and the hook token store.
- `app/`: Svelte frontend and `app/src-tauri` backend. It is excluded
  from the Cargo workspace.
- `tests/fixtures/`: recorded herdr screens and other fixtures.

## Docs

- Spec (authoritative): `docs/superpowers/specs/2026-10-07-agent-inbox-design.md`
- Roadmap: `docs/superpowers/plans/2026-10-07-roadmap.md`
- Deferred items and known flakes: `docs/superpowers/plans/2026-10-07-p1-followups.md`
- Per-phase plans: `docs/superpowers/plans/`
- Live check records and research: `docs/research/`

Read the spec section and the follow-ups entry for an area before you
change it. If the code and the spec disagree, report it. Do not change
either one to match the other without saying so.

## Commands

Rust workspace, from the repository root:

```sh
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```

App, from `app/` (macOS only, because it builds the bundle):

```sh
npm ci && npm run verify
```

CI runs the workspace tests and clippy on Ubuntu and macOS, and the app
on macOS. A change is ready when both workspace commands pass.

## Rules

- T3 message text never reaches logs. Log tags, lengths and hashes only.
- Fail closed. If the daemon cannot prove which agent and screen an
  answer belongs to, it does not deliver the answer.
- Secrets and tokens never go to stdout in tests. Capture the output in
  the test harness.
- Do not run `cargo fmt` on the workspace. The tree is not formatted
  yet, and a full format pass is its own change. Keep diffs to the lines
  you need.
- Hook endpoint tests use the `settle` and `hold_and_close` helpers in
  `crates/acd-adapters/tests/hooks_endpoint.rs`.
- CLI tests get ports from `free_port` in `crates/acd/tests/common/mod.rs`.
- Add a test for every behavior change. A test for a fix must fail
  without the fix.

## Platform limits

`launchctl` service install, the Keychain, notifications and the Tauri
bundle work only on macOS. Live checks need herdr, T3 Code, Tailscale
and the test machines. Linux sessions cannot verify any of these. Say
so when your change touches them.

The tests below fail in sandboxes that ignore special file mode bits or
restrict `ps`, and pass outside them:

- `secrets::tests::repair_mode_clears_special_bits_and_reports_them`
- `init_repairs_a_setuid_secret_and_a_loose_t3_token`
- `hooks::claude_sessions::tests::the_system_check_recognises_this_process`

Tests that expect a permission error also fail when run as root. Check
`id -u` before you treat such a failure as a regression.
