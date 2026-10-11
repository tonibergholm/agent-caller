#!/usr/bin/env bash
# Tears everything down: logger, acd, scratch T3, herdr session, tmux server,
# scratch tokens.  Safe to run twice. 
. "$(dirname "$0")/lib.sh"

[ -f "$E2E_ROOT/logger.pid" ] && kill "$(cat "$E2E_ROOT/logger.pid")" 2>/dev/null
stop_acd
[ -f "$E2E_ROOT/t3.pid" ] && { kill "$(cat "$E2E_ROOT/t3.pid")" 2>/dev/null; sleep 1; kill -9 "$(cat "$E2E_ROOT/t3.pid")" 2>/dev/null; }
# Agents first (they hold panes), then the herdr server through its own socket.
if [ -S "$HERDR_SOCK" ]; then
  for p in $(hd pane list 2>/dev/null | grep -Eo '"pane_id": *"[^"]+"' | sed 's/.*: *"//;s/"//'); do hd pane close "$p" >/dev/null 2>&1; done
  HERDR_SOCKET_PATH=$HERDR_SOCK herdr server stop >/dev/null 2>&1
fi
sleep 1
tmux -L $TMUX_SOCK kill-server 2>/dev/null
# Scratch tokens (acd's own, T3's) go away with the config dir and T3 home.
rm -rf "$SCRATCH_HOME" "$ACD_CONFIG_DIR" "$E2E_ROOT/t3home" "$CLAUDE_CFG"
echo "down: acd/t3/herdr/tmux stopped, scratch tokens removed"
