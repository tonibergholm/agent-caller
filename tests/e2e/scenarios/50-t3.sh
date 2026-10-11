#!/usr/bin/env bash
# Protocol side of spec 14.3 against a SCRATCH t3 server (never the desktop T3 on 3773, never ~/.t3):
# a T3 approval and a T3 single-select question, answered through acd/1.  The T3 server (and so its
# Claude provider processes) runs under the sandbox-exec deny profile so they cannot reach the user's acd.
# Also 14.4 for t3 requests (late answer -> request_gone) and 14.5 for T3 follow-ups (exactly once).
. "$(dirname "$0")/../lib.sh" || exit 1
SCEN=50-t3; : >"$RESULTS/$SCEN.txt"; FAILED=0
. "$E2E_DIR/t3-up.sh" || exit 1

log "approval"
T0=$(epoch_now)
out=$(python3 "$E2E_DIR/t3drive.py" --port "$T3_PORT" --token-file "$T3TOK" launch "Run exactly this shell command with the Bash tool and nothing else: touch /tmp/ace2e/x-t3-yes.txt" --title "t3 approval") || { fail "launch: $out"; exit 1; }
note "thread: $out"
for _ in $(seq 120); do agent=$(inbox agents | jq -c '[.[]|select(.source=="t3" and .pending!=null)][0] // empty'); [ -n "$agent" ] && break; sleep 1; done
[ -n "$agent" ] || { fail "no T3 request appeared"; exit 1; }
AID=$(echo "$agent" | jq -r .id); req=$(echo "$agent" | jq -c .pending); RID=$(echo "$req" | jq -r .id)
OPENED=$(jl "select(.t>$T0 and .msg.type==\"request.opened\" and .msg.request.id==\"$RID\") | .t" | head -1)
note "request: $(echo "$req" | jq -c '{id,kind,answer_path,status,title,options:[.options[]|{id,effect}]}') (opened $(ms $T0 $OPENED) ms after launch; model latency included)"
echo "$req" | jq -e '.kind=="permission" and .answer_path=="t3"' >/dev/null && pass "3a T3 approval appears as a permission request on the t3 path" || fail "3a $req"
ans=$(inbox answer "$RID" accept); note "answer: $ans"
echo "$ans" | jq -e '.ok and .outcome=="delivered"' >/dev/null && pass "3b T3 approval accepted from the inbox ($(echo "$ans" | jq ._ms) ms)" || fail "3b $ans"
sleep 8; [ -e /tmp/ace2e/x-t3-yes.txt ] && pass "3c the approved command ran under T3" || fail "3c no file"
r=$(jl "select(.t>$T0 and .msg.type==\"request.resolved\" and .msg.request_id==\"$RID\")|.msg|{by,option_id}" | head -1); note "resolved: $r"
echo "$r" | jq -e '.by=="inbox"' >/dev/null && pass "3d request.resolved by=inbox" || fail "3d $r"
late=$(inbox answer "$RID" decline); echo "$late" | jq -e '.error.code=="request_gone"' >/dev/null && pass "4a late answer on a t3 request -> request_gone" || fail "4a $late"

log "single-select question"
T0=$(epoch_now)
out=$(python3 "$E2E_DIR/t3drive.py" --port "$T3_PORT" --token-file "$T3TOK" launch "Use the AskUserQuestion tool to ask one single-select question 'Pick a color' with exactly two options Red and Blue, then reply with only the word I picked." --title "t3 question") || { fail "launch: $out"; exit 1; }
for _ in $(seq 120); do agent=$(inbox agents | jq -c '[.[]|select(.source=="t3" and .pending!=null and .pending.kind=="question")][0] // empty'); [ -n "$agent" ] && break; sleep 1; done
[ -n "$agent" ] || { fail "no T3 question appeared"; exit 1; }
req=$(echo "$agent" | jq -c .pending); RID=$(echo "$req" | jq -r .id)
note "question: $(echo "$req" | jq -c '{id,kind,answer_path,options:[.options[]|{id,label}]}')"
OPT=$(echo "$req" | jq -r '[.options[]|select(.label=="Blue")][0].id // .options[1].id')
ans=$(inbox answer "$RID" "$OPT"); note "answer: $ans"
echo "$ans" | jq -e '.ok and .outcome=="delivered"' >/dev/null && pass "3e T3 single-select question answered from the inbox" || fail "3e $ans"
sleep 8; r=$(jl "select(.t>$T0 and .msg.type==\"request.resolved\" and .msg.request_id==\"$RID\")|.msg|{by,option_id}" | head -1); note "resolved: $r"
echo "$r" | jq -e '.by=="inbox"' >/dev/null && pass "3f question request.resolved by=inbox" || fail "3f $r"

log "follow-up to a T3 thread (14.5)"
AID=$(inbox agents | jq -r '[.[]|select(.source=="t3")][0].id')
for _ in $(seq 60); do st=$(inbox agents | jq -r --arg a "$AID" '.[]|select(.id==$a)|.state'); [ "$st" != working ] && [ "$st" != needs_you ] && break; sleep 1; done
MARK="T3FUP-$RANDOM"; CID=$(python3 -c 'import uuid;print(uuid.uuid4())'); KEY=$(python3 -c 'import uuid;print(uuid.uuid4())')
r1=$(inbox send "$AID" "Reply with exactly ACK-$MARK." --client-id "$CID" --id "$KEY" --repeat 2); r2=$(inbox send "$AID" "Reply with exactly ACK-$MARK." --client-id "$CID" --id "$KEY")
note "send results: $(echo "$r1 $r2" | jq -sc '[.[].results[]|{ok,outcome,code:.error.code}]')"
sleep 15
TID=$(echo "$AID" | sed 's/^t3://;s/.*://')
cnt=$(python3 "$E2E_DIR/t3drive.py" --port "$T3_PORT" --token-file "$T3TOK" count "$TID" "ACK-$MARK"); note "T3 thread $TID: $cnt"
echo "$cnt" | jq -e '.user_messages_with_needle==1' >/dev/null && pass "5c the follow-up is in the T3 thread exactly once (3 sends, one command key)" || fail "5c $cnt (agent id $AID)"
echo "$r1 $r2" | jq -s -e 'all(.[].results[]; .ok)' >/dev/null && pass "5d every send/retry returned ok" || fail "5d"
exit $FAILED
