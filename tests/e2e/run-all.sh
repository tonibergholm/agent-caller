#!/usr/bin/env bash
# The body of `just e2e` (build is done by the just recipe): up, every scenario, down.
# Teardown (down.sh, which also restores ~/.codex/config.toml) runs on EXIT, INT, TERM and HUP, so Ctrl-C, a
# failing step or a crash in up.sh cannot leave processes or Codex trust entries behind.
# Exit status: non-zero when any scenario printed a FAIL line, a step failed, the run was interrupted,
# or teardown (including the Codex trust restore) failed.
set -uo pipefail
. "$(dirname "$0")/lib.sh" || exit 1

# One run at a time.  The lock is taken BEFORE the trap exists, so a second run that finds it held exits
# without tearing down the first run's environment.  A lock whose recorded pid is dead is stale: reclaimed.
mkdir -p "$E2E_ROOT"; chmod 700 "$E2E_ROOT"
LOCK=$E2E_ROOT/run.lock
if ! mkdir "$LOCK" 2>/dev/null; then
  holder=$(cat "$LOCK/pid" 2>/dev/null || true)
  if [ -n "$holder" ] && kill -0 "$holder" 2>/dev/null; then
    echo "another e2e run holds $LOCK (pid $holder); exiting without teardown" >&2; exit 1
  fi
  rm -rf "$LOCK"
  mkdir "$LOCK" 2>/dev/null || { echo "could not take $LOCK; exiting without teardown" >&2; exit 1; }
fi
echo $$ >"$LOCK/pid"

RUN_RC=0; CLEANED=0
RUNLOG=$RESULTS/00-run-all.txt
runlog_fail() { mkdir -p "$RESULTS"; echo "FAIL  $*" | tee -a "$RUNLOG" >&2; RUN_RC=1; }

cleanup() {
  local rc=$? down_rc=0 nfail
  [ "$CLEANED" = 1 ] && return; CLEANED=1
  trap '' INT TERM HUP
  [ "$rc" -ne 0 ] && [ "$RUN_RC" = 0 ] && RUN_RC=$rc
  echo "== teardown" >&2
  "$E2E_DIR/down.sh" || down_rc=$?
  if [ "$down_rc" -ne 0 ]; then echo "FAIL  teardown / codex trust restore failed (status $down_rc)" | tee -a "$RUNLOG" >&2; RUN_RC=1; fi
  rm -rf "$LOCK"
  echo "== results" >&2
  grep -hE '^(PASS|FAIL)' "$RESULTS"/*.txt 2>/dev/null
  nfail=$(cat "$RESULTS"/*.txt 2>/dev/null | grep -c '^FAIL')
  [ "${nfail:-0}" -gt 0 ] && RUN_RC=1
  exit "$RUN_RC"
}
trap cleanup EXIT
trap 'RUN_RC=130; exit 130' INT
trap 'RUN_RC=143; exit 143' TERM
trap 'RUN_RC=129; exit 129' HUP

step() { # name script...   (a failing scenario does not stop the run)
  local name=$1; shift
  "$@"; local rc=$?
  [ "$rc" -eq 0 ] || runlog_fail "$name exited with status $rc"
  return 0
}

"$E2E_DIR/up.sh" || { runlog_fail "up.sh failed"; exit 1; }
step 10-hooks-roundtrip "$E2E_DIR/scenarios/10-hooks-roundtrip.sh"
step 20-synthetic-daemon "$E2E_DIR/scenarios/20-synthetic-daemon.sh"
if "$E2E_DIR/codex-trust.sh" add; then
  step 30-codex-live "$E2E_DIR/scenarios/30-codex-live.sh"
  step 31-codex-resilience "$E2E_DIR/scenarios/31-codex-resilience.sh"
else
  runlog_fail "codex-trust.sh add failed; Codex scenarios 30 and 31 skipped"
fi
step 40-claude-live "$E2E_DIR/scenarios/40-claude-live.sh"
step 41-claude-resilience "$E2E_DIR/scenarios/41-claude-resilience.sh"
step 50-t3 "$E2E_DIR/scenarios/50-t3.sh"
exit "$RUN_RC"
