#!/usr/bin/env python3
"""Device time per write in each 100 ms sample of dsample.py, inside the write window of
each process of a campaign. Prints, per process, the share of samples whose writes took
under 0.4 ms, 0.4 to 1.0 ms and over 1.0 ms on the device, and the lengths in seconds of
the runs of consecutive fast samples. Usage: dspells.py SAMPLES CAMPAIGN_DIR"""
import json, pathlib, sys
rows = [tuple(map(float, l.split())) for l in open(sys.argv[1]) if len(l.split()) == 5]
res = json.loads((pathlib.Path(sys.argv[2]) / "results.json").read_text())
for r in res:
    s, e = r["write_start_unix"], r["write_end_unix"]
    w = [x for x in rows if s <= x[0] <= e]
    if len(w) < 10:
        continue
    per = []
    for a, b in zip(w, w[1:]):
        n = b[1] - a[1]
        if n >= 20:
            per.append((b[2] - a[2]) / n)
    if not per:
        continue
    fast = sum(1 for x in per if x < 0.4); mid = sum(1 for x in per if 0.4 <= x <= 1.0)
    slow = len(per) - fast - mid
    spells, cur = [], 0
    for x in per:
        if x < 0.4:
            cur += 1
        elif cur:
            spells.append(cur); cur = 0
    if cur:
        spells.append(cur)
    spells = sorted((c / 10 for c in spells), reverse=True)
    t = len(per)
    q = sorted(per); qs = " ".join(f"{q[int(len(q)*f)]:.2f}" for f in (0.1, 0.25, 0.5, 0.75, 0.9))
    print(f"{r['index']} b{r['block']} {r['variant']} samples={t} fast={100*fast/t:.0f}% mid={100*mid/t:.0f}% slow={100*slow/t:.0f}% "
          f"q10..q90={qs} fast runs={len(spells)} longest(s)={' '.join(f'{x:.1f}' for x in spells[:6])}")
