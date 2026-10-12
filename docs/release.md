# Releases

This page covers how to cut a release, which secrets the pipeline reads,
and what the release binaries mean for users.

The pipeline is [`.github/workflows/release.yml`](../.github/workflows/release.yml).
On a `v*` tag it runs the same tests as `ci.yml`, builds the artifacts and
creates a draft pre-release. The owner reviews the draft and publishes it.
A `workflow_dispatch` run builds the same artifacts and uploads them to the
run. It creates no release.

## Artifacts

| Artifact | Built on | Contents |
|---|---|---|
| `acd-<version>-aarch64-apple-darwin.tar.gz` | `macos-latest` | `acd`, `LICENSE` |
| `acd-<version>-x86_64-apple-darwin.tar.gz` | `macos-latest`, cross-compiled | `acd`, `LICENSE` |
| `acd-<version>-x86_64-unknown-linux-gnu.tar.gz` | `ubuntu-22.04` | `acd`, `acd-inbox`, `LICENSE` |
| `acd-<version>-aarch64-unknown-linux-gnu.tar.gz` | `ubuntu-22.04-arm` | `acd`, `acd-inbox`, `LICENSE` |
| `agent-caller-<version>-aarch64-apple-darwin.dmg` | `macos-latest` | The Agent Caller app |
| `SHA256SUMS` | publish job | SHA-256 of every file above |

Notes on the choices:

- The Linux builds run on native runners, not cross images. `acd` and
  `acd-inbox` need only glibc. `acd-inbox` uses `zbus`, which is pure Rust,
  so no `libdbus` is linked. The `ubuntu-22.04` runners set the glibc floor
  at 2.35.
- The dmg covers Apple silicon only. `app/src-tauri/build.rs` builds the
  Swift voice bridge for the host architecture and passes no target triple.
  An x86_64 app build is therefore untested. Add it after a build succeeds.
- The Intel `acd` is cross-compiled on the Apple silicon runner. It has no
  Swift code.

## Cutting a release

1. Merge the release changes to `main`, with CI green.
2. Choose the version `X.Y.Z`. Set it in these files, which
   `scripts/check-version.sh` reads:
   - `Cargo.toml`, `[workspace.package] version`
   - `app/src-tauri/Cargo.toml`, `[package] version`, and the
     `agent-caller-app` entry in `app/src-tauri/Cargo.lock`
   - `app/package.json` and the two `version` fields in
     `app/package-lock.json`
   - `app/src-tauri/tauri.conf.json`
3. In `CHANGELOG.md`, add a `## [X.Y.Z] - YYYY-MM-DD` section above the
   entries in `[Unreleased]`, and move those entries into it.
4. Run `scripts/check-version.sh vX.Y.Z`. It prints the version when every
   file agrees, the tag matches and the CHANGELOG has the section.
5. Dry run: in the Actions tab, run the Release workflow from `main` with
   **Run workflow**. Download one tarball and the dmg from the run, then
   test `scripts/install.sh` against them (see [Install](#installing-a-release)).
6. Tag the tested commit and push the tag:

   ```sh
   git tag -a vX.Y.Z -m "agent-caller X.Y.Z"
   git push origin vX.Y.Z
   ```

7. The `ci` job runs first. A failing test stops the release. The build
   jobs run next, and the publish job creates a **draft** pre-release with
   the artifacts, `SHA256SUMS` and the CHANGELOG section as notes.
8. Review the draft: the asset list, the checksums and the notes. Then
   publish it from the GitHub release page. Keep the pre-release flag.
9. Install from the published release on a Mac and on a Linux machine.

The tag and the manifests must agree. The workflow fails before any build
when they differ. A pre-release such as `v0.2.0-rc.1` needs the manifests
to read `0.2.0-rc.1`.

## Secrets

Set these under **Settings, Secrets and variables, Actions** in the
repository. None is needed for an ad-hoc build.

| Secret | Purpose |
|---|---|
| `APPLE_CERTIFICATE` | Base64 `.p12` export of a Developer ID Application certificate. Setting it turns on Developer ID signing. |
| `APPLE_CERTIFICATE_PASSWORD` | Password of the `.p12` file. |
| `APPLE_SIGNING_IDENTITY` | Identity name, for example `Developer ID Application: Name (TEAMID)`. Optional. Without it, the identity is taken from the certificate. |
| `APPLE_ID` | Apple ID email used for notarization. |
| `APPLE_PASSWORD` | App-specific password for that Apple ID. |
| `APPLE_TEAM_ID` | The 10-character team ID. |

The pipeline chooses a mode from these values:

- No `APPLE_CERTIFICATE`: ad-hoc signing with the identity `-`. Nothing is
  notarized. This is the default.
- `APPLE_CERTIFICATE` set, and `APPLE_ID`, `APPLE_PASSWORD` or
  `APPLE_TEAM_ID` missing: Developer ID signing without notarization. The
  job logs a warning.
- All six set: Developer ID signing and notarization. Tauri staples the
  notarization ticket to the app. The `acd` binary is notarized as a zip,
  and a bare binary cannot be stapled.

The Tauri bundler reads the certificate variables itself. For `acd`,
`scripts/apple-keychain.sh` imports the certificate into a temporary
keychain, and the job deletes that keychain at the end. The pipeline never
prints a secret. Linux jobs use no secrets.

## What ad-hoc signing means for users

Ad-hoc signing (`codesign --sign -`) gives the code a valid signature
without an Apple identity. Apple silicon requires a signature, so the app
and `acd` run. Gatekeeper cannot tell who built them, and the app is not
notarized. A downloaded file also carries a quarantine flag, which makes
macOS check it on first launch.

First launch of the app:

- Open System Settings, then Privacy & Security. Find the message about
  "Agent Caller" and choose **Open Anyway**. Since macOS 15, Control-click
  on the app no longer bypasses the check.
- Or remove the quarantine flag in Terminal:

  ```sh
  xattr -dr com.apple.quarantine "/Applications/Agent Caller.app"
  ```

For `acd` extracted in Finder from a browser download, remove the flag from
the binary:

```sh
xattr -d com.apple.quarantine ~/.local/bin/acd
```

Extracting with `tar` in Terminal does not add the flag, and
`scripts/install.sh` downloads with `curl`, which does not add it either.

Check a download against the published checksums:

```sh
shasum -a 256 acd-0.1.0-aarch64-apple-darwin.tar.gz
grep acd-0.1.0-aarch64-apple-darwin.tar.gz SHA256SUMS
```

The two hashes must match. `scripts/install.sh` runs this check for you.

## Keychain and privacy permissions

The app stores remote machine credentials in the login Keychain
(`app/src-tauri/src/keychain.rs`). The `acd` CLI keeps its tokens in
owner-only files and does not use the Keychain.

The Keychain and the privacy permissions (Microphone, Speech Recognition,
Input Monitoring and Notifications) are tied to the app's code identity.
An ad-hoc signature has no team identity, so each new build has a new
identity. After an update, expect:

- Keychain prompts for access to the remote credentials. Allow them again.
- The privacy prompts may come back. If voice stops working after an
  update, remove Agent Caller from the relevant list in System Settings and
  allow it again.

Developer ID signing gives every build the same identity (team ID and
bundle ID), so grants and Keychain access survive updates.

Local builds work the same way. `just app-install` uses
`APPLE_SIGNING_IDENTITY` when it is set, for the same reason.

## Installing a release

`scripts/install.sh` installs `acd`, and `acd-inbox` on Linux, into
`~/.local/bin`. It needs `curl`, `tar`, and `sha256sum` or `shasum`. It
never uses sudo. It checks the archive against `SHA256SUMS` before it
extracts anything, and it stops without installing when the check fails.

```sh
curl -fsSLO https://raw.githubusercontent.com/tonibergholm/agent-caller/vX.Y.Z/scripts/install.sh
sh install.sh --tag vX.Y.Z
```

To install from files you already downloaded, put the archive and
`SHA256SUMS` in one directory and run:

```sh
sh scripts/install.sh --version X.Y.Z --from-dir /path/to/downloads
```

## Limits of this pipeline

- No x86_64 app dmg (see above).
- The arm64 Linux job uses a GitHub-hosted arm runner. Those runners are
  free only for public repositories. A private repository needs a paid
  runner for that job.
- `ubuntu-22.04` will be retired by GitHub at some point. When that
  happens, move the Linux jobs to a newer image and update the glibc floor
  in this page.
- The pipeline does not yet produce SBOMs or build provenance attestations.
