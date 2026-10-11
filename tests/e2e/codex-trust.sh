#!/usr/bin/env bash
# Codex only runs project hooks after the folder and each hook are trusted, and `-c` cannot
# grant either (checked with `codex app-server` hooks/list).  So `add` appends the scratch project's
# trust entries to the Codex config.toml, after a backup, and `restore` takes them out again.
# Only entries for $PROJ are added.  Isolation from the user's real acd additionally needs the user's own
# acd hooks in ~/.codex/hooks.json to be untrusted; `add` verifies that and rolls back if it does not hold.
#
#   codex-trust.sh add | restore | status
#
# State lives in a reboot-safe dir (not /tmp, not a purgeable cache):
#   macOS  ~/Library/Application Support/ace2e/   (an older ~/Library/Caches/ace2e/ with a backup is still used)
#   elsewhere  ${XDG_CACHE_HOME:-~/.cache}/ace2e/
#     config.before.sha256  sha256 of config.toml before `add` (written first)
#     config.before.toml    copy of config.toml before `add`
#     config.block          the exact bytes `add` appends (written before they are appended)
#     config.post.sha256    sha256 of config.toml right after the append
#     config.path           which config.toml this belongs to
. "$(dirname "$0")/lib.sh" || exit 1

# Always the real ~/.codex: the scenarios start Codex without CODEX_HOME.
CODEX_DIR=$REAL_HOME/.codex
CFG=$CODEX_DIR/config.toml
if [ "$(uname)" = Darwin ]; then
  STATE="$REAL_HOME/Library/Application Support/ace2e"; LEGACY="$REAL_HOME/Library/Caches/ace2e"
  [ -e "$STATE/config.before.toml" ] || { [ -e "$LEGACY/config.before.toml" ] && STATE=$LEGACY; }
else
  STATE=${XDG_CACHE_HOME:-$REAL_HOME/.cache}/ace2e
fi
BAK=$STATE/config.before.toml; SUM=$STATE/config.before.sha256; BLOCK=$STATE/config.block
POST=$STATE/config.post.sha256; PATHF=$STATE/config.path
SCR_CODEX=$E2E_ROOT/codex-cfg   # scratch dir for `acd hooks install codex` (never called CODEX_HOME)
mkdir -p "$PROJ"
PROJ_REAL=$(cd "$PROJ" && pwd -P)
HEADER="[projects.\"$PROJ_REAL\"]"

sha() { shasum -a 256 "$1" | cut -d' ' -f1; }
sha1() { cut -d' ' -f1 "$1"; }
state_clear() { rm -f "$BAK" "$SUM" "$BLOCK" "$POST" "$PATHF" "$STATE/hooks.part"; rmdir "$STATE" 2>/dev/null || true; }
# codex_hooks.py must see the hook set that really runs: the real HOME, no CODEX_HOME.
hooks_py() { env -u CODEX_HOME HOME="$REAL_HOME" python3 "$E2E_DIR/codex_hooks.py" "$@"; }

# Only line numbers and TOML table headers: config.toml may hold tokens.
safe_diff() { # backup current
  local h='s/^[[:space:]]*(\[[^]]*\]+).*/\1/'
  echo "  changed line ranges (diff, line numbers only):" >&2
  diff "$1" "$2" | grep -E '^[0-9,]+[acd][0-9,]+$' | head -40 | sed 's/^/    /' >&2
  echo "  table headers (< backup only, > current only):" >&2
  diff <(grep -E '^[[:space:]]*\[' "$1" | sed -E "$h") <(grep -E '^[[:space:]]*\[' "$2" | sed -E "$h") \
    | grep -E '^[<>]' | head -40 | sed 's/^/    /' >&2
}

# replace_cfg SRC CFG: copy SRC to a temp file next to the real config.toml, then rename it over.
replace_cfg() {
  local src=$1 cfg=$2 real tmp
  real=$(python3 -c 'import os,sys;print(os.path.realpath(sys.argv[1]))' "$cfg") || return 1
  tmp=$real.ace2e-tmp.$$
  cp -p "$src" "$tmp" && mv -f "$tmp" "$real" || { rm -f "$tmp"; return 1; }
}

do_restore() {
  trap '' INT TERM HUP
  local cfg=$CFG cur
  if [ ! -e "$BAK" ]; then
    if [ -f "$CFG" ] && grep -qF "$HEADER" "$CFG"; then
      echo "RESTORE FAILED: no backup in $STATE, but $CFG still contains $HEADER" >&2
      echo "  Remove that table and the [hooks.state.\"$PROJ_REAL/.codex/hooks.json:...\"] tables by hand." >&2
      return 1
    fi
    echo "nothing to restore (no backup in $STATE)"; rm -f "$SUM" "$BLOCK" "$POST" "$PATHF" "$STATE/hooks.part"; rmdir "$STATE" 2>/dev/null || true
    return 0
  fi
  [ -f "$PATHF" ] && cfg=$(cat "$PATHF")
  [ -f "$cfg" ] || { echo "RESTORE: $cfg is missing; backup kept in $STATE" >&2; return 1; }
  cur=$(sha "$cfg")
  if [ -f "$SUM" ] && [ "$cur" = "$(sha1 "$SUM")" ]; then
    echo "config.toml already equals the original; nothing to undo"
  elif { [ -f "$POST" ] && [ "$cur" = "$(sha1 "$POST")" ]; } || python3 - "$cfg" "$BAK" "$BLOCK" <<'PY'
import sys
cfg, bak, blk = (open(p, "rb").read() for p in sys.argv[1:4])
# unchanged since add: current == backup + some prefix of the block add was appending
sys.exit(0 if cfg.startswith(bak) and blk.startswith(cfg[len(bak):]) else 1)
PY
  then
    { [ -f "$SUM" ] && replace_cfg "$BAK" "$cfg" && [ "$(sha "$cfg")" = "$(sha1 "$SUM")" ]; } \
      || { echo "RESTORE: copying the backup back failed or its sha256 differs; backup kept in $STATE" >&2; return 1; }
    echo "config.toml restored from the backup (unchanged since add; sha256 matches the original)"
  elif [ -s "$BLOCK" ] && python3 - "$cfg" "$BLOCK" <<'PY'
import os, sys
cfg, blk = sys.argv[1], sys.argv[2]
real = os.path.realpath(cfg)
data, b = open(real, "rb").read(), open(blk, "rb").read()
if not data.endswith(b):
    sys.exit(1)
tmp = "%s.ace2e-tmp.%d" % (real, os.getpid())
try:
    with open(tmp, "wb") as f:
        f.write(data[:len(data) - len(b)])
    os.chmod(tmp, os.stat(real).st_mode & 0o7777)
    os.replace(tmp, real)
except BaseException:
    if os.path.exists(tmp):
        os.unlink(tmp)
    raise
PY
  then
    echo "config.toml changed during the run: removed exactly the scratch block, kept the other edits"
    [ "$(sha "$cfg")" = "$(sha1 "$SUM")" ] || echo "  (it now differs from the original backup because of those edits)"
  else
    echo "RESTORE REFUSED: config.toml changed during the run and no longer ends with the scratch block." >&2
    echo "  config: $cfg" >&2
    echo "  backup: $BAK  (kept)" >&2
    echo "  Remove the [projects.\"$PROJ_REAL\"] and [hooks.state.\"...\"] entries for the scratch project by hand." >&2
    safe_diff "$BAK" "$cfg"
    return 1
  fi
  state_clear
}

case "${1:-status}" in
add)
  [ -e "$BAK" ] && { echo "already added (backup exists in $STATE); run restore first" >&2; exit 1; }
  if [ -n "${CODEX_HOME:-}" ] && [ "$CODEX_HOME" != "$REAL_HOME/.codex" ]; then
    echo "CODEX_HOME=$CODEX_HOME differs from ~/.codex; the scenarios start Codex without it, so the hook check would" >&2
    echo "not describe the hooks that run.  Unset it (or point it at ~/.codex) and retry.  Refusing." >&2; exit 1
  fi
  # Leftovers of an add that died before the backup was complete (the config itself was not touched yet).
  rm -f "$SUM" "$BLOCK" "$POST" "$PATHF" "$STATE/hooks.part"
  [ -f "$CFG" ] || { echo "$CFG does not exist; refusing" >&2; exit 1; }
  if grep -qF "$HEADER" "$CFG"; then
    echo "$CFG already contains $HEADER but there is no backup (a crashed run, or you added it yourself)." >&2
    echo "Refusing: restoring is impossible to do safely.  Remove that entry (and the [hooks.state.\"...\"] entries for" >&2
    echo "$PROJ_REAL/.codex/hooks.json) by hand, then run add again." >&2
    exit 1
  fi
  mkdir -p "$PROJ/.codex" "$SCR_CODEX" "$STATE"; chmod 700 "$STATE"
  CODEX_HOME=$SCR_CODEX acd hooks install codex >/dev/null || exit 1
  cp "$SCR_CODEX/hooks.json" "$PROJ/.codex/hooks.json"
  # Order matters for an interrupt: the sha and path first (a partial backup then still compares equal to
  # the untouched config), the backup next, the config is changed only after all of them exist.
  echo "$CFG" >"$PATHF"; shasum -a 256 "$CFG" >"$SUM"; : >"$BLOCK"; cp -p "$CFG" "$BAK"
  # Stage 1: trust the folder (needed before hooks/list reports the project hooks).
  printf '\n[projects."%s"]\ntrust_level = "trusted"\n' "$PROJ_REAL" >"$BLOCK"
  cat "$BLOCK" >>"$CFG"
  # Stage 2: trust the project hooks only.
  if ! hooks_py "$PROJ_REAL" --trust-project-hooks >"$STATE/hooks.part"; then
    echo "could not compute the project hook trust entries; rolling back" >&2
    rm -f "$STATE/hooks.part"
    do_restore; exit 1
  fi
  cat "$STATE/hooks.part" >>"$BLOCK"; cat "$STATE/hooks.part" >>"$CFG"; rm -f "$STATE/hooks.part"
  shasum -a 256 "$CFG" >"$POST"
  # Isolation precondition: nothing but the scratch project's hooks may be live.
  if ! hooks_py "$PROJ_REAL" --verify; then
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
