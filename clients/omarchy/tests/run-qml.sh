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
for tool in qmllint qmltestrunner; do
  command -v "$tool" >/dev/null || { echo "$tool not found: install the Qt 6 declarative tools" >&2; exit 2; }
done
export QT_QPA_PLATFORM=offscreen
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-$(mktemp -d)}"

# One line per warning: file | message | the source line. qmllint 6.4 reports
# every `Model.X` class of a JS library as unknown and ids used across a
# delegate as unqualified; those are listed in qmllint.known and nothing else
# may appear.
lint() {
  for f in "$plugin"/*.qml; do
    { qmllint -I "$here/stubs" "$f" 2>&1 || true; } | awk -v file="$(basename "$f")" '
      /^Warning: / { msg = $0; sub(/^Warning: [^ ]*:[0-9]+:[0-9]+: /, "", msg); getline code; gsub(/^[ \t]+/, "", code); print file " | " msg " | " code }'
  done | sort -u
}

if [ "${1:-}" = "--update" ]; then
  lint > "$here/qmllint.known"
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
qmltestrunner -input qml -import stubs
