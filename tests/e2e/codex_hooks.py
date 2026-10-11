#!/usr/bin/env python3
"""Lists Codex hooks for a cwd through `codex app-server` (hooks/list: read-only, starts no
session, runs no hook).  With --trust-project-hooks it prints the [hooks.state] TOML that
trusts the project-level hooks only.  Default: print source, trust status, event, command stem.
"""
import json, subprocess, sys, time

def hooks(cwd):
    p = subprocess.Popen(["codex", "app-server"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                         stderr=subprocess.DEVNULL, text=True, cwd=cwd)
    def send(o): p.stdin.write(json.dumps(o) + "\n"); p.stdin.flush()
    def rd(w):
        end = time.time() + 30
        while time.time() < end:
            l = p.stdout.readline()
            if not l: return None
            m = json.loads(l)
            if m.get("id") == w: return m
    send({"id": 1, "method": "initialize", "params": {"clientInfo": {"name": "e2e", "version": "0"}}}); rd(1)
    send({"method": "initialized"}); send({"id": 2, "method": "hooks/list", "params": {"cwds": [cwd]}})
    r = rd(2); p.terminate()
    return [h for e in r["result"]["data"] for h in e["hooks"]]

if __name__ == "__main__":
    cwd = sys.argv[1]
    hs = hooks(cwd)
    if "--trust-project-hooks" in sys.argv:
        mine = [h for h in hs if h["source"] == "project"]
        if not mine: sys.exit("no project hooks listed (is the folder trusted?)")
        for h in mine:
            print('\n[hooks.state."%s"]\ntrusted_hash = "%s"' % (h["key"], h["currentHash"]))
    else:
        for h in hs:
            cmd = h.get("command") or ""
            print("%-18s %-10s %-9s enabled=%s %s" % (h["eventName"], h["source"], h["trustStatus"], h["enabled"], cmd[:70]))
