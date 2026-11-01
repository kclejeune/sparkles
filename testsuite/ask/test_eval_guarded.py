"""Credential-free tests of the guarded secondary score and of killed Ask children."""

import importlib.machinery
import importlib.util
import hashlib
import pathlib
import signal
import sys
import tempfile
import unittest

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE / "eval_support"))
import guarded
from evaluator import answer_set

loader = importlib.machinery.SourceFileLoader(
    "evaluation_guarded_under_test", str(HERE.parents[1] / "scripts/eval-ask")
)
spec = importlib.util.spec_from_loader(loader.name, loader)
eval_ask = importlib.util.module_from_spec(spec)
loader.exec_module(eval_ask)

XSD = "http://www.w3.org/2001/XMLSchema#"


def uri(v):
    return {"type": "uri", "value": v}


def lit(v, dt=None, lang=None):
    t = {"type": "literal", "value": v}
    if dt:
        t["datatype"] = XSD + dt
    if lang:
        t["xml:lang"] = lang
    return t


def rel(vars_, *rows):
    return answer_set(
        {
            "head": {"vars": list(vars_)},
            "results": {
                "bindings": [
                    {v: c for v, c in zip(vars_, row) if c is not None} for row in rows
                ]
            },
        }
    )


def lenient(gold, got, labels):
    ok, _ = guarded.compare(gold, got)
    if ok:
        return True
    g, a, _ = guarded.canonical(gold, got, labels)
    return guarded.compare(g, a)[0]


class GuardedScore(unittest.TestCase):
    def test_numbers_match_across_numeric_datatypes_only(self):
        gold = rel(["n"], [lit("34317", "integer")])
        self.assertFalse(guarded.compare(gold, rel(["c"], [lit("34317", "nonNegativeInteger")]))[0])
        self.assertTrue(lenient(gold, rel(["c"], [lit("34317", "nonNegativeInteger")]), {}))
        self.assertTrue(lenient(gold, rel(["c"], [lit("3.4317E4", "double")]), {}))
        self.assertFalse(lenient(gold, rel(["c"], [lit("34317")]), {}))
        self.assertFalse(lenient(gold, rel(["c"], [lit("34318", "int")]), {}))

    def test_a_label_names_a_gold_iri_only_when_unambiguous(self):
        gold = rel(["uri"], [uri("ex:Pinta_(ship)")], [uri("ex:Nina")])
        got = rel(["name"], [lit("Pinta (ship)", lang="en")], [lit("Niña", lang="en")])
        labels = {"ex:Pinta_(ship)": {"Pinta (ship)"}, "ex:Nina": {"Niña"}}
        self.assertTrue(lenient(gold, got, labels))
        # The same text labels two gold IRIs, so it names neither.
        labels["ex:Nina"].add("Pinta (ship)")
        self.assertFalse(lenient(gold, got, labels))
        # A label of no gold IRI never matches.
        self.assertFalse(lenient(gold, rel(["n"], [lit("Pinta")], [lit("Niña")]), {}))

    def test_mixed_gold_literal_matches_the_answer_iri_it_labels(self):
        gold = rel(["uri"], [lit("Podgorica", lang="en")], [uri("ex:Cetinje")])
        got = rel(["c"], [uri("ex:Cetinje")], [uri("ex:Podgorica")])
        self.assertTrue(lenient(gold, got, {"ex:Podgorica": {"Podgorica"}}))
        self.assertFalse(lenient(gold, got, {}))
        # A literal-only gold column is not mixed and does not take IRIs.
        gold = rel(["name"], [lit("Podgorica", lang="en")])
        self.assertFalse(lenient(gold, rel(["c"], [uri("ex:Podgorica")]), {"ex:Podgorica": {"Podgorica"}}))
        self.assertEqual(guarded.label_requests(gold, rel(["c"], [uri("ex:Podgorica")])), ([], []))

    def test_empty_strings_drop_from_both_sides(self):
        gold = rel(["x"], [uri("ex:a")], [lit("")])
        g2, n = guarded.drop_empty(gold)
        self.assertEqual(n, 1)
        self.assertTrue(guarded.compare(g2, guarded.drop_empty(rel(["y"], [uri("ex:a")]))[0])[0])
        self.assertTrue(guarded.compare(g2, guarded.drop_empty(rel(["y"], [uri("ex:a")], [lit("", lang="en")]))[0])[0])

    def test_extra_column_cap_and_flags(self):
        gold = rel(["x"], [uri("ex:a")])
        four = rel(["x", "a", "b", "c", "d"], [uri("ex:a"), lit("1"), lit("2"), lit("3"), lit("4")])
        self.assertEqual(guarded.compare(gold, four)[1]["status"], "guard_extra_columns")
        three = rel(["x", "a", "b", "c"], [uri("ex:a"), lit("1"), lit("2"), lit("3")])
        self.assertTrue(guarded.compare(gold, three)[0])
        # an extra column of predicates
        got = rel(["x", "p"], [uri("ex:a"), uri("ex:p1")])
        ok, d = guarded.compare(gold, got)
        self.assertTrue(ok)
        self.assertEqual(guarded.flags(gold, got, d, lambda iris: set(iris)), ["predicate_column"])
        self.assertEqual(guarded.flags(gold, got, d, lambda iris: set()), [])
        self.assertEqual(guarded.flags(gold, got, d, lambda iris: None), ["predicate_column_unknown"])
        # a projection that collapses rows
        got = rel(["x", "l"], [uri("ex:a"), lit("A")], [uri("ex:a"), lit("B")])
        ok, d = guarded.compare(gold, got)
        self.assertTrue(ok)
        self.assertEqual(guarded.flags(gold, got, d, lambda iris: set()), ["projection_drops_rows"])
        # two competing guesses on one row, the second matching
        got = rel(["g1", "g2"], [uri("ex:b"), uri("ex:a")])
        ok, d = guarded.compare(gold, got)
        self.assertTrue(ok)
        self.assertEqual(guarded.flags(gold, got, d, lambda iris: set()), ["rival_column"])

    def test_errata_rules(self):
        q = {"id": "q1", "gold": "SELECT DISTINCT * WHERE { ?x ?p ?y }"}
        gold = rel(["x"], [uri("ex:a")])
        self.assertEqual(guarded.errata_exclusion(q, gold, {})["rule"], "select-star")
        q = {"id": "q1", "gold": 'SELECT ?x WHERE { ?x ?p "SELECT *" }'}
        self.assertIsNone(guarded.errata_exclusion(q, gold, {}))
        q = {"id": "q1", "gold": "SELECT ?x ?s WHERE { ?x ?p ?o }"}
        unbound = rel(["x", "s"], [uri("ex:a"), None])
        self.assertEqual(guarded.errata_exclusion(q, unbound, {})["rule"], "unbound-column")
        sha = hashlib.sha256(q["gold"].encode()).hexdigest()
        named = {("q1", sha): {"reason": "r"}}
        self.assertEqual(guarded.errata_exclusion(q, gold, named)["rule"], "named")
        # an entry for another gold text with the same id does not apply
        self.assertIsNone(guarded.errata_exclusion(dict(q, gold=q["gold"] + " "), gold, named))
        self.assertTrue(guarded.load_errata())


class KilledChild(unittest.TestCase):
    def test_sigkill_and_allocation_abort_are_case_failures(self):
        with tempfile.TemporaryDirectory() as d:
            p = pathlib.Path(d)
            (p / "stderr").write_bytes(b"memory allocation of 8 bytes failed\n")
            self.assertTrue(eval_ask.killed({"returncode": -signal.SIGKILL}, p))
            self.assertTrue(eval_ask.killed({"returncode": -signal.SIGABRT}, p))
            (p / "stderr").write_bytes(b"thread panicked\n")
            self.assertFalse(eval_ask.killed({"returncode": -signal.SIGABRT}, p))
            self.assertFalse(eval_ask.killed({"returncode": -signal.SIGSEGV}, p))
            self.assertFalse(eval_ask.killed({"returncode": 1}, p))
            self.assertFalse(eval_ask.killed({"returncode": None}, p))


if __name__ == "__main__":
    unittest.main()
