# iOS and Android

Both phone apps are **foreground clients**. They show requests while the app
is open and let you answer them. They reach remote machines over Tailscale.
Neither is in the release files: you build them from source.

## What works

- Pair a machine by pasting the line from `acd pair`
  ([pair-a-remote-machine.md](pair-a-remote-machine.md)). Only Tailscale
  addresses are accepted.
- List the cards, answer a permission or a single-select question, mark
  seen, remove a machine. The answer has the same 2-second undo. The iOS app
  shares the desktop UI, so it also has typed answers and replies; the
  Android bridge has pairing, pick, answer, mark seen and remove only
  (`docs/android.md`).
- Cards that cannot be answered whole are read-only, as on the desktop.
- The secret is stored in the iOS Keychain (this-device-only, not backed up)
  or, on Android, in a file wrapped with an Android Keystore key.
- With the daemon stopped, the iOS app shows the connection error and keeps
  the last requests, dimmed and marked offline.

## What does not work

- **No push.** Requests appear only while the app is on screen. There is no
  relay, no background refresh, no wake-up.
- No `local` machine (`acd.sock`), no panel, tray, hotkey, voice or desktop
  notifications.
- Android has no camera: a QR code must be decoded elsewhere and pasted.
- iOS: a free personal team expires the install after 7 days and allows 3
  devices. Rebuild and reinstall after that.

## iOS: build and install

Needs a Mac with Xcode, rustup with the iOS targets, Node 24 and `xcodegen`.
Put the rustup toolchain first on `PATH`:

```sh
export PATH="$(dirname "$(rustup which cargo)"):$PATH"
rustup target add aarch64-apple-ios aarch64-apple-ios-sim
(cd app && npm ci)
(cd apps/ios && npm ci && npm run ios:init)
(cd apps/ios && npm run ios:build)        # debug build for the simulator
```

Simulator: boot it, then
`xcrun simctl install booted "apps/ios/src-tauri/gen/apple/build/arm64-sim/Agent Caller.app"`
and `xcrun simctl launch booted dev.agentcaller.inbox.ios`.

Device with a free personal team: sign in to Xcode with your Apple ID, open
`apps/ios/src-tauri/gen/apple/agent-caller-ios.xcodeproj`, choose your
Personal Team under Signing & Capabilities for `agent-caller-ios_iOS`, enable
Developer Mode on the phone, trust the developer under Settings → General →
VPN & Device Management, install Tailscale on the phone (same tailnet), then
`npm run tauri -- ios build --debug --target aarch64` and run from Xcode. If
the bundle identifier is taken, change `identifier` in
`apps/ios/src-tauri/tauri.conf.json` and run `ios:init` again. Full steps and
the free-team limit errors: [apps/ios/README.md](../../apps/ios/README.md).

Pair: run `acd pair` on the daemon's machine, copy the line (Universal
Clipboard works from a Mac), open Settings (the gear), tap **Add from
clipboard**, allow the clipboard prompt.

## Android: build and sideload

Status: **a debug APK that has never run on a device or emulator.** Treat it
as untested. See [docs/android.md](../android.md) for what was and was not
checked.

Build (needs a real NDK r26 or newer, `aapt`, `zipalign`, `apksigner`, and
`dx` or `d8`):

```sh
rustup target add aarch64-linux-android x86_64-linux-android
ANDROID_NDK_HOME=/path/to/ndk just android-apk
# output: apps/android/build/agent-caller-debug.apk
```

The real-NDK path of `build-apk.sh` is itself untested. Without an NDK, the
sandbox stand-in in `docs/android.md` builds the APK.

Sideload with the standard Android tool (this step is not described in the
repository docs and was not run):

```sh
adb install apps/android/build/agent-caller-debug.apk
```

The APK is signed with a throwaway debug key created in
`apps/android/build/`. `minSdk` is 23. Install Tailscale on the phone and
pair by pasting the line.

Problems: [troubleshooting.md](troubleshooting.md).

Verified: the scope and limits, from `apps/ios/README.md`, `docs/android.md`
and `README.md`;
Not verified: every build and install command above (no
Mac, Xcode, NDK or phone in this session), the Android app on any device,
and `adb install`. The iOS README records a debug build checked on one
iPhone on 2026-10-13; a release build was not.
