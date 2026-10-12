#!/bin/sh
# Runs every release script test. Exit status is non-zero if any test fails.
#
# Usage: scripts/tests/run.sh
# CI runs it on ubuntu-latest (.github/workflows/ci.yml).
set -u

dir=$(cd "$(dirname "$0")" && pwd)
status=0
for test_file in "$dir"/test_*.sh; do
	echo "== $(basename "$test_file")"
	sh "$test_file" || status=1
done

if [ "$status" -eq 0 ]; then
	echo "All release script tests passed."
else
	echo "Release script tests FAILED." >&2
fi
exit "$status"
