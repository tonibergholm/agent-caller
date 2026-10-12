# shellcheck shell=sh
# Shared helpers for the release script tests. Sourced by test_*.sh files.
# Needs only sh, awk, tar, sed and python3. Each test runs in its own subshell
# with a fresh WORK directory, which is removed afterwards.

# ROOT is read by the test files that source this library.
# shellcheck disable=SC2034
TESTS_DIR=$(cd "$(dirname "$0")" && pwd)
ROOT=${ACD_REPO_ROOT:-$(cd "$TESTS_DIR/../.." && pwd)}

# Failing assertions exit the test's subshell with status 1.
fail() {
	echo "      assertion failed: $*" >&2
	exit 1
}

assert_eq() {
	[ "$1" = "$2" ] || fail "expected [$2], got [$1]${3:+ ($3)}"
}

assert_contains() {
	case "$1" in
	*"$2"*) ;;
	*) fail "expected output to contain [$2]; output was: $1" ;;
	esac
}

assert_file() {
	[ -f "$1" ] || fail "expected file $1"
}

assert_no_path() {
	[ ! -e "$1" ] || fail "expected no path at $1"
}

# run_capture VAR_PREFIX cmd args...
# Sets ${PREFIX}_RC, ${PREFIX}_OUT (stdout) and ${PREFIX}_ERR (stderr).
run_capture() {
	_prefix=$1
	shift
	_rc=0
	"$@" >"$WORK/stdout" 2>"$WORK/stderr" || _rc=$?
	eval "${_prefix}_RC=$_rc"
	eval "${_prefix}_OUT=\$(cat \"\$WORK/stdout\")"
	eval "${_prefix}_ERR=\$(cat \"\$WORK/stderr\")"
}

sha256_of() {
	if command -v sha256sum >/dev/null 2>&1; then
		sha256sum "$1" | awk '{ print $1 }'
	else
		shasum -a 256 "$1" | awk '{ print $1 }'
	fi
}

# make_tarball DIR NAME ENTRY... : ENTRY is ARCNAME=SOURCE_FILE.
# Writes DIR/NAME with python3 so that unusual member names can be used.
make_tarball() {
	_dir=$1
	_name=$2
	shift 2
	python3 -c '
import io, sys, tarfile
out, specs = sys.argv[1], sys.argv[2:]
with tarfile.open(out, "w:gz") as tar:
    for spec in specs:
        arcname, source = spec.split("=", 1)
        data = open(source, "rb").read()
        info = tarfile.TarInfo(arcname)
        info.size = len(data)
        info.mode = 0o755
        tar.addfile(info, io.BytesIO(data))
' "$_dir/$_name" "$@"
}

# write_sums DIR FILE... : writes DIR/SHA256SUMS listing each FILE (in DIR).
write_sums() {
	_dir=$1
	shift
	: >"$_dir/SHA256SUMS"
	for _f in "$@"; do
		printf '%s  %s\n' "$(sha256_of "$_dir/$_f")" "$_f" >>"$_dir/SHA256SUMS"
	done
}

# A fake acd binary for installer tests. It only answers --version.
make_fake_acd() {
	printf '#!/bin/sh\necho "acd 0.1.0 (test)"\n' >"$1"
	chmod 755 "$1"
}

# Runs every test_* function of the calling file, each in its own subshell.
run_tests() {
	_file=$1
	_failed=0
	_names=$(sed -n 's/^\(test_[A-Za-z0-9_]*\)() *{.*/\1/p' "$_file")
	for _t in $_names; do
		_work=$(mktemp -d "${TMPDIR:-/tmp}/acd-test.XXXXXX")
		if (WORK=$_work; export WORK; cd "$_work" && "$_t") >"$_work/log" 2>&1; then
			echo "ok   $_t"
		else
			echo "FAIL $_t"
			sed 's/^/     /' "$_work/log"
			_failed=$((_failed + 1))
		fi
		rm -rf "$_work"
	done
	[ "$_failed" -eq 0 ]
}
