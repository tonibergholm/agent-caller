#!/usr/bin/env bash
# LIVE Claude Code (haiku) in the scratch herdr session, under sandbox-exec so its http hooks
# cannot reach the user's own acd (see lib.sh claude_launch).  Hooks come from the scratch
# `acd hooks install claude` (--settings).  Only `touch /tmp/ace2e/x-*.txt` is approved.
. "$(dirname "$0")/../lib.sh"
SCEN=40-claude; : >"$RESULTS/$SCEN.txt"; FAILED=0
Q=${Q:-w2:p1}
ms() { python3 -c "print(round(($2-$1)*1000))"; }
cl_prompt() { hd pane send-text $Q "$1"; sleep 0.3; hd pane send-keys $Q Enter; }
cl_agent() { inbox agents | jq -r '[.[]|select(.kind=="claude" and (.id|startswith("herdr")))][0].id'; }
cl_up() { hd agent list | jq -e '.result.agents[]|select(.agent=="claude")' >/dev/null || { claude_launch $Q; wait_pane_text $Q 'for shortcuts' 40 || return 1; sleep 2; }; }
ask_touch() { T_ENTER=$(epoch_now); cl_prompt "Use the Bash tool to run exactly: touch /tmp/ace2e/$1 . Then reply done. Do nothing else."; }
dlg() { for _ in $(seq 600); do pane_text $Q | grep -q 'Do you want to proceed' && { epoch_now; return 0; }; sleep 0.1; done; return 1; }
hook_ts() { sed 's/\x1b\[[0-9;]*m//g' "$ACD_LOG" | grep -E "$1" | python3 -c '
import sys,re,datetime
after=float(sys.argv[1])
for l in sys.stdin:
    m=re.match(r"(\S+Z)",l)
    t=datetime.datetime.strptime(m.group(1),"%Y-%m-%dT%H:%M:%S.%fZ").replace(tzinfo=datetime.timezone.utc).timestamp()
    if t>=after: print(t); break' "$2"; }
wait_agent_idle() { for _ in $(seq 150); do pane_text $Q | grep -q 'esc to interrupt' || return 0; sleep 0.4; done; }
wait_resolved() { # after-epoch request-id [timeout s] -> json of the first request.resolved, "" on timeout
  local end=$(( $(date +%s) + ${3:-15} )) r
  while [ "$(date +%s)" -lt $end ]; do r=$(resolved_of "$1" "$2"); [ -n "$r" ] && { echo "$r"; return 0; }; sleep 0.2; done; return 1; }
resolved_of() { jl "select(.t>$1 and .msg.type==\"request.resolved\" and .msg.request_id==\"$2\") | {by:.msg.by,o:.msg.option_id,t}" | head -1; }

mkdir -p "$PROJ/.claude"
# Coexisting permission hook: a foreign command hook on PermissionRequest next to acd's http hook.
cat >"$PROJ/.claude/settings.local.json" <<J
{"hooks":{"PermissionRequest":[{"matcher":"Bash","hooks":[{"type":"command","command":"touch /tmp/ace2e/coexist-fired && cat >/dev/null","timeout":5}]}]}}
J
rm -f /tmp/ace2e/x-cl*.txt /tmp/ace2e/coexist-fired
if hd agent list | jq -e '.result.agents[]|select(.agent=="claude")' >/dev/null; then hd pane send-text $Q "/exit"; hd pane send-keys $Q Enter; sleep 3; fi
cl_up || { fail "setup: Claude did not start"; exit 1; }
AID=$(cl_agent); note "claude agent $AID"

log "1. Yes from the inbox"
ask_touch x-cl-yes.txt
req=$(inbox wait-open --agent "$AID" --timeout 60 --ready) || { fail "1 no request: $(pane_text $Q | tail -5)"; exit 1; }
RID=$(echo "$req" | jq -r .id); T_DLG=$(dlg); H=$(hook_ts 'name=PermissionRequest' "$T_ENTER")
OPENED=$(jl "select(.t>$T_ENTER and .msg.type==\"request.opened\" and .msg.request.id==\"$RID\") | .t" | head -1)
note "request: $(echo "$req" | jq -c '{id,kind,answer_path,status,options:[.options[].id]}')"
note "hook at acd ${H:+$(ms $T_ENTER $H)} ms after Enter; request.opened $(ms $H $OPENED) ms after the hook; dialog seen $(ms $T_ENTER $T_DLG) ms after Enter"
[ "$(ms $H $OPENED)" -lt 2000 ] && pass "1a Claude permission appears in the inbox $(ms $H $OPENED) ms after the hook (spec: panel within 2 s)" || fail "1a slow: $(ms $H $OPENED) ms"
ans=$(inbox answer "$RID" allow); note "answer: $ans"
echo "$ans" | jq -e '.ok and .outcome=="delivered"' >/dev/null && pass "1b Yes delivered ($(echo "$ans" | jq ._ms) ms)" || fail "1b $ans"
sleep 4; [ -e /tmp/ace2e/x-cl-yes.txt ] && pass "1c Yes unblocked the agent: file created, dialog closed" || fail "1c no file"
pane_text $Q | grep -q 'Do you want to proceed' && fail "1d dialog still open" || pass "1d the terminal dialog closed"
r=$(resolved_of $T_ENTER $RID); note "resolved: $r"; echo "$r" | jq -e '.by=="inbox"' >/dev/null && pass "1e request.resolved by=inbox" || fail "1e $r"
[ -e /tmp/ace2e/coexist-fired ] && pass "1f coexisting foreign PermissionRequest command hook fired alongside acd's http hook" || fail "1f foreign hook did not fire (project settings.local.json not loaded?)"
wait_agent_idle

log "2. No from the inbox"
ask_touch x-cl-no.txt
req=$(inbox wait-open --agent "$AID" --timeout 60 --ready); RID=$(echo "$req" | jq -r .id)
ans=$(inbox answer "$RID" deny); sleep 4
echo "$ans" | jq -e '.ok' >/dev/null && [ ! -e /tmp/ace2e/x-cl-no.txt ] && pass "2a No delivered, file not created" || fail "2a $ans"
note "screen: $(pane_text $Q | grep -iE 'denied|agent inbox' | head -2 | tr -s ' ' | tr '\n' '|')"
wait_agent_idle

log "3. Terminal first (hook request): Yes in the terminal while held"
ask_touch x-cl-term.txt
req=$(inbox wait-open --agent "$AID" --timeout 60 --ready); RID=$(echo "$req" | jq -r .id)
dlg >/dev/null; T_KEY=$(epoch_now); hd pane send-keys $Q 1
r=$(wait_resolved $T_KEY $RID 10); note "resolved: $r"
by=$(echo "$r" | jq -r .by 2>/dev/null); t=$(echo "$r" | jq -r .t 2>/dev/null)
[ "$by" = terminal ] && pass "3a terminal Yes resolved the card by=terminal $(ms $T_KEY $t) ms after the key (spec: within 2 s)" || fail "3a $r"
late=$(inbox answer "$RID" deny); echo "$late" | jq -e '.error.code=="request_gone"' >/dev/null && pass "3b late inbox answer -> request_gone" || fail "3b $late"
[ -e /tmp/ace2e/x-cl-term.txt ] && pass "3c only the terminal answer applied (file created)" || fail "3c"
wait_agent_idle

log "3d. Terminal Esc while held (3 runs)"
ESC_MS=""
for i in 1 2 3; do
  ask_touch x-cl-esc$i.txt
  req=$(inbox wait-open --agent "$AID" --timeout 60 --ready); RID=$(echo "$req" | jq -r .id)
  dlg >/dev/null; T_KEY=$(epoch_now); hd pane send-keys $Q esc
  r=$(wait_resolved $T_KEY $RID 60); by=$(echo "$r" | jq -r .by 2>/dev/null); t=$(echo "$r" | jq -r .t 2>/dev/null)
  if [ "$by" = terminal ]; then ESC_MS="$ESC_MS $(ms $T_KEY $t)"; else ESC_MS="$ESC_MS FAIL($r)"; fi
  wait_agent_idle; sleep 1
done
note "Esc key -> request.resolved(by=terminal), ms:$ESC_MS"
case "$ESC_MS" in *FAIL*) fail "3d Esc runs:$ESC_MS";; *) pass "3d Esc in the terminal resolved by=terminal in all 3 runs (ms:$ESC_MS; spec: within 2 s)";; esac

log "4. Simultaneous answers (hook request)"
ask_touch x-cl-race.txt
req=$(inbox wait-open --agent "$AID" --timeout 60 --ready); RID=$(echo "$req" | jq -r .id)
race=$(inbox race "$RID" allow deny); note "race: $(echo "$race" | jq -c '[.[]|{o:._option,ok,code:.error.code}]')"
[ "$(echo "$race" | jq '[.[]|select(.ok)]|length')" = 1 ] && pass "4a exactly one simultaneous answer delivered" || fail "4a"
sleep 4; win=$(echo "$race" | jq -r '.[]|select(.ok)|._option')
if [ "$win" = allow ]; then [ -e /tmp/ace2e/x-cl-race.txt ] && pass "4b winner allow -> file exists" || fail "4b"; else [ ! -e /tmp/ace2e/x-cl-race.txt ] && pass "4b winner deny -> no file" || fail "4b"; fi
wait_agent_idle

log "5. Single-select question from the inbox (keys path)"
cl_prompt "Use the AskUserQuestion tool to ask one single-select question 'Pick a color' with exactly two options Red and Blue. After I answer, reply with only the word I picked."
req=$(inbox wait-open --agent "$AID" --timeout 60 --ready); RID=$(echo "$req" | jq -r .id)
note "question: $(echo "$req" | jq -c '{id,kind,answer_path,options:[.options[]|{id,label}]}')"
OPT=$(echo "$req" | jq -r '[.options[]|select(.label=="Blue")][0].id // .options[1].id')
T_Q=$(epoch_now); ans=$(inbox answer "$RID" "$OPT"); note "first attempt, immediately after request.opened: $(echo "$ans" | jq -c '{ok,code:.error.code}')"
for _ in $(seq 40); do echo "$ans" | jq -e '.error.code=="request_not_ready"' >/dev/null || break; sleep 0.25; ans=$(inbox answer "$RID" "$OPT"); done
note "answer accepted $(ms $T_Q $(epoch_now)) ms after request.opened (dialog confirmation window): $(echo "$ans" | jq -c '{ok,outcome}')"; sleep 6
echo "$ans" | jq -e '.ok' >/dev/null && pane_text $Q | grep -q 'Blue' && pass "5a question answered from the inbox ($OPT=Blue reached Claude)" || fail "5a $ans"
wait_agent_idle

log "5b. Question answered in the terminal first (keys request): late inbox answer"
cl_prompt "Use the AskUserQuestion tool to ask one single-select question 'Pick a shape' with exactly two options Circle and Square. After I answer, reply with only the word I picked."
req=$(inbox wait-open --agent "$AID" --timeout 60 --ready); RID=$(echo "$req" | jq -r .id)
sleep 3; T_KEY=$(epoch_now); hd pane send-keys $Q 1
r=$(wait_resolved $T_KEY $RID 10); by=$(echo "$r" | jq -r .by 2>/dev/null); t=$(echo "$r" | jq -r .t 2>/dev/null)
[ "$by" = terminal ] && pass "5b terminal answer resolved the question by=terminal $(ms $T_KEY $t) ms after the key" || fail "5b $r"
late=$(inbox answer "$RID" "$(echo "$req" | jq -r '.options[1].id')"); echo "$late" | jq -e '.ok==false and (.error.code=="request_gone" or .error.code=="screen_mismatch")' >/dev/null && pass "5c late inbox answer on a keys request -> $(echo "$late" | jq -r .error.code)" || fail "5c $late"
wait_agent_idle
exit $FAILED
