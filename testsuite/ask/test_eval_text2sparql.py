"""The Text2SPARQL'25 converter on small fixtures and a fake query binary."""

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
import convert as cv
from evaluator import Invalid, answer_set

loader = importlib.machinery.SourceFileLoader(
    "t2s_under_test", str(HERE.parents[1] / "scripts/text2sparql-convert")
)
spec = importlib.util.spec_from_loader(loader.name, loader)
t2s = importlib.util.module_from_spec(spec)
loader.exec_module(t2s)
PYTHON = str(pathlib.Path(sys.base_prefix) / "bin/python3")

# The quirks of the real files: texts at 6 or 8 spaces, a text over two lines, a
# query as a block or as a double-quoted scalar, and a feature list.
YAML = """# yamllint disable rule:line-length
---
dataset:
  id: https://text2sparql.aksw.org/2025/dbpedia/
  prefix: db25
questions:

  - id: 1
    question:
      en: Who is in it?
      es: ¿Quién
        está?
    query:
      sparql: |
        SELECT ?u WHERE {
          <urn:a> <urn:p> ?u
        }

  - id: 2
    question:
        en: Is it true?
        es: ¿Es verdad?
    query:
      sparql: "ASK WHERE { <urn:a> <urn:p> \\"x\\" }"

  - id: 3
    question:
      en: Who is nobody?
    features:
      - SELECT
      - RESULT_ORDER_MATTERS
    query:
      sparql: |
        SELECT ?u WHERE { <urn:none> <urn:p> ?u } ORDER BY ?u

  - id: 4
    question:
      en: Which changed?
    query:
      sparql: |
        SELECT ?u WHERE { <urn:b> <urn:p> ?u }
"""
ANSWERS = {
    "db25:1-en": {"urn:x": 1},
    "db25:2-en": {"true": 0},
    "db25:4-en": {"urn:old": 1},
}
FAKE = """
import json, sys
query = sys.argv[-1]
if query.startswith("ASK"):
    print(json.dumps({"head": {}, "boolean": True}))
    raise SystemExit
rows = [] if "urn:none" in query else [{"u": {"type": "uri", "value": "urn:x"}}]
print(json.dumps({"head": {"vars": ["u"]}, "results": {"bindings": rows}}))
"""


class Parse(unittest.TestCase):
    def test_the_quirks_of_the_real_files_are_read(self):
        items = t2s.parse_questions(YAML)
        self.assertEqual([i["id"] for i in items], ["1", "2", "3", "4"])
        self.assertEqual(
            items[0]["question"], {"en": "Who is in it?", "es": "¿Quién está?"}
        )
        self.assertEqual(
            items[0]["sparql"], "SELECT ?u WHERE {\n  <urn:a> <urn:p> ?u\n}\n"
        )
        self.assertEqual(items[1]["question"]["en"], "Is it true?")
        self.assertEqual(items[1]["sparql"], 'ASK WHERE { <urn:a> <urn:p> "x" }')
        self.assertEqual(items[2]["features"], ["SELECT", "RESULT_ORDER_MATTERS"])

    def test_anything_else_is_refused(self):
        for text in (
            YAML + "other:\n",
            YAML.replace(
                "    query:\n      sparql: |\n        SELECT ?u WHERE { <urn:b>",
                "    query:\n      text: |\n        SELECT ?u WHERE { <urn:b>",
            ),
            YAML.replace("  - id: 4\n    question:", "  - id: 4\n    unknown:"),
            YAML.replace("      en: Which changed?", "      en: [Which changed?]"),
        ):
            with self.subTest(text=text[-80:]), self.assertRaises(Invalid):
                t2s.parse_questions(text)

    def test_published_answers(self):
        self.assertIs(t2s.published({"k": {"true": 1}}, "k"), True)
        self.assertIs(t2s.published({"k": {"true": 0}}, "k"), False)
        self.assertEqual(t2s.published({"k": {"a": 1, "b": 2}}, "k"), {"a", "b"})
        self.assertEqual(t2s.published({"k": {}}, "k"), set())
        self.assertIsNone(t2s.published({}, "k"))


class Convert(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="t2s-")
        self.addCleanup(self.tmp.cleanup)
        self.root = pathlib.Path(self.tmp.name)
        (self.root / "q.yaml").write_text(YAML)
        (self.root / "a.json").write_text(json.dumps(ANSWERS))
        self.binary = self.root / "sparkles"
        self.binary.write_text("#!" + PYTHON + "\n" + FAKE)
        self.binary.chmod(0o700)
        self.files = {
            "questions": self.root / "q.yaml",
            "answers": self.root / "a.json",
        }
        self.old = dict(t2s.SETS["db25"])
        t2s.SETS["db25"]["questions"] = 4
        self.addCleanup(t2s.SETS["db25"].update, self.old)

    def args(self, **kw):
        base = dict(
            set="db25",
            language="en",
            loc=None,
            sparkles=None,
            release="2022.12.01",
            manifest=None,
            timeout=5,
        )
        base.update(kw)
        return types.SimpleNamespace(**base)

    def test_without_a_database_every_question_is_kept(self):
        out = t2s.convert(self.args(), self.files, self.root)
        self.assertEqual(
            [q["id"] for q in out["questions"]], [f"db25-{i}" for i in range(1, 5)]
        )
        self.assertEqual(out["questions"][1]["category"], "ask")
        self.assertTrue(out["questions"][2]["orderMatters"])
        self.assertFalse(out["goldRecomputed"])
        self.assertIsNone(out["publishedComparison"])
        self.assertEqual(out["license"], "CC BY-SA 4.0")
        self.assertEqual(
            out["source"]["questions"]["sha256"], t2s.FILES["db25-questions"][3]
        )

    def test_recompute_drops_and_compares(self):
        manifest = self.root / "manifest.json"
        manifest.write_text('{"release": "2022.12.01"}')
        args = self.args(
            loc=str(self.root), sparkles=str(self.binary), manifest=str(manifest)
        )
        out = t2s.convert(args, self.files, self.root)
        self.assertEqual(
            [q["id"] for q in out["questions"]], ["db25-1", "db25-2", "db25-4"]
        )
        self.assertEqual(out["dropped"]["empty"]["ids"], ["db25-3"])
        compared = out["publishedComparison"]
        self.assertEqual(compared["counts"], {"equal": 1, "differs": 2})
        self.assertEqual(compared["differs"], ["db25-2", "db25-4"])
        self.assertEqual(
            compared["byId"]["db25-2"], {"status": "differs", "kind": "ask"}
        )
        self.assertEqual(out["goldDatabase"]["manifest"], {"release": "2022.12.01"})
        self.assertEqual(out["dbpediaRelease"], "2022.12.01")

    def test_a_file_with_another_hash_is_refused(self):
        sys.argv = [
            "text2sparql-convert",
            "--set",
            "db25",
            "--input",
            str(self.root),
            "--out",
            str(self.root / "set.json"),
        ]
        (self.root / "questions_db25.yaml").write_text(YAML)
        with self.assertRaises(Invalid):
            t2s.main()
        self.assertFalse((self.root / "set.json").exists())


class Normalize(unittest.TestCase):
    """The Virtuoso habits of the QALD-9-plus and db25 gold queries that failed."""

    def test_projections(self):
        cases = {
            "SELECT DISTINCT COUNT(?uri) WHERE { ?uri a ?c }": (
                "SELECT DISTINCT (COUNT(?uri) AS ?count) WHERE { ?uri a ?c }"
            ),
            "SELECT Count(?sub) as ?c WHERE { ?sub ?p ?o }": (
                "SELECT (Count(?sub) AS ?c) WHERE { ?sub ?p ?o }"
            ),
            "SELECT COUNT(DISTINCT ?y AS ?y) WHERE { ?x ?p ?y . }": (
                "SELECT (COUNT(DISTINCT ?y) AS ?count) WHERE { ?x ?p ?y . }"
            ),
            "SELECT DISTINCT COUNT(?a) { ?a ?b ?c }": (
                "SELECT DISTINCT (COUNT(?a) AS ?count) { ?a ?b ?c }"
            ),
        }
        for query, want in cases.items():
            with self.subTest(query=query):
                self.assertEqual(cv.normalize_dbpedia(query), (want, ["projection"]))

    def test_function_projection_and_prefixes(self):
        query = (
            "PREFIX res: <http://dbpedia.org/resource/> "
            "SELECT DISTINCT xsd:date(?date) WHERE { res:X dbo:deathDate ?date }"
        )
        got, changes = cv.normalize_dbpedia(query)
        self.assertEqual(changes, ["projection", "prefixes:dbo,xsd"])
        self.assertTrue(got.startswith("PREFIX dbo: <http://dbpedia.org/ontology/>\n"))
        self.assertIn("SELECT DISTINCT (xsd:date(?date) AS ?value) WHERE", got)

    def test_standard_queries_are_left_alone(self):
        for query in (
            "PREFIX dbo: <http://dbpedia.org/ontology/> SELECT ?x WHERE { ?x dbo:p ?y }",
            "SELECT (COUNT(?x) AS ?n) WHERE { ?x <http://a.b/c:d> 'e:f' }",
            "SELECT ?x WHERE { ?x <urn:p> ?y FILTER(bif:contains(?y, 'a')) }",
            "ASK { <http://dbpedia.org/resource/A> ?p ?o }",
        ):
            with self.subTest(query=query):
                self.assertEqual(cv.normalize_dbpedia(query), (query, []))


class Compare(unittest.TestCase):
    def rel(self, *values):
        return answer_set(
            {
                "head": {"vars": ["u"]},
                "results": {
                    "bindings": [{"u": {"type": "literal", "value": v}} for v in values]
                },
            }
        )

    def test_compare_by_value(self):
        self.assertEqual(cv.compare(None, self.rel("a"))["status"], "unpublished")
        self.assertEqual(cv.compare({"a"}, self.rel("a"))["status"], "equal")
        self.assertEqual(
            cv.compare(self.rel("a", "b"), self.rel("b", "c")),
            {"status": "differs", "published": 2, "recomputed": 2, "common": 1},
        )
        self.assertEqual(cv.compare(True, True), {"status": "equal", "kind": "ask"})
        self.assertEqual(cv.compare({"a"}, True)["status"], "differs")
        self.assertEqual(
            cv.summarize({"x": {"status": "equal"}, "y": {"status": "differs"}}),
            {"counts": {"equal": 1, "differs": 1}, "differs": ["y"]},
        )


if __name__ == "__main__":
    unittest.main()
