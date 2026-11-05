#!/usr/bin/env python3
"""relay -> FCM response handling: huge body capped, redirect not followed,
stalled body capped by SEND_DEADLINE, garbage status line handled, memory
bounded (max_body=16KiB per main.rs)."""
import os, time
import rr
W = os.environ["W"]
pid = int(open(W + "/relay.pid").read())

def wake_via(mode):
    h = rr.make_handle("resp-%s:tok" % mode)
    rr.fcm_mode(send=mode)
    t0 = time.time()
    rss0, fds0, _ = rr.proc_stat(pid)
    st, b = rr.post("/v1/wake", {"v": 1, "handle": h}, timeout=15)
    dt = time.time() - t0
    rss1, fds1, _ = rr.proc_stat(pid)
    rr.fcm_mode(send="ok")
    return st, b, dt, rss1 - rss0, fds1 - fds0

for mode in ["huge", "redirect", "stall_body", "garbage"]:
    st, b, dt, drss, dfds = wake_via(mode)
    print(f"{mode:12s} status={st} body={b} time={dt:.2f}s drss_kib={drss} dfds={dfds}")
