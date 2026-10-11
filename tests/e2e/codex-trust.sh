#!/usr/bin/env bash
# Codex only runs project hooks after the folder and each hook are trusted, and `-c` cannot
# grant either (checked with `codex app-server` hooks/list).  So this adds the scratch project's
# trust entries to ~/.codex/config.toml, after a backup, and `restore` puts the file back
# byte for byte.  Only entries for $PROJ are added; the user-level hooks stay untrusted (inert).
#   codex-trust.sh add | restore | status
. "$(dirname "$0")/lib.sh"
CFG=$HOME/.codex/config.toml; BAK=$E2E_ROOT/codex-config.before.toml; SUM=$E2E_ROOT/codex-config.before.sha256
case "${1:-status}" in
add)
  [ -e "$BAK" ] && { echo "already added (backup exists)"; exit 1; }
  mkdir -p "$PROJ/.codex"
  CODEX_HOME=$E2E_ROOT/codex-cfg; mkdir -p "$CODEX_HOME"
  CODEX_HOME=$CODEX_HOME acd hooks install codex >/dev/null || exit 1
  cp "$CODEX_HOME/hooks.json" "$PROJ/.codex/hooks.json"
  cp -p "$CFG" "$BAK"; shasum -a 256 "$CFG" >"$SUM"
  printf '\n[projects."%s"]\ntrust_level = "trusted"\n' "$(cd "$PROJ" && pwd -P)" >>"$CFG"
  python3 "$E2E_DIR/codex_hooks.py" "$(cd "$PROJ" && pwd -P)" --trust-project-hooks >>"$CFG" || { cp -p "$BAK" "$CFG"; exit 1; }
  echo "added trust entries for $PROJ; backup $BAK" ;;
restore)
  [ -e "$BAK" ] || { echo "nothing to restore"; exit 0; }
  cp -p "$BAK" "$CFG" && [ "$(shasum -a 256 "$CFG" | cut -d' ' -f1)" = "$(cut -d' ' -f1 "$SUM")" ] && echo "config.toml restored, sha256 matches" && rm -f "$BAK" "$SUM" || { echo "RESTORE MISMATCH" >&2; exit 1; } ;;
status) [ -e "$BAK" ] && echo "trust entries ACTIVE (backup present)" || echo "no trust entries added" ;;
esac
