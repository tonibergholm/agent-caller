# Windows

Status: groundwork. Three pieces of the client side are written and
type-checked for `x86_64-pc-windows-msvc`. **None of it has run on Windows.**
No Windows machine was available. Everything under "Not built" is a design.
The numbered checklist at the end is what the owner must run on a Windows PC
before anyone relies on any of this.

Background and the blocker list: [feasibility note](research/2026-11-03-windows-android-feasibility.md).
Roadmap step: "Windows and Android clients" ([roadmap](superpowers/plans/2026-10-07-roadmap.md)).

## Evidence levels

- **[checked]**: `cargo check`/`clippy` for `x86_64-pc-windows-msvc` passed, or a
  Linux test ran. Type check only: nothing was linked or run on Windows.
- **[design]**: a decision with reasons; not built, not verified.
- **[manual]**: needs a Windows machine; listed in the checklist.

## What is built

### 1. Owner-only files (`crates/acd-securefs`) [checked] [manual]

`cfg(windows)` now has a real implementation (`src/windows.rs`) instead of
refusing. `OWNER_CHECKS` and `platform_supports_owner_checks("windows", false)`
stay **false**: the flag means "Unix mode bits and `acd.sock` exist", and the
client uses it to refuse the `local` endpoint (`conn.rs` `refuse_local`). That
refusal is still right on Windows, because there is no pipe transport yet.
Android still refuses every check.

Rules (pure logic in `src/winacl.rs`, tested on Linux):

- Open with `FILE_FLAG_OPEN_REPARSE_POINT`; a reparse point (symlink or
  junction) in the last component is `Symlink`. Directories also use
  `FILE_FLAG_BACKUP_SEMANTICS`. Only the last component is checked, as on Unix
  (`O_NOFOLLOW`).
- Everything else is judged on the open handle, so a swap between check and
  use cannot change the answer.
- The owner SID must equal the process token's user SID, else `Owner`. A file
  made by an elevated process is owned by Administrators and is refused.
- The DACL must exist, be non-empty, and contain only "access allowed" entries
  naming the current user. A NULL DACL (everyone), an empty DACL, any deny,
  audit, object or unknown entry, and any other trustee (including SYSTEM and
  Administrators) is refused with a `PermissionDenied` I/O error whose text
  names the reason and never a SID. The DACL need not carry the "protected"
  flag: the effective entries are what is judged, at every open.
- Intentional difference from the feasibility note (B4 step 3), which allowed
  SYSTEM and Administrators: the rule here is "only the current user". The cost
  is that a file made the ordinary way inherits the profile's ACL (SYSTEM,
  Administrators, user) and is refused until it is re-created with
  `create_private_file`. Administrators can take ownership of anything; this
  guards against other standard users and services, not against an administrator.
- `max_mode` has no meaning on Windows and is ignored; the rule is the
  strictest one. `open_owned_file` requires the owner only (callers repair
  through the returned handle).
- Creation: `create_private_dir`, `create_private_dir_all` and
  `create_private_file` pass `SECURITY_ATTRIBUTES` built from SDDL
  `O:<sid>D:P(A;;FA;;;<sid>)` (directories `D:P(A;OICI;FA;;;<sid>)`): protected
  (nothing inherited), one entry, the user. The SID is validated before it is
  put into SDDL, and the result is verified with the same check before it is
  returned. Files use `CREATE_NEW`; an existing path is an error.
- `check_socket_path*` still refuse (no sockets on Windows).

### 2. Secrets (`crates/acd-app-core/src/credman.rs`) [checked] [manual]

A `SecretStore` over the Windows Credential Manager, selected by
`platform_secrets()` on Windows and wrapped in `CachedSecrets` like the Keychain.

- One generic credential per machine key, target `dev.agentcaller.inbox/<key>`,
  holding the same JSON (`MachineSecret`: token and pin) the Keychain holds. The
  blob limit is 2560 bytes; a real secret is about 130. An oversize value is
  refused, never truncated.
- `CRED_PERSIST_LOCAL_MACHINE`, **not** `CRED_PERSIST_ENTERPRISE` (which roams
  with a domain profile). Credentials are per user and protected by Windows.
- Errors carry the Win32 code only. A missing credential is `Ok(None)` for
  `get` and `Ok(())` for `delete`; error 1312 (no logon session) is reported as
  the "locked" kind; everything else fails. There is no file fallback.
- Why `windows-sys`: Microsoft's own raw-binding crate, and **already in
  `Cargo.lock` (0.61.2, pulled in by tokio and socket2)**. The change adds no
  new package, only a dependency edge and features (`Win32_Foundation`,
  `Win32_Security_Credentials` here; securefs also `Win32_Security`,
  `Win32_Security_Authorization`, `Win32_Storage_FileSystem`,
  `Win32_System_Threading`). `keyring` is not in the lock file and would add a
  crate tree for about 100 lines of FFI. `windows` (the safe wrapper) would add
  `windows-core` and proc-macro crates. The cost taken on instead is `unsafe`
  FFI code that nobody has run yet: see the checklist.

### 3. Directories (`crates/acd-securefs/src/paths.rs`) [checked]

One place for the rules; pure string logic, tested on any OS.

- Config: `%LOCALAPPDATA%\acd\config` (tokens, TLS key, settings). Data:
  `%LOCALAPPDATA%\acd\data` (rebuildable state, logs).
- **Never `%APPDATA%`** (Roaming): roaming profiles and folder redirection copy
  it to other machines. `is_under_roaming` lets a shell refuse Tauri's
  `app_config_dir()`, which is Roaming on Windows, for settings or secrets.
- `ACD_CONFIG_DIR` overrides the config dir only if it is absolute (drive letter
  or UNC) with no `..`. A bad override is an error, not a fall-through. A
  missing or relative `LOCALAPPDATA` gives no directory: no fallback to the
  current directory, `%TEMP%` or `%APPDATA%`.
- `windows_config_dir()` and `windows_data_dir()` read the environment.
  **Nothing calls them yet.** The Unix callers (`acd-client::local`,
  `acd::paths`) are untouched on purpose, and `local::default_config_dir()`
  stays `None` on Windows. The future Windows shell must use these, not Tauri's
  `app_config_dir()`, for `settings.json`.

### Known gap in this change

`acd-client::settings::Settings::save` still writes `settings.json` with the
inherited ACL on Windows (its own `shortcut:` comment says so). It holds the
pinned machine list but no tokens. Switching it to `create_private_file` was
left out to keep this change inside the crates it was scoped to.

## Not built (design) [design]

### A. Windows shell without voice

New crate `apps/windows/src-tauri`, its own Cargo workspace like `apps/ios` and
`app/src-tauri`. It reuses `acd-app-core` (`setup::open_inbox` with
`local_machine` hidden, as iOS does; `platform_secrets()` now gives the
Credential Manager). Tray icon, one normal window (no `tauri-nspanel`, no
`ActivationPolicy`), `tauri-plugin-notification`, clipboard pairing import, no
hotkey and no voice at first. It needs `icons/icon.ico`. `acd-voice` already
checks for Windows but has no speech backend, so the shell does not use it.
The settings path comes from `windows_config_dir()`. It is remote-only: it
pairs with a Mac or Linux `acd` over Tailscale, like iOS. The local endpoint
stays unavailable until the pipe transport (D).

### B. Notifications

`tauri-plugin-notification`. Windows toasts need an AppUserModelID tied to an
installed shortcut or package; unpackaged dev builds show a borrowed identity
and may not show actions [guess]. Toasts show the request summary only, never
T3 message text. Clicking a toast focuses the window; no action buttons in v1,
so answering stays in the window.

### C. Deep links for `acd://`

`acd://host:port?pin=…#token` is the existing pairing string. Register it per
user under `HKCU\Software\Classes\acd` (no admin), from the installer or with
`tauri-plugin-deep-link`. The link is **never** acted on silently: it opens the
window with the import dialog filled in and waits for the user to confirm,
since a link can come from any web page. A running instance receives it by
single-instance forwarding, not a second process. The token is in the URL
fragment, which browsers do not send over the network, but a launched handler
gets the whole URL on its command line, visible to the same user's other
processes; the shell should read it once, show only the host, and prefer the
clipboard import for anything long-lived.

### D. Hook shim and `acd` local transport

This needs a Windows `acd` daemon, the large remaining piece (blockers 4-9 in
the feasibility note). Recommended: a named pipe `\\.\pipe\acd-<user SID>-hook`
(and a second one for the client), per feasibility B1: created with
`first_pipe_instance(true)`, `reject_remote_clients(true)` and a DACL granting
only the user's SID (reuse the SDDL builders in `winacl`, with pipe access
rights); the shim calls `GetNamedPipeServerProcessId`, opens that process's
token and requires its user SID to equal its own before sending the token. A
failed check prints nothing and exits 0, as the shim does today. Localhost TCP
is rejected for the Codex shim (it cannot prove the peer); the Claude Code HTTP
hook on `127.0.0.1:<port>` with `X-Acd-Token` is portable as it stands. Whether
Codex hooks run on native Windows is unconfirmed.

### E. Daemon supervision

A per-user **Task Scheduler** logon task, not a Windows service (a service runs
in session 0 under another account, so the per-user config dir, `~/.claude` and
the per-user pipe DACL do not apply). Trigger "at log on of current user",
restart on failure every minute (covers exit 75 after a Tailscale address
change), no time limit, run only when logged on, ignore a second instance.
Install with `schtasks /Create /XML` from generated XML, so the definition is
testable as text, behind the `Runner` fake `service.rs` already uses. Uninstall
removes the task. v1 alternative: `acd run` in a terminal.

### F. Packaging

- **MSI (WiX) first**, per user (install under
  `%LOCALAPPDATA%\Programs\agent-caller`): no admin prompt, `acd://` keys under
  HKCU, a Start Menu shortcut carrying the AppUserModelID for toasts, an
  optional startup entry. Tauri's bundler can produce it.
- **MSIX later**: package identity gives toasts and deep links cleanly and a
  clean uninstall, but needs a signing certificate the machine trusts, and a
  packaged app's file and registry writes are virtualised, which can hide the
  config dir from the `acd` CLI. Check that the CLI and the app agree on
  `%LOCALAPPDATA%\acd` before choosing MSIX.
- Authenticode-sign the binaries; unsigned builds trigger SmartScreen.

## How the type check was done

```sh
rustup target add x86_64-pc-windows-msvc
cargo check  --locked --target x86_64-pc-windows-msvc -p acd-app-core -p acd-securefs -p acd-client --tests
cargo clippy --locked --target x86_64-pc-windows-msvc -p acd-app-core -p acd-securefs -p acd-client --all-targets -- -D warnings
```

`ring`'s build script compiles C even for `check`, and Linux has no `cl.exe`.
What worked here (clang 18, `mingw-w64-x86-64-dev`): copy
`/usr/x86_64-w64-mingw32/include` to a scratch dir, change the `__declspec(...)`
lines for `__MINGW_ATTRIB_NORETURN` and `__MINGW_ATTRIB_DEPRECATED` in
`_mingw.h` to `__attribute__((...))`, then:

```sh
export CC_x86_64_pc_windows_msvc=clang AR_x86_64_pc_windows_msvc=llvm-ar
export CFLAGS_x86_64_pc_windows_msvc="-isystem <scratch>/mingw-inc -w"
```

Those objects are never linked or run. `cargo-xwin` (which downloads the MSVC
CRT and SDK) is the proper tool and was not tried. CI should use a
`windows-latest` runner, where none of this is needed.

## Manual checks for a Windows machine

Windows 10 1809+ or 11, as an ordinary (non-elevated) user, in a clone of this
branch with Rust installed. Report any failure with the exact output.

1. `cargo test -p acd-securefs --locked`. Everything passes, including the
   three `windows::tests` (`the_current_user_sid_is_a_valid_sid_string`,
   `a_created_dir_and_file_pass_and_a_loosened_one_fails`,
   `a_junction_or_symlink_is_refused`). The last one returns early without
   permission to create symlinks; enable Developer Mode and rerun so it runs.
2. `cargo test -p acd-app-core --locked credman`. Confirm
   `credman::store::tests::round_trip` passes (it uses the real Credential
   Manager), then `cmdkey /list` shows no `dev.agentcaller.inbox/acd-test-*` left.
3. Store a credential, and in Control Panel, Credential Manager, Windows
   Credentials confirm it is a generic credential with persistence "Local
   computer", not "Enterprise".
4. With a second standard user, confirm that user cannot see the credential
   from step 3.
5. Create a file with `create_private_file` and run `icacls <file>`. Expect
   exactly one entry, `<DOMAIN\user>:(F)`, with no `(I)` inherited entry.
6. For a directory made with `create_private_dir`, `icacls <dir>` shows
   `<user>:(OI)(CI)(F)` only. A file created inside it with Notepad has only
   that inherited entry, and `check_private_file` accepts it.
7. Make a file with `echo x > f.txt` in the user profile (it inherits SYSTEM and
   Administrators). `check_private_file` must fail with the "someone other than
   you" text.
8. Run `icacls <private file> /grant Everyone:R`; `check_private_file` and
   `read_private` must now fail. Run `icacls <file> /remove Everyone`; they
   pass again.
9. From an elevated prompt, `takeown /f <file>`, then check as the standard
   user: expect an `Owner` error.
10. Create a symlink (`mklink f2 f`, Developer Mode) and a directory junction
    (`mklink /J d2 d`). Checking each must give `Symlink`, never a pass.
11. Check a path with non-ASCII characters and one longer than 260 characters
    in `\\?\` form. Both work or fail with a clear error.
12. From a scratch binary, call `acd_securefs::paths::windows_config_dir()` with
    `LOCALAPPDATA` relative, then unset: expect `None` both times. With
    `ACD_CONFIG_DIR=C:\x\..\y`: `None`.
13. On a machine with a roaming profile or folder redirection, confirm nothing
    appears under `%APPDATA%\acd` after steps 1-6.
14. `cargo check --locked --target x86_64-pc-windows-msvc --workspace` natively
    and record which crates do not build. From Linux, `acd`, `acd-adapters`
    and `acd-inbox` fail (Unix sockets, `std::os::unix`); `acd-protocol`,
    `acd-core`, `acd-voice`, `acd-voice-client`, `acd-securefs`, `acd-client`
    and `acd-app-core` check.
