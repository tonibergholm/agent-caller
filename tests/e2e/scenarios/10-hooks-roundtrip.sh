#!/usr/bin/env bash
# §13 "running hook install and uninstall twice each" / §14.9 / coexisting permission hooks.
# Pure file test in scratch dirs; needs no daemon and no agent.  Seeds contain a foreign
# PermissionRequest hook so coexistence is part of the round trip.
. "$(dirname "$0")/../lib.sh" || exit 1
RT=$E2E_ROOT/rt; rm -rf "$RT"; mkdir -p "$RT/home" "$RT/claude" "$RT/codex" "$RT/acd"; chmod 700 "$RT/acd"
export ACD_CONFIG_DIR=$RT/acd CLAUDE_CONFIG_DIR=$RT/claude CODEX_HOME=$RT/codex
A() { HOME=$RT/home "$ACD_BIN" "$@"; }
canon() { jq -S 'walk(if type=="object" and has("X-Acd-Token") then .["X-Acd-Token"]="<redacted>" else . end)' "$1"; }
A init >/dev/null || exit 1
cat >"$RT/claude/settings.json" <<'J'
{ "permissions": {"allow": ["Bash(ls:*)"]},
  "hooks": {
    "PermissionRequest": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "echo foreign-permission-hook", "timeout": 5}]}],
    "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "rtk hook claude"}]}],
    "SessionStart": [{"hooks": [{"type": "command", "command": "echo foreign-start"}]}] } }
J
cat >"$RT/codex/hooks.json" <<'J'
{ "hooks": { "SessionStart": [ { "hooks": [ { "type": "command", "command": "sh /tmp/foreign-codex-hook.sh session", "timeout": 10 } ] } ],
  "PermissionRequest": [ { "hooks": [ { "type": "command", "command": "echo foreign-codex-permission", "timeout": 1 } ] } ] } }
J
fail=0
for t in claude codex; do
  f=$RT/$t/$([ $t = claude ] && echo settings.json || echo hooks.json)
  canon "$f" >"$RT/$t.before.json"
  A hooks install $t >"$RT/$t.install1.txt" 2>&1 || { echo "FAIL: $t install 1: $(head -3 "$RT/$t.install1.txt")"; fail=1; }
  canon "$f" >"$RT/$t.after1.json"
  A hooks install $t >"$RT/$t.install2.txt" 2>&1 || { echo "FAIL: $t install 2: $(head -3 "$RT/$t.install2.txt")"; fail=1; }
  canon "$f" >"$RT/$t.after2.json"
  A hooks uninstall $t >"$RT/$t.uninstall1.txt" 2>&1 || { echo "FAIL: $t uninstall: $(head -3 "$RT/$t.uninstall1.txt")"; fail=1; }
  canon "$f" >"$RT/$t.after-uninstall.json"
  cmp -s "$RT/$t.after1.json" "$RT/$t.after2.json" && echo "PASS $t: install x2 == install x1 (jq -S)" || { echo "FAIL $t: install 2 changed the config"; diff "$RT/$t.after1.json" "$RT/$t.after2.json" | head; fail=1; }
  cmp -s "$RT/$t.before.json" "$RT/$t.after-uninstall.json" && echo "PASS $t: install x2 + uninstall x1 == original (jq -S)" || { echo "FAIL $t: uninstall differs from original"; diff "$RT/$t.before.json" "$RT/$t.after-uninstall.json" | head -20; fail=1; }
  # coexistence: foreign PermissionRequest hook still present next to acd's after install, and after uninstall.
  jq -e '[.hooks.PermissionRequest[]?.hooks[]?.command // empty | select(startswith("echo foreign"))] | length == 1' "$RT/$t.after2.json" >/dev/null \
    && echo "PASS $t: foreign PermissionRequest hook kept next to acd's" || { echo "FAIL $t: foreign hook lost"; fail=1; }
  echo "  PermissionRequest handlers after install: $(jq '[.hooks.PermissionRequest[]?.hooks[]?] | length' "$RT/$t.after2.json")"
done
A hooks status 2>&1 | sed 's/X-Acd-Token[^ ,]*/X-Acd-Token=<redacted>/' | head -8
rm -rf "$RT/acd" "$RT/home"   # scratch tokens
exit $fail
