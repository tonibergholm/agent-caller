#!/usr/bin/env python3
"""Lists Codex hooks for a cwd through `codex app-server` (hooks/list: read-only, starts no
session, runs no hook).

  codex_hooks.py CWD                        print source, trust status, event, command stem
  codex_hooks.py CWD --trust-project-hooks  print the [hooks.state] TOML that trusts the project-level hooks only
  codex_hooks.py CWD --verify               exit 1 unless every acd hook outside the project is untrusted/modified/disabled
                                            and every project hook is trusted and enabled

Every read from the app-server has a deadline (READ_TIMEOUT seconds, select-based), so a hung server
cannot wedge the caller.
"""
import json, os, re, select, shlex, subprocess, sys, time

READ_TIMEOUT = 20


def hooks(cwd):
    p = subprocess.Popen(["codex", "app-server"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                         stderr=subprocess.DEVNULL, bufsize=0, cwd=cwd)
    try:
        buf = bytearray()

        def send(o):
            p.stdin.write((json.dumps(o) + "\n").encode()); p.stdin.flush()

        def readline(deadline):
            # Unbuffered fd + our own line buffer, so select() never waits on data we already hold.
            while b"\n" not in buf:
                left = deadline - time.time()
                if left <= 0 or not select.select([p.stdout], [], [], left)[0]:
                    return None
                chunk = os.read(p.stdout.fileno(), 65536)
                if not chunk:
                    return None
                buf.extend(chunk)
            i = buf.index(b"\n"); line = bytes(buf[:i]); del buf[:i + 1]
            return line

        def rd(want):
            deadline = time.time() + READ_TIMEOUT
            while time.time() < deadline:
                line = readline(deadline)
                if not line:
                    return None
                try:
                    m = json.loads(line)
                except ValueError:
                    continue
                if m.get("id") == want:
                    return m
            return None

        send({"id": 1, "method": "initialize", "params": {"clientInfo": {"name": "e2e", "version": "0"}}})
        if rd(1) is None:
            sys.exit("codex app-server: no initialize reply within %ds" % READ_TIMEOUT)
        send({"method": "initialized"})
        send({"id": 2, "method": "hooks/list", "params": {"cwds": [cwd]}})
        r = rd(2)
        if r is None or "result" not in r:
            sys.exit("codex app-server: no hooks/list reply within %ds: %s" % (READ_TIMEOUT, json.dumps(r)[:200]))
        return [h for e in r["result"]["data"] for h in e["hooks"]]
    finally:
        p.terminate()
        try:
            p.wait(timeout=5)
        except subprocess.TimeoutExpired:
            p.kill()


_SECRET_KEY = r"(?:token|key|secret|bearer|password|passwd|auth|credential)"


def redact(cmd):
    """Mask values that look like credentials before a hook command is printed."""
    cmd = re.sub(r"(?i)\b(Bearer)\s+\S+", r"\1 <redacted>", cmd)
    cmd = re.sub(r"(?i)(\b[\w.-]*%s[\w.-]*\s*[=:]\s*)(\"[^\"]*\"|'[^']*'|\S+)" % _SECRET_KEY, r"\1<redacted>", cmd)
    cmd = re.sub(r"(?i)(--?[\w-]*%s[\w-]*[ =])(\"[^\"]*\"|'[^']*'|\S+)" % _SECRET_KEY, r"\1<redacted>", cmd)
    return cmd


def is_acd(h):
    """Fail closed: anything that could be an acd hook counts as one.  A missed acd hook would let
    Codex deliver scratch events to the user's real daemon (a marker-less `acd hook codex X` falls back
    to $ACD_CONFIG_DIR or ~/.config/acd)."""
    cmd = h.get("command") or ""
    try:
        toks = shlex.split(cmd)
    except ValueError:
        return True
    if any(os.path.basename(t) == "acd" for t in toks):
        return True
    if re.search(r"hook\s+codex", cmd):
        return True
    return any(m in cmd for m in ("acd-managed", ".config/acd", "47801", "hook.sock"))


def check(hs):
    """Returns a list of problems; empty means the isolation precondition holds."""
    bad = []
    for h in hs:
        ts, en = str(h.get("trustStatus")).lower(), h.get("enabled") is not False
        who = "%s %s %s" % (h.get("source"), h.get("eventName"), redact(h.get("command") or "")[:60])
        if h.get("source") == "project":
            if ts != "trusted" or not en:
                bad.append("project hook not trusted/enabled (%s, enabled=%s): %s" % (ts, en, who))
            if not re.search(r"--config-dir\s+['\"]?(?:/private)?/tmp/ace2e/acd['\"]?(?:\s|$)", h.get("command") or ""):
                bad.append("project hook lacks --config-dir '/tmp/ace2e/acd': %s" % who)
        elif is_acd(h) and en and ts not in ("untrusted", "modified"):
            # Other hooks (the user's own scripts, plugins) may run; only an acd
            # hook outside the scratch project would reach the user's real acd.
            bad.append("acd hook outside the scratch project would run (%s, enabled=%s): %s" % (ts, en, who))
    if not any(h.get("source") == "project" for h in hs):
        bad.append("no project hooks listed")
    return bad


if __name__ == "__main__":
    cwd = sys.argv[1]
    hs = hooks(cwd)
    if "--trust-project-hooks" in sys.argv:
        mine = [h for h in hs if h["source"] == "project"]
        if not mine: sys.exit("no project hooks listed (is the folder trusted?)")
        for h in mine:
            print('\n[hooks.state."%s"]\ntrusted_hash = "%s"' % (h["key"], h["currentHash"]))
    elif "--verify" in sys.argv:
        bad = check(hs)
        for b in bad: print(b, file=sys.stderr)
        sys.exit(1 if bad else 0)
    else:
        print("# %d hooks, %d classified as acd" % (len(hs), sum(1 for h in hs if is_acd(h))))
        for h in hs:
            cmd = redact(h.get("command") or "")
            print("%-18s %-10s %-9s enabled=%s %s%s" % (h["eventName"], h["source"], h["trustStatus"], h["enabled"], "[acd] " if is_acd(h) else "", cmd[:70]))
