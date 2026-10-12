# acd-app-core

The part of the agent-caller apps that does not depend on a platform:

- `setup::open_inbox`: loads the settings file, builds the inbox and starts it.
  `OpenOptions::local_machine` decides whether this machine's `acd.sock` is offered.
  The iOS shell passes `false`.
- `pairing::import_text`: turns clipboard text into a remote machine.
- `keychain` (Apple targets only): the Keychain secret store and `cached_secrets()`.
- `commands`: the command functions and `AppState`. The shells wrap them in
  `#[tauri::command]` functions with the same IPC names.

Used by `app/src-tauri` (the Mac app) and `apps/ios/src-tauri` (the iOS shell).

## Placement

This crate sits in the root workspace, not outside it, for these reasons:

- Its tests run in CI on Ubuntu and macOS. `cargo test --manifest-path` on
  `app/src-tauri` does not run the tests of path dependencies, so a crate outside
  the root workspace would have no CI run.
- The Keychain code is behind `cfg(target_vendor = "apple")`, and so is the
  `security-framework` dependency. The Linux build compiles the crate without them.
- The crate does not depend on Tauri. Functions that need the Tauri runtime take
  a `tokio::runtime::Handle` instead.

## Invariants

- `SERVICE_NAME` (`dev.agentcaller.inbox`) names the Keychain items. Changing it
  orphans the secrets that existing installs stored.
- The pairing error text is part of the UI. Keep it stable.
- Errors from pairing never include the clipboard text.
