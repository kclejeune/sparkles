#!/usr/bin/env python3
"""mixsum.py records.json...: medians and ranges per (bin, config) of a mixab run"""
import json, statistics as st, sys
from collections import defaultdict
by = defaultdict(list)
for p in sys.argv[1:]:
    for r in json.load(open(p)):
        by[(r['bin'], r['config'])].append(r)
def f(xs): return f"{st.median(xs):8.1f} ({min(xs):.1f}-{max(xs):.1f})"
for k, rs in by.items():
    g = lambda key: [r[key] for r in rs]
    sk = lambda kind, m: st.median(r['sched'].get(kind, {}).get(m, 0) / 1000 for r in rs)
    print(f"{k[0]:>6}/{k[1]:<7} n={len(rs)} reads/s {f(g('read_qps'))} writes/s {f(g('writes_per_s'))} "
          f"rp50 {st.median(g('read_p50_ms')):.2f} wp50 {st.median(g('write_p50_ms')):.2f} busyP {st.median(g('busy_p')):.2f} busyE {st.median(g('busy_e')):.2f} "
          f"tokio run/wait s {sk('tokio','run_ms'):.0f}/{sk('tokio','wait_ms'):.0f} rayon {sk('rayon','run_ms'):.0f}/{sk('rayon','wait_ms'):.0f}")
