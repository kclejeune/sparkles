"""Tests of scripts/bench-bsbm/bi-float-oracle.py on a reduced BSBM BI fixture.

    python3 -m unittest discover -s scripts/bench-tests
"""

import importlib.util
import json
import os
import struct
import subprocess
import sys
import tempfile
import unittest

SCRIPTS = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ORACLE = os.path.join(SCRIPTS, "bench-bsbm", "bi-float-oracle.py")
spec = importlib.util.spec_from_file_location("oracle", ORACLE)
oracle = importlib.util.module_from_spec(spec)
spec.loader.exec_module(oracle)

I = "http://www4.wiwiss.fu-berlin.de/bizer/bsbm/v01/instances/"
V = "http://www4.wiwiss.fu-berlin.de/bizer/bsbm/v01/vocabulary/"
FLOAT = "http://www.w3.org/2001/XMLSchema#float"
# prices whose float32 sum depends on the order of the additions
PRICES = {"o1": "16777216.00", "o2": "1.00", "o3": "1.00", "o4": "3.50", "o5": "7.25", "o6": "100.10"}
# offer -> product, product -> features
OFFERS = {"o1": "p1", "o2": "p1", "o3": "p2", "o4": "p2", "o5": "p3", "o6": "p3"}
FEATURES = {"p1": ["f1"], "p2": ["f1", "f2"], "p3": ["f2"]}


def f32(x):
    return struct.unpack("<f", struct.pack("<f", x))[0]


def fixture(d):
    lines = []
    for p in FEATURES:
        lines.append(f"<{I}{p}> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <{I}ProductType1> .")
        for f in FEATURES[p]:
            lines.append(f"<{I}{p}> <{V}productFeature> <{I}{f}> .")
    for o, p in OFFERS.items():
        lines.append(f"<{I}{o}> <{V}product> <{I}{p}> .")
        lines.append(f'<{I}{o}> <{V}price> "{PRICES[o]}"^^<{V}USD> .')
    lines.append(f"<{I}r1> <{V}reviewFor> <{I}p1> .")
    lines.append(f"<{I}r1> <http://purl.org/stuff/rev#reviewer> <{I}u1> .")
    lines.append(f"<{I}u1> <{V}country> <http://downlode.org/rdf/iso-3166/countries#US> .")
    data = os.path.join(d, "dataset.nt")
    with open(data, "w") as f:
        f.write("\n".join(lines) + "\n")
    q = os.path.join(d, "queries")
    os.mkdir(q)
    with open(os.path.join(q, "bi-q4-1.rq"), "w") as f:
        f.write(f"SELECT ... <{I}ProductType1> ...")
    with open(os.path.join(q, "bi-q5-1.rq"), "w") as f:
        f.write(f"SELECT ... <{I}ProductType1> ...")
    os.mkdir(os.path.join(d, "raw"))
    return data, q


def mean(prices, accumulate):
    s = 0.0
    for x in prices:
        s = accumulate(s + f32(float(x)))
    return accumulate(s / len(prices))


def q4_answer(accumulate, order=lambda xs: xs, drop=None):
    rows = []
    for feat in ("f1", "f2"):
        w = [PRICES[o] for o, p in OFFERS.items() if feat in FEATURES[p] and o != drop]
        wo = [PRICES[o] for o, p in OFFERS.items() if feat not in FEATURES[p] and o != drop]
        r = accumulate(mean(order(w), accumulate) / mean(order(wo), accumulate))
        rows.append((r, feat))
    rows.sort(key=lambda t: (-t[0], t[1]))
    return {"head": {"vars": ["feature", "priceRatio"]}, "results": {"bindings": [
        {"feature": {"type": "uri", "value": I + f}, "priceRatio": {"type": "literal", "datatype": FLOAT, "value": repr(r)}}
        for r, f in rows]}}


def q5_answer(avg, reviews=1):
    return {"head": {"vars": ["country", "product", "nrOfReviews", "avgPrice"]}, "results": {"bindings": [{
        "country": {"type": "uri", "value": "http://downlode.org/rdf/iso-3166/countries#US"},
        "product": {"type": "uri", "value": I + "p1"},
        "nrOfReviews": {"type": "literal", "value": str(reviews)},
        "avgPrice": {"type": "literal", "datatype": FLOAT, "value": repr(avg)}}]}}


class OracleTests(unittest.TestCase):
    def run_oracle(self, answers):
        with tempfile.TemporaryDirectory() as d:
            data, q = fixture(d)
            for name, engine, body in answers:
                with open(os.path.join(d, "raw", f"{name}.{engine}.json"), "w") as f:
                    json.dump(body, f)
            out = os.path.join(d, "report.json")
            p = subprocess.run([sys.executable, "-I", ORACLE, data, q, os.path.join(d, "raw"), "--json", out],
                               capture_output=True, text=True)
            with open(out) as f:
                return p, json.load(f)

    def test_summation_orders_and_wider_accumulators_agree(self):
        p, rep = self.run_oracle([
            ("bi-q4-1", "forward", q4_answer(f32)),
            ("bi-q4-1", "reverse", q4_answer(f32, order=lambda xs: list(reversed(xs)))),
            ("bi-q4-1", "double", q4_answer(lambda x: x)),
        ])
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        self.assertEqual({e: v["status"] for e, v in rep["bi-q4-1"].items()},
                         {"forward": "ok", "reverse": "ok", "double": "ok"})

    def test_the_orders_really_give_different_floats(self):
        # the fixture is only useful if the engines' legal answers differ beyond 12 digits
        a = q4_answer(f32)["results"]["bindings"]
        b = q4_answer(f32, order=lambda xs: list(reversed(xs)))["results"]["bindings"]
        self.assertNotEqual([x["priceRatio"]["value"] for x in a], [x["priceRatio"]["value"] for x in b])

    def test_a_missing_offer_is_a_disagreement(self):
        p, rep = self.run_oracle([("bi-q4-1", "wrong", q4_answer(f32, drop="o6"))])
        self.assertEqual(p.returncode, 1)
        self.assertEqual(rep["bi-q4-1"]["wrong"]["status"], "disagrees")

    def test_q5_counts_must_match_and_averages_within_rounding(self):
        good = mean([PRICES["o1"], PRICES["o2"]], f32)
        p, rep = self.run_oracle([
            ("bi-q5-1", "ok", q5_answer(good)),
            ("bi-q5-1", "count", q5_answer(good, reviews=2)),
            ("bi-q5-1", "value", q5_answer(good * 1.001)),
        ])
        self.assertEqual(p.returncode, 1)
        st = {e: v["status"] for e, v in rep["bi-q5-1"].items()}
        self.assertEqual(st, {"ok": "ok", "count": "disagrees", "value": "disagrees"})
        self.assertEqual(rep["bi-q5-1"]["ok"]["datatypes"], [FLOAT])

    def test_a_repeated_row_is_a_disagreement(self):
        q4 = q4_answer(f32)
        b = q4["results"]["bindings"]
        b[1] = dict(b[0])
        good = mean([PRICES["o1"], PRICES["o2"]], f32)
        q5 = q5_answer(good)
        q5["results"]["bindings"] *= 2
        p, rep = self.run_oracle([("bi-q4-1", "dup", q4), ("bi-q5-1", "dup", q5)])
        self.assertEqual(p.returncode, 1)
        self.assertEqual(rep["bi-q4-1"]["dup"]["status"], "disagrees")
        self.assertEqual(rep["bi-q5-1"]["dup"]["status"], "disagrees")

    def ranked(self, values, bound="1/1000"):
        rows = {f"<f{i}>": (oracle.Fraction(v), oracle.Fraction(bound)) for i, v in enumerate(values)}
        order = sorted(rows, key=lambda f: (-rows[f][0], f))
        return rows, order

    def answer_of(self, rows, feats):
        return {"results": {"bindings": [{"feature": {"type": "uri", "value": f[1:-1]},
                                          "priceRatio": {"type": "literal", "value": str(float(rows[f][0]))}}
                                         for f in feats]}}

    def test_a_left_out_winner_is_not_rounding(self):
        # ranks 2 to 11 without the clear first, although ranks 10 and 11 overlap
        rows, order = self.ranked([100] + [20 - i for i in range(8)] + ["11.001", "11.0005"])
        got = self.answer_of(rows, order[1:11])
        self.assertTrue(any("left out" in x for x in oracle.check_q4(rows, order, got)))

    def test_a_swap_within_the_rounding_is_accepted(self):
        rows, order = self.ranked([100] + [20 - i for i in range(8)] + ["11.001", "11.0005"])
        got = self.answer_of(rows, order[:9] + [order[10]])
        self.assertEqual(oracle.check_q4(rows, order, got), [])

    def test_nothing_to_check_fails(self):
        with tempfile.TemporaryDirectory() as d:
            data, q = fixture(d)
            p = subprocess.run([sys.executable, "-I", ORACLE, data, q, os.path.join(d, "raw")],
                               capture_output=True, text=True)
            self.assertNotEqual(p.returncode, 0)
            self.assertIn("nothing was checked", p.stderr)
            empty = os.path.join(d, "none")
            os.mkdir(empty)
            p = subprocess.run([sys.executable, "-I", ORACLE, data, empty, os.path.join(d, "raw")],
                               capture_output=True, text=True)
            self.assertNotEqual(p.returncode, 0)
            self.assertIn("no bi-q4", p.stderr)

    def test_float_parsing_is_binary32(self):
        self.assertEqual(oracle.f32("0.1"), oracle.Fraction(f32(0.1)))
        self.assertNotEqual(oracle.f32("0.1"), oracle.Fraction(0.1))


if __name__ == "__main__":
    unittest.main()
