# Sourced by scenarios/50-t3.sh and 51-t3-free-text.sh: scratch T3 server, token, project, acd pointed at T3.
# Needs lib.sh, $SCEN, $RESULTS.  T3_ACD_PORT (default $T3_PORT) is the port acd is told to use (51 points it at a tap).
T3H=$E2E_ROOT/t3home; T3P=$E2E_ROOT/t3proj; T3TOK=$E2E_ROOT/t3.token
ms() { python3 -c "print(round(($2-$1)*1000))"; }
# The T3 CLI is always pointed at the scratch data dir explicitly (--base-dir on every subcommand that has
# state), with T3CODE_HOME as a second guard; the desktop T3 (~/.t3, port 3773) must never be reached.
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
  # Own session (setsid via python3) so the whole process tree is one group and down.sh can kill the group;
  # the exec keeps the pid, which is recorded for the ps command-line check in down.sh.
  ( cd "$E2E_ROOT"; exec env -i HOME="$HOME" PATH="$PATH" TERM=xterm-256color LANG="${LANG:-en_US.UTF-8}" USER="$USER" \
      SHELL="${SHELL:-/bin/zsh}" TMPDIR="${TMPDIR:-/tmp}" T3CODE_HOME="$T3H" \
      python3 -c 'import os,sys; os.setsid(); os.execvp(sys.argv[1], sys.argv[1:])' \
      t3 serve --base-dir "$T3H" --port "$T3_PORT" --host 127.0.0.1 --no-browser "$T3P" >"$E2E_ROOT/t3.log" 2>&1 </dev/null & echo $! >"$E2E_ROOT/t3.pid" )
fi
for _ in $(seq 60); do curl -s -o /dev/null --max-time 1 "http://127.0.0.1:$T3_PORT/" && break; sleep 1; done
curl -s -o /dev/null -w "%{http_code}" --max-time 2 "http://127.0.0.1:$T3_PORT/" | grep -q 200 || { fail "scratch t3 did not come up: $(tail -3 "$E2E_ROOT/t3.log")"; exit 1; }
note "scratch T3 on 127.0.0.1:$T3_PORT, T3CODE_HOME=$T3H (desktop T3 untouched)"
( umask 077; t3 t3 auth session issue --base-dir "$T3H" --scope orchestration:read --scope orchestration:operate --ttl 2h --token-only >"$T3TOK" 2>/dev/null ) || { fail "token issue failed"; exit 1; }
chmod 600 "$T3TOK"; [ -s "$T3TOK" ] || { fail "empty t3 token"; exit 1; }
# project registered for the scratch server
t3 t3 project add --base-dir "$T3H" "$T3P" >/dev/null 2>&1 || true

# Point the scratch acd at the scratch T3 (token file in the scratch config dir).
stop_acd; cp "$T3TOK" "$ACD_CONFIG_DIR/t3.token"; chmod 600 "$ACD_CONFIG_DIR/t3.token"
python3 - <<PY
import re,os
p=os.path.join(os.environ["ACD_CONFIG_DIR"],"config.toml"); s=open(p).read()
s=re.sub(r"\[t3\][^\[]*","[t3]\nenabled = true\nurl = \"http://127.0.0.1:${T3_ACD_PORT:-$T3_PORT}\"\n\n",s,count=1)
open(p,"w").write(s)
PY
start_acd; start_logger; sleep 3
acd status | sed -n 2p | tee -a "$RESULTS/$SCEN.txt"
