#!/usr/bin/env bash
# Diagnosis of "Esc on a held Claude dialog resolves uncertain" (run doc, bug 1).  Runs Esc 3 times in two
# variants: (a) with a foreign PermissionRequest command hook in the project settings, (b) without it.
# Needs ./up.sh.  Best with a daemon built with the temporary `e2ediag` tracing (see the run doc).
. "$(dirname "$0")/../lib.sh"
SCEN=60-esc-diag; : >"$RESULTS/$SCEN.txt"; FAILED=0
Q=${Q:-w2:p1}
ms() { python3 -c "print(round(($2-$1)*1000))"; }
cl_prompt() { hd pane send-text $Q "$1"; sleep 0.3; hd pane send-keys $Q Enter; }
dlg() { for _ in $(seq 800); do pane_text $Q | grep -q 'Do you want to proceed' && { epoch_now; return 0; }; sleep 0.1; done; echo 0; return 1; }
idle() { for _ in $(seq 300); do pane_text $Q | grep -q 'esc to interrupt' || return 0; sleep 0.3; done; }
wait_resolved() { local end=$(( $(date +%s) + ${3:-60} )) r; while [ "$(date +%s)" -lt $end ]; do r=$(jl "select(.t>$1 and .msg.type==\"request.resolved\" and .msg.request_id==\"$2\") | {by:.msg.by,t}" | head -1); [ -n "$r" ] && { echo "$r"; return 0; }; sleep 0.2; done; return 1; }
mkdir -p "$PROJ/.claude"
variant() { # name foreign(0/1)
  if [ "$2" = 1 ]; then
    printf '%s' '{"hooks":{"PermissionRequest":[{"matcher":"Bash","hooks":[{"type":"command","command":"touch /tmp/ace2e/coexist-fired && cat >/dev/null","timeout":5}]}]}}' >"$PROJ/.claude/settings.local.json"
  else rm -f "$PROJ/.claude/settings.local.json"; fi
  hd agent list | jq -e '.result.agents[]|select(.agent=="claude")' >/dev/null && { hd pane send-text $Q "/exit"; hd pane send-keys $Q Enter; sleep 3; }
  claude_launch $Q; wait_pane_text $Q 'for shortcuts' 40; sleep 2
  AID=$(inbox agents | jq -r '[.[]|select(.kind=="claude" and (.id|startswith("herdr")))][0].id')
  for i in 1 2 3; do
    T0=$(epoch_now); cl_prompt "Use the Bash tool to run exactly: touch /tmp/ace2e/x-esc-$1-$i.txt . Then reply done. Do nothing else."
    req=$(inbox wait-open --agent "$AID" --timeout 60 --ready) || { note "$1 run $i: no request"; continue; }
    RID=$(echo "$req" | jq -r .id); dlg >/dev/null; sleep ${ESC_DELAY:-0}; TK=$(epoch_now); hd pane send-keys $Q esc
    r=$(wait_resolved $TK $RID 60); note "variant $1 (Esc delay ${ESC_DELAY:-0} s after the dialog is seen) run $i: $RID Esc -> $(echo "$r" | jq -c '{by}') after $(ms $TK $(echo "$r" | jq -r .t 2>/dev/null || echo $TK)) ms; epoch-ms window start $(python3 -c "print(int($TK*1000))")"
    idle; sleep 1
  done
}
variant with-foreign-hook 1
variant without-foreign-hook 0
rm -f "$PROJ/.claude/settings.local.json"
