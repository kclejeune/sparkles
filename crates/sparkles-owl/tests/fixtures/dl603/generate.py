#!/usr/bin/env python3
"""Writes the OWL 2 DL ontologies of the fixture suite of roadmap item 603.

The suite has one family of random ontologies for each constructor of OWL 2 DL beyond
OWL 2 EL and for each interaction between them that a consequence-based calculus treats
with a rule of its own, such as nominals with inverses or cardinality with role
hierarchies, and a family of hand-written cases with a known entailment each. Every
ontology is in OWL 2 DL: number restrictions, functionality, self restrictions and role
disjointness name simple roles only, and the role hierarchy is regular (Structural
Specification §11). A complete OWL 2 DL reasoner's taxonomy of an ontology is the
classifier's expected answer. The files are `FAMILY-NNN.ofn` and `hand-NAME.ofn`, and
the sequence of each family is fixed by the seed and the family's name.

Usage: generate.py DIR [COUNT] [SEED]   (COUNT cases per family, 50 by default)
"""
import random
import re
import sys

EX = "http://example.org/d#"
XSD = "http://www.w3.org/2001/XMLSchema#"
CLASSES = [f":C{i}" for i in range(6)]
INDS = [f":a{i}" for i in range(4)]
DATA = [f":d{i}" for i in range(3)]
# r0 and r1 are simple: no chain, transitivity or non-simple subproperty reaches them, so
# number restrictions, functionality, self restrictions and role disjointness may name
# them and their inverses. Chains and transitivity go on r2 and r3, which are never
# subproperties of r0 or r1. The chains are regular for the order r0, r1 < r3 < r2.
SIMPLE = [":r0", ":r1"]
COMPLEX = [":r2", ":r3"]
ROLE_EDGES = [(0, 1), (1, 0), (0, 2), (0, 3), (1, 2), (1, 3), (3, 2)]
EL_TYPES = ["xsd:integer", "xsd:decimal", "xsd:nonNegativeInteger", "xsd:string", "rdfs:Literal"]
DL_TYPES = ["xsd:int", "xsd:short", "xsd:byte", "xsd:boolean", "xsd:positiveInteger",
            "xsd:negativeInteger", "xsd:double", "xsd:float", "xsd:unsignedByte"]
LITERALS = ['"1"^^xsd:integer', '"2"^^xsd:integer', '"-1"^^xsd:integer', '"0"^^xsd:integer',
            '"true"^^xsd:boolean', '"false"^^xsd:boolean', '"a"', '"1.5"^^xsd:decimal',
            '"7"^^xsd:byte', '"300"^^xsd:integer']


def canonical(e):
    """An expression with the operands of each intersection and union sorted, since an
    n-ary constructor's operands are a set and two equal ones are one."""
    tokens = re.findall(r"\(|\)|[^\s()]+", e)
    at = 0

    def node():
        nonlocal at
        t = tokens[at]
        at += 1
        if at < len(tokens) and tokens[at] == "(":
            at += 1
            args = []
            while tokens[at] != ")":
                args.append(node())
            at += 1
            if t in ("ObjectIntersectionOf", "ObjectUnionOf"):
                args = sorted(set(args))
            return f"{t}({' '.join(args)})"
        return t

    return node()


class Gen:
    """A random ontology over the constructors a family allows. `ops` weighs the class
    constructors, `axioms` the axiom kinds."""

    def __init__(self, r, ops, axioms, inverse=False):
        self.r = r
        self.ops = ops
        self.axioms = axioms
        self.inverse = inverse

    def pick(self, xs):
        return self.r.choice(xs)

    def weighted(self, table):
        total = sum(w for w, _ in table)
        k = self.r.uniform(0, total)
        for w, f in table:
            if k < w:
                return f()
            k -= w
        return table[0][1]()

    def simple(self):
        p = self.pick(SIMPLE)
        if self.inverse and self.r.random() < 0.4:
            return f"ObjectInverseOf({p})"
        return p

    def any_role(self):
        p = self.pick(SIMPLE + COMPLEX)
        if self.inverse and self.r.random() < 0.4:
            return f"ObjectInverseOf({p})"
        return p

    def named(self):
        k = self.r.random()
        if k < 0.85:
            return self.pick(CLASSES)
        if k < 0.92:
            return "owl:Thing"
        return "owl:Nothing" if "bottom" in self.ops else self.pick(CLASSES)

    def nominal(self):
        if "oneof" in self.ops and self.r.random() < 0.5:
            a, b = self.r.sample(INDS, 2)
            return f"ObjectOneOf({a} {b})"
        return f"ObjectOneOf({self.pick(INDS)})"

    def data_range(self):
        k = self.r.random()
        if k < 0.35:
            return self.pick(EL_TYPES + DL_TYPES)
        if k < 0.55:
            lo, hi = sorted(self.r.sample(range(-2, 4), 2))
            t = self.pick(["xsd:integer", "xsd:int", "xsd:byte"])
            return f'DatatypeRestriction({t} xsd:minInclusive "{lo}"^^xsd:integer xsd:maxInclusive "{hi}"^^xsd:integer)'
        if k < 0.7:
            a, b = self.r.sample(LITERALS, 2)
            return f"DataOneOf({a} {b})"
        if k < 0.85:
            return f"DataUnionOf({self.pick(EL_TYPES)} {self.pick(DL_TYPES)})"
        return f"DataComplementOf({self.pick(EL_TYPES + DL_TYPES)})"

    def expr(self, depth=0):
        if depth >= 2:
            return self.named()
        o = self.ops
        n = lambda: self.r.randint(0, 2)
        table = [
            (6, self.named),
            (3, lambda: f"ObjectSomeValuesFrom({self.any_role()} {self.expr(depth + 1)})"),
            (2, lambda: self.binary("ObjectIntersectionOf", depth)),
        ]
        if "union" in o:
            table.append((3, lambda: self.binary("ObjectUnionOf", depth)))
        if "complement" in o:
            table.append((2, lambda: f"ObjectComplementOf({self.expr(depth + 1)})"))
        if "forall" in o:
            table.append((3, lambda: f"ObjectAllValuesFrom({self.any_role()} {self.expr(depth + 1)})"))
        if "card" in o or "min" in o:
            table.append((2, lambda: f"ObjectMinCardinality({n() + 1} {self.simple()} {self.expr(depth + 1)})"))
        if "card" in o:
            table.append((2, lambda: f"ObjectMaxCardinality({n()} {self.simple()} {self.expr(depth + 1)})"))
            table.append((1, lambda: f"ObjectExactCardinality({n() + 1} {self.simple()} {self.expr(depth + 1)})"))
        if "max1" in o:
            # the Horn number restrictions: at most one in a positive position, of a
            # filler without disjunction in a negative position
            table.append((3, lambda: f"ObjectMaxCardinality(1 {self.simple()} {self.named()})"))
        if "nominal" in o:
            table.append((3, self.nominal))
            table.append((2, lambda: f"ObjectHasValue({self.any_role()} {self.pick(INDS)})"))
        if "self" in o:
            table.append((1, lambda: f"ObjectHasSelf({self.simple()})"))
        if "data" in o:
            table.append((2, lambda: f"DataSomeValuesFrom({self.pick(DATA)} {self.data_range()})"))
            table.append((2, lambda: f"DataAllValuesFrom({self.pick(DATA)} {self.data_range()})"))
            table.append((1, lambda: f"DataMaxCardinality({n()} {self.pick(DATA)})"))
            table.append((1, lambda: f"DataMinCardinality({n() + 1} {self.pick(DATA)} {self.data_range()})"))
            table.append((1, lambda: f"DataHasValue({self.pick(DATA)} {self.pick(LITERALS)})"))
        if "top" in o:
            table.append((1, lambda: f"ObjectSomeValuesFrom(owl:topObjectProperty {self.named()})"))
            table.append((1, lambda: f"ObjectAllValuesFrom(owl:topObjectProperty {self.named()})"))
        return self.weighted(table)

    def binary(self, op, depth):
        a, b = self.expr(depth + 1), self.expr(depth + 1)
        return f"{op}({a} {b})" if canonical(a) != canonical(b) else a

    def horn_sub(self):
        """A Horn axiom (Krötzsch, Rudolph and Hitzler, TOCL 2013): an intersection of
        EL expressions below an expression whose positive parts have no disjunction."""
        left = self.pick(CLASSES)
        if self.r.random() < 0.4:
            left = "ObjectIntersectionOf({} {})".format(*self.r.sample(CLASSES, 2))
        if self.r.random() < 0.3:
            left = f"ObjectSomeValuesFrom({self.any_role()} {self.pick(CLASSES)})"
        table = [
            (3, lambda: self.pick(CLASSES)),
            (3, lambda: f"ObjectSomeValuesFrom({self.any_role()} {self.pick(CLASSES)})"),
            (3, lambda: f"ObjectAllValuesFrom({self.any_role()} {self.pick(CLASSES)})"),
            (1, lambda: "owl:Nothing"),
        ]
        if "max1" in self.ops:
            table.append((3, lambda: f"ObjectMaxCardinality(1 {self.simple()} {self.pick(CLASSES + ['owl:Thing'])})"))
        if "min" in self.ops:
            table.append((2, lambda: f"ObjectMinCardinality({self.r.randint(1, 3)} {self.simple()} {self.pick(CLASSES)})"))
        if "nominal" in self.ops:
            table.append((2, lambda: f"ObjectOneOf({self.pick(INDS)})"))
            table.append((2, lambda: f"ObjectHasValue({self.any_role()} {self.pick(INDS)})"))
        return f"SubClassOf({left} {self.weighted(table)})"

    def chain(self):
        sup = self.r.choice([2, 3])
        below = [0, 1] if sup == 3 else [0, 1, 3]
        x, y = self.r.choice(below), self.r.choice(below)
        a, b = self.r.choice([(x, y), (sup, x), (x, sup), (sup, sup)])
        ops = [f":r{a}", f":r{b}"]
        # an operand below the superproperty may be inverted: the order of the chains
        # puts a role and its inverse in the same place (Structural Specification §11.2)
        if self.inverse and self.r.random() < 0.3:
            k = 0 if a != sup else 1
            if (a, b)[k] != sup:
                ops[k] = f"ObjectInverseOf({ops[k]})"
        return f"SubObjectPropertyOf(ObjectPropertyChain({ops[0]} {ops[1]}) :r{sup})"

    def axiom(self):
        r = self.r
        kinds = {
            "sub": (10, lambda: f"SubClassOf({self.expr()} {self.expr()})"),
            "named_sub": (6, lambda: f"SubClassOf({self.pick(CLASSES)} {self.expr()})"),
            "horn": (14, self.horn_sub),
            "equiv": (3, lambda: f"EquivalentClasses({self.pick(CLASSES)} {self.expr()})"),
            "disjoint": (1, lambda: "DisjointClasses({} {})".format(*r.sample(CLASSES, 2))),
            "disjoint_union": (1, lambda: "DisjointUnion({} {} {})".format(*r.sample(CLASSES, 3))),
            "role_sub": (2, lambda: "SubObjectPropertyOf(:r{} :r{})".format(*self.pick(ROLE_EDGES))),
            "role_inverse_sub": (1, lambda: "SubObjectPropertyOf(ObjectInverseOf(:r{}) :r{})".format(*self.pick(ROLE_EDGES))),
            # inverse pairs stay among the simple roles, which keeps the order of the
            # chains regular
            "inverses": (2, lambda: "InverseObjectProperties({} {})".format(*r.sample(SIMPLE, 2))),
            "symmetric": (1, lambda: f"SymmetricObjectProperty({self.pick(SIMPLE + COMPLEX)})"),
            "chain": (1, self.chain),
            "transitive": (1, lambda: f"TransitiveObjectProperty({self.pick(COMPLEX)})"),
            "reflexive": (1, lambda: f"ReflexiveObjectProperty({self.pick(SIMPLE + COMPLEX)})"),
            "irreflexive": (1, lambda: f"IrreflexiveObjectProperty({self.pick(SIMPLE)})"),
            "asymmetric": (1, lambda: f"AsymmetricObjectProperty({self.pick(SIMPLE)})"),
            "role_disjoint": (1, lambda: "DisjointObjectProperties({} {})".format(*r.sample(SIMPLE, 2))),
            "functional": (2, lambda: f"FunctionalObjectProperty({self.pick(SIMPLE)})"),
            "inverse_functional": (2, lambda: f"InverseFunctionalObjectProperty({self.pick(SIMPLE)})"),
            "domain": (1, lambda: f"ObjectPropertyDomain({self.any_role()} {self.expr(1)})"),
            "range": (2, lambda: f"ObjectPropertyRange({self.any_role()} {self.expr(1)})"),
            "class_assertion": (3, lambda: f"ClassAssertion({self.expr()} {self.pick(INDS)})"),
            "role_assertion": (3, lambda: f"ObjectPropertyAssertion({self.pick(SIMPLE + COMPLEX)} {self.pick(INDS)} {self.pick(INDS)})"),
            "negative_assertion": (1, lambda: f"NegativeObjectPropertyAssertion({self.pick(SIMPLE)} {self.pick(INDS)} {self.pick(INDS)})"),
            "same": (1, lambda: "SameIndividual({} {})".format(*r.sample(INDS, 2))),
            "different": (2, lambda: "DifferentIndividuals({} {})".format(*r.sample(INDS, 2))),
            "data_assertion": (2, lambda: f"DataPropertyAssertion({self.pick(DATA)} {self.pick(INDS)} {self.pick(LITERALS)})"),
            "data_functional": (1, lambda: f"FunctionalDataProperty({self.pick(DATA)})"),
            "data_range": (1, lambda: f"DataPropertyRange({self.pick(DATA)} {self.data_range()})"),
            "data_sub": (1, lambda: "SubDataPropertyOf({} {})".format(*r.sample(DATA, 2))),
            "data_disjoint": (1, lambda: "DisjointDataProperties({} {})".format(*r.sample(DATA, 2))),
            "key": (1, lambda: f"HasKey({self.pick(CLASSES)} ({self.pick(SIMPLE)}) ())"),
        }
        table = [kinds[k] for k in self.axioms]
        while True:
            ax = self.weighted(table)
            # an equivalence of a class with itself says nothing
            if not (ax.startswith("EquivalentClasses(") and len(set(ax[18:-1].split(" ", 1))) == 1):
                return ax


# Each family: the class constructors, the axiom kinds, whether roles may be inverted,
# and the calculus or rule set of the plan that it exercises.
CORE = ["sub", "named_sub", "equiv", "disjoint"]
ROLES = ["role_sub"]
ABOX = ["class_assertion", "role_assertion", "different", "same"]
FAMILIES = {
    # ALC: disjunction, negation and universal restrictions (Simančík, Kazakov and Horrocks,
    # IJCAI 2011)
    "alc": (["union", "complement", "forall", "bottom"], CORE + ["disjoint_union"], False),
    # ALCH: with a role hierarchy, domains and ranges
    "alch": (["union", "complement", "forall"], CORE + ROLES + ["domain", "range"], False),
    # Horn-SHIQ: inverses, at most one, transitivity, positive universals (Kazakov, IJCAI
    # 2009)
    "horn-shiq": (["forall", "max1"], ["horn", "role_sub", "role_inverse_sub", "inverses",
                                       "transitive", "functional", "inverse_functional",
                                       "domain", "range"], True),
    # Horn-SROIQ: Horn with nominals, chains and qualified at-least restrictions
    # (Ortiz, Rudolph and Šimkus, KR 2010)
    "horn-sroiq": (["forall", "max1", "min", "nominal"], ["horn", "role_sub", "inverses",
                                                          "chain", "transitive", "functional",
                                                          "inverse_functional", "class_assertion",
                                                          "role_assertion", "different"], True),
    # ALCI and SHI: inverses with disjunction, the backward propagation of universals
    "shi": (["union", "complement", "forall"], CORE + ROLES + ["inverses", "symmetric",
                                                             "transitive", "role_inverse_sub",
                                                             "range"], True),
    # ALCHQ and SHQ: qualified number restrictions with a role hierarchy
    "shq": (["union", "complement", "forall", "card"], CORE + ROLES + ["functional", "transitive"], False),
    # SHIQ: number restrictions on inverses (Bate, Motik, Cuenca Grau, Simančík and
    # Horrocks, KR 2016)
    "shiq": (["union", "forall", "card"], CORE + ROLES + ["inverses", "functional",
                                                        "inverse_functional", "role_inverse_sub",
                                                        "transitive"], True),
    # SRIQ: complex role inclusions, role disjointness, self, reflexivity and asymmetry
    # with number restrictions on simple roles
    "sriq": (["union", "forall", "card", "self"], CORE + ROLES + ["chain", "transitive",
                                                                "reflexive", "irreflexive",
                                                                "asymmetric", "role_disjoint",
                                                                "inverses", "functional"], True),
    # ALCO and SHOI: nominals with disjunction and inverses, without number restrictions
    "shoi": (["union", "complement", "forall", "nominal", "oneof"],
             CORE + ROLES + ABOX + ["inverses", "transitive"], True),
    # SHOIQ and SROIQ: nominals with inverses and number restrictions, where a nominal's
    # predecessors are bounded (Tena Cucala, Cuenca Grau and Horrocks, IJCAI 2018)
    "sroiq": (["union", "forall", "card", "nominal", "oneof", "self"],
              CORE + ROLES + ABOX + ["inverses", "functional", "inverse_functional", "chain",
                                     "transitive", "negative_assertion"], True),
    # The datatypes and data ranges of OWL 2 DL beyond OWL 2 EL: facets, finite datatypes,
    # unions, complements, enumerations, universals and data cardinality
    "data": (["union", "forall", "data"], CORE + ["data_assertion", "data_functional",
                                                 "data_range", "data_sub", "data_disjoint",
                                                 "class_assertion"], False),
    # Everything at once, with keys and the top property
    "mixed": (["union", "complement", "forall", "card", "nominal", "oneof", "self", "data",
               "top", "bottom"],
              CORE + ROLES + ABOX + ["disjoint_union", "inverses", "symmetric", "chain",
                                     "transitive", "reflexive", "irreflexive", "asymmetric",
                                     "role_disjoint", "functional", "inverse_functional",
                                     "domain", "range", "negative_assertion", "data_assertion",
                                     "data_functional", "data_range", "data_sub", "key"], True),
}


# Hand-written cases, each with the entailment it is written for. The reference
# reasoners decide the expected taxonomy, and the comment says why.
HAND = {
    "union-cases":
        # A ⊑ B ⊔ C, B ⊑ D, C ⊑ D, so A ⊑ D by cases
        ["SubClassOf(:A ObjectUnionOf(:B :C))", "SubClassOf(:B :D)", "SubClassOf(:C :D)"],
    "union-unsat":
        # both disjuncts clash with A, so A is unsatisfiable
        ["SubClassOf(:A ObjectUnionOf(:B :C))", "DisjointClasses(:A :B)", "DisjointClasses(:A :C)"],
    "complement-tautology":
        # ⊤ ⊑ B ⊔ ¬B gives A ⊑ D from B ⊑ D and ¬B ⊑ D
        ["SubClassOf(:B :D)", "SubClassOf(ObjectComplementOf(:B) :D)", "SubClassOf(:A owl:Thing)"],
    "forall-back":
        # A ⊑ ∃r.B and B ⊑ ∀r⁻.C, so A ⊑ C, through the inverse
        ["SubClassOf(:A ObjectSomeValuesFrom(:r :B))",
         "SubClassOf(:B ObjectAllValuesFrom(ObjectInverseOf(:r) :C))"],
    "forall-transitive":
        # with r transitive, A ⊑ ∀r.B reaches the r-successor of an r-successor, so
        # A ⊑ ∃r.(B ⊓ C) ⊑ D
        ["TransitiveObjectProperty(:r)", "SubClassOf(:A ObjectAllValuesFrom(:r :B))",
         "SubClassOf(:A ObjectSomeValuesFrom(:r ObjectSomeValuesFrom(:r :C)))",
         "EquivalentClasses(:D ObjectSomeValuesFrom(:r ObjectIntersectionOf(:B :C)))"],
    "functional-merge":
        # with r functional, A ⊑ ∃r.B ⊓ ∃r.C gives A ⊑ ∃r.(B ⊓ C)
        ["FunctionalObjectProperty(:r)",
         "SubClassOf(:A ObjectIntersectionOf(ObjectSomeValuesFrom(:r :B) ObjectSomeValuesFrom(:r :C)))",
         "EquivalentClasses(:D ObjectSomeValuesFrom(:r ObjectIntersectionOf(:B :C)))"],
    "inverse-functional-merge":
        # with r inverse functional, a B's two r-predecessors are one, so B ⊑ D
        ["InverseFunctionalObjectProperty(:r)",
         "SubClassOf(:B ObjectSomeValuesFrom(ObjectInverseOf(:r) :A))",
         "SubClassOf(:B ObjectSomeValuesFrom(ObjectInverseOf(:r) :C))",
         "EquivalentClasses(:D ObjectSomeValuesFrom(ObjectInverseOf(:r) ObjectIntersectionOf(:A :C)))"],
    "max-clash":
        # A ⊑ ≥2 r.B and A ⊑ ≤1 r.⊤, so A is unsatisfiable
        ["SubClassOf(:A ObjectMinCardinality(2 :r :B))", "SubClassOf(:A ObjectMaxCardinality(1 :r))"],
    "max-qualified-cases":
        # A ⊑ ≤1 r.⊤, A ⊑ ∃r.B, A ⊑ ∃r.C: the one successor is B ⊓ C, so A ⊑ ∃r.(B ⊓ C)
        ["SubClassOf(:A ObjectMaxCardinality(1 :r owl:Thing))", "SubClassOf(:A ObjectSomeValuesFrom(:r :B))",
         "SubClassOf(:A ObjectSomeValuesFrom(:r :C))",
         "EquivalentClasses(:D ObjectSomeValuesFrom(:r ObjectIntersectionOf(:B :C)))"],
    "max-qualified-merge":
        # ≤1 r.B with two r-successors that are both B makes them one
        ["SubClassOf(:A ObjectMaxCardinality(1 :r :B))", "SubClassOf(:A ObjectSomeValuesFrom(:r :C))",
         "SubClassOf(:A ObjectSomeValuesFrom(:r :E))", "SubClassOf(:C :B)", "SubClassOf(:E :B)",
         "EquivalentClasses(:D ObjectSomeValuesFrom(:r ObjectIntersectionOf(:C :E)))"],
    "min-hierarchy":
        # ≥2 s.B with s ⊑ r gives ≥2 r.B
        ["SubObjectPropertyOf(:s :r)", "SubClassOf(:A ObjectMinCardinality(2 :s :B))",
         "EquivalentClasses(:D ObjectMinCardinality(2 :r :B))"],
    "inverse-max":
        # A ⊑ ∃r.B, B ⊑ ≤1 r⁻.⊤ and B ⊑ ∃r⁻.C: an A's r-successor has one r-predecessor,
        # which is the A and a C, so A ⊑ C (SHIQ's interaction of inverses and counting)
        ["SubClassOf(:A ObjectSomeValuesFrom(:r :B))",
         "SubClassOf(:B ObjectMaxCardinality(1 ObjectInverseOf(:r) owl:Thing))",
         "SubClassOf(:B ObjectSomeValuesFrom(ObjectInverseOf(:r) :C))"],
    "nominal-disjunction":
        # {a, b} with C on a and on b gives ObjectOneOf(a b) ⊑ C, hence D ⊑ C
        ["EquivalentClasses(:D ObjectOneOf(:a :b))", "ClassAssertion(:C :a)", "ClassAssertion(:C :b)"],
    "nominal-inverse-max":
        # o has one r-predecessor, which b is, so every A is b and A ⊑ ∃t.⊤, and A ⊑ D
        # (the interaction of nominals, inverses and counting that SROIQ calculi bound)
        ["SubClassOf(:A ObjectSomeValuesFrom(:r ObjectOneOf(:o)))",
         "SubClassOf(ObjectOneOf(:o) ObjectMaxCardinality(1 ObjectInverseOf(:r) owl:Thing))",
         "SubClassOf(:B ObjectSomeValuesFrom(:r ObjectOneOf(:o)))", "SubClassOf(:A :C)",
         "SubClassOf(:B ObjectSomeValuesFrom(:t owl:Thing))", "ClassAssertion(:B :b)",
         "EquivalentClasses(:D ObjectIntersectionOf(:C ObjectSomeValuesFrom(:t owl:Thing)))"],
    "nominal-bounded-count":
        # an A has three distinct s-successors that are r-predecessors of o, which has
        # at most two, so A is unsatisfiable
        ["SubClassOf(ObjectOneOf(:o) ObjectMaxCardinality(2 ObjectInverseOf(:r) owl:Thing))",
         "SubClassOf(:A ObjectMinCardinality(3 :s ObjectSomeValuesFrom(:r ObjectOneOf(:o))))"],
    "nominal-ripple":
        # a has an instance in every model, so o is B and E ⊑ ∃r.B ⊑ D
        ["SubClassOf(:A ObjectSomeValuesFrom(:r ObjectIntersectionOf(ObjectOneOf(:o) :B)))",
         "SubClassOf(:E ObjectSomeValuesFrom(:r ObjectOneOf(:o)))", "ClassAssertion(:A :a)",
         "EquivalentClasses(:D ObjectSomeValuesFrom(:r :B))"],
    "nominal-ripple-conditional":
        # without an A in every model, o is B only where A has an instance: A ⊑ D, and
        # E ⊑ D is not entailed (the reachability condition of EL++'s rule CR6)
        ["SubClassOf(:A ObjectSomeValuesFrom(:r ObjectIntersectionOf(ObjectOneOf(:o) :B)))",
         "SubClassOf(:E ObjectSomeValuesFrom(:r ObjectOneOf(:o)))",
         "EquivalentClasses(:D ObjectSomeValuesFrom(:r :B))"],
    "self-cardinality":
        # ∃r.Self ⊓ ≤1 r.⊤ means the one r-successor is the instance itself
        ["SubClassOf(:A ObjectIntersectionOf(ObjectHasSelf(:r) ObjectMaxCardinality(1 :r owl:Thing)))",
         "SubClassOf(:A ObjectSomeValuesFrom(:r :B))", "SubClassOf(:A :C)",
         "EquivalentClasses(:D ObjectIntersectionOf(:B :C))"],
    "chain-forall":
        # r ∘ s ⊑ t and A ⊑ ∀t.B give A ⊑ ∀r.∀s.B
        ["SubObjectPropertyOf(ObjectPropertyChain(:r :s) :t)", "SubClassOf(:A ObjectAllValuesFrom(:t :B))",
         "EquivalentClasses(:D ObjectAllValuesFrom(:r ObjectAllValuesFrom(:s :B)))"],
    "role-disjoint":
        # disjoint r and s with a shared successor nominal clash
        ["DisjointObjectProperties(:r :s)",
         "SubClassOf(:A ObjectIntersectionOf(ObjectHasValue(:r :o) ObjectHasValue(:s :o)))"],
    "asymmetric-self":
        # an asymmetric role has no loops, so ∃r.Self is unsatisfiable
        ["AsymmetricObjectProperty(:r)", "EquivalentClasses(:A ObjectHasSelf(:r))"],
    "disjoint-union":
        # DisjointUnion(A B C) with D ⊑ A ⊓ ¬B gives D ⊑ C
        ["DisjointUnion(:A :B :C)", "SubClassOf(:D ObjectIntersectionOf(:A ObjectComplementOf(:B)))"],
    "data-facets":
        # an integer between 1 and 2 that is not 1 is 2: a finite range is a disjunction
        ['SubClassOf(:A DataSomeValuesFrom(:d DatatypeRestriction(xsd:integer xsd:minInclusive "1"^^xsd:integer xsd:maxInclusive "2"^^xsd:integer)))',
         'SubClassOf(:A DataAllValuesFrom(:d DataComplementOf(DataOneOf("1"^^xsd:integer))))',
         'EquivalentClasses(:D DataHasValue(:d "2"^^xsd:integer))'],
    "data-finite-count":
        # xsd:boolean has two values, so three distinct boolean values do not exist
        ["SubClassOf(:A DataMinCardinality(3 :d xsd:boolean))"],
    "data-max-merge":
        # at most one d value and two values that must be equal
        ['SubClassOf(:A DataMaxCardinality(1 :d))',
         'SubClassOf(:A DataSomeValuesFrom(:d DataOneOf("1"^^xsd:integer "2"^^xsd:integer)))',
         'SubClassOf(:A DataSomeValuesFrom(:d DataOneOf("2"^^xsd:integer "3"^^xsd:integer)))',
         'EquivalentClasses(:D DataHasValue(:d "2"^^xsd:integer))'],
    "top-forall":
        # ∀U.B on one instance makes every individual B, so a nominal class is below B
        ["SubClassOf(:A ObjectAllValuesFrom(owl:topObjectProperty :B))", "ClassAssertion(:A :a)",
         "EquivalentClasses(:D ObjectOneOf(:b))"],
    "horn-inverse-chain":
        # Horn-SHIQ: A ⊑ ∃r.B, B ⊑ ∃r.C and C ⊑ ∀r⁻.∀r⁻.E give A ⊑ E
        ["SubClassOf(:A ObjectSomeValuesFrom(:r :B))", "SubClassOf(:B ObjectSomeValuesFrom(:r :C))",
         "SubClassOf(:C ObjectAllValuesFrom(ObjectInverseOf(:r) ObjectAllValuesFrom(ObjectInverseOf(:r) :E)))"],
    "deep-disjunction":
        # eight cases from three binary disjunctions all lead to D
        ["SubClassOf(:A ObjectIntersectionOf(ObjectUnionOf(:B1 :C1) ObjectUnionOf(:B2 :C2) ObjectUnionOf(:B3 :C3)))",
         "SubClassOf(ObjectIntersectionOf(:B1 :B2) :D)", "SubClassOf(ObjectIntersectionOf(:C1 :B3) :D)",
         "SubClassOf(ObjectIntersectionOf(:C1 :C3) :D)", "SubClassOf(ObjectIntersectionOf(:B1 :C2) :D)"],
    "disjunction-through-successor":
        # A ⊑ ∃r.(B ⊔ C), ∃r.B ⊑ D, ∃r.C ⊑ D: case analysis in the successor's context
        ["SubClassOf(:A ObjectSomeValuesFrom(:r ObjectUnionOf(:B :C)))",
         "SubClassOf(ObjectSomeValuesFrom(:r :B) :D)", "SubClassOf(ObjectSomeValuesFrom(:r :C) :D)"],
    "complement-left":
        # ¬A ⊑ B is ⊤ ⊑ A ⊔ B, so with A ⊑ D and B ⊑ D every class is below D
        ["SubClassOf(ObjectComplementOf(:A) :B)", "SubClassOf(:A :D)", "SubClassOf(:B :D)",
         "SubClassOf(:C owl:Thing)"],
    "inverse-chain":
        # r⁻ ∘ s ⊑ t: an r-predecessor's s-successor in B is a t-successor, so A ⊑ D
        ["SubObjectPropertyOf(ObjectPropertyChain(ObjectInverseOf(:r) :s) :t)",
         "SubClassOf(:A ObjectSomeValuesFrom(ObjectInverseOf(:r) ObjectSomeValuesFrom(:s :B)))",
         "EquivalentClasses(:D ObjectSomeValuesFrom(:t :B))"],
    "shared-filler-no-join":
        # r ∘ s⁻ ⊑ t: an A's r-successor in B need not be the s-successor of any C, so A
        # is not below D, although B is the filler of both
        ["SubObjectPropertyOf(ObjectPropertyChain(:r ObjectInverseOf(:s)) :t)",
         "SubClassOf(:A ObjectSomeValuesFrom(:r :B))", "SubClassOf(:C ObjectSomeValuesFrom(:s :B))",
         "EquivalentClasses(:D ObjectSomeValuesFrom(:t :C))"],
    "top-forall-conditional":
        # ∀U.B on an A holds for every element, the A among them, with no individual:
        # A ⊑ B, and E ⊑ ∃r.¬B makes A ⊓ E unsatisfiable
        ["SubClassOf(:A ObjectAllValuesFrom(owl:topObjectProperty :B))",
         "SubClassOf(:E ObjectSomeValuesFrom(:r ObjectComplementOf(:B)))",
         "EquivalentClasses(:F ObjectIntersectionOf(:A :E))"],
    "data-cross-property":
        # d and e are below the functional f, so their values are one and 1 is not 2
        ["SubDataPropertyOf(:d :f)", "SubDataPropertyOf(:e :f)", "FunctionalDataProperty(:f)",
         'SubClassOf(:A ObjectIntersectionOf(DataHasValue(:d "1"^^xsd:integer) DataHasValue(:e "2"^^xsd:integer)))'],
    "data-disjoint-properties":
        # disjoint data properties cannot share a value
        ["DisjointDataProperties(:d :e)",
         'SubClassOf(:A ObjectIntersectionOf(DataHasValue(:d "1"^^xsd:integer) DataHasValue(:e "1"^^xsd:integer)))'],
    "data-finite-cases":
        # a boolean value is true or false, and both cases lead to D
        ["SubClassOf(:A DataSomeValuesFrom(:d xsd:boolean))",
         'EquivalentClasses(:B DataHasValue(:d "true"^^xsd:boolean))',
         'EquivalentClasses(:C DataHasValue(:d "false"^^xsd:boolean))',
         "SubClassOf(:B :D)", "SubClassOf(:C :D)"],
    "key-disjunction":
        # a is K by cases, so the key makes a and b one, and b gets a's class G
        ["HasKey(:K (:r) ())", "SubClassOf(:E :K)", "ClassAssertion(ObjectUnionOf(:K :E) :a)",
         "ClassAssertion(:K :b)", "ObjectPropertyAssertion(:r :a :o)",
         "ObjectPropertyAssertion(:r :b :o)", "ClassAssertion(:G :a)",
         "EquivalentClasses(:D ObjectOneOf(:b))"],
    "nominal-predecessor-cases":
        # o has one r-predecessor, a, so every A is a, and a is not C, so A ⊑ E
        ["SubClassOf(ObjectOneOf(:o) ObjectMaxCardinality(1 ObjectInverseOf(:r) owl:Thing))",
         "ObjectPropertyAssertion(:r :a :o)",
         "SubClassOf(:A ObjectIntersectionOf(ObjectHasValue(:r :o) ObjectUnionOf(:C :E)))",
         "ClassAssertion(ObjectComplementOf(:C) :a)"],
    "finite-domain":
        # the domain is {a, b}, a is not B, so every B is b
        ["SubClassOf(owl:Thing ObjectOneOf(:a :b))", "DifferentIndividuals(:a :b)",
         "ClassAssertion(ObjectComplementOf(:B) :a)", "SubClassOf(:A ObjectSomeValuesFrom(:r :B))",
         "EquivalentClasses(:D ObjectOneOf(:b))"],
    "el-control":
        # an OWL 2 EL ontology, which the EL rules classify alone
        ["SubClassOf(:A ObjectSomeValuesFrom(:r :B))", "SubClassOf(:B :C)",
         "EquivalentClasses(:D ObjectSomeValuesFrom(:r :C))"],
}


def header(name, classes, roles, data, inds):
    out = [
        f"Prefix(:=<{EX}>)",
        f"Prefix(xsd:=<{XSD}>)",
        "Prefix(owl:=<http://www.w3.org/2002/07/owl#>)",
        "Prefix(rdf:=<http://www.w3.org/1999/02/22-rdf-syntax-ns#>)",
        "Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)",
        f"Ontology(<{EX}{name}>",
    ]
    out += [f"Declaration(Class({c}))" for c in classes]
    out += [f"Declaration(ObjectProperty({p}))" for p in roles]
    out += [f"Declaration(DataProperty({p}))" for p in data]
    out += [f"Declaration(NamedIndividual({i}))" for i in inds]
    return out


def random_case(family, r, n):
    ops, axioms, inverse = FAMILIES[family]
    g = Gen(r, set(ops), axioms, inverse)
    out = header(f"{family}-{n:03}", CLASSES, SIMPLE + COMPLEX, DATA, INDS)
    out += sorted({g.axiom() for _ in range(r.randint(6, 16))})
    out.append(")")
    return "\n".join(out) + "\n"


def hand_case(name, axioms):
    found = sorted({m for a in axioms for m in re.findall(r"(?<![\w>]):[A-Za-z]\w*", a)})
    roles = sorted({m for m in found if m[1] in "rst"})
    data = sorted({m for m in found if m[1] in "def"})
    inds = sorted({m for m in found if m[1] in "abo"})
    classes = sorted(set(found) - set(roles) - set(data) - set(inds))
    out = header(f"hand-{name}", classes, roles, data, inds)
    out += axioms
    out.append(")")
    return "\n".join(out) + "\n"


def main():
    d = sys.argv[1]
    count = int(sys.argv[2]) if len(sys.argv) > 2 else 50
    seed = int(sys.argv[3]) if len(sys.argv) > 3 else 603
    for family in FAMILIES:
        r = random.Random(f"{seed}:{family}")
        for n in range(count):
            with open(f"{d}/{family}-{n:03}.ofn", "w") as f:
                f.write(random_case(family, r, n))
    for name, axioms in HAND.items():
        with open(f"{d}/hand-{name}.ofn", "w") as f:
            f.write(hand_case(name, axioms))


if __name__ == "__main__":
    main()
