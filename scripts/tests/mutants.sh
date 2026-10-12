#!/bin/sh
# Mutation check for the security checks in scripts/install.sh. Each mutant
# removes or weakens one check in a copy of the script. The matching test must
# then fail. A mutant that the tests miss means the test does not cover the check.
#
# Usage: scripts/tests/mutants.sh   (not run in CI; run it after changing install.sh)
# The sed expressions are single-quoted on purpose.
# shellcheck disable=SC2016
set -u

dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$dir/../.." && pwd)
work=$(mktemp -d "${TMPDIR:-/tmp}/acd-mutants.XXXXXX")
trap 'rm -rf "$work"' EXIT
status=0

# mutant NAME SED_EXPRESSION TEST_THAT_MUST_FAIL
mutant() {
	name=$1
	expr=$2
	expect=$3
	sed "$expr" "$root/scripts/install.sh" >"$work/install.sh"
	if cmp -s "$root/scripts/install.sh" "$work/install.sh"; then
		echo "BROKEN $name: the sed expression did not change install.sh"
		status=1
		return
	fi
	ACD_INSTALL_SCRIPT="$work/install.sh" sh "$dir/test_install.sh" >"$work/out" 2>&1
	if grep -q "^FAIL $expect\$" "$work/out"; then
		echo "caught $name (FAIL $expect)"
	else
		echo "MISSED $name: $expect still passes"
		sed 's/^/    /' "$work/out"
		status=1
	fi
}

mutant checksum-compare \
	's/if \[ "\$actual" != "\$expected" \]; then/if false; then/' \
	test_refuses_checksum_mismatch

mutant duplicate-sums-line \
	's/if (n != 1) exit 1/if (n < 1) exit 1/' \
	test_refuses_duplicate_sums_line

mutant missing-sums-line \
	's/if (n != 1) exit 1/if (n > 1) exit 1/' \
	test_refuses_missing_sums_line

mutant parent-directory-entry \
	's#(^|\\/)\\.\\.(\\/|\$)#NOMATCH#' \
	test_refuses_parent_directory_entry

mutant absolute-path-entry \
	's#/^\\// || ##' \
	test_refuses_absolute_path_entry

mutant bad-tag \
	's#\*) die "--tag must look like v0.1.0" ;;#*) version="$tag" ;;#' \
	test_refuses_bad_tag

if [ "$status" -eq 0 ]; then
	echo "Every mutant was caught."
else
	echo "Some mutants were missed." >&2
fi
exit "$status"
