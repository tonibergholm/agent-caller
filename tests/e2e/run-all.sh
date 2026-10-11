#!/usr/bin/env bash
# The body of `just e2e` (build is done by the just recipe): up, every scenario, down.
# Teardown (down.sh, which also restores ~/.codex/config.toml) runs on EXIT, INT and TERM, so Ctrl-C, a
# failing step or a crash in up.sh cannot leave processes or Codex trust entries behind.
# Exit status: non-zero when any scenario printed a FAIL line, a step failed, the run was interrupted,
# or teardown (including the Codex trust restore) failed.
set -uo pipefail
. "$(dirname "$0")/lib.sh" || exit 1

RUN_RC=0; CLEANED=0
RUNLOG=$RESULTS/00-run-all.txt
runlog_fail() { mkdir -p "$RESULTS"; echo "FAIL  $*" | tee -a "$RUNLOG" >&2; RUN_RC=1; }

cleanup() {
  local rc=$? down_rc=0 nfail
  [ "$CLEANED" = 1 ] && return; CLEANED=1
  trap '' INT TERM
  [ "$rc" -ne 0 ] && [ "$RUN_RC" = 0 ] && RUN_RC=$rc
  echo "== teardown" >&2
  "$E2E_DIR/down.sh" || down_rc=$?
  if [ "$down_rc" -ne 0 ]; then echo "FAIL  teardown / codex trust restore failed (status $down_rc)" | tee -a "$RUNLOG" >&2; RUN_RC=1; fi
  echo "== results" >&2
  grep -hE '^(PASS|FAIL)' "$RESULTS"/*.txt 2>/dev/null
  nfail=$(cat "$RESULTS"/*.txt 2>/dev/null | grep -c '^FAIL')
  [ "${nfail:-0}" -gt 0 ] && RUN_RC=1
  exit "$RUN_RC"
}
trap cleanup EXIT
trap 'RUN_RC=130; exit 130' INT
trap 'RUN_RC=143; exit 143' TERM

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
