#!/usr/bin/env bash
# Builds, installs, upgrades and removes both Arch packages in an archlinux
# container, from the committed HEAD of this repository.
#
#   packaging/test-arch.sh                 normal: pacman installs the build tools
#   ARCH_TEST_TOOLCHAIN=DIR packaging/test-arch.sh
#       offline: pacman is not reachable. DIR holds `rustup` and `cargo`
#       (a copy of ~/.rustup as DIR/rustup and ~/.cargo as DIR/cargo). The
#       image archlinux/archlinux:base-devel supplies makepkg, fakeroot,
#       strip and systemd-analyze. makepkg runs with -d and a tar source in
#       place of git+https, because git and cargo are not pacman packages.
#
# ARCH_TEST_GIT=1 (offline only): mount the host's git into the container so the
# PKGBUILDs' git source lines and the -git packages run for real.
# ARCH_TEST_SHELLCHECK=FILE (offline only): a shellcheck binary to mount.
#
# Uncommitted changes are not tested: the source is `git archive HEAD`.
# Needs docker. HTTPS_PROXY and a CA bundle in SSL_CERT_FILE are passed on.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(git -C "$here" rev-parse --show-toplevel)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
git -C "$repo" archive --prefix=agent-caller/ -o "$tmp/src.tar" HEAD

args=(--rm --network host -v "$tmp:/in:ro" -v "$here:/pkg:ro")
for v in HTTPS_PROXY https_proxy HTTP_PROXY http_proxy NO_PROXY no_proxy; do
	[ -n "${!v:-}" ] && args+=(-e "$v")
done
if [ -n "${SSL_CERT_FILE:-}" ] && [ -f "$SSL_CERT_FILE" ]; then
	args+=(-v "$SSL_CERT_FILE:/ca.crt:ro" -e SSL_CERT_FILE=/ca.crt -e CARGO_HTTP_CAINFO=/ca.crt -e GIT_SSL_CAINFO=/ca.crt)
fi
if [ -n "${ARCH_TEST_TOOLCHAIN:-}" ]; then
	image=archlinux/archlinux:base-devel
	gitpath=
	args+=(-v "$ARCH_TEST_TOOLCHAIN:/opt/rust-ro:ro" -e ARCH_TEST_OFFLINE=1 -e CARGO_HOME=/opt/rust/cargo -e RUSTUP_HOME=/opt/rust/rustup)
	if [ -n "${ARCH_TEST_GIT:-}" ]; then
		# git and the shared libraries it needs, copied from the host.
		mkdir -p "$tmp/hostgit/lib"
		mkdir -p "$tmp/hostgit/real" && cp "$(command -v git)" "$tmp/hostgit/real/git"
		cp -a "$(git --exec-path)" "$tmp/hostgit/git-core"
		# Not libc: the container's own libc must serve every process.
		ldd "$(command -v git)" "$tmp/hostgit/git-core/git-remote-https" 2>/dev/null | awk '$3 ~ /^\// { print $3 }' | sort -u |
			grep -Ev '/(libc|libm|libdl|libpthread|librt|ld-linux[^/]*)\.so' | xargs -r cp -L -t "$tmp/hostgit/lib"
		# A wrapper, so the host libraries reach git and nothing else.
		printf '#!/bin/sh\nexec env GIT_EXEC_PATH=/opt/hostgit/git-core LD_LIBRARY_PATH=/opt/hostgit/lib /opt/hostgit/real/git "$@"\n' >"$tmp/hostgit/git"
		chmod +x "$tmp/hostgit/git"
		args+=(-v "$tmp/hostgit:/opt/hostgit:ro")
		gitpath=/opt/hostgit:
	fi
	[ -n "${ARCH_TEST_SHELLCHECK:-}" ] && args+=(-v "$ARCH_TEST_SHELLCHECK:/usr/local/bin/shellcheck:ro")
	# shellcheck disable=SC2016 # $PATH expands in the container, on purpose
	prep='mkdir -p /opt/rust && cp -a /opt/rust-ro/. /opt/rust/ && chmod -R a+rX /opt/rust && chmod -R go+w /opt/rust/cargo && export PATH='"${gitpath:-}"'/opt/rust/cargo/bin:$PATH && '
else
	image=archlinux:latest
	prep=''
fi
exec docker run "${args[@]}" "$image" bash -c "${prep}exec bash /pkg/arch/container-test.sh"
