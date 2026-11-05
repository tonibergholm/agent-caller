#!/usr/bin/env bash
# Static analysis and headless wiring tests for the Omarchy plugin's QML.
# Needs Qt 6 declarative tools (Debian/Ubuntu: qt6-declarative-dev-tools and
# the qml6-module-qttest, -qtquick, -qtquick-layouts, -qtquick-controls,
# -qtqml-workerscript packages). Quickshell itself is NOT needed: tests/stubs
# stands in for the qs.Commons, qs.Ui and Quickshell modules.
#   tests/run-qml.sh            lint, then run the tests
#   tests/run-qml.sh --update   rewrite tests/qmllint.known from the current lint output
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
plugin="$(dirname "$here")"
for d in /usr/lib/qt6/bin /usr/lib64/qt6/bin /usr/lib/qt/bin; do if [ -d "$d" ]; then PATH="$d:$PATH"; fi; done
export PATH
# QMLLINT and QMLTESTRUNNER override the tools (the wrapper's own tests use fakes).
qmllint_bin="${QMLLINT:-qmllint}"
qmltestrunner_bin="${QMLTESTRUNNER:-qmltestrunner}"
for tool in "$qmllint_bin" "$qmltestrunner_bin"; do
  command -v "$tool" >/dev/null || { echo "$tool not found: install the Qt 6 declarative tools" >&2; exit 2; }
done
export QT_QPA_PLATFORM=offscreen
# A private runtime dir, never the real one, removed on every exit.
XDG_RUNTIME_DIR="$(mktemp -d)"
export XDG_RUNTIME_DIR
trap 'rm -rf "$XDG_RUNTIME_DIR"' EXIT

# One line per warning: file | message | the source line. qmllint 6.4 reports
# every `Model.X` class of a JS library as unknown and ids used across a
# delegate as unqualified; those are listed in qmllint.known and nothing else
# may appear.
# Fatal output (an Error: line, or a nonzero exit that printed no Warning:
# line, i.e. a crash or a bad invocation) fails the run instead of being dropped.
lint() {
  for f in "$plugin"/*.qml; do
    rc=0
    out="$("$qmllint_bin" -I "$here/stubs" "$f" 2>&1)" || rc=$?
    if printf '%s\n' "$out" | grep -q '^Error: '; then
      echo "qmllint: error in $(basename "$f"):" >&2
      printf '%s\n' "$out" >&2
      exit 1
    fi
    # qmllint exits 255 when it only warned; any other nonzero status is a crash or bad invocation.
    if [ "$rc" -ne 0 ] && { [ "$rc" -ne 255 ] || ! printf '%s\n' "$out" | grep -q '^Warning: '; }; then
      echo "qmllint: exit $rc on $(basename "$f"), not a warnings-only run:" >&2
      printf '%s\n' "$out" >&2
      exit 1
    fi
    printf '%s\n' "$out" | awk -v file="$(basename "$f")" '
      /^Warning: / { msg = $0; sub(/^Warning: [^ ]*:[0-9]+:[0-9]+: /, "", msg); getline code; gsub(/^[ \t]+/, "", code); print file " | " msg " | " code }'
  done | sort -u
}

if [ "${1:-}" = "--update" ]; then
  found="$(lint)"
  printf '%s\n' "$found" > "$here/qmllint.known"
  echo "wrote $here/qmllint.known"
  exit 0
fi

found="$(lint)"
new="$(comm -23 <(printf '%s\n' "$found" | sed '/^$/d') <(sort -u "$here/qmllint.known"))"
if [ -n "$new" ]; then
  echo "qmllint: warnings not in tests/qmllint.known:" >&2
  printf '%s\n' "$new" >&2
  exit 1
fi
echo "qmllint: no new warnings"

cd "$here"
"$qmltestrunner_bin" -input qml -import stubs
