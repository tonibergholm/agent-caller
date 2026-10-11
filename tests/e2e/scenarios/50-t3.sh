#!/usr/bin/env bash
# Protocol side of spec 14.3 against a SCRATCH t3 server (never the desktop T3 on 3773, never ~/.t3):
# a T3 approval and a T3 single-select question, answered through acd/1.  The T3 server (and so its
# Claude provider processes) runs under the sandbox-exec deny profile so they cannot reach the user's acd.
# Also 14.4 for t3 requests (late answer -> request_gone) and 14.5 for T3 follow-ups (exactly once).
. "$(dirname "$0")/../lib.sh"
SCEN=50-t3; : >"$RESULTS/$SCEN.txt"; FAILED=0
T3H=$E2E_ROOT/t3home; T3P=$E2E_ROOT/t3proj; T3TOK=$E2E_ROOT/t3.token
ms() { python3 -c "print(round(($2-$1)*1000))"; }
t3() { clean_env T3CODE_HOME="$T3H" "$@"; }
mkdir -p "$T3H" "$T3P"; [ -d "$T3P/.git" ] || { git -C "$T3P" init -q; echo hi >"$T3P/f.txt"; git -C "$T3P" add f.txt; git -C "$T3P" -c user.email=e@e -c user.name=e commit -qm init; }
write_deny_profile
# t3 serve itself cannot run under sandbox-exec (it spawns helper binaries: "spawn EPERM"), so the T3 server runs
# normally and only its Claude provider is pointed, through settings.json providerInstances.claudeAgent.binaryPath,
# at a wrapper that runs the real claude under the deny profile.  Codex in T3 stays off.
CLAUDE_REAL=$(command -v claude)
cat >"$E2E_ROOT/claude-sandboxed.sh" <<W
#!/bin/sh
exec /usr/bin/sandbox-exec -f "$DENY_SB" "$CLAUDE_REAL" "\$@"
W
chmod 755 "$E2E_ROOT/claude-sandboxed.sh"
mkdir -p "$T3H/userdata"
cat >"$T3H/userdata/settings.json" <<J
{"providerInstances":{"claudeAgent":{"driver":"claudeAgent","config":{"binaryPath":"$E2E_ROOT/claude-sandboxed.sh"}},"codex":{"driver":"codex","enabled":false,"config":{"enabled":false}}}}
J
chmod 600 "$T3H/userdata/settings.json"
if ! [ -f "$E2E_ROOT/t3.pid" ]; then
  ( cd "$E2E_ROOT"; clean_env T3CODE_HOME="$T3H" t3 serve --base-dir "$T3H" --port "$T3_PORT" --host 127.0.0.1 --no-browser "$T3P" >"$E2E_ROOT/t3.log" 2>&1 </dev/null & echo $! >"$E2E_ROOT/t3.pid" )
fi
for _ in $(seq 60); do curl -s -o /dev/null --max-time 1 "http://127.0.0.1:$T3_PORT/" && break; sleep 1; done
curl -s -o /dev/null -w "%{http_code}" --max-time 2 "http://127.0.0.1:$T3_PORT/" | grep -q 200 || { fail "scratch t3 did not come up: $(tail -3 "$E2E_ROOT/t3.log")"; exit 1; }
note "scratch T3 on 127.0.0.1:$T3_PORT, T3CODE_HOME=$T3H (desktop T3 untouched)"
( umask 077; t3 t3 auth session issue --scope orchestration:read --scope orchestration:operate --ttl 2h --token-only >"$T3TOK" 2>/dev/null ) || { fail "token issue failed"; exit 1; }
chmod 600 "$T3TOK"; [ -s "$T3TOK" ] || { fail "empty t3 token"; exit 1; }
# project registered for the scratch server
t3 t3 project add "$T3P" >/dev/null 2>&1 || true

# Point the scratch acd at the scratch T3 (token file in the scratch config dir).
stop_acd; cp "$T3TOK" "$ACD_CONFIG_DIR/t3.token"; chmod 600 "$ACD_CONFIG_DIR/t3.token"
python3 - <<PY
import re,os
p=os.path.join(os.environ["ACD_CONFIG_DIR"],"config.toml"); s=open(p).read()
s=re.sub(r"\[t3\][^\[]*","[t3]\nenabled = true\nurl = \"http://127.0.0.1:$T3_PORT\"\n\n",s,count=1)
open(p,"w").write(s)
PY
start_acd; start_logger; sleep 3
acd status | sed -n 2p | tee -a "$RESULTS/$SCEN.txt"

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
