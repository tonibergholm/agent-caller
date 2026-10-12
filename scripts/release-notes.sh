#!/bin/sh
# Prints the CHANGELOG.md section for one version, without its heading.
#
# Usage: scripts/release-notes.sh 0.1.0 > notes.md
# Exits 1 when the section is missing or empty.
set -eu

if [ "$#" -ne 1 ]; then
	echo "usage: $0 VERSION" >&2
	exit 2
fi

root="${ACD_REPO_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"

notes=$(awk -v v="$1" '
	index($0, "## [" v "]") == 1 { on = 1; next }
	on && /^## \[/ { exit }
	on { print }
' "$root/CHANGELOG.md")

# Trim leading and trailing blank lines.
notes=$(printf '%s\n' "$notes" | awk '
	NF { seen = 1 }
	seen { lines[++n] = $0 }
	END {
		while (n > 0 && lines[n] ~ /^[ \t]*$/) n--
		for (i = 1; i <= n; i++) print lines[i]
	}
')

if [ -z "$notes" ]; then
	echo "release-notes: no CHANGELOG.md entry for $1" >&2
	exit 1
fi
printf '%s\n' "$notes"
