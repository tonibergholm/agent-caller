#!/usr/bin/env python3
"""
Slowloris-style attacks at each stage of the acd/1 connection lifecycle:

  A. TCP-connect-only, never send a byte (tests `max_handshakes`=16
     concurrency and the accept-level admission pool).
  B. Send one byte of a TLS ClientHello, then stall (never completes the
     handshake; tests the 5s `handshake` timeout in `tls::Limits`).
  C. Complete TLS, send a partial HTTP upgrade request one byte at a time
     with long gaps (tests whatever header-read timeout, if any, applies
     pre-upgrade on the acd/1 listener -- unlike the hook endpoint, this
     is the WebSocket upgrade path).
  D. Complete the WS upgrade, then trickle a `hello` JSON body one byte
     every 0.5s (tests whether a slow-but-progressing pre-hello connection
     is still cut off by the 5s hello_timeout, or whether byte-by-byte
     progress resets it).

For each stage: open many such connections, hold them past the
documented timeout, and time how long until the server actually frees
the slot (observed via a legitimate client's ability to connect).

Usage:
    python3 -I slowloris_handshake.py --dir <acd config dir> --port <ws port>

Safety: connects only to 127.0.0.1:<port>.
"""
import argparse
import os
import socket
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import acdws  # noqa: E402


def try_legit(port, token, timeout=5.0):
    t0 = time.time()
    try:
        ws = acdws.WS.connect("127.0.0.1", port, use_tls=True, timeout=timeout)
        ws.send_json(acdws.hello_msg(token))
        ws.recv_json(timeout=timeout)
        ws.close()
        return True, time.time() - t0
    except Exception as e:  # noqa: BLE001
        return False, time.time() - t0


def stage_a_tcp_only(port, n, hold_s):
    socks = []
    for _ in range(n):
        try:
            s = socket.create_connection(("127.0.0.1", port), timeout=3)
            socks.append(s)
        except OSError:
            break
    time.sleep(hold_s)
    return socks


def stage_b_partial_tls(port, n, hold_s):
    socks = []
    for _ in range(n):
        try:
            s = socket.create_connection(("127.0.0.1", port), timeout=3)
            # One byte of a TLS record header, never the rest.
            s.sendall(b"\x16")
            socks.append(s)
        except OSError:
            break
    time.sleep(hold_s)
    return socks


def stage_c_partial_http_after_tls(port, n, hold_s):
    socks = []
    for _ in range(n):
        try:
            raw = socket.create_connection(("127.0.0.1", port), timeout=3)
            s = acdws.tls_wrap(raw)
            s.sendall(b"GET / HTTP/1.1\r\nHost: x\r\n")  # never finishes the headers
            socks.append(s)
        except OSError:
            break
    time.sleep(hold_s)
    return socks


def stage_d_trickle_hello(port, n, hold_s):
    socks = []
    for _ in range(n):
        try:
            raw = socket.create_connection(("127.0.0.1", port), timeout=3)
            s = acdws.tls_wrap(raw)
            s.sendall(acdws.ws_handshake_request("127.0.0.1", port))
            acdws.read_http_response_headers(s, timeout=3)
            socks.append(s)
        except OSError:
            break
    deadline = time.time() + hold_s
    partial = b'{"type":"hello","protocol":"acd/1","token":"x"'
    frame = acdws.build_frame(acdws.OPCODE_TEXT, partial, fin=False)
    for s in socks:
        try:
            s.sendall(frame)
        except OSError:
            pass
    while time.time() < deadline:
        time.sleep(0.5)
    return socks


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dir", required=True)
    ap.add_argument("--port", type=int, required=True)
    ap.add_argument("--n", type=int, default=20)
    ap.add_argument("--hold", type=float, default=8.0)
    args = ap.parse_args()
    token = open(os.path.join(args.dir, "client.token")).read().strip()

    stages = {
        "A_tcp_only": stage_a_tcp_only,
        "B_partial_tls": stage_b_partial_tls,
        "C_partial_http_after_tls": stage_c_partial_http_after_tls,
        "D_trickle_hello": stage_d_trickle_hello,
    }
    for name, fn in stages.items():
        ok0, dt0 = try_legit(args.port, token)
        print(f"{name}: baseline legit connect ok={ok0} dt={dt0:.2f}s")
        socks = fn(args.port, args.n, args.hold)
        print(f"{name}: held {len(socks)} connections for {args.hold}s")
        ok1, dt1 = try_legit(args.port, token, timeout=8.0)
        print(f"{name}: during-hold legit connect ok={ok1} dt={dt1:.2f}s")
        for s in socks:
            try:
                s.close()
            except OSError:
                pass
        time.sleep(1)
        ok2, dt2 = try_legit(args.port, token)
        print(f"{name}: after-release legit connect ok={ok2} dt={dt2:.2f}s\n")


if __name__ == "__main__":
    main()
