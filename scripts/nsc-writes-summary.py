#!/usr/bin/env python3
"""Summary of a scripts/nsc-writes.sh run, as Markdown.

    nsc-writes-summary.py OUT VARIANT [VARIANT...]

Reads OUT/results/runs.tsv (one write-bench result per run) and prints, for every scenario,
the median over the rounds of each variant's figures and the ratio to the first variant,
then the range over the rounds. Runs whose receipts were not contiguous or that failed are
listed.
"""
import json
import statistics
import sys
from collections import defaultdict

out, variants = sys.argv[1], sys.argv[2:]
rows = defaultdict(lambda: defaultdict(list))
bad = []
scenarios = []
with open(f"{out}/results/runs.tsv") as f:
    next(f)
    for line in f:
        rnd, v, sc, res = line.rstrip("\n").split("\t", 3)
        if sc not in scenarios:
            scenarios.append(sc)
        try:
            d = json.loads(res)
        except ValueError:
            d = {"error": True}
        if d.get("error") or not d.get("receipts_contiguous", False) or d.get("errors", 1):
            bad.append(f"round {rnd} {v} {sc}: {res[:200]}")
            continue
        rows[sc][v].append(d)


def med(xs):
    return statistics.median(xs) if xs else float("nan")


def figures(runs):
    w = [r["writers"] for r in runs]
    f = {
        "transactions/s": [r["tps"] for r in runs],
        "writer p50 ms": [x["p50_ms"] for x in w],
        "writer p90 ms": [x["p90_ms"] for x in w],
        "writer p99 ms": [x["p99_ms"] for x in w],
    }
    if runs and runs[0].get("readers"):
        rd = [r["readers"] for r in runs]
        f["reader p50 ms"] = [x["p50_ms"] for x in rd]
        f["reader p99 ms"] = [x["p99_ms"] for x in rd]
    if runs and len(runs[0].get("per_client", [])) > 1:
        f["max share deviation"] = [r["max_share_deviation"] for r in runs]
    return f


print("# Durable commit A/B\n")
for sc in scenarios:
    print(f"## {sc}\n")
    figs = {v: figures(rows[sc][v]) for v in variants if rows[sc][v]}
    if not figs:
        print("no successful runs\n")
        continue
    names = list(next(iter(figs.values())).keys())
    first = variants[0]
    head = "| " + " | ".join(["Measure"] + [f"{v} median (range)" for v in variants] + [f"{v}/{first}" for v in variants[1:]]) + " |"
    print(head)
    print("|---|" + "---:|" * (len(variants) * 2 - 1))
    for n in names:
        cells = [n]
        for v in variants:
            xs = figs.get(v, {}).get(n, [])
            cells.append(f"{med(xs):.3f} ({min(xs):.3f}-{max(xs):.3f})" if xs else "-")
        base = med(figs.get(first, {}).get(n, []))
        for v in variants[1:]:
            m = med(figs.get(v, {}).get(n, []))
            cells.append(f"{m / base:.3f}" if base and base == base else "-")
        print("| " + " | ".join(cells) + " |")
    print()
if bad:
    print("## Failed or discarded runs\n")
    for b in bad:
        print(f"- {b}")
