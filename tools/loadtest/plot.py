#!/usr/bin/env python3
"""Tables, ASCII plots and trend slopes from a soak CSV (standard library only).

  plot.py soak.csv                 summary table every 10 minutes + plots
  plot.py soak.csv rss_kb fds      plot these columns only
"""
import csv
import sys

PLOT_COLS = ["rss_kb", "fds", "cpu_pct", "vis_p99_ms", "res_p99_ms", "keys", "idem", "tasks", "log_bytes"]


def load(path):
    with open(path) as f:
        rows = list(csv.DictReader(f))
    out = []
    for r in rows:
        d = {}
        for k, v in r.items():
            try:
                d[k] = float(v)
            except (TypeError, ValueError):
                d[k] = None
        out.append(d)
    return out


def slope(xs, ys):
    n = len(xs)
    if n < 3:
        return 0.0
    mx, my = sum(xs) / n, sum(ys) / n
    den = sum((x - mx) ** 2 for x in xs)
    return sum((x - mx) * (y - my) for x, y in zip(xs, ys)) / den if den else 0.0


def segments(rows):
    """Row runs with one pid: a restart starts a new run."""
    runs, cur = [], []
    for r in rows:
        if cur and r["pid"] != cur[-1]["pid"]:
            runs.append(cur)
            cur = []
        cur.append(r)
    if cur:
        runs.append(cur)
    return runs


def ascii_plot(rows, col, width=72, height=10):
    pts = [(r["t_s"], r[col]) for r in rows if r.get(col) is not None]
    if not pts:
        return f"{col}: no data\n"
    lo, hi = min(v for _, v in pts), max(v for _, v in pts)
    span = hi - lo or 1.0
    t0, t1 = pts[0][0], pts[-1][0]
    grid = [[" "] * width for _ in range(height)]
    for t, v in pts:
        x = int((t - t0) / ((t1 - t0) or 1) * (width - 1))
        y = height - 1 - int((v - lo) / span * (height - 1))
        grid[y][x] = "*"
    lines = [f"{col}  (min {lo:g}, max {hi:g}; x: {t0:.0f}..{t1:.0f} s)"]
    for i, row in enumerate(grid):
        label = f"{hi - span * i / (height - 1):>10.4g} |"
        lines.append(label + "".join(row))
    return "\n".join(lines) + "\n"


def table(rows, every=600):
    cols = [("t_min", None), ("rss_MB", "rss_kb"), ("fds", "fds"), ("thr", "threads"), ("cpu%", "cpu_pct"),
            ("opens/s", "opens_s"), ("vis_p50", "vis_p50_ms"), ("vis_p99", "vis_p99_ms"), ("vis_max", "vis_max_ms"),
            ("res_p99", "res_p99_ms"), ("res_max", "res_max_ms"), ("errs", "errors"), ("lag", "lagged")]
    print("  ".join(f"{n:>8}" for n, _ in cols))
    buckets = {}
    for r in rows:
        buckets.setdefault(int(r["t_s"] // every), []).append(r)
    for b in sorted(buckets):
        rs = buckets[b]
        line = [f"{b * every / 60:>8.0f}"]
        for name, c in cols[1:]:
            vals = [r[c] for r in rs if r.get(c) is not None]
            if not vals:
                line.append(f"{'':>8}")
            elif name in ("vis_max", "res_max", "errs", "lag", "fds", "thr"):
                line.append(f"{max(vals):>8.4g}")
            elif name == "rss_MB":
                line.append(f"{sum(vals) / len(vals) / 1024:>8.1f}")
            else:
                line.append(f"{sum(vals) / len(vals):>8.4g}")
        print("  ".join(line))


def trends(rows):
    print("\ntrend per process run (least squares over the run, first 60 s skipped as warm-up):")
    for run in segments(rows):
        run = [r for r in run if r["t_s"] - run[0]["t_s"] >= 60]
        if len(run) < 6:
            continue
        xs = [r["t_s"] / 3600 for r in run]
        parts = []
        for c, unit in (("rss_kb", "KB/h"), ("fds", "fd/h"), ("threads", "thr/h"), ("log_bytes", "B/h")):
            ys = [r[c] for r in run if r.get(c) is not None]
            if len(ys) == len(xs):
                parts.append(f"{c} {slope(xs, ys):+.1f} {unit}")
        print(f"  pid {run[0]['pid']:.0f}  t={run[0]['t_s']:.0f}..{run[-1]['t_s']:.0f} s  " + "; ".join(parts))


if __name__ == "__main__":
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    data = load(sys.argv[1])
    cols = sys.argv[2:] or PLOT_COLS
    table(data)
    trends(data)
    print()
    for c in cols:
        if data and data[0].get(c) is not None:
            print(ascii_plot(data, c))
