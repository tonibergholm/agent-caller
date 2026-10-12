# Voice step 2, Task 2 report

## Implemented
`crates/acd-voice/src/listen.rs` (new) and `pub mod listen;` in `lib.rs`, with the plan's code and tests verbatim: constants, `Limits`, `TriggerEvent`, `Hold`, `toggle`, `ListenError`, `Unavailable`, `Capabilities`, `ListenEvent`, `SpeechInput`, `Recorder` (cap checked in `tick`, `stop`, `event`), `judge`, and `fake::{FakeSpeechInput, Call}`.

## Evidence
- RED: `cargo test -p acd-voice --locked listen::` failed to compile (`unresolved import fake`, `cannot find type Duration/VoiceText`) with only the tests present.
- GREEN: same command, `15 passed; 0 failed`.
- `cargo test --workspace --locked`: exit 0 (two further runs, no FAILED lines).
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: finished clean.

## Files changed
- crates/acd-voice/src/listen.rs (new)
- crates/acd-voice/src/lib.rs (+1 line)

## Deviations
- `git checkout p10-voice-speech` failed because that branch is checked out in another worktree (.worktrees/p10-voice-speech). I created `p10-voice-core` directly from `p10-voice-speech` with `git checkout -b`.
- Code and tests used as written; no changes were needed (it compiles under edition 2024, including the let-chain).

## Concerns
- One full-workspace run counted 2 lines matching `FAILED`; three later runs were clean. I did not capture which test. Likely a sandbox-sensitive test listed in CLAUDE.md or a flake. Not related to this change (new module only).
