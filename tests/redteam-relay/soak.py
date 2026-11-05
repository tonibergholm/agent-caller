#!/usr/bin/env python3
"""One-hour mixed hostile soak of a real acd-relay; samples RSS/fds/threads.

Run it isolated (own network namespace, own fake FCM on :443):
  unshare -n python3 nsrun.py python3 soak.py WORKDIR SECONDS
Writes WORKDIR/soak.csv (every 10 s) and WORKDIR/soak-summary.json.
Safe against a local test relay only.
"""
import json, os, random, socket, subprocess, sys, threading, time
HERE = os.path.dirname(os.path.abspath(__file__))
W, DUR = sys.argv[1], int(sys.argv[2])
os.makedirs(W, exist_ok=True)
subprocess.check_call([HERE + "/up.sh", W], env=dict(os.environ, TRUSTED_PROXY=""))
import rr
pid = int(open(W + "/relay.pid").read())
fcm_pid = int(open(W + "/fcm.pid").read())
stop = threading.Event()
T0 = time.time()
stats = {"probe_ok": 0, "probe_fail": 0, "probe_max_ms": 0, "attacks": {}}
lock = threading.Lock()
def bump(k, n=1):
    with lock:
        stats["attacks"][k] = stats["attacks"].get(k, 0) + n
def src():
    return "127.0.0.%d" % random.randint(2, 250)

def hold(kind, n, payload, period):
    """n connections that never finish a request head; reopen when closed."""
    socks = []
    def loop():
        while not stop.is_set():
            socks[:] = [s for s in socks if s]
            while len(socks) < n and not stop.is_set():
                try:
                    s = rr.conn(src(), 3); s.setblocking(False); socks.append(s); bump(kind + "_opened")
                    try: s.send(payload(len(socks)))
                    except OSError: pass
                except OSError:
                    break
            time.sleep(period)
            for i, s in enumerate(socks):
                try:
                    if s.recv(65536) == b"": raise OSError
                except BlockingIOError:
                    try: s.send(b"X-a: b\r\n")
                    except OSError: socks[i] = None
                except OSError:
                    socks[i] = None
    threading.Thread(target=loop, daemon=True).start()

def pipeliner(n):
    """connections that pipeline /healthz forever and never read."""
    def loop():
        req = b"GET /healthz HTTP/1.1\r\nHost: x\r\n\r\n" * 2000
        while not stop.is_set():
            ss = []
            for _ in range(n):
                try:
                    s = rr.conn(src(), 3); s.settimeout(0.01); ss.append(s)
                except OSError: pass
            t_end = time.time() + 120
            while time.time() < t_end and not stop.is_set():
                for s in ss:
                    try: s.send(req); bump("pipeline_sent")
                    except OSError: pass
                time.sleep(1)
            for s in ss: s.close()
    threading.Thread(target=loop, daemon=True).start()

def garbage_wakes(rate):
    def loop():
        while not stop.is_set():
            try:
                rr.post("/v1/wake", {"v": 1, "handle": "A" * random.randint(1, 600)}, src=src(), timeout=5)
                bump("garbage_wake")
            except OSError: pass
            time.sleep(1.0 / rate)
    threading.Thread(target=loop, daemon=True).start()

def registrations(rate):
    def loop():
        i = 0
        while not stop.is_set():
            i += 1
            try:
                rr.post("/v1/handles", rr.reg_body("soak%d:%d" % (i, random.randint(0, 1 << 30))), src=src(), timeout=15)
                bump("register")
            except OSError: pass
            time.sleep(1.0 / rate)
    threading.Thread(target=loop, daemon=True).start()

def legit_wakes():
    """valid handles, one wake each, through the FCM path (drives FCM faults)."""
    def loop():
        i = 0
        while not stop.is_set():
            i += 1
            try:
                h = rr.make_handle("legit%d" % i)
                rr.post("/v1/wake", {"v": 1, "handle": h}, timeout=15)
                bump("legit_wake")
            except Exception:
                bump("legit_wake_err")
            time.sleep(8)
    threading.Thread(target=loop, daemon=True).start()

def fcm_cycle():
    modes = [("ok", "ok"), ("ok", "429"), ("ok", "500"), ("ok", "stall_head"), ("ok", "stall_body"),
             ("ok", "slow_body"), ("ok", "huge"), ("stall_head", "ok"), ("500", "ok"), ("huge_expires", "ok"),
             ("ok", "redirect"), ("ok", "garbage"), ("ok", "401"), ("ok", "ok")]
    def loop():
        i = 0
        while not stop.is_set():
            t, s = modes[i % len(modes)]; i += 1
            try: rr.fcm_mode(t, s)
            except Exception: pass
            bump("fcm_mode_%s_%s" % (t, s), 1)
            stop.wait(180)
    threading.Thread(target=loop, daemon=True).start()

def probe():
    """a victim: /healthz latency and success, once a second, from one source."""
    def loop():
        while not stop.is_set():
            t = time.time()
            try:
                st, _, _ = rr.request("GET", "/healthz", src="127.0.0.1", timeout=5)
                ok = st == 200
            except OSError:
                ok = False
            ms = int((time.time() - t) * 1000)
            with lock:
                stats["probe_ok" if ok else "probe_fail"] += 1
                stats["probe_max_ms"] = max(stats["probe_max_ms"], ms)
            time.sleep(1)
    threading.Thread(target=probe_loop if False else loop, daemon=True).start()

hold("slowloris", 400, lambda i: b"POST /v1/wake HTTP/1.1\r\nHost: x\r\n", 5)
hold("bighead", 100, lambda i: b"POST /v1/wake HTTP/1.1\r\nHost: x\r\nX-Pad: " + b"A" * 300_000, 5)
pipeliner(20)
garbage_wakes(0.5)
registrations(0.1)
legit_wakes()
fcm_cycle()
probe()

with open(W + "/soak.csv", "w") as f:
    f.write("t,rss_kib,fds,threads,fcm_open_conns,fcm_max_open,fcm_sends,fcm_token_reqs,probe_ok,probe_fail,probe_max_ms\n")
    while time.time() - T0 < DUR:
        try:
            rss, fds, thr = rr.proc_stat(pid)
            c = rr.fcm()
            with lock:
                row = (int(time.time() - T0), rss, fds, thr, c["open"], c["max_open"], c["sends"], c["token_requests"],
                       stats["probe_ok"], stats["probe_fail"], stats["probe_max_ms"])
            f.write(",".join(map(str, row)) + "\n"); f.flush()
        except Exception as e:
            f.write("# sample error %r\n" % (e,)); f.flush()
        time.sleep(10)
stop.set()
try:
    rr.fcm_mode("ok", "ok")
    time.sleep(15)
    rss, fds, thr = rr.proc_stat(pid)
    stats["final_idle"] = {"rss_kib": rss, "fds": fds, "threads": thr}
    st, _, _ = rr.request("GET", "/healthz", src="127.0.0.1")
    stats["recovered_healthz"] = st
except Exception as e:
    stats["final_error"] = repr(e)
json.dump(stats, open(W + "/soak-summary.json", "w"), indent=1)
print(json.dumps(stats))
