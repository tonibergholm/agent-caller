#!/usr/bin/env bash
# Codex only runs project hooks after the folder and each hook are trusted, and `-c` cannot
# grant either (checked with `codex app-server` hooks/list).  So `add` appends the scratch project's
# trust entries to the Codex config.toml, after a backup, and `restore` takes them out again.
# Only entries for $PROJ are added.  Isolation from the user's real acd additionally needs the user's own
# acd hooks in ~/.codex/hooks.json to be untrusted; `add` verifies that and rolls back if it does not hold.
#
#   codex-trust.sh add | restore | status
#
# State lives in a reboot-safe cache dir (not /tmp):
#   macOS  ~/Library/Caches/ace2e/      elsewhere  ${XDG_CACHE_HOME:-~/.cache}/ace2e/
#     config.before.toml    copy of config.toml before `add`
#     config.before.sha256  its sha256
#     config.block          the exact bytes `add` appended
#     config.post.sha256    sha256 of config.toml right after the append
#     config.path           which config.toml this belongs to
#
# restore: current == post-add sha  -> copy the backup back (byte for byte)
#          file still ends with the saved block -> cut the block off, keep edits made during the run
#          otherwise -> refuse (exit 1), print a diff against the backup, keep everything
. "$(dirname "$0")/lib.sh" || exit 1

CODEX_DIR=${CODEX_HOME:-$HOME/.codex}
CFG=$CODEX_DIR/config.toml
if [ "$(uname)" = Darwin ]; then STATE=$HOME/Library/Caches/ace2e; else STATE=${XDG_CACHE_HOME:-$HOME/.cache}/ace2e; fi
BAK=$STATE/config.before.toml; SUM=$STATE/config.before.sha256; BLOCK=$STATE/config.block
POST=$STATE/config.post.sha256; PATHF=$STATE/config.path
SCR_CODEX=$E2E_ROOT/codex-cfg   # scratch dir for `acd hooks install codex` (never called CODEX_HOME)
mkdir -p "$PROJ"
PROJ_REAL=$(cd "$PROJ" && pwd -P)
HEADER="[projects.\"$PROJ_REAL\"]"

sha() { shasum -a 256 "$1" | cut -d' ' -f1; }
sha1() { cut -d' ' -f1 "$1"; }
state_clear() { rm -f "$BAK" "$SUM" "$BLOCK" "$POST" "$PATHF"; rmdir "$STATE" 2>/dev/null || true; }

do_restore() {
  [ -e "$BAK" ] || { echo "nothing to restore (no backup in $STATE)"; return 0; }
  local cfg=$CFG cur
  [ -f "$PATHF" ] && cfg=$(cat "$PATHF")
  [ -f "$cfg" ] || { echo "RESTORE: $cfg is missing; backup kept in $STATE" >&2; return 1; }
  cur=$(sha "$cfg")
  if [ -f "$POST" ] && [ "$cur" = "$(sha1 "$POST")" ]; then
    cp -p "$BAK" "$cfg" && [ "$(sha "$cfg")" = "$(sha1 "$SUM")" ] \
      || { echo "RESTORE: copying the backup back failed or its sha256 differs; backup kept in $STATE" >&2; return 1; }
    echo "config.toml restored from the backup (unchanged since add; sha256 matches the original)"
  elif [ "$cur" = "$(sha1 "$SUM")" ]; then
    echo "config.toml already equals the original; nothing to undo"
  elif [ -s "$BLOCK" ] && python3 - "$cfg" "$BLOCK" <<'PY'
import sys
cfg, blk = sys.argv[1], sys.argv[2]
data, b = open(cfg, "rb").read(), open(blk, "rb").read()
if not data.endswith(b):
    sys.exit(1)
with open(cfg, "r+b") as f:
    f.truncate(len(data) - len(b))
PY
  then
    echo "config.toml changed during the run: removed exactly the scratch block, kept the other edits"
    [ "$(sha "$cfg")" = "$(sha1 "$SUM")" ] || echo "  (it now differs from the original backup because of those edits)"
  else
    echo "RESTORE REFUSED: config.toml changed during the run and no longer ends with the scratch block." >&2
    echo "  config: $cfg" >&2
    echo "  backup: $BAK  (kept)" >&2
    echo "  Remove the [projects.\"$PROJ_REAL\"] and [hooks.state.\"...\"] entries for the scratch project by hand." >&2
    echo "  diff -u backup current:" >&2
    diff -u "$BAK" "$cfg" | head -80 >&2
    return 1
  fi
  state_clear
}

case "${1:-status}" in
add)
  [ -e "$BAK" ] && { echo "already added (backup exists in $STATE); run restore first" >&2; exit 1; }
  [ -f "$CFG" ] || { echo "$CFG does not exist; refusing" >&2; exit 1; }
  if grep -qF "$HEADER" "$CFG"; then
    echo "$CFG already contains $HEADER but there is no backup (a crashed run, or you added it yourself)." >&2
    echo "Refusing: restoring is impossible to do safely.  Remove that entry (and the [hooks.state.\"...\"] entries for" >&2
    echo "$PROJ_REAL/.codex/hooks.json) by hand, then run add again." >&2
    exit 1
  fi
  [ "$CODEX_DIR" = "$HOME/.codex" ] || echo "note: CODEX_HOME=$CODEX_DIR differs from ~/.codex; the scenarios start Codex without CODEX_HOME" >&2
  mkdir -p "$PROJ/.codex" "$SCR_CODEX" "$STATE"; chmod 700 "$STATE"
  CODEX_HOME=$SCR_CODEX acd hooks install codex >/dev/null || exit 1
  cp "$SCR_CODEX/hooks.json" "$PROJ/.codex/hooks.json"
  cp -p "$CFG" "$BAK"; shasum -a 256 "$CFG" >"$SUM"; echo "$CFG" >"$PATHF"; : >"$BLOCK"
  # Stage 1: trust the folder (needed before hooks/list reports the project hooks).
  printf '\n[projects."%s"]\ntrust_level = "trusted"\n' "$PROJ_REAL" >"$BLOCK"
  cat "$BLOCK" >>"$CFG"
  # Stage 2: trust the project hooks only.
  if ! python3 "$E2E_DIR/codex_hooks.py" "$PROJ_REAL" --trust-project-hooks >"$STATE/hooks.part"; then
    echo "could not compute the project hook trust entries; rolling back" >&2
    rm -f "$STATE/hooks.part"
    do_restore; exit 1
  fi
  cat "$STATE/hooks.part" >>"$BLOCK"; cat "$STATE/hooks.part" >>"$CFG"; rm -f "$STATE/hooks.part"
  shasum -a 256 "$CFG" >"$POST"
  # Isolation precondition: nothing but the scratch project's hooks may be live.
  if ! python3 "$E2E_DIR/codex_hooks.py" "$PROJ_REAL" --verify; then
    echo "ISOLATION CHECK FAILED: a hook from outside the scratch project would run under Codex (see above)." >&2
    echo "Untrust or disable your own acd hooks in ~/.codex/hooks.json first (README, preconditions).  Rolling back." >&2
    do_restore; exit 1
  fi
  echo "added trust entries for $PROJ_REAL; isolation verified; backup in $STATE" ;;
restore)
  do_restore ;;
status)
  if [ -e "$BAK" ]; then echo "trust entries ACTIVE (backup present in $STATE; recover with: $0 restore)"
  elif grep -qF "$HEADER" "$CFG" 2>/dev/null; then echo "WARNING: $CFG contains $HEADER but there is no backup; remove it by hand"
  else echo "no trust entries added"; fi ;;
*) echo "usage: $0 add|restore|status" >&2; exit 2 ;;
esac
