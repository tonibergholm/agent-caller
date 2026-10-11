#!/usr/bin/env bash
# Brings up the isolated environment: scratch herdr session in its own tmux
# server, scratch acd (fresh `acd init`, moved ports, T3 off), scratch hook
# install for Claude, and the acd/1 logger.  Idempotent only after down.sh.
set -euo pipefail
. "$(dirname "$0")/lib.sh"

[ -x "$ACD_BIN" ] || { echo "build first: cargo build --release -p acd --locked" >&2; exit 1; }
[ ! -e "$E2E_ROOT/acd.pid" ] || { echo "already up (acd.pid exists); run down.sh" >&2; exit 1; }
mkdir -p "$SCRATCH_HOME/.config/herdr" "$E2E_ROOT" "$SCRATCH_HOME" "$PROJ" "$RESULTS" "$CLAUDE_CFG"
chmod 700 "$E2E_ROOT"
[ -d "$PROJ/.git" ] || git -C "$PROJ" init -q

printf "onboarding = false\n" >"$SCRATCH_HOME/herdr-config.toml"
# 1. herdr: own session in an own tmux server (-L), clean environment.
if ! tmux -L $TMUX_SOCK has-session -t $HERDR_SESSION 2>/dev/null; then
  clean_env HOME="$SCRATCH_HOME" HERDR_CONFIG_PATH="$SCRATCH_HOME/herdr-config.toml" tmux -L $TMUX_SOCK new-session -d -s $HERDR_SESSION -c "$PROJ" -x 200 -y 50 "herdr --session $HERDR_SESSION" >/dev/null 2>&1
fi
for _ in $(seq 100); do [ -S "$HERDR_SOCK" ] && break; sleep 0.1; done
[ -S "$HERDR_SOCK" ] || { echo "herdr socket $HERDR_SOCK did not appear" >&2; exit 1; }
hd workspace create --cwd "$PROJ" --label e2e --no-focus >/dev/null
log "herdr session $HERDR_SESSION up: $(hd pane list | head -3 | tr '\n' ' ')"

# 2. acd: fresh init, then moved ports, T3 off (the default would try the desktop T3).
acd init >/dev/null
python3 - <<PY
import re,os
p=os.path.join(os.environ["ACD_CONFIG_DIR"],"config.toml")
s=open(p).read()
def setkey(s,key,val,section=None):
    pat=re.compile(r"^%s\s*=.*$"%key,re.M)
    return pat.sub("%s = %s"%(key,val),s,1) if pat.search(s) else "%s = %s\n%s"%(key,val,s)
s=setkey(s,"port","$CLIENT_PORT"); s=setkey(s,"hook_port","$HOOK_PORT")
s=setkey(s,"listen",'["loopback"]')
if "[t3]" in s: s=re.sub(r"\[t3\][^\[]*","[t3]\nenabled = false\n\n",s,count=1)
else: s+="\n[t3]\nenabled = false\n"
open(p,"w").write(s)
PY
chmod 600 "$ACD_CONFIG_DIR/config.toml"
grep -q '^port = '"$CLIENT_PORT" "$ACD_CONFIG_DIR/config.toml" && grep -q '^hook_port = '"$HOOK_PORT" "$ACD_CONFIG_DIR/config.toml" \
  || { echo "config.toml port edit failed" >&2; exit 1; }
if [ -n "${E2E_CLAUDE_HOLD:-}" ]; then
  sed -i '' "s/^permission_hold = .*/permission_hold = $E2E_CLAUDE_HOLD/" "$ACD_CONFIG_DIR/config.toml"
fi

# 3. Scratch hook installs: Claude into a scratch CLAUDE_CONFIG_DIR (sessions load it via --settings),
#    Codex into the scratch project's .codex/hooks.json (see README for why not ~/.codex).
CLAUDE_CONFIG_DIR=$CLAUDE_CFG acd hooks install claude >"$E2E_ROOT/hooks-install-claude.txt" 2>&1 || { cat "$E2E_ROOT/hooks-install-claude.txt"; exit 1; }

start_acd
python3 "$E2E_DIR/inbox.py" --name logger log --out "$INBOX_LOG" >/dev/null 2>"$E2E_ROOT/logger.err" &
echo $! >"$E2E_ROOT/logger.pid"
sleep 1
log "acd up (pid $(acd_pid)), client port $CLIENT_PORT, hook port $HOOK_PORT"
acd status || true
