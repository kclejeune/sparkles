#!/usr/bin/env python3
"""Fingerprint an engine's answer to a benchmark query, for cross-engine comparison.

    bench-answers.py <endpoint> <query-file> <answers.json> <query-name> <engine> [--count-only] [--no-check]

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
  lines, vertices in the middle of straight runs and the order of collection members;
  Z and M values are ignored, and the default CRS84 prefix is dropped.

Solutions are compared as multisets over variables sorted by name, since ORDER BY ties
may be broken differently and engines list the result variables in different orders.
`--count-only` records just the row count, for LIMIT-without-ORDER probes whose
solutions are legitimately engine-dependent. A failed request records `error`.

Blank nodes compare by structure. Each response is canonicalized on its own, so a blank
node label (which only has meaning inside one response) never decides equality, and two
different arrangements of blank nodes do not compare equal.

ASK queries record a digest of the boolean. CONSTRUCT and DESCRIBE queries are fetched as
N-Triples, parsed, and compared as a set of triples by RDF term identity (a graph has no
duplicates). The value digest normalizes the terms of that set without merging triples, so
`"1"^^xsd:integer` and `"1.0"^^xsd:decimal` stay two triples. Spacing, escapes in literals
and an explicit `xsd:string` do not matter. `--no-check` records the
row count with a fixed digest, for DESCRIBE queries whose result the specification leaves
to the engine.

A query whose own ORDER BY uses only variables that the result projects also records
`order`, a digest of the sequence of those key values in the returned order. Rows that tie
on the keys have the same key values, so a correct engine gives the same sequence whatever
it does with ties. ORDER BY with expressions, and ORDER BY inside subqueries, are not
checked.

The whole request, not each wait, is limited to MAX_TIME seconds (default 300). A slower
answer is recorded as `error`.
"""

import hashlib
import json
import os
import re
import signal
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


def _straight(a, b, c):
    """Whether b lies on the segment from a to c (a vertex that adds nothing)."""
    cross = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
    scale = max(abs(c[0] - a[0]), abs(c[1] - a[1]), 1e-300)
    if abs(cross) > 10 ** -GEO_DECIMALS * scale:
        return False
    return min(a[0], c[0]) <= b[0] <= max(a[0], c[0]) and min(a[1], c[1]) <= b[1] <= max(a[1], c[1])


def _simplify(pts, closed):
    """Drop repeated vertices and vertices in the middle of a straight run."""
    out = []
    for p in pts:
        if not out or out[-1] != p:
            out.append(p)
    changed = True
    while changed and len(out) > (3 if closed else 2):
        changed = False
        n = len(out)
        for i in range(n) if closed else range(1, n - 1):
            if _straight(out[i - 1], out[i], out[(i + 1) % n]):
                del out[i]
                changed = True
                break
    return out


def _ring(coords):
    """A closed ring without its closing vertex or vertices on straight runs, from its
    smallest vertex, in the direction that gives the smaller sequence (equal rings
    compare equal)."""
    pts = [tuple(c) for c in coords]
    if len(pts) > 1 and pts[0] == pts[-1]:
        pts = pts[:-1]
    pts = _simplify(pts, True)
    if not pts:
        return ()
    k = pts.index(min(pts))
    fwd = pts[k:] + pts[:k]
    back = list(reversed(pts))
    k = back.index(min(back))
    back = back[k:] + back[:k]
    return tuple(min(fwd, back))


def _line(coords):
    pts = tuple(_simplify([tuple(c) for c in coords], False))
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
        return ("bnode", t.get("value", ""))
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


def canon(rows):
    """The digest of a solution multiset or a set of triples that does not depend on blank node
    labels. Each blank node gets a colour from the rows it occurs in (the other blank nodes shown
    by their own colour), refined four times, and the rows are digested with colours for labels.
    It tells apart the graphs that matter here, though it is not a full isomorphism test."""
    rows = list(rows)
    labels = {t[1] for r in rows for t in r if t and t[0] == "bnode"}
    if not labels:
        return digest(rows)
    color = {l: "" for l in labels}

    def show(t, own=None):
        if t and t[0] == "bnode":
            return ("bnode", "*" if t[1] == own else color[t[1]])
        return t

    for _ in range(4):
        sig = {l: [] for l in labels}
        for r in rows:
            for l in {t[1] for t in r if t and t[0] == "bnode"}:
                sig[l].append(tuple(show(t, l) for t in r))
        color = {l: hashlib.sha256(repr(sorted(sig[l])).encode()).hexdigest()[:12] for l in labels}
    return digest(tuple(show(t) for t in r) for r in rows)


UNESCAPE = re.compile(r"\\(u[0-9A-Fa-f]{4}|U[0-9A-Fa-f]{8}|[tbnrf\"'\\])")
ESCAPES = {"t": "\t", "b": "\b", "n": "\n", "r": "\r", "f": "\f", '"': '"', "'": "'", "\\": "\\"}
NT_LINE = re.compile(r"^(<[^>]*>|_:\S+)\s+(<[^>]*>)\s+(.+?)\s*\.\s*$")
NT_LITERAL = re.compile(r'^"((?:[^"\\]|\\.)*)"(?:@([A-Za-z0-9-]+)|\^\^<([^>]*)>)?$')


def _unescape(text):
    return UNESCAPE.sub(lambda m: chr(int(m.group(1)[1:], 16)) if m.group(1)[0] in "uU" else ESCAPES[m.group(1)], text)


def nt_term(tok):
    """An N-Triples term as the JSON form of a SPARQL result, so that `term` normalizes it. The
    lexical form is unescaped, so `\\u00e9` and `é` are the same literal."""
    if tok.startswith("<<"):
        return {"type": "other", "value": tok}
    if tok.startswith("<"):
        return {"type": "uri", "value": tok[1:-1]}
    if tok.startswith("_:"):
        return {"type": "bnode", "value": tok[2:]}
    m = NT_LITERAL.match(tok)
    if not m:
        return {"type": "other", "value": tok}
    t = {"type": "literal", "value": _unescape(m.group(1))}
    if m.group(2):
        t["xml:lang"] = m.group(2)
    elif m.group(3):
        t["datatype"] = m.group(3)
    return t


def nt_triples(text, by_value):
    """The set of triples of an N-Triples document: a graph has no duplicate triples, and the
    spacing and escapes of a line are not part of it."""
    out = set()
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        m = NT_LINE.match(line)
        if not m:
            out.add((("line", line),))
            continue
        out.add(tuple(term(nt_term(tok), by_value) for tok in m.groups()))
    return out


def nt_rows(text):
    """The triples of an N-Triples document as lists of JSON-form terms, each triple once by RDF
    term identity (the first occurrence is kept). A line that is not a triple is one `other` item."""
    out, seen = [], set()
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        m = NT_LINE.match(line)
        triple = tuple(nt_term(tok) for tok in m.groups()) if m else ({"type": "other", "value": line},)
        key = tuple(term(d, False) for d in triple)
        if key not in seen:
            seen.add(key)
            out.append(triple)
    return out


def graph_record(text, count_only, no_check):
    """The answer record of a CONSTRUCT or DESCRIBE result. The graph is a set of triples by term
    identity. The value digest normalizes the terms of that set without merging triples."""
    triples = nt_rows(text)
    rec = {"rows": len(triples)}
    if count_only:
        return rec
    if no_check:
        rec["exact"] = rec["value"] = "unchecked"
        return rec
    rec["exact"] = canon([tuple(term(d, False) for d in r) for r in triples])
    rec["value"] = canon([tuple(term(d, True) for d in r) for r in triples])
    return rec


def order_keys(query, head_vars):
    """The variables of the query's own ORDER BY when it uses only plain variables, `ASC(?v)` or
    `DESC(?v)` that are in the result, else None. Strings, comments and IRIs are blanked first,
    and only an ORDER BY outside every group counts."""
    q = re.sub(r"<[^<>\s]*>", "<>", query)
    q = re.sub(r'"(?:[^"\\]|\\.)*"|\'(?:[^\'\\]|\\.)*\'', '""', q)
    q = re.sub(r"#[^\n]*", "", q)
    depth, start = 0, None
    for m in re.finditer(r"[{}]|\border\s+by\b", q, re.I):
        if m.group() == "{":
            depth += 1
        elif m.group() == "}":
            depth -= 1
        elif depth == 0:
            start = m.end()
    if start is None:
        return None
    clause = re.split(r"\b(?:limit|offset)\b", q[start:], flags=re.I)[0].strip()
    keys = re.findall(r"(?:(?:asc|desc)\s*\(\s*)?\?(\w+)\s*\)?", clause, re.I)
    rest = re.sub(r"(?:(?:asc|desc)\s*\(\s*)?\?\w+\s*\)?", "", clause, flags=re.I).strip()
    if not keys or rest or any(k not in head_vars for k in keys):
        return None
    return keys


def form(query):
    """select, ask, construct or describe: the first of these words outside IRIs and comments."""
    q = re.sub(r"#[^\n]*", "", re.sub(r"<[^<>\s]*>", "", query))
    m = re.search(r"\b(select|construct|describe|ask)\b", q, re.I)
    return m.group(1).lower() if m else "select"


class Deadline(Exception):
    pass


def fetch(req, seconds):
    """The whole response body, or Deadline when it takes longer than `seconds` altogether. The
    socket timeout of urlopen limits each wait, and a server that keeps sending slowly would
    pass it, so an interval timer enforces the total."""
    def expire(*_):
        raise Deadline(f"no complete answer in {seconds:g} s")

    old = signal.signal(signal.SIGALRM, expire)
    signal.setitimer(signal.ITIMER_REAL, seconds)
    try:
        with urllib.request.urlopen(req, timeout=seconds) as r:
            return r.read()
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        signal.signal(signal.SIGALRM, old)


def main():
    endpoint, qfile, out, name, engine = sys.argv[1:6]
    count_only = "--count-only" in sys.argv[6:]
    no_check = "--no-check" in sys.argv[6:]
    with open(qfile) as f:
        query = f.read()
    kind = form(query)
    graph = kind in ("construct", "describe")
    req = urllib.request.Request(
        endpoint if endpoint.startswith("http") else "http://" + endpoint,
        data=urllib.parse.urlencode({"query": query}).encode(),
        headers={"Accept": "application/n-triples" if graph else "application/sparql-results+json",
                 "Content-Type": "application/x-www-form-urlencoded"},
    )
    try:
        body = fetch(req, float(os.environ.get("MAX_TIME", "300")))
        if graph:
            rec = graph_record(body.decode(), count_only, no_check)
        else:
            res = json.loads(body)
            if kind == "ask":
                rec = {"rows": 1}
                rec["exact"] = rec["value"] = digest([(bool(res["boolean"]),)])
            else:
                # variable order in the head is not part of a solution (engines differ)
                vars_ = sorted(res["head"].get("vars", []))
                bindings = res["results"]["bindings"]
                rec = {"rows": len(bindings)}
                if not count_only:
                    for key, by_value in (("exact", False), ("value", True)):
                        rec[key] = canon(tuple(term(b.get(v), by_value) for v in vars_) for b in bindings)
                    keys = order_keys(query, vars_)
                    if keys:
                        seq = [tuple(term(b.get(v), True) for v in keys) for b in bindings]
                        if not any(t[0] == "bnode" for row in seq for t in row):
                            # the sequence of ORDER BY keys, which ties in the keys cannot change
                            rec["order"] = hashlib.sha256(repr(seq).encode()).hexdigest()[:16]
    except Exception as e:  # noqa: BLE001 - any failure is recorded, not raised
        rec = {"rows": "error", "error": str(e)[:200]}
    d = {}
    if os.path.exists(out):
        with open(out) as f:
            d = json.load(f)
    d.setdefault(name, {})[engine] = rec
    with open(out, "w") as f:
        json.dump(d, f, indent=1)
    print(rec["rows"])


if __name__ == "__main__":
    main()
