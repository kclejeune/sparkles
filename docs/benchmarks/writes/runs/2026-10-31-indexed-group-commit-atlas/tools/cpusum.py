#!/usr/bin/env python3
"""Instructions, cycles and CPU time per serial commit from cpuab.sh runs, by mode.
Usage: cpusum.py RUNDIR..."""
import json, pathlib, statistics, sys
for root in sys.argv[1:]:
    rows = {"ordinary": [], "grouped": []}
    for d in sorted(pathlib.Path(root).glob("r*-*")):
        mode = d.name.rsplit("-", 1)[1]
        ev = {}
        for line in (d / "perf.csv").read_text().splitlines():
            f = line.split(",")
            if len(f) > 2 and f[0] and f[0][0].isdigit():
                name = f[2].replace("cpu_core/", "").replace("/u", ":u").rstrip("/")
                ev[name] = float(f[0])
        c = json.loads((d / "client.json").read_text().strip().splitlines()[-1])
        n = c["changes"] + 200
        rows[mode].append((ev.get("instructions:u", 0) / n, ev.get("cycles:u", 0) / n,
                           ev.get("task-clock", 0) / n, c["writers"]["p50_ms"], c["writers"]["p90_ms"], d.name))
    for mode, rs in rows.items():
        if not rs:
            continue
        med = lambda i: statistics.median(r[i] for r in rs)
        print(f"{root} {mode:8s} n={len(rs)} instr/commit={med(0):.0f} cycles/commit={med(1):.0f} "
              f"cpu-ms/commit={med(2):.4f} p50={med(3):.3f} p90={med(4):.3f}")
        for r in rs:
            print(f"    {r[5]:22s} instr={r[0]:.0f} cycles={r[1]:.0f} p50={r[3]:.3f} p90={r[4]:.3f}")
