#!/usr/bin/env python3
"""
WebSocket frame-level abuse against a running acd/1 TLS listener:
oversized frames, fragment bombs, malformed/invalid frames, ping floods,
and control-character/unicode payloads in `hello`. Each case is run
against a fresh connection so one failure does not poison the next.

Usage:
    python3 -I frame_abuse.py --dir <acd config dir> --port <ws port>

Safety: connects only to 127.0.0.1:<port>. Reads the client token from
`<dir>/client.token` to drive the post-auth cases, never prints it.
"""
import argparse
import json
import os
import socket
import struct
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import acdws  # noqa: E402

RESULTS = []


def record(name, outcome, detail=""):
    RESULTS.append((name, outcome, detail))
    print(f"[{outcome:>12}] {name}  {detail}")


def fresh(port):
    return acdws.WS.connect("127.0.0.1", port, use_tls=True)


def authed(port, token):
    ws = fresh(port)
    ws.send_json(acdws.hello_msg(token))
    ws.recv_json(timeout=5)  # welcome
    ws.recv_json(timeout=5)  # snapshot
    return ws


def try_close_or_error(ws, name, timeout=5):
    try:
        f = ws.recv(timeout=timeout)
        if f.opcode == acdws.OPCODE_CLOSE:
            code = struct.unpack("!H", f.payload[:2])[0] if len(f.payload) >= 2 else None
            record(name, "closed", f"code={code}")
        elif f.opcode == acdws.OPCODE_TEXT:
            record(name, "message", f.payload[:200])
        else:
            record(name, "other-frame", f"opcode={f.opcode} len={len(f.payload)}")
    except (ConnectionError, socket.timeout, OSError) as e:
        record(name, "conn-dropped", str(e))
    finally:
        ws.close()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dir", required=True)
    ap.add_argument("--port", type=int, required=True)
    args = ap.parse_args()
    token = open(os.path.join(args.dir, "client.token")).read().strip()
    port = args.port

    # 1. Oversized single text frame pre-auth (2 MiB; max_frame=1MiB).
    ws = fresh(port)
    try:
        ws.sock.sendall(acdws.build_frame(acdws.OPCODE_TEXT, b"x" * (2 * 1024 * 1024)))
    except OSError as e:
        record("oversized_frame_preauth_send", "send-failed", str(e))
    else:
        try_close_or_error(ws, "oversized_frame_preauth")

    # 2. hello with the token, then an oversized authenticated frame.
    ws = authed(port, token)
    big = json.dumps({"id": acdws.new_id(), "type": "ping", "pad": "y" * (2 * 1024 * 1024)})
    try:
        ws.sock.sendall(acdws.build_frame(acdws.OPCODE_TEXT, big.encode()))
    except OSError as e:
        record("oversized_frame_authed_send", "send-failed", str(e))
    else:
        try_close_or_error(ws, "oversized_frame_authed")

    # 3. Fragment bomb: many small continuation frames whose sum exceeds
    # max_frame (1 MiB), none of them individually over the per-frame cap.
    ws = authed(port, token)
    try:
        chunk = b"a" * 4096
        n_chunks = 300  # 300*4096 ~= 1.17 MiB, over the 1 MiB message cap
        ws.sock.sendall(acdws.build_frame(acdws.OPCODE_TEXT, chunk, fin=False))
        for i in range(1, n_chunks):
            fin = i == n_chunks - 1
            ws.sock.sendall(acdws.build_frame(acdws.OPCODE_CONT, chunk, fin=fin))
        try_close_or_error(ws, "fragment_bomb_over_cap")
    except OSError as e:
        record("fragment_bomb_over_cap", "send-failed", str(e))
        ws.close()

    # 4. Many tiny fragments (fragment flood) within the size cap -- does
    # the server choke on fragment *count* rather than total bytes?
    ws = authed(port, token)
    try:
        n = 50000
        ws.sock.sendall(acdws.build_frame(acdws.OPCODE_TEXT, b"{", fin=False))
        t0 = time.time()
        for i in range(n - 2):
            ws.sock.sendall(acdws.build_frame(acdws.OPCODE_CONT, b" ", fin=False))
        tail = json.dumps({"id": acdws.new_id(), "type": "ping"})[1:].encode()
        ws.sock.sendall(acdws.build_frame(acdws.OPCODE_CONT, tail, fin=True))
        dt = time.time() - t0
        try_close_or_error(ws, "fragment_flood_many_tiny", timeout=10)
        record("fragment_flood_many_tiny_timing", "info", f"{n} frames sent in {dt:.2f}s")
    except OSError as e:
        record("fragment_flood_many_tiny", "send-failed", str(e))
        ws.close()

    # 5. Unmasked client frame (RFC 6455 says client MUST mask; server
    # should refuse per spec).
    ws = fresh(port)
    try:
        ws.sock.sendall(acdws.build_frame(acdws.OPCODE_TEXT, b'{"type":"ping"}', masked=False))
        try_close_or_error(ws, "unmasked_client_frame")
    except OSError as e:
        record("unmasked_client_frame", "send-failed", str(e))
        ws.close()

    # 6. RSV bits set without any negotiated extension.
    ws = fresh(port)
    try:
        ws.sock.sendall(acdws.build_frame(acdws.OPCODE_TEXT, b'{"type":"ping"}', rsv=0b011))
        try_close_or_error(ws, "rsv_bits_set")
    except OSError as e:
        record("rsv_bits_set", "send-failed", str(e))
        ws.close()

    # 7. Reserved/invalid opcode.
    ws = fresh(port)
    try:
        ws.sock.sendall(acdws.build_frame(0x3, b"x"))
        try_close_or_error(ws, "reserved_opcode")
    except OSError as e:
        record("reserved_opcode", "send-failed", str(e))
        ws.close()

    # 8. Lone continuation frame (no preceding start).
    ws = fresh(port)
    try:
        ws.sock.sendall(acdws.build_frame(acdws.OPCODE_CONT, b"x"))
        try_close_or_error(ws, "lone_continuation")
    except OSError as e:
        record("lone_continuation", "send-failed", str(e))
        ws.close()

    # 9. Oversized control frame (ping payload > 125 bytes, a protocol
    # violation per RFC 6455 5.5).
    ws = fresh(port)
    try:
        ws.sock.sendall(acdws.build_frame(acdws.OPCODE_PING, b"p" * 1000))
        try_close_or_error(ws, "oversized_ping_control_frame")
    except OSError as e:
        record("oversized_ping_control_frame", "send-failed", str(e))
        ws.close()

    # 10. Invalid UTF-8 in a text frame.
    ws = fresh(port)
    try:
        ws.sock.sendall(acdws.build_frame(acdws.OPCODE_TEXT, b"\xff\xfe\x00{"))
        try_close_or_error(ws, "invalid_utf8_text_frame")
    except OSError as e:
        record("invalid_utf8_text_frame", "send-failed", str(e))
        ws.close()

    # 11. Ping flood post-auth: does the server answer every pong, and
    # does flooding pings let an authed-but-hostile peer dodge rate limits
    # that `ping` commands (application-level) might have?
    ws = authed(port, token)
    try:
        t0 = time.time()
        n = 2000
        for _ in range(n):
            ws.send_ping(b"x")
        pongs = 0
        deadline = time.time() + 5
        ws.sock.settimeout(5)
        try:
            while time.time() < deadline:
                f = ws.recv(timeout=1)
                if f.opcode == acdws.OPCODE_PONG:
                    pongs += 1
        except (socket.timeout, ConnectionError):
            pass
        record("ping_flood_authed", "info", f"sent={n} pongs_seen={pongs} in {time.time()-t0:.2f}s")
    finally:
        ws.close()

    # 12. hello with control/unicode chars in every string field.
    ws = fresh(port)
    evil = "a\u0000b‮c d\x7f" * 5
    msg = {
        "id": evil,
        "type": "hello",
        "protocol": "acd/1",
        "token": evil,
        "client": {"id": evil, "name": evil, "version": evil},
    }
    ws.send_json(msg)
    try_close_or_error(ws, "hello_control_unicode_fields")

    # 13. hello with a 10 MiB client.id (within the 1 MiB frame cap but
    # far over the documented 128-byte id cap).
    ws = fresh(port)
    msg = acdws.hello_msg(token, client_id="x" * 500000)
    try:
        ws.send_json(msg)
        try_close_or_error(ws, "hello_oversized_client_id")
    except OSError as e:
        record("hello_oversized_client_id", "send-failed", str(e))
        ws.close()

    # 14. hello with wrong protocol version.
    ws = fresh(port)
    ws.send_json(acdws.hello_msg(token, protocol="acd/99"))
    try_close_or_error(ws, "hello_wrong_protocol")

    # 15. hello with malformed JSON.
    ws = fresh(port)
    ws.send_text('{"type": "hello", not json')
    try_close_or_error(ws, "hello_malformed_json")

    # 16. hello with wrong-length token (shorter than the 43-byte real one).
    ws = fresh(port)
    ws.send_json(acdws.hello_msg("short"))
    try_close_or_error(ws, "hello_wrong_length_token")

    # 17. hello with right-length, wrong-content token.
    ws = fresh(port)
    wrong = "A" * len(token)
    ws.send_json(acdws.hello_msg(wrong))
    try_close_or_error(ws, "hello_right_length_wrong_token")

    # 18. resume with a huge seq number.
    ws = fresh(port)
    ws.send_json(acdws.hello_msg(token, resume={"epoch": "deadbeef", "seq": 2**62}))
    try_close_or_error(ws, "hello_resume_huge_seq")

    # 19. resume with a negative-ish / wrong-typed seq (protocol says int).
    ws = fresh(port)
    msg = acdws.hello_msg(token, resume={"epoch": "deadbeef", "seq": -1})
    ws.send_json(msg)
    try_close_or_error(ws, "hello_resume_negative_seq")

    print("\n--- summary ---")
    for name, outcome, detail in RESULTS:
        print(f"{outcome:>12}  {name}")


if __name__ == "__main__":
    main()
