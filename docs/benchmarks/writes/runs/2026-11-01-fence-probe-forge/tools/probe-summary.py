#!/usr/bin/env python3
"""Summarize fence-io-probe output: each second is fast when its fence p90 is under
0.8 ms, slow when its fence p50 is over 1.2 ms, and mixed otherwise. For alternating
runs (wt2:1 fl2:1 ...), each wt2 second pairs with the fl2 second after it, and a pair
counts in a state only when both seconds are in it."""
import re, statistics, sys, collections

def parse(path):
    rows = []
    for line in open(path):
        m = re.match(r"\s*([\d.]+) (\w+)\s+fences=\s*(\d+) mean=([\d.]+) p50=([\d.]+) p90=([\d.]+) max=([\d.]+) dev_w=(\d+) wms=([\d.]+) dev_fl=(\d+) fms=([\d.]+)", line)
        if m:
            t, pat, n, mean, p50, p90, mx, w, wms, fl, fms = m.groups()
            rows.append(dict(t=float(t), pat=pat, n=int(n), mean=float(mean), p50=float(p50), p90=float(p90), w=int(w), wms=float(wms), fl=int(fl), fms=float(fms)))
    return rows

def state(r):
    if r["n"] == 0:
        return None
    if r["p90"] < 0.8:
        return "fast"
    if r["p50"] > 1.2:
        return "slow"
    return "mixed"

def med(xs):
    return statistics.median(xs) if xs else float("nan")

for path in sys.argv[1:]:
    rows = [r for r in parse(path) if r["pat"] != "idle"]
    print(f"== {path}: {len(rows)} seconds")
    by = collections.defaultdict(list)
    for r in rows:
        by[(r["pat"], state(r))].append(r)
    for (pat, st), rs in sorted(by.items(), key=lambda k: (k[0][0], str(k[0][1]))):
        print(f"  {pat:5} {st:6} seconds={len(rs):4} fence p50 median={med([r['p50'] for r in rs]):.3f} mean median={med([r['mean'] for r in rs]):.3f} "
              f"writes/fence={sum(r['w'] for r in rs)/max(1,sum(r['n'] for r in rs)):.2f} wms={med([r['wms'] for r in rs]):.3f} "
              f"flushes/fence={sum(r['fl'] for r in rs)/max(1,sum(r['n'] for r in rs)):.2f} fms={med([r['fms'] for r in rs if r['fl']]):.3f}")
    pairs = collections.defaultdict(list)
    for a, b in zip(rows, rows[1:]):
        if a["pat"] == "wt2" and b["pat"] == "fl2" and abs(b["t"] - a["t"] - 1) < 0.2:
            sa, sb = state(a), state(b)
            key = sa if sa == sb else "split"
            pairs[key].append((a, b))
    for key, ps in sorted(pairs.items()):
        ratios = [a["mean"] / b["mean"] for a, b in ps]
        print(f"  pairs {key:6} n={len(ps):4} wt2 mean median={med([a['mean'] for a,_ in ps]):.3f} fl2 mean median={med([b['mean'] for _,b in ps]):.3f} "
              f"ratio wt2/fl2 median={med(ratios):.3f} wt2 faster in {sum(r < 1 for r in ratios)}")
