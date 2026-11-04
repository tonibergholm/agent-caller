# Android client

A foreground inbox for the agent-caller daemon (`acd`) on Android, reached over
Tailscale. Roadmap cut-line item, first milestone of
[the feasibility study](research/2026-11-03-windows-android-feasibility.md)
(section E, step 3). **Status: builds a debug APK in a sandbox; never run on a
device or emulator.** Read [What was not verified](#what-was-not-verified)
before relying on any of it.

Scope: pair a machine by pasting the `acd://` line from `acd pair` (a QR code
is the same text; this app has no camera, so decode it elsewhere and paste),
list the cards, answer a request. No push notifications (the relay is a
separate change), no background work, no `local` machine, no panel, hotkey or
voice. The app is only live while it is on screen.

## Layout

| Path | What |
|---|---|
| `crates/acd-app-core/src/wrapped.rs` | The Android secret store: `KeyWrap` trait, `WrappedFileSecrets`, `install()`. Compiled and tested on every platform. |
| `crates/acd-app-core/src/nostore.rs` | `platform_secrets()` returns the installed store on Android (refusing until installed). Windows keeps the refusing store. |
| `apps/android/` | The shell. Its own Cargo workspace, outside the root workspace, like `apps/ios`. |
| `apps/android/src/model.rs` | The screen model, decided in Rust: which options exist and are enabled, or the read-only hint. |
| `apps/android/src/bridge.rs` | Opens the inbox over the store; pairing import, pick, answer, mark seen, remove. |
| `apps/android/src/jni_glue.rs` | JNI exports (strings in, JSON out, no logging, panics caught) and the Keystore cipher behind `KeyWrap`. |
| `apps/android/java/` | `MainActivity` (draws the model), `KeystoreCipher` (Android Keystore AES-GCM), `Native` (the JNI declarations). |
| `apps/android/build-apk.sh` | Debug APK build. |
| `apps/android/tools/` | `sandbox-ndk.sh` and `bionic-symbols.txt`: a stand-in link environment for a machine with no NDK. |
| `apps/android/jvm-test.sh`, `jvm-test/` | Runs the real JNI exports on a host JVM. |

## Why this is not a Tauri mobile app

`apps/ios` is a Tauri 2 shell. Tauri's Android target generates a Gradle
project that needs the Android Gradle Plugin (AGP), the SDK and the NDK. None
of the three could be installed in the sandbox this was written in. Probed on
2026-11-04, from a Linux x86_64 sandbox behind an egress proxy that allows a
fixed list of hosts:

| Needed for | Host | Result |
|---|---|---|
| SDK command-line tools, NDK, platforms, build-tools, emulator, system images | `dl.google.com` | CONNECT 403 (policy denial) |
| AGP, aapt2, R8, AndroidX | `maven.google.com` | answers 200 for the index, but every artifact 301-redirects to `dl.google.com/dl/android/maven2/...`, which is 403 |
| A bionic sysroot from elsewhere | `mirror.bazel.build`, `packages.termux.dev`, `android.googlesource.com` | unreachable |
| Docker images that bundle an SDK (`ghcr.io/cirruslabs/android-sdk:*-ndk`) | `ghcr.io` answers, the blob host `pkg-containers.githubusercontent.com` is 403; Docker Hub is rate-limited; no Docker daemon runs in the sandbox | blocked |
| NDK from package managers | Ubuntu `google-android-ndk-*-installer` downloads from `dl.google.com`; conda-forge has only `rust-std-*-android`; PyPI has no NDK; `ziglang` (installed and tried) cannot provide Android's libc (`unable to provide libc for target aarch64-linux...android`) | no NDK |

What is reachable: the Ubuntu archive, Maven Central, `plugins.gradle.org`,
crates.io, `static.rust-lang.org`, PyPI, conda-forge, npm.

So the fallback in the task applies: a Rust library, JNI, and a small activity.
The activity is Java, not Kotlin. The Kotlin compiler jars are on Maven Central
(a first fetch returned HTTP 429, a retry later returned 200), but the only
dex tool available here is Debian's `dx` 10.0.0, which does not desugar, and
current Kotlin emits `invokedynamic` lambdas and a standard library that needs
desugaring or `d8`. I did not try it. Nothing in the activity needs Kotlin; it
is written without lambdas for the same reason. The UI is plain
`android.widget` views: no WebView, no Svelte, no resources.

If the network policy changes, the shell crate is the part to keep: moving to
Tauri mobile means adding a Tauri `lib.rs` beside the bridge, as `apps/ios`
does (`acd_app_core::wrapped` is already shared).

## Build

Every step I ran, in order. All commands from the repository root unless noted.

```sh
# Toolchain (Ubuntu 24.04; apt versions in brackets)
apt-get install -y aapt zipalign apksigner dalvik-exchange android-sdk-platform-23 \
    lld libc6-dev-arm64-cross dexdump libandroid-json-org-java
#   aapt 1:14~beta1-2build3, zipalign 1:10.0.0+r36-1ubuntu2, apksigner 31.0.2-1ubuntu1,
#   dalvik-exchange 10.0.0+r36-4 (dx), android-sdk-platform-23 6.0.1+r72-6ubuntu1 (android.jar, API 23)
#   JDK 21.0.12, clang 18.1.3, rustc 1.97.0
rustup target add aarch64-linux-android x86_64-linux-android
rustup component add rust-src          # LLVM libunwind source, for the stand-in sysroot

# Stand-in for the NDK (sandbox only, see below)
apps/android/tools/sandbox-ndk.sh /path/to/ndk-standin

# The APK
ACD_SANDBOX_NDK=/path/to/ndk-standin just android-apk
#   = apps/android/build-apk.sh  ->  apps/android/build/agent-caller-debug.apk
```

On a machine with a real NDK (r26 or later) the stand-in is not needed:

```sh
ANDROID_NDK_HOME=/path/to/ndk just android-apk
```

That path sets the NDK's clang as compiler and linker for both targets and is
**untested**: there was no NDK to run it against. It is the standard recipe; if
it fails, fix the script, not the code.

`build-apk.sh` does: `cargo build --release --locked` for `aarch64-linux-android`
(`arm64-v8a`) and `x86_64-linux-android` (`x86_64`); `javac -source 8 -target 8
-bootclasspath android.jar`; `dalvik-exchange --dex` (or `d8` if on `PATH`, also
untested); `aapt package` and `aapt add` (manifest, `classes.dex`, the two
`.so` files); `zipalign -f 4`; `apksigner sign` with a debug key that
`keytool` creates in `apps/android/build/` (git-ignored; no key is in the
repository, and no release signing is set up). The version name is the crate
version, kept in step with the workspace by `scripts/check-version.sh`.

Manifest: `minSdk` 23 (`KeyGenParameterSpec`, AES-GCM in the Keystore),
`targetSdk` 28, only the `INTERNET` permission, `allowBackup=false`,
`usesCleartextTraffic=false`. `targetSdk` is 28 and not 23 because newer
Android releases refuse to install apps that target very old API levels. A
Play-Store-grade target (current API level) needs a matching `android.jar` and
is a follow-up. The window is `FLAG_SECURE`, so the pairing line stays out of
screenshots and the recents screen.

### The stand-in sysroot (`tools/sandbox-ndk.sh`)

Not an NDK. It exists so that `rustc` can link a `cdylib` for Android where the
NDK cannot be downloaded:

- `libc.so`, `libdl.so`, `libm.so`, `liblog.so`: stub shared libraries that
  export the symbols in `tools/bionic-symbols.txt` and nothing else. The linker
  records them as `NEEDED` and leaves the symbols undefined; Android's dynamic
  linker resolves them against the real bionic at load time. Nothing is
  resolved against glibc.
- `libunwind.a`: LLVM libunwind from the `rust-src` component, compiled by the
  host clang for the Android target. Rust's Android std links `-lunwind`.
  glibc's `__errno_location` is renamed to bionic's `__errno` on the command line.
- `crt_so.o`: defines the hidden `__dso_handle` that std refers to, as the NDK's
  `crtbegin_so.o` does.
- C headers for `ring`'s C and assembly code come from the host's glibc
  (declarations only).

`bionic-symbols.txt` is the list of C library symbols the library imports
today. It was produced from the linked library and **not checked against a real
bionic**: every name is one I recognise as a bionic export, and none is
glibc-specific, but that is judgement, not a check. A new dependency that needs
another symbol fails the link with `undefined symbol`; add it to the list.
`getrandom` (bionic API 28) and `gettid` are weak imports in the linked
library; on Android 6 to 8 the library depends on the callers handling a
missing `getrandom`, which was not verified.

## Secrets

`SecretStore` on Android is `WrappedFileSecrets` over a `KeyWrap`. The shell
installs it once at start (`wrapped::install(filesDir/secrets, cipher)`); until
then `platform_secrets()` returns a store that refuses every call. Each secret
is one file in the app's private directory, named by the hex of its key. The
file is `ACDW\x01` followed by `IV(12) || AES-256-GCM ciphertext || tag(16)`. The AES key is created
on first use in the Android Keystore under the alias
`dev.agentcaller.inbox.secrets.v1` and never leaves it. The GCM associated data
is `dev.agentcaller.secret.v1\0` followed by the key name, so a blob copied to
another machine's file does not open.

Fail closed, all tested:

- A missing file is `Ok(None)`. A file the cipher refuses, a damaged or foreign
  file, a bad header, a Keystore that cannot be used, or a key that is gone is
  an `Err`; the machine shows "The saved pairing cannot be read here. Remove
  this machine and pair it again." and is never taken for "not paired" or
  online. Re-pairing writes a new file (and a new Keystore key if needed).
- A write that fails keeps the old file (temp file, `sync_all`, rename). A
  pairing whose secret cannot be stored commits no machine and writes no
  settings.
- Errors carry constant text (and an `io::ErrorKind` at most): never a secret,
  a key name or a path. The JNI layer and the activity log nothing.
- The Java side never throws across JNI: `null` means the Keystore cannot be
  used, an empty array means the cipher refused.
- The PR #79 platform rule stays: `acd-securefs` owner checks are off on
  Android (`owner_checks` is unix except Android). App-private storage is the
  boundary there.

The Keystore key is not bound to user authentication. A device with a
compromised OS or root can use the key while the app's data is readable; the
wrap protects against backups, file exfiltration and other apps, not against
code running as the app. `allowBackup=false` keeps the files out of backups.

## Test plan (no device, no emulator)

```sh
cargo test -p acd-app-core --locked                      # the store, with a fake Keystore
(cd apps/android && cargo test --locked)                 # view model, bridge, pairing import
(cd apps/android && cargo clippy --all-targets --locked -- -D warnings)
ORG_JSON_JAR=/usr/share/java/json-latest.jar just android-test   # also runs the host JNI test
```

- `wrapped.rs`: round trip; the file does not hold the plaintext; a missing
  file is `None`; a blob moved to another key's file is refused (**this test
  fails with the AAD removed**: tried); damaged, truncated, empty and foreign
  files are errors, never `None`; an unavailable Keystore fails reads and
  writes and keeps the old file; non-UTF-8 plaintext is an error; keys cannot
  name a path; errors never echo values, keys or paths; `install` keeps the
  first store.
- `pairing.rs`: with the Android "not installed" store a pairing commits
  nothing and keeps the line (a second case beside the Windows one).
- `model.rs`: options only on a live answerable card, a disabled option shows
  "Not supported here", a missing `answer` capability / `none` answer path /
  unparsed / not-open / offline request has no options and says where to
  answer; busy cards show neither; free text only when answerable; unusable
  secrets override the machine status. The cards come from the client's own
  `build_view`, so the answerable rule under test is the client's.
- `bridge.rs`: pairing from pasted text (whitespace trimmed); bad lines and
  LAN addresses refused without echoing; a pairing survives a restart through
  the wrapped store and the token is in no plain file; an unavailable Keystore
  commits nothing; a machine whose secret cannot be opened says so.
- `jvm-test.sh`: the real library, built for the host, loaded by a JVM through
  the real `Java_dev_agentcaller_inbox_Native_*` exports, with a software
  AES-GCM double of `KeystoreCipher` (same contract). Four runs against one
  data directory: pair (bad line, LAN line, good line, Keystore unavailable,
  another Java thread, unknown card/action refusals); restart and read the
  secret back; restart with the Keystore unavailable; restart after damaging
  the secret file. This exercises the JNI argument handling, `JniKeyWrap`
  (global ref, byte-array round trips, null/empty contract) and the store
  together. It does not exercise a native worker thread attaching to the VM:
  the client reads secrets only on the calling thread today, so that path
  (`attach_current_thread`) is written but unexercised.

On the APK itself:

```sh
aapt dump badging apps/android/build/agent-caller-debug.apk
aapt dump xmltree apps/android/build/agent-caller-debug.apk AndroidManifest.xml
apksigner verify --verbose --print-certs apps/android/build/agent-caller-debug.apk
zipalign -c -v 4 apps/android/build/agent-caller-debug.apk
unzip -l apps/android/build/agent-caller-debug.apk
for abi in arm64-v8a x86_64; do
  unzip -p apps/android/build/agent-caller-debug.apk lib/$abi/libagent_caller_android.so > /tmp/x.so
  readelf -h /tmp/x.so | grep Machine; readelf -d /tmp/x.so | grep NEEDED
  readelf --dyn-syms -W /tmp/x.so | grep -c GLIBC      # must print 0
done
```

`apkanalyzer` is part of the Android command-line tools and is not available in
the sandbox, so it was not run; `aapt dump` and `dexdump` stand in for it.

## What was verified

In the sandbox, 2026-11-04:

- All Rust tests above pass; clippy is clean for the workspace and for
  `apps/android`; `cargo check -p acd-app-core --target aarch64-linux-android`
  passes (the Android cfg branch of `platform_secrets()` compiles).
- `build-apk.sh` produces `agent-caller-debug.apk` (about 3.8 MB): manifest,
  `classes.dex` (DEX 035, all activity, cipher and `Native` classes present per
  `dexdump`), and `lib/arm64-v8a` and `lib/x86_64`.
- `aapt dump badging`: package `dev.agentcaller.inbox`, version 0.9.0
  (code 900), `sdkVersion` 23, `targetSdkVersion` 28, only
  `android.permission.INTERNET`, launchable `MainActivity`, `native-code:
  'arm64-v8a' 'x86_64'`.
- `apksigner verify`: valid v1, v2 and v3 signatures from the debug key.
  `zipalign -c -v 4`: verified.
- Both `.so` files: right ELF machine (AArch64, x86-64); `NEEDED` is only
  `libdl.so`, `libm.so`, `libc.so`; no `GLIBC` symbols; LOAD segments aligned
  to 16 KiB; the eight `Java_dev_agentcaller_inbox_Native_*` exports present.
- `jvm-test.sh`: all four runs pass.
- `scripts/check-version.sh` and `scripts/tests/run.sh` pass with the new
  `apps/android` checks (the two new drift tests fail without the check).

## What was not verified

- **Running on a device or an emulator. Nothing here has run on Android.** No
  emulator: the SDK emulator and system images come from `dl.google.com`
  (blocked), and the sandbox has no `/dev/kvm`. So not verified: the library
  loads under bionic (`System.loadLibrary`), the stand-in link matches the real
  bionic ABI, the activity starts and draws, the Keystore key is created and
  used, the APK installs on any API level, the 16 KiB alignment is accepted by
  a 16 KiB-page device.
- The Keystore: `KeystoreCipher` compiles against the API 23 `android.jar` and
  the contract is tested through the JNI with a software double. The real
  `AndroidKeyStore` provider was never called. Key invalidation (for example a
  lock-screen reset) is handled by design (unwrap refuses, the user re-pairs)
  but never seen.
- Tailscale: the phone must route `100.64.0.0/10` through the tailnet; the
  client's address check is the same as iOS. Not run. No connection to a
  daemon was made from Android code, so the TLS pin check, the handshake and
  answering a live request over the network are untested on this target (the
  same client code is covered by the workspace tests on the host).
- The real-NDK path of `build-apk.sh` and the `d8` path.
- Android 6 to 8 (`getrandom` weak import), screen sizes, dark mode, rotation
  (the activity handles `configChanges` itself and redraws from the model).
- `Remove`, `Mark seen` and `Undo` buttons were never tapped; the answer path is
  tested at the bridge level for refusals only (there is no daemon in the unit
  tests), not for a successful answer.
- Play Store requirements (target API level, 64-bit, signing).

## Follow-ups

- Owner: install on a phone and check the list in the iOS README's "Checked on
  a real device" section (pair, hold a request, answer allow, answer after a
  relaunch, daemon stopped).
- CI (not in this PR; `ci.yml` is owned elsewhere). Proposed job on
  `ubuntu-latest`, which has an NDK at `$ANDROID_NDK_HOME`:
  `cd apps/android && cargo test --locked && cargo clippy --all-targets --locked -- -D warnings`,
  then `rustup target add aarch64-linux-android x86_64-linux-android` and
  `ANDROID_NDK_HOME=$ANDROID_NDK_HOME apps/android/build-apk.sh` with the
  build tools on `PATH`. The last step would also be the first run of the
  real-NDK path.
- A Gradle or Tauri-mobile build once `dl.google.com` is reachable, for a
  current target SDK, Kotlin, AndroidX and proper resources/icon.
- A QR scanner (CameraX or ML Kit) feeding the same `importPairing`.
- FCM wake-up after the relay lands (feasibility doc, section D).
