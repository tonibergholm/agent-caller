#!/usr/bin/env python3
"""Does acd-relay's outbound HttpClient leak the spawned connection task (and
its fd) when the overall tokio::time::timeout fires while FCM stalls?
http.rs::exchange spawns `conn` and awaits `sender.send_request`; on the OUTER
timeout (post()'s tokio::time::timeout), the spawned task is just dropped,
not aborted -- check if that leaks fds/threads over many stalled sends."""
import time
import rr

import os
pid = int(open(os.environ["W"] + "/relay.pid").read())

import concurrent.futures as cf
N = 20
handles = [rr.make_handle("stall%d:token" % i, src="127.0.0.%d" % (10 + i)) for i in range(N)]  # register while FCM is healthy
rr.fcm_mode(send="stall_head")  # OAuth token still works; FCM send call hangs
rss0, fds0, thr0 = rr.proc_stat(pid)
print("before:", rss0, fds0, thr0)
def one(h):
    return rr.post("/v1/wake", {"v": 1, "handle": h}, timeout=15)[0]
with cf.ThreadPoolExecutor(max_workers=N) as ex:
    sts = list(ex.map(one, handles))
print("wake statuses (expect all 503 after the 10s SEND_DEADLINE):", sts)
rss1, fds1, thr1 = rr.proc_stat(pid)
print("after:", rss1, fds1, thr1, "delta fds:", fds1 - fds0, "delta threads:", thr1 - thr0)
rr.fcm_mode(send="ok")
