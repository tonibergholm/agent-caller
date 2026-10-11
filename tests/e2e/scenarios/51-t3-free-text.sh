#!/usr/bin/env bash
# Free-text answers on the T3 path (spec 6.5 "Typed answers") against a SCRATCH t3 server whose Claude provider
# runs under the sandbox-exec deny profile (see t3-up.sh).  A recording tap (t3tap.py) sits between the scratch acd
# and the scratch T3 and keeps only the runtime-request.respond command acd sends and T3's Exit for it.
#   a) a question that needs free text (Claude must still offer options) -> answer-text -> echoed back verbatim
#   b) a Red/Blue question answered with free text -> T3 records it (answers) and Claude acts on it
#   c) exploratory: can Claude produce a question with no options? (reported, never a FAIL)
# Candidate fixtures are written to /tmp/ace2e/fixtures/t3 (copy by hand into tests/fixtures/t3).
. "$(dirname "$0")/../lib.sh" || exit 1
SCEN=51-t3-free-text; : >"$RESULTS/$SCEN.txt"; FAILED=0
TAP_PORT=${E2E_T3TAP_PORT:-52958}; TAP_LOG=$E2E_ROOT/t3tap.jsonl; FIX=$E2E_ROOT/fixtures/t3
mkdir -p "$FIX"; : >"$TAP_LOG"; chmod 600 "$TAP_LOG"
python3 "$E2E_DIR/t3tap.py" --listen "$TAP_PORT" --target "$T3_PORT" --out "$TAP_LOG" >"$E2E_ROOT/t3tap.log" 2>&1 </dev/null &
echo $! >"$E2E_ROOT/t3tap.pid"
trap 'kill "$(cat "$E2E_ROOT/t3tap.pid" 2>/dev/null)" 2>/dev/null; rm -f "$E2E_ROOT/t3tap.pid"' EXIT
sleep 1
export T3_ACD_PORT=$TAP_PORT
. "$E2E_DIR/t3-up.sh" || exit 1
t3d() { python3 "$E2E_DIR/t3drive.py" --port "$T3_PORT" --token-file "$T3TOK" "$@"; }
TID_RE=''
# wait_q <thread id> -> prints the agent JSON once it has a pending question
wait_q() { local a; for _ in $(seq "${2:-120}"); do a=$(inbox agents | jq -c --arg t "$1" '[.[]|select(.source=="t3" and (.id|endswith($t)) and .pending!=null and .pending.kind=="question")][0] // empty'); [ -n "$a" ] && { echo "$a"; return 0; }; sleep 1; done; return 1; }
# wait_reply <thread id> <needle> -> 0 when an assistant message contains it; leaves the dump in $2.json
wait_reply() { for _ in $(seq 60); do t3d dump "$1" --out "$E2E_ROOT/thread-$1.json" >"$E2E_ROOT/thread-$1.msgs" 2>/dev/null; jq -e --arg n "$2" 'any(.messages[]; .role=="assistant" and ((.text//"")|contains($n)))' "$E2E_ROOT/thread-$1.msgs" >/dev/null 2>&1 && return 0; sleep 2; done; return 1; }
# tap_last -> the last respond command and its Exit, as two JSON lines
tap_cmd()  { jq -c 'select(.dir=="acd->t3")' "$TAP_LOG" | tail -1; }
tap_exit() { local id; id=$(tap_cmd | jq -r .msg.id); jq -c --arg i "$id" 'select(.dir=="t3->acd" and .msg.requestId==$i)' "$TAP_LOG" | tail -1; }

run_case() { # tag prompt answer-text echo-needle
  local tag=$1 prompt=$2 text=$3 needle=$4 T0 out tid agent req rid qid ans cmd ex r
  log "case $tag"; T0=$(epoch_now)
  out=$(t3d launch "$prompt" --title "t3 free text $tag") || { fail "$tag launch: $out"; return; }
  tid=$(echo "$out" | jq -r .threadId); note "$tag thread: $tid"
  agent=$(wait_q "$tid") || { fail "$tag no T3 question appeared"; return; }
  req=$(echo "$agent" | jq -c .pending); rid=$(echo "$req" | jq -r .id)
  note "$tag request: $(echo "$req" | jq -c '{id:(.id|.[0:24]),kind,answer_path,status,free_text,title,options:[.options[]|{id,label}]}')"
  echo "$req" | jq -e '.answer_path=="t3" and .free_text==true' >/dev/null && pass "$tag acd publishes the question free_text=true on the t3 path" || fail "$tag request shape: $req"
  ans=$(inbox answer-text "$rid" "$text"); note "$tag answer-text: $ans"
  echo "$ans" | jq -e '.ok and .outcome=="delivered"' >/dev/null && pass "$tag free-text answer delivered ($(echo "$ans" | jq ._ms) ms)" || fail "$tag answer: $ans"
  sleep 2
  r=$(jl "select(.t>$T0 and .msg.type==\"request.resolved\" and .msg.request_id==\"$rid\")|.msg|{by,option_id}" | head -1); note "$tag resolved: $r"
  echo "$r" | jq -e '.by=="inbox"' >/dev/null && pass "$tag request.resolved by=inbox" || fail "$tag resolved: $r"
  cmd=$(tap_cmd); ex=$(tap_exit)
  note "$tag command seen by T3: $(echo "$cmd" | jq -c '.msg.payload|{type,answers}')"
  echo "$cmd" | jq -e --arg t "$text" '.msg.payload.type=="runtime-request.respond" and (.msg.payload.answers|length==1) and ([.msg.payload.answers[]]==[$t]) and (.msg.payload|has("decision")|not)' >/dev/null \
    && pass "$tag respond command carries answers[question id] == the typed text, no decision" || fail "$tag command: $cmd"
  note "$tag T3 exit: $ex"
  echo "$ex" | jq -e '.msg.exit._tag=="Success"' >/dev/null && pass "$tag T3 accepted the command (Exit Success)" || fail "$tag T3 exit: $ex"
  if wait_reply "$tid" "$needle"; then pass "$tag Claude's reply contains the expected text"; else fail "$tag no reply with the expected text: $(cat "$E2E_ROOT/thread-$tid.msgs" | cut -c1-600)"; fi
  note "$tag assistant messages: $(jq -c '[.messages[]|select(.role=="assistant")|.text]' "$E2E_ROOT/thread-$tid.msgs" | cut -c1-500)"
  CASE_TID=$tid
  # candidate fixtures: the command acd sent, T3's Exit, the resolved turn item and runtime request (ids/uuids are scratch)
  jq '.msg' <<<"$cmd" >"$FIX/cmd_request_respond_answers_free_text_$tag.json"
  jq '.msg' <<<"$ex" >"$FIX/cmd_exit_respond_answers_free_text_success_$tag.json"
  jq '[.turnItems[]|select(.questionAnswer!=null)][0]' "$E2E_ROOT/thread-$tid.json" >"$FIX/turn_item_question_answer_free_text_$tag.json"
  jq '.runtimeRequests[0]' "$E2E_ROOT/thread-$tid.json" >"$FIX/thread_runtime_request_resolved_free_text_$tag.json"
  jq -e --arg t "$text" '[.questionAnswer.answers[]]==[$t] and .status=="completed"' "$FIX/turn_item_question_answer_free_text_$tag.json" >/dev/null \
    && pass "$tag T3 turn item questionAnswer.answers holds the typed text" || fail "$tag turn item: $(cut -c1-300 "$FIX/turn_item_question_answer_free_text_$tag.json")"
}

NA="zq$RANDOM café — done"
run_case a "Use the AskUserQuestion tool to ask the user ONE question: 'What should the new service be called?'. The answer must be free text, but the tool requires options, so offer exactly two example options 'Example: alpha' and 'Example: beta'. Do not answer for the user. After the user answers, reply with exactly GOT<<ANSWER>> where ANSWER is the user's answer copied verbatim, character for character, and nothing else." "$NA" "GOT<<$NA>>"
NB="Green$RANDOM"
run_case b "Use the AskUserQuestion tool to ask one single-select question 'Pick a color' with exactly two options Red and Blue, then reply with only the word I picked." "$NB" "$NB"

log "case c: zero-option question (exploratory)"
out=$(t3d launch "Use the AskUserQuestion tool to ask the question 'What is your name?' with an EMPTY options list (zero options). If the tool refuses, report the exact error in your reply instead." --title "t3 free text c") && {
  tid=$(echo "$out" | jq -r .threadId)
  if agent=$(wait_q "$tid" 90); then note "c produced: $(echo "$agent" | jq -c '.pending|{kind,answer_path,free_text,options:[.options[]|{id,label}]}')"
  else sleep 5; t3d dump "$tid" --out "$E2E_ROOT/thread-$tid.json" >"$E2E_ROOT/thread-$tid.msgs" 2>/dev/null; note "c no question within 90 s; assistant said: $(jq -c '[.messages[]|select(.role=="assistant")|.text]' "$E2E_ROOT/thread-$tid.msgs" | cut -c1-500)"; fi; }
note "tap log: $(wc -l <"$TAP_LOG") lines; token never recorded"
exit $FAILED
