#!/bin/sh
# Tests for scripts/check-version.sh. Run by scripts/tests/run.sh.
# Each test runs in a subshell that sets WORK, so SC2031 does not apply.
# shellcheck disable=SC2031
# shellcheck source=./lib.sh
. "$(dirname "$0")/lib.sh"

CHECK=${ACD_CHECK_SCRIPT:-$ROOT/scripts/check-version.sh}

# Copies the version-bearing files of the repository into $WORK/tree.
prepare_tree() {
	_tree=$WORK/tree
	mkdir -p "$_tree/app/src-tauri" "$_tree/crates"
	cp "$ROOT/CHANGELOG.md" "$ROOT/Cargo.toml" "$ROOT/Cargo.lock" "$_tree/"
	cp "$ROOT/app/package.json" "$ROOT/app/package-lock.json" "$_tree/app/"
	cp "$ROOT/app/src-tauri/Cargo.toml" "$ROOT/app/src-tauri/Cargo.lock" \
		"$ROOT/app/src-tauri/tauri.conf.json" "$_tree/app/src-tauri/"
	for _m in "$ROOT"/crates/*/Cargo.toml; do
		_d=$(basename "$(dirname "$_m")")
		mkdir -p "$_tree/crates/$_d"
		cp "$_m" "$_tree/crates/$_d/"
	done
}

# replace FILE OLD NEW: replaces the first OLD in FILE. Fails when OLD is absent,
# so a stale test fixture cannot pass silently.
replace() {
	python3 -c '
import sys
path, old, new = sys.argv[1:4]
text = open(path).read()
if old not in text:
    sys.exit("fixture text not found in " + path)
open(path, "w").write(text.replace(old, new, 1))
' "$1" "$2" "$3"
}

check_in_tree() {
	run_capture R env ACD_REPO_ROOT="$WORK/tree" sh "$CHECK" "$@"
}

test_passes_on_consistent_tree() {
	prepare_tree
	check_in_tree
	assert_eq "$R_RC" 0 "exit status"
	assert_eq "$R_OUT" "0.1.0" "printed version"
}

test_passes_with_matching_tag() {
	prepare_tree
	check_in_tree v0.1.0
	assert_eq "$R_RC" 0 "exit status"
}

test_rejects_mismatched_tag() {
	prepare_tree
	check_in_tree v0.2.0
	assert_eq "$R_RC" 1 "exit status"
	assert_contains "$R_ERR" "tag 'v0.2.0' does not match"
}

test_rejects_tag_without_v_prefix() {
	prepare_tree
	check_in_tree 0.1.0
	assert_eq "$R_RC" 1 "exit status"
	assert_contains "$R_ERR" "does not match"
}

test_rejects_package_json_drift() {
	prepare_tree
	replace "$WORK/tree/app/package.json" '"version": "0.1.0"' '"version": "0.1.1"'
	check_in_tree
	assert_eq "$R_RC" 1 "exit status"
	assert_contains "$R_ERR" "app/package.json is '0.1.1'"
}

test_rejects_package_lock_root_drift() {
	prepare_tree
	replace "$WORK/tree/app/package-lock.json" '"version": "0.1.0"' '"version": "0.1.1"'
	check_in_tree
	assert_eq "$R_RC" 1 "exit status"
	assert_contains "$R_ERR" "package-lock.json (root)"
}

test_rejects_tauri_conf_drift() {
	prepare_tree
	replace "$WORK/tree/app/src-tauri/tauri.conf.json" '"version": "0.1.0"' '"version": "0.1.2"'
	check_in_tree
	assert_eq "$R_RC" 1 "exit status"
	assert_contains "$R_ERR" "tauri.conf.json is '0.1.2'"
}

test_rejects_app_cargo_toml_drift() {
	prepare_tree
	replace "$WORK/tree/app/src-tauri/Cargo.toml" 'version = "0.1.0"' 'version = "0.1.3"'
	check_in_tree
	assert_eq "$R_RC" 1 "exit status"
	assert_contains "$R_ERR" "app/src-tauri/Cargo.toml [package] is '0.1.3'"
}

test_rejects_app_cargo_lock_drift() {
	prepare_tree
	replace "$WORK/tree/app/src-tauri/Cargo.lock" \
		'name = "agent-caller-app"
version = "0.1.0"' 'name = "agent-caller-app"
version = "0.1.4"'
	check_in_tree
	assert_eq "$R_RC" 1 "exit status"
	assert_contains "$R_ERR" "agent-caller-app) is '0.1.4'"
}

test_rejects_root_cargo_lock_drift() {
	prepare_tree
	replace "$WORK/tree/Cargo.lock" \
		'name = "acd-core"
version = "0.1.0"' 'name = "acd-core"
version = "0.1.5"'
	check_in_tree
	assert_eq "$R_RC" 1 "exit status"
	assert_contains "$R_ERR" "Cargo.lock (acd-core) is '0.1.5'"
}

test_rejects_crate_that_does_not_inherit_version() {
	prepare_tree
	replace "$WORK/tree/crates/acd/Cargo.toml" 'version.workspace = true' 'version = "0.1.0"'
	check_in_tree
	assert_eq "$R_RC" 1 "exit status"
	assert_contains "$R_ERR" "crates/acd/Cargo.toml does not inherit the version"
}

test_rejects_missing_changelog_section() {
	prepare_tree
	replace "$WORK/tree/CHANGELOG.md" '## [0.1.0]' '## [0.1.1]'
	check_in_tree
	assert_eq "$R_RC" 1 "exit status"
	assert_contains "$R_ERR" "CHANGELOG.md has no '## [0.1.0]' section"
}

run_tests "$0"
