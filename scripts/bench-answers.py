#!/usr/bin/env python3
"""Fingerprint an engine's answer to a benchmark query, for cross-engine comparison.

    bench-answers.py <endpoint> <query-file> <answers.json> <query-name> <engine> [--count-only]

Fetches SPARQL JSON results (`application/sparql-results+json`) and records three
things for <engine> under <query-name> in <answers.json>:

* `rows`: the number of solutions;
* `exact`: a digest of the solution multiset by RDF term identity. The lexical form,
  datatype and language tag are kept; only differences that RDF itself treats as
  identical are normalized: a missing datatype means `xsd:string`, and language tags
  compare case-insensitively. Blank nodes compare by position, not label, which is a
  weaker policy than graph isomorphism but enough for these queries.
* `value`: the same digest with numeric literals compared by value, so for example
  `"34.50"^^xsd:decimal` equals `"34.5"^^xsd:decimal`, rounded to 12 significant digits
  (engines print non-terminating decimals such as averages to different precisions).

Solutions are compared as multisets over variables sorted by name, since ORDER BY ties
may be broken differently and engines list the result variables in different orders.
`--count-only` records just the row count, for LIMIT-without-ORDER probes whose
solutions are legitimately engine-dependent. A failed request records `error`.
"""

import hashlib
import json
import os
import sys
import urllib.parse
import urllib.request
from decimal import Decimal, InvalidOperation, localcontext

XSD = "http://www.w3.org/2001/XMLSchema#"
NUMERIC = {XSD + t for t in (
    "integer", "decimal", "double", "float", "int", "long", "short", "byte",
    "nonNegativeInteger", "positiveInteger", "negativeInteger", "nonPositiveInteger",
    "unsignedLong", "unsignedInt", "unsignedShort", "unsignedByte")}


def term(t, by_value):
    if t is None:
        return ("undef",)
    kind = t.get("type")
    if kind == "uri":
        return ("uri", t["value"])
    if kind == "bnode":
        return ("bnode",)
    if kind in ("literal", "typed-literal"):
        lang = t.get("xml:lang")
        if lang:
            return ("lang", t["value"], lang.lower(), t.get("its:dir", ""))
        dt = t.get("datatype", XSD + "string")
        if by_value and dt in NUMERIC:
            try:
                # 12 significant digits: the precision of a non-terminating decimal
                # (an AVG) is implementation-defined
                with localcontext() as c:
                    c.prec = 12
                    v = +Decimal(t["value"])
                return ("num", str(v.normalize()) if v == v else "NaN")
            except InvalidOperation:
                pass
        return ("lit", t["value"], dt)
    if kind == "triple":
        v = t["value"]
        return ("triple",) + tuple(term(v[k], by_value) for k in ("subject", "predicate", "object"))
    return ("other", json.dumps(t, sort_keys=True))


def digest(rows):
    h = hashlib.sha256()
    for r in sorted(rows):
        h.update(repr(r).encode())
    return h.hexdigest()[:16]


def main():
    endpoint, qfile, out, name, engine = sys.argv[1:6]
    count_only = "--count-only" in sys.argv[6:]
    query = open(qfile).read()
    req = urllib.request.Request(
        endpoint if endpoint.startswith("http") else "http://" + endpoint,
        data=urllib.parse.urlencode({"query": query}).encode(),
        headers={"Accept": "application/sparql-results+json",
                 "Content-Type": "application/x-www-form-urlencoded"},
    )
    try:
        with urllib.request.urlopen(req, timeout=float(os.environ.get("MAX_TIME", "300"))) as r:
            res = json.load(r)
        # variable order in the head is not part of a solution (engines differ)
        vars_ = sorted(res["head"].get("vars", []))
        bindings = res["results"]["bindings"]
        rec = {"rows": len(bindings)}
        if not count_only:
            for key, by_value in (("exact", False), ("value", True)):
                rec[key] = digest(tuple(term(b.get(v), by_value) for v in vars_) for b in bindings)
    except Exception as e:  # noqa: BLE001 - any failure is recorded, not raised
        rec = {"rows": "error", "error": str(e)[:200]}
    d = json.load(open(out)) if os.path.exists(out) else {}
    d.setdefault(name, {})[engine] = rec
    json.dump(d, open(out, "w"), indent=1)
    print(rec["rows"])


if __name__ == "__main__":
    main()
