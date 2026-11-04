# Linux packaging

This page covers the Arch packages for `acd` and the Omarchy inbox, the Linux
release workflow, and how to check a download. Omarchy is Arch based, so
`pacman` is the target. The macOS release is in [release.md](release.md).

Everything here is unsigned. See [Signing, later](#signing-later).

## What is packaged

| Package | Directory | Installs |
|---|---|---|
| `agent-caller` | `packaging/arch/agent-caller` | `/usr/bin/acd`, `/usr/lib/systemd/user/acd.service`, the licence |
| `agent-caller-omarchy` | `packaging/arch/agent-caller-omarchy` | `/usr/bin/acd-inbox`, `/usr/lib/systemd/user/acd-inbox.service`, a reference copy of the plugin in `/usr/share/agent-caller/omarchy/tonibergholm.agentcaller/`, `/usr/share/agent-caller/smoke-test.sh`, the licence |

Both build from the git tag `v<pkgver>` with `cargo --locked` (`--frozen`
after `cargo fetch --locked`). Neither package writes into a home directory,
`/etc` or the keyring, and neither has a `backup=()` entry.

There are no shell completions: the CLI has no completion generator, and this
change does not add one.

### One source of truth

The packages copy two kinds of files; neither is a second implementation.

- **Units.** `packaging/systemd/acd.service` is what `acd service install`
  writes, with the executable fixed to `/usr/bin/acd` and the home directory
  written as systemd's `%h` (`ACD_CONFIG_DIR=%h/.config/acd`, which is the
  default directory). `packaging/systemd/acd-inbox.service` is `unit_text()`
  from `crates/acd-inbox/src/install.rs` for `/usr/bin/acd-inbox`. Two tests
  fail when either file drifts from the code:
  `crates/acd/tests/packaging_unit.rs` and `crates/acd-inbox/tests/packaging.rs`.
- **Plugin files.** The PKGBUILD lists the files `acd-inbox install` embeds
  (`PLUGIN_FILES`). The same test fails if the list and the installer differ,
  or if a file in `clients/omarchy` differs from the embedded copy.

A unit in `~/.config/systemd/user` (written by `acd service install` or
`acd-inbox install`) overrides the packaged unit of the same name, so both can
exist.

### Why `acd-inbox install` is still the install step

A package cannot write `~/.config/omarchy/plugins/...`, and Omarchy's plugin
search path beyond the user directory is not documented in
[the plugin API note](research/2026-10-12-omarchy-plugin-api.md), so the
package does not guess a system plugin directory. Each user runs
`acd-inbox install` once. That command, not the package, writes the user unit,
the plugin directory and the keybinding offer.

One consequence: `acd-inbox install` copies the binary to `~/.local/bin/acd-inbox`
and points its user unit there. After a package upgrade the user runs
`acd-inbox install` again to refresh that copy and the plugin files. The smoke
test reports a stale copy as a failure. A future `--use-installed-binary`
option would remove the copy; it is not part of this change.

## Build and install

From a checkout of the tag:

```sh
cd packaging/arch/agent-caller
makepkg -si            # builds, runs the unit check, installs with pacman -U
cd ../agent-caller-omarchy
makepkg -si
```

`makepkg` needs `base-devel`, `git` and `cargo` (the PKGBUILDs declare
`git` and `cargo` as `makedepends`). Run it as a normal user. The build
downloads crates, so it needs network access, but `Cargo.lock` decides every
version.

`options=(!lto)` is deliberate. `makepkg`'s default LTO flags make the link of
`ring`'s C and assembly objects fail with undefined `ring_core_*` symbols.
The container test found this.

The omarchy package suggests `gnome-keyring` (any Secret Service provider
works) for pairing secrets, and `agent-caller` if agents run on the same
machine.

### First run

```sh
acd init                         # creates tokens and config in ~/.config/acd
acd service install              # or: systemctl --user enable --now acd.service
acd-inbox install                # on Omarchy; see "Why acd-inbox install..." above
packaging/smoke-test.sh          # or /usr/share/agent-caller/smoke-test.sh
```

`systemctl --user enable --now acd.service` uses the packaged unit and needs
`acd init` to have run first. To keep the daemon running after logout:
`loginctl enable-linger $USER`.

## Upgrade

```sh
git pull && git checkout vX.Y.Z   # or fetch the new tag
cd packaging/arch/agent-caller && makepkg -si
cd ../agent-caller-omarchy && makepkg -si
acd-inbox install                 # per user: refreshes ~/.local/bin and the plugin
omarchy-restart-shell
systemctl --user restart acd.service
```

What an upgrade does to your data:

- **Config and tokens** (`~/.config/acd`: `client.token`, `hook-tokens.json`,
  `config.toml`, the TLS key and certificate) are not part of any package.
  `pacman` does not read, replace or delete them. Their modes stay as `acd init`
  made them (owner only).
- **Inbox settings** (`~/.config/acd-inbox`) and **pairing secrets** (the
  Secret Service keyring) are likewise untouched.
- **Hooks** installed with `acd hooks install` point at the `acd` path they
  were installed with. The packaged path, `/usr/bin/acd`, does not change.
  If you installed hooks from a copy in `~/.local/bin`, reinstall them after
  you switch to the package.
- The packaged units are replaced. A unit you wrote with `acd service install`
  is yours and stays.

`SOURCE_DATE_EPOCH` and a pkgrel bump are the only knobs a rebuild needs; see
[Reproducibility](#reproducibility).

## Uninstall

```sh
acd-inbox uninstall               # per user: stops the service, removes ~/.local/bin copy and the plugin
systemctl --user disable --now acd.service
sudo pacman -R agent-caller-omarchy agent-caller
```

`pacman -R` removes only the files the packages installed. It never deletes
`~/.config/acd`, `~/.config/acd-inbox` or keyring entries; delete those by
hand if you want them gone (`acd` tokens stay owner-only until you do).
`acd-inbox uninstall` leaves the keybinding line in `~/.config/hypr/bindings.lua`
and prints it so you can remove it.

## Verifying a download

The Linux workflow (below) publishes `agent-caller-linux-<version>-<target>.tar.gz`
and `SHA256SUMS` in one draft release.

```sh
sha256sum --check --ignore-missing SHA256SUMS
```

`--ignore-missing` skips the target you did not download. The command prints
`OK` for the file it found and exits non-zero on a mismatch. Do not install
a file that does not pass.

A checksum from the same release page shows that the download is intact. It
does not show who built it: nothing is signed yet, so anyone who can change the
release can change both files. Until signing exists, prefer building the tag
yourself with `makepkg`, and compare with the release if you want.

Contents of each archive: `bin/acd`, `bin/acd-inbox`, `systemd/*.service`,
`omarchy/tonibergholm.agentcaller/*`, `smoke-test.sh`, `LICENSE`.

## The Linux release workflow

`.github/workflows/release-linux.yml` builds `x86_64-unknown-linux-gnu` and
`aarch64-unknown-linux-gnu` on native runners (`ubuntu-22.04`,
`ubuntu-22.04-arm`), as `release.yml` does, so the glibc floor is 2.35.

- **Dry run:** Actions, *Release Linux*, *Run workflow*. It uploads a
  `dist-linux` artifact (tarballs and `SHA256SUMS`) and creates no release.
- **Draft release:** push a tag `linux-vX.Y.Z`, where `X.Y.Z` is the version in
  the manifests (`scripts/check-version.sh vX.Y.Z` must pass). The `ci`
  workflow runs as a gate, the build jobs run, and a draft pre-release named
  after the tag gets the tarballs and `SHA256SUMS`. If a release for that tag
  already exists the job stops and does not touch it.

The tag is `linux-v*`, not `v*`, on purpose. `release.yml` already creates a
draft for every `v*` tag and deletes an existing draft for that tag before it
does, so a second workflow on the same tag would collide with it. The
`release.yml` Linux tarballs (`acd-<version>-<target>.tar.gz`) are unchanged;
the new archives carry the units, the plugin files and the smoke test as well.

Each archive is packed with sorted names, owner 0, a fixed mtime and
`gzip -n`, so the same build output gives the same archive. Binaries are built
with `--locked`, stripped, and linked with the same `--remap-path-prefix`
flags as the PKGBUILDs.

A `Run the packaged binaries` step extracts each archive and checks
`acd --version` and `acd-inbox --version` on a runner of the same architecture.

## Reproducibility

What the build does for byte-identical output:

| Item | State |
|---|---|
| `--locked` / `--frozen` | Used in both PKGBUILDs and the workflow. |
| Build and registry paths in the binary | Remapped with `--remap-path-prefix` (`$PWD` to `/build/agent-caller`, `$CARGO_HOME` to `/cargo`). |
| Strip | `makepkg` strips (default `strip` option); the workflow runs `strip`. |
| `SOURCE_DATE_EPOCH` | Rust embeds no build time. `makepkg` clamps file times in the package to it; the workflow uses the commit time for the archive. |
| Toolchain | Not pinned. Arch builds with the `rust` package version of the day; the workflow with `stable`. The same tag built with different compilers can differ. |

For a repeatable Arch package, set the epoch to the tag's commit time:

```sh
SOURCE_DATE_EPOCH=$(git log -1 --format=%ct v0.9.0) makepkg
```

The container test builds each package twice in different directories and
compares the installed binary; its result is in the pull request that added it.

## Testing the packages

```sh
packaging/test-arch.sh
```

It needs `docker`. In an `archlinux` container it installs the build tools,
builds both packages as a non-root user, installs them with `pacman -U`,
runs `packaging/smoke-test.sh`, verifies both units with `systemd-analyze
verify`, builds a `pkgrel=2` upgrade, removes the packages, checks that the
config and tokens in the build user's home are byte-for-byte unchanged and
owner-only through the upgrade and the removal, and rebuilds in another
directory to compare binaries. It tests the committed `HEAD`, not uncommitted
changes. `packaging/arch/container-test.sh` is the part that runs in the
container.

`ARCH_TEST_TOOLCHAIN=DIR packaging/test-arch.sh` is the variant for hosts where
the Arch mirrors are blocked: `DIR` holds copies of `~/.rustup` (`DIR/rustup`)
and `~/.cargo` (`DIR/cargo`), and the image `archlinux/archlinux:base-devel`
supplies `makepkg`. That variant runs `makepkg -d` and builds from a tar of
`HEAD` instead of `git+https`, so it does not exercise the `makedepends`
check, the git source line, `shellcheck` or `namcap`. Run the normal variant
once where `pacman` works.

### On the Omarchy machine

```sh
packaging/smoke-test.sh --live
```

It exits non-zero on any failed check. It checks `acd --version`,
`acd-inbox --version`, that both versions agree, that both units verify,
that `acd init` in a temporary `HOME` makes owner-only files and that
`acd.service` equals `acd service install --print`. It compares
`~/.local/bin/acd-inbox` and `~/.config/omarchy/plugins/tonibergholm.agentcaller`
with the package copies, so a stale install after an upgrade fails. With
`--live` it also runs `acd-inbox status` against the real session. Packages
that are not installed are reported as `SKIP`. It never reads or prints a token
and never touches your real `~/.config/acd`.

## Publishing to the AUR (not done)

Nothing publishes to the AUR. The owner would need:

1. An AUR account, and an SSH public key added to it (AUR account page).
   The private key would be a repository secret, for example `AUR_SSH_KEY`,
   used by a job that holds no other secret.
2. A decision on package names. `agent-caller` and `agent-caller-omarchy`
   are used here; check that they are free on the AUR first.
3. Per release: bump `pkgver` (reset `pkgrel=1`), run
   `makepkg --printsrcinfo > .SRCINFO` in each package directory, then push
   `PKGBUILD`, `.SRCINFO` and the `.install` file to
   `ssh://aur@aur.archlinux.org/<name>.git` on `master`.
4. Optionally the `-git` variant that tracks `main`; not written.

The PKGBUILDs here use `sha256sums=(SKIP)` because the source is a git tag:
git checks the object ids. That is only as trustworthy as the tag. See below.

## Signing, later

Signed releases are a later roadmap step, after the Apple membership renewal.
Nothing below exists today and nothing in the workflow pretends it does.
When it is time, the owner would need:

- A GPG (or SSH) signing key used only for releases, and its public key
  published (keys.openpgp.org and the repository).
- Signed tags (`git tag -s`), then in the PKGBUILDs `validpgpkeys=(<fingerprint>)`
  and `#tag=v$pkgver?signed` on the source, so `makepkg` refuses an unsigned or
  wrong-key tag.
- A detached signature of `SHA256SUMS` (`gpg --detach-sign --armor`) attached
  to the release, and the matching `gpg --verify` line in the section above.
  The key would be a repository secret used by one job.

## Residuals

- `acd-inbox install` keeps a per-user copy of the binary (above).
- The Omarchy plugin is not installed under a system path because the search
  path is unknown.
- No completions; the CLI does not generate them.
- The toolchain is not pinned.
- Verified on x86_64 only. `aarch64` is built by the workflow on a native
  runner but was not built in this session.
- The macOS app and `acd` on macOS are not packaged here.
