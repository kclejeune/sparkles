"""The LC-QuAD 2.0 converter on a small fixture and a fake query binary."""

import importlib.machinery
import importlib.util
import json
import pathlib
import sys
import tempfile
import types
import unittest

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE / "eval_support"))
from evaluator import Invalid

loader = importlib.machinery.SourceFileLoader(
    "lcquad2_under_test", str(HERE.parents[1] / "scripts/lcquad2-convert")
)
spec = importlib.util.spec_from_loader(loader.name, loader)
lcq = importlib.util.module_from_spec(spec)
loader.exec_module(lcq)
PYTHON = str(pathlib.Path(sys.base_prefix) / "bin/python3")


def entry(uid, question, paraphrase, sparql):
    return {
        "uid": uid,
        "question": question,
        "paraphrased_question": paraphrase,
        "sparql_wikidata": sparql,
        "template_id": 1,
        "answer": [],
    }


FIXTURE = [
    entry(
        1,
        "What is the capital of France?",
        [],
        " select ?o where { wd:Q142 wdt:P36 ?o } ",
    ),
    entry(
        2,
        "Who is the {country} of {X}",
        "Which country is X in?",
        "select ?s where { ?s wdt:P17 wd:Q1 }",
    ),
    entry(3, "n/a", [], "ASK { wd:Q1 wdt:P31 wd:Q5 }"),
    entry(4, None, "gsdfgh", "ASK { wd:Q1 wdt:P31 wd:Q5 }"),
    entry(
        5,
        "When did the statement start to hold?",
        [],
        "select ?q where { wd:Q1 p:P39 ?s . ?s ps:P39 wd:Q2 . ?s pq:P580 ?q }",
    ),
    entry(6, "Who is not there at all?", [], "select ?s where { ?s wdt:P1 wd:none }"),
]
FAKE = """
import json, sys
query = sys.argv[-1]
assert "PREFIX wd: <http://www.wikidata.org/entity/>" in query
rows = [] if "wd:none" in query else [{"s": {"type": "uri", "value": "urn:x"}}]
print(json.dumps({"head": {"vars": ["s"]}, "results": {"bindings": rows}}))
"""


class Convert(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="lcq-")
        self.addCleanup(self.tmp.cleanup)
        self.root = pathlib.Path(self.tmp.name)
        self.source = self.root / "test.json"
        self.source.write_text(json.dumps(FIXTURE))
        self.binary = self.root / "sparkles"
        self.binary.write_text("#!" + PYTHON + "\n" + FAKE)
        self.binary.chmod(0o700)

    def args(self, **kw):
        base = dict(
            sample=None, seed="s", loc=None, sparkles=None, manifest=None, timeout=5
        )
        base.update(kw)
        return types.SimpleNamespace(**base)

    def test_text_choice_and_statement_flag(self):
        out = lcq.convert(self.args(), self.source)
        ids = [q["id"] for q in out["questions"]]
        self.assertEqual(ids, ["lcquad2-1", "lcquad2-2", "lcquad2-5", "lcquad2-6"])
        self.assertEqual(out["questions"][1]["question"], "Which country is X in?")
        self.assertEqual(out["textFields"], {"question": 3, "paraphrased_question": 1})
        self.assertEqual(out["dropped"]["noText"]["ids"], ["lcquad2-3", "lcquad2-4"])
        self.assertTrue(out["questions"][2]["statements"])
        self.assertFalse(out["questions"][0]["statements"])
        self.assertEqual(
            out["questions"][0]["gold"], "select ?o where { wd:Q142 wdt:P36 ?o }"
        )
        self.assertIn("pq", out["prefixes"])

    def test_the_sample_is_fixed_by_seed(self):
        a = lcq.sample(FIXTURE, 3, "s")
        self.assertEqual(a, lcq.sample(list(reversed(FIXTURE)), 3, "s"))
        self.assertEqual(len(a), 3)
        self.assertNotEqual(
            [e["uid"] for e in lcq.sample(FIXTURE, 6, "s")],
            [e["uid"] for e in lcq.sample(FIXTURE, 6, "t")],
        )

    def test_recompute_prefixes_the_gold_and_drops_empty(self):
        args = self.args(loc=str(self.root), sparkles=str(self.binary))
        out = lcq.convert(args, self.source)
        self.assertEqual(out["dropped"]["empty"]["ids"], ["lcquad2-6"])
        self.assertTrue(out["goldRecomputed"])

    def test_a_sample_accounts_for_every_question_it_took(self):
        out = lcq.convert(self.args(sample=4), self.source)
        self.assertEqual((out["sourceQuestions"], out["sampledQuestions"]), (6, 4))
        dropped = sum(v["count"] for v in out["dropped"].values())
        self.assertEqual(len(out["questions"]) + dropped, 4)

    def test_a_missing_or_repeated_uid_is_refused(self):
        no_uid = {k: v for k, v in FIXTURE[0].items() if k != "uid"}
        for broken in (
            [*FIXTURE, dict(FIXTURE[0])],
            [*FIXTURE, no_uid],
            [*FIXTURE, dict(FIXTURE[0], uid=None)],
        ):
            with self.subTest(last=broken[-1].get("uid", "missing")):
                self.source.write_text(json.dumps(broken))
                with self.assertRaises(Invalid):
                    lcq.convert(self.args(), self.source)

    def test_whitespace_is_collapsed_only_outside_literals(self):
        self.assertEqual(
            lcq.compact(" select ?o\n where { ?o rdfs:label 'two  spaces'@en } "),
            "select ?o where { ?o rdfs:label 'two  spaces'@en }",
        )
        self.assertEqual(
            lcq.compact('ASK  { ?s ?p "a\\"  b" }'), 'ASK { ?s ?p "a\\"  b" }'
        )
        with_comment = "select ?o # the object\nwhere { ?s ?p ?o }"
        self.assertEqual(lcq.compact(with_comment + "\n"), with_comment)

    def test_a_split_with_another_hash_is_refused(self):
        sys.argv = [
            "lcquad2-convert",
            "--input",
            str(self.source),
            "--out",
            str(self.root / "o.json"),
        ]
        with self.assertRaises(Invalid):
            lcq.main()


if __name__ == "__main__":
    unittest.main()
