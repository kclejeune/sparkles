#!/usr/bin/env python3
"""Compare the axioms of two ontologies in OWL functional-style syntax.

The comparison of `sparkles ontology reason`, `reduce` and `relax` with ROBOT's commands
in docs/benchmarks/el-classification reads both outputs with this script. Each document is
parsed whole, prefixed names are expanded to full IRIs, a literal typed xsd:string is
written as a plain literal, and the operands of n-ary constructors and axioms whose order
does not matter (intersections, unions, enumerations, equivalences, disjointness, same and
different individuals, keys and annotations) are sorted, so that two writers' spellings of
one axiom compare equal.

    scripts/elk-compare/ofn-axioms.py [--all] [--show N] A.ofn B.ofn

The answer is a JSON object. `logical` compares the logical axioms without their
annotations: every axiom but declarations and the annotation axioms. With `--all`,
`all` also compares every axiom with its annotations. Each comparison gives the counts of
each side, of the common axioms, and of the axioms only one side has, by axiom type, with
up to N examples of each (default 5). The exit status is 1 when a comparison differs.
"""

import json
import re
import sys
from collections import Counter

XSD_STRING = "http://www.w3.org/2001/XMLSchema#string"

TOKEN = re.compile(
    r"""\s+|\#[^\n]*|(\()|(\))|<([^>]*)>|("(?:[^"\\]|\\.)*")(?:\^\^(<[^>]*>|[^\s()]*)|@([A-Za-z0-9-]+))?|(=)|([^\s()<>"=]+)"""
)

NARY = {
    "ObjectIntersectionOf",
    "ObjectUnionOf",
    "ObjectOneOf",
    "DataIntersectionOf",
    "DataUnionOf",
    "DataOneOf",
    "EquivalentClasses",
    "DisjointClasses",
    "EquivalentObjectProperties",
    "DisjointObjectProperties",
    "EquivalentDataProperties",
    "DisjointDataProperties",
    "SameIndividual",
    "DifferentIndividuals",
}

NONLOGICAL = {
    "Declaration",
    "AnnotationAssertion",
    "SubAnnotationPropertyOf",
    "AnnotationPropertyDomain",
    "AnnotationPropertyRange",
}


def parse(path):
    """The axioms of a document, as normalized strings with their type."""
    with open(path, encoding="utf-8") as f:
        text = f.read()
    prefixes = {}
    stack = [[]]
    pos = 0
    n = len(text)
    while pos < n:
        m = TOKEN.match(text, pos)
        if not m:
            raise SystemExit(f"{path}: cannot read at offset {pos}: {text[pos:pos + 40]!r}")
        pos = m.end()
        op, cl, iri, lit, dt, lang, eq, word = m.groups()
        if op:
            stack.append([])
        elif cl:
            node = stack.pop()
            stack[-1].append(tuple(node))
        elif iri is not None:
            stack[-1].append(("<", iri))
        elif lit is not None:
            stack[-1].append(("lit", lit, dt, lang))
        elif eq:
            stack[-1].append(("=",))
        elif word is not None:
            stack[-1].append(("w", word))
    top = stack[0]
    # Prefix(p:=<iri>) Ontology(...)
    i = 0
    out = []
    while i < len(top):
        t = top[i]
        if t[0] == "w" and t[1] == "Prefix" and i + 1 < len(top):
            args = top[i + 1]
            name = args[0][1] if args and args[0][0] == "w" else ""
            iri = next(a[1] for a in args if a[0] == "<")
            prefixes[name.rstrip(":") + ":" if name.endswith(":") else name] = iri
            i += 2
            continue
        if t[0] == "w" and t[1] == "Ontology" and i + 1 < len(top):
            body = top[i + 1]
            out = axioms_of(body, prefixes)
            i += 2
            continue
        i += 1
    return out


def expand(word, prefixes):
    if word.startswith("_:"):
        return "_:" + word[2:]
    if ":" in word:
        p, local = word.split(":", 1)
        ns = prefixes.get(p + ":")
        if ns is not None:
            return "<" + ns + local + ">"
    return word


def axioms_of(body, prefixes):
    out = []
    j = 0
    while j < len(body):
        t = body[j]
        if t[0] == "w" and j + 1 < len(body) and is_node(body[j + 1]):
            kind, args = t[1], body[j + 1]
            j += 2
            if kind not in ("Import", "Annotation"):
                out.append((kind, norm_axiom(kind, args, prefixes)))
            continue
        j += 1
    return out


def is_node(x):
    """A parenthesized group: a tuple whose items are all tokens or groups."""
    return isinstance(x, tuple) and (len(x) == 0 or not isinstance(x[0], str))


def norm_items(items, prefixes):
    """Normalize a list of tokens and groups: a word followed by a group is a
    constructor applied to it."""
    out = []
    k = 0
    while k < len(items):
        t = items[k]
        if t[0] == "w" and k + 1 < len(items) and is_node(items[k + 1]):
            out.append(norm_call(t[1], items[k + 1], prefixes))
            k += 2
            continue
        if t[0] == "w":
            out.append(expand(t[1], prefixes))
        elif t[0] == "<":
            out.append("<" + t[1] + ">")
        elif t[0] == "lit":
            out.append(norm_lit(t, prefixes))
        k += 1
    return out


def norm_lit(t, prefixes):
    _, lit, dt, lang = t
    if lang:
        return f"{lit}@{lang.lower()}"
    if dt:
        d = dt[1:-1] if dt.startswith("<") else expand(dt, prefixes)[1:-1]
        if d != XSD_STRING:
            return f"{lit}^^<{d}>"
    return lit


def norm_call(name, args, prefixes):
    items = norm_items(args, prefixes)
    if name == "Annotation":
        anns = sorted(x for x in items if x.startswith("Annotation("))
        rest = [x for x in items if not x.startswith("Annotation(")]
        return f"Annotation({' '.join(anns + rest)})"
    if name in NARY:
        items = sorted(items)
    elif name == "DisjointUnion" and items:
        items = items[:1] + sorted(items[1:])
    elif name == "HasKey":
        pass
    return f"{name}({' '.join(items)})"


def norm_axiom(kind, args, prefixes):
    items = norm_items(args, prefixes)
    anns = sorted(x for x in items if x.startswith("Annotation("))
    rest = [x for x in items if not x.startswith("Annotation(")]
    if kind in NARY:
        rest = sorted(rest)
    elif kind == "DisjointUnion" and rest:
        rest = rest[:1] + sorted(rest[1:])
    plain = f"{kind}({' '.join(rest)})"
    full = f"{kind}({' '.join(anns + rest)})"
    return plain, full


def compare(a, b, show):
    sa, sb = set(a), set(b)
    only_a = sorted(sa - sb)
    only_b = sorted(sb - sa)
    kind = lambda s: s.split("(", 1)[0]
    return {
        "a": len(sa),
        "b": len(sb),
        "common": len(sa & sb),
        "onlyA": len(only_a),
        "onlyB": len(only_b),
        "onlyAByType": dict(Counter(kind(x) for x in only_a)),
        "onlyBByType": dict(Counter(kind(x) for x in only_b)),
        "onlyAExamples": only_a[:show],
        "onlyBExamples": only_b[:show],
    }


def main(argv):
    show = 5
    full = False
    args = []
    it = iter(argv)
    for x in it:
        if x == "--all":
            full = True
        elif x == "--show":
            show = int(next(it))
        else:
            args.append(x)
    if len(args) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    a = parse(args[0])
    b = parse(args[1])
    logical = lambda xs: [p for k, (p, _) in xs if k not in NONLOGICAL]
    report = {"logical": compare(logical(a), logical(b), show)}
    differ = report["logical"]["onlyA"] or report["logical"]["onlyB"]
    if full:
        report["all"] = compare([f for _, (_, f) in a], [f for _, (_, f) in b], show)
        differ = differ or report["all"]["onlyA"] or report["all"]["onlyB"]
    print(json.dumps(report, indent=1))
    return 1 if differ else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
