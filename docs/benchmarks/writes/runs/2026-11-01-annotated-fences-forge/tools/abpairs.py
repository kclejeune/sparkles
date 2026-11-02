#!/usr/bin/env python3
"""Paired A/B ratios of an ABBA campaign: per block, B1/A0 and B2/A3 for tps and fence
time. Usage: abpairs.py CAMPAIGN [scenario]"""
import json, pathlib, re, statistics, sys
root = pathlib.Path(sys.argv[1]); scen = sys.argv[2] if len(sys.argv) > 2 else "writers"
res = json.loads((root / "results.json").read_text())
def fence(cell, run):
    d = root / cell / run
    def tot(p):
        s = c = 0.0
        for l in p.read_text().splitlines():
            m = re.match(r'sparkles_commit_sync_seconds_(sum|count)\{[^}]*path="group"\} (\S+)', l)
            if m:
                if m.group(1) == "sum": s += float(m.group(2))
                else: c += float(m.group(2))
        return s, c
    a, b = tot(d / "metrics.txt"), tot(d / "before" / "metrics.txt")
    return (a[0] - b[0]) / (a[1] - b[1]) * 1000 if a[1] > b[1] else None
cells = sorted({(r["index"], r["people"]) for r in res})
for idx, people in cells:
    rows = [r for r in res if r["index"] == idx and r["people"] == people and r["scenario"] == scen]
    by = {}
    for r in rows:
        by.setdefault(r["block"], []).append(r)
    tps, fen, p50, p90 = [], [], [], []
    for b, rs in sorted(by.items()):
        if len(rs) != 4: continue
        for bi, ai in ((1, 0), (2, 3)):
            tps.append(rs[bi]["tps"] / rs[ai]["tps"])
            p50.append(rs[bi]["writers"]["p50_ms"] / rs[ai]["writers"]["p50_ms"])
            p90.append(rs[bi]["writers"]["p90_ms"] / rs[ai]["writers"]["p90_ms"])
            cell = f"{idx}-{people}"
            fa, fb = fence(cell, f"b{b}-{scen}-{ai}"), fence(cell, f"b{b}-{scen}-{bi}")
            if fa and fb: fen.append(fb / fa)
    med = lambda x: statistics.median(x) if x else float("nan")
    print(f"{idx}-{people} {scen}: pairs={len(tps)} tps B/A median {med(tps):.3f} [{' '.join(f'{x:.2f}' for x in tps)}] "
          f"p50 {med(p50):.3f} p90 {med(p90):.3f} fence {med(fen):.3f}")
    for v in ("A", "B"):
        t = [r["tps"] for r in rows if r.get("variant") == v]
        print(f"   {v}: tps median {med(t):.1f} range {min(t):.0f}-{max(t):.0f}")
