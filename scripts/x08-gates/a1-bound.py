#!/usr/bin/env python3
"""The X08 A1 gate applied to the table that a1.sh writes.

usage: a1-bound.py a1.tsv

For each cache size, with medians over the repeats, the bound is RssAnon of one dataset
after the suite, plus 1.25 times the difference between the two servers' cache bytes, plus
nine times the anonymous memory of an idle opened dataset. The memory part passes when
RssAnon of ten datasets after the suite is at most 1.10 times the bound.

The gate also fails when a case is missing, when any suite request failed, when the
block cache held more than `--cache-mb` in any case, or, in a table that records the
datasets' block bytes, when their sum differs from the cache's weight.
"""

import csv
import statistics as st
import sys
from collections import defaultdict

CACHES = ("block_bytes", "result_bytes", "service_bytes", "history_bytes")


def main():
    d = defaultdict(list)
    problems = []
    with open(sys.argv[1]) as f:
        for r in csv.DictReader(f, delimiter="\t"):
            d[(int(r["datasets"]), int(r["cache_mb"]), r["suite"])].append(r)
            case = f"case {r['case']} repeat {r['rep']}"
            if int(r["errors"] or 0):
                problems.append(f"{case}: {r['errors']} suite requests failed")
            if int(r["block_bytes"]) > int(r["cache_mb"]) << 20:
                problems.append(
                    f"{case}: block cache holds {r['block_bytes']} bytes, over --cache-mb"
                )
            per_dataset = r.get("dataset_block_bytes")
            if per_dataset not in (None, "") and int(per_dataset) != int(
                r["block_bytes"]
            ):
                problems.append(
                    f"{case}: datasets' block bytes sum to {per_dataset}, the cache weighs {r['block_bytes']}"
                )

    def med(rows, key):
        return st.median(float(r[key]) for r in rows)

    def cache_kib(rows):
        return st.median(sum(float(r[k] or 0) for k in CACHES) for r in rows) / 1024

    sizes = sorted({k[1] for k in d}, reverse=True)
    if not sizes:
        problems.append("the table has no cases")
    print(
        "| cache MiB | RssAnon 1 | RssAnon 10 | cache 1 | cache 10 | idle per dataset | bound | ratio | memory |"
    )
    print("|--:|--:|--:|--:|--:|--:|--:|--:|---|")
    for mb in sizes:
        missing = [
            k
            for k in ((1, mb, "1"), (10, mb, "1"), (1, mb, "0"), (10, mb, "0"))
            if k not in d
        ]
        if missing:
            problems.append(f"cache {mb} MiB: no rows for {missing}")
            continue
        one, ten = d[(1, mb, "1")], d[(10, mb, "1")]
        idle = (
            med(d[(10, mb, "0")], "RssAnon_kB") - med(d[(1, mb, "0")], "RssAnon_kB")
        ) / 9
        c1, c10 = cache_kib(one), cache_kib(ten)
        bound = med(one, "RssAnon_kB") + 1.25 * (c10 - c1) + 9 * idle
        ratio = med(ten, "RssAnon_kB") / bound
        ok = ratio <= 1.10
        if not ok:
            problems.append(
                f"cache {mb} MiB: ten datasets use {ratio:.3f} times the bound"
            )
        print(
            f"| {mb} | {med(one, 'RssAnon_kB'):.0f} | {med(ten, 'RssAnon_kB'):.0f} | {c1:.0f} | {c10:.0f} "
            f"| {idle:.0f} | {bound:.0f} | {ratio:.3f} | {'pass' if ok else 'FAIL'} |"
        )
    print("\nmemory in KiB")
    for p in problems:
        print(f"FAIL: {p}")
    print("the A1 gate fails" if problems else "the A1 gate passes")
    sys.exit(1 if problems else 0)


if __name__ == "__main__":
    main()
