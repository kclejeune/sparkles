#!/usr/bin/env python3
"""Inputs and queries for the cursor BIND reuse matrix of roadmap item 293.

`gen-bind-reuse.py data [SCALES]` writes N-Triples to stdout, one predicate per term kind,
repetition shape and scale. Subject i is <urn:s%09d>, so a PSO scan yields rows in key
order, and every other subject has <urn:keep> for the joined queries.

`gen-bind-reuse.py queries DIR` writes one .rq per expression, shape and scale into DIR.
"""
import os
import random
import sys

SHAPES = ["unique", "one-early-duplicate", "low-repeat", "moderate-repeat", "stride-8",
          "stride-16", "late-skew", "sustained-skew", "global-repeat", "random-60"]
EXPRS = {  # name: (data kind, BIND expression, aggregate of ?b)
    "str": ("string", "STR(?v)", "SUM(STRLEN(?b))"),
    "iri": ("iri", "STR(?v)", "SUM(STRLEN(?b))"),
    "date": ("date", "FLOOR(YEAR(?v) / 10) * 10", "SUM(?b)"),
    "sha": ("string", "SHA256(STR(?v))", "MAX(?b)"),
    "int": ("int", "ABS(?v - 100000)", "SUM(?b)"),
}


def shapes(n):
    r = random.Random(n)
    # Uniform draws from a domain of 0.6 n give about 0.49 n distinct keys, so about half
    # the rows repeat a key seen earlier, sparsely at first.
    table = [r.randrange(max(1, n * 6 // 10)) for _ in range(n)]
    return {
        "unique": lambda i: i,
        "one-early-duplicate": lambda i: 0 if i == 1 else i,
        "low-repeat": lambda i: i - 1 if i > 0 and i % 64 == 0 else i,
        "moderate-repeat": lambda i: i - 1 if i > 0 and i % 4 == 0 else i,
        "stride-8": lambda i: i - 1 if i > 0 and i % 8 == 0 else i,
        "stride-16": lambda i: i - 1 if i > 0 and i % 16 == 0 else i,
        "late-skew": lambda i: i if i < 4096 else i % 8,
        "sustained-skew": lambda i: i * 7 % 32,
        "global-repeat": lambda i: i * 2654435761 % 32768,
        "random-60": lambda i: table[i],
    }


def term(kind, k):
    if kind == "string":
        return f'"value{k:09d}"'
    if kind == "int":
        return f'"{k}"^^<http://www.w3.org/2001/XMLSchema#integer>'
    if kind == "iri":
        return f"<urn:object:{k:09d}>"
    y, m, d = 1000 + k // 336, 1 + (k // 28) % 12, 1 + k % 28
    return f'"{y:04d}-{m:02d}-{d:02d}+00:00"^^<http://www.w3.org/2001/XMLSchema#date>'


def data(scales):
    out = sys.stdout
    for i in range(0, max(scales), 2):
        out.write(f"<urn:s{i:09d}> <urn:keep> \"k\" .\n")
    for n in scales:
        for name, f in shapes(n).items():
            for kind in ("string", "iri", "date", "int"):
                p = f"<urn:p/{kind}/{name}/{n}>"
                for i in range(n):
                    out.write(f"<urn:s{i:09d}> {p} {term(kind, f(i))} .\n")


def queries(out):
    os.makedirs(out, exist_ok=True)

    def write(name, kind, shape, n, bind, agg, join):
        keep = "?s <urn:keep> ?k . " if join else ""
        q = (f"SELECT (COUNT(?b) AS ?c) ({agg} AS ?t) WHERE {{ {keep}?s "
             f"<urn:p/{kind}/{shape}/{n}> ?v BIND({bind} AS ?b) }}\n")
        with open(f"{out}/{'join-' if join else ''}{name}-{shape}-{n}.rq", "w") as f:
            f.write(q)

    for n in (2048, 65536, 262144):
        for shape in SHAPES:
            for name, (kind, bind, agg) in EXPRS.items():
                write(name, kind, shape, n, bind, agg, False)
    for n in (65536, 262144):
        for shape in ("unique", "moderate-repeat", "global-repeat", "random-60"):
            for name in ("str", "sha"):
                kind, bind, agg = EXPRS[name]
                write(name, kind, shape, n, bind, agg, True)


if __name__ == "__main__":
    if len(sys.argv) >= 2 and sys.argv[1] == "data":
        data([int(x) for x in (sys.argv[2] if len(sys.argv) > 2 else "2048,65536,262144").split(",")])
    elif len(sys.argv) == 3 and sys.argv[1] == "queries":
        queries(sys.argv[2])
    else:
        sys.exit(__doc__)
