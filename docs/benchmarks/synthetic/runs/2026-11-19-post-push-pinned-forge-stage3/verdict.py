#!/usr/bin/env python3
"""verdict.py pinned-summary.json: the combined verdict of each row of analyze.py's summary.

analyze.py's d4 field applies D4 when both arm medians are under 0.3 ms and otherwise only
tests the ratio against 1.05. The combined rule for a query above 0.3 ms also calls a ratio
below 1.05 a confirmed cost when B is slower in every round. A confirmed cost passes the
check and still needs a roadmap row."""
import json, sys
for o in json.load(open(sys.argv[1])):
    under = max(o["a_ms"], o["b_ms"]) < 0.3
    if under:
        v = "pass (D4)" if o["d4"] == "pass" else "FAIL (D4)"
    elif o["ratio"] >= 1.05:
        v = "FAIL (ratio 1.05 or more)"
    elif o["rounds_slower"] == o["rounds"]:
        v = "confirmed cost (below 1.05, slower in every round)"
    else:
        v = "pass (below 1.05, not slower in every round)"
    print(f"{o['run']:22} {o['query']:20} {o['ratio']:.3f} slower {o['rounds_slower']}/{o['rounds']} {v}")
