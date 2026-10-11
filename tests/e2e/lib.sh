# Shared by the e2e scripts.  Source it; do not run it.
# Everything lives under $E2E_ROOT.  Nothing here may touch the user's own
# acd (launchd job, ~/.config/acd, ports 47800/47801), herdr default session,
# or the desktop T3 (port 3773, ~/.t3).

E2E_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO=$(cd "$E2E_DIR/../.." && pwd)
E2E_ROOT=${E2E_ROOT:-/tmp/ace2e}
case "$E2E_ROOT" in /tmp/ace2e*) ;; *) echo "E2E_ROOT must be under /tmp/ace2e" >&2; return 1 2>/dev/null || exit 1;; esac

ACD_BIN=${ACD_BIN:-$REPO/target/release/acd}
export ACD_CONFIG_DIR=$E2E_ROOT/acd
SCRATCH_HOME=$E2E_ROOT/home
REAL_HOME=$HOME
HERDR_SESSION=ace2e
# herdr runs with HOME=$SCRATCH_HOME so its session socket is NOT under the real
# ~/.config/herdr/sessions, which the user's own acd scans (it would adopt our panes).
HERDR_SOCK=$SCRATCH_HOME/.config/herdr/sessions/$HERDR_SESSION/herdr.sock
TMUX_SOCK=ace2e
CLIENT_PORT=${E2E_CLIENT_PORT:-52956}
HOOK_PORT=${E2E_HOOK_PORT:-52955}
T3_PORT=${E2E_T3_PORT:-52957}
for p in $CLIENT_PORT $HOOK_PORT $T3_PORT; do
  case $p in 47800|47801|3773) echo "port $p belongs to the user's real services" >&2; return 1 2>/dev/null || exit 1;; esac
done
INBOX_LOG=$E2E_ROOT/inbox.jsonl
ACD_LOG=$E2E_ROOT/acd.log
RESULTS=$E2E_ROOT/results
CLAUDE_CFG=$E2E_ROOT/claude-cfg          # CLAUDE_CONFIG_DIR for the scratch `acd hooks install claude`
PROJ=$E2E_ROOT/proj                      # cwd for the test agents
export INBOX_LOG

# A clean environment for everything that launches an agent: no CLAUDE_* /
# CODEX_* inheritance (an inherited CLAUDE_CODE_CHILD_SESSION turns transcripts
# off; sdk-ts entrypoints are ignored by acd).
clean_env() {
  env -i HOME="$HOME" PATH="$PATH" TERM=xterm-256color LANG="${LANG:-en_US.UTF-8}" \
      USER="$USER" SHELL="${SHELL:-/bin/zsh}" TMPDIR="${TMPDIR:-/tmp}" "$@"
}

# Every herdr call goes through the scratch session's socket.
hd() { HERDR_SOCKET_PATH=$HERDR_SOCK herdr "$@"; }
inbox() { python3 "$E2E_DIR/inbox.py" -d "$ACD_CONFIG_DIR" "$@"; }
acd() { HOME=$SCRATCH_HOME HERDR_SOCKET_PATH=$HERDR_SOCK "$ACD_BIN" "$@"; }
acd_pid() { cat "$E2E_ROOT/acd.pid" 2>/dev/null; }
now_iso() { python3 -c 'from datetime import datetime,timezone;print(datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%f")[:-3]+"Z")'; }
log() { echo "[$(date +%H:%M:%S)] $*" >&2; }

start_acd() {
  ( cd "$E2E_ROOT"; HOME=$SCRATCH_HOME HERDR_SOCKET_PATH=$HERDR_SOCK RUST_LOG=${RUST_LOG:-debug} \
      exec "$ACD_BIN" run >>"$ACD_LOG" 2>&1 </dev/null & echo $! >"$E2E_ROOT/acd.pid" )
  for _ in $(seq 100); do [ -S "$ACD_CONFIG_DIR/acd.sock" ] && return 0; sleep 0.1; done
  echo "acd did not create acd.sock" >&2; return 1
}
stop_acd() {
  local p; p=$(acd_pid) || return 0
  [ -n "$p" ] && kill -CONT "$p" 2>/dev/null; [ -n "$p" ] && kill -TERM "$p" 2>/dev/null
  for _ in $(seq 50); do kill -0 "$p" 2>/dev/null || break; sleep 0.1; done
  kill -0 "$p" 2>/dev/null && kill -KILL "$p" 2>/dev/null
  rm -f "$E2E_ROOT/acd.pid"; return 0
}

# Pane helpers.  PANE ids come from `hd pane list`.
pane_text() { hd pane read "$1" --source visible 2>/dev/null; }
pane_det()  { hd pane read "$1" --source detection 2>/dev/null; }
wait_pane_text() { # pane regex timeout
  local end=$(( $(date +%s) + ${3:-30} ))
  while [ "$(date +%s)" -lt $end ]; do pane_text "$1" | grep -Eq "$2" && return 0; sleep 0.5; done; return 1
}
