"""Check scripts/rl-reference/rl_terms.py, the term encoding and the closure comparison of
the OWL 2 RL reference: the datatypes it gives each literal (the rule dt-type2), the
round trip of encoded terms, the split of an N-Quads dump, and the differ's verdicts."""

import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("rl_terms", ROOT / "scripts" / "rl-reference" / "rl_terms.py")
rl = importlib.util.module_from_spec(spec)
spec.loader.exec_module(rl)

XSD = rl.XSD
RDF = rl.RDF
RDFS = rl.RDFS


def dts(term):
    return set(rl.datatypes(*rl.literal(term))[0])


class Datatypes(unittest.TestCase):
    def test_integer_members(self):
        got = dts(f'"1"^^<{XSD}int>')
        for local in ("decimal", "integer", "positiveInteger", "byte", "unsignedByte", "int", "long"):
            self.assertIn(XSD + local, got)
        self.assertNotIn(XSD + "negativeInteger", got)
        self.assertNotIn(XSD + "double", got)
        self.assertTrue(rl.datatypes(*rl.literal(f'"01"^^<{XSD}int>'))[1])

    def test_integer_ranges(self):
        self.assertNotIn(XSD + "byte", dts(f'"128"^^<{XSD}integer>'))
        self.assertIn(XSD + "unsignedByte", dts(f'"255"^^<{XSD}integer>'))
        self.assertIn(XSD + "nonPositiveInteger", dts(f'"0"^^<{XSD}integer>'))
        self.assertIn(XSD + "nonNegativeInteger", dts(f'"0"^^<{XSD}integer>'))

    def test_integral_decimal_is_an_integer(self):
        got = dts(f'"5.0"^^<{XSD}decimal>')
        self.assertIn(XSD + "integer", got)
        self.assertNotIn(XSD + "integer", dts(f'"5.5"^^<{XSD}decimal>'))
        self.assertTrue(rl.datatypes(*rl.literal(f'"1.00"^^<{XSD}decimal>'))[1])

    def test_ill_typed_literal_has_no_datatype(self):
        self.assertEqual(dts(f'"abc"^^<{XSD}integer>'), set())
        self.assertEqual(dts(f'"300"^^<{XSD}unsignedByte>'), set())

    def test_strings(self):
        got = dts('"en-GB"')
        for t in (XSD + "string", RDF + "PlainLiteral", XSD + "token", XSD + "language", XSD + "NMTOKEN", XSD + "Name"):
            self.assertIn(t, got)
        got = dts('"two words"')
        self.assertIn(XSD + "token", got)
        self.assertNotIn(XSD + "NMTOKEN", got)
        self.assertNotIn(XSD + "token", dts('"two  spaces"'))
        self.assertNotIn(XSD + "NCName", dts('"a:b"'))
        self.assertIn(XSD + "Name", dts('"a:b"'))

    def test_language_tagged_string(self):
        self.assertEqual(dts('"chat"@FR'), {RDFS + "Literal", RDF + "PlainLiteral"})

    def test_floating_point_and_others(self):
        self.assertEqual(dts(f'"1.5"^^<{XSD}double>'), {RDFS + "Literal", XSD + "double"})
        self.assertFalse(rl.datatypes(*rl.literal(f'"1"^^<{XSD}double>'))[1])
        self.assertIn(XSD + "dateTimeStamp", dts(f'"2020-01-01T00:00:00Z"^^<{XSD}dateTime>'))
        self.assertNotIn(XSD + "dateTimeStamp", dts(f'"2020-01-01T00:00:00"^^<{XSD}dateTime>'))
        self.assertEqual(dts('"x"^^<http://example.org/dt>'), {RDFS + "Literal"})

    def test_escapes(self):
        self.assertEqual(rl.literal(r'"a\"bé\n"')[0], 'a"bé\n')


class RoundTrip(unittest.TestCase):
    def test_encode_decode_split_diff(self):
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            (d / "dump.nq").write_text(
                '<urn:s> <urn:p> "1"^^<http://www.w3.org/2001/XMLSchema#long> .\n'
                "<urn:s> <urn:p> _:b0 .\n"
                '_:b0 <urn:q> "x"@en <urn:x-sparkles:inferred> .\n'
                "<urn:s> <urn:r> <urn:o> <urn:x-sparkles:inferred> .\n"
            )
            rl.split(str(d / "dump.nq"), str(d / "base.nt"), str(d / "derived.nt"), "urn:x-sparkles:inferred")
            self.assertEqual(len((d / "base.nt").read_text().splitlines()), 2)
            (d / "enc").mkdir()
            rl.encode(str(d / "base.nt"), str(d / "enc"))
            encoded = (d / "enc" / "base.nt").read_text()
            self.assertNotIn('"', encoded)
            self.assertNotIn("_:", encoded)
            lits = (d / "enc" / "literals.nt").read_text()
            self.assertIn(f"<{rl.RL}kind> <{rl.RL}literal>", lits)
            self.assertIn(f"<{rl.RL}value> <{rl.RL}one>", lits)
            # a derived triple over encoded terms decodes to the original terms
            bnode = [ln.split()[2] for ln in encoded.splitlines() if "x-bnode" in ln][0]
            (d / "nemo.nt").write_text(f"{bnode} <urn:q> <urn:o> .\n{bnode} <urn:q> <urn:o> .\n")
            rl.decode(str(d / "enc"), str(d / "nemo.nt"), str(d / "ref.nt"))
            self.assertEqual((d / "ref.nt").read_text(), "_:b0 <urn:q> <urn:o> .\n")

    def test_decode_refuses_an_unknown_encoded_term(self):
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            (d / "terms.tsv").write_text("")
            (d / "nemo.nt").write_text("<urn:x-lit:7> <urn:p> <urn:o> .\n")
            with self.assertRaises(ValueError):
                rl.decode(str(d), str(d / "nemo.nt"), str(d / "out.nt"))


class Diff(unittest.TestCase):
    def run_diff(self, a, b, known="", subset=False):
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            (d / "a.nt").write_text(a)
            (d / "b.nt").write_text(b)
            (d / "known.tsv").write_text(known)
            rc = rl.diff(str(d / "a.nt"), str(d / "b.nt"), str(d / "r.json"), str(d / "known.tsv"), subset)
            return rc, json.loads((d / "r.json").read_text())

    def test_equal(self):
        rc, r = self.run_diff("<urn:a> <urn:p> <urn:b> .\n", "<urn:a> <urn:p> <urn:b> .\n")
        self.assertEqual(rc, 0)
        self.assertEqual(r["common"], 1)

    def test_unexplained_difference_fails(self):
        rc, r = self.run_diff("<urn:a> <urn:p> <urn:b> .\n", "")
        self.assertEqual(rc, 1)
        self.assertEqual(r["differences"]["sparkles"]["by_explanation"], {"unexplained": 1})

    def test_known_difference_passes(self):
        known = "# side\tpattern\twhy\nreference\t<urn:chain4>\ta chain of four properties\n"
        rc, r = self.run_diff("", "<urn:a> <urn:chain4> <urn:b> .\n", known)
        self.assertEqual(rc, 0)
        self.assertEqual(r["differences"]["reference"]["by_explanation"], {"a chain of four properties": 1})

    def test_subset_counts_only_the_reference_side(self):
        rc, _ = self.run_diff("<urn:a> <urn:p> <urn:b> .\n", "", subset=True)
        self.assertEqual(rc, 0)
        rc, _ = self.run_diff("", "<urn:a> <urn:p> <urn:b> .\n", subset=True)
        self.assertEqual(rc, 1)


if __name__ == "__main__":
    unittest.main()
