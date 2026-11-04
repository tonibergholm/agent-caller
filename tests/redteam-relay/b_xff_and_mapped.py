#!/usr/bin/env python3
"""Checks: X-Forwarded-For multi-line / malformed, and IPv6-mapped-IPv4
peers vs the trusted_proxy comparison and the /64 keying.
Run with TRUSTED_PROXY=127.0.0.1 ./up.sh $W first."""
import rr

# 1. A client sends two XFF header lines (as a chain proxy would append its
#    own); axum/hyper's HeaderMap.get returns only the FIRST occurrence.
st, h, b = rr.request("GET", "/healthz", headers={}, src="127.0.0.2")
raw = (b"GET /healthz HTTP/1.1\r\nHost: x\r\nX-Forwarded-For: 9.9.9.9\r\n"
       b"X-Forwarded-For: 127.0.0.1\r\nConnection: close\r\n\r\n")
st, h, b = rr.request("GET", "/healthz", raw_head=raw, src="127.0.0.1")
print("two XFF lines, healthz:", st)  # just confirms parsing path is reachable; real check below on /v1/wake

def wake_with_xff(xff_lines, src):
    body = b'{"v":1,"handle":"' + b"A" * 20 + b'"}'
    head = ("POST /v1/wake HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\n" +
            "".join("X-Forwarded-For: %s\r\n" % x for x in xff_lines) +
            "Content-Length: %d\r\nConnection: close\r\n\r\n" % len(body)).encode()
    return rr.request("POST", "/v1/wake", raw_head=head, body=body, src=src)

# drain the per-IP budget of a spoofed victim IP via a SECOND XFF line (the
# proxy-appended one); if the relay reads the rightmost line it is immune,
# if it reads the first header occurrence it is not.
for i in range(5):
    st, h, b = wake_with_xff(["203.0.113.9", "127.0.0.1"], "127.0.0.1")
print("5 requests via trusted proxy with two XFF LINES, first=203.0.113.9:", st, b)
st2, _, b2 = wake_with_xff(["203.0.113.9"], "127.0.0.1")  # single header, same value -> should share the bucket
print("single XFF header, same claimed IP, 6th request:", st2, b2)

# 2. IPv6-mapped-IPv4 peer: does it equal trusted_proxy (an IPv4 value)?
# We cannot bind an IPv6 socket here (no IPv6), so this is argued from source
# instead: AppState::client_ip compares `self.trusted_proxy == Some(peer)`
# where `trusted_proxy: Option<IpAddr>` is parsed from TOML as V4 unless
# written as "::ffff:a.b.c.d". IpAddr::V4(x) != IpAddr::V6(y) even for the
# same numeric address, so a V6-mapped peer can never equal a V4
# trusted_proxy, and the reverse: a trusted_proxy written as "::ffff:127.0.0.1"
# never matches a real V4 peer 127.0.0.1. This is self-consistent (not a
# bypass) and is recorded as a tried/no-finding below.
