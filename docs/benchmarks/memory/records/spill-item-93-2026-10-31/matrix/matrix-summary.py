#!/usr/bin/env python3
"""matrix-summary.py OUT: medians over the runs of each binary, query and cell."""
import json, re, statistics, sys
from collections import defaultdict
from pathlib import Path
out = Path(sys.argv[1])
cells = defaultdict(list)
for log in sorted(out.glob("*-*.log")):
    m = re.match(r"(\d+)-(.+?)-(ordered-join|optional|minus|distinct-order|pdistinct-scan|pdistinct-join)-(.+)\.log$", log.name)
    if not m:
        continue
    text = log.read_text()
    line = next((l for l in text.splitlines() if l.startswith("metrics ")), None)
    if not line:
        cells[(m[3], m[4], m[2])].append(None); continue
    d = json.loads(line[8:])
    counters = re.findall(r'"operator":"(\w+)"[^{]*?"counters":\{([^}]*)\}', text)
    cells[(m[3], m[4], m[2])].append((d["cursor_ms"], d["eager_ms"], d["cursor_peak_bytes"], d.get("spill_written_bytes_all_rounds", 0) / 3, d["cursor_digest"] == d["eager_digest"], counters[:3]))
print("| query | cell | binary | cursor ms | eager ms | peak MB | written MB per run | answers match |")
print("|---|---|---|---|---|---|---|---|")
for (q, c, b), runs in sorted(cells.items()):
    ok = [r for r in runs if r]
    med = lambda i: statistics.median(r[i] for r in ok)
    print(f"| {q} | {c} | {b} | {med(0):.1f} | {med(1):.1f} | {med(2)/1e6:.1f} | {med(3)/1e6:.1f} | {all(r[4] for r in ok) and len(ok)==len(runs)} |")
