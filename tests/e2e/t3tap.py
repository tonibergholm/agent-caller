#!/usr/bin/env python3
"""Recording TCP tap between the scratch acd and the SCRATCH T3 server (stdlib; harness use only).

  t3tap.py --listen 52958 --target 52957 --out /tmp/ace2e/t3tap.jsonl

Forwards bytes unchanged.  After the HTTP upgrade it parses WebSocket frames and records, as JSON lines, only
(a) client frames whose payload is a `runtime-request.respond` dispatchCommand and (b) the server `Exit` frame for
that request id.  The upgrade request (which carries the bearer token) is never recorded; frames over 1 MiB are
forwarded but not inspected.  Binds 127.0.0.1 only and refuses the real T3/acd ports.
"""
import argparse, json, socket, sys, threading, time

LIMIT = 1 << 20


class Parser:
    def __init__(self, masked):
        self.masked, self.buf, self.need = masked, b"", None

    def feed(self, data):
        """Yield (opcode, fin, payload|None) per complete frame."""
        self.buf += data
        while True:
            b = self.buf
            if len(b) < 2: return
            n, i = b[1] & 0x7F, 2
            if n == 126:
                if len(b) < 4: return
                n, i = int.from_bytes(b[2:4], "big"), 4
            elif n == 127:
                if len(b) < 10: return
                n, i = int.from_bytes(b[2:10], "big"), 10
            mask = b""
            if b[1] & 0x80:
                if len(b) < i + 4: return
                mask, i = b[i:i + 4], i + 4
            if n > LIMIT:
                # skip the body without keeping it
                if len(b) - i >= n:
                    self.buf = b[i + n:]; yield (b[0] & 0xF, bool(b[0] & 0x80), None); continue
                self.need_skip = n - (len(b) - i)
                self.buf = b""
                yield (b[0] & 0xF, bool(b[0] & 0x80), None)
                self._skip = self.need_skip
                return
            if len(b) < i + n: return
            p = b[i:i + n]
            if mask: p = bytes(x ^ mask[k % 4] for k, x in enumerate(p))
            self.buf = b[i + n:]
            yield (b[0] & 0xF, bool(b[0] & 0x80), p)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--listen", type=int, required=True); ap.add_argument("--target", type=int, required=True)
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    for p in (a.listen, a.target):
        if p in (47800, 47801, 3773): sys.exit("port %d belongs to the user's real services" % p)
    out_lock, seen = threading.Lock(), set()

    def rec(direction, obj):
        line = json.dumps({"t": time.time(), "dir": direction, "msg": obj})
        with out_lock:
            fd = open(a.out, "a"); fd.write(line + "\n"); fd.close()

    def pump(src, dst, direction, up):
        masked = direction == "acd->t3"
        parser, upgraded, head = Parser(masked), False, b""
        skip = 0
        try:
            while True:
                d = src.recv(65536)
                if not d: break
                dst.sendall(d)
                if not upgraded:
                    head += d
                    if b"\r\n\r\n" in head:
                        upgraded, d = True, head.split(b"\r\n\r\n", 1)[1]
                    else: continue
                if skip:
                    if len(d) <= skip: skip -= len(d); continue
                    d, skip = d[skip:], 0
                for op, fin, p in parser.feed(d):
                    if p is None or op != 1 or not fin: continue
                    if masked and b"runtime-request.respond" in p:
                        try: m = json.loads(p)
                        except ValueError: continue
                        seen.add(m.get("id")); rec(direction, m)
                    elif not masked and b'"Exit"' in p:
                        try: m = json.loads(p)
                        except ValueError: continue
                        if m.get("requestId") in seen: rec(direction, m)
                skip = getattr(parser, "_skip", 0); parser._skip = 0
        except OSError:
            pass
        finally:
            for s in (src, dst):
                try: s.shutdown(socket.SHUT_RDWR)
                except OSError: pass

    ls = socket.socket(); ls.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    ls.bind(("127.0.0.1", a.listen)); ls.listen(16)
    while True:
        c, _ = ls.accept()
        try: u = socket.create_connection(("127.0.0.1", a.target), timeout=10); u.settimeout(None)
        except OSError: c.close(); continue
        threading.Thread(target=pump, args=(c, u, "acd->t3", True), daemon=True).start()
        threading.Thread(target=pump, args=(u, c, "t3->acd", False), daemon=True).start()


main()
