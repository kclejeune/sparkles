#!/usr/bin/env python3
"""generate.py DIR COUNT [SEED]: random OWL 2 DL ontologies whose number restrictions have
compound qualifiers, for the differential check of item 897 against HermiT. A qualifier is
an intersection, an existential restriction over a simple, an inverse or a transitive role,
or one within the other, in maximums and exact cardinalities in positive positions and in
minimums and exact cardinalities in negative positions. The told hierarchy and the
existential axioms make neighbours derive a qualifier from its parts, which no axiom
names."""
import random
import sys

EX = "http://example.org/o#"


def gen(r):
    n = r.randint(5, 8)
    cs = [f":C{i}" for i in range(n)]
    counted = [":r0", ":r1", ":s"]
    inner = [":p", ":q", ":t"]
    out = ["SubObjectPropertyOf(:r0 :r1)", "TransitiveObjectProperty(:t)"]
    if r.random() < 0.4:
        out.append("SymmetricObjectProperty(:s)")
    if r.random() < 0.3:
        out.append("SubObjectPropertyOf(:p :t)")

    def crole():
        x = r.choice(counted)
        return f"ObjectInverseOf({x})" if r.random() < 0.15 else x

    def irole():
        x = r.choice(inner + counted)
        return f"ObjectInverseOf({x})" if r.random() < 0.15 else x

    def qual(depth=0):
        k = r.random()
        if k < 0.12 or depth > 1:
            return r.choice(cs)
        if k < 0.45:
            a, b = r.sample(cs, 2)
            return f"ObjectIntersectionOf({a} {b})"
        if k < 0.7:
            return f"ObjectSomeValuesFrom({irole()} {r.choice(cs)})"
        if k < 0.85:
            return f"ObjectIntersectionOf({r.choice(cs)} ObjectSomeValuesFrom({irole()} {qual(depth + 1)}))"
        return f"ObjectSomeValuesFrom({irole()} {qual(depth + 1)})"

    # a told hierarchy, so that a class is below both operands of an intersection
    for i in range(n):
        for j in range(i + 1, n):
            if r.random() < 0.25:
                out.append(f"SubClassOf({cs[i]} {cs[j]})")
    # existential axioms, so that a class derives an existential qualifier
    for _ in range(r.randint(2, 4)):
        out.append(f"SubClassOf({r.choice(cs)} ObjectSomeValuesFrom({irole()} {r.choice(cs)}))")
    # a minimum with a maximum or an exact cardinality over a compound qualifier on purpose
    for _ in range(r.randint(1, 3)):
        a = r.choice(cs)
        rr = r.choice(counted)
        k = r.randint(2, 3)
        out.append(f"SubClassOf({a} ObjectMinCardinality({k} {rr} {r.choice(cs)}))")
        b = a if r.random() < 0.6 else r.choice(cs)
        op = r.choice(["ObjectMaxCardinality", "ObjectMaxCardinality", "ObjectExactCardinality"])
        rr2 = r.choice([rr, ":r1"])
        out.append(f"SubClassOf({b} {op}({r.randint(1, k)} {rr2} {qual()}))")
    for _ in range(r.randint(3, 7)):
        k = r.random()
        a = r.choice(cs)
        if k < 0.2:
            out.append(f"SubClassOf({a} ObjectMaxCardinality({r.randint(0, 2)} {crole()} {qual()}))")
        elif k < 0.3:
            out.append(f"SubClassOf({a} ObjectExactCardinality({r.randint(1, 3)} {crole()} {qual()}))")
        elif k < 0.45:
            b = r.choice(cs)
            op = r.choice(["ObjectMinCardinality", "ObjectExactCardinality", "ObjectMaxCardinality"])
            out.append(f"EquivalentClasses({a} ObjectIntersectionOf({b} {op}({r.randint(1, 3)} {crole()} {qual()})))")
        elif k < 0.55:
            op = r.choice(["ObjectMinCardinality", "ObjectExactCardinality"])
            out.append(f"SubClassOf({op}({r.randint(2, 3)} {crole()} {qual()}) {a})")
        elif k < 0.65:
            b = r.choice(cs)
            out.append(f"SubClassOf({a} ObjectUnionOf({b} ObjectMinCardinality({r.randint(2, 3)} {crole()} {r.choice(cs)})))")
        elif k < 0.75:
            out.append(f"SubClassOf({a} ObjectAllValuesFrom({irole()} {r.choice(cs)}))")
        elif k < 0.87:
            b, c = r.sample(cs, 2)
            out.append(f"DisjointClasses({b} {c})")
        else:
            b, c = r.sample(cs, 2)
            out.append(f"SubClassOf({a} ObjectUnionOf({b} {c}))")
    out.append("SubClassOf(:Z ObjectUnionOf(:Z1 :Z2))")
    decl = [f"Declaration(Class({c}))" for c in cs + [":Z", ":Z1", ":Z2"]]
    decl += [f"Declaration(ObjectProperty({p}))" for p in counted + inner]
    return decl + out


def main():
    d, count = sys.argv[1], int(sys.argv[2])
    seed = int(sys.argv[3]) if len(sys.argv) > 3 else 897
    r = random.Random(seed)
    for i in range(count):
        body = "\n".join(gen(r))
        with open(f"{d}/qz-{i:04d}.ofn", "w") as f:
            f.write(f"Prefix(:=<{EX}>)\nPrefix(owl:=<http://www.w3.org/2002/07/owl#>)\nOntology(<{EX}qz-{i:04d}>\n{body}\n)\n")


main()
