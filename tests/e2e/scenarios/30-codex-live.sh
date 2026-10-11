#!/usr/bin/env bash
# LIVE Codex in the scratch herdr session (embedded mode, read-only sandbox, so `touch` needs approval).
# Needs ./up.sh and ./codex-trust.sh add first.  Only `touch /tmp/ace2e/x-*.txt` is ever approved.
. "$(dirname "$0")/../lib.sh"
SCEN=30-codex; : >"$RESULTS/$SCEN.txt"; FAILED=0
P=${P:-w1:p1}; TS=$(date +%s)
ms() { python3 -c "print(round(($2-$1)*1000))"; }
acd_ts() { # first acd.log line matching $1 after epoch $2 -> epoch seconds ("" if none)
  sed 's/\x1b\[[0-9;]*m//g' "$ACD_LOG" | grep -E "$1" | python3 -c '
import sys,re,datetime
after=float(sys.argv[1])
for l in sys.stdin:
    m=re.match(r"(\S+Z)",l)
    t=datetime.datetime.strptime(m.group(1),"%Y-%m-%dT%H:%M:%S.%fZ").replace(tzinfo=datetime.timezone.utc).timestamp()
    if t>=after: print(t); break' "$2"
}
codex_up() {
  pgrep -f 'codex.*--no-daemon' >/dev/null && hd agent list | jq -e '.result.agents[]|select(.agent=="codex")' >/dev/null && return 0
  hd pane run $P "clear; cd /private/tmp/ace2e/proj && HOME=$REAL_HOME codex --no-daemon -m gpt-5.6-terra -c model_reasoning_effort=low -a on-request -s read-only" >/dev/null
  wait_pane_text $P 'Hooks need review|Ask Codex' 30 || return 1
  if pane_text $P | grep -q 'Hooks need review'; then hd pane send-keys $P Down Down Enter; wait_pane_text $P 'Ask Codex' 20 || return 1; fi
  sleep 2
}
agent_id() { inbox agents | jq -r '[.[]|select(.kind=="codex")][0].id'; }
cx_prompt() { hd pane send-text $P "$1"; sleep 0.3; hd pane send-keys $P Enter; }
wait_idle() { # agent idle again
  for _ in $(seq 120); do [ "$(inbox agents | jq -r '[.[]|select(.kind=="codex")][0].state')" = idle ] && return 0; sleep 0.5; done; return 1; }
wait_clear() { # no pending request on the codex agent; echoes seconds waited
  local t0; t0=$(epoch_now)
  for _ in $(seq 240); do [ "$(inbox agents | jq -r '[.[]|select(.kind=="codex")][0].pending.id // "none"')" = none ] && { python3 -c "print(round($(epoch_now)-$t0,1))"; return 0; }; sleep 0.5; done; echo ">120"; return 1; }
ask_touch() { # file -> prompts Codex to request approval for it; sets T_ENTER
  T_ENTER=$(epoch_now)
  cx_prompt "Run touch /tmp/ace2e/$1 . The sandbox is read-only, so request approval to run it outside the sandbox (require_escalated). Do nothing else."
}
dialog_wait() { # polls the screen; echoes the epoch when 'Would you like to run' is visible
  for _ in $(seq 400); do pane_text $P | grep -q 'Would you like to run' && { epoch_now; return 0; }; sleep 0.1; done; return 1; }

codex_up || { fail "setup: Codex did not start"; exit 1; }
AID=$(agent_id); note "codex agent $AID"; rm -f /tmp/ace2e/x-cx*.txt

log "1. Yes from the inbox (spec 14.2)"
ask_touch x-cx-yes.txt
req=$(inbox wait-open --agent "$AID" --timeout 90 --ready); T_OPEN=$(epoch_now); RID=$(echo "$req" | jq -r .id)
T_DLG=$(dialog_wait); T_HOOK=$(acd_ts 'name=PermissionRequest' "$T_ENTER")
LOGGED=$(jl "select(.t>$T_ENTER and .msg.type==\"request.opened\" and .msg.request.id==\"$RID\") | .t" | head -1)
note "request $RID: $(echo "$req" | jq -c '{kind,status,answer_path,options:[.options[].id],detail}' | cut -c1-200)"
note "PermissionRequest hook at acd: $(ms $T_ENTER ${T_HOOK:-$T_ENTER}) ms after Enter; request.opened: $(ms $T_ENTER ${LOGGED:-$T_ENTER}) ms after Enter; dialog seen on screen (100 ms poll): $(ms $T_ENTER ${T_DLG:-$T_ENTER}) ms after Enter"
[ -n "$T_HOOK" ] && note "hook -> request.opened (answerable, answer_path keys, status open): $(ms $T_HOOK $LOGGED) ms"
echo "$req" | jq -e '.answer_path=="keys" and .status=="open"' >/dev/null && [ -n "$T_HOOK" ] && [ "$(ms $T_HOOK $LOGGED)" -lt 3000 ] \
  && pass "2a Codex approval answerable within 3 s of the hook ($(ms $T_HOOK $LOGGED) ms, keys path, status open)" || fail "2a request: $req"
ans=$(inbox answer "$RID" allow); note "answer: $ans"
echo "$ans" | jq -e '.ok and .outcome=="delivered"' >/dev/null && pass "2b Yes delivered ($(echo "$ans" | jq ._ms) ms)" || fail "2b $ans"
sleep 4; [ -e /tmp/ace2e/x-cx-yes.txt ] && pass "2c Yes acted: x-cx-yes.txt exists" || fail "2c file missing after Yes"
jl "select(.t>$T_ENTER and .msg.type==\"request.resolved\" and .msg.request_id==\"$RID\") | .msg|{by,option_id}" | head -1 | while read -r l; do note "request.resolved $l"; done
wait_idle

log "2. No from the inbox"
ask_touch x-cx-no.txt
req=$(inbox wait-open --agent "$AID" --timeout 90 --ready); RID=$(echo "$req" | jq -r .id)
ans=$(inbox answer "$RID" deny); note "answer: $ans"
echo "$ans" | jq -e '.ok and .outcome=="delivered"' >/dev/null && pass "2d No delivered" || fail "2d $ans"
sleep 5; [ ! -e /tmp/ace2e/x-cx-no.txt ] && pass "2e No acted: x-cx-no.txt does not exist" || fail "2e file exists after No"
note "screen after No: $(pane_text $P | grep -iE 'cancel|interrupt|declin|denied' | head -2 | tr -s ' ' | tr '\n' '|')"
W=$(wait_clear); note "after the delivered No the card stayed pending (answering) for ${W} s more"
jl "select(.t>$T_ENTER and .msg.type==\"request.resolved\" and .msg.request_id==\"$RID\") | .msg|{by}" | head -1 | while read -r l; do note "No: request.resolved $l"; done
wait_idle

log "3. Answered in the terminal first (spec 14.4, keys request)"
ask_touch x-cx-term.txt
req=$(inbox wait-open --agent "$AID" --timeout 90 --ready); RID=$(echo "$req" | jq -r .id)
T_KEY=$(epoch_now); hd pane send-keys $P y; sleep 3
res=$(jl "select(.t>$T_KEY and .msg.type==\"request.resolved\" and .msg.request_id==\"$RID\") | {by:.msg.by,t}" | head -1); note "resolved: $res"
by=$(echo "$res" | jq -r .by 2>/dev/null); rt=$(echo "$res" | jq -r .t 2>/dev/null)
[ "$by" = terminal ] && pass "3a terminal answer resolved the card by=terminal $(ms $T_KEY $rt) ms after the key (spec: within 2 s)" || fail "3a $res"
late=$(inbox answer "$RID" deny); note "late answer: $late"
echo "$late" | jq -e '.ok==false and (.error.code=="request_gone" or .error.code=="screen_mismatch")' >/dev/null && pass "3b late inbox answer refused: $(echo "$late" | jq -r .error.code)" || fail "3b $late"
[ -e /tmp/ace2e/x-cx-term.txt ] && pass "3c the terminal's Yes was the one applied (file exists, deny never sent)" || fail "3c file missing"
wait_idle

log "4. Simultaneous answers (keys request)"
ask_touch x-cx-race.txt
req=$(inbox wait-open --agent "$AID" --timeout 90 --ready); RID=$(echo "$req" | jq -r .id)
race=$(inbox race "$RID" allow deny); note "race: $(echo "$race" | jq -c '[.[]|{o:._option,ok,code:.error.code,outcome}]')"
ok=$(echo "$race" | jq '[.[]|select(.ok)]|length')
[ "$ok" = 1 ] && pass "4a exactly one of two simultaneous answers delivered" || fail "4a $ok delivered"
sleep 5; win=$(echo "$race" | jq -r '.[]|select(.ok)|._option')
if [ "$win" = allow ]; then [ -e /tmp/ace2e/x-cx-race.txt ] && pass "4b winner allow: file exists" || fail "4b allow won but no file"
else [ ! -e /tmp/ace2e/x-cx-race.txt ] && pass "4b winner deny: no file" || fail "4b deny won but file exists"; fi
W=$(wait_clear); note "after the race the card stayed pending for ${W} s more"; wait_idle

log "5. Follow-up: exactly once, plus a client retry with the same key (spec 14.5)"
MARK="FUP-$TS-$RANDOM"; CID=$(python3 -c 'import uuid;print(uuid.uuid4())'); KEY=$(python3 -c 'import uuid;print(uuid.uuid4())')
T5=$(epoch_now)
r1=$(inbox send "$AID" "Reply with exactly the word ACK-$MARK and nothing else." --client-id "$CID" --id "$KEY" --repeat 2); note "send x2 on one connection: $(echo "$r1" | jq -c '[.results[]|{ok,outcome,code:.error.code,ms:._ms}]')"
r2=$(inbox send "$AID" "Reply with exactly the word ACK-$MARK and nothing else." --client-id "$CID" --id "$KEY"); note "retry on a new connection, same client.id and key: $(echo "$r2" | jq -c '[.results[]|{ok,outcome,code:.error.code,ms:._ms}]')"
sleep 8
n=$(sed 's/\x1b\[[0-9;]*m//g' "$ACD_LOG" | grep -c "name=UserPromptSubmit.*ACK-$MARK")
[ "$n" = 1 ] && pass "5a the follow-up reached Codex exactly once (1 UserPromptSubmit hook with the marker; 3 sends with one key)" || fail "5a UserPromptSubmit hooks with the marker: $n"
shown=$(pane_text $P | grep -c "ACK-$MARK"); note "pane shows the marker on $shown lines (prompt echo + reply)"
echo "$r1 $r2" | jq -s -e 'all(.[].results[]; .ok)' >/dev/null && pass "5b every send/retry returned ok (cached result for the duplicates)" || fail "5b a send failed: $r1 $r2"
exit $FAILED
