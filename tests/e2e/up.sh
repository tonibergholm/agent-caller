#!/usr/bin/env bash
# Brings up the isolated environment: scratch herdr session (herdr's own headless
# server), scratch acd (fresh `acd init`, moved ports, T3 off), scratch hook
# install for Claude, and the acd/1 logger.  Idempotent only after down.sh.
set -euo pipefail
. "$(dirname "$0")/lib.sh" || exit 1

[ -x "$ACD_BIN" ] || { echo "build first: cargo build --release -p acd --locked" >&2; exit 1; }
[ ! -e "$E2E_ROOT/acd.pid" ] || { echo "already up (acd.pid exists); run down.sh" >&2; exit 1; }
[ ! -e "$E2E_ROOT/herdr.pid" ] || { echo "already up (herdr.pid exists); run down.sh" >&2; exit 1; }
mkdir -p "$E2E_ROOT" "$SCRATCH_HOME/.config/herdr" "$PROJ" "$CLAUDE_CFG"
chmod 700 "$E2E_ROOT"
# Fresh results every run, so a FAIL from an earlier run cannot leak into this one.
rm -rf "$RESULTS"; mkdir -p "$RESULTS"
[ -d "$PROJ/.git" ] || git -C "$PROJ" init -q

printf "onboarding = false\n" >"$SCRATCH_HOME/herdr-config.toml"
# 1. herdr: its own headless server (`herdr --session NAME server`), detached, clean environment,
#    HOME=$SCRATCH_HOME so the session socket is not under the real ~/.config/herdr.
if [ ! -S "$HERDR_SOCK" ]; then
  ( exec env -i HOME="$SCRATCH_HOME" PATH="$PATH" TERM=xterm-256color LANG="${LANG:-en_US.UTF-8}" \
      USER="$USER" SHELL="${SHELL:-/bin/zsh}" TMPDIR="${TMPDIR:-/tmp}" HERDR_CONFIG_PATH="$SCRATCH_HOME/herdr-config.toml" \
      nohup herdr --session "$HERDR_SESSION" server >"$E2E_ROOT/herdr.log" 2>&1 </dev/null & echo $! >"$E2E_ROOT/herdr.pid" )
else
  echo "herdr socket $HERDR_SOCK already exists but no herdr.pid; run down.sh" >&2; exit 1
fi
for _ in $(seq 100); do [ -S "$HERDR_SOCK" ] && break; sleep 0.1; done
[ -S "$HERDR_SOCK" ] || { echo "herdr socket $HERDR_SOCK did not appear" >&2; exit 1; }
pid_cmd_matches "$(cat "$E2E_ROOT/herdr.pid")" "$HERDR_RE" || log "note: herdr.pid does not match the server command; down.sh will use 'herdr server stop'"
# Two workspaces: w1 (Codex scenarios, P=w1:p1) and w2 (Claude scenarios, Q=w2:p1).  A headless server
# starts with none, unlike the old tmux-hosted TUI, which came with a default w1.
hd workspace create --cwd "$PROJ" --label e2e-codex --no-focus >/dev/null
hd workspace create --cwd "$PROJ" --label e2e-claude --no-focus >/dev/null
for want in w1:p1 w2:p1; do
  hd pane list | grep -q "$want" || echo "WARNING: pane $want not found; set P / Q for the scenarios (hd pane list)" >&2
done
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
start_logger
sleep 1
log "acd up (pid $(acd_pid)), client port $CLIENT_PORT, hook port $HOOK_PORT"
acd status || true
