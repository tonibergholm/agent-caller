#!/usr/bin/env python3
"""Stdlib-only acd/1 client for the live e2e harness.

Connects to <config_dir>/acd.sock (plain WebSocket over a Unix socket), says
hello with <config_dir>/client.token, and offers a few modes.  The token is
read from the file and never printed or logged.

  inbox.py [-d DIR] log [--out FILE] [--duration S]
  inbox.py [-d DIR] list
  inbox.py [-d DIR] wait-open [--agent ID] [--timeout S] [--ready]
  inbox.py [-d DIR] answer REQUEST_ID (OPTION_ID | --text TEXT) [--id UUID]
  inbox.py [-d DIR] race REQUEST_ID OPTION_A OPTION_B
  inbox.py [-d DIR] send AGENT_ID TEXT [--id UUID] [--repeat N] [--client-id ID]

DIR defaults to $ACD_CONFIG_DIR.  Every message in either direction is also
appended, with a wall-clock timestamp, to $INBOX_LOG (default: none) as one JSON
object per line: {"t": <epoch seconds>, "iso": ..., "dir": "in"|"out", "who":
<client name>, "msg": {...}}.
"""
import argparse, base64, hashlib, json, os, select, socket, struct, sys, threading
import time, uuid
from datetime import datetime, timezone

GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"


def now():
    t = time.time()
    return t, datetime.fromtimestamp(t, timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%f")[:-3] + "Z"


class Conn:
    def __init__(self, cfg_dir, name="e2e-inbox", client_id=None, log=None, resume=None):
        self.name = name
        self.client_id = client_id or str(uuid.uuid4())
        self.log = log or os.environ.get("INBOX_LOG")
        self._lock = threading.Lock()
        self.buf = b""
        token = open(os.path.join(cfg_dir, "client.token")).read().strip()
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.connect(os.path.join(cfg_dir, "acd.sock"))
        key = base64.b64encode(os.urandom(16)).decode()
        s.sendall((
            "GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\n"
            "Connection: Upgrade\r\nSec-WebSocket-Key: %s\r\n"
            "Sec-WebSocket-Version: 13\r\n\r\n" % key).encode())
        head = b""
        while b"\r\n\r\n" not in head:
            c = s.recv(4096)
            if not c:
                raise ConnectionError("closed during upgrade")
            head += c
        head, _, self.buf = head.partition(b"\r\n\r\n")
        if b" 101 " not in head.split(b"\r\n")[0]:
            raise ConnectionError("upgrade refused: %r" % head.split(b"\r\n")[0])
        want = base64.b64encode(hashlib.sha1((key + GUID).encode()).digest())
        if want not in head:
            raise ConnectionError("bad Sec-WebSocket-Accept")
        self.s = s
        hello = {"type": "hello", "protocol": "acd/1", "token": token,
                 "client": {"id": self.client_id, "name": name, "version": "e2e"}}
        if resume:
            hello["resume"] = resume
        self.send_raw(hello, redact=True)

    # --- framing -------------------------------------------------------
    def _record(self, direction, msg):
        if not self.log:
            return
        t, iso = now()
        line = json.dumps({"t": t, "iso": iso, "dir": direction, "who": self.name, "msg": msg})
        with self._lock:
            fd = os.open(self.log, os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o600)
            try:
                os.write(fd, (line + "\n").encode())
            finally:
                os.close(fd)

    def send_raw(self, msg, redact=False):
        payload = json.dumps(msg).encode()
        mask = os.urandom(4)
        n = len(payload)
        hdr = bytes([0x81])
        if n < 126:
            hdr += bytes([0x80 | n])
        elif n < 65536:
            hdr += bytes([0x80 | 126]) + struct.pack(">H", n)
        else:
            hdr += bytes([0x80 | 127]) + struct.pack(">Q", n)
        body = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
        self.s.sendall(hdr + mask + body)
        shown = dict(msg)
        if redact and "token" in shown:
            shown["token"] = "<redacted>"
        self._record("out", shown)

    def _need(self, n, timeout):
        end = None if timeout is None else time.time() + timeout
        while len(self.buf) < n:
            left = None if end is None else max(0, end - time.time())
            r, _, _ = select.select([self.s], [], [], left)
            if not r:
                return False
            c = self.s.recv(65536)
            if not c:
                raise ConnectionError("closed")
            self.buf += c
        return True

    def recv(self, timeout=None):
        """Next JSON message, or None on timeout.  Raises ConnectionError on close."""
        while True:
            if not self._need(2, timeout):
                return None
            b0, b1 = self.buf[0], self.buf[1]
            ln, off = b1 & 0x7F, 2
            if ln == 126:
                if not self._need(4, timeout): return None
                ln, off = struct.unpack(">H", self.buf[2:4])[0], 4
            elif ln == 127:
                if not self._need(10, timeout): return None
                ln, off = struct.unpack(">Q", self.buf[2:10])[0], 10
            if not self._need(off + ln, timeout):
                return None
            data, self.buf = self.buf[off:off + ln], self.buf[off + ln:]
            op = b0 & 0x0F
            if op == 0x8:
                code = struct.unpack(">H", data[:2])[0] if len(data) >= 2 else None
                self._record("in", {"type": "_close", "code": code})
                raise ConnectionError("closed by server (code %s)" % code)
            if op == 0x9:  # ping -> pong (masked)
                mask = os.urandom(4)
                self.s.sendall(bytes([0x8A, 0x80 | len(data)]) + mask +
                               bytes(b ^ mask[i % 4] for i, b in enumerate(data)))
                continue
            if op != 0x1:
                continue
            msg = json.loads(data.decode())
            self._record("in", msg)
            return msg

    def close(self):
        try:
            self.s.close()
        except OSError:
            pass


class Mirror:
    """Tracks agents and requests from snapshot / agent.* / request.* events."""

    def __init__(self):
        self.agents, self.epoch, self.seq = {}, None, None

    def apply(self, m):
        t = m.get("type")
        if t == "snapshot":
            self.agents = {a["id"]: a for a in m["agents"]}
            self.epoch, self.seq = m["epoch"], m["seq"]
        elif t == "agent.updated":
            self.agents[m["agent"]["id"]] = m["agent"]
        elif t == "agent.removed":
            self.agents.pop(m["agent_id"], None)
        if t in ("agent.updated", "agent.removed", "request.opened", "request.resolved"):
            self.epoch, self.seq = m.get("epoch", self.epoch), m.get("seq", self.seq)

    def open_requests(self):
        return [a["pending"] for a in self.agents.values() if a.get("pending")]


def handshake(c, timeout=10):
    """Reads until the first snapshot; returns (mirror, welcome)."""
    mir, welcome, end = Mirror(), None, time.time() + timeout
    while time.time() < end:
        m = c.recv(max(0.1, end - time.time()))
        if m is None:
            break
        if m.get("type") == "error":
            raise SystemExit("server error: %s" % json.dumps(m))
        if m.get("type") == "welcome":
            welcome = m
        mir.apply(m)
        if m.get("type") == "snapshot":
            return mir, welcome
    raise SystemExit("no snapshot within %ss" % timeout)


def wait_result(c, mir, cid, timeout=15):
    end = time.time() + timeout
    while time.time() < end:
        m = c.recv(max(0.05, end - time.time()))
        if m is None:
            continue
        mir.apply(m)
        if m.get("type") == "result" and m.get("id") == cid:
            return m
    return {"type": "result", "id": cid, "ok": False, "error": {"code": "client_timeout", "message": "no result"}}


def summary(a):
    p = a.get("pending")
    s = "%-10s %-7s %-9s %-8s can_send=%s title=%r" % (
        a["id"][:10], a["kind"], a["state"], a["source"], a.get("can_send"), a.get("title"))
    if p:
        s += "\n    request %s kind=%s status=%s path=%s opts=%s title=%r" % (
            p["id"], p["kind"], p["status"], p["answer_path"],
            [o["id"] for o in p["options"]], p["title"])
    return s


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("-d", "--dir", default=os.environ.get("ACD_CONFIG_DIR"))
    ap.add_argument("--name", default="e2e-inbox")
    sub = ap.add_subparsers(dest="mode", required=True)
    p = sub.add_parser("log"); p.add_argument("--out"); p.add_argument("--duration", type=float)
    sub.add_parser("list")
    p = sub.add_parser("wait-open"); p.add_argument("--agent"); p.add_argument("--timeout", type=float, default=30)
    p.add_argument("--ready", action="store_true", help="only a request whose answer_path is hook/keys/t3")
    p = sub.add_parser("answer"); p.add_argument("request_id"); p.add_argument("option_id", nargs="?")
    p.add_argument("--text"); p.add_argument("--id"); p.add_argument("--client-id")
    p = sub.add_parser("race"); p.add_argument("request_id"); p.add_argument("a"); p.add_argument("b")
    p = sub.add_parser("send"); p.add_argument("agent_id"); p.add_argument("text")
    p.add_argument("--id"); p.add_argument("--repeat", type=int, default=1); p.add_argument("--client-id")
    args = ap.parse_args()
    if not args.dir:
        sys.exit("set ACD_CONFIG_DIR or pass -d")

    if args.mode == "log":
        if args.out:
            os.environ["INBOX_LOG"] = args.out
        c = Conn(args.dir, name=args.name)
        end = time.time() + args.duration if args.duration else None
        try:
            while end is None or time.time() < end:
                c.recv(1.0)
        except (ConnectionError, KeyboardInterrupt) as e:
            print("log ended: %s" % e, file=sys.stderr)
        return

    if args.mode == "wait-open":
        c = Conn(args.dir, name=args.name)
        mir, _ = handshake(c)
        end = time.time() + args.timeout
        while True:
            for r in mir.open_requests():
                if args.agent and r["agent_id"] != args.agent:
                    continue
                if r["status"] == "open" and (not args.ready or r["answer_path"] in ("hook", "keys", "t3")):
                    print(json.dumps(r)); return
            if time.time() > end:
                sys.exit("no open request within %ss" % args.timeout)
            m = c.recv(min(0.5, max(0.05, end - time.time())))
            if m: mir.apply(m)

    if args.mode == "list":
        c = Conn(args.dir, name=args.name)
        mir, w = handshake(c)
        print("epoch %s seq %s" % (mir.epoch, mir.seq))
        for a in mir.agents.values():
            print(summary(a))
        return

    if args.mode == "answer":
        c = Conn(args.dir, name=args.name, client_id=args.client_id)
        mir, _ = handshake(c)
        cid = args.id or str(uuid.uuid4())
        msg = {"type": "request.answer", "id": cid, "request_id": args.request_id}
        if args.text is not None: msg["text"] = args.text
        else: msg["option_id"] = args.option_id
        t0 = time.time(); c.send_raw(msg)
        res = wait_result(c, mir, cid)
        res["_ms"] = round((time.time() - t0) * 1000)
        print(json.dumps(res)); return

    if args.mode == "race":
        conns = []
        for i, opt in enumerate((args.a, args.b)):
            c = Conn(args.dir, name="%s-%d" % (args.name, i)); handshake(c)
            conns.append((c, opt, str(uuid.uuid4())))
        barrier, out = threading.Barrier(2), {}
        def go(i, c, opt, cid):
            mir = Mirror()
            barrier.wait()
            t0 = time.time()
            c.send_raw({"type": "request.answer", "id": cid, "request_id": args.request_id, "option_id": opt})
            r = wait_result(c, mir, cid); r["_ms"] = round((time.time() - t0) * 1000); r["_option"] = opt
            out[i] = r
        ts = [threading.Thread(target=go, args=(i, *x)) for i, x in enumerate(conns)]
        [t.start() for t in ts]; [t.join() for t in ts]
        print(json.dumps([out[0], out[1]])); return

    if args.mode == "send":
        c = Conn(args.dir, name=args.name, client_id=args.client_id)
        mir, _ = handshake(c)
        cid = args.id or str(uuid.uuid4())
        results = []
        for _ in range(args.repeat):
            t0 = time.time()
            c.send_raw({"type": "agent.send", "id": cid, "agent_id": args.agent_id, "text": args.text})
            r = wait_result(c, mir, cid); r["_ms"] = round((time.time() - t0) * 1000)
            results.append(r)
        print(json.dumps({"command_id": cid, "client_id": c.client_id, "results": results})); return


if __name__ == "__main__":
    main()
