#!/usr/bin/env python3
"""
Samples RSS, FD count and thread count of a running `acd` daemon every
10 seconds and writes a CSV. Meant to run for the duration of the hostile
soak load started by `soak_attack.py`, so the two together answer: does a
long hostile run grow memory, leak file descriptors, or leak tasks
(threads are a proxy; acd is a single-threaded-ish tokio runtime using a
worker pool, so a task leak would more likely show as growing memory or
FD count than as a thread count change -- we record threads anyway).

Usage:
    python3 -I soak_sample.py <pid> <output.csv> [duration_seconds]

Safety: reads only /proc/<pid>/..., writes only to the given output path.
"""
import sys
import time
import csv


def sample(pid: int):
    with open(f"/proc/{pid}/status") as f:
        status = f.read()
    rss_kb = None
    threads = None
    for line in status.splitlines():
        if line.startswith("VmRSS:"):
            rss_kb = int(line.split()[1])
        elif line.startswith("Threads:"):
            threads = int(line.split()[1])
    import os
    try:
        fds = len(os.listdir(f"/proc/{pid}/fd"))
    except OSError:
        fds = None
    return rss_kb, threads, fds


def main():
    pid = int(sys.argv[1])
    out_path = sys.argv[2]
    duration = float(sys.argv[3]) if len(sys.argv) > 3 else 3600.0
    interval = 10.0
    start = time.time()
    with open(out_path, "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["elapsed_s", "rss_kb", "threads", "fds"])
        while time.time() - start < duration:
            try:
                rss, threads, fds = sample(pid)
            except FileNotFoundError:
                w.writerow([round(time.time() - start, 1), "PROCESS_GONE", "", ""])
                f.flush()
                print("process gone, stopping sampler", file=sys.stderr)
                return
            w.writerow([round(time.time() - start, 1), rss, threads, fds])
            f.flush()
            time.sleep(interval)


if __name__ == "__main__":
    main()
