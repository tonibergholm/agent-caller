#!/bin/sh
# Tests for scripts/release-notes.sh. Run by scripts/tests/run.sh.
# Each test runs in a subshell that sets WORK, so SC2031 does not apply.
# shellcheck disable=SC2031
# shellcheck source=./lib.sh
. "$(dirname "$0")/lib.sh"

NOTES=${ACD_NOTES_SCRIPT:-$ROOT/scripts/release-notes.sh}

prepare_changelog() {
	mkdir -p "$WORK/tree"
	cat >"$WORK/tree/CHANGELOG.md" <<'EOF'
# Changelog

## [Unreleased]

## [0.2.0] - 2026-11-01

### Added

- Second release entry.

## [0.1.0] - 2026-10-12

### Added

- First entry, line one.

- First entry, line two.

### Known limits

- A limit.
EOF
}

test_prints_section_without_heading() {
	prepare_changelog
	run_capture R env ACD_REPO_ROOT="$WORK/tree" sh "$NOTES" 0.1.0
	assert_eq "$R_RC" 0 "exit status"
	assert_contains "$R_OUT" "- First entry, line one."
	assert_contains "$R_OUT" "- A limit."
	case "$R_OUT" in
	*"## [0.1.0]"*) fail "heading must not be part of the notes" ;;
	esac
	case "$R_OUT" in
	*"Second release entry"*) fail "notes leaked into the next section" ;;
	esac
}

test_section_ends_at_next_heading() {
	prepare_changelog
	run_capture R env ACD_REPO_ROOT="$WORK/tree" sh "$NOTES" 0.2.0
	assert_eq "$R_RC" 0 "exit status"
	assert_contains "$R_OUT" "Second release entry."
	case "$R_OUT" in
	*"First entry"*) fail "notes of 0.1.0 leaked into 0.2.0" ;;
	esac
}

test_fails_for_missing_version() {
	prepare_changelog
	run_capture R env ACD_REPO_ROOT="$WORK/tree" sh "$NOTES" 9.9.9
	assert_eq "$R_RC" 1 "exit status"
	assert_contains "$R_ERR" "no CHANGELOG.md entry for 9.9.9"
	assert_eq "$R_OUT" "" "stdout"
}

test_fails_for_empty_section() {
	prepare_changelog
	run_capture R env ACD_REPO_ROOT="$WORK/tree" sh "$NOTES" Unreleased
	assert_eq "$R_RC" 1 "exit status"
}

test_requires_one_argument() {
	prepare_changelog
	run_capture R env ACD_REPO_ROOT="$WORK/tree" sh "$NOTES"
	assert_eq "$R_RC" 2 "exit status"
	assert_contains "$R_ERR" "usage:"
}

run_tests "$0"
