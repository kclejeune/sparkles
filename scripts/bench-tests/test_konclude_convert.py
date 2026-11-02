"""Check scripts/elk-compare/konclude.py's conversion of Konclude's class hierarchy into
the reference form that scripts/el-oracle.py reads."""

from pathlib import Path
import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("konclude", ROOT / "scripts" / "elk-compare" / "konclude.py")
konclude = importlib.util.module_from_spec(spec)
spec.loader.exec_module(konclude)

EX = "http://example.org/"
OWX = """<?xml version="1.0"?>
<Ontology xmlns="http://www.w3.org/2002/07/owl#" ontologyIRI="http://example.org/h">
  <Prefix name="ex" IRI="http://example.org/"/>
  <Declaration><Class IRI="http://example.org/A"/></Declaration>
  <Declaration><Class abbreviatedIRI="ex:B"/></Declaration>
  <Declaration><Class IRI="http://example.org/C"/></Declaration>
  <Declaration><Class IRI="http://example.org/D"/></Declaration>
  <Declaration><Class IRI="http://example.org/E"/></Declaration>
  <Declaration><Class IRI="http://example.org/U"/></Declaration>
  <Declaration><Class IRI="http://example.org/T"/></Declaration>
  <SubClassOf><Class IRI="http://example.org/B"/><Class IRI="http://example.org/A"/></SubClassOf>
  <SubClassOf><Class IRI="http://example.org/C"/><Class IRI="http://example.org/B"/></SubClassOf>
  <SubClassOf><Class IRI="http://example.org/D"/><Class IRI="http://example.org/C"/></SubClassOf>
  <SubClassOf><Class IRI="http://example.org/C"/><Class IRI="http://example.org/D"/></SubClassOf>
  <SubClassOf><Class IRI="http://example.org/E"/><Class IRI="http://example.org/A"/></SubClassOf>
  <SubClassOf><Class IRI="http://example.org/U"/><Class abbreviatedIRI="owl:Nothing"/></SubClassOf>
  <SubClassOf><Class abbreviatedIRI="owl:Thing"/><Class IRI="http://example.org/T"/></SubClassOf>
</Ontology>
"""


def lines(path):
    return Path(path).read_text().splitlines()


class KoncludeConvertTests(unittest.TestCase):
    def convert(self, text, suffix, reduce=False, input_text=None):
        d = Path(tempfile.mkdtemp())
        raw = d / f"raw{suffix}"
        raw.write_text(text)
        inp = None
        if input_text is not None:
            inp = d / "in.ofn"
            inp.write_text(input_text)
        out = d / "tax.ofn"
        result = konclude.convert(str(raw), str(out), str(inp) if inp else None, reduce)
        return result, lines(out)

    def test_owl_xml_hierarchy(self):
        (consistent, classes, unsat), got = self.convert(OWX, ".owx")
        self.assertTrue(consistent)
        self.assertEqual(classes, 7)
        self.assertEqual(unsat, 1)
        a, b, c, d, e, u, t = (f"<{EX}{x}>" for x in "ABCDEUT")
        self.assertIn(f"EquivalentClasses({c} {d})", got)
        # members sort as strings, as ElkClassify writes them
        self.assertIn(f"EquivalentClasses({t} <{konclude.THING}>)", got)
        self.assertIn(f"EquivalentClasses({u} <{konclude.NOTHING}>)", got)
        self.assertIn(f"SubClassOf({b} {a})", got)
        self.assertIn(f"SubClassOf({c} {b})", got)
        self.assertIn(f"SubClassOf({e} {a})", got)
        self.assertIn(f"SubClassOf({a} {t})", got)
        self.assertEqual(got, sorted(got))

    def test_functional_syntax_and_reduction(self):
        text = "\n".join(
            [
                "Prefix(:=<http://example.org/>)",
                "Ontology(",
                "SubClassOf(:B :A)",
                "SubClassOf(:C :B)",
                "SubClassOf(:C :A)",
                "SubClassOf(:A owl:Thing)",
                ")",
            ]
        )
        _, got = self.convert(text, ".ofn")
        self.assertIn(f"SubClassOf(<{EX}C> <{EX}A>)", got)
        _, got = self.convert(text, ".ofn", reduce=True)
        self.assertNotIn(f"SubClassOf(<{EX}C> <{EX}A>)", got)
        self.assertIn(f"SubClassOf(<{EX}C> <{EX}B>)", got)

    def test_inconsistent_ontology_takes_the_input_classes(self):
        text = "Ontology(\nSubClassOf(owl:Thing owl:Nothing)\n)\n"
        source = "Prefix(:=<http://example.org/>)\nOntology(\nDeclaration(Class(:A))\nDeclaration(Class(<http://example.org/B>))\n)\n"
        (consistent, classes, unsat), got = self.convert(text, ".ofn", input_text=source)
        self.assertFalse(consistent)
        self.assertEqual(classes, 2)
        self.assertEqual(unsat, 3)
        self.assertEqual(
            got,
            [f"EquivalentClasses(<{EX}A> <{EX}B> <{konclude.NOTHING}> <{konclude.THING}>)"],
        )

    def test_the_oracle_reads_the_output(self):
        d = Path(tempfile.mkdtemp())
        (d / "raw.owx").write_text(OWX)
        konclude.convert(str(d / "raw.owx"), str(d / "ref.ofn"))
        ours = [
            f"EQ {EX}C {EX}D",
            f"EQ {konclude.NOTHING} {EX}U",
            f"EQ {konclude.THING} {EX}T",
            f"SUB {EX}A {konclude.THING}",
            f"SUB {EX}B {EX}A",
            f"SUB {EX}C {EX}B",
            f"SUB {EX}E {EX}A",
        ]
        (d / "ours.txt").write_text("\n".join(ours) + "\n")
        r = subprocess.run(
            [sys.executable, str(ROOT / "scripts" / "el-oracle.py"), str(d / "ref.ofn"), str(d / "ours.txt")],
            capture_output=True,
            text=True,
        )
        report = json.loads(r.stdout)
        self.assertTrue(report["agree"], report)
        self.assertEqual(report["unsatisfiable"], 1)


if __name__ == "__main__":
    unittest.main()
