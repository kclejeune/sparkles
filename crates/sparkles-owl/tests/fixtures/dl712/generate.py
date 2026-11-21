#!/usr/bin/env python3
"""generate.py DIR [COUNT] [SEED]: random OWL 2 DL ontologies for the check of roadmap item
712, which reduces the automata by which the disjunctive rules unfold universal
restrictions on properties that chains and transitivity reach.

Each ontology has transitive properties under transitive properties, chains and property
inclusions in both directions of a property, so that the closure of an automaton over its
ε-transitions leaves transitions by a subproperty beside those by a superproperty into the
same state, and states that accept the same words. Universal restrictions and negative
existential restrictions on those properties, and their inverses, meet paths of
existential restrictions through unions, so that a wrong merge or a dropped transition
changes the taxonomy. The hierarchy is regular for the order r0 < r1 < r2 < r3 < r4, with
inverses in the place of their properties. Each file is `gen-NNN.ofn`, and HermiT's
taxonomy of it is `gen-NNN.hermit.ofn`.
"""
import random
import sys

EX = "http://example.org/a#"
ROLES = [f":r{i}" for i in range(5)]
CLASSES = [f":C{i}" for i in range(7)]


def gen(r):
    out = []
    # the order of the hierarchy is the index; a property inclusion goes up the order
    transitive = [i for i in range(5) if r.random() < 0.55]
    for i in transitive:
        out.append(f"TransitiveObjectProperty({ROLES[i]})")
    for i in range(5):
        for j in range(i + 1, 5):
            if r.random() < 0.25:
                a = ROLES[i]
                b = ROLES[j]
                if r.random() < 0.2:
                    a = f"ObjectInverseOf({a})"
                    b = f"ObjectInverseOf({b})"
                out.append(f"SubObjectPropertyOf({a} {b})")
    for _ in range(r.randint(0, 2)):
        t = r.randint(1, 4)
        kind = r.random()
        lower = [ROLES[i] for i in range(t)]
        if kind < 0.4:
            ops = [r.choice(lower), r.choice(lower)]
        elif kind < 0.7:
            ops = [ROLES[t], r.choice(lower)]
        else:
            ops = [r.choice(lower), ROLES[t]]
        out.append(f"SubObjectPropertyOf(ObjectPropertyChain({' '.join(ops)}) {ROLES[t]})")

    def name(i, inv):
        return f"ObjectInverseOf({ROLES[i]})" if inv else ROLES[i]

    def role():
        return name(r.randrange(5), r.random() < 0.25)

    def path(roles, end):
        e = end
        for i, inv in reversed(roles):
            e = f"ObjectSomeValuesFrom({name(i, inv)} {e})"
        return e

    def up(roles):
        """The roles of a path with some raised up the order and some neighbours joined,
        so that a path of the other often implies it."""
        out = [(r.randint(i, 4) if r.random() < 0.4 else i, inv) for i, inv in roles]
        if len(out) > 1 and r.random() < 0.4:
            k = r.randrange(len(out) - 1)
            out[k : k + 2] = [max(out[k], out[k + 1])]
        return out

    cs = CLASSES
    for _ in range(r.randint(1, 3)):
        out.append(f"SubClassOf({r.choice(cs)} ObjectAllValuesFrom({role()} {r.choice(cs)}))")
    for _ in range(r.randint(0, 2)):
        out.append(f"SubClassOf(ObjectSomeValuesFrom({role()} {r.choice(cs)}) {r.choice(cs)})")
    for _ in range(r.randint(1, 3)):
        a, b, c, d = r.sample(cs, 4)
        out.append(f"SubClassOf(ObjectIntersectionOf({a} {b}) ObjectUnionOf({c} {d}))")
    for _ in range(r.randint(0, 2)):
        a, b = r.sample(cs, 2)
        out.append(f"SubClassOf({a} {b})")
    if r.random() < 0.3:
        a, b = r.sample(cs, 2)
        out.append(f"DisjointClasses({a} {b})")
    for k in range(r.randint(2, 4)):
        q = f":Q{k}"
        a, b = r.sample(cs, 2)
        roles = [(r.randrange(5), r.random() < 0.2) for _ in range(r.randint(1, 3))]
        body = path(roles, b)
        if r.random() < 0.6:
            body = f"ObjectIntersectionOf({a} {body})"
        out.append(f"SubClassOf({q} {body})")
        w = f":W{k}"
        out.append(f"EquivalentClasses({w} {path(up(roles), r.choice(cs))})")
    return out


def main():
    d = sys.argv[1]
    count = int(sys.argv[2]) if len(sys.argv) > 2 else 200
    seed = int(sys.argv[3]) if len(sys.argv) > 3 else 712
    r = random.Random(seed)
    for n in range(count):
        axioms = gen(r)
        name = f"gen-{n:03d}"
        decls = [f"Declaration(Class({c}))" for c in CLASSES]
        decls += [f"Declaration(Class(:Q{k}))" for k in range(4)]
        decls += [f"Declaration(Class(:W{k}))" for k in range(4)]
        decls += [f"Declaration(ObjectProperty({p}))" for p in ROLES]
        lines = [
            f"Prefix(:=<{EX}>)",
            "Prefix(owl:=<http://www.w3.org/2002/07/owl#>)",
            f"Ontology(<{EX}{name}>",
            *decls,
            *axioms,
            ")",
        ]
        with open(f"{d}/{name}.ofn", "w") as f:
            f.write("\n".join(lines) + "\n")


if __name__ == "__main__":
    main()
