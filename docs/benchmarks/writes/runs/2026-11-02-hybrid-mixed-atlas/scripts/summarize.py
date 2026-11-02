#!/usr/bin/env python3
"""summarize.py records.json [metric]: per (bin, config) and query, the pooled median, the
per-process medians and where the server threads ran. metric is wall (default) or server."""
import json, statistics, sys
from collections import defaultdict

d = json.load(open(sys.argv[1]))
metric = sys.argv[2] if len(sys.argv) > 2 else 'wall'
recs = d['records']
by = defaultdict(lambda: defaultdict(list))
cpu = defaultdict(lambda: defaultdict(lambda: defaultdict(float)))
for r in recs:
    key = (r['bin'], r['config'])
    for q, v in r['queries'].items():
        xs = v['wall_ms'] if metric == 'wall' else v['server_ms']['total']
        by[key][q].append(xs)
        for k, ms in v['cpu_ms'].items():
            cpu[key][q][k] += ms
keys = sorted(by)
queries = list(next(iter(by.values())).keys())
templates = sorted({q.rsplit('-', 1)[0] if q.startswith('bi-') else q for q in queries})
print(f'metric={metric}  host={d["meta"]["host"]}  governor={d["meta"]["governor"]["0"]["scaling_governor"]}/{d["meta"]["governor"]["0"]["energy_performance_preference"]}')
hdr = 'template'.ljust(18) + ''.join(f'{b}/{c}'.rjust(16) for b, c in keys)
print(hdr)
for t in templates:
    row = t.ljust(18)
    for k in keys:
        pooled = [x for q, runs in by[k].items() if q == t or q.rsplit('-', 1)[0] == t for run in runs for x in run]
        row += f'{statistics.median(pooled):16.2f}' if pooled else ' ' * 16
    print(row)
print('\nper-process medians (instance 1 and births):')
for t in [q for q in queries if q.endswith('-1') or q == 'births-by-decade']:
    print(t)
    for k in keys:
        meds = [statistics.median(run) for run in by[k][t]]
        print('  ', f'{k[0]}/{k[1]}'.ljust(18), ' '.join(f'{m:7.2f}' for m in meds))
print('\nCPU ms per timed request by thread kind and core type (instance 1 and births):')
for t in [q for q in queries if q.endswith('-1') or q == 'births-by-decade']:
    print(t)
    for k in keys:
        n = sum(len(run) for run in by[k][t])
        c = cpu[k][t]
        print('  ', f'{k[0]}/{k[1]}'.ljust(18), '  '.join(f'{kk}={c[kk] / n:6.2f}' for kk in sorted(c)))
