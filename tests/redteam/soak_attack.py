#!/usr/bin/env python3
"""
Mixed hostile load for the long soak (task: "sample RSS every 10 s for at
least one hour under attack"). Three threads run concurrently against a
local test daemon:

1. Pre-auth churn from many distinct 127.0.0.x source addresses: opens a
   TLS connection, finishes the WS upgrade, sends no `hello`, holds it
   until the daemon's hello timeout (or a close), and repeats. Exercises
   the per-source cap (2) and the daemon-wide pre-auth pool (32).
2. An authenticated client flooding `request.answer` and `ping` commands
   (hostile but token-holding peer).
3. Hook-endpoint abuse: idle connections plus malformed-body POSTs against
   the loopback hook port.

Usage:
    python3 -I soak_attack.py --dir <acd config dir> --port <ws port> \
        --hook-port <hook port> --duration 3600

Safety: every socket is bound to 127.0.0.0/8 and connects only to
127.0.0.1 at the given ports. Reads the client token from `<dir>/client.token`
only to drive the authenticated-flood thread, and never prints it.
"""
import argparse
import json
import os
import socket
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import acdws  # noqa: E402

STOP = threading.Event()


def preauth_churn_worker(port: int, stats: dict):
    n = 0
    next_octet = 2
    while not STOP.is_set():
        src = f"127.0.0.{next_octet}"
        next_octet = 2 if next_octet >= 250 else next_octet + 1
        try:
            raw = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            raw.bind((src, 0))
            raw.settimeout(3.0)
            raw.connect(("127.0.0.1", port))
            s = acdws.tls_wrap(raw)
            s.sendall(acdws.ws_handshake_request("127.0.0.1", port))
            acdws.read_http_response_headers(s, timeout=3.0)
            # Deliberately send no hello; hold until the daemon closes us
            # (hello_timeout) or a short grace period, then drop.
            time.sleep(0.3)
            s.close()
            n += 1
        except OSError:
            n += 1
        stats["preauth_churn"] = n


def authed_flood_worker(port: int, token: str, stats: dict):
    """Floods pings and bogus answers from an authenticated-but-hostile
    peer. Reuses a *fixed* command id per connection on purpose: acd's
    idempotency table (crates/acd-core/src/idem.rs, 16,384 entries,
    daemon-wide) caches a Done result per (client.id, id) for 15 minutes,
    and the fast path for a repeated id is a HashMap lookup, not a new
    reservation. Hammering with ever-fresh ids instead would fill that
    whole daemon-wide table in seconds and starve every other client's
    request.answer with `bad_request: "too many recent commands"` -- a
    real, reproduced finding (see docs/research/2026-11-04-red-team-acd.md,
    "idempotency table exhaustion"), but conflating it into the hour-long
    soak would make every other soak measurement meaningless. The
    dedicated, isolated reproducer is idem_table_flood.py."""
    n = 0
    while not STOP.is_set():
        try:
            ws = acdws.WS.connect("127.0.0.1", port, use_tls=True)
            ws.send_json(acdws.hello_msg(token))
            ws.recv_json(timeout=3.0)  # welcome
            ws.recv_json(timeout=3.0)  # snapshot
            ping_id = acdws.new_id()
            answer_id = acdws.new_id()
            deadline = time.time() + 2.0
            while time.time() < deadline and not STOP.is_set():
                ws.send_json({"id": ping_id, "type": "ping"})
                ws.send_json({
                    "id": answer_id,
                    "type": "request.answer",
                    "request_id": acdws.new_id(),
                    "option_id": "yes",
                })
                n += 2
            ws.close()
        except OSError:
            pass
        stats["authed_flood"] = n


def hook_abuse_worker(port: int, stats: dict):
    n = 0
    idle = []
    while not STOP.is_set():
        try:
            # Keep up to 20 idle connections open, cycling them, plus send
            # one malformed-body POST per loop.
            if len(idle) < 20:
                s = socket.create_connection(("127.0.0.1", port), timeout=3.0)
                idle.append(s)
            else:
                idle.pop(0).close()
            bad = socket.create_connection(("127.0.0.1", port), timeout=3.0)
            body = b"{not json"
            req = (
                f"POST /hook HTTP/1.1\r\nHost: 127.0.0.1\r\n"
                f"Content-Type: application/json\r\nContent-Length: {len(body)}\r\n"
                f"Connection: close\r\nX-Acd-Token: wrong-token\r\n\r\n"
            ).encode() + body
            bad.sendall(req)
            try:
                bad.settimeout(2.0)
                bad.recv(4096)
            except OSError:
                pass
            bad.close()
            n += 1
        except OSError:
            n += 1
        stats["hook_abuse"] = n
        time.sleep(0.05)
    for s in idle:
        try:
            s.close()
        except OSError:
            pass


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dir", required=True)
    ap.add_argument("--port", type=int, required=True)
    ap.add_argument("--hook-port", type=int, required=True)
    ap.add_argument("--duration", type=float, default=3600.0)
    ap.add_argument("--stats-out", default=None)
    args = ap.parse_args()

    token = open(os.path.join(args.dir, "client.token")).read().strip()

    stats = {"preauth_churn": 0, "authed_flood": 0, "hook_abuse": 0}
    threads = [
        threading.Thread(target=preauth_churn_worker, args=(args.port, stats), daemon=True),
        threading.Thread(target=preauth_churn_worker, args=(args.port, stats), daemon=True),
        threading.Thread(target=authed_flood_worker, args=(args.port, token, stats), daemon=True),
        threading.Thread(target=hook_abuse_worker, args=(args.hook_port, stats), daemon=True),
    ]
    for t in threads:
        t.start()

    start = time.time()
    try:
        while time.time() - start < args.duration:
            time.sleep(10)
            print(f"[{int(time.time() - start)}s] {stats}", file=sys.stderr)
    except KeyboardInterrupt:
        pass
    finally:
        STOP.set()
        for t in threads:
            t.join(timeout=5)
    if args.stats_out:
        with open(args.stats_out, "w") as f:
            json.dump(stats, f)
    print("final stats:", stats, file=sys.stderr)


if __name__ == "__main__":
    main()
