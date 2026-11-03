"""The polarity counts and features of the ORE 2015 DL scan (scripts/elk-compare/ore-dl.py)."""

import importlib.util
from pathlib import Path
import unittest


SCRIPTS = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("ore_dl", SCRIPTS / "elk-compare" / "ore-dl.py")
ORE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ORE)


def scan(*lines):
    s = ORE.Scan()
    for line in lines:
        ax = ORE.parse(line)
        if line.startswith(ORE.FACTS):
            s.fact(ax)
        else:
            s.axiom(ax)
    return s


def stratum(*lines):
    s = scan(*lines)
    return ORE.stratum(dict(s.f), s.nonhorn > 0, 0)


class ScanTests(unittest.TestCase):
    def test_disjunctions_count_by_their_position(self):
        self.assertEqual(scan("SubClassOf(:A ObjectUnionOf(:B :C))").nonhorn, 1)
        self.assertEqual(scan("SubClassOf(ObjectUnionOf(:B :C) :A)").nonhorn, 0)
        self.assertEqual(scan("SubClassOf(ObjectComplementOf(:A) :B)").nonhorn, 1)

    def test_minimums_in_a_negative_position_count_as_their_dual_maximum(self):
        self.assertEqual(scan("SubClassOf(ObjectMinCardinality(3 :r :B) owl:Nothing)").nonhorn, 1)
        self.assertEqual(scan("SubClassOf(ObjectMinCardinality(2 :r :B) owl:Nothing)").nonhorn, 0)
        self.assertEqual(scan("SubClassOf(ObjectMinCardinality(2 :r :B) :C)").nonhorn, 1)
        self.assertEqual(
            scan("SubClassOf(ObjectMinCardinality(0 :r ObjectComplementOf(:B)) :C)").nonhorn, 0
        )

    def test_a_key_concludes_an_equality(self):
        self.assertEqual(scan("HasKey(:A (:r) ())").nonhorn, 0)
        self.assertEqual(scan("HasKey(ObjectComplementOf(:A) (:r) ())").nonhorn, 1)
        self.assertEqual(stratum("HasKey(:A (ObjectInverseOf(:r)) ())"), "horn")

    def test_disjointness_counts_the_two_largest(self):
        self.assertEqual(scan("DisjointClasses(ObjectComplementOf(:A) :B :C)").nonhorn, 0)
        self.assertEqual(
            scan("DisjointClasses(ObjectComplementOf(:A) ObjectComplementOf(:B) :C)").nonhorn, 1
        )
        self.assertEqual(scan("EquivalentClasses(:A :B :C)").nonhorn, 0)
        self.assertEqual(scan("EquivalentClasses(:A ObjectUnionOf(:B :C))").nonhorn, 1)

    def test_literal_datatypes_outside_el(self):
        self.assertEqual(stratum('SubClassOf(:A DataHasValue(:d "true"^^xsd:boolean))'), "horn")
        self.assertEqual(stratum('DataPropertyRange(:d DataOneOf("true"^^xsd:boolean))'), "horn")
        self.assertEqual(stratum('DataPropertyAssertion(:d :a "1"^^xsd:boolean)'), "horn")
        self.assertEqual(stratum('DataPropertyAssertion(:d :a "1"^^xsd:integer)'), "el")
        # the datatype is the suffix after the closing quote, not text inside the literal
        self.assertEqual(stratum('DataPropertyAssertion(:d :a "x^^y")'), "el")
        self.assertEqual(stratum('DataPropertyAssertion(:d :a "x^^y"@en)'), "el")
        self.assertEqual(stratum('SubClassOf(:A DataHasValue(:d "a\\"^^b"))'), "el")

    def test_anonymous_individuals_are_outside_el_and_horn(self):
        self.assertEqual(stratum("ClassAssertion(:A _:x)"), "horn")
        self.assertEqual(stratum("ObjectPropertyAssertion(:r :a _:x)"), "horn")
        self.assertEqual(stratum("ClassAssertion(:A :a)"), "el")

    def test_facts_fingerprint_without_annotations_or_spacing(self):
        a = ORE.parse('ObjectPropertyAssertion(Annotation(:p "x") :r :a :b)')
        b = ORE.parse("ObjectPropertyAssertion( :r   :a :b )")
        self.assertEqual(repr(a), repr(b))


if __name__ == "__main__":
    unittest.main()
