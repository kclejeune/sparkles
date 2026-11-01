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
    found = guarded.lenient_compare(gold, got, labels)
    return bool(found and found[0])


class GuardedScore(unittest.TestCase):
    def test_numbers_match_across_numeric_datatypes_only(self):
        gold = rel(["n"], [lit("34317", "integer")])
        self.assertFalse(guarded.compare(gold, rel(["c"], [lit("34317", "nonNegativeInteger")]))[0])
        self.assertTrue(lenient(gold, rel(["c"], [lit("34317", "nonNegativeInteger")]), {}))
        self.assertTrue(lenient(gold, rel(["c"], [lit("3.4317E4", "double")]), {}))
        self.assertFalse(lenient(gold, rel(["c"], [lit("34317")]), {}))
        self.assertFalse(lenient(gold, rel(["c"], [lit("34318", "int")]), {}))
        self.assertFalse(lenient(gold, rel(["c"], [lit("34317/1", "integer")]), {}))
        # past Python's digit limit for integer conversion, a value is simply unknown
        self.assertIsNone(guarded.numeric_value(("literal", "9" * 5000, XSD + "integer")))
        self.assertIsNone(guarded.numeric_value(("literal", "9" * 5000 + ".5", XSD + "decimal")))
        self.assertFalse(lenient(gold, rel(["c"], [lit("9" * 5000, "integer")]), {}))
        big = rel(["n"], [lit("9007199254740993", "integer")])
        self.assertFalse(lenient(big, rel(["c"], [lit("9007199254740993", "double")]), {}))
        self.assertTrue(lenient(rel(["n"], [lit("0.5", "decimal")]), rel(["c"], [lit("5E-1", "float")]), {}))
        self.assertIsNone(guarded.numeric_value(("literal", "1e39", XSD + "float")))
        self.assertIsNone(guarded.numeric_value(("literal", "-1e39", XSD + "float")))
        self.assertFalse(lenient(rel(["n"], [lit("-1", "integer")]), rel(["c"], [lit("-1", "unsignedInt")]), {}))
        self.assertFalse(lenient(rel(["n"], [lit("128", "integer")]), rel(["c"], [lit("128", "byte")]), {}))
        self.assertTrue(lenient(rel(["n"], [lit("127", "integer")]), rel(["c"], [lit("127", "byte")]), {}))

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

    def test_a_label_column_survives_number_canonicalization(self):
        gold = rel(["x", "l", "n"], [uri("ex:a"), lit("Alice"), lit("1", "integer")])
        got = rel(["x", "l", "n"], [uri("ex:a"), lit("Alice"), lit("1", "nonNegativeInteger")])
        self.assertTrue(lenient(gold, got, {"ex:a": {"Alice"}}))
        self.assertTrue(lenient(gold, got, {}))
        wrong = rel(["x", "l", "n"], [uri("ex:a"), lit("Bob"), lit("1", "nonNegativeInteger")])
        self.assertFalse(lenient(gold, wrong, {"ex:a": {"Alice"}}))
        # a label column that already matches keeps its literal while another
        # column's label turns into the IRI
        gold = rel(["x", "l"], [uri("ex:a"), lit("Alice")])
        self.assertTrue(lenient(gold, rel(["y", "m"], [lit("Alice"), lit("Alice")]), {"ex:a": {"Alice"}}))
        self.assertFalse(lenient(gold, rel(["y", "m"], [lit("Alice"), lit("Bob")]), {"ex:a": {"Alice"}}))

    def test_empty_strings_drop_from_both_sides(self):
        gold = rel(["x"], [uri("ex:a")], [lit("")])
        g2, n = guarded.drop_empty(gold)
        self.assertEqual(n, 1)
        self.assertTrue(guarded.compare(g2, guarded.drop_empty(rel(["y"], [uri("ex:a")]))[0])[0])
        self.assertTrue(guarded.compare(g2, guarded.drop_empty(rel(["y"], [uri("ex:a")], [lit("", lang="en")]))[0])[0])
        # an unbound row holds no empty string and stays
        unbound = rel(["y", "l"], [uri("ex:a"), lit("A")], [None, None])
        self.assertEqual(guarded.drop_empty(unbound)[1], 0)
        # an empty label beside a wrong answer keeps that answer
        got = rel(["y", "l"], [uri("ex:a"), lit("A")], [uri("ex:b"), lit("")])
        self.assertFalse(guarded.compare(g2, guarded.drop_empty(got)[0])[0])
        # an empty side cell beside a gold answer keeps that answer
        gold = rel(["x", "l"], [uri("ex:a"), lit("A")], [uri("ex:b"), lit("")])
        g3, n = guarded.drop_empty(gold)
        self.assertEqual(n, 0)
        self.assertTrue(guarded.compare(g3, guarded.drop_empty(gold)[0])[0])
        self.assertFalse(guarded.compare(g3, rel(["x", "l"], [uri("ex:a"), lit("A")]))[0])

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
        # a mixed column is not a predicate column
        two = rel(["x"], [uri("ex:a")], [uri("ex:b")])
        mixed = rel(["x", "p"], [uri("ex:a"), uri("ex:p")], [uri("ex:b"), lit("note")])
        ok, d = guarded.compare(two, mixed)
        self.assertTrue(ok)
        self.assertEqual(guarded.flags(two, mixed, d, lambda iris: set(iris)), [])
        many = rel(["x", "p"], *[[uri("ex:a"), uri(f"ex:p{i}")] for i in range(201)])
        ok, d = guarded.compare(gold, many)
        self.assertIn("predicate_column_unknown", guarded.flags(gold, many, d, lambda iris: set(iris)))
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
        q = {"id": "q1", "gold": "SELECT ?x WHERE { { SELECT * WHERE { ?x <urn:p> ?y } } }"}
        self.assertIsNone(guarded.errata_exclusion(q, gold, {}))
        q = {"id": "q1", "gold": "ASK { { SELECT * WHERE { ?s <urn:p> ?o } } }"}
        self.assertIsNone(guarded.errata_exclusion(q, True, {}))
        q = {"id": "q1", "gold": "SELECT ?x ?s WHERE { ?x ?p ?o }"}
        unbound = rel(["x", "s"], [uri("ex:a"), None])
        self.assertEqual(guarded.errata_exclusion(q, unbound, {})["rule"], "unbound-column")
        sha = hashlib.sha256(q["gold"].encode()).hexdigest()
        named = {("q1", sha): {"reason": "r", "release": "2022.12.01"}}
        self.assertEqual(guarded.errata_exclusion(q, gold, named, "2022.12.01")["rule"], "named")
        # the same gold recomputed on another release is not excluded
        self.assertIsNone(guarded.errata_exclusion(q, gold, named, "2023.06.01"))
        self.assertIsNone(guarded.errata_exclusion(q, gold, named))
        # an entry for another gold text with the same id does not apply
        self.assertIsNone(guarded.errata_exclusion(dict(q, gold=q["gold"] + " "), gold, named))
        self.assertTrue(guarded.load_errata())
        # a prefix named like a query form does not hide the outer SELECT *
        star = {"id": "q2", "gold": "PREFIX ask: <urn:example:> BASE <urn:b> SELECT * WHERE { ?s ask:p ?o }"}
        self.assertEqual(guarded.errata_exclusion(star, gold, {})["rule"], "select-star")
        plain = {"id": "q3", "gold": "PREFIX select: <urn:x:> SELECT ?s WHERE { ?s select:p ?o }"}
        self.assertIsNone(guarded.errata_exclusion(plain, gold, {}))


class GroundingRecall(unittest.TestCase):
    def test_chat_completions_prompts_count(self):
        import json
        import grounding_recall

        qset = {"questions": [{"id": "a", "kind": "answerable", "question": "Q?",
                               "gold": "SELECT ?x WHERE { ?x <http://ex.org/p> ?y }"}]}
        with tempfile.TemporaryDirectory() as d:
            p = pathlib.Path(d)
            request = {"model": "m", "messages": [
                {"role": "system", "content": "You translate"},
                {"role": "user", "content": "<http://ex.org/p> Question: Q?"}]}
            reply = {"usage": {"prompt_tokens": 900, "completion_tokens": 5,
                               "prompt_tokens_details": {"cached_tokens": 800}}}
            (p / "replies.jsonl").write_text(json.dumps(
                {"case": "c1", "status": 200, "request": request, "reply": reply}) + "\n")
            (p / "s").mkdir()
            (p / "s" / "report.json").write_text(json.dumps(
                {"pairs": ["m"], "cases": [{"id": "a", "case": "c1", "pair": "m"}]}))
            row = grounding_recall.recall(qset, p)["a"]
            self.assertTrue(row["prompt"] and row["complete"])
            self.assertEqual(row["promptTokens"], 900)

    def test_a_schema_retry_counts_only_the_grounding_prompt(self):
        import json
        import grounding_recall

        qset = {"questions": [{"id": "a", "kind": "answerable", "question": "Q?",
                               "gold": "SELECT ?x WHERE { ?x <http://ex.org/p> ?y . ?y <http://ex.org/q> ?x }"}]}
        with tempfile.TemporaryDirectory() as d:
            p = pathlib.Path(d)
            # A retry after an answer that did not match the schema carries that answer
            # and a correction, and neither is grounding.
            request = {"system": "You translate", "messages": [
                {"role": "user", "content": "<http://ex.org/p> Question: Q?"},
                {"role": "assistant", "content": "{\"query\": \"SELECT * { ?x <http://ex.org/q> ?y }\""},
                {"role": "user", "content": "That answer did not match the schema: <http://ex.org/q>"}]}
            (p / "replies.jsonl").write_text(json.dumps(
                {"case": "c1", "status": 200, "request": request, "reply": {}}) + "\n")
            (p / "s").mkdir()
            (p / "s" / "report.json").write_text(json.dumps(
                {"pairs": ["m"], "cases": [{"id": "a", "case": "c1", "pair": "m"}]}))
            row = grounding_recall.recall(qset, p)["a"]
            self.assertTrue(row["prompt"])
            self.assertFalse(row["complete"])

    def test_one_report_of_one_pair_per_set(self):
        import json
        import grounding_recall

        qset = {"questions": [{"id": "a", "kind": "answerable", "question": "Q?",
                               "gold": "SELECT ?x WHERE { ?x <http://ex.org/p> <http://ex.org/resource/R> }"}]}
        with tempfile.TemporaryDirectory() as d:
            p = pathlib.Path(d)
            (p / "replies.jsonl").write_text(json.dumps({"case": "c1", "status": 200, "request": {
                "system": "You translate", "messages": [{"content": "<http://ex.org/p> Question: Q?"}]}, "reply": {}}) + "\n")
            (p / "s").mkdir()
            report = {"pairs": ["m"], "cases": [{"id": "a", "case": "c1", "pair": "m"}]}
            (p / "s" / "report.json").write_text(json.dumps(report))
            rows = grounding_recall.recall(qset, p, ["http://ex.org/resource/"])
            self.assertEqual(rows["a"]["missingSchema"], [])
            self.assertEqual(rows["a"]["missingResources"], ["http://ex.org/resource/R"])
            (p / "t").mkdir()
            (p / "t" / "report.json").write_text(json.dumps(report))
            with self.assertRaises(ValueError):
                grounding_recall.recall(qset, p)
            (p / "t" / "report.json").unlink()
            (p / "s" / "report.json").write_text(json.dumps(dict(report, pairs=["m", "n"])))
            with self.assertRaises(ValueError):
                grounding_recall.recall(qset, p)
            # a prompt whose response failed was still sent and still counts
            (p / "s" / "report.json").write_text(json.dumps(report))
            (p / "replies.jsonl").write_text(json.dumps({"case": "c1", "status": 529, "request": {
                "system": "You translate", "messages": [{"content": "<http://ex.org/p> Question: Q?"}]}}) + "\n")
            rows = grounding_recall.recall(qset, p)
            self.assertTrue(rows["a"]["prompt"])
            self.assertIsNone(rows["a"]["promptTokens"])
            self.assertEqual(grounding_recall.summary(rows)["promptTokensKnown"], 0)
            # a malformed 200 reply saved as text keeps its prompt
            (p / "replies.jsonl").write_text(json.dumps({"case": "c1", "status": 200, "reply": "not json", "request": {
                "system": "You translate", "messages": [{"content": "<http://ex.org/p> Question: Q?"}]}}) + "\n")
            self.assertTrue(grounding_recall.recall(qset, p)["a"]["prompt"])

    def test_resources_come_from_named_namespaces_only(self):
        import json
        import grounding_recall

        qset = {"questions": [{"id": "a", "kind": "answerable", "question": "Q?",
                               "gold": "SELECT ?x WHERE { <urn:person:Alice> <urn:vocab:knows> ?x }"}]}
        with tempfile.TemporaryDirectory() as d:
            p = pathlib.Path(d)
            (p / "replies.jsonl").write_text(json.dumps({"case": "c1", "status": 200, "request": {
                "system": "You translate", "messages": [{"content": "<urn:vocab:knows> Question: Q?"}]}}) + "\n")
            (p / "s").mkdir()
            (p / "s" / "report.json").write_text(json.dumps({"pairs": ["m"], "cases": [{"id": "a", "case": "c1"}]}))
            rows = grounding_recall.recall(qset, p, ["urn:person:"])
            self.assertEqual(rows["a"]["missingResources"], ["urn:person:Alice"])
            self.assertFalse(rows["a"]["resourcesComplete"])
            rows = grounding_recall.recall(qset, p)
            self.assertIsNone(rows["a"]["resourcesComplete"])
            self.assertFalse(rows["a"]["complete"])
            self.assertIsNone(grounding_recall.summary(rows)["resourcesComplete"])

    def test_a_term_matches_only_a_whole_name(self):
        import grounding_recall

        present = grounding_recall.present
        pre = {"ex": "http://example.org/"}
        self.assertFalse(present("http://example.org/p", "<http://example.org/p/qualifier>", pre))
        self.assertFalse(present("http://example.org/p", "http://example.org/p/qualifier", pre))
        self.assertFalse(present("http://example.org/p", "ex:p.suffix", pre))
        self.assertFalse(present("http://example.org/p", "ex:p/q", pre))
        self.assertTrue(present("http://example.org/p", "Use ex:p.", pre))
        self.assertTrue(present("http://example.org/p", "ex:p, ex:q", pre))
        self.assertTrue(present("http://example.org/p", "<http://example.org/p>", pre))
        self.assertTrue(present("http://example.org/p", "see http://example.org/p.", pre))
        self.assertTrue(present("http://example.org/p", "ex:p: a predicate", pre))
        dbr = {"dbr": "http://dbpedia.org/resource/"}
        self.assertFalse(present("http://dbpedia.org/resource/Star_Trek", "dbr:Star_Trek:_The_Next_Generation", dbr))
        self.assertFalse(present("http://dbpedia.org/resource/Star_Trek", "http://dbpedia.org/resource/Star_Trek:_TNG", dbr))
        # the empty prefix and dotted prefix names
        text = ": <http://example.org/>\na.b: <http://example.org/b/>\nUse :p and a.b:q.\n: Thing 3, Other 1"
        pre = grounding_recall.prompt_prefixes(text)
        self.assertEqual(pre, {"": "http://example.org/", "a.b": "http://example.org/b/"})
        self.assertTrue(present("http://example.org/p", text, pre))
        self.assertTrue(present("http://example.org/b/q", text, pre))
        self.assertIn("http://example.org/Thing", grounding_recall.grouped_terms(text))
        self.assertFalse(present("http://example.org/q", text, pre))

    def test_a_text_mode_first_draft_is_not_a_repair(self):
        import json
        import grounding_recall

        with tempfile.TemporaryDirectory() as d:
            p = pathlib.Path(d) / "replies.jsonl"
            first = "Question: Q?\nAnswer with one fenced ```sparql block."
            repair = "Question: Q?\nYour previous query was:\n```sparql\nASK {}\n```\nIt failed."
            p.write_text("".join(json.dumps({"case": "c", "status": 200, "request": {
                "system": "You translate", "messages": [{"content": t}]}}) + "\n" for t in (first, repair)))
            got = grounding_recall.drafts(p)["c"]
            self.assertEqual(len(got), 1)
            self.assertIn("one fenced", got[0][0])


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
