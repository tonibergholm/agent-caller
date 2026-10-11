#!/usr/bin/env bash
# SYNTHETIC daemon-level checks.  A hand-made client plays Claude's http hook
# (hookpost.py) and Codex's command shim is run for real.  These are NOT live agent runs:
# they show the daemon's behavior at the hook boundary only.  Needs ./up.sh first.
. "$(dirname "$0")/../lib.sh" || exit 1
SCEN=20-synthetic; : >"$RESULTS/$SCEN.txt"; FAILED=0
CODEX_PAYLOAD='{"session_id":"cx-synth","cwd":"/private/tmp/ace2e/synth","hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"touch /tmp/ace2e/x-codex.txt"}}'
shim_ms() { # event -> "<ms> <stdout-bytes> <exit>"
  local t0 t1 out rc; t0=$(epoch_now)
  out=$(printf '%s' "$CODEX_PAYLOAD" | "$ACD_BIN" hook codex "$1" --config-dir "$ACD_CONFIG_DIR" acd-managed:v1 2>/dev/null); rc=$?
  t1=$(epoch_now); python3 -c "print(round(($t1-$t0)*1000), ${#out}, $rc)"
}
open_wait() { inbox wait-open --agent "$1" --timeout 10 --ready; }   # prints the request json

log "A. acd down (connection refused)"
stop_acd; sleep 0.5
r=$(hp UserPromptSubmit --session s-down --timeout 5); note "claude lifecycle hook: $r"
echo "$r" | jq -e '.error|test("ConnectionRefused")' >/dev/null && [ "$(echo "$r" | jq .ms)" -lt 100 ] \
  && pass "A1 acd down: Claude http hook is refused at once ($(echo "$r" | jq .ms) ms), so Claude shows its own dialog" \
  || fail "A1 acd down: $r"
for ev in PermissionRequest SessionStart; do
  set -- $(shim_ms $ev); note "codex shim $ev: ${1} ms, stdout ${2} bytes, exit ${3}"
  [ "$3" = 0 ] && [ "$2" = 0 ] && [ "$1" -lt 300 ] && pass "A2 acd down: codex shim $ev exits 0, prints nothing, in $1 ms" || fail "A2 codex shim $ev: $*"
done

log "B. acd stalled (SIGSTOP)"
start_acd; start_logger; sleep 1
P=$(acd_pid); kill -STOP "$P"
note "installed Claude timeouts (s): $(jq -c '[.hooks|to_entries[]|{(.key):.value[0].hooks[0].timeout}]|add' "$CLAUDE_CFG/settings.json")"
r=$(hp UserPromptSubmit --session s-stall --timeout 1); note "claude lifecycle hook, client timeout 1 s: $r"
echo "$r" | jq -e '.error=="client timeout"' >/dev/null && pass "B1 stalled: the hook gets no reply; Claude's own 1 s timeout (installed value) is the bound" || fail "B1 $r"
for ev in PermissionRequest SessionStart UserPromptSubmit; do
  set -- $(shim_ms $ev)
  lim=2000; [ $ev = PermissionRequest ] && lim=1000
  [ "$3" = 0 ] && [ "$2" = 0 ] && [ "$1" -le $lim ] && pass "B2 stalled: codex shim $ev gives up after $1 ms (exit 0, no output; Codex limit $([ $ev = PermissionRequest ] && echo 1 || echo 2) s)" || fail "B2 codex shim $ev: $*"
done
kill -CONT "$P"; sleep 1
inbox list >/dev/null && pass "B3 SIGCONT: acd serves acd/1 again" || fail "B3 acd did not recover"
note "NOT covered here: 'a held PermissionRequest never blocks the terminal dialog' needs a live Claude."

log "C. held PermissionRequest answered from the inbox, then a late answer"
hp PermissionRequest --session s-c --cmd "touch /tmp/ace2e/x-c.txt" --timeout 30 >"$E2E_ROOT/c.hold" &
H=$!; T0=$(epoch_now)
req=$(open_wait claude:s-c || open_wait "$(inbox list | awk '/^claude:/{print $1; exit}')") ; RID=$(echo "$req" | jq -r .id)
note "request $RID: $(echo "$req" | jq -c '{kind,status,answer_path,options:[.options[].id]}')"
opened=$(jl "select(.t>$T0 and .msg.type==\"request.opened\" and .msg.request.id==\"$RID\") | .t" | head -1)
[ -n "$opened" ] && note "hook posted $T0, request.opened logged $opened: $(python3 -c "print(round(($opened-$T0)*1000))") ms"
ans=$(inbox answer "$RID" allow); note "answer: $ans"
echo "$ans" | jq -e '.ok and .outcome=="delivered"' >/dev/null && pass "C1 inbox answer allow -> delivered ($(echo "$ans" | jq ._ms) ms)" || fail "C1 $ans"
wait $H; body=$(cat "$E2E_ROOT/c.hold"); note "held hook reply: $body"
echo "$body" | jq -e '.body|fromjson|.hookSpecificOutput.decision.behavior=="allow"' >/dev/null && pass "C2 the held hook connection received decision=allow" || fail "C2 $body"
late=$(inbox answer "$RID" deny); note "late answer before the agent confirmed: $late"
echo "$late" | jq -e '.ok==false and (.error.code=="request_busy" or .error.code=="request_gone")' >/dev/null && pass "C3 a late inbox answer is refused ($(echo "$late" | jq -r .error.code)); it never reaches the agent" || fail "C3 $late"
hp PostToolUse --session s-c --cmd "touch /tmp/ace2e/x-c.txt" >/dev/null; sleep 0.5
late=$(inbox answer "$RID" deny); echo "$late" | jq -e '.error.code=="request_gone"' >/dev/null && pass "C4 after the agent's PostToolUse, a late inbox answer gets request_gone" || fail "C4 $late"
jl "select(.t>$T0 and .msg.type==\"request.resolved\" and .msg.request_id==\"$RID\") | .msg | {by,option_id}" | head -2 | while read -r l; do note "request.resolved $l"; done

log "D. terminal answered first (PostToolUse arrives while held)"
hp PermissionRequest --session s-d --cmd "touch /tmp/ace2e/x-d.txt" --timeout 30 >"$E2E_ROOT/d.hold" &
H=$!; req=$(inbox wait-open --timeout 10 --ready --agent "$(inbox list | awk '/^claude:s-d/{print $1}' | head -1)")
RID=$(echo "$req" | jq -r .id); T1=$(epoch_now)
hp PostToolUse --session s-d --cmd "touch /tmp/ace2e/x-d.txt" >/dev/null; sleep 1
res=$(jl "select(.t>$T1 and .msg.type==\"request.resolved\" and .msg.request_id==\"$RID\") | {by:.msg.by,t}" | head -1); note "resolved: $res"
by=$(echo "$res" | jq -r .by 2>/dev/null); rt=$(echo "$res" | jq -r .t 2>/dev/null)
[ "$by" = terminal ] && pass "D1 request resolved by=terminal $(python3 -c "print(round(($rt-$T1)*1000))") ms after the PostToolUse was posted" || fail "D1 $res"
wait $H; note "held reply after terminal answer: $(cat "$E2E_ROOT/d.hold")"
jq -e '.error==null and .body==""' "$E2E_ROOT/d.hold" >/dev/null && pass "D2 the held connection was released with an empty reply (the dialog stays the terminal's)" || fail "D2"
late=$(inbox answer "$RID" allow); echo "$late" | jq -e '.error.code=="request_gone"' >/dev/null && pass "D3 late inbox answer -> request_gone" || fail "D3 $late"

log "E. simultaneous answers"
hp PermissionRequest --session s-e --cmd "touch /tmp/ace2e/x-e.txt" --timeout 30 >"$E2E_ROOT/e.hold" &
H=$!; req=$(inbox wait-open --timeout 10 --ready --agent "$(inbox list | awk '/^claude:s-e/{print $1}' | head -1)")
RID=$(echo "$req" | jq -r .id)
race=$(inbox race "$RID" allow deny); note "race: $race"
ok=$(echo "$race" | jq '[.[]|select(.ok)]|length'); gone=$(echo "$race" | jq '[.[]|select(.ok==false and (.error.code=="request_gone" or .error.code=="request_busy"))]|length')
[ "$ok" = 1 ] && [ "$gone" = 1 ] && pass "E1 two simultaneous answers: exactly one delivered, the other $(echo "$race" | jq -r '[.[]|select(.ok==false)|.error.code][0]')" || fail "E1 ok=$ok gone=$gone"
wait $H; w=$(jq -r '.body|fromjson|.hookSpecificOutput.decision.behavior' "$E2E_ROOT/e.hold"); win=$(echo "$race" | jq -r '.[]|select(.ok)|._option')
[ "$w" = "$win" ] && pass "E2 the agent got the winner's decision ($w)" || fail "E2 agent got $w, winner $win"

log "F. hold expiry (permission_hold shortened to 4 s in the scratch config)"
stop_acd; sed -i '' 's/^permission_hold = .*/permission_hold = 4/' "$ACD_CONFIG_DIR/config.toml"; start_acd; start_logger; sleep 1
T0=$(epoch_now); r=$(hp PermissionRequest --session s-f --cmd "touch /tmp/ace2e/x-f.txt" --timeout 30); note "held hook with hold=4: $r"
ms=$(echo "$r" | jq .ms)
[ "$ms" -ge 3800 ] && [ "$ms" -le 5500 ] && echo "$r" | jq -e '.body==""' >/dev/null && pass "F1 hold expiry: connection released empty after $ms ms (config permission_hold=4)" || fail "F1 $r"
sleep 1; note "agent after expiry: $(inbox list | grep -A1 '^claude:s-f' | tr '\n' ' ')"
note "Default is 3600 s; shorten with [claude] permission_hold in config.toml (E2E_CLAUDE_HOLD=<s> ./up.sh)."
stop_acd; sed -i '' 's/^permission_hold = .*/permission_hold = 3600/' "$ACD_CONFIG_DIR/config.toml"; start_acd; start_logger; sleep 1

log "G. acd restart mid-hold"
hp PermissionRequest --session s-g --cmd "touch /tmp/ace2e/x-g.txt" --timeout 60 >"$E2E_ROOT/g.hold" &
H=$!; inbox wait-open --timeout 10 --ready --agent "$(inbox list | awk '/^claude:s-g/{print $1}' | head -1)" >/dev/null
E1=$(inbox list | head -1); T0=$(epoch_now); stop_acd
wait $H; T1=$(epoch_now); note "held hook after SIGTERM: $(cat "$E2E_ROOT/g.hold")"
jq -e '.error != "client timeout"' "$E2E_ROOT/g.hold" >/dev/null && pass "G1 restart: the held connection ended $(python3 -c "print(round(($T1-$T0)*1000))") ms after SIGTERM (not stuck), so Claude falls back to its dialog" || fail "G1"
start_acd; start_logger; sleep 1; E2=$(inbox list | head -1)
[ "$E1" != "$E2" ] && pass "G2 restart: new epoch ($E1 -> $E2); a client resyncs by snapshot" || fail "G2 same epoch"
note "NOT covered here: 'requests still on screen reappear' needs a live pane (herdr screen read)."
exit $FAILED
