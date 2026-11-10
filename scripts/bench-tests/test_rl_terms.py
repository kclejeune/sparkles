"""Check scripts/rl-reference/rl_terms.py, the term encoding and the closure comparison of
the OWL 2 RL reference: the datatypes it gives each literal (the rule dt-type2), the
round trip of encoded terms, the split of an N-Quads dump, and the differ's verdicts, which
must match those of a comparison of two in-memory sets."""

import contextlib
import importlib.util
import io
import json
import random
import re
import tempfile
import unittest
from collections import Counter
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

    def test_invalid_lexical_forms_have_no_datatype(self):
        for term in (
            f'"2026-99-99T99:99:99+99:99"^^<{XSD}dateTime>',
            f'"2023-02-29T00:00:00"^^<{XSD}dateTime>',
            f'"2020-01-01T24:00:01Z"^^<{XSD}dateTime>',
            f'"2020-01-01T00:00:00+14:01"^^<{XSD}dateTime>',
            f'"0FB"^^<{XSD}hexBinary>',
            f'"zz"^^<{XSD}hexBinary>',
            f'"SGVsbG8"^^<{XSD}base64Binary>',
            f'"\u0661\u0662"^^<{XSD}integer>',
            f'"02024-01-01T00:00:00Z"^^<{XSD}dateTime>',
            f'"2024-01-01T24:00:00.{"0" * 325}1Z"^^<{XSD}dateTime>',
        ):
            self.assertEqual(dts(term), set(), term)
        self.assertIn(XSD + "dateTimeStamp", dts(f'"2024-02-29T24:00:00Z"^^<{XSD}dateTime>'))
        self.assertIn(XSD + "dateTimeStamp", dts(f'"2024-01-01T00:00:59.999999999999999999Z"^^<{XSD}dateTime>'))
        self.assertIn(XSD + "dateTime", dts(f'"0024-01-01T00:00:00"^^<{XSD}dateTime>'))
        self.assertIn(XSD + "hexBinary", dts(f'"0fb7"^^<{XSD}hexBinary>'))
        self.assertIn(XSD + "base64Binary", dts(f'"SGVsbG8="^^<{XSD}base64Binary>'))

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

    def test_reader_takes_a_label_before_the_dot_and_a_comment(self):
        self.assertEqual(rl.terms("<urn:s> <urn:p> _:b.c.", 3)[0][2], "_:b.c")
        with tempfile.TemporaryDirectory() as d:
            f = Path(d) / "x.nt"
            f.write_text("<urn:s> <urn:p> _:b. # a comment\n")
            self.assertEqual(list(rl.triples(str(f))), [("<urn:s>", "<urn:p>", "_:b")])
        self.assertEqual(rl.terms("_:1x <urn:p> _:\u00e9\u00b7x .", 3)[0], ["_:1x", "<urn:p>", "_:\u00e9\u00b7x"])
        for line in ("<urn:s> <urn:p> _:-bad .", "<urn:s> <urn:p> _:bad/name .", "<urn:s> <urn:p> _:.b ."):
            with self.assertRaises(ValueError, msg=line):
                rl.terms(line, 3)

    def test_encode_marks_blank_nodes_and_refuses_reserved_iris(self):
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            (d / "in.nt").write_text("_:x <urn:p> <urn:o> .\n")
            rl.encode(str(d / "in.nt"), str(d))
            self.assertIn(f"<{rl.RL}kind> <{rl.RL}bnode>", (d / "literals.nt").read_text())
            for bad in ("<urn:x-lit:0>", "<urn:\\u0078-lit:0>", "<urn:x\\u002Dbnode:1>"):
                (d / "bad.nt").write_text(f'{bad} <urn:p> "x" .\n')
                with self.assertRaises(ValueError, msg=bad):
                    rl.encode(str(d / "bad.nt"), str(d))

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


def set_diff(sparkles, reference, known_rows, subset):
    """The comparison of two in-memory sets that rl_terms.diff replaces with a sort on disk,
    as the report and the stderr lines it gives."""
    known = [(s, re.compile(p), w) for s, p, w in known_rows]

    def load(path):
        return {f"{s} {p} {o} ." for s, p, o in rl.triples(path)}

    a, b = load(sparkles), load(reference)
    result = {"sparkles": len(a), "reference": len(b), "common": len(a & b), "subset": subset, "differences": {}}
    unexplained = []
    for side, only in (("sparkles", a - b), ("reference", b - a)):
        by_why, by_pred, examples = Counter(), Counter(), {}
        for line in sorted(only):
            by_pred[rl.terms(line, 2)[0][1]] += 1
            why = next((w for s, r, w in known if s == side and r.search(line)), None)
            if why is None:
                why = "unexplained"
                if not (subset and side == "sparkles"):
                    unexplained.append(f"{side} only: {line}")
            by_why[why] += 1
            if len(examples.setdefault(why, [])) < 5:
                examples[why].append(line)
        result["differences"][side] = {
            "total": len(only),
            "by_explanation": dict(by_why.most_common()),
            "by_predicate": dict(by_pred.most_common()),
            "examples": examples,
        }
    result["unexplained"] = len(unexplained)
    summary = (
        f"sparkles {len(a)}, reference {len(b)}, common {len(a & b)}, "
        f"sparkles only {len(a - b)}, reference only {len(b - a)}, unexplained {len(unexplained)}"
    )
    return result, [summary] + unexplained[:20]


class DiffMatchesSets(unittest.TestCase):
    """The sorted comparison gives the report, the examples and the stderr lines of the
    comparison of two sets on unsorted input with duplicates, non-ASCII terms, padding and
    more than twenty unexplained differences."""

    def graph(self, rng, n):
        subjects = ["<urn:a>", "<urn:b>", "<urn:\u00e9>", "<urn:z\u4e2d>", "_:b1", "<urn:\U0001f600>"]
        preds = ["<urn:p>", "<urn:q>", "<urn:chain4>", "<urn:\u00e9p>"]
        objs = ['"x"', '"\u00e9"@fr', '"1"^^<urn:int>', "<urn:o>", "_:b2", '"a\\"b"', "<urn:O>"]
        lines = []
        for _ in range(n):
            s, p, o = rng.choice(subjects), rng.choice(preds), rng.choice(objs)
            lines.append(f"{s}  {p} {o}." if rng.random() < 0.2 else f"{s} {p} {o} .")
        return "\n".join(lines) + "\n"

    def test_random_graphs(self):
        known_rows = [("reference", "<urn:chain4>", "a chain of four properties"), ("sparkles", "_:b1 ", "a blank subject")]
        known = "".join(f"{s}\t{p}\t{w}\n" for s, p, w in known_rows)
        for seed in range(12):
            rng = random.Random(seed)
            a, b = self.graph(rng, rng.randrange(0, 150)), self.graph(rng, rng.randrange(0, 150))
            for subset in (False, True):
                with self.subTest(seed=seed, subset=subset), tempfile.TemporaryDirectory() as d:
                    d = Path(d)
                    (d / "a.nt").write_text(a, encoding="utf-8")
                    (d / "b.nt").write_text(b, encoding="utf-8")
                    (d / "known.tsv").write_text(known, encoding="utf-8")
                    err = io.StringIO()
                    with contextlib.redirect_stderr(err):
                        rc = rl.diff(str(d / "a.nt"), str(d / "b.nt"), str(d / "r.json"), str(d / "known.tsv"), subset)
                    want, want_err = set_diff(str(d / "a.nt"), str(d / "b.nt"), known_rows, subset)
                    self.assertEqual(json.loads((d / "r.json").read_text()), want)
                    self.assertEqual(err.getvalue().splitlines(), want_err)
                    self.assertEqual(rc, 1 if want["unexplained"] else 0)
                    self.assertEqual(sorted(p.name for p in d.iterdir()), ["a.nt", "b.nt", "known.tsv", "r.json"])

    def test_decode_sorts_by_code_point_without_duplicates(self):
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            (d / "terms.tsv").write_text('<urn:x-lit:0>\t"\u00e9"\n<urn:x-bnode:1>\t_:b\n', encoding="utf-8")
            lines = [
                "<urn:\U0001f600> <urn:p> <urn:x-lit:0> .",
                "<urn:\u00e9> <urn:p> <urn:o> .",
                "<urn:x-bnode:1> <urn:p> <urn:o> .",
                "<urn:z> <urn:p> <urn:x-lit:0> .",
                "<urn:\u00e9> <urn:p> <urn:o> .",
                "<urn:Z> <urn:p> <urn:o> .",
            ]
            (d / "nemo.nt").write_text("\n".join(lines) + "\n", encoding="utf-8")
            with contextlib.redirect_stderr(io.StringIO()):
                rl.decode(str(d), str(d / "nemo.nt"), str(d / "out.nt"))
            m = {"<urn:x-lit:0>": '"\u00e9"', "<urn:x-bnode:1>": "_:b"}
            want = sorted({" ".join(m.get(t, t) for t in ln.split(" ")) for ln in lines})
            self.assertEqual((d / "out.nt").read_text(encoding="utf-8").splitlines(), want)

    def test_a_failed_decode_writes_no_output(self):
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            (d / "terms.tsv").write_text("")
            (d / "nemo.nt").write_text("<urn:s> <urn:p> <urn:o> .\n<urn:x-lit:7> <urn:p> <urn:o> .\n")
            with self.assertRaises(ValueError):
                rl.decode(str(d), str(d / "nemo.nt"), str(d / "out.nt"))
            self.assertEqual(sorted(p.name for p in d.iterdir()), ["nemo.nt", "terms.tsv"])


if __name__ == "__main__":
    unittest.main()
