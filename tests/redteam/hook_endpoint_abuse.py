#!/usr/bin/env python3
"""
Local attacker (no token, no acd config dir access) against the hook
endpoint: the loopback TCP hook port (any local user can connect --
PROTOCOL.md says so explicitly) and, where reachable, the Unix hook
socket.

Re-checks static finding local-F3 ("hook listeners can be starved by idle
connections") against the fix that followed it
(8947572 "Serve hook listeners with hyper's header_read_timeout",
90a6ad8 "Close hook connections after every response"): opens many idle
connections and times whether they get reaped, instead of blocking a
legitimate request forever.

Also probes: unauthenticated POST, oversized body, slow body (trickle),
malformed JSON, wrong Content-Length, path tricks on the route segments,
and many concurrent connections.

Usage:
    python3 -I hook_endpoint_abuse.py --hook-port <port>

Safety: connects only to 127.0.0.1:<hook-port>. Never touches a config
directory (this script intentionally has none -- it plays the
no-credential local attacker).
"""
import argparse
import socket
import sys
import threading
import time

RESULTS = []


def record(name, outcome, detail=""):
    RESULTS.append((name, outcome, detail))
    print(f"[{outcome:>14}] {name}  {detail}")


def send_and_read(port, req: bytes, read_timeout=5.0, send_timeout=5.0):
    s = socket.create_connection(("127.0.0.1", port), timeout=send_timeout)
    try:
        try:
            s.sendall(req)
        except (BrokenPipeError, ConnectionResetError):
            # The server closed early (e.g. an oversized body rejected
            # before it was all sent). Whatever it already sent back is
            # still worth reading.
            pass
        s.settimeout(read_timeout)
        buf = b""
        try:
            while True:
                chunk = s.recv(65536)
                if not chunk:
                    break
                buf += chunk
        except OSError:
            pass
        return buf
    finally:
        s.close()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--hook-port", type=int, required=True)
    args = ap.parse_args()
    port = args.hook_port

    # 1. No token at all.
    body = b'{"hook_event_name":"Stop","session_id":"x"}'
    req = (
        f"POST /hook/claude/Stop HTTP/1.1\r\nHost: 127.0.0.1\r\n"
        f"Content-Type: application/json\r\nContent-Length: {len(body)}\r\n"
        f"Connection: close\r\n\r\n"
    ).encode() + body
    resp = send_and_read(port, req)
    record("no_token", "info", resp.split(b"\r\n")[0])

    # 2. Wrong token.
    req = (
        f"POST /hook/claude/Stop HTTP/1.1\r\nHost: 127.0.0.1\r\n"
        f"Content-Type: application/json\r\nContent-Length: {len(body)}\r\n"
        f"X-Acd-Token: not-the-real-token\r\nConnection: close\r\n\r\n"
    ).encode() + body
    resp = send_and_read(port, req)
    record("wrong_token", "info", resp.split(b"\r\n")[0])

    # 3. Oversized body (2 MiB; documented cap is 1 MiB).
    big_body = b"{" + b" " * (2 * 1024 * 1024) + b"}"
    req = (
        f"POST /hook/claude/Stop HTTP/1.1\r\nHost: 127.0.0.1\r\n"
        f"Content-Type: application/json\r\nContent-Length: {len(big_body)}\r\n"
        f"X-Acd-Token: x\r\nConnection: close\r\n\r\n"
    ).encode() + big_body
    resp = send_and_read(port, req, read_timeout=8.0)
    record("oversized_body_2mib", "info", resp.split(b"\r\n")[0] if resp else "no-response")

    # 4. Malformed JSON, correct Content-Length.
    bad = b"{not json at all"
    req = (
        f"POST /hook/claude/Stop HTTP/1.1\r\nHost: 127.0.0.1\r\n"
        f"Content-Type: application/json\r\nContent-Length: {len(bad)}\r\n"
        f"X-Acd-Token: x\r\nConnection: close\r\n\r\n"
    ).encode() + bad
    resp = send_and_read(port, req)
    record("malformed_json", "info", resp.split(b"\r\n")[0])

    # 5. Content-Length lies (says more than is ever sent) -- server must
    # time out / close, not hang forever (body_timeout is documented as 5s).
    req = (
        b"POST /hook/claude/Stop HTTP/1.1\r\nHost: 127.0.0.1\r\n"
        b"Content-Type: application/json\r\nContent-Length: 999999\r\n"
        b"X-Acd-Token: x\r\nConnection: close\r\n\r\n"
        b"{\"a\":1}"
    )
    t0 = time.time()
    resp = send_and_read(port, req, read_timeout=10.0)
    dt = time.time() - t0
    record("content_length_lie", "info", f"dt={dt:.2f}s resp={resp[:60]!r}")

    # 6. Slow body: trickle one byte every 0.5s, well under the 5s
    # body_timeout but over many iterations -- is the timeout on *total*
    # time or *per-read idle* time? (local-F3's cousin: "no read timeout"
    # claim was about the header phase; this probes the body phase.)
    s = socket.create_connection(("127.0.0.1", port), timeout=5)
    head = (
        b"POST /hook/claude/Stop HTTP/1.1\r\nHost: 127.0.0.1\r\n"
        b"Content-Type: application/json\r\nContent-Length: 20\r\n"
        b"X-Acd-Token: x\r\nConnection: close\r\n\r\n"
    )
    s.sendall(head)
    t0 = time.time()
    try:
        payload = b'{"hook_event_na":1}'
        for b in payload:
            s.sendall(bytes([b]))
            time.sleep(0.5)
        s.settimeout(5)
        resp = s.recv(4096)
        record("slow_body_trickle", "info", f"dt={time.time()-t0:.2f}s resp={resp[:60]!r}")
    except OSError as e:
        record("slow_body_trickle", "info", f"dt={time.time()-t0:.2f}s dropped: {e}")
    finally:
        s.close()

    # 7. Path tricks on the {agent}/{event} segments.
    for path in ["/hook/../../etc/passwd", "/hook/%2e%2e/x", "/hook//claude/Stop",
                 "/hook/claude/Stop/", "/hook/claude%00/Stop", "/HOOK/claude/Stop"]:
        req = (
            f"POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n"
            f"Content-Type: application/json\r\nContent-Length: {len(body)}\r\n"
            f"X-Acd-Token: x\r\nConnection: close\r\n\r\n"
        ).encode() + body
        resp = send_and_read(port, req)
        record(f"path_trick:{path}", "info", resp.split(b"\r\n")[0] if resp else "no-response")

    # 8. F3 re-check: open N idle connections (just the TCP connect, no
    # bytes at all) and see whether the server eventually reaps them
    # (header_read_timeout) rather than holding the slot forever, by
    # timing when a legitimate POST on a fresh connection gets a reply
    # while the idle ones are still open.
    idle = []
    for _ in range(64):
        try:
            s = socket.create_connection(("127.0.0.1", port), timeout=3)
            idle.append(s)
        except OSError:
            break
    record("f3_idle_connections_opened", "info", f"{len(idle)} opened")
    t0 = time.time()
    resp = send_and_read(port, req, read_timeout=15.0)
    dt = time.time() - t0
    record(
        "f3_legit_request_during_idle_flood",
        "ok" if resp.split(b"\r\n")[0] else "no-response",
        f"dt={dt:.2f}s status={resp.split(b'\\r\\n')[0] if resp else None}",
    )
    # Hold the idle connections for >30s (past hyper's old default header
    # timeout) and see if they get closed server-side.
    time.sleep(32)
    alive = 0
    for s in idle:
        try:
            s.setblocking(False)
            d = s.recv(1)
            if d != b"":
                alive += 1
        except BlockingIOError:
            alive += 1  # still open, no data -- consistent with either state
        except OSError:
            pass  # closed
        finally:
            try:
                s.setblocking(True)
            except OSError:
                pass
    record("f3_idle_connections_alive_after_32s", "info", f"{alive} of {len(idle)} still open")
    for s in idle:
        try:
            s.close()
        except OSError:
            pass

    print("\n--- summary ---")
    for name, outcome, detail in RESULTS:
        print(f"{outcome:>14}  {name}")


if __name__ == "__main__":
    main()
