#!/usr/bin/env python3
"""Summarize an interleaved A/B run of scripts/nsc-bench.sh.

    scripts/nsc-bench-summary.py OUT

Reads OUT/manifest.txt and OUT/results/{samples,answers,load}.tsv and env.txt, prints a
Markdown summary and writes OUT/summary.json. For each query and metric (the server's
execution time and the client's wall time), the summary gives the median of every timed
request of A and of B, their ratio B/A, the ratio within each A,B,B,A round, and the spread
of each variant between server processes (the largest median of one process over the
smallest, minus one). The last row is the geometric mean of the ratios.
"""

import csv
import json
import math
import statistics
import sys
from collections import defaultdict
from pathlib import Path


def read_tsv(path):
    if not path.exists():
        return []
    with path.open() as f:
        return list(csv.reader(f, delimiter="\t"))


def main():
    out = Path(sys.argv[1])
    res = out / "results"
    rows = read_tsv(res / "samples.tsv")
    header, rows = rows[0], rows[1:]
    col = {name: i for i, name in enumerate(header)}

    # samples[metric][query][variant] -> list of (round, epoch, value)
    samples = {m: defaultdict(lambda: defaultdict(list)) for m in ("exec_ms", "wall_ms")}
    errors = defaultdict(int)
    order = []
    for r in rows:
        q, v = r[col["query"]], r[col["variant"]]
        if q not in order:
            order.append(q)
        if r[col["exec_ms"]] == "error":
            errors[(q, v)] += 1
            continue
        for m in samples:
            samples[m][q][v].append((int(r[col["round"]]), int(r[col["epoch"]]), float(r[col["exec_ms"] if m == "exec_ms" else col["wall_ms"]])))

    def stats(per_variant):
        a, b = per_variant.get("A", []), per_variant.get("B", [])
        if not a or not b:
            return None
        med = lambda xs: statistics.median(x[2] for x in xs)
        rounds = sorted({x[0] for x in a} & {x[0] for x in b})
        per_round = [med([x for x in b if x[0] == k]) / med([x for x in a if x[0] == k]) for k in rounds]

        def spread(xs):
            epochs = defaultdict(list)
            for x in xs:
                epochs[x[1]].append(x[2])
            meds = [statistics.median(v) for v in epochs.values()]
            return max(meds) / min(meds) - 1 if min(meds) > 0 else float("nan")

        ma, mb = med(a), med(b)
        return {"a": ma, "b": mb, "ratio": mb / ma, "rounds": per_round, "spread_a": spread(a), "spread_b": spread(b), "n_a": len(a), "n_b": len(b)}

    summary = {"queries": {}, "geomean": {}}
    for m in samples:
        for q in order:
            s = stats(samples[m][q])
            if s:
                summary["queries"].setdefault(q, {})[m] = s
        ratios = [summary["queries"][q][m]["ratio"] for q in order if m in summary["queries"].get(q, {})]
        if ratios:
            summary["geomean"][m] = math.exp(sum(math.log(r) for r in ratios) / len(ratios))

    p = print
    manifest = (out / "manifest.txt").read_text().splitlines() if (out / "manifest.txt").exists() else []
    p("# Interleaved A/B on Namespace")
    p()
    for line in manifest:
        p(f"- {line}")
    env = (res / "env.txt").read_text().splitlines() if (res / "env.txt").exists() else []
    for line in env:
        p(f"- {line}")
    p()

    answers = read_tsv(res / "answers.tsv")
    by_q = defaultdict(dict)
    for v, q, n, h in answers:
        by_q[q][v] = (n, h)
    diff = [q for q in order if len(set(by_q[q].values())) != 1 or "error" in str(by_q[q])]
    if answers:
        if diff:
            p("Answers differ or failed for: " + ", ".join(f"{q} (A {by_q[q].get('A')}, B {by_q[q].get('B')})" for q in diff) + ".")
        else:
            p(f"A and B returned the same sorted TSV answers to all {len(by_q)} queries.")
        p()
    if errors:
        p("Failed timed requests: " + ", ".join(f"{q} {v} {n}" for (q, v), n in sorted(errors.items())) + ".")
        p()
    load = read_tsv(res / "load.tsv")
    try:
        (_, ta, ka), (_, tb, kb) = load
        ta, tb = float(ta), float(tb)
        p(f"Load: A {ta:.2f} s, B {tb:.2f} s (B/A {tb / ta:.3f}). Store size: A {int(ka) // 1024} MiB, B {int(kb) // 1024} MiB.")
        p()
    except (ValueError, ZeroDivisionError):
        pass

    titles = {"exec_ms": "Server execution time (execMs, ms)", "wall_ms": "Client wall time (curl time_total, ms)"}
    for m, title in titles.items():
        p(f"## {title}")
        p()
        p("| query | A | B | B/A | change | per-round B/A | A spread | B spread |")
        p("|---|--:|--:|--:|--:|---|--:|--:|")
        for q in order:
            s = summary["queries"].get(q, {}).get(m)
            if not s:
                p(f"| {q} | error | | | | | | |")
                continue
            rounds = " ".join(f"{r:.3f}" for r in s["rounds"])
            p(f"| {q} | {s['a']:.3f} | {s['b']:.3f} | {s['ratio']:.3f} | {(s['ratio'] - 1) * 100:+.1f}% | {rounds} | {s['spread_a'] * 100:.1f}% | {s['spread_b'] * 100:.1f}% |")
        if m in summary["geomean"]:
            g = summary["geomean"][m]
            p(f"| **geometric mean** | | | {g:.3f} | {(g - 1) * 100:+.1f}% | | | |")
        p()
    p("The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.")
    (out / "summary.json").write_text(json.dumps(summary, indent=1))


if __name__ == "__main__":
    main()
