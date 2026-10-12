#!/bin/sh
# Checks that every version field agrees and that CHANGELOG.md has a section for it.
#
# Usage: scripts/check-version.sh [TAG]
#   TAG is optional, for example v0.1.0. When given, it must equal v<version>.
# On success, prints the version to stdout and exits 0. Errors go to stderr.
# ACD_REPO_ROOT overrides the repository root (used by scripts/tests).
set -eu

root="${ACD_REPO_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"
cd "$root"

fail=0
die() {
	echo "check-version: $*" >&2
	fail=1
}

# toml_value TABLE KEY FILE: prints the first KEY = "value" inside TABLE.
toml_value() {
	awk -v table="$1" -v key="$2" '
		/^\[/ { in_table = ($0 == table) }
		in_table && index($0, key) == 1 && $0 ~ "^" key "[ \t]*=" {
			gsub(/^[^"]*"|"[^"]*$/, "")
			print
			exit
		}
	' "$3"
}

# lock_version NAME: prints the version of package NAME in the root Cargo.lock.
lock_version() {
	awk -v n="$1" '
		/^name = / { cur = $0; sub(/^name = "/, "", cur); sub(/"$/, "", cur); next }
		/^version = / && cur == n { v = $0; sub(/^version = "/, "", v); sub(/"$/, "", v); print v; exit }
	' Cargo.lock
}

json_field() {
	python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))[sys.argv[2]])' "$1" "$2"
}

workspace=$(toml_value "[workspace.package]" version Cargo.toml)
[ -n "$workspace" ] || die "no version under [workspace.package] in Cargo.toml"

check() {
	# $1 label, $2 value
	if [ "$2" != "$workspace" ]; then
		die "$1 is '$2', expected '$workspace' (the workspace version)"
	fi
}

check "app/src-tauri/Cargo.toml [package]" "$(toml_value "[package]" version app/src-tauri/Cargo.toml)"
check "app/package.json" "$(json_field app/package.json version)"
check "app/package-lock.json (root)" "$(json_field app/package-lock.json version)"
check "app/package-lock.json (packages[\"\"])" \
	"$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["packages"][""]["version"])' app/package-lock.json)"
check "app/src-tauri/tauri.conf.json" "$(json_field app/src-tauri/tauri.conf.json version)"
check "app/src-tauri/Cargo.lock (agent-caller-app)" \
	"$(awk '/^name = "agent-caller-app"$/ { getline; gsub(/^[^"]*"|"[^"]*$/, ""); print; exit }' app/src-tauri/Cargo.lock)"

for manifest in crates/*/Cargo.toml; do
	if ! grep -q '^version\.workspace = true' "$manifest"; then
		die "$manifest does not inherit the version (expected 'version.workspace = true')"
	fi
	name=$(toml_value "[package]" name "$manifest")
	check "Cargo.lock ($name)" "$(lock_version "$name")"
done

if ! grep -q "^## \[$workspace\]" CHANGELOG.md; then
	die "CHANGELOG.md has no '## [$workspace]' section"
fi

if [ "$#" -ge 1 ]; then
	tag="$1"
	if [ "$tag" != "v$workspace" ]; then
		die "tag '$tag' does not match version 'v$workspace'"
	fi
fi

if [ "$fail" -ne 0 ]; then
	exit 1
fi
echo "$workspace"
