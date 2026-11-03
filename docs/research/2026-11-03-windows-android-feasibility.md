# Windows and Android feasibility

Date: 2026-11-03. Scope: roadmap "Road to 1.0" step 4
([roadmap](../superpowers/plans/2026-10-07-roadmap.md)). Docs only: no code,
Cargo or cfg changes. Read against `main` at `108034c`.

Host: Linux x86_64 cloud sandbox, rustc 1.97.0, rustup 1.28.2. No Windows
machine, no Android device or emulator, no Mac.

## Evidence levels

Every claim carries one tag:

- **[compiled]**: `cargo check --locked --target <t>` ran here on an
  unmodified tree. Type check only: nothing was linked or run.
- **[stub]**: the same check in a throwaway scratch worktree where
  `acd-securefs` was replaced by a Windows stub, to see errors hidden behind
  it. Never committed.
- **[scan]**: read from the source; not compiled for that target.
- **[docs]**: an external page, URL and read date given.
- **[guess]**: inference, not verified.

## Toolchains used

| Target | Setup | Result |
|---|---|---|
| `x86_64-pc-windows-gnu` | `rustup target add`, `apt-get install gcc-mingw-w64-x86-64` (13.2.0) for ring's C code | Works. `ring` builds; `acd-core` (rustls + ring) checks. |
| `x86_64-pc-windows-msvc` | `rustup target add` only | std installed; no `cl.exe` or Windows SDK headers, so not attempted. |
| `aarch64-linux-android` | `rustup target add`. The NDK download (`dl.google.com`) is blocked by the sandbox network policy (HTTP 403 at the proxy). | Without a C compiler, `ring`'s build script fails: `failed to find tool "aarch64-linux-android-clang"`. With host `clang --target=aarch64-linux-android24` plus glibc arm64 headers (`libc6-dev-arm64-cross`) for ring's C code, the Rust crates check. This tells us the Rust code type-checks for `target_os = "android"`. It says nothing about linking against bionic. |

## Summary of blockers

| # | Blocker | Where | Evidence | Recommended option | Size |
|---|---|---|---|---|---|
| 1 | `acd-securefs` does not compile on Windows (14 errors: `std::os::unix`, `std::os::fd`, `libc::geteuid`, `O_NOFOLLOW`…) | `crates/acd-securefs/src/lib.rs:5,42,66-99,109-125` | [compiled] | Windows impl: reparse-point refusal + owner SID + DACL check. For a client-only first step, a Windows branch that always refuses (fail closed) is enough. | S (client) / M (daemon) |
| 2 | Every crate that depends on securefs fails: `acd-client`, `acd-adapters`, `acd-app-core`, `acd-voice-client`, `acd-inbox`, `acd` | dependency graph | [compiled] | Fixed by 1 | — |
| 3 | `acd-client` local transport and settings file modes | `conn.rs:160` (`UnixStream`), `settings.rs:7,291,298` (`PermissionsExt`, `.mode(0o600)`) | [stub] (4 errors) | `local` endpoint unavailable on Windows for now; settings file in per-user AppData | S |
| 4 | Local socket `acd.sock` and hook socket `hook.sock` | `acd/src/local_socket.rs:7-9,32-40,70-85`, `daemon.rs:548,703`, `status.rs:19`, `hook_shim.rs:8-9,212,227` | [scan] (hidden behind 5) | Named pipes with a user-only DACL, and server-SID verification in the shim | M |
| 5 | `acd-adapters`: herdr Unix socket, hook `UnixListener`, T3 token mode check | `herdr/client.rs:11-12,148`, `herdr/discovery.rs:5,13,85`, `hooks/endpoint.rs:20,545,558`, `t3/reconciler.rs:262-278` | [stub] (8 errors) | herdr off on Windows; hook pipe listener; T3 token checked through securefs | M |
| 6 | Secrets and locks: `openat`/`renameat`/`flock`/modes | `acd/src/secrets.rs:3,36,116-280`, `tls.rs:4,31-36`, `writer_lock.rs:4-61`, `local_socket.rs:38` | [scan] | Owner-only files with an explicit protected DACL; `LockFileEx` for locks | M |
| 7 | Service install supports macOS and Linux only | `acd/src/service.rs:309,363,386-387` (bails with "supports macOS and Linux") | [scan] | None in v1 of Windows `acd`; then a per-user Task Scheduler logon task | S |
| 8 | Claude process verification has no Windows branch | `hooks/claude_sessions.rs:106-181` (`macos`/`linux` only; others return `Err(())` at `:178-181`) | [scan] | Already fails closed (scan marked incomplete). Later: `OpenProcess` + `GetProcessTimes` | S |
| 9 | Hook install reads modes and uids | `acd/src/hooks_install/mod.rs:148,228,251-319` | [scan] | Through the securefs Windows impl | S |
| 10 | App-core secret store is Apple-only | `acd-app-core/src/lib.rs:12-13`, `Cargo.toml` (`security-framework`, `cfg(target_vendor = "apple")`) | [compiled] (Android: the shell's only error) | Credential Manager (Windows), Keystore-wrapped file (Android) | S each |
| 11 | Mac app is macOS-shaped: `tauri-nspanel`, Swift bridge, objc2 notify, Accessory policy | `app/src-tauri/Cargo.toml:35,43-52`, `src/main.rs:30,72`, `src/panel.rs:11`, `src/notify.rs:142-144` | [scan]; nspanel re-exports `objc2` with no cfg (`tauri-nspanel-2.1.1/src/lib.rs:8-37`), which matches the iOS spike's nspanel failure | Separate Windows shell crate, like `apps/ios` | M |
| 12 | `acd-inbox` is Linux only | `acd-inbox/src/lib.rs:11`, `Cargo.toml` (linux-only `secret-service`, `zbus`) | [compiled] (fails at securefs) | Not needed on Windows or Android | — |

Crates that already check for Windows: `acd-protocol`, `acd-core`,
`acd-voice` [compiled]. For Android: `acd-protocol`, `acd-securefs`,
`acd-client`, `acd-app-core`, `acd-voice`, `acd-voice-client` [compiled, with
the header workaround above]. The iOS shell `apps/ios/src-tauri`, with Tauri
2.12.1 and wry, checks for Android with exactly one error:
`src/lib.rs:11:40: unresolved import acd_app_core::keychain` [compiled].

## A. Windows `acd` daemon

### First errors per crate (`x86_64-pc-windows-gnu`)

| Crate | Unmodified tree [compiled] | With securefs stubbed [stub] |
|---|---|---|
| acd-protocol | ok | — |
| acd-core | ok | — |
| acd-voice | ok | — |
| acd-securefs | 14 errors. First: `lib.rs:66:18 cannot find unix in os`; `lib.rs:5:9 unresolved import std::os::fd`; `lib.rs:42:20 cannot find function geteuid in crate libc`; `lib.rs:82 O_NOFOLLOW / O_NONBLOCK / O_CLOEXEC not found` | (stubbed) |
| acd-client | stops at securefs | 4 errors: `settings.rs:7:14 cannot find unix in os`; `conn.rs:160:42 cannot find UnixStream in net`; `settings.rs:291:55 no from_mode`; `settings.rs:298:14 no method mode` |
| acd-adapters | stops at securefs | 8 errors: `herdr/client.rs:11:5` and `herdr/discovery.rs:13:5` `no UnixStream in tokio::net`; `herdr/client.rs:12:17 tokio::net::unix`; `herdr/discovery.rs:5:14`, `:85:64 is_socket`; `hooks/endpoint.rs:20:31 no UnixListener`; `t3/reconciler.rs:262:18`, `:277:27 mode` |
| acd-app-core, acd-voice-client, acd-inbox | stop at securefs | stop at acd-client |
| acd | stops at securefs | stops at acd-adapters. Its own errors are not enumerated: see the [scan] list below. |

rustc stops at the first failing dependency. So the `acd` binary's own
errors are known only from the source scan. That scan has 18 files with
Unix-only uses in `crates/acd/src` (grep for `std::os::unix`, `libc::`,
`UnixListener`/`UnixStream`, `mode(0o`, `launchctl`, `systemctl`).

### Unix-only uses that matter at runtime [scan]

- **Owner-only files** (`acd-securefs/src/lib.rs`): an `O_NOFOLLOW` open, then
  checks on the open fd that the file is the right type, `uid == euid` and
  `mode & !max_mode == 0`. Used for `client.token`, `hook.token`,
  `t3.token`, the TLS key, settings and hook install records.
- **Secret writes** (`acd/src/secrets.rs:116-280`): `openat`/`unlinkat`/`renameat`
  relative to an opened dir fd with `O_NOFOLLOW`, plus a mode repair. Windows
  has no `openat`. The same race-free effect needs handle-relative
  `NtCreateFile` or (simpler) a DACL-protected dir plus `FILE_FLAG_OPEN_REPARSE_POINT`
  checks [guess].
- **Daemon lock and writer lock**: `flock` (`local_socket.rs:38`,
  `writer_lock.rs:55,61`, `tls.rs:36`). Windows equivalent: `LockFileEx` [guess].
- **`acd.sock`** (`local_socket.rs:70-85`, `daemon.rs:703`): mode 0600 Unix
  socket, so the client token never crosses TCP (PROTOCOL.md §1 "Local
  socket").
- **`hook.sock`** (`daemon.rs:548`; `hooks/endpoint.rs:545-558`) and the Codex
  shim (`hook_shim.rs`). The shim does root-to-leaf `lstat` checks of the
  config dir, then requires the socket to be owned by the user with mode 0600.
  It never falls back to TCP (spec §6.1, amendment A1). The producer API is
  served on `hook.sock` only (PROTOCOL.md §8 "Transport").
- **Claude hook endpoint**: already loopback TCP, `127.0.0.1:<hook_port>` with
  `X-Acd-Token` (`daemon.rs:543-546`; `hooks_install/entries.rs:39-45`). This
  part is portable as it stands.
- **T3**: `ws://host:port` over TCP (`t3/conn.rs:179-201`). Portable except the
  `t3.token` mode check (`t3/reconciler.rs:262-278`).
- **herdr**: Unix socket discovery and client (`herdr/discovery.rs`,
  `herdr/client.rs`).
- **Claude process check**: `proc_pidinfo` on macOS, `/proc` on Linux,
  `Err(())` elsewhere (`claude_sessions.rs:106-181`). On Windows every entry
  is unverifiable, so a scan is never complete. The spec says an incomplete
  scan never removes an agent and never retires a hook token (comment at
  `claude_sessions.rs:185-187`). That is fail closed, but stale hooks-only
  agents would stay until they get a hook or a restart [guess].
- **Supervisor**: on a Tailscale address change, acd exits 75 so launchd or
  systemd restarts it (`daemon.rs:21`; PROTOCOL.md §1). Windows needs a
  supervisor that restarts on failure.
- **Tailscale CLI paths** are macOS/Linux only (`bind.rs:47-58`). A Windows
  path list is needed [scan].

### Which adapters exist on Windows at all

| Source | On Windows | Evidence |
|---|---|---|
| Claude Code | Native: "Windows 10 1809+ or Windows Server 2019+", also WSL. HTTP hooks (`type: "http"`) with `headers` and `allowedEnvVars` are documented with no Windows caveat. | [docs] https://code.claude.com/docs/en/setup and https://code.claude.com/docs/en/hooks, read 2026-11-03 |
| Codex | The README has a Windows PowerShell installer. Whether command hooks (`PermissionRequest`) run under native Windows is **not confirmed**: `developers.openai.com` did not resolve from this sandbox. A third-party note says native Windows is "experimental" and that WSL and Windows keep separate `.codex` dirs. | [docs] https://github.com/openai/codex read 2026-11-03; secondary: https://raw.githubusercontent.com/rullerzhou-afk/clawd-on-desk/main/docs/guides/codex-wsl-clarification.md, read 2026-11-03 (not authoritative) |
| herdr | The README links a "windows-beta" page and a PowerShell installer. It does not say how the socket API is exposed on Windows (pipe or AF_UNIX). `herdr.dev` did not resolve from this sandbox. Third-party reviews say: preview beta, ConPTY, no `--remote` on Windows. | [docs] https://github.com/ogulcancelik/herdr read 2026-11-03; secondary: https://www.bitdoze.com/herdr-agent-multiplexer/ (search result only, not read) |
| T3 Code | Windows desktop app (`winget install T3Tools.T3Code`, `scoop install extras/t3code`) and a PowerShell CLI installer. "We are very very early in this project. Expect bugs." | [docs] https://github.com/pingdotgg/t3code read 2026-11-03 |

Consequence [guess, from spec §6.3]: Codex answers go through herdr keys
(Codex's `PermissionRequest` is not held: shim budget 800 ms,
`hook_shim.rs:33-40`). Binding needs herdr panes (spec §6.3 "Binding").
Without a verified herdr socket API on Windows, Codex on Windows is at best
visible, never answerable. That is the fail-closed outcome. Claude through
held HTTP hooks and T3 through its RPC are the two sources that can be
answerable on Windows.

## B. Options per blocker

The rule that limits every option: **if the daemon cannot prove which agent
and screen an answer belongs to, it does not deliver the answer.** None of
the options below change how answers bind (hook connection, T3 request id,
herdr fingerprint). They only replace how files and sockets prove that their
owner is the user.

### B1. Hook endpoint for the Codex shim and producers (`hook.sock`)

| Option | Proves the peer? | Notes |
|---|---|---|
| Loopback TCP + hook token | No. The shim would send the token to whatever holds the port. A1 rejected this for Codex. | Already used for Claude, where Claude itself is the client. |
| AF_UNIX on Windows (10 1803+) | Partly. File ACLs apply, but Rust std and tokio have no Windows AF_UNIX type, so this needs a third-party crate [guess]. | Keeps one code path in theory; least proven in Rust. |
| **Named pipe** `\\.\pipe\acd-<user SID>-hook` | Yes. The server creates it with `first_pipe_instance(true)` (fails if a squatter holds the name), `reject_remote_clients(true)` and a DACL granting only the user's SID (`create_with_security_attributes_raw`). The shim calls `GetNamedPipeServerProcessId`, opens that process's token and requires its user SID to equal its own before it sends the token. | tokio 1.53.2 has all three options (`tokio-1.53.2/src/net/windows/named_pipe.rs:2051,2164,2304`). `GetNamedPipeServerProcessId` is in `windows-sys` 0.59 (`Win32/System/Pipes/mod.rs:19`). [scan of vendored sources] |

**Recommend: named pipe**, for both `hook.sock` and `acd.sock`. On a failed
SID check the shim prints nothing and exits 0, as it already does on any
failure (`hook_shim.rs:1-5`). Codex then shows its own dialog: fail open for
the agent, fail closed for delivery.

### B2. Secret and token storage

| Option | Daemon (`client.token`, `hook.token`, TLS key, `t3.token`) | Client app (machine secrets) |
|---|---|---|
| Owner-only files with an explicit protected DACL (user SID, no inheritance) | Keeps the layout, `acd pair`, `acd token rotate`, and the readers in `acd-client/src/local.rs`. securefs checks owner SID and that no ACE grants access to anyone else. | Possible but unlike macOS/iOS. |
| DPAPI (`CryptProtectData`, user scope) | Protects data at rest but the file still needs the DACL check: any process of the same user can decrypt it. Adds a second format. | — |
| **Credential Manager** (`CredWriteW`, generic credential) | Same-user boundary as DPAPI; poor fit for files other tools read (`t3.token` is written by the user). | Mirrors the Keychain store behind `SecretStore` (`acd-client/src/settings.rs:312-321`). |

**Recommend:** daemon secrets stay files, with a protected DACL and a securefs
Windows check (one format on all OSes). Client machine secrets go in
Credential Manager behind the existing `SecretStore` trait. The blob limit is
2560 bytes; a machine secret is a 43-character token plus a pin [guess,
verify on Windows].

### B3. Service install

| Option | Fit |
|---|---|
| Windows service (SCM) | Runs in session 0 under a service account, not as the user, so the per-user config dir, `~/.claude` and the per-user pipe DACL do not fit [guess]. Needs admin to install. Reject. |
| **Task Scheduler logon task** (per user, "restart on failure") | Runs as the user at logon; restart covers exit 75. Install with `schtasks` or the Task Scheduler COM API; testable like `service.rs`'s `Runner` fake (`service.rs:300-308`). |
| None for v1 | `acd run` in a terminal. `service install` keeps failing cleanly (`service.rs:386-387`). |

**Recommend:** none for the first Windows `acd`, then a Task Scheduler logon task.

### B4. Owner-only file checks (securefs on Windows)

Smallest equivalent [guess, needs a Windows test run]:

1. Open with `FILE_FLAG_OPEN_REPARSE_POINT` and refuse a reparse point (symlink or
   junction) as `SecureFsError::Symlink`.
2. `GetSecurityInfo` on the open handle: the owner must be the current user's
   SID (from `GetTokenInformation(TokenUser)`), or else `Owner`.
3. The DACL must be protected and grant nothing beyond the user, SYSTEM and
   Administrators, or else `Mode` (reported as "ACL too open" instead of an
   octal mode).
4. Directory variant with `FILE_FLAG_BACKUP_SEMANTICS`.

Anything the check cannot read is an error, so callers fail closed as they do
today.

## C. Windows client

- **Reuse `acd-app-core`: yes, after blockers 1 and 3.** It has no
  platform code apart from `keychain` (`lib.rs:12-13`, Apple-only).
  `setup::OpenOptions.local_machine` (`setup.rs:22,54-55`) already lets a shell
  hide the local machine, as the iOS shell does (`apps/ios/src-tauri/src/lib.rs:54-58`).
- **macOS-only in `app/src-tauri`** [scan]:
  - `tauri-nspanel` (`Cargo.toml:35`, `main.rs:30`, `panel.rs:11,118,352`): the
    floating panel. Does not compile off macOS.
  - The Swift speech bridge and the model (`build.rs:2` gates it to macOS;
    `voice/bridge.rs`, `voice/model.rs`).
  - objc2 notifications (`notify.rs:142-144`; `Cargo.toml:43-52`). Off macOS,
    `tauri-plugin-notification` is already registered (`main.rs:27-28`).
  - `ActivationPolicy::Accessory` (`main.rs:72`).
  - The Keychain (via `acd-app-core::keychain`).
  - The global shortcut and tray are Tauri features that also exist on
    Windows [guess, from memory: `v2.tauri.app` did not resolve here].
    `hotkey.rs` and `tray.rs` have one and zero `cfg(target_os = "macos")`
    gates. Whether they compile on Windows was not reached: the check stops
    at securefs.
- **Minimal Windows tray-and-window client:** a new shell crate
  `apps/windows/src-tauri`, shaped like `apps/ios/src-tauri`: the same
  `acd-app-core` commands as `apps/ios/src-tauri/src/lib.rs:27-47`, plus a
  tray icon, one normal window (no panel), `tauri-plugin-notification`, the
  clipboard pairing import, and a Credential Manager `SecretStore`. No voice,
  no hotkey at first. `icon.ico` is required: the iOS shell's build script
  fails on Windows with "`icons/icon.ico` not found; required for generating a
  Windows Resource file" [stub]. The Mac app has one (`app/src-tauri/icons/icon.ico`).
- **A remote-only Windows client first: viable, and the cheapest Windows step.**
  It talks acd/1 over TLS with the pinned SPKI to a Mac or Linux `acd` over
  Tailscale, exactly like iOS (`docs/research/2026-10-12-ios-spike.md` §4). It
  needs blockers 1 (client-side: refuse), 3 and 10, plus the shell crate. It
  does not need blockers 4 to 9. Cost: the user cannot see agents running on
  the Windows PC itself. Given the adapter table above, that loses Claude on
  Windows and T3 on Windows. Codex on Windows is unanswerable either way.

## D. Android client

- **Reuse `apps/ios/src-tauri`: yes.** It already uses `tauri::mobile_entry_point`
  (`lib.rs:16`) and `crate-type = ["staticlib", "cdylib", "rlib"]`
  (`Cargo.toml`). For `aarch64-linux-android` it checks with one error, the
  missing `keychain` module (`lib.rs:11`, used at `lib.rs:52`) [compiled].
  `tauri android init` and an APK build were not run: they need the Android
  SDK, NDK and a JDK, and the NDK download is blocked here.
- **`cargo check --target aarch64-linux-android`**: `acd-protocol` ok;
  `acd-client` and `acd-app-core` fail without the NDK (ring's C build:
  `failed to find tool "aarch64-linux-android-clang"`). With host clang and
  glibc arm64 headers standing in, `acd-protocol`, `acd-securefs`,
  `acd-client`, `acd-app-core`, `acd-voice` and `acd-voice-client` check
  [compiled]. Real NDK builds and links: not run.
- **Secret storage:** the Android Keystore keeps *keys* non-exportable ("Key
  material never enters the application process"). It is not a store for
  arbitrary strings. A Keystore AES-GCM key that encrypts a machine-secret
  file in app-private storage fits. That needs Kotlin, so a small Tauri mobile
  plugin implements `SecretStore` [docs: https://developer.android.com/privacy-and-security/keystore,
  read 2026-11-03; plugin shape is a guess]. `SecretStore` is synchronous
  (`settings.rs:312-315`), so the plugin call must block off the UI thread,
  as `CachedSecrets` reads already assume [guess].
- **Tailscale on Android:** not verified. `tailscale.com` did not resolve from
  here. The client's address vetting is the same as on iOS, so the iOS
  spike's open risk carries over: the phone must route `100.64.0.0/10` and
  `fd7a:115c:a1e0::/48` through the tunnel. Also an assumption, not checked:
  Android's VPN service carries other apps' tailnet traffic.
- **FCM wake-up, later:** the integration point is the Android shell's Kotlin
  layer. A `FirebaseMessagingService` receives the relay's content-free
  wake-up and brings the app (and `Inbox`) to the foreground or posts a local
  notification. One new shell command registers the FCM token with the relay.
  acd/1 and `acd-client` do not change; content still arrives only over the
  pinned acd/1 connection. The relay is specified elsewhere (roadmap step 3).

## E. Plan and recommendation

Small, independently reviewable PRs, cheapest useful milestone first.

**Android (depends on roadmap step 3 only for push, not for the foreground inbox):**

1. CI: `cargo check --target aarch64-linux-android` for `acd-protocol`,
   `acd-client`, `acd-app-core` with the NDK on the runner. Guards what passes
   today. XS.
2. `acd-app-core`: a platform `secret_store()` so shells stop naming
   `keychain` directly, and an Android `SecretStore` (Keystore-wrapped file
   through a small Kotlin plugin). Tests with a fake. S.
3. Make the iOS shell build for Android (rename to a mobile shell, or add
   `gen/android` beside `gen/apple`), `tauri android init`, emulator run with
   the touch UI from the iOS work. S–M. **Milestone: foreground Android inbox
   over Tailscale**, paired by clipboard, like iOS today.
4. Live check on a phone over Tailscale: list, answer, altered pin refused. Owner action.
5. FCM wake-up after the relay lands (step 3). S in the shell.

**Windows client (remote machines only):**

6. `acd-securefs` Windows branch that refuses every check (fail closed), and
   `acd-client`: `local` unavailable on Windows, settings written without
   Unix modes. CI `cargo check --target x86_64-pc-windows-msvc` for
   `acd-client` and `acd-app-core` on `windows-latest`. S.
7. Credential Manager `SecretStore` in `acd-app-core`. S.
8. `apps/windows/src-tauri`: tray, window, notifications, clipboard pairing. M.
   **Milestone: Windows PC answers a Mac's or Linux box's agents.**

**Windows `acd` (post-1.0):**

9. Platform transport seam: one local-listener type over Unix socket or named
   pipe for `acd.sock` and `hook.sock`, with the shim's SID check. M.
10. securefs Windows checks (B4) and DACL secret writes (`secrets.rs`, `tls.rs`,
    `writer_lock.rs`, `LockFileEx`). M.
11. Windows process verification in `claude_sessions.rs`, and Tailscale CLI
    paths in `bind.rs`. S.
12. Hook install on Windows (Claude `settings.json` under `%USERPROFILE%\.claude`),
    then live check with Claude Code and T3 on Windows. M.
13. Task Scheduler logon task in `service.rs`. S.
14. herdr and Codex on Windows only after herdr documents its Windows socket
    API and Codex hooks are confirmed on native Windows. Unknown.

**Recommendation:**

- For 1.0: ship Android steps 1–4 if roadmap step 3 is on time, because the
  code is nearly there: the only compile error in the shell is the secret
  store. Push (step 5) follows the relay.
- The Windows client (6–8) is optional for 1.0. Take it only if Android is
  done early; it is about one week of small PRs [guess].
- Cut the Windows `acd` (9–14) from 1.0. It touches the trust boundary
  (sockets, secrets, file checks), needs a Windows machine for every test,
  and two of its four sources (herdr, Codex) are unconfirmed on Windows.
- If dates slip, the roadmap cut line still holds: Windows and Android both move after 1.0.

## Checks not run

- Any Windows build on Windows, the msvc target, linking, or running tests on Windows.
- `acd` binary's own Windows errors (hidden behind `acd-adapters`; [scan] only).
- `app/src-tauri` on Windows past securefs (nspanel, tray, hotkey not reached).
- Real Android NDK build and link, `tauri android init/build`, emulator or device.
- Tailscale on Android or Windows; Credential Manager and Keystore round trips.
- Official Codex, herdr, Tauri, Tailscale and docs.rs pages: `developers.openai.com`,
  `herdr.dev`, `v2.tauri.app`, `tailscale.com`, `docs.rs` and `dl.google.com`
  are not reachable from this sandbox.
- `cargo test --workspace` and `clippy`: no code changed, so not run.
