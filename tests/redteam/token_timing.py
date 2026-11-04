#!/usr/bin/env python3
"""
Timing comparison for `hello.token` checks: wrong-length tokens vs
right-length-wrong-content tokens, many samples each, to check whether
`Auth::check`'s `ct_eq` (crates/acd-core/src/server.rs) leaks anything
beyond length over the wire (the static review's "Considered and
rejected" already read the code and concluded it does not; this is the
live measurement). Also separately times the pre-auth admission/backoff
rate itself (how many hello attempts per second the 8/32-slot pools and
5s hello_timeout actually allow), since that rate is what would make or
break a real brute-force regardless of per-comparison timing.

Stdlib only: implements a small two-sample test (a simplified
Mann-Whitney U via rank-sum) instead of depending on scipy, since this
sandbox has no network access to pip-install anything and shouldn't.

Usage:
    python3 -I token_timing.py --dir <acd config dir> --port <ws port> \
        --samples 500

Safety: connects only to 127.0.0.1:<port>.
"""
import argparse
import os
import statistics
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import acdws  # noqa: E402


def one_hello_timing(port, token):
    t0 = time.perf_counter()
    try:
        ws = acdws.WS.connect("127.0.0.1", port, use_tls=True, timeout=5)
    except Exception:
        return None
    try:
        ws.send_json(acdws.hello_msg(token))
        ws.recv_json(timeout=5)
    except Exception:
        pass
    dt = time.perf_counter() - t0
    ws.close()
    return dt


def rank_sum_u(a, b):
    """A minimal Mann-Whitney U (rank-sum), no ties correction needed for
    a rough significance read. Returns (U, z) where |z| > ~1.96 would
    usually be called significant at p<0.05 for large samples -- used
    here only to say "no detectable difference" vs "investigate
    further", not as a rigorous proof."""
    combined = sorted((v, 0) for v in a) + sorted((v, 1) for v in b)
    combined.sort(key=lambda t: t[0])
    n1, n2 = len(a), len(b)
    ranks = {}
    i = 0
    rank = 1
    while i < len(combined):
        j = i
        while j < len(combined) and combined[j][0] == combined[i][0]:
            j += 1
        avg_rank = (rank + (rank + (j - i) - 1)) / 2
        for k in range(i, j):
            ranks[k] = avg_rank
        rank += j - i
        i = j
    r1 = sum(ranks[idx] for idx, (_, grp) in enumerate(combined) if grp == 0)
    u1 = r1 - n1 * (n1 + 1) / 2
    mu = n1 * n2 / 2
    sigma = (n1 * n2 * (n1 + n2 + 1) / 12) ** 0.5
    z = (u1 - mu) / sigma if sigma > 0 else 0.0
    return u1, z


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dir", required=True)
    ap.add_argument("--port", type=int, required=True)
    ap.add_argument("--samples", type=int, default=500)
    args = ap.parse_args()
    real_token = open(os.path.join(args.dir, "client.token")).read().strip()

    wrong_length_tokens = [f"short-{i}" for i in range(args.samples)]
    right_length_tokens = ["A" * len(real_token) for _ in range(args.samples)]
    # Vary content slightly so results are not literally one cached path.
    right_length_tokens = [
        (c[:i % len(c)] + "B" + c[i % len(c) + 1:]) for i, c in enumerate(right_length_tokens)
    ]

    print(f"real token length: {len(real_token)} bytes")
    print(f"sampling {args.samples} x wrong-length, {args.samples} x right-length-wrong-content...")

    t_wrong_len = []
    t_right_len = []
    # Interleave the two classes so any slow drift (GC, scheduler, the
    # ambient soak load if one is running) affects both equally.
    for i in range(args.samples):
        dt_a = one_hello_timing(args.port, wrong_length_tokens[i])
        dt_b = one_hello_timing(args.port, right_length_tokens[i])
        if dt_a is not None:
            t_wrong_len.append(dt_a)
        if dt_b is not None:
            t_right_len.append(dt_b)

    def report(name, xs):
        xs = sorted(xs)
        print(
            f"{name}: n={len(xs)} median={statistics.median(xs)*1e6:.1f}us "
            f"p10={xs[len(xs)//10]*1e6:.1f}us p90={xs[9*len(xs)//10]*1e6:.1f}us "
            f"stdev={statistics.pstdev(xs)*1e6:.1f}us"
        )

    report("wrong_length_token", t_wrong_len)
    report("right_length_wrong_content_token", t_right_len)
    u, z = rank_sum_u(t_wrong_len, t_right_len)
    print(f"\nrank-sum U={u:.1f} z={z:.3f} "
          f"({'no significant difference' if abs(z) < 1.96 else 'POSSIBLE DIFFERENCE -- investigate'})")

    # Brute-force rate: how many hello attempts per second does the
    # pre-auth pool + hello_timeout actually allow, sustained, from one
    # source? (Reported, not asserted -- the static review already noted
    # the 256-bit token makes this moot regardless of the number.)
    t0 = time.time()
    n = 200
    for i in range(n):
        one_hello_timing(args.port, f"ratecheck-{i}")
    dt = time.time() - t0
    print(f"\nsustained single-source wrong-token hello rate: {n/dt:.1f} attempts/s "
          f"({n} attempts in {dt:.2f}s)")


if __name__ == "__main__":
    main()
