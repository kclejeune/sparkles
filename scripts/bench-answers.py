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
With `ANSWERS_RAW_DIR` set, the response body is also kept as `<query-name>.<engine>.json`
(or `.nt` for a graph) in that directory, so that a disagreement can be examined value by
value. The fingerprints do not depend on it.

`--count-only` records just the row count, for LIMIT-without-ORDER probes whose
solutions are legitimately engine-dependent. A failed request records `error`.

Blank nodes compare by structure. Each response is canonicalized on its own, so a blank
node label (which only has meaning inside one response) never decides equality, and two
different arrangements of blank nodes do not compare equal. This includes blank nodes inside
SELECT triple terms. Triple terms in N-Triples output (`<< ... >>`) are compared as text,
labels included. This is a limitation, and no suite returns them.

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
it does with ties. The digest is recorded only for key columns whose order SPARQL defines.
These hold IRIs, unbound values and one class of literal (strings, numbers, booleans,
`xsd:dateTime` or `xsd:date`). A column with language-tagged strings, blank nodes, other
datatypes or a mix of literal classes gets no digest. A mix of `xsd:dateTime` values with and
without a time zone is not detected, although its order is partly undefined. ORDER BY with
expressions, and ORDER BY inside subqueries, are not checked.

With a LIMIT or a nonzero OFFSET, the rows that tie on the keys at the cut may be any of the tied
rows. For such a query, when the order digest applies, the script sends one more request, which
is not timed, with the cut widened by one row on each side. It leaves rows out of `exact` and
`value` only when that request shows that the row past the cut has the same keys as the boundary
row, which is the first row for an OFFSET and the last row when the result reaches the LIMIT.
`boundary_rows` says how many rows were left out, and it is 0 when no tie crosses the cut. Keys
that are unique, such as a final IRI key, never leave a row out. `OFFSET 0` is no cut. A failed
or inconsistent probe leaves every row in, and a failed one records `boundary_probe` as
`failed`, so it can cause a false rejection but never hides a wrong answer. The `order` digest
still covers every row, so the key values and the number of rows in each boundary group are
checked. A query with other keys, such as an expression, can still be rejected when rows tie at
the cut.

The whole request, not each wait, is limited to MAX_TIME seconds (default 300). A slower
answer is recorded as `error`. The boundary probe has its own MAX_TIME deadline, so the worst
case for one query is twice MAX_TIME.
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


def _bnodes(t):
    """The blank node labels of a term, including those inside a triple term."""
    if t and t[0] == "bnode":
        yield t[1]
    elif t and t[0] == "triple":
        for x in t[1:]:
            yield from _bnodes(x)


def canon(rows):
    """The digest of a solution multiset or a set of triples that does not depend on blank node
    labels. Each blank node gets a colour from the rows it occurs in (the other blank nodes shown
    by their own colour), refined four times, and the rows are digested with colours for labels.
    It tells apart the graphs that matter here, though it is not a full isomorphism test."""
    rows = list(rows)
    labels = {l for r in rows for t in r for l in _bnodes(t)}
    if not labels:
        return digest(rows)
    color = {l: "" for l in labels}

    def show(t, own=None):
        if t and t[0] == "bnode":
            return ("bnode", "*" if t[1] == own else color[t[1]])
        if t and t[0] == "triple":
            return ("triple",) + tuple(show(x, own) for x in t[1:])
        return t

    for _ in range(4):
        sig = {l: [] for l in labels}
        for r in rows:
            for l in {l for t in r for l in _bnodes(t)}:
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


def _blank(m):
    """The same-length blanking of one matched IRI, string or comment, so that positions in the
    blanked text are positions in the query."""
    t = m.group()
    if t[0] == "<":
        return "<" + " " * (len(t) - 2) + ">"
    if t[0] in "\"'":
        return t[0] + " " * (len(t) - 2) + t[0]
    return " " * len(t)


def _order_by_tail(query):
    """(start, tail) of the query's last top-level ORDER BY, where start is the index in `query`
    after the ORDER BY keywords and tail is the text from there with IRIs, strings and comments
    blanked to the same length, or None. Only an ORDER BY outside every group counts."""
    q = re.sub(r"<[^<>\s]*>", _blank, query)
    q = re.sub(r'"(?:[^"\\]|\\.)*"|\'(?:[^\'\\]|\\.)*\'', _blank, q)
    q = re.sub(r"#[^\n]*", _blank, q)
    depth, start = 0, None
    for m in re.finditer(r"[{}]|\border\s+by\b", q, re.I):
        if m.group() == "{":
            depth += 1
        elif m.group() == "}":
            depth -= 1
        elif depth == 0:
            start = m.end()
    return None if start is None else (start, q[start:])


def _after_order_by(query):
    """The text after the query's last top-level ORDER BY with strings, comments and IRIs blanked,
    or None."""
    t = _order_by_tail(query)
    return None if t is None else t[1]


def order_keys(query, head_vars):
    """The variables of the query's own ORDER BY when it uses only plain variables, `ASC(?v)` or
    `DESC(?v)` that are in the result, else None."""
    tail = _after_order_by(query)
    if tail is None:
        return None
    clause = re.split(r"\b(?:limit|offset)\b", tail, flags=re.I)[0].strip()
    keys = re.findall(r"(?:(?:asc|desc)\s*\(\s*)?\?(\w+)\s*\)?", clause, re.I)
    rest = re.sub(r"(?:(?:asc|desc)\s*\(\s*)?\?\w+\s*\)?", "", clause, flags=re.I).strip()
    if not keys or rest or any(k not in head_vars for k in keys):
        return None
    return keys


def slice_of(query):
    """(limit, offset) of the query's own top-level ORDER BY ... LIMIT ... OFFSET ..., where limit
    is an integer or None and offset is an integer, 0 when there is none. A query without a
    top-level ORDER BY gives (None, 0)."""
    tail = _after_order_by(query)
    if tail is None:
        return None, 0
    lim = re.search(r"\blimit\s+(\d+)", tail, re.I)
    off = re.search(r"\boffset\s+(\d+)", tail, re.I)
    return (int(lim.group(1)) if lim else None), (int(off.group(1)) if off else 0)


def probe_query(query, offset, count):
    """The query with its top-level LIMIT and OFFSET clauses replaced by `LIMIT count OFFSET offset`,
    put where the earlier of the two clauses was so that a trailing VALUES block stays valid. Only
    called for a query with at least one of the clauses."""
    start, tail = _order_by_tail(query)
    spans = [(start + m.start(), start + m.end())
             for m in re.finditer(r"\b(?:limit|offset)\s+\d+", tail, re.I)]
    chars = list(query)
    for a, b in spans:
        chars[a:b] = " " * (b - a)
    at = min(a for a, _ in spans)
    text = "".join(chars)
    return text[:at] + f" LIMIT {count} OFFSET {offset} " + text[at:]


# literal classes whose order SPARQL defines. xsd:date is outside the SPARQL 1.1 operator table, but
# every compared engine orders it and BSBM query 8 sorts on it.
LITERAL_CLASS = {XSD + "string": "str", XSD + "boolean": "bool", XSD + "dateTime": "dateTime",
                 XSD + "date": "date"}


def orderable(seq):
    """Whether SPARQL defines the order of every key column of `seq`, a list of key tuples of
    value-normalized terms. IRIs, unbound values and one class of literal are ordered, and a mix of
    literal classes, language-tagged strings, blank nodes and other datatypes are not."""
    for col in zip(*seq):
        classes = set()
        for t in col:
            if t[0] in ("undef", "uri"):
                continue
            if t[0] == "num":
                classes.add("num")
            elif t[0] == "lit" and t[2] in LITERAL_CLASS:
                classes.add(LITERAL_CLASS[t[2]])
            else:
                return False
        if len(classes) > 1:
            return False
    return True


def boundary_edge(query, bindings, keys, seq, run):
    """(edge, probed) for a query with a LIMIT or an offset. `edge` holds the key tuples of the
    boundary rows that the probe request `run` shows tied with the row past the cut, and `probed`
    is "failed" when the probe raised, else None. Nothing is an edge without a shown tie, or when
    the probe disagrees with the answer."""
    n, o = slice_of(query)
    k = len(bindings)
    low = o > 0 and k > 0
    high = n is not None and k > 0 and k == n
    if run is None or not (low or high):
        return set(), None
    lo = 1 if low else 0
    count = 2 if n is None else k + lo + (1 if high else 0)
    try:
        res = run(probe_query(query, o - 1 if low else o, count))
        probe = [tuple(term(b.get(v), True) for v in keys) for b in res["results"]["bindings"]]
    except Exception:  # noqa: BLE001 - a failed probe masks nothing
        return set(), "failed"
    need = min(k, count - lo)
    if probe[lo:lo + need] != seq[:need]:
        return set(), None
    edge = set()
    if low and probe[0] == seq[0]:
        edge.add(seq[0])
    if high and len(probe) > lo + k and probe[lo + k] == seq[-1]:
        edge.add(seq[-1])
    return edge, None


def select_record(query, res, count_only, run=None):
    """The answer record of a SELECT result in SPARQL JSON form. `run` takes a query text and
    returns its parsed SPARQL JSON result or raises, and serves the boundary probe."""
    # variable order in the head is not part of a solution (engines differ)
    vars_ = sorted(res["head"].get("vars", []))
    bindings = res["results"]["bindings"]
    rec = {"rows": len(bindings)}
    if count_only:
        return rec
    keys = order_keys(query, vars_)
    seq = [tuple(term(b.get(v), True) for v in keys) for b in bindings] if keys else None
    ordered = seq is not None and orderable(seq)
    kept = bindings
    if ordered:
        # the sequence of ORDER BY keys, which ties in the keys cannot change
        rec["order"] = hashlib.sha256(repr(seq).encode()).hexdigest()[:16]
        limit, offset = slice_of(query)
        if limit is not None or offset:
            # rows that tie on the key across the cut may be any of the tied rows, so only their
            # keys count, and a second request shows whether a tie crosses the cut
            edge, probed = boundary_edge(query, bindings, keys, seq, run)
            if probed:
                rec["boundary_probe"] = probed
            kept = [b for b, kt in zip(bindings, seq) if kt not in edge]
            rec["boundary_rows"] = len(bindings) - len(kept)
    for key, by_value in (("exact", False), ("value", True)):
        rec[key] = canon(tuple(term(b.get(v), by_value) for v in vars_) for b in kept)
    return rec


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
    url = endpoint if endpoint.startswith("http") else "http://" + endpoint

    def request(text):
        return urllib.request.Request(
            url, data=urllib.parse.urlencode({"query": text}).encode(),
            headers={"Accept": "application/n-triples" if graph else "application/sparql-results+json",
                     "Content-Type": "application/x-www-form-urlencoded"})

    max_time = float(os.environ.get("MAX_TIME", "300"))
    raw_dir = os.environ.get("ANSWERS_RAW_DIR")
    try:
        body = fetch(request(query), max_time)
        if raw_dir:
            # the whole response, for a later look at a disagreement the digests only detect
            os.makedirs(raw_dir, exist_ok=True)
            ext = "nt" if graph else "json"
            with open(os.path.join(raw_dir, f"{name}.{engine}.{ext}"), "wb") as f:
                f.write(body)
        if graph:
            rec = graph_record(body.decode(), count_only, no_check)
        else:
            res = json.loads(body)
            if kind == "ask":
                rec = {"rows": 1}
                rec["exact"] = rec["value"] = digest([(bool(res["boolean"]),)])
            else:
                rec = select_record(query, res, count_only,
                                    lambda text: json.loads(fetch(request(text), max_time)))
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
