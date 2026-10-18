#!/usr/bin/env python3
"""The X08 A1 bound applied to the table that a1.sh writes.

usage: a1-bound.py a1.tsv

For each cache size, with medians over the repeats, the bound is RssAnon of one dataset
after the suite, plus 1.25 times the difference between the two servers' cache bytes, plus
nine times the anonymous memory of an idle opened dataset. The gate passes when RssAnon of
ten datasets after the suite is at most 1.10 times the bound.
"""

import csv
import statistics as st
import sys
from collections import defaultdict

CACHES = ("block_bytes", "result_bytes", "service_bytes", "history_bytes")


def main():
    d = defaultdict(list)
    with open(sys.argv[1]) as f:
        for r in csv.DictReader(f, delimiter="\t"):
            d[(int(r["datasets"]), int(r["cache_mb"]), r["suite"])].append(r)

    def med(rows, key):
        return st.median(float(r[key]) for r in rows)

    def cache_kib(rows):
        return st.median(sum(float(r[k] or 0) for k in CACHES) for r in rows) / 1024

    failed = 0
    print(
        "| cache MiB | RssAnon 1 | RssAnon 10 | cache 1 | cache 10 | idle per dataset | bound | ratio | gate |"
    )
    print("|--:|--:|--:|--:|--:|--:|--:|--:|---|")
    for mb in sorted({k[1] for k in d}, reverse=True):
        one, ten = d[(1, mb, "1")], d[(10, mb, "1")]
        idle = (
            med(d[(10, mb, "0")], "RssAnon_kB") - med(d[(1, mb, "0")], "RssAnon_kB")
        ) / 9
        c1, c10 = cache_kib(one), cache_kib(ten)
        bound = med(one, "RssAnon_kB") + 1.25 * (c10 - c1) + 9 * idle
        ratio = med(ten, "RssAnon_kB") / bound
        ok = ratio <= 1.10
        failed += not ok
        print(
            f"| {mb} | {med(one, 'RssAnon_kB'):.0f} | {med(ten, 'RssAnon_kB'):.0f} | {c1:.0f} | {c10:.0f} "
            f"| {idle:.0f} | {bound:.0f} | {ratio:.3f} | {'pass' if ok else 'FAIL'} |"
        )
    print("\nmemory in KiB")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
