#!/usr/bin/env python3
"""The answer checks of latency.sh's base-and-tip form over its runs.

usage: answers.py --queries "q ..." RUN_DIR...

Each RUN_DIR holds the results of one scripts/nsc-bench-remote.sh run with ORDER_DIGEST
and WARM_DIGEST set. Every variant of every run must give each query one untimed TSV
answer, the same rows and order digest as every other, and every timed request of every
variant must have exactly one native digest, the same for all of a query's requests,
whose row count matches the TSV answer. A LIMIT without ORDER BY may return other rows in
each answer, so for it only the row count, the schema and the number of distinct rows must
agree. The check fails when a run holds other queries than --queries names, or when any
file is missing.
"""

import re
import sys
from collections import defaultdict
from pathlib import Path

def blank_tokens(query):
    """The query with every comment, IRI and string literal, long or short, replaced by
    spaces of the same length, so that words inside them are not read as syntax."""
    out, i, n = [], 0, len(query)
    while i < n:
        c = query[i]
        if c == "\\":
            # an escape outside a string, such as a prefixed name's \#, is no comment
            out.append(query[i:i + 2])
            i += 2
            continue
        if c == "#":
            j = query.find("\n", i)
            j = n if j < 0 else j
        elif c == "<" and (m := re.match(r"<[^<>\"{}|^`\\\s]*>", query[i:])):
            j = i + m.end()
        elif query.startswith('"""', i) or query.startswith("'''", i):
            q3, j = query[i:i + 3], i + 3
            while j < n and not query.startswith(q3, j):
                j += 2 if query[j] == "\\" else 1
            j = min(n, j + 3)
        elif c in "\"'":
            j = i + 1
            while j < n and query[j] != c and query[j] != "\n":
                j += 2 if query[j] == "\\" else 1
            j = min(n, j + 1)
        else:
            out.append(c)
            i += 1
            continue
        out.append(" " * (j - i))
        i = j
    return "".join(out)


def unordered_slice(query):
    """Whether the query has a top-level LIMIT and no top-level ORDER BY."""
    q = blank_tokens(query)
    depth, limit, ordered = 0, False, False
    for m in re.finditer(r"[{}]|\border\s+by\b|\blimit\s+\d+", q, re.I):
        t = m.group()
        if t == "{":
            depth += 1
        elif t == "}":
            depth -= 1
        elif depth == 0:
            if t.lower().startswith("order"):
                ordered = True
            else:
                limit = True
    return limit and not ordered


def tsv(path, problems):
    if not path.exists():
        problems.append(f"{path} is missing")
        return []
    return [line.rstrip("\n").split("\t") for line in path.read_text().splitlines() if line]


def main():
    args = sys.argv[1:]
    if args[:1] != ["--queries"] or len(args) < 3:
        sys.exit(__doc__)
    inventory = set(args[1].split())
    runs = [Path(a) for a in args[2:]]
    problems = []
    untimed = defaultdict(set)
    timed = defaultdict(set)
    unordered = set()
    for q in inventory:
        f = runs[0] / "queries" / f"{q}.rq"
        if f.exists() and unordered_slice(f.read_text()):
            unordered.add(q)
    for run in runs:
        res = run / "results"
        samples = tsv(res / "samples.tsv", problems)[1:]
        queries = {r[3] for r in samples}
        if queries != inventory:
            problems.append(f"{run.name} holds other queries than those staged: {sorted(queries ^ inventory)}")
        variants = {r[2] for r in samples}
        seen = defaultdict(set)
        for r in tsv(res / "answers.tsv", problems):
            v, q = r[0], r[1]
            if v in seen[q]:
                problems.append(f"{run.name}: {q} has two untimed answers of {v}")
            seen[q].add(v)
            if "error" in r[2:4]:
                problems.append(f"{run.name}: {q} failed in {v}")
            if re.search(r"missing:(?!0\b)\d+", "\t".join(r[4:])):
                problems.append(f"{run.name}: {q} returned rows of {v} that are not triples of the input")
            # the rows and the order digest, or for an unordered LIMIT the row count and the
            # distinct rows and schema of the order digest
            untimed[q].add((r[2],) + tuple(r[4:5]) if q in unordered else tuple(r[2:]))
        for q in queries:
            if seen[q] != variants:
                problems.append(f"{run.name}: {q} lacks an untimed answer of {sorted(variants - seen[q])}")
        want = {(r[1], r[2], r[3], r[4]) for r in samples}
        got = defaultdict(int)
        for r in tsv(res / "warm-answers.tsv", problems):
            if len(r) != 5:
                problems.append(f"{run.name}: a timed digest row has {len(r)} fields, not 5")
                continue
            got[(r[0], r[1], r[2], r[3])] += 1
            timed[r[2]].add(",".join(r[4].split(",")[:3]) if r[2] in unordered else r[4])
        lacking = want - set(got)
        if lacking:
            problems.append(f"{run.name}: {len(lacking)} timed requests have no digest")
        extra = set(got) - want
        if extra or any(n > 1 for n in got.values()):
            problems.append(f"{run.name}: timed digests of no timed request or repeated")
    for q in sorted(inventory):
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
