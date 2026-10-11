#!/usr/bin/env bash
# LIVE Claude while acd is down, stalled, restarted mid-hold, and with a short hold (spec 13, 14.6, 14.8).
# Needs ./up.sh and a running Claude pane (40-claude-live.sh starts it).
. "$(dirname "$0")/../lib.sh" || exit 1
SCEN=41-claude-resilience; : >"$RESULTS/$SCEN.txt"; FAILED=0
Q=${Q:-w2:p1}
ms() { python3 -c "print(round(($2-$1)*1000))"; }
cl_prompt() { hd pane send-text $Q "$1"; sleep 0.3; hd pane send-keys $Q Enter; }
dlg() { for _ in $(seq 800); do pane_text $Q | grep -q 'Do you want to proceed' && { epoch_now; return 0; }; sleep 0.1; done; echo 0; return 1; }
idle() { for _ in $(seq 300); do pane_text $Q | grep -q 'esc to interrupt' || return 0; sleep 0.3; done; }
ask_touch() { T_ENTER=$(epoch_now); cl_prompt "Use the Bash tool to run exactly: touch /tmp/ace2e/$1 . Then reply done. Do nothing else."; }
turn_ms() { local t0; t0=$(epoch_now); cl_prompt "Reply with the single word OK."; sleep 1.5; idle; ms $t0 $(epoch_now); }
rm -f /tmp/ace2e/x-clr*.txt

log "baseline (acd up)"
B1=$(turn_ms); B2=$(turn_ms); ask_touch x-clr-base.txt; DB=$(dlg); DBMS=$(ms $T_ENTER $DB); note "acd up: trivial turns $B1 / $B2 ms; Enter -> dialog $DBMS ms"
hd pane send-keys $Q 1; sleep 3; idle

log "acd down"
stop_acd; sleep 0.5
D1=$(turn_ms); ask_touch x-clr-down.txt; DD=$(dlg); note "acd down: trivial turn $D1 ms; Enter -> dialog $(ms $T_ENTER $DD) ms"
[ "$DD" != 0 ] && pass "6a acd stopped: the Claude dialog appears as without agent-caller ($(ms $T_ENTER $DD) ms; acd up: $DBMS ms)" || fail "6a no dialog"
hd pane send-keys $Q 1; sleep 3; [ -e /tmp/ace2e/x-clr-down.txt ] && pass "6b acd stopped: terminal Yes works" || fail "6b no file"
note "hook errors shown: $(pane_text $Q | grep -c 'ECONNREFUSED.*52955') line(s) 'ECONNREFUSED 127.0.0.1:52955' on screen"; idle

log "acd stalled"
start_acd; start_logger; sleep 2; PID=$(acd_pid); kill -STOP "$PID"
S1=$(turn_ms); note "acd stalled: trivial turn $S1 ms (acd up $B1/$B2 ms, down $D1 ms)"
ask_touch x-clr-stall.txt; DS=$(dlg); note "acd stalled: Enter -> dialog $(ms $T_ENTER $DS) ms (acd up: $DBMS ms) while PermissionRequest is held open by a hung acd"
[ "$DS" != 0 ] && pass "6c acd stalled: the held PermissionRequest does not block the terminal dialog ($(ms $T_ENTER $DS) ms)" || fail "6c no dialog"
hd pane send-keys $Q 1; sleep 3; [ -e /tmp/ace2e/x-clr-stall.txt ] && pass "6d acd stalled: terminal Yes works while the hook hangs" || fail "6d no file"
note "screen: $(pane_text $Q | grep -iE 'hook' | head -3 | tr -s ' ' | tr '\n' '|' | cut -c1-200)"
kill -CONT "$PID"; sleep 3; idle

log "acd restart mid-hold (spec 14.8)"
ask_touch x-clr-restart.txt
AID=$(inbox agents | jq -r '[.[]|select(.kind=="claude" and (.id|startswith("herdr")))][0].id')
req=$(inbox wait-open --agent "$AID" --timeout 60 --ready); note "before: $(echo "$req" | jq -c '{id,kind,answer_path,status}')"
T0=$(epoch_now); stop_acd
pane_text $Q | grep -q 'Do you want to proceed' && pass "8a acd killed mid-hold: Claude still shows its dialog (nothing stuck)" || fail "8a dialog gone"
start_acd; start_logger; T1=$(epoch_now)
for _ in $(seq 100); do r2=$(inbox agents | jq -c '[.[]|select(.kind=="claude" and (.id|startswith("herdr")))][0].pending // empty'); [ -n "$r2" ] && break; sleep 0.3; done
if [ -n "$r2" ]; then pass "8b request reappears $(ms $T1 $(epoch_now)) ms after the restart: $(echo "$r2" | jq -c '{id,kind,answer_path,status}')"; else fail "8b no request after restart"; fi
if echo "$r2" | jq -e '.answer_path=="keys" and .status=="open"' >/dev/null 2>&1; then
  for _ in $(seq 30); do ans=$(inbox answer "$(echo "$r2" | jq -r .id)" allow); echo "$ans" | jq -e '.error.code=="request_not_ready"' >/dev/null || break; sleep 0.3; done
  note "answer after restart: $ans"; sleep 4
  [ -e /tmp/ace2e/x-clr-restart.txt ] && pass "8c the reappeared request is answerable from the inbox" || fail "8c no file"
else hd pane send-keys $Q 1; sleep 3; note "reappeared request not answerable; answered in the terminal"; fi
idle

log "hold expiry (permission_hold=5 s in the scratch config)"
stop_acd; sed -i '' 's/^permission_hold = .*/permission_hold = 5/' "$ACD_CONFIG_DIR/config.toml"; start_acd; start_logger; sleep 2
ask_touch x-clr-expiry.txt
req=$(inbox wait-open --agent "$AID" --timeout 60 --ready); RID=$(echo "$req" | jq -r .id); TO=$(epoch_now)
note "opened: $(echo "$req" | jq -c '{id,answer_path}') (hold 5 s)"
sleep 9
now=$(inbox agents | jq -c '[.[]|select(.kind=="claude" and (.id|startswith("herdr")))][0].pending|{id,kind,answer_path,status}')
note "9 s later the request is: $now"; pane_text $Q | grep -q 'Do you want to proceed' && pass "hold expiry: after the 5 s hold the terminal dialog is still up and unharmed" || fail "dialog gone after expiry"
jl "select(.t>$TO and .msg.type==\"request.resolved\")|.msg|{by,request_id}" | head -2 | while read -r l; do note "resolved events: $l"; done
hd pane send-keys $Q 1; sleep 3; idle
stop_acd; sed -i '' 's/^permission_hold = .*/permission_hold = 3600/' "$ACD_CONFIG_DIR/config.toml"; start_acd; start_logger
exit $FAILED
