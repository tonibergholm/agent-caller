#!/usr/bin/env python3
"""Cryptography: handle tampering, truncation, kid confusion, replay,
challenge brute force / attempt limit, concurrent-confirm atomicity, and
wrong-handle timing (many samples)."""
import base64, os, statistics, time
import concurrent.futures as cf
import rr

h = rr.make_handle("crypto:victim")
raw = base64.urlsafe_b64decode(h + "=" * (-len(h) % 4))
print("handle len", len(h), "raw bytes", len(raw), "version", raw[0], "kid", raw[1])

def mutate(i, raw=raw):
    b = bytearray(raw)
    b[i % len(b)] ^= 1 << (i % 8)
    return base64.urlsafe_b64encode(bytes(b)).decode().rstrip("=")

results = {}
for i in range(len(raw) * 2):
    st, body = rr.post("/v1/wake", {"v": 1, "handle": mutate(i)})
    results[body.get("error", st) if isinstance(body, dict) else st] = results.get(body.get("error", st) if isinstance(body, dict) else st, 0) + 1
print("bit-flip outcomes across every byte:", results)

print("truncated handle (drop last 10 chars):", rr.post("/v1/wake", {"v": 1, "handle": h[:-10]}))
print("truncated handle (first 5 chars only):", rr.post("/v1/wake", {"v": 1, "handle": h[:5]}))
print("empty handle:", rr.post("/v1/wake", {"v": 1, "handle": ""}))
print("version byte 0:", rr.post("/v1/wake", {"v": 1, "handle": base64.urlsafe_b64encode(bytes([0]) + raw[1:]).decode().rstrip('=')}))
print("version byte 2:", rr.post("/v1/wake", {"v": 1, "handle": base64.urlsafe_b64encode(bytes([2]) + raw[1:]).decode().rstrip('=')}))
print("kid byte 255 (nonexistent key):", rr.post("/v1/wake", {"v": 1, "handle": base64.urlsafe_b64encode(raw[:1] + bytes([255]) + raw[2:]).decode().rstrip('=')}))
print("replay same valid handle 3x:", [rr.post("/v1/wake", {"v": 1, "handle": h})[0] for _ in range(3)])

# nonce reuse across restarts: two seals of the SAME payload must use
# different salts (hence different ciphertexts) even with the same
# sealing key -- verify by sealing twice via two independent registrations.
h2 = rr.make_handle("crypto:victim")  # same address, different challenge
print("two handles for the same address are byte-different:", h != h2)

# challenge brute force / attempt limit
st, b = rr.post("/v1/handles", rr.reg_body("brute:addr"))
cid = b["challenge_id"]
real_code = rr.last_code()
tries = []
for guess in ["AAAAAAAAAAAAAAAAAAAAAA", "BBBBBBBBBBBBBBBBBBBBBB", "CCCCCCCCCCCCCCCCCCCCCC", real_code, real_code]:
    st, b = rr.post("/v1/handles/confirm", rr.confirm_body(cid, guess, "brute:addr"))
    tries.append((st, b))
print("confirm attempts (3 wrong + correct + replay):", tries)

# concurrent confirm: same challenge, many threads, exactly one must succeed
st, b = rr.post("/v1/handles", rr.reg_body("race:addr"))
cid = b["challenge_id"]
code = rr.last_code()
def try_confirm(_):
    return rr.post("/v1/handles/confirm", rr.confirm_body(cid, code, "race:addr"))[0]
with cf.ThreadPoolExecutor(max_workers=32) as ex:
    outs = list(ex.map(try_confirm, range(32)))
from collections import Counter
print("32 concurrent confirms of ONE challenge:", Counter(outs))

# wrong-handle timing: many samples of "wrong kid" vs "auth failure" vs "bad base64"
def timeit(handle, n=300):
    ts = []
    for _ in range(n):
        t0 = time.perf_counter()
        rr.post("/v1/wake", {"v": 1, "handle": handle})
        ts.append(time.perf_counter() - t0)
    return statistics.median(ts), statistics.stdev(ts)
bad_auth = mutate(30)                          # auth tag fails
bad_kid = base64.urlsafe_b64encode(raw[:1] + bytes([254]) + raw[2:]).decode().rstrip("=")
bad_b64 = h[:-1] + "!"
print("median,stdev seconds -- valid:", timeit(h))
print("median,stdev seconds -- bad auth tag:", timeit(bad_auth))
print("median,stdev seconds -- bad kid (no key):", timeit(bad_kid))
print("median,stdev seconds -- bad base64:", timeit(bad_b64))
