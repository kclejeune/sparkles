"""Data-backed score and cutoff checks must accept valid differences and reject wrong rows."""
import copy
from fractions import Fraction
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

SCRIPT = Path(__file__).parents[1] / "bench-bsbm" / "bi-answer-oracle.py"
spec = importlib.util.spec_from_file_location("bi_answer_oracle", SCRIPT)
oracle = importlib.util.module_from_spec(spec)
spec.loader.exec_module(oracle)
V = "http://www4.wiwiss.fu-berlin.de/bizer/bsbm/v01/vocabulary/"
XSD = "http://www.w3.org/2001/XMLSchema#"
PRODUCER = b"<urn:producer>"


def score_answer(value, datatype="float", people=("alice",)):
    return {"head": {"vars": ["reviewer", "reviewerAvgScore"]}, "results": {"bindings": [
        {"reviewer": {"type": "uri", "value": "urn:" + person},
         "reviewerAvgScore": {"type": "literal", "datatype": XSD + datatype, "value": str(value)}}
        for person in people]}}


def products_answer(products):
    return {"head": {"vars": ["product"]}, "results": {"bindings": [
        {"product": {"type": "uri", "value": "urn:" + p}} for p in products]}}


class ScoreTests(unittest.TestCase):
    def data(self):
        # Alice's28/3mean passes HAVING. Bob's1mean does not. Equal ratings on
        # different predicates are two UNION rows; repeated source triples are one.
        lines = [f'<urn:p> <{V}producer> <urn:producer> .']
        for r, person, values in [("a", "alice", [9, 9, 10]), ("b", "bob", [1, 1, 1]), ("c", "bob", [1, 1, 1])]:
            lines += [f'<urn:{r}> <{V}reviewFor> <urn:p> .',
                      f'<urn:{r}> <http://purl.org/stuff/rev#reviewer> <urn:{person}> .']
            lines += [f'<urn:{r}> <{V}rating{i}> "{v}"^^<{XSD}integer> .' for i, v in enumerate(values, 1)]
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "data.nt"
            path.write_text("\n".join([*reversed(lines), lines[-1]]) + "\n")
            return oracle.inputs(path, {"q": ("q6", {"producer": PRODUCER})})

    def test_float_double_and_decimal_are_equivalent_inside_data_bounds(self):
        expected = oracle.q6_rows(self.data(), PRODUCER)
        self.assertEqual(expected[1], {b"<urn:alice>"})
        for datatype, value in [("float", float(oracle.binary32(Fraction(28, 3)))),
                                ("double", 28 / 3), ("decimal", "9.333333333333333333333333333")]:
            self.assertEqual(oracle.check_q6(expected, score_answer(value, datatype)), [])

    def test_wrong_score_missing_duplicate_and_ineligible_people_are_rejected(self):
        expected = oracle.q6_rows(self.data(), PRODUCER)
        for answer in [score_answer(10), score_answer(28 / 3, people=()),
                       score_answer(28 / 3, people=("alice", "alice")),
                       score_answer(28 / 3, people=("alice", "bob")),
                       score_answer(28 / 3, datatype="string"), score_answer("NaN")]:
            self.assertTrue(oracle.check_q6(expected, answer), answer)

    def test_identical_rating_values_on_distinct_union_arms_are_not_deduplicated(self):
        data = self.data()
        expected = oracle.q6_rows(data, PRODUCER)
        low, high = expected[0][b"<urn:alice>"]
        self.assertLessEqual(low, Fraction(28, 3))
        self.assertGreaterEqual(high, Fraction(28, 3))
        self.assertFalse(low <= Fraction(19, 2) <= high)


class CutoffTests(unittest.TestCase):
    def data(self):
        t = b"<urn:type>"
        counts = {b"<urn:A>": 3, b"<urn:B>": 2, b"<urn:C>": 2, b"<urn:D>": 2, b"<urn:E>": 1}
        data = {"members": {t: set(counts)}, "counts": counts,
                "excluded": {b"<urn:US>": {b"<urn:D>"}, b"<urn:JP>": {b"<urn:A>"}}}
        return data, {"type": t, "country": b"<urn:US>", "limit": 3}

    def test_valid_different_row_counts_at_tied_inner_limit(self):
        data, spec = self.data()
        expected = oracle.q7_rows(data, spec)
        self.assertEqual((len(expected["mandatory"]) + expected["low"],
                          len(expected["mandatory"]) + expected["high"]), (2, 3))
        for products in [("A", "B"), ("A", "C"), ("A", "B", "C")]:
            self.assertEqual(oracle.check_q7(expected, products_answer(products)), [])

    def test_omitted_mandatory_below_cutoff_repeats_and_impossible_hidden_slots_fail(self):
        data, spec = self.data()
        expected = oracle.q7_rows(data, spec)
        for products in [("B", "C"), ("A", "E"), ("A", "B", "B"), ("A",), ("A", "D")]:
            self.assertTrue(oracle.check_q7(expected, products_answer(products)), products)

    def test_country_is_taken_from_each_instance(self):
        data, spec = self.data()
        us = oracle.q7_rows(data, spec)
        jp = oracle.q7_rows(data, {**spec, "country": b"<urn:JP>"})
        self.assertIn(b"<urn:A>", us["mandatory"])
        self.assertNotIn(b"<urn:A>", jp["mandatory"])
        text = "SELECT <http://www4.wiwiss.fu-berlin.de/bizer/bsbm/v01/instances/ProductType8> " \
               "<http://downlode.org/rdf/iso-3166/countries#JP> LIMIT 1000"
        self.assertTrue(oracle.parameters(text, "q7", check_shape=False)["country"].endswith(b"#JP>"))

    def test_changed_query_shape_is_refused(self):
        with self.assertRaisesRegex(ValueError, "unsupported"):
            oracle.parameters("SELECT ?product WHERE {} LIMIT 1000", "q7")
        original = "SELECT ?product WHERE {} LIMIT 1000"
        self.assertEqual(oracle.query_shape(original), oracle.query_shape(original.replace("1000", "5")))
        self.assertNotEqual(oracle.query_shape(original), oracle.query_shape(original.replace("?product", "?vendor")))

    def test_fewer_than_limit_means_every_surviving_product_is_mandatory(self):
        data, spec = self.data()
        expected = oracle.q7_rows(data, {**spec, "limit": 1000})
        self.assertEqual(expected["mandatory"], data["members"][spec["type"]] - data["excluded"][spec["country"]])
        self.assertTrue(oracle.check_q7(expected, products_answer(("A", "B", "C"))))


if __name__ == "__main__":
    unittest.main()
