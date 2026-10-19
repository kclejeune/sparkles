#!/usr/bin/env python3
"""The X08 profile gate (§3.3) from the runs of profiles.sh.

    profiles.py OUT

For each mode, query and profile (`default`, `small`), the median of the per-round medians
is compared with that of the defaults before X08 (`earlier`). A query regresses when that
ratio is above 1.05 and every round of the profile was slower than every round of the
earlier defaults. A query that failed in any round is reported as such. Prints a Markdown
table and writes OUT/gate.json, and exits with 1 when a profile has a regression or a
failure.
"""

import glob
import json
import os
import re
import statistics
import sys

out = sys.argv[1]
runs = {}  # (mode, config, query) -> [round medians], or "error"
for path in glob.glob(os.path.join(out, "*", "*-r*.json")):
    mode = os.path.basename(os.path.dirname(path))
    m = re.match(r"(.+)-r(\d+)\.json$", os.path.basename(path))
    if not m:
        continue
    with open(path) as f:
        res = json.load(f)
    for q, v in res.items():
        k = (mode, m.group(1), q)
        if "error" in v or runs.get(k) == "error":
            runs[k] = "error"
        else:
            runs.setdefault(k, []).append(v["median"])

gate, bad = [], False
for mode in sorted({k[0] for k in runs}):
    print(f"\n## {mode}\n")
    print("| Query | Earlier (ms) | Default (ms) | Ratio | Small (ms) | Ratio | Verdict |")
    print("|---|---:|---:|---:|---:|---:|---|")
    for q in sorted({k[2] for k in runs if k[0] == mode}):
        base = runs.get((mode, "earlier", q))
        cells, verdicts = [], []
        if not isinstance(base, list):
            print(f"| `{q}` | failed | | | | | the earlier defaults failed |")
            continue
        bm = statistics.median(base)
        for c in ("default", "small"):
            v = runs.get((mode, c, q))
            if not isinstance(v, list):
                cells += ["failed", ""]
                verdicts.append(f"{c} failed")
                gate.append({"mode": mode, "query": q, "profile": c, "verdict": "failed"})
                bad = True
                continue
            cm = statistics.median(v)
            ratio = cm / bm if bm else float("nan")
            regressed = ratio > 1.05 and min(v) > max(base)
            if regressed:
                verdicts.append(f"{c} regresses")
                bad = True
            cells += [f"{cm:.2f}", f"{ratio:.3f}"]
            gate.append({"mode": mode, "query": q, "profile": c, "earlier_ms": base, "profile_ms": v,
                         "ratio": round(ratio, 4), "verdict": "regression" if regressed else "ok"})
        print(f"| `{q}` | {bm:.2f} | {' | '.join(cells)} | {', '.join(verdicts) or 'ok'} |")
with open(os.path.join(out, "gate.json"), "w") as f:
    json.dump(gate, f, indent=1)
sys.exit(1 if bad else 0)
