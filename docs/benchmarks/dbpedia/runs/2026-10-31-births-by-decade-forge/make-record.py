#!/usr/bin/env python3
"""Build record.json from the five query passes of this run (dbpedia 50m, births-by-decade).

The hyperfine on forge writes per-run `measurements`, where scripts/bench-record.py reads
`times` and `exit_codes`. This script converts them, joins the timed requests of the five
passes into one result per engine, and builds cold-runs.json from each pass's cold.json.
The first two results of every pass file are an earlier pass that ran right after the loads
while the disks were still writing back (see README.md). They are skipped.

    make-record.py WORKDIR     (WORKDIR holds a copy of passes/ and manifest.json)
"""
import json, statistics, subprocess, sys
from pathlib import Path

d = Path(sys.argv[1])
res = d / "results"
res.mkdir(exist_ok=True)
final = json.load(open(d / "passes/5/births-by-decade.json"))["results"][2:]
assert len(final) == 10, len(final)
joined = {}
for r in final:
    j = joined.setdefault(r["name"], {"command": r["name"], "times": [], "exit_codes": []})
    for m in r["measurements"]:
        j["times"].append(m["time_wall_clock"]["value"])
        j["exit_codes"].append(m["exit_code"])
json.dump({"results": list(joined.values())}, open(res / "births-by-decade.json", "w"), indent=1)
cold = {}
for p in range(1, 6):
    for e, per in json.load(open(d / f"passes/{p}/cold.json")).items():
        cold.setdefault(e, []).append(per["births-by-decade"])
json.dump({e: {"births-by-decade": statistics.median(v)} for e, v in cold.items()}, open(res / "cold.json", "w"), indent=1)
json.dump({e: {"births-by-decade": {"times": v}} for e, v in cold.items()}, open(res / "cold-runs.json", "w"), indent=1)
for name in ("answers.json", "load-details.json", "load.json", "rss.json"):
    (res / name).write_bytes((d / "passes/5" / name).read_bytes())
json.dump({"settings": {"passes": 5, "note": "five passes of one cold request and ten timed requests after two warm-ups; the warm result joins the 50 timed requests"}},
          open(res / "extra.json", "w"), indent=1)
