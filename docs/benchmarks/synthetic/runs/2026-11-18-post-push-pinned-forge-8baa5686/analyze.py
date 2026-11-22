#!/usr/bin/env python3
"""analyze.py RUN_DIR... : D4 verdicts of pinned A/B pairs from nsc-bench-remote.sh results.

ratio = median of all B exec samples / median of all A exec samples. Each of the ROUNDS
rounds (A,B,B,A) gives a round ratio, the median of its B samples over the median of its A
samples. The 95% interval is the t interval of the mean of the log round ratios, mapped back.
D4 (X14 SPEC): a query under 0.3 ms passes when the interval's upper end is at most 1.02
or its median added time is at most 0.5 us."""
import csv, json, math, statistics as st, sys
from collections import defaultdict
T975 = {23: 2.069, 11: 2.201, 7: 2.365, 3: 3.182, 1: 12.706}
def t975(df):
    if df < 1: return float("nan")
    if df in T975: return T975[df]
    return 1.96 + 2.4 / df  # close enough for df not in the table
out = []
for run in sys.argv[1:]:
    revs = open(f"{run}/revs.txt").read().split()
    rows = list(csv.DictReader(open(f"{run}/results/samples.tsv"), delimiter="\t"))
    ans = defaultdict(dict)
    for line in open(f"{run}/results/answers.tsv"):
        v, q, n, h = line.rstrip("\n").split("\t")[:4]
        ans[q][v] = (n, h)
    by = defaultdict(list); byr = defaultdict(list); byp = defaultdict(list); wall = defaultdict(list)
    for r in rows:
        q, v = r["query"], r["variant"]
        x = float(r["exec_ms"]); by[q, v].append(x); wall[q, v].append(float(r["wall_ms"]))
        byr[q, v, int(r["round"])].append(x); byp[q, v, int(r["round"]), r["epoch"]].append(x)
    for q in sorted({k[0] for k in by}):
        a, b = st.median(by[q, "A"]), st.median(by[q, "B"])
        rounds = sorted({k[2] for k in byr if k[0] == q})
        rr = [st.median(byr[q, "B", i]) / st.median(byr[q, "A", i]) for i in rounds]
        logs = [math.log(x) for x in rr]
        m = st.mean(logs); se = st.stdev(logs) / math.sqrt(len(logs)) if len(logs) > 1 else float("nan")
        lo, hi = math.exp(m - t975(len(logs) - 1) * se), math.exp(m + t975(len(logs) - 1) * se)
        pm = {v: [st.median(xs) for k, xs in byp.items() if k[0] == q and k[1] == v] for v in "AB"}
        spread = {v: (max(p) - min(p)) / st.median(p) for v, p in pm.items()}
        added_us = (b - a) * 1000
        under = max(a, b) < 0.3
        passed = (hi <= 1.02 or added_us <= 0.5) if under else (b / a < 1.05)
        out.append(dict(run=run.rstrip("/").split("/")[-1], A=revs[0], B=revs[1], query=q,
            a_ms=round(a, 5), b_ms=round(b, 5), ratio=round(b / a, 4),
            ci95=[round(lo, 4), round(hi, 4)], added_us=round(added_us, 3),
            rounds_slower=sum(x > 1 for x in rr), rounds=len(rr),
            round_ratio_range=[round(min(rr), 3), round(max(rr), 3)],
            spread_A=round(spread["A"], 3), spread_B=round(spread["B"], 3),
            wall_ratio=round(st.median(wall[q, "B"]) / st.median(wall[q, "A"]), 4),
            answers_equal=ans[q].get("A") == ans[q].get("B"), n_A=len(by[q, "A"]), n_B=len(by[q, "B"]),
            d4="pass" if passed else "FAIL"))
json.dump(out, sys.stdout, indent=1)
print()
for o in out:
    print(f"{o['run']:22} {o['query']:20} {o['a_ms']:.5f} {o['b_ms']:.5f} {o['ratio']:.3f} ci[{o['ci95'][0]:.3f},{o['ci95'][1]:.3f}] +{o['added_us']:.2f}us slower {o['rounds_slower']}/{o['rounds']} wall {o['wall_ratio']:.3f} ans={'eq' if o['answers_equal'] else 'DIFF'} {o['d4']}", file=sys.stderr)
