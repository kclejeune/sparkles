#!/usr/bin/env python3
"""Writes the random OWL 2 EL ontologies of the fixture suite of roadmap item 587.

Each ontology mixes individuals in class expressions, assertions, ranges, chains, data
ranges, literals, functional data properties, keys, negative assertions and the top
property, within OWL 2 EL and OWL 2 DL, so that a complete OWL 2 DL reasoner's taxonomy
of it is the classifier's expected answer. The sequence is fixed by its seed.

Usage: generate.py DIR [COUNT] [SEED]
"""
import random
import sys

EX = "http://example.org/f#"
XSD = "http://www.w3.org/2001/XMLSchema#"
CLASSES = [f":C{i}" for i in range(6)]
ROLES = [f":r{i}" for i in range(4)]
DATA = [f":d{i}" for i in range(3)]
INDS = [f":a{i}" for i in range(4)]
DATATYPES = ["xsd:integer", "xsd:decimal", "xsd:nonNegativeInteger", "xsd:string", "xsd:token",
             "rdfs:Literal", "xsd:dateTime", "owl:rational", "rdf:PlainLiteral"]
LITERALS = ['"1"^^xsd:integer', '"01"^^xsd:integer', '"1.0"^^xsd:decimal', '"1.5"^^xsd:decimal',
            '"-2"^^xsd:integer', '"a"', '"a b"', '"x"@en', '"2020-01-01T00:00:00Z"^^xsd:dateTime',
            '"2020-01-01T01:00:00+01:00"^^xsd:dateTime', '"3/2"^^owl:rational']


def pick(r, xs):
    return r.choice(xs)


def literal(r):
    return pick(r, LITERALS)


def data_range(r, depth=0):
    k = r.random()
    if k < 0.6 or depth > 0:
        return pick(r, DATATYPES)
    if k < 0.8:
        return f"DataOneOf({literal(r)})"
    a, b = data_range(r, 1), data_range(r, 1)
    return f"DataIntersectionOf({a} {b})" if a != b else a


def cls(r):
    k = r.random()
    if k < 0.8:
        return pick(r, CLASSES)
    if k < 0.88:
        return "owl:Thing"
    return f"ObjectOneOf({pick(r, INDS)})"


def expr(r, depth=0, negative=False):
    if depth >= 2:
        return cls(r)
    k = r.random()
    if k < 0.35:
        return cls(r)
    if k < 0.55:
        return f"ObjectSomeValuesFrom({pick(r, ROLES)} {expr(r, depth + 1, negative)})"
    if k < 0.7:
        a, b = expr(r, depth + 1, negative), expr(r, depth + 1, negative)
        return f"ObjectIntersectionOf({a} {b})" if a != b else a
    if k < 0.8:
        return f"ObjectHasValue({pick(r, ROLES)} {pick(r, INDS)})"
    if k < 0.85:
        # a simple role: no chain or transitivity reaches r0 or r1
        return f"ObjectHasSelf({pick(r, ROLES[:2])})"
    if k < 0.92:
        return f"DataSomeValuesFrom({pick(r, DATA)} {data_range(r)})"
    if k < 0.97 or not negative:
        return f"DataHasValue({pick(r, DATA)} {literal(r)})"
    return f"ObjectSomeValuesFrom(owl:topObjectProperty {cls(r)})"


# Ranges go on r0 and r1, and chains and transitivity on r2 and r3, which are never
# subproperties of r0 or r1, so that every ontology meets the range restriction of OWL 2
# EL and its keys and self restrictions name simple properties. The chains are regular
# (Structural Specification §11.2) for the order r0, r1 < r3 < r2, which HermiT needs.
ROLE_EDGES = [(0, 1), (1, 0), (0, 2), (0, 3), (1, 2), (1, 3), (3, 2)]


def chain(r):
    sup = r.choice([2, 3])
    below = [0, 1] if sup == 3 else [0, 1, 3]
    x, y = r.choice(below), r.choice(below)
    a, b = r.choice([(x, y), (sup, x), (x, sup), (sup, sup)])
    return f"SubObjectPropertyOf(ObjectPropertyChain(:r{a} :r{b}) :r{sup})"


def axiom(r):
    kinds = [
        (10, lambda: f"SubClassOf({expr(r, negative=True)} {expr(r)})"),
        (6, lambda: f"SubClassOf({pick(r, CLASSES)} {expr(r)})"),
        (3, lambda: f"EquivalentClasses({pick(r, CLASSES)} {expr(r, negative=True)})"),
        (1, lambda: "DisjointClasses({} {})".format(*r.sample(CLASSES, 2))),
        (2, lambda: "SubObjectPropertyOf(:r{} :r{})".format(*pick(r, ROLE_EDGES))),
        (1, lambda: chain(r)),
        (1, lambda: f"TransitiveObjectProperty({pick(r, ROLES[2:])})"),
        (1, lambda: f"ReflexiveObjectProperty({pick(r, ROLES)})"),
        (1, lambda: f"ObjectPropertyDomain({pick(r, ROLES)} {cls(r)})"),
        (3, lambda: f"ObjectPropertyRange({pick(r, ROLES[:2])} {cls(r)})"),
        (3, lambda: f"ClassAssertion({expr(r)} {pick(r, INDS)})"),
        (3, lambda: f"ObjectPropertyAssertion({pick(r, ROLES)} {pick(r, INDS)} {pick(r, INDS)})"),
        (1, lambda: f"NegativeObjectPropertyAssertion({pick(r, ROLES)} {pick(r, INDS)} {pick(r, INDS)})"),
        (1, lambda: "SameIndividual({} {})".format(*r.sample(INDS, 2))),
        (1, lambda: "DifferentIndividuals({} {})".format(*r.sample(INDS, 2))),
        (2, lambda: f"DataPropertyAssertion({pick(r, DATA)} {pick(r, INDS)} {literal(r)})"),
        (1, lambda: f"NegativeDataPropertyAssertion({pick(r, DATA)} {pick(r, INDS)} {literal(r)})"),
        (1, lambda: "SubDataPropertyOf({} {})".format(*r.sample(DATA, 2))),
        (1, lambda: f"FunctionalDataProperty({pick(r, DATA)})"),
        (1, lambda: f"DataPropertyRange({pick(r, DATA)} {pick(r, DATATYPES)})"),
        (1, lambda: f"DataPropertyDomain({pick(r, DATA)} {cls(r)})"),
        (1, lambda: f"HasKey({pick(r, CLASSES)} ({pick(r, ROLES[:2])}) ())"),
        (1, lambda: f"HasKey({pick(r, CLASSES)} () ({pick(r, DATA)}))"),
    ]
    total = sum(w for w, _ in kinds)
    k = r.uniform(0, total)
    for w, f in kinds:
        if k < w:
            return f()
        k -= w
    return kinds[0][1]()


def ontology(r, n):
    out = [
        f"Prefix(:=<{EX}>)",
        f"Prefix(xsd:=<{XSD}>)",
        "Prefix(owl:=<http://www.w3.org/2002/07/owl#>)",
        "Prefix(rdf:=<http://www.w3.org/1999/02/22-rdf-syntax-ns#>)",
        "Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)",
        f"Ontology(<{EX}{n}>",
    ]
    out += [f"Declaration(Class({c}))" for c in CLASSES]
    out += [f"Declaration(ObjectProperty({p}))" for p in ROLES]
    out += [f"Declaration(DataProperty({p}))" for p in DATA]
    out += [f"Declaration(NamedIndividual({i}))" for i in INDS]
    out += sorted({axiom(r) for _ in range(r.randint(8, 22))})
    out.append(")")
    return "\n".join(out) + "\n"


def main():
    d = sys.argv[1]
    count = int(sys.argv[2]) if len(sys.argv) > 2 else 400
    seed = int(sys.argv[3]) if len(sys.argv) > 3 else 587
    r = random.Random(seed)
    for n in range(count):
        with open(f"{d}/case-{n:03}.ofn", "w") as f:
            f.write(ontology(r, n))


if __name__ == "__main__":
    main()
