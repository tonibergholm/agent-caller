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
	args+=(-v "$ARCH_TEST_TOOLCHAIN:/opt/rust-ro:ro" -e ARCH_TEST_OFFLINE=1 -e CARGO_HOME=/opt/rust/cargo -e RUSTUP_HOME=/opt/rust/rustup)
	prep='mkdir -p /opt/rust && cp -a /opt/rust-ro/. /opt/rust/ && chmod -R a+rX /opt/rust && chmod -R go+w /opt/rust/cargo && export PATH=/opt/rust/cargo/bin:$PATH && '
else
	image=archlinux:latest
	prep=''
fi
exec docker run "${args[@]}" "$image" bash -c "${prep}exec bash /pkg/arch/container-test.sh"
