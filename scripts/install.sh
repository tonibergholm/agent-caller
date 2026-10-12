#!/bin/sh
# Installs acd (and acd-inbox on Linux) from a release tarball into a user bin directory.
#
# Usage:
#   scripts/install.sh --tag v0.1.0 [--bin-dir DIR]
#       Downloads the tarball and SHA256SUMS from the GitHub release.
#   scripts/install.sh --version 0.1.0 --from-dir DIR [--bin-dir DIR]
#       Uses DIR/acd-<version>-<target>.tar.gz and DIR/SHA256SUMS. No network.
#
# The default bin directory is $HOME/.local/bin. The script never uses sudo.
# It checks the SHA-256 of the tarball against SHA256SUMS before it extracts
# anything, and it installs nothing when the check fails.
set -eu

REPO="tonibergholm/agent-caller"
BASE_URL="https://github.com/$REPO/releases/download"

usage() {
	awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"
}

die() {
	echo "install.sh: $*" >&2
	exit 1
}

tag=""
version=""
from_dir=""
bin_dir="$HOME/.local/bin"

while [ "$#" -gt 0 ]; do
	case "$1" in
	--tag)
		[ "$#" -ge 2 ] || die "--tag needs a value"
		tag="$2"
		shift 2
		;;
	--version)
		[ "$#" -ge 2 ] || die "--version needs a value"
		version="$2"
		shift 2
		;;
	--from-dir)
		[ "$#" -ge 2 ] || die "--from-dir needs a value"
		from_dir="$2"
		shift 2
		;;
	--bin-dir)
		[ "$#" -ge 2 ] || die "--bin-dir needs a value"
		bin_dir="$2"
		shift 2
		;;
	-h | --help)
		usage
		exit 0
		;;
	*)
		die "unknown argument: $1 (see --help)"
		;;
	esac
done

if [ -n "$tag" ] && [ -n "$from_dir" ]; then
	die "use either --tag or --from-dir, not both"
fi
if [ -n "$tag" ]; then
	case "$tag" in
	v*) version="${tag#v}" ;;
	*) die "--tag must look like v0.1.0" ;;
	esac
fi
if [ -z "$version" ]; then
	die "give --tag vX.Y.Z (or --version X.Y.Z with --from-dir)"
fi
if [ -n "$from_dir" ] && [ ! -d "$from_dir" ]; then
	die "--from-dir is not a directory: $from_dir"
fi

# ACD_INSTALL_TARGET replaces the detected target. The tests in scripts/tests use it.
if [ -n "${ACD_INSTALL_TARGET:-}" ]; then
	target="$ACD_INSTALL_TARGET"
else
	os=$(uname -s)
	arch=$(uname -m)
	case "$os/$arch" in
	Darwin/arm64) target="aarch64-apple-darwin" ;;
	Darwin/x86_64) target="x86_64-apple-darwin" ;;
	Linux/x86_64) target="x86_64-unknown-linux-gnu" ;;
	Linux/aarch64 | Linux/arm64) target="aarch64-unknown-linux-gnu" ;;
	*) die "no release build for $os/$arch" ;;
	esac
fi

archive="acd-$version-$target.tar.gz"

sha256_of() {
	if command -v sha256sum >/dev/null 2>&1; then
		sha256sum "$1" | awk '{ print $1 }'
	else
		shasum -a 256 "$1" | awk '{ print $1 }'
	fi
}

work=$(mktemp -d "${TMPDIR:-/tmp}/acd-install.XXXXXX")
trap 'rm -rf "$work"' EXIT
trap 'rm -rf "$work"; exit 130' INT TERM
mkdir "$work/extract"

if [ -n "$from_dir" ]; then
	tarball="$from_dir/$archive"
	sums="$from_dir/SHA256SUMS"
else
	[ -n "$tag" ] || die "--tag is required to download"
	command -v curl >/dev/null 2>&1 || die "curl is required to download"
	tarball="$work/$archive"
	sums="$work/SHA256SUMS"
	echo "Downloading $archive from $tag"
	curl -fsSL --proto '=https' --tlsv1.2 -o "$sums" "$BASE_URL/$tag/SHA256SUMS" ||
		die "could not download SHA256SUMS for $tag"
	curl -fsSL --proto '=https' --tlsv1.2 -o "$tarball" "$BASE_URL/$tag/$archive" ||
		die "could not download $archive for $tag"
fi

[ -f "$tarball" ] || die "missing $archive (no build for $target in this release?)"
[ -f "$sums" ] || die "missing SHA256SUMS"

# Exactly one checksum line must name the archive. sha256sum writes "hash  name"
# and binary mode writes "hash *name".
expected=$(awk -v f="$archive" '$2 == f || $2 == "*" f { print $1; n++ } END { if (n != 1) exit 1 }' "$sums") ||
	die "SHA256SUMS does not list $archive exactly once"

actual=$(sha256_of "$tarball")
if [ "$actual" != "$expected" ]; then
	die "checksum mismatch for $archive (expected $expected, got $actual); nothing installed"
fi
echo "Checksum OK: $archive"

# Refuse absolute paths and parent-directory entries before extracting.
if tar -tzf "$tarball" | awk '/^\// || /(^|\/)\.\.(\/|$)/ { bad = 1 } END { exit bad }'; then
	:
else
	die "$archive contains absolute or parent-directory paths; nothing installed"
fi
tar -xzf "$tarball" -C "$work/extract"

[ -f "$work/extract/acd" ] || die "$archive has no acd binary"

mkdir -p "$bin_dir"
install -m 0755 "$work/extract/acd" "$bin_dir/acd"
echo "Installed $bin_dir/acd"
if [ -f "$work/extract/acd-inbox" ]; then
	install -m 0755 "$work/extract/acd-inbox" "$bin_dir/acd-inbox"
	echo "Installed $bin_dir/acd-inbox"
fi

case ":$PATH:" in
*":$bin_dir:"*) ;;
*) echo "Note: $bin_dir is not on PATH. Add it to your shell profile." ;;
esac

"$bin_dir/acd" --version
echo "Next: acd init, then acd service install (see README, Install the daemon)."
