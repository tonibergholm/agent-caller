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
# The provenance check runs gh. Point it at a missing command unless a test
# installs a stub, so a developer's real gh never reaches the network here.
export ACD_INSTALL_GH=/nonexistent/gh

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

# A stub gh: `auth status` exits with $2 (default 0); any other call records
# its arguments and exits with $1.
make_stub_gh() {
	mkdir -p "$WORK/stub"
	printf '#!/bin/sh\n[ "$1" = auth ] && exit %s\necho "$@" >"%s/gh-args"\nexit %s\n' "${2:-0}" "$WORK" "$1" >"$WORK/stub/gh"
	chmod +x "$WORK/stub/gh"
	ACD_INSTALL_GH="$WORK/stub/gh"
	export ACD_INSTALL_GH
}

test_continues_when_the_attestation_verifies() {
	prepare_release
	make_stub_gh 0
	install_from
	assert_eq "$R_RC" 0 "exit status"
	assert_file "$WORK/bin/acd"
	assert_contains "$(cat "$WORK/gh-args")" "attestation verify $WORK/rel/$ARCHIVE --repo tonibergholm/agent-caller"
	assert_contains "$R_OUT" "Provenance OK"
}

# Every acd-<version>-<target> archive is built and attested by release.yml
# (release-linux.yml makes agent-caller-linux-* archives, which install.sh
# never installs), so the signer and the tag ref are pinned for each target.
test_pins_signer_workflow_and_tag_ref() {
	for target in aarch64-apple-darwin x86_64-unknown-linux-gnu; do
		rm -rf "$WORK/rel" "$WORK/gh-args"
		ACD_INSTALL_TARGET=$target
		ARCHIVE=acd-0.1.0-$target.tar.gz
		prepare_release
		make_stub_gh 0
		install_from
		assert_eq "$R_RC" 0 "exit status ($target)"
		args=$(cat "$WORK/gh-args")
		assert_contains "$args" "attestation verify $WORK/rel/$ARCHIVE "
		assert_contains "$args" "--signer-workflow tonibergholm/agent-caller/.github/workflows/release.yml"
		assert_contains "$args" "--source-ref refs/tags/v0.1.0"
		assert_contains "$args" "--repo tonibergholm/agent-caller"
	done
}

# --tag downloads with curl. A stub curl writes the archive and sums.
test_pins_the_tag_ref_with_tag_flag() {
	prepare_release
	make_stub_gh 0
	mkdir -p "$WORK/stub"
	printf '#!/bin/sh\nwhile [ "$#" -gt 0 ]; do [ "$1" = -o ] && out=$2; shift; done\ncase "$out" in *SHA256SUMS) cp "%s/rel/SHA256SUMS" "$out" ;; *) cp "%s/rel/%s" "$out" ;; esac\n' "$WORK" "$WORK" "$ARCHIVE" >"$WORK/stub/curl"
	chmod +x "$WORK/stub/curl"
	PATH="$WORK/stub:$PATH" run_capture R sh "$INSTALL" --tag v0.1.0 --bin-dir "$WORK/bin"
	assert_eq "$R_RC" 0 "exit status"
	args=$(cat "$WORK/gh-args")
	assert_contains "$args" "--signer-workflow tonibergholm/agent-caller/.github/workflows/release.yml"
	assert_contains "$args" "--source-ref refs/tags/v0.1.0"
}

test_refuses_when_the_attestation_fails() {
	prepare_release
	make_stub_gh 1
	install_from
	assert_eq "$R_RC" 1 "exit status"
	assert_contains "$R_ERR" "provenance check failed"
	assert_no_path "$WORK/bin"
}

test_skips_provenance_when_asked() {
	prepare_release
	make_stub_gh 1
	export ACD_INSTALL_SKIP_PROVENANCE=1
	install_from
	unset ACD_INSTALL_SKIP_PROVENANCE
	assert_eq "$R_RC" 0 "exit status"
	assert_file "$WORK/bin/acd"
	assert_contains "$R_OUT" "skipped by ACD_INSTALL_SKIP_PROVENANCE"
	assert_no_path "$WORK/gh-args"
}

test_says_provenance_was_not_checked_when_gh_is_signed_out() {
	prepare_release
	make_stub_gh 1 1
	install_from
	assert_eq "$R_RC" 0 "exit status"
	assert_file "$WORK/bin/acd"
	assert_contains "$R_OUT" "Provenance not checked: gh is not signed in"
	assert_no_path "$WORK/gh-args"
}

test_says_provenance_was_not_checked_without_gh() {
	prepare_release
	install_from
	assert_eq "$R_RC" 0 "exit status"
	assert_file "$WORK/bin/acd"
	assert_contains "$R_OUT" "Provenance not checked"
	assert_contains "$R_OUT" "gh attestation verify $ARCHIVE --repo tonibergholm/agent-caller --signer-workflow tonibergholm/agent-caller/.github/workflows/release.yml --source-ref refs/tags/v0.1.0"
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
