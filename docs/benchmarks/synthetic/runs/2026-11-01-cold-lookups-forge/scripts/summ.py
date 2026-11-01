#!/usr/bin/env python3
# summ.py OUTDIR: per query, median of all samples of A and of B, ratio B/A, per-round medians
import json, sys, glob, statistics, os, re
d = sys.argv[1]
data = {}
for f in sorted(glob.glob(os.path.join(d, '[AB]-*.json'))):
    lab, rnd = re.match(r'([AB])-(\d+)\.json', os.path.basename(f)).groups()
    for q, r in json.load(open(f)).items():
        if 'ms' not in r: continue
        e = data.setdefault(q, {'A': [], 'B': [], 'rA': [], 'rB': [], 'h': set(), 'pw': []})
        e[lab] += r['ms']; e['r' + lab].append(round(statistics.median(r['ms']), 1))
        e['h'].update(r.get('hashes', []))
        e['pw'] += r.get('prewarm_ms', []) if lab == 'B' else []
print(f"{'query':16} {'A med':>8} {'B med':>8} {'B/A':>6}  hashes  rounds A | rounds B")
for q, e in data.items():
    if not e['A'] or not e['B']: continue
    a, b = statistics.median(e['A']), statistics.median(e['B'])
    pw = f"  prewarm med {statistics.median(e['pw']):.1f} ms" if e['pw'] else ''
    print(f"{q:16} {a:8.2f} {b:8.2f} {b/a:6.3f}  {len(e['h'])}  {e['rA']} | {e['rB']}{pw}")
