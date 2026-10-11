#!/usr/bin/env bash
# Tears everything down: Codex panes and the scratch herdr server first, then the logger, acd and
# scratch T3, then any Codex process left in the scratch project, then the Codex trust restore, then the scratch tokens.  Safe to run twice.
# Exit status is non-zero when the Codex trust restore fails (config.toml is then left alone and
# the backup is kept); the other steps are best effort and followed by a leftover report.
. "$(dirname "$0")/lib.sh" || exit 1

# 1. Agents first.  Codex must be gone before config.toml is restored, or it could rewrite the file
#    (or run with entries we are about to remove).  Close the panes, stop the scratch herdr server.
if [ -S "$HERDR_SOCK" ]; then
  for p in $(hd pane list 2>/dev/null | grep -Eo '"pane_id": *"[^"]+"' | sed 's/.*: *"//;s/"//'); do hd pane close "$p" >/dev/null 2>&1; done
  hd server stop >/dev/null 2>&1
  for _ in $(seq 30); do [ -S "$HERDR_SOCK" ] || break; sleep 0.2; done
fi
# PID fallback, only for a process that really is our scratch server.
kill_pidfile "$E2E_ROOT/herdr.pid" "$HERDR_RE"

# 2. Other scratch processes.
stop_logger
stop_acd
kill_pidfile "$E2E_ROOT/t3tap.pid" "t3tap\\.py --listen"
kill_pidfile "$E2E_ROOT/t3.pid" "$T3_RE" group
sleep 1

# 3. Codex processes whose cwd is the scratch project (a pane that survived, a Codex started by hand).
#    They could rewrite config.toml, so TERM them and wait; while any remain, do not restore.
codex_scratch_pids() { # prints pids of codex processes with cwd under /private/tmp/ace2e (or /tmp/ace2e)
  command -v lsof >/dev/null 2>&1 || { echo "down: lsof missing, cannot check for scratch Codex processes" >&2; echo UNKNOWN; return; }
  lsof -a -d cwd -c codex -Fpn 2>/dev/null | awk '/^p/{p=substr($0,2)} /^n/{n=substr($0,2); if (n ~ /^\/(private\/)?tmp\/ace2e(\/|$)/) print p}' | sort -u
}
CODEX_LEFT=$(codex_scratch_pids)
if [ -n "$CODEX_LEFT" ] && [ "$CODEX_LEFT" != UNKNOWN ]; then
  echo "down: terminating Codex processes in the scratch project: $(echo $CODEX_LEFT)" >&2
  for p in $CODEX_LEFT; do kill -TERM "$p" 2>/dev/null; done
  for _ in $(seq 50); do CODEX_LEFT=$(codex_scratch_pids); [ -z "$CODEX_LEFT" ] && break; sleep 0.2; done
fi

# 4. Restore ~/.codex/config.toml (fatal on failure).
RC=0
if [ -n "$CODEX_LEFT" ]; then
  RC=1
  echo "!!!!!! down.sh: NOT restoring ~/.codex/config.toml: Codex processes may still be running in the scratch project ($(echo $CODEX_LEFT))." >&2
  echo "!!!!!! Stop them, then run: $E2E_DIR/codex-trust.sh restore" >&2
elif ! "$E2E_DIR/codex-trust.sh" restore; then
  RC=1
  echo "!!!!!! down.sh: CODEX TRUST RESTORE FAILED.  ~/.codex/config.toml may still hold the scratch trust entries." >&2
  echo "!!!!!! The backup is kept.  Run: $E2E_DIR/codex-trust.sh status   (see tests/e2e/README.md, recovery)" >&2
fi

# 5. Scratch tokens (acd's own, T3's) go away with the config dir and T3 home.
rm -rf "$SCRATCH_HOME" "$ACD_CONFIG_DIR" "$E2E_ROOT/t3home" "$CLAUDE_CFG" "$E2E_ROOT/claude-settings.json" "$E2E_ROOT/t3.token" "$E2E_ROOT/codex-cfg" "$E2E_ROOT/rt" "$E2E_ROOT/t3.pid" "$E2E_ROOT/herdr.pid" "$E2E_ROOT/logger.pid"

# 6. Leftover report: anything still running that belongs to the scratch environment.
LEFT=0
procs=$(pgrep -fl "ace2e|$E2E_ROOT" 2>/dev/null | grep -v "pgrep" | grep -Ev "^$$ " || true)
if [ -n "$procs" ]; then LEFT=1; echo "down: LEFTOVER processes mentioning ace2e:" >&2; echo "$procs" | cut -c1-200 >&2; fi
for port in $CLIENT_PORT $HOOK_PORT $T3_PORT; do
  l=$(lsof -nP -iTCP:"$port" -sTCP:LISTEN 2>/dev/null | tail -n +2 || true)
  if [ -n "$l" ]; then LEFT=1; echo "down: LEFTOVER listener on scratch port $port:" >&2; echo "$l" | cut -c1-160 >&2; fi
done
[ "$LEFT" = 0 ] && echo "down: no leftover processes or listeners"
echo "down: acd/t3/herdr stopped, scratch tokens removed (restore rc=$RC)"
exit $RC
