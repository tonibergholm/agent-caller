"""Shared helpers for the relay red-team scripts (stdlib only).
Safe against a LOCAL test relay (see README.md); never point at a real one."""
import json, os, socket, time, urllib.request

RELAY = ("127.0.0.1", int(os.environ.get("RELAY_PORT", "8787")))
CONTROL = "http://127.0.0.1:%s" % os.environ.get("FCM_CONTROL", "9099")
APP = "com.example.agentcaller"


def conn(src=None, timeout=10):
    s = socket.socket()
    s.settimeout(timeout)
    if src:
        s.bind((src, 0))
    s.connect(RELAY)
    return s


def recv_all(s, limit=1 << 20):
    buf = b""
    try:
        while len(buf) < limit:
            d = s.recv(65536)
            if not d:
                break
            buf += d
    except (socket.timeout, ConnectionError):
        pass
    return buf


def parse(resp):
    """-> (status, headers dict lowercased, body bytes) or (0, {}, b'') for no response."""
    if not resp.startswith(b"HTTP/"):
        return 0, {}, b""
    head, _, body = resp.partition(b"\r\n\r\n")
    lines = head.decode("latin1").split("\r\n")
    st = int(lines[0].split()[1])
    h = {}
    for l in lines[1:]:
        if ":" in l:
            k, v = l.split(":", 1)
            h[k.strip().lower()] = v.strip()
    return st, h, body


def request(method, path, body=b"", headers=None, src=None, ctype="application/json", timeout=10, raw_head=None):
    s = conn(src, timeout)
    try:
        if raw_head is not None:
            s.sendall(raw_head + body)
        else:
            hs = {"Host": "relay", "Connection": "close"}
            if method != "GET" or body:
                hs["Content-Type"] = ctype
                hs["Content-Length"] = str(len(body))
            hs.update(headers or {})
            head = "%s %s HTTP/1.1\r\n" % (method, path) + "".join("%s: %s\r\n" % kv for kv in hs.items()) + "\r\n"
            s.sendall(head.encode("latin1") + body)
        return parse(recv_all(s))
    finally:
        s.close()


def post(path, obj, **kw):
    b = obj if isinstance(obj, bytes) else json.dumps(obj).encode()
    st, h, body = request("POST", path, b, **kw)
    try:
        return st, json.loads(body)
    except Exception:
        return st, body


def fcm(path="/stats", obj=None):
    req = urllib.request.Request(CONTROL + path, data=json.dumps(obj).encode() if obj is not None else None)
    return json.load(urllib.request.urlopen(req, timeout=10))


def fcm_mode(token="ok", send="ok"):
    return fcm("/mode", {"token": token, "send": send})


def reg_body(addr):
    return {"platform": "android", "app": APP, "address_kind": "fcm_token", "address": addr}


def confirm_body(cid, code, addr):
    return dict(reg_body(addr), challenge_id=cid, code=code)


def last_code():
    for b in reversed(fcm()["bodies"]):
        d = json.loads(b)["message"].get("data", {})
        if d.get("t") == "challenge":
            return d["c"]
    raise RuntimeError("no challenge push seen")


def make_handle(addr, xff=None):
    h = {"X-Forwarded-For": xff} if xff else {}
    st, b = post("/v1/handles", reg_body(addr), headers=h)
    assert st == 202, (st, b)
    code = last_code()
    st, b = post("/v1/handles/confirm", confirm_body(b["challenge_id"], code, addr), headers=h)
    assert st == 200, (st, b)
    return b["handle"]


def proc_stat(pid):
    """(rss_kib, fds, threads) of a process."""
    rss = thr = 0
    for l in open("/proc/%d/status" % pid):
        if l.startswith("VmRSS:"):
            rss = int(l.split()[1])
        elif l.startswith("Threads:"):
            thr = int(l.split()[1])
    return rss, len(os.listdir("/proc/%d/fd" % pid)), thr
