#!/bin/sh
# Tests for scripts/install.sh. Run by scripts/tests/run.sh.
# ACD_INSTALL_SCRIPT selects another copy of install.sh (used by mutants.sh).
# Each test runs in a subshell that sets WORK, so SC2031 does not apply.
# shellcheck disable=SC2031
# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib.sh
. "$(dirname "$0")/lib.sh"

INSTALL=${ACD_INSTALL_SCRIPT:-$ROOT/scripts/install.sh}
TARGET=x86_64-unknown-linux-gnu
ARCHIVE=acd-0.1.0-$TARGET.tar.gz
export ACD_INSTALL_TARGET=$TARGET

# A release directory holding one valid archive and its SHA256SUMS.
prepare_release() {
	mkdir -p "$WORK/src" "$WORK/rel"
	make_fake_acd "$WORK/src/acd"
	make_tarball "$WORK/rel" "$ARCHIVE" "acd=$WORK/src/acd"
	write_sums "$WORK/rel" "$ARCHIVE"
}

install_from() {
	run_capture R sh "$INSTALL" --version 0.1.0 --from-dir "$WORK/rel" --bin-dir "$WORK/bin"
}

test_installs_verified_archive() {
	prepare_release
	install_from
	assert_eq "$R_RC" 0 "exit status"
	assert_file "$WORK/bin/acd"
	[ -x "$WORK/bin/acd" ] || fail "installed acd is not executable"
	assert_contains "$R_OUT" "Checksum OK: $ARCHIVE"
}

test_refuses_checksum_mismatch() {
	prepare_release
	printf 'tampered' >>"$WORK/rel/$ARCHIVE"
	install_from
	assert_eq "$R_RC" 1 "exit status"
	assert_contains "$R_ERR" "checksum mismatch"
	assert_no_path "$WORK/bin"
}

test_refuses_duplicate_sums_line() {
	prepare_release
	cat "$WORK/rel/SHA256SUMS" "$WORK/rel/SHA256SUMS" >"$WORK/rel/SHA256SUMS.new"
	mv "$WORK/rel/SHA256SUMS.new" "$WORK/rel/SHA256SUMS"
	install_from
	assert_eq "$R_RC" 1 "exit status"
	assert_contains "$R_ERR" "exactly once"
	assert_no_path "$WORK/bin"
}

test_refuses_missing_sums_line() {
	prepare_release
	printf '%s  other-archive.tar.gz\n' "$(sha256_of "$WORK/rel/$ARCHIVE")" >"$WORK/rel/SHA256SUMS"
	install_from
	assert_eq "$R_RC" 1 "exit status"
	assert_contains "$R_ERR" "exactly once"
	assert_no_path "$WORK/bin"
}

test_refuses_parent_directory_entry() {
	mkdir -p "$WORK/src"
	make_fake_acd "$WORK/src/acd"
	mkdir -p "$WORK/rel"
	make_tarball "$WORK/rel" "$ARCHIVE" "acd=$WORK/src/acd" "../escape-acd=$WORK/src/acd"
	write_sums "$WORK/rel" "$ARCHIVE"
	install_from
	assert_eq "$R_RC" 1 "exit status"
	assert_contains "$R_ERR" "parent-directory"
	assert_no_path "$WORK/bin"
}

test_refuses_absolute_path_entry() {
	mkdir -p "$WORK/src"
	make_fake_acd "$WORK/src/acd"
	mkdir -p "$WORK/rel"
	make_tarball "$WORK/rel" "$ARCHIVE" "acd=$WORK/src/acd" "$WORK/absolute-escape=$WORK/src/acd"
	write_sums "$WORK/rel" "$ARCHIVE"
	install_from
	assert_eq "$R_RC" 1 "exit status"
	assert_contains "$R_ERR" "absolute"
	assert_no_path "$WORK/bin"
	assert_no_path "$WORK/absolute-escape"
}

test_refuses_missing_archive() {
	prepare_release
	rm "$WORK/rel/$ARCHIVE"
	install_from
	assert_eq "$R_RC" 1 "exit status"
	assert_contains "$R_ERR" "missing $ARCHIVE"
	assert_no_path "$WORK/bin"
}

test_refuses_missing_sums_file() {
	prepare_release
	rm "$WORK/rel/SHA256SUMS"
	install_from
	assert_eq "$R_RC" 1 "exit status"
	assert_contains "$R_ERR" "missing SHA256SUMS"
	assert_no_path "$WORK/bin"
}

test_refuses_bad_tag() {
	prepare_release
	run_capture R sh "$INSTALL" --tag 0.1.0 --bin-dir "$WORK/bin"
	assert_eq "$R_RC" 1 "exit status"
	assert_contains "$R_ERR" "--tag must look like v0.1.0"
	assert_no_path "$WORK/bin"
}

test_refuses_unknown_argument() {
	run_capture R sh "$INSTALL" --bogus
	assert_eq "$R_RC" 1 "exit status"
	assert_contains "$R_ERR" "unknown argument"
}

run_tests "$0"
