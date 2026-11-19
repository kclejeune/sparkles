#!/usr/bin/env python3
"""The answer checks of latency.sh's base-and-tip form over its runs.

usage: answers.py RUN_DIR...

Each RUN_DIR holds the results of one scripts/nsc-bench-remote.sh run with ORDER_DIGEST
and WARM_DIGEST set. Every variant of every run must give each query one untimed TSV
answer, the same rows and order digest as every other, and every timed request of every
variant must have exactly one native digest, the same for all of a query's requests,
whose row count matches the TSV answer. The check fails when a run holds other queries
than the first, or when any file is missing.
"""

import re
import sys
from collections import defaultdict
from pathlib import Path


def tsv(path, problems):
    if not path.exists():
        problems.append(f"{path} is missing")
        return []
    return [line.rstrip("\n").split("\t") for line in path.read_text().splitlines() if line]


def main():
    runs = [Path(a) for a in sys.argv[1:]]
    if not runs:
        sys.exit(__doc__)
    problems = []
    untimed = defaultdict(set)
    timed = defaultdict(set)
    inventory = None
    for run in runs:
        res = run / "results"
        samples = tsv(res / "samples.tsv", problems)[1:]
        queries = {r[3] for r in samples}
        if inventory is None:
            inventory = queries
        elif queries != inventory:
            problems.append(f"{run.name} holds other queries: {sorted(queries ^ inventory)}")
        variants = {r[2] for r in samples}
        seen = defaultdict(set)
        for r in tsv(res / "answers.tsv", problems):
            v, q = r[0], r[1]
            if v in seen[q]:
                problems.append(f"{run.name}: {q} has two untimed answers of {v}")
            seen[q].add(v)
            if "error" in r[2:4]:
                problems.append(f"{run.name}: {q} failed in {v}")
            untimed[q].add(tuple(r[2:]))
        for q in queries:
            if seen[q] != variants:
                problems.append(f"{run.name}: {q} lacks an untimed answer of {sorted(variants - seen[q])}")
        want = {(r[1], r[2], r[3], r[4]) for r in samples}
        got = defaultdict(int)
        for r in tsv(res / "warm-answers.tsv", problems):
            if len(r) < 5:
                problems.append(f"{run.name}: a timed digest names no repetition")
                continue
            got[(r[0], r[1], r[2], r[3])] += 1
            timed[r[2]].add(r[4])
        lacking = want - set(got)
        if lacking:
            problems.append(f"{run.name}: {len(lacking)} timed requests have no digest")
        extra = set(got) - want
        if extra or any(n > 1 for n in got.values()):
            problems.append(f"{run.name}: timed digests of no timed request or repeated")
    for q in sorted(inventory or ()):
        if len(untimed[q]) != 1:
            problems.append(f"{q}: the untimed answers differ: {sorted(untimed[q])}")
        digests = timed[q]
        one = next(iter(digests)) if len(digests) == 1 else ""
        rows = next(iter(untimed[q]))[0] if len(untimed[q]) == 1 else None
        if len(digests) != 1:
            problems.append(f"{q}: the timed answers differ: {sorted(digests)}")
        elif not (one.startswith("ask ") or one.split(" ")[0] == rows) or re.search(r"missing (?!0\b)\d+", one):
            problems.append(f"{q}: the timed answer {one!r} does not match the {rows} untimed rows")
    for p in problems:
        print(f"FAIL: {p}")
    print("the answers agree" if not problems else "the answers disagree")
    sys.exit(1 if problems else 0)


if __name__ == "__main__":
    main()
