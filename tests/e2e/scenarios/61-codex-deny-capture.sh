#!/usr/bin/env bash
# Captures the Codex pane after a No (inbox), and after a terminal Esc / `n`, for designing a cancellation
# signature (run doc, bug 2).  Saves fixture candidates under $E2E_ROOT/fixtures (copy the good ones into tests/fixtures/herdr by hand).
# Needs ./up.sh and ./codex-trust.sh add.
. "$(dirname "$0")/../lib.sh" || exit 1
SCEN=61-codex-deny-capture; : >"$RESULTS/$SCEN.txt"; P=${P:-w1:p1}
OUT=$E2E_ROOT/fixtures; mkdir -p "$OUT"   # scratch, not the repo: copy what is worth keeping into tests/fixtures/herdr by hand
cx_prompt() { hd pane send-text $P "$1"; sleep 0.3; hd pane send-keys $P Enter; }
codex_up() {
  hd agent list | jq -e '.result.agents[]|select(.agent=="codex")' >/dev/null && return 0
  hd pane run $P "clear; cd /private/tmp/ace2e/proj && HOME=$REAL_HOME ACD_CONFIG_DIR=$E2E_ROOT/acd codex --no-daemon -m gpt-5.6-terra -c model_reasoning_effort=low -a on-request -s read-only" >/dev/null
  wait_pane_text $P 'Hooks need review|Ask Codex' 30 || return 1; sleep 3
  if pane_text $P | grep -q 'Hooks need review'; then hd pane send-keys $P esc; wait_pane_text $P 'Ask Codex' 20 || return 1; fi
  sleep 2
}
wait_clear() { for _ in $(seq 240); do [ "$(inbox agents | jq -r '[.[]|select(.kind=="codex")][0].pending.id // "none"')" = none ] && return 0; sleep 0.5; done; }
snap() { # name  (visible + detection)
  pane_text $P >"$OUT/$1-visible.txt"; pane_det $P >"$OUT/$1-detection.txt"; }
codex_up || { echo "codex did not start"; exit 1; }
AID=$(inbox agents | jq -r '[.[]|select(.kind=="codex")][0].id')
ask() { cx_prompt "Run touch /tmp/ace2e/$1 . The sandbox is read-only, so request approval to run it outside the sandbox (require_escalated). Do nothing else."; inbox wait-open --agent "$AID" --timeout 90 --ready; }
cycle() { # label  action(inbox|esc|n)
  local label=$1 action=$2 req RID
  req=$(ask x-fx-$label.txt) || { echo "$label: no request"; return; }; RID=$(echo "$req" | jq -r .id)
  snap codex-dialog-$label   # the dialog before acting
  T0=$(epoch_now)
  case $action in
    inbox) inbox answer "$RID" deny >/dev/null ;;
    esc) hd pane send-keys $P esc ;;
    n) hd pane send-keys $P n ;;
  esac
  for t in 0.5 1 2; do
    python3 -c "import time;time.sleep(max(0,$t-(time.time()-$T0)))"
    snap codex-after-$action-$label-${t}s
  done
  echo "$label ($action): $(ls $OUT | grep -c "codex-after-$action-$label") files; request $RID" | tee -a "$RESULTS/$SCEN.txt"
  # a lingering dialog (a key that is not a shortcut) is closed with Esc so the next cycle starts clean
  pane_text $P | grep -q 'Would you like to run' && hd pane send-keys $P esc
  wait_clear; sleep 2
}
for i in 1 2 3; do cycle run$i inbox; done
cycle run1 esc
cycle run1 n
