#!/usr/bin/env python3
"""Fake FCM + OAuth endpoint with fault injection (TLS, asyncio, stdlib only).

Safe against a LOCAL test relay only: it binds 127.0.0.1 and serves one
throwaway certificate. The relay under test reaches it because
`fcm.googleapis.com` is mapped to 127.0.0.1 in /etc/hosts (see README.md).

  fakefcm.py --certs DIR [--port 443] [--control 9099]

Control (plain HTTP on 127.0.0.1:CONTROL):
  POST /mode  {"token": MODE, "send": MODE}   MODE is one of
     ok | 401 | 429 | 500 | unregistered | invalid_argument | redirect |
     stall_head | stall_body | slow_body | huge | garbage | huge_expires
  GET  /stats  -> counters and the last 50 /messages:send bodies
  POST /reset
"""
import argparse, asyncio, json, ssl, time

STATE = {"mode": {"token": "ok", "send": "ok"}, "token_requests": 0,
         "sends": 0, "bodies": [], "auths": [], "assertions": [], "open": 0, "max_open": 0}


async def read_request(r):
    head = await r.readuntil(b"\r\n\r\n")
    lines = head.decode("latin1").split("\r\n")
    meth, path, _ = lines[0].split(" ", 2)
    hdrs = {}
    for l in lines[1:]:
        if ":" in l:
            k, v = l.split(":", 1)
            hdrs[k.strip().lower()] = v.strip()
    n = int(hdrs.get("content-length", "0"))
    body = await r.readexactly(n) if n else b""
    return meth, path, hdrs, body


def resp(status, body, extra=""):
    b = body.encode() if isinstance(body, str) else body
    return (f"HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {len(b)}\r\n"
            f"Connection: close\r\n{extra}\r\n").encode() + b


async def faulty(w, mode, ok_body):
    if mode == "ok":
        w.write(resp(200, ok_body))
    elif mode == "401":
        w.write(resp(401, '{"error":{"status":"UNAUTHENTICATED","message":"SECRET-LEAK-CANARY-401"}}'))
    elif mode == "429":
        w.write(resp(429, '{"error":{"status":"QUOTA_EXCEEDED","message":"SECRET-LEAK-CANARY-429"}}'))
    elif mode == "500":
        w.write(resp(500, '{"error":{"status":"INTERNAL","message":"SECRET-LEAK-CANARY-500"}}'))
    elif mode == "unregistered":
        w.write(resp(404, '{"error":{"code":404,"status":"NOT_FOUND","details":[{"errorCode":"UNREGISTERED"}]}}'))
    elif mode == "invalid_argument":
        w.write(resp(400, '{"error":{"status":"INVALID_ARGUMENT","message":"SECRET-LEAK-CANARY-400"}}'))
    elif mode == "redirect":
        w.write(resp(302, "", "Location: https://127.0.0.1:1/leak\r\n"))
    elif mode == "garbage":
        w.write(b"\x00\x01garbage\r\n\r\n")
    elif mode == "stall_head":
        await asyncio.sleep(3600)
    elif mode == "stall_body":
        w.write(b"HTTP/1.1 200 OK\r\nContent-Length: 100000\r\nConnection: close\r\n\r\n")
        await w.drain()
        await asyncio.sleep(3600)
    elif mode == "slow_body":
        w.write(b"HTTP/1.1 200 OK\r\nContent-Length: 100000\r\nConnection: close\r\n\r\n")
        for _ in range(100000):
            w.write(b"x"); await w.drain(); await asyncio.sleep(0.05)
    elif mode == "huge":
        w.write(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n")
        chunk = b"A" * 65536
        for _ in range(4096):  # up to 256 MiB, stops when the client hangs up
            w.write(chunk); await w.drain()
    elif mode == "huge_expires":
        w.write(resp(200, '{"access_token":"tok","expires_in":18446744073709551615}'))
    else:
        w.write(resp(500, "{}"))
    await w.drain()


async def handle(r, w):
    STATE["open"] += 1
    STATE["max_open"] = max(STATE["max_open"], STATE["open"])
    try:
        meth, path, hdrs, body = await read_request(r)
        if path == "/token":
            STATE["token_requests"] += 1
            STATE["assertions"] = (STATE["assertions"] + [body.decode("latin1")[-60:]])[-5:]
            await faulty(w, STATE["mode"]["token"],
                         json.dumps({"access_token": "ya29.FAKE-OAUTH-TOKEN", "expires_in": 3600,
                                     "token_type": "Bearer"}))
        elif path.endswith("/messages:send"):
            STATE["sends"] += 1
            STATE["bodies"] = (STATE["bodies"] + [body.decode("utf8", "replace")])[-50:]
            STATE["auths"] = (STATE["auths"] + [hdrs.get("authorization", "")])[-50:]
            await faulty(w, STATE["mode"]["send"], '{"name":"projects/p/messages/1"}')
        else:
            w.write(resp(404, "{}"))
        await w.drain()
    except Exception:
        pass
    finally:
        STATE["open"] -= 1
        try:
            w.close()
        except Exception:
            pass


async def control(r, w):
    try:
        meth, path, hdrs, body = await read_request(r)
        if path == "/mode":
            STATE["mode"].update(json.loads(body))
            out = STATE["mode"]
        elif path == "/reset":
            STATE.update(token_requests=0, sends=0, bodies=[], auths=[], assertions=[], max_open=0)
            out = {"ok": True}
        else:
            out = {k: v for k, v in STATE.items()}
        w.write(resp(200, json.dumps(out)))
        await w.drain()
    except Exception:
        pass
    finally:
        w.close()


async def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--certs", required=True)
    ap.add_argument("--port", type=int, default=443)
    ap.add_argument("--control", type=int, default=9099)
    a = ap.parse_args()
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.load_cert_chain(f"{a.certs}/srv.pem", f"{a.certs}/srv.key")
    s1 = await asyncio.start_server(handle, "127.0.0.1", a.port, ssl=ctx, backlog=4096)
    s2 = await asyncio.start_server(control, "127.0.0.1", a.control)
    print("fakefcm up", flush=True)
    await asyncio.gather(s1.serve_forever(), s2.serve_forever())

asyncio.run(main())
