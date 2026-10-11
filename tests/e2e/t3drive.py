#!/usr/bin/env python3
"""Minimal T3 Code driver for the e2e harness (stdlib).  Talks to a SCRATCH t3 server only.

  t3drive.py --port P --token-file F launch "prompt text" [--model claude-sonnet-5-5] [--title T]
  t3drive.py --port P --token-file F threads

`launch` creates a thread in the first project (approval-required, so tools need approval) and
dispatches the first message; it prints {"threadId": ...}.  The token is read from the file and never printed.
Frames follow the T3 gate-3 notes: effect/rpc JSON, every Chunk must be Acked.
"""
import argparse, base64, hashlib, json, os, socket, sys, time, uuid
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from inbox import Conn, GUID  # reuse the WebSocket framing


class T3(Conn):
    def __init__(self, port, token):
        self.name, self.client_id, self.log, self.buf = "t3drive", "t3drive", None, b""
        import threading; self._lock = threading.Lock()
        s = socket.create_connection(("127.0.0.1", port), timeout=10)
        key = base64.b64encode(os.urandom(16)).decode()
        s.sendall(("GET /ws?orchestrationProtocol=2 HTTP/1.1\r\nHost: 127.0.0.1:%d\r\nUpgrade: websocket\r\n"
                   "Connection: Upgrade\r\nSec-WebSocket-Key: %s\r\nSec-WebSocket-Version: 13\r\n"
                   "Authorization: Bearer %s\r\n\r\n" % (port, key, token)).encode())
        head = b""
        while b"\r\n\r\n" not in head:
            c = s.recv(4096)
            if not c: raise ConnectionError("closed during upgrade")
            head += c
        head, _, self.buf = head.partition(b"\r\n\r\n")
        if b" 101 " not in head.split(b"\r\n")[0]:
            raise ConnectionError("upgrade refused: %r" % head.split(b"\r\n")[0])
        s.settimeout(None)
        self.s = s

    def call(self, rid, tag, payload):
        self.send_raw({"_tag": "Request", "id": rid, "tag": tag, "payload": payload, "headers": []})


def shell_snapshot(c, timeout=20):
    c.call("shell", "orchestration.subscribeShell", {"requestCompletionMarker": True})
    end, snap = time.time() + timeout, None
    while time.time() < end:
        m = c.recv(1.0)
        if m is None: continue
        if m.get("_tag") == "Chunk":
            c.send_raw({"_tag": "Ack", "requestId": m["requestId"]})
            for v in m["values"]:
                if v.get("kind") == "snapshot" and "resolvedRepositoryIdentityRoots" not in v:
                    snap = v["snapshot"]
                if v.get("kind") == "synchronized" or (isinstance(v, dict) and v.get("_tag") == "synchronized"):
                    return snap
            if snap is not None and snap.get("projects"): return snap
    return snap


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, required=True)
    ap.add_argument("--token-file", required=True)
    sub = ap.add_subparsers(dest="mode", required=True)
    p = sub.add_parser("launch"); p.add_argument("text"); p.add_argument("--model", default="claude-sonnet-5-5")
    p.add_argument("--instance", default="claudeAgent"); p.add_argument("--title", default="e2e")
    sub.add_parser("threads")
    p = sub.add_parser("count"); p.add_argument("thread_id"); p.add_argument("needle")
    a = ap.parse_args()
    token = open(a.token_file).read().strip()
    c = T3(a.port, token)
    if a.mode == "count":
        c.call("t", "orchestration.subscribeThread", {"threadId": a.thread_id})
        end, n = time.time() + 15, None
        while time.time() < end and n is None:
            m = c.recv(1.0)
            if m and m.get("_tag") == "Chunk":
                c.send_raw({"_tag": "Ack", "requestId": m["requestId"]})
                for v in m["values"]:
                    proj = v.get("projection") if isinstance(v, dict) else None
                    if proj is not None:
                        msgs = [x for x in proj.get("messages", []) if x.get("role") == "user" and a.needle in json.dumps(x.get("text", x))]
                        n = len(msgs)
        print(json.dumps({"user_messages_with_needle": n})); return
    snap = shell_snapshot(c)
    if a.mode == "threads":
        print(json.dumps([{k: t.get(k) for k in ("id", "title", "status", "pendingRuntimeRequest")} for t in snap.get("threads", [])]))
        return
    if not snap or not snap.get("projects"):
        sys.exit("no project in the T3 shell snapshot: %s" % json.dumps(snap)[:300])
    pid = snap["projects"][0].get("id") or snap["projects"][0].get("projectId")
    tid, mid = str(uuid.uuid4()), str(uuid.uuid4())
    c.call("c1", "orchestration.dispatchCommand", {
        "type": "thread.create", "createdBy": "user", "creationSource": "web", "commandId": str(uuid.uuid4()),
        "threadId": tid, "projectId": pid, "title": a.title,
        "modelSelection": {"instanceId": a.instance, "model": a.model},
        "runtimeMode": "approval-required", "interactionMode": "default", "branch": None, "worktreePath": None})
    def result(rid):
        end = time.time() + 20
        while time.time() < end:
            m = c.recv(1.0)
            if m and m.get("_tag") == "Chunk": c.send_raw({"_tag": "Ack", "requestId": m["requestId"]})
            if m and m.get("_tag") == "Exit" and m.get("requestId") == rid: return m["exit"]
    r = result("c1")
    if not r or r.get("_tag") != "Success": sys.exit("thread.create failed: %s" % json.dumps(r)[:400])
    c.call("c2", "orchestration.dispatchCommand", {
        "type": "message.dispatch", "createdBy": "user", "creationSource": "web", "commandId": str(uuid.uuid4()),
        "threadId": tid, "messageId": mid, "text": a.text, "attachments": [], "titleSeed": a.title,
        "dispatchMode": {"type": "start_immediately"}})
    r = result("c2")
    if not r or r.get("_tag") != "Success": sys.exit("message.dispatch failed: %s" % json.dumps(r)[:400])
    print(json.dumps({"threadId": tid, "projectId": pid}))

main()
