# Omarchy Task 1 report: action owners and outcome cards

Branch `p9-omarchy-impl` (from `p9-omarchy`).

## Implemented

- `acd_client::actions::ActionOwner` (`DEFAULT = ActionOwner(0)`), `Action.owner`,
  `Actions::cancel_latest_of`, `Actions::cancel_owned`.
- `ActionEvent.card_key`; `Inbox::emit` takes `&Action` (14 call sites).
- `Origin` struct threaded through `pick_with`, `answer_text_with`, `reply_with`.
- New `Inbox::{pick_owned, answer_text_owned, reply_owned, cancel_owned, escape_owned}`.
  Unowned and guarded forms keep their signatures and use `ActionOwner::DEFAULT`;
  `escape()` still cancels the newest action of any owner.
- `owner` added to the `Action` literal in `crates/acd-voice-client/tests/port.rs`.
- Plan tests added verbatim: `actions::tests::owners_cancel_only_their_own` and 8
  integration tests in `crates/acd-client/tests/inbox.rs`.

## RED

Tests were added before any implementation.

- `cargo test -p acd-client --locked --lib actions::tests::owners`:
  `error[E0433]: cannot find type ActionOwner`, `no field owner on type Action`.
- `cargo test -p acd-client --locked --test inbox owned`:
  `error[E0432]: unresolved import acd_client::actions::ActionOwner`,
  `no method named pick_owned`.

## GREEN

- `cargo test -p acd-client --locked`: all suites pass (64 lib, 14, 21, 7, 66, ...).
- `cargo test --workspace --locked`: no failures (run on macOS, uid 501). `cargo clippy --workspace --all-targets --locked -- -D warnings`: clean after one fix (the plan's perl rewrite turned `emit(action, ..)` into `emit(&action, ..)` at inbox.rs:716, a needless borrow; reverted that one site).

## Files changed

`crates/acd-client/src/{actions,inbox,voice}.rs`, `crates/acd-client/tests/inbox.rs`,
`crates/acd-voice-client/tests/port.rs`. Nothing under `app/`; `npm run verify` not needed.

## Deviations

- The plan says 15 `emit` call sites; there are 14 `emit(&x.id, ..)` matches. All converted.
- The plan says to run on Linux; this session ran on macOS (Darwin). The Linux CI leg is
  not exercised here.
- `p9-omarchy` is checked out in another worktree, so `p9-omarchy-impl` was created with
  `git checkout -b p9-omarchy-impl p9-omarchy`.

## Concerns

None for the code. Mac app behaviour is unchanged: the app only uses unowned forms.
