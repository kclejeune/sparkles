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
  Geometry literals (`geo:wktLiteral`, `geo:geoJSONLiteral`) compare by their
  coordinates rounded to `GEO_DECIMALS` decimal places (default 6, about 0.1 m in
  degrees), whatever the spelling, the ring starts and directions, the direction of
  lines and the order of collection members; Z and M values are ignored, and the
  default CRS84 prefix is dropped.

Solutions are compared as multisets over variables sorted by name, since ORDER BY ties
may be broken differently and engines list the result variables in different orders.
`--count-only` records just the row count, for LIMIT-without-ORDER probes whose
solutions are legitimately engine-dependent. A failed request records `error`.
"""

import hashlib
import json
import os
import re
import sys
import urllib.parse
import urllib.request
from decimal import Decimal, InvalidOperation, localcontext

XSD = "http://www.w3.org/2001/XMLSchema#"
NUMERIC = {XSD + t for t in (
    "integer", "decimal", "double", "float", "int", "long", "short", "byte",
    "nonNegativeInteger", "positiveInteger", "negativeInteger", "nonPositiveInteger",
    "unsignedLong", "unsignedInt", "unsignedShort", "unsignedByte")}


GEO = "http://www.opengis.net/ont/geosparql#"
WKT, GEOJSON = GEO + "wktLiteral", GEO + "geoJSONLiteral"
CRS84 = "<http://www.opengis.net/def/crs/OGC/1.3/CRS84>"
GEO_DECIMALS = int(os.environ.get("GEO_DECIMALS", "6"))


TOKEN = re.compile(r"\s*(?:([A-Za-z]+)|(\()|(\))|(,)|([-+]?(?:\d+\.?\d*|\.\d+)(?:[eE][-+]?\d+)?))")


def _wkt_tokens(text):
    pos, out = 0, []
    while pos < len(text):
        m = TOKEN.match(text, pos)
        if not m or m.end() == pos:
            if text[pos:].strip() == "":
                break
            raise ValueError(f"unexpected {text[pos:pos + 10]!r}")
        pos = m.end()
        word, op, cl, comma, num = m.groups()
        out.append(("w", word.upper()) if word else ("(",) if op else (")",) if cl else (",",) if comma else ("n", float(num)))
    return out


def _parse_wkt(text):
    """WKT as (TYPE, body): body is a coordinate tuple, a list of bodies, or None (EMPTY)."""
    toks = _wkt_tokens(text)
    i = 0

    def peek():
        return toks[i] if i < len(toks) else (None,)

    def take(kind):
        nonlocal i
        t = peek()
        if t[0] != kind:
            raise ValueError(f"expected {kind}, got {t}")
        i += 1
        return t

    def items():
        # "(" item ("," item)* ")" where an item is a coordinate, a nested list or a tagged geometry
        take("(")
        out = []
        while True:
            t = peek()
            if t[0] == "n":
                nums = []
                while peek()[0] == "n":
                    nums.append(take("n")[1])
                out.append(tuple(round(x, GEO_DECIMALS) + 0.0 for x in nums[:2]))
            elif t[0] == "(":
                out.append(items())
            elif t[0] == "w":
                out.append(geom())
            else:
                raise ValueError(f"unexpected {t}")
            if peek()[0] == ",":
                take(",")
                continue
            take(")")
            return out

    def geom():
        typ = take("w")[1]
        while peek()[0] == "w" and peek()[1] in ("Z", "M", "ZM"):
            take("w")
        if peek()[0] == "w" and peek()[1] == "EMPTY":
            take("w")
            return (typ, None)
        return (typ, items())

    g = geom()
    if i != len(toks):
        raise ValueError("trailing input")
    return g


def _ring(coords):
    """A closed ring without its closing vertex, from its smallest vertex, in the
    direction that gives the smaller sequence (equal rings compare equal)."""
    pts = [tuple(c) for c in coords]
    if len(pts) > 1 and pts[0] == pts[-1]:
        pts = pts[:-1]
    if not pts:
        return ()
    k = pts.index(min(pts))
    fwd = pts[k:] + pts[:k]
    back = list(reversed(pts))
    k = back.index(min(back))
    back = back[k:] + back[:k]
    return tuple(min(fwd, back))


def _line(coords):
    pts = tuple(tuple(c) for c in coords)
    return min(pts, tuple(reversed(pts)))


def _poly(rings):
    if not rings:
        return ()
    return (_ring(rings[0]),) + tuple(sorted(_ring(r) for r in rings[1:]))


def _norm(g):
    """A canonical, comparable form of a parsed geometry."""
    typ, body = g
    if body is None:
        return (typ, "EMPTY")
    if typ == "POINT":
        return ("POINT", tuple(body[0]))
    if typ in ("LINESTRING", "LINEARRING"):
        return ("LINESTRING", _line(body))
    if typ in ("POLYGON", "TRIANGLE"):
        return ("POLYGON", _poly(body))
    if typ == "MULTIPOINT":
        return ("MULTIPOINT", tuple(sorted(tuple(p[0]) if isinstance(p, list) else tuple(p) for p in body)))
    if typ == "MULTILINESTRING":
        return ("MULTILINESTRING", tuple(sorted(_line(l) for l in body)))
    if typ in ("MULTIPOLYGON", "TIN", "POLYHEDRALSURFACE"):
        return ("MULTIPOLYGON", tuple(sorted(_poly(p) for p in body)))
    if typ == "GEOMETRYCOLLECTION":
        return ("GEOMETRYCOLLECTION", tuple(sorted((_norm(m) for m in body), key=repr)))
    raise ValueError(f"unknown type {typ}")


def _from_geojson(v):
    t = v["type"].upper()
    if t == "GEOMETRYCOLLECTION":
        return (t, [_from_geojson(m) for m in v["geometries"]] or None)
    c = v["coordinates"]

    def r(x):
        if isinstance(x, list) and x and not isinstance(x[0], list):
            return tuple(round(float(n), GEO_DECIMALS) + 0.0 for n in x[:2])
        return [r(y) for y in x]

    if not c:
        return (t, None)
    if t == "POINT":
        return (t, [r(c)])
    return (t, r(c))


def geometry(value, dt):
    """A geometry literal's canonical form, or None when it does not parse: coordinates
    rounded, rings from their smallest vertex in a fixed direction, lines in a fixed
    direction, members of collections sorted. The CRS IRI is kept (CRS84's dropped)."""
    try:
        if dt == GEOJSON:
            return repr(_norm(_from_geojson(json.loads(value))))
        text = value.strip()
        crs = ""
        if text.startswith("<"):
            crs, _, text = text.partition(">")
            crs = "" if crs + ">" == CRS84 else crs + ">"
        if not text:
            return crs + "EMPTY"
        return crs + repr(_norm(_parse_wkt(text)))
    except (ValueError, KeyError, TypeError, IndexError, AttributeError):
        return None


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
        if by_value and dt in (WKT, GEOJSON):
            g = geometry(t["value"], dt)
            if g is not None:
                return ("geom", g, dt)
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
