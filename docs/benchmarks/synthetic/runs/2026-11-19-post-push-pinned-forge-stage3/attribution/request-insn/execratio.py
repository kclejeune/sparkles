# usage: execratio.py RUN...: per query, median over rounds of the round ratio of B's to A's median exec_ms (stanley, under load)
import csv, sys, statistics as st
from collections import defaultdict
for run in sys.argv[1:]:
    d = defaultdict(list)
    for x in csv.DictReader(open(f"{run}/samples.tsv"), delimiter="\t"):
        d[x["query"], x["round"], x["variant"]].append(float(x["exec_ms"]))
    qs = sorted({k[0] for k in d}); rs = sorted({k[1] for k in d}, key=int)
    out = []
    for q in qs:
        rr = [st.median(d[q, r, "B"]) / st.median(d[q, r, "A"]) for r in rs if d[q, r, "A"] and d[q, r, "B"]]
        out.append(f"{q} {st.median(rr):.3f} [{min(rr):.3f}-{max(rr):.3f}]")
    print(run.split("/")[-1], " | ".join(out))
