"""The guarded secondary score `rdf-term-set-projected-guarded-lenient-v3`.

The strict policy `rdf-term-set-projected-columns-v2` stays the score of record. This
module computes a second score beside it, which fixes demonstrated evaluation defects
and closes the ways the projected policy can be gamed:

- Gold errata `gold-errata-v1` exclude a question whose gold cannot be matched by any
  correctly shaped answer on the measured release. Three generic rules and a short
  list of named questions decide it, and every exclusion carries its reason.
- Rows that hold an empty plain or language-tagged string are dropped from the gold and
  from the answer alike, since such a row is an infobox artefact and not an answer.
- Two numeric literals with XSD numeric datatypes match when their values are equal,
  so a count typed `xsd:nonNegativeInteger` matches one typed `xsd:integer`.
- A literal in the answer matches a gold IRI when its lexical form is a label of that
  IRI and of no other gold IRI. A literal in a gold column that also holds IRIs matches
  an answer IRI in the same way.
- An answer with more than three columns beyond the gold's is wrong. A pass whose extra
  column holds only predicates or whose projection drops answer rows is flagged, and
  the unflagged score sets it apart. A pass whose matched column is not the first while
  an extra column holds terms of the same kind is flagged as a rival column and only
  reported, since a related resource beside the answer raises that flag too.

Nothing here reads a model answer to decide what the gold is.
"""

import fractions
import hashlib
import math
import json
import pathlib
import re
import struct

from evaluator import UNBOUND, XSD_STRING, Invalid, Relation, project, align

POLICY_V3 = "rdf-term-set-projected-guarded-lenient-v3"
ERRATA_VERSION = "gold-errata-v1"
MAX_EXTRA_COLUMNS = 3
# The flags whose passes the unflagged score sets apart.
SET_APART = frozenset({"predicate_column", "predicate_column_unknown", "projection_drops_rows"})
LABEL_PREDICATES = (
    "http://www.w3.org/2000/01/rdf-schema#label",
    "http://www.w3.org/2004/02/skos/core#prefLabel",
)
XSD = "http://www.w3.org/2001/XMLSchema#"
NUMERIC = {
    XSD + t
    for t in (
        "integer decimal float double nonPositiveInteger negativeInteger long int "
        "short byte nonNegativeInteger unsignedLong unsignedInt unsignedShort "
        "unsignedByte positiveInteger"
    ).split()
}
ERRATA_FILE = pathlib.Path(__file__).resolve().parent.parent / "gold-errata-v1.json"
SELECT_STAR = re.compile(r"\bSELECT\s+(?:DISTINCT\s+|REDUCED\s+)?\*", re.I)
QUERY_FORM = re.compile(r"\b(SELECT|ASK|CONSTRUCT|DESCRIBE)\b", re.I)
# The prologue's BASE and PREFIX declarations, whose names may be keywords.
PROLOGUE = re.compile(r"\s*(?:(?:BASE\b|PREFIX\s+[^\s:]*:)\s*)*", re.I)


def load_errata(path=ERRATA_FILE):
    data = json.loads(pathlib.Path(path).read_text())
    if data.get("version") != ERRATA_VERSION:
        raise Invalid("unknown gold errata version")
    return {(e["id"], e["goldSha256"]): e for e in data["questions"]}


def strip_strings(text):
    return re.sub(r'"(?:[^"\\]|\\.)*"|\'(?:[^\'\\]|\\.)*\'|<[^<>\s]*>|#[^\n]*', " ", text)


def errata_exclusion(question, gold, errata, release=None):
    """The reason this question leaves the v3 denominator, or None. A named entry
    applies only to the exact gold text it was checked against, recomputed on the data
    release it names, since its defect lies in that release's data."""
    sha = hashlib.sha256(question["gold"].encode()).hexdigest()
    named = errata.get((question["id"], sha))
    if named is not None and release is not None and named.get("release") == release:
        return {"rule": "named", "reason": named["reason"]}
    text = strip_strings(question["gold"])
    # strip_strings turns each IRI into a space, so only the prologue's keywords and
    # prefix names remain before the query form.
    outer = QUERY_FORM.search(text, PROLOGUE.match(text).end())
    # Only the outer query form and projection decide the answer's columns. A
    # subquery's SELECT * inside it does not.
    if outer and SELECT_STAR.match(text, outer.start()):
        return {
            "rule": "select-star",
            "reason": "the gold uses SELECT *, so the scorer demands every bound "
            "variable beside the answer",
        }
    if type(gold) is not bool and gold.rows:
        for i, v in enumerate(gold.variables):
            if all(row[i] == UNBOUND for row in gold.rows):
                return {
                    "rule": "unbound-column",
                    "reason": f"the gold column ?{v} is unbound in every row, as when "
                    "a cast fails, so no correctly shaped answer matches",
                }
    return None


def empty_string(cell):
    return (cell[0] == "literal" and cell[2] == XSD_STRING and cell[1] == "") or (
        cell[0] == "lang" and cell[1] == ""
    )


def drop_empty(rel):
    """`rel` without its rows whose bound cells are all empty strings. A row with an
    empty label beside a real value keeps that value, on either side, so that an
    empty cell can neither hide a wrong answer nor remove a gold answer."""

    def keep(r):
        return not (
            any(empty_string(c) for c in r) and all(empty_string(c) or c == UNBOUND for c in r)
        )

    rows = frozenset(r for r in rel.rows if keep(r))
    return Relation(rel.variables, rows, rel.duplicates), len(rel.rows) - len(rows)


INTEGER = re.compile(r"[+-]?[0-9]+")
# The value spaces of the integer subtypes, as XML Schema 1.1 Part 2 defines them.
INTEGER_RANGES = {
    XSD + "nonPositiveInteger": (None, 0),
    XSD + "negativeInteger": (None, -1),
    XSD + "long": (-(2**63), 2**63 - 1),
    XSD + "int": (-(2**31), 2**31 - 1),
    XSD + "short": (-(2**15), 2**15 - 1),
    XSD + "byte": (-128, 127),
    XSD + "nonNegativeInteger": (0, None),
    XSD + "unsignedLong": (0, 2**64 - 1),
    XSD + "unsignedInt": (0, 2**32 - 1),
    XSD + "unsignedShort": (0, 2**16 - 1),
    XSD + "unsignedByte": (0, 255),
    XSD + "positiveInteger": (1, None),
}
DECIMAL = re.compile(r"[+-]?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)")
FLOATING = re.compile(r"[+-]?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)(?:[eE][+-]?[0-9]+)?")


def numeric_value(cell):
    """The exact value of an XSD numeric literal whose lexical form is valid for its
    datatype, or None. A float or double is the value it rounds to at its precision."""
    if cell[0] != "literal" or cell[2] not in NUMERIC:
        return None
    text, datatype = cell[1].strip(), cell[2]
    if datatype in (XSD + "float", XSD + "double"):
        if not FLOATING.fullmatch(text):
            return None
        value = float(text)
        if datatype == XSD + "float":
            try:
                value = struct.unpack("f", struct.pack("f", value))[0]
            except OverflowError:
                return None
        return fractions.Fraction(value) if math.isfinite(value) else None
    try:
        if datatype == XSD + "decimal":
            return fractions.Fraction(text) if DECIMAL.fullmatch(text) else None
        if not INTEGER.fullmatch(text):
            return None
        value = int(text)
    except ValueError:
        return None  # past Python's limit on digits converted to an integer
    low, high = INTEGER_RANGES.get(datatype, (None, None))
    if (low is not None and value < low) or (high is not None and value > high):
        return None
    return fractions.Fraction(value)


def literal_text(cell):
    if cell[0] in ("literal", "lang") and numeric_value(cell) is None:
        return cell[1]
    return None


def iris_of(rel):
    return {c[1] for r in rel.rows for c in r if c[0] == "uri"}


def label_requests(gold, got):
    """The IRIs whose labels the lenient match needs and the lexical forms to look
    for: labels of gold IRIs for literals in the answer, and labels of answer IRIs for
    literals in the gold columns that also hold IRIs."""
    gold_iris, got_iris = iris_of(gold), iris_of(got)
    answer_texts = {t for r in got.rows for c in r if (t := literal_text(c))}
    gold_texts = set()
    for i in range(len(gold.variables)):
        col = [r[i] for r in gold.rows]
        if any(c[0] == "uri" for c in col):
            gold_texts |= {t for c in col if (t := literal_text(c))}
    iris, texts = set(), set()
    if answer_texts and gold_iris:
        iris |= gold_iris
        texts |= answer_texts
    if gold_texts and (got_iris | gold_iris):
        iris |= got_iris | gold_iris
        texts |= gold_texts
    return sorted(iris), sorted(texts)


def canonical(gold, got, labels, columns=None):
    """Both relations with numeric literals as values and unambiguous labels as the
    IRIs they name. `labels` maps an IRI to the set of its label texts. `columns`, when
    given, is the set of ("gold", i) and ("got", j) columns whose labels may turn into
    IRIs, and every other column keeps its literals."""
    gold_iris, got_iris = iris_of(gold), iris_of(got)

    def named_by(text, universe):
        hits = [i for i in universe if text in labels.get(i, ())]
        return hits[0] if len(hits) == 1 else None

    def num(c):
        v = numeric_value(c)
        return ("number", v) if v is not None else c

    mixed = [
        any(r[i][0] == "uri" for r in gold.rows) for i in range(len(gold.variables))
    ]
    changes = {"numbers": 0, "goldLabels": 0, "answerLabels": 0}

    def gold_cell(i, c):
        n = num(c)
        if n is not c:
            changes["numbers"] += 1
            return n
        t = literal_text(c)
        if t is not None and mixed[i] and (columns is None or ("gold", i) in columns):
            iri = named_by(t, got_iris | gold_iris)
            if iri:
                changes["goldLabels"] += 1
                return ("uri", iri)
        return c

    def got_cell(j, c):
        n = num(c)
        if n is not c:
            changes["numbers"] += 1
            return n
        t = literal_text(c)
        if t is not None and (columns is None or ("got", j) in columns):
            iri = named_by(t, gold_iris)
            if iri:
                changes["answerLabels"] += 1
                return ("uri", iri)
        return c

    g = frozenset(tuple(gold_cell(i, c) for i, c in enumerate(r)) for r in gold.rows)
    a = frozenset(tuple(got_cell(j, c) for j, c in enumerate(r)) for r in got.rows)
    return (
        Relation(gold.variables, g, gold.duplicates),
        Relation(got.variables, a, got.duplicates),
        changes,
    )


MAX_LABEL_COLUMNS = 6


def label_columns(gold, got, labels):
    """The columns of either side in which some literal is a label that canonical()
    would turn into an IRI."""
    g1, a1, _ = canonical(gold, got, labels)
    g0, a0, _ = canonical(gold, got, {})
    out = [
        ("gold", i)
        for i in range(len(gold.variables))
        if {r[i] for r in g1.rows} != {r[i] for r in g0.rows}
    ]
    out += [
        ("got", j)
        for j in range(len(got.variables))
        if {r[j] for r in a1.rows} != {r[j] for r in a0.rows}
    ]
    return out


def lenient_compare(gold, got, labels, check=lambda: None):
    """(correct, detail, changes, gold, got) of the lenient comparison, or None when
    canonicalization changes nothing. It tries the label conversion in every subset of
    the columns that hold labels, all of them first and none last, since turning a
    label into its IRI in a column that already matches the gold's literal would break
    that column. Past MAX_LABEL_COLUMNS such columns it tries only all and none."""
    cols = label_columns(gold, got, labels) if labels else []
    if len(cols) > MAX_LABEL_COLUMNS:
        subsets = [cols, []]
    else:
        subsets = [
            [c for k, c in enumerate(cols) if mask >> k & 1]
            for mask in sorted(range(2 ** len(cols)), key=lambda m: -bin(m).count("1"))
        ]
    first = None
    for chosen in subsets:
        g, a, changes = canonical(gold, got, labels, set(chosen))
        if not any(changes.values()):
            continue
        ok, detail = compare(g, a, check)
        if ok:
            return ok, detail, changes, g, a
        first = first or (ok, detail, changes, g, a)
    return first


def compare(gold, got, check=lambda: None):
    """(correct, detail) for two SELECT relations under the projected comparison."""
    gw, aw = len(gold.variables), len(got.variables)
    if gw > aw:
        return False, {"status": "missing_columns"}
    if aw - gw > MAX_EXTRA_COLUMNS:
        return False, {"status": "guard_extra_columns", "extraColumns": aw - gw}
    if gw == aw:
        n, mapping = align(gold, got, check)
        ok = len(gold.rows) == len(got.rows) == n
        return ok, {
            "intersection": n,
            "goldRows": len(gold.rows),
            "gotRows": len(got.rows),
            "mapping": list(mapping) if mapping is not None else None,
            "extraColumns": 0,
        }
    n, mapping, projected = project(gold, got, check)
    ok = len(gold.rows) == len(projected) == n
    return ok, {
        "intersection": n,
        "goldRows": len(gold.rows),
        "gotRows": len(got.rows),
        "projectedRows": len(projected),
        "mapping": list(mapping) if mapping is not None else None,
        "extraColumns": aw - gw,
    }


def flags(gold, got, detail, predicates):
    """The guard flags of a pass with extra columns. `predicates(iris)` returns the
    subset of `iris` used as a predicate in the data, or None when it is unknown."""
    out = []
    mapping = detail.get("mapping")
    if not detail.get("extraColumns") or mapping is None:
        return out
    extra = [j for j in range(len(got.variables)) if j not in mapping]
    for j in extra:
        cells = [r[j] for r in got.rows if r[j] != UNBOUND]
        if not cells or any(c[0] != "uri" for c in cells):
            continue
        values = {c[1] for c in cells}
        found = predicates(sorted(values)) if len(values) <= 200 else None
        if found is None:
            out.append("predicate_column_unknown")
        elif found >= values:
            out.append("predicate_column")
    if detail["gotRows"] > detail["projectedRows"]:
        out.append("projection_drops_rows")
    if mapping != list(range(len(mapping))):
        for i, j in enumerate(mapping):
            kinds = {r[i][0] for r in gold.rows}
            if any({r[j2][0] for r in got.rows} == kinds for j2 in extra):
                out.append("rival_column")
                break
    return sorted(set(out))
