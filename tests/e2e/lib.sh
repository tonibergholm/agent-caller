# Shared by the e2e scripts.  Source it; do not run it.
# Everything lives under $E2E_ROOT.  Nothing here may touch the user's own
# acd (launchd job, ~/.config/acd, ports 47800/47801), herdr default session,
# or the desktop T3 (port 3773, ~/.t3).

E2E_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO=$(cd "$E2E_DIR/../.." && pwd)
readonly E2E_ROOT=/tmp/ace2e
# /tmp/ace2e must be a real directory we own: not a symlink, not someone else's.
if [ -L "$E2E_ROOT" ]; then echo "$E2E_ROOT is a symlink; refusing" >&2; return 1; fi
if [ -e "$E2E_ROOT" ]; then
  [ -d "$E2E_ROOT" ] && [ -O "$E2E_ROOT" ] \
    || { echo "$E2E_ROOT is not a directory owned by you; refusing" >&2; return 1; }
  case "$(cd "$E2E_ROOT" && pwd -P)" in /tmp/ace2e|/private/tmp/ace2e) ;; *) echo "$E2E_ROOT resolves elsewhere; refusing" >&2; return 1;; esac
fi

ACD_BIN=${ACD_BIN:-$REPO/target/release/acd}
case $ACD_BIN in */.local/bin/acd) echo "ACD_BIN=$ACD_BIN is the user's real acd; refusing" >&2; return 1;; esac
export ACD_CONFIG_DIR=$E2E_ROOT/acd
SCRATCH_HOME=$E2E_ROOT/home
REAL_HOME=$HOME
HERDR_SESSION=ace2e
# herdr runs with HOME=$SCRATCH_HOME so its session socket is NOT under the real
# ~/.config/herdr/sessions, which the user's own acd scans (it would adopt our panes).
HERDR_SOCK=$SCRATCH_HOME/.config/herdr/sessions/$HERDR_SESSION/herdr.sock
CLIENT_PORT=${E2E_CLIENT_PORT:-52956}
HOOK_PORT=${E2E_HOOK_PORT:-52955}
T3_PORT=${E2E_T3_PORT:-52957}
for p in $CLIENT_PORT $HOOK_PORT $T3_PORT; do
  case $p in 47800|47801|3773) echo "port $p belongs to the user's real services" >&2; return 1;; esac
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
hd() {
  [ -S "$HERDR_SOCK" ] || { echo "scratch herdr socket missing" >&2; return 1; }
  # Strip any herdr context inherited from a pane we may be running inside, and pin the session, so
  # a call can never reach the user's own herdr.
  env -u HERDR_SESSION -u HERDR_PANE_ID -u HERDR_CLIENT_SOCKET_PATH -u HERDR_ENV \
      HOME="$SCRATCH_HOME" HERDR_SOCKET_PATH="$HERDR_SOCK" herdr --session "$HERDR_SESSION" "$@"
}
inbox() { python3 "$E2E_DIR/inbox.py" -d "$ACD_CONFIG_DIR" "$@"; }
acd() { HOME=$SCRATCH_HOME HERDR_SOCKET_PATH=$HERDR_SOCK "$ACD_BIN" "$@"; }
# The scratch acd pid, only if the pid file still points at a scratch `acd run` (the scenarios SIGSTOP/CONT it).
acd_pid() { local p; p=$(cat "$E2E_ROOT/acd.pid" 2>/dev/null) && pid_cmd_matches "$p" "$ACD_RE" && echo "$p"; }
now_iso() { python3 -c 'from datetime import datetime,timezone;print(datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%f")[:-3]+"Z")'; }
log() { echo "[$(date +%H:%M:%S)] $*" >&2; }

# pid_cmd_matches PID REGEX: true when PID is alive and its command line matches REGEX.  Every kill of
# a pid read from a pid file goes through this, so a stale file cannot kill an unrelated process.
# A pid whose command line mentions the user's real acd (~/.local/bin/acd, ~/.config/acd) never matches.
pid_cmd_matches() {
  local c
  [ -n "${1:-}" ] || return 1
  c=$(ps -p "$1" -o command= 2>/dev/null) || return 1
  case $c in */.local/bin/acd*|*"$REAL_HOME/.config/acd"*|*" $HOME/.config/acd"*) return 1;; esac
  printf '%s\n' "$c" | grep -Eq -- "$2"
}
# kill_pidfile FILE REGEX [group]: TERM, wait, KILL a verified pid; with "group" the whole process group.
kill_pidfile() {
  local f=$1 re=$2 grp=${3:-} p t=
  [ -f "$f" ] || return 0
  p=$(cat "$f" 2>/dev/null)
  if pid_cmd_matches "$p" "$re"; then
    [ -n "$grp" ] && [ "$(ps -p "$p" -o pgid= | tr -d ' ')" = "$p" ] && t=-
    kill -CONT -- "${t}$p" 2>/dev/null; kill -TERM -- "${t}$p" 2>/dev/null
    for _ in $(seq 50); do kill -0 "$p" 2>/dev/null || break; sleep 0.1; done
    kill -0 "$p" 2>/dev/null && kill -KILL -- "${t}$p" 2>/dev/null
  elif [ -n "$p" ]; then
    echo "pid file $f: pid $p is not ours (stale); not killing" >&2
  fi
  rm -f "$f"; return 0
}
# Exactly the scratch binary: the user's launchd acd (`~/.local/bin/acd run`) must never match.
ACD_RE="^$(printf '%s' "$ACD_BIN" | sed 's/[][\\.^$*+?(){}|/]/\\&/g') run( |\$)"
LOGGER_RE='inbox\.py --name logger'
T3_RE='--base-dir /tmp/ace2e/t3home|--base-dir /private/tmp/ace2e/t3home'
HERDR_RE="herdr --session $HERDR_SESSION server"

start_acd() {
  ( cd "$E2E_ROOT"; HOME=$SCRATCH_HOME HERDR_SOCKET_PATH=$HERDR_SOCK RUST_LOG=${RUST_LOG:-debug} \
      exec "$ACD_BIN" run >>"$ACD_LOG" 2>&1 </dev/null & echo $! >"$E2E_ROOT/acd.pid" )
  for _ in $(seq 100); do [ -S "$ACD_CONFIG_DIR/acd.sock" ] && return 0; sleep 0.1; done
  echo "acd did not create acd.sock" >&2; return 1
}
stop_acd() { kill_pidfile "$E2E_ROOT/acd.pid" "$ACD_RE"; }

# Pane helpers.  PANE ids come from `hd pane list`.
pane_text() { hd pane read "$1" --source visible 2>/dev/null; }
pane_det()  { hd pane read "$1" --source detection 2>/dev/null; }
wait_pane_text() { # pane regex timeout
  local end=$(( $(date +%s) + ${3:-30} ))
  while [ "$(date +%s)" -lt $end ]; do pane_text "$1" | grep -Eq "$2" && return 0; sleep 0.5; done; return 1
}

start_logger() {
  stop_logger
  python3 "$E2E_DIR/inbox.py" --name logger log --out "$INBOX_LOG" >/dev/null 2>>"$E2E_ROOT/logger.err" </dev/null &
  echo $! >"$E2E_ROOT/logger.pid"
}
stop_logger() { kill_pidfile "$E2E_ROOT/logger.pid" "$LOGGER_RE"; }
epoch_now() { python3 -c 'import time;print(time.time())'; }
# hookpost: a synthetic Claude http hook (see hookpost.py); clearly not a live Claude.
hp() { python3 "$E2E_DIR/hookpost.py" --settings "$CLAUDE_CFG/settings.json" --port "$HOOK_PORT" "$@"; }
# jq over the inbox log: jl '<filter>' prints matching server messages as compact lines.
jl() { jq -c "select(.dir==\"in\" and .who==\"logger\") | $1" "$INBOX_LOG"; }
# The user's own acd, read-only status.  ACD_CONFIG_DIR is exported for the scratch daemon, so unset it.
real_acd_status() { env -u ACD_CONFIG_DIR "$HOME/.local/bin/acd" status 2>&1 | cut -c1-200; }
pass() { echo "PASS  $*" | tee -a "$RESULTS/$SCEN.txt"; }
fail() { echo "FAIL  $*" | tee -a "$RESULTS/$SCEN.txt"; FAILED=1; }
note() { echo "      $*" | tee -a "$RESULTS/$SCEN.txt"; }

# Claude cannot be isolated from the user's hooks by --setting-sources (it still loads ~/.claude/settings.json,
# whose http hooks point at the user's acd on 47801), nor by allowedHttpHookUrls (2.1.296).  So the Claude
# process runs under sandbox-exec with an outbound-connect deny for the user's acd hook and client ports.
# (Not used for Codex: its own seatbelt cannot nest.  Codex is isolated by hook trust instead.)
DENY_SB=$E2E_ROOT/deny-real-acd.sb
write_deny_profile() {
  # Deny: connecting to the user's acd (tcp 47800/47801 and its unix sockets) and writing the user's
  # acd / Claude / Codex hook configuration.  Everything else is allowed.
  local h=$REAL_HOME
  cat >"$DENY_SB" <<SB
(version 1)
(allow default)
(deny network-outbound (remote tcp "*:47801"))
(deny network-outbound (remote tcp "*:47800"))
(deny network-outbound (remote unix-socket (path-literal "$h/.config/acd/acd.sock")))
(deny network-outbound (remote unix-socket (path-literal "$h/.config/acd/hook.sock")))
(deny file-write* (subpath "$h/.config/acd"))
(deny file-write* (literal "$h/.claude/settings.json"))
(deny file-write* (literal "$h/.codex/hooks.json"))
(deny file-write* (literal "$h/.codex/config.toml"))
SB
}
claude_launch() { # pane [extra claude args...]
  local pane=$1; shift
  write_deny_profile
  hd pane run "$pane" "clear; cd /private/tmp/ace2e/proj && HOME=$REAL_HOME sandbox-exec -f $DENY_SB claude --model haiku --permission-mode default --settings $CLAUDE_CFG/settings.json $*" >/dev/null
}
