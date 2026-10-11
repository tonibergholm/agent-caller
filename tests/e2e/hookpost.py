#!/usr/bin/env python3
"""Synthetic Claude http hook client (NOT a live Claude): posts one hook payload
to the scratch acd hook port the way Claude's http hooks do, and prints
{"status", "ms", "body", "error"} as JSON.  The token is read from the scratch
settings.json that `acd hooks install claude` wrote; it is never printed.

  hookpost.py --settings F --port P EVENT --session S [--cmd C] [--timeout SEC] [--cwd D]
"""
import argparse, json, socket, time, uuid

ap = argparse.ArgumentParser()
ap.add_argument("--settings", required=True)
ap.add_argument("--port", type=int, required=True)
ap.add_argument("event")
ap.add_argument("--session", required=True)
ap.add_argument("--cmd", default="touch /tmp/ace2e/x-synth.txt")
ap.add_argument("--cwd", default="/private/tmp/ace2e/synth")
ap.add_argument("--timeout", type=float, default=10)
ap.add_argument("--pane")
a = ap.parse_args()

hooks = json.load(open(a.settings))["hooks"][a.event][0]["hooks"][0]
token = hooks["headers"]["X-Acd-Token"]
payload = {"session_id": a.session, "cwd": a.cwd, "permission_mode": "default", "hook_event_name": a.event,
           "prompt_id": str(uuid.uuid4())}
if a.event in ("PermissionRequest", "PreToolUse", "PostToolUse", "PostToolUseFailure"):
    payload.update(tool_name="Bash", tool_input={"command": a.cmd, "description": "e2e synthetic"})
if a.event == "PostToolUse":
    payload["tool_response"] = {"stdout": "", "stderr": "", "interrupted": False}
if a.event == "UserPromptSubmit":
    payload["prompt"] = "e2e synthetic prompt"
if a.event == "Stop":
    payload["last_assistant_message"] = "done"
body = json.dumps(payload).encode()
req = (f"POST /hook/claude/{a.event}?marker=acd-managed:v1 HTTP/1.1\r\nHost: 127.0.0.1:{a.port}\r\n"
       f"Content-Type: application/json\r\nContent-Length: {len(body)}\r\nX-Acd-Token: {token}\r\n"
       f"X-Claude-Entrypoint: cli\r\nConnection: close\r\n\r\n").encode() + body
t0 = time.time()
out = {"event": a.event, "session": a.session, "status": None, "body": "", "error": None}
try:
    s = socket.create_connection(("127.0.0.1", a.port), timeout=a.timeout)
    s.settimeout(max(0.01, a.timeout - (time.time() - t0)))
    s.sendall(req)
    buf = b""
    while True:
        s.settimeout(max(0.01, a.timeout - (time.time() - t0)))
        c = s.recv(65536)
        if not c:
            break
        buf += c
    head, _, rest = buf.partition(b"\r\n\r\n")
    out["status"] = int(head.split(b" ")[1]) if head else None
    out["body"] = rest.decode(errors="replace")[:400]
    if not buf:
        out["error"] = "empty reply (connection closed)"
except socket.timeout:
    out["error"] = "client timeout"
except OSError as e:
    out["error"] = type(e).__name__ + ": " + str(e)
out["ms"] = round((time.time() - t0) * 1000)
print(json.dumps(out))
