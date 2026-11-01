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
import json
import pathlib
import re

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


def load_errata(path=ERRATA_FILE):
    data = json.loads(pathlib.Path(path).read_text())
    if data.get("version") != ERRATA_VERSION:
        raise Invalid("unknown gold errata version")
    return {(e["id"], e["goldSha256"]): e for e in data["questions"]}


def strip_strings(text):
    return re.sub(r'"(?:[^"\\]|\\.)*"|\'(?:[^\'\\]|\\.)*\'|<[^<>\s]*>|#[^\n]*', " ", text)


def errata_exclusion(question, gold, errata):
    """The reason this question leaves the v3 denominator, or None. A named entry
    applies only to the exact gold text it was checked against."""
    sha = hashlib.sha256(question["gold"].encode()).hexdigest()
    named = errata.get((question["id"], sha))
    if named is not None:
        return {"rule": "named", "reason": named["reason"]}
    if SELECT_STAR.search(strip_strings(question["gold"])):
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
    rows = frozenset(r for r in rel.rows if not any(empty_string(c) for c in r))
    return Relation(rel.variables, rows, rel.duplicates), len(rel.rows) - len(rows)


def numeric_value(cell):
    if cell[0] != "literal" or cell[2] not in NUMERIC:
        return None
    text = cell[1].strip()
    try:
        if cell[2] in (XSD + "float", XSD + "double"):
            if text.lower().lstrip("+-") in ("inf", "infinity", "nan"):
                return None
        return fractions.Fraction(text)
    except (ValueError, ZeroDivisionError):
        return None


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


def canonical(gold, got, labels):
    """Both relations with numeric literals as values and unambiguous labels as the
    IRIs they name. `labels` maps an IRI to the set of its label texts."""
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
        if t is not None and mixed[i]:
            iri = named_by(t, got_iris | gold_iris)
            if iri:
                changes["goldLabels"] += 1
                return ("uri", iri)
        return c

    def got_cell(c):
        n = num(c)
        if n is not c:
            changes["numbers"] += 1
            return n
        t = literal_text(c)
        if t is not None:
            iri = named_by(t, gold_iris)
            if iri:
                changes["answerLabels"] += 1
                return ("uri", iri)
        return c

    g = frozenset(tuple(gold_cell(i, c) for i, c in enumerate(r)) for r in gold.rows)
    a = frozenset(tuple(got_cell(c) for c in r) for r in got.rows)
    return (
        Relation(gold.variables, g, gold.duplicates),
        Relation(got.variables, a, got.duplicates),
        changes,
    )


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
        values = {r[j][1] for r in got.rows if r[j][0] == "uri"}
        if values and len(values) <= 200:
            found = predicates(sorted(values))
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
