#!/usr/bin/env python3
"""The effect of X over Y from two A/B runs in opposite orders, which cancels the bias of
the second position, and the X08 latency gate applied to it.

usage: combine.py xy.tsv yx.tsv [same.tsv]

xy.tsv has A=Y and B=X, so its ratio B/A is effect times bias, and yx.tsv has A=X and B=Y,
so its ratio is bias over effect. The estimate is sqrt(m1/m2), where m1 and m2 are the
medians of each run's per-round ratios of median execution times, with a 95% percentile
bootstrap over the rounds of both runs. A query passes when the upper end of the interval
is at most 1.02 or when its median added time is at most 0.5 microseconds. With a run of
one setting against itself, its position bias is printed too.
"""

import random
import statistics as st
import sys
from collections import defaultdict


def rounds(path):
    """Per query, the ratio B/A of each round's medians and the median time of Y (A)."""
    d = defaultdict(lambda: defaultdict(lambda: defaultdict(list)))
    with open(path) as f:
        hdr = f.readline().rstrip("\n").split("\t")
        ix = {h: i for i, h in enumerate(hdr)}
        for line in f:
            p = line.rstrip("\n").split("\t")
            if p[ix["exec_ms"]] == "error":
                continue
            d[p[ix["query"]]][int(p[ix["round"]])][p[ix["variant"]]].append(
                float(p[ix["exec_ms"]])
            )
    out = {}
    for q, rs in d.items():
        pairs = [(v["A"], v["B"]) for _, v in sorted(rs.items()) if v["A"] and v["B"]]
        out[q] = (
            [st.median(b) / st.median(a) for a, b in pairs],
            st.median([t for a, _ in pairs for t in a]),
        )
    return out


def main():
    rng = random.Random(42)
    r1, r2 = rounds(sys.argv[1]), rounds(sys.argv[2])
    same = rounds(sys.argv[3]) if len(sys.argv) > 3 else {}
    print(
        "| query | Y ms | X/Y | 95% interval | added µs | first order | second order | bias | gate |"
    )
    print("|---|--:|--:|---|--:|--:|--:|--:|---|")
    failed = 0
    for q in r1:
        if q not in r2:
            continue
        x, y = r1[q][0], r2[q][0]
        boots = sorted(
            (st.median(rng.choices(x, k=len(x))) / st.median(rng.choices(y, k=len(y))))
            ** 0.5
            for _ in range(10000)
        )
        est = (st.median(x) / st.median(y)) ** 0.5
        base = r1[q][1]
        added_us = (est - 1) * base * 1000
        hi = boots[9749]
        ok = hi <= 1.02 or added_us <= 0.5
        failed += not ok
        bias = f"{st.median(same[q][0]):.4f}" if q in same else ""
        print(
            f"| {q} | {base:.4f} | {est:.4f} | {boots[249]:.4f}-{hi:.4f} | {added_us:.2f} "
            f"| {st.median(x):.4f} | {st.median(y):.4f} | {bias} | {'pass' if ok else 'FAIL'} |"
        )
    print(f"\n{failed} queries fail the gate")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
