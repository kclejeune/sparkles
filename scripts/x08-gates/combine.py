#!/usr/bin/env python3
"""The effect of the query memory pool from two A/B runs in opposite orders, which cancels
the bias of the second position, and the X08 latency gate applied to it.

usage: combine.py pool-on-second.tsv pool-on-first.tsv [same.tsv]

pool-on-second.tsv has the pool off as A and on as B, so its ratio B/A is the effect times
the bias. pool-on-first.tsv has the pool on as A and off as B, so its ratio is the bias
over the effect. The estimate is sqrt(m1/m2), where m1 and m2 are the medians of each
run's per-round ratios of median execution times, with a 95% percentile bootstrap over the
rounds of both runs. A query passes when the upper end of the interval is at most 1.02 or
when its median added time is at most 0.5 microseconds. With a run of one setting against
itself, its position bias is printed too.

The gate fails when the runs do not cover the same queries, when a query has fewer than
MIN_ROUNDS rounds in a run, or when any timed request failed.
"""

import random
import statistics as st
import sys
from collections import defaultdict

MIN_ROUNDS = 10


def rounds(path, problems):
    """Per query, the ratio B/A of each round's medians and the median time of A, and the
    set of queries the run holds."""
    d = defaultdict(lambda: defaultdict(lambda: defaultdict(list)))
    errors = defaultdict(int)
    with open(path) as f:
        hdr = f.readline().rstrip("\n").split("\t")
        ix = {h: i for i, h in enumerate(hdr)}
        for line in f:
            p = line.rstrip("\n").split("\t")
            if p[ix["exec_ms"]] == "error":
                errors[p[ix["query"]]] += 1
                continue
            d[p[ix["query"]]][int(p[ix["round"]])][p[ix["variant"]]].append(
                float(p[ix["exec_ms"]])
            )
    out = {}
    for q, rs in d.items():
        pairs = [(v["A"], v["B"]) for _, v in sorted(rs.items()) if v["A"] and v["B"]]
        if len(pairs) < MIN_ROUNDS:
            problems.append(
                f"{path}: {q} has {len(pairs)} rounds, fewer than {MIN_ROUNDS}"
            )
            continue
        out[q] = (
            [st.median(b) / st.median(a) for a, b in pairs],
            st.median([t for a, _ in pairs for t in a]),
        )
    for q, n in sorted(errors.items()):
        problems.append(f"{path}: {n} requests of {q} failed")
    return out, set(d) | set(errors)


def main():
    rng = random.Random(42)
    problems = []
    on_second, q1 = rounds(sys.argv[1], problems)
    on_first, q2 = rounds(sys.argv[2], problems)
    same = rounds(sys.argv[3], problems)[0] if len(sys.argv) > 3 else {}
    if q1 != q2:
        problems.append(f"the runs cover different queries: {sorted(q1 ^ q2)}")
    if not q1:
        problems.append("the runs hold no queries")
    print(
        "| query | pool off ms | on/off | 95% interval | added µs "
        "| pool-on-second ratio | pool-on-first ratio | same-build ratio | gate |"
    )
    print("|---|--:|--:|---|--:|--:|--:|--:|---|")
    for q in sorted(on_second.keys() & on_first.keys()):
        x, y = on_second[q][0], on_first[q][0]
        boots = sorted(
            (st.median(rng.choices(x, k=len(x))) / st.median(rng.choices(y, k=len(y))))
            ** 0.5
            for _ in range(10000)
        )
        est = (st.median(x) / st.median(y)) ** 0.5
        base = on_second[q][1]
        added_us = (est - 1) * base * 1000
        hi = boots[9749]
        ok = hi <= 1.02 or added_us <= 0.5
        if not ok:
            problems.append(
                f"{q}: on/off {est:.4f} up to {hi:.4f}, {added_us:.2f} µs added"
            )
        same_ratio = f"{st.median(same[q][0]):.4f}" if q in same else ""
        print(
            f"| {q} | {base:.4f} | {est:.4f} | {boots[249]:.4f}-{hi:.4f} | {added_us:.2f} "
            f"| {st.median(x):.4f} | {st.median(y):.4f} | {same_ratio} "
            f"| {'pass' if ok else 'FAIL'} |"
        )
    print()
    for p in problems:
        print(f"FAIL: {p}")
    print("the latency gate fails" if problems else "the latency gate passes")
    sys.exit(1 if problems else 0)


if __name__ == "__main__":
    main()
