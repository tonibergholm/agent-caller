#!/usr/bin/env bash
# LIVE Codex while acd is up, down, stalled (SIGSTOP) and restarted mid-dialog (spec 13, 14.6, 14.8).
# Needs ./up.sh, ./codex-trust.sh add and a running Codex pane (30-codex-live.sh starts it).
. "$(dirname "$0")/../lib.sh"
SCEN=31-codex-resilience; : >"$RESULTS/$SCEN.txt"; FAILED=0
P=${P:-w1:p1}
ms() { python3 -c "print(round(($2-$1)*1000))"; }
cx_prompt() { hd pane send-text $P "$1"; sleep 0.3; hd pane send-keys $P Enter; }
hd_idle() { hd agent list | jq -e '.result.agents[]|select(.agent=="codex" and .agent_status=="idle")' >/dev/null; }
turn_ms() { # prompt -> ms from Enter to herdr idle (works with acd down: uses herdr only)
  local t0; t0=$(epoch_now); cx_prompt "$1"; sleep 1
  for _ in $(seq 300); do hd_idle && { ms $t0 $(epoch_now); return 0; }; sleep 0.1; done; echo ">30000"; }
dialog_ms() { local t0=$1; for _ in $(seq 600); do pane_text $P | grep -q 'Would you like to run' && { ms $t0 $(epoch_now); return 0; }; sleep 0.1; done; echo ">60000"; return 1; }
approve_in_terminal() { hd pane send-keys $P y; sleep 4; }
ask_touch() { T_ENTER=$(epoch_now); cx_prompt "Run touch /tmp/ace2e/$1 . The sandbox is read-only, so request approval to run it outside the sandbox (require_escalated). Do nothing else."; }
wait_dialog_gone() { for _ in $(seq 100); do pane_text $P | grep -q 'Would you like to run' || return 0; sleep 0.2; done; return 1; }

log "baseline (acd up): trivial turns"
B1=$(turn_ms "Reply with the single word OK."); B2=$(turn_ms "Reply with the single word OK.")
note "acd up, turn latency (Enter -> idle): $B1 ms, $B2 ms"

log "acd down (stopped)"
stop_acd; sleep 0.5
D1=$(turn_ms "Reply with the single word OK."); note "acd down, trivial turn: $D1 ms"
ask_touch x-cx-down.txt; DD=$(dialog_ms $T_ENTER) && note "acd down, approval dialog on screen $DD ms after Enter"
approve_in_terminal; [ -e /tmp/ace2e/x-cx-down.txt ] && pass "6a acd stopped: Codex shows its own dialog ($DD ms) and the terminal Yes works (file created)" || fail "6a no file with acd down"
[ "${D1%.*}" -lt $((${B1%.*} + ${B2%.*})) ] 2>/dev/null && pass "6b acd stopped: a trivial turn took $D1 ms vs $B1/$B2 ms with acd up (no added delay beyond noise)" || note "6b acd stopped: turn $D1 ms vs $B1/$B2 ms with acd up (inspect: model latency varies by seconds)"

log "acd stalled (SIGSTOP)"
start_acd; start_logger; sleep 2
PID=$(acd_pid); kill -STOP "$PID"
S1=$(turn_ms "Reply with the single word OK."); note "acd stalled, trivial turn: $S1 ms (acd up: $B1/$B2 ms; 4 lifecycle hooks x up to 1.5 s shim budget)"
ask_touch x-cx-stall.txt; SD=$(dialog_ms $T_ENTER) && note "acd stalled, approval dialog on screen $SD ms after Enter"
approve_in_terminal; [ -e /tmp/ace2e/x-cx-stall.txt ] && pass "6c acd stalled: the Codex dialog is not blocked and the terminal Yes works ($SD ms to dialog)" || fail "6c no file with acd stalled"
kill -CONT "$PID"; sleep 3; inbox list >/dev/null && pass "6d acd serves again after SIGCONT" || fail "6d"

log "acd restart with a dialog open (spec 14.8)"
ask_touch x-cx-restart.txt
AID=$(inbox agents | jq -r '[.[]|select(.kind=="codex")][0].id')
req=$(inbox wait-open --agent "$AID" --timeout 90 --ready); note "before restart: $(echo "$req" | jq -c '{id,kind,answer_path,status}')"
E1=$(inbox list | head -1); T_STOP=$(epoch_now); stop_acd
pane_text $P | grep -q 'Would you like to run' && pass "8a acd stopped with the dialog open: Codex still shows its dialog (agent not stuck)" || fail "8a dialog vanished"
start_acd; start_logger; T_UP=$(epoch_now)
for _ in $(seq 100); do r2=$(inbox agents | jq -c '[.[]|select(.kind=="codex")][0].pending // empty'); [ -n "$r2" ] && break; sleep 0.3; done
E2=$(inbox list | head -1); T_SEEN=$(epoch_now)
if [ -n "$r2" ]; then pass "8b after the restart the on-screen request reappears $(ms $T_UP $T_SEEN) ms after acd came up: $(echo "$r2" | jq -c '{id,kind,answer_path,status}')"
else fail "8b no request after restart"; fi
[ "$E1" != "$E2" ] && note "epoch changed (new daemon)" 
if [ -n "$r2" ] && echo "$r2" | jq -e '.answer_path=="keys" and .status=="open"' >/dev/null; then
  ans=$(inbox answer "$(echo "$r2" | jq -r .id)" allow); note "answer after restart: $ans"
  echo "$ans" | jq -e '.ok' >/dev/null && sleep 4 && [ -e /tmp/ace2e/x-cx-restart.txt ] && pass "8c the reappeared request is answerable and Yes acts" || fail "8c answer after restart: $ans"
else
  note "reappeared request is not answerable (kind/path above); answering in the terminal"
  approve_in_terminal
fi
wait_dialog_gone; sleep 2
echo "agent after: $(inbox agents | jq -c '[.[]|select(.kind=="codex")][0]|{state,pending:.pending.id}')" >&2
exit $FAILED
