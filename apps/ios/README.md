# Agent Caller for iOS

A foreground inbox for the agent-caller daemon (`acd`). The phone reaches the
daemon over Tailscale. It runs `acd-client` and the shared Svelte UI from
`app/`.

Scope of v1: foreground only. No push notifications, no relay, no background
refresh. Requests show up only while the app is open. The local machine
(`acd.sock`) is never offered on iOS. Panel, tray, hotkey, voice and Mac
notifications are not part of this shell.

## Layout

- `src-tauri/`: the Rust shell (`agent-caller-ios`). Its own Cargo workspace,
  outside the root workspace, like `app/src-tauri`.
- `src-tauri/src/commands.rs`: the command subset. IPC names match the Mac app.
- `src-tauri/src/lib.rs`: the mobile entry point and the simulator-only dev pairing.
- `package.json`: the Tauri CLI, pinned to the version the Mac app uses. The
  generated Xcode build phase runs `npm run -- tauri ios xcode-script`, so npm
  finds this file from `src-tauri/gen/apple`.
- `src-tauri/gen/`: the generated Xcode project. Git-ignored; created by `ios:init`.

Shared code: `crates/acd-app-core` (inbox setup, pairing, Keychain store and the
platform-independent commands) and `app/` (the Svelte UI, built into `app/dist`).

## Prerequisites

- Xcode with an iOS simulator runtime (Xcode > Settings > Platforms).
- Rust from rustup, with the iOS targets:
  `rustup target add aarch64-apple-ios-sim` for the simulator, and
  `rustup target add aarch64-apple-ios` for a device.
- Node 24 and npm.
- `xcodegen`: `brew install xcodegen`.

The Homebrew `cargo` has no iOS standard library, and the build fails with
"can't find crate for `core`" if it comes first on `PATH`. Put the rustup
toolchain first, in the shell you build from:

```sh
export PATH="$(dirname "$(rustup which cargo)"):$PATH"
```

Xcode runs the build phase with its own environment, and a build started from
the Xcode window does not read your shell profile. To build from Xcode, start
it from that shell: `open -a Xcode`.

## Build

From the repository root:

```sh
(cd app && npm ci)            # the frontend the iOS build packages
(cd apps/ios && npm ci)       # the Tauri CLI
(cd apps/ios && npm run ios:init)   # generates src-tauri/gen/apple (rerun after changing tauri.conf.json)
(cd apps/ios && npm run ios:build)  # debug build for the simulator (aarch64-sim)
```

`ios:init` must run again after any change to `tauri.conf.json` (identifier,
name) or to the plugins. It is not committed, so a fresh checkout needs it once.

## Run in the simulator

```sh
xcrun simctl boot "iPhone 18 Pro"          # or any installed iOS 27 simulator
open -a Simulator
xcrun simctl install booted "apps/ios/src-tauri/gen/apple/build/arm64-sim/Agent Caller.app"
xcrun simctl launch booted dev.agentcaller.inbox.ios
```

The simulator shares the Mac's loopback, so a daemon on the Mac is reachable
at `127.0.0.1`.

### Live development

```sh
(cd apps/ios && npm run ios:dev -- "iPhone 18 Pro")
```

This starts the Vite dev server for the iOS layout (`app/`, port 1420) and runs
the app in the simulator against it, so frontend edits reload without a rebuild.
It needs the same `PATH` as the build. The first run installs `ios-deploy` with
Homebrew. A physical device is not covered: it needs the dev server to listen
on the Mac's network address.

### Pairing

Run `acd pair` on the daemon's machine and copy the line it prints. On the
phone, open Settings (the gear in the header) and tap Add from clipboard. iOS
may ask whether the app can read the clipboard; allow it. The clipboard
route works with Universal Clipboard from a Mac. Only Tailscale addresses are
accepted. The app stores the secret in the iOS Keychain. Clear the clipboard
afterwards: the line contains the pairing secret.

### Dev pairing in the simulator

To skip typing the line in the simulator, a debug
build imports one `acd pair` line at launch when `ACD_IOS_DEV_PAIRING_FILE`
names a file that holds it:

```sh
SIMCTL_CHILD_ACD_IOS_DEV_PAIRING_FILE=/path/to/pairing.line \
  xcrun simctl launch booted dev.agentcaller.inbox.ios
```

The import goes through the real Keychain store. The line is never logged.
Keep the file mode at 0600 and delete it when you are done. The code is
compiled only for debug builds on the simulator
(`cfg(all(debug_assertions, target_os = "ios", target_abi = "sim"))`). Device and
release builds do not contain it.

## Run on a device with a free personal team (manual steps)

The owner does these by hand. Nothing in the repository sets a team or
identity.

1. Xcode > Settings > Accounts: sign in with the Apple ID. This creates the
   personal team.
2. Open `apps/ios/src-tauri/gen/apple/agent-caller-ios.xcodeproj`. Select the
   target `agent-caller-ios_iOS`, open Signing & Capabilities, and choose the
   Personal Team under Team. Xcode writes the team into the generated project,
   which is not committed. Do not put a team ID into tracked files.
3. If Xcode reports that the bundle identifier is taken, change `identifier`
   in `src-tauri/tauri.conf.json` to a unique value, then run `ios:init` again.
4. On the iPhone: enable Developer Mode (Settings > Privacy & Security). Trust
   the developer under Settings > General > VPN & Device Management. Install
   the Tailscale iOS app and sign in to the same tailnet as the daemon.
5. Build with `npm run tauri -- ios build --debug --target aarch64` and run
   from Xcode.

Free-team limits, from Apple's comparison page (checked 2026-10-12): a device
install and its provisioning profile expire after 7 days, so the app must be
rebuilt and reinstalled after that. A free account registers up to 3 devices
and 3 apps per device. Devices registered under an earlier or lapsed paid
membership count against the 3 and a free account cannot remove them. Xcode
then fails with "Your development team has reached the maximum number of
registered iPhone devices", and the install fails with "This provisioning
profile cannot be installed on this device". Use a different Apple ID (a new
Personal Team has its own 3 devices), or pay for the Developer Program.

Secrets: on iOS, machine secrets are stored `AccessibleWhenUnlockedThisDeviceOnly`
(not backed up, not moved to another device). `KeychainSecrets::write` in
`crates/acd-app-core/src/keychain.rs` recreates the item to set the class, because
an update keeps an old item's class. Release gate: confirm on a device that a
secret stored before this change is replaced with the new class on the next
pairing, and that the machine stays Online after a relaunch.

Checked on a real device (iPhone 16 Pro, iOS 27.0.1, 2026-10-13, debug build, a
scratch daemon reached over Tailscale):

- Install and launch, with a free personal team.
- Pairing from the clipboard (Universal Clipboard from the Mac), over the
  Tailscale address.
- A held Claude permission request showed up and was answered Allow; a second
  one, sent after a force-quit and relaunch, was answered Deny. Both answers
  reached the hook. So the stored secret survived the relaunch.
- iOS showed no Local Network prompt for the Tailscale route (100.64.0.0/10).
- With the daemon stopped, the app showed the connection error and kept the
  last requests, dimmed and marked offline.

Not verified: that the machine shows Online right after a relaunch, before any
new request (it was not looked at; the answer after the relaunch shows the
connection came back). The old-class Keychain item replacement described
above (needs a secret stored by a build from before that change). A release
build, and a physical device with `ios:dev`.
