"""The QALD-9-plus converter on a three-question fixture and a fake query binary."""

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
    "qald_under_test", str(HERE.parents[1] / "scripts/qald9plus-convert")
)
spec = importlib.util.spec_from_loader(loader.name, loader)
qald = importlib.util.module_from_spec(spec)
loader.exec_module(qald)
PYTHON = str(pathlib.Path(sys.base_prefix) / "bin/python3")


def entry(ident, text, sparql, language="en"):
    return {
        "id": ident,
        "question": [
            {"language": "de", "string": "Frage"},
            {"language": language, "string": text},
        ],
        "query": {"sparql": sparql},
        "answers": [],
    }


FIXTURE = {
    "questions": [
        entry(
            "1",
            "What is the time zone of Salt Lake City?",
            "SELECT ?u WHERE { <urn:a> <urn:p> ?u }",
        ),
        entry("2", "Who is nobody?", "SELECT ?u WHERE { <urn:none> <urn:p> ?u }"),
        entry("3", "Is it broken?", "ASK { <urn:fail> <urn:p> ?u }"),
        entry("4", "A blank node.", "SELECT ?u WHERE { <urn:blank> <urn:p> ?u }"),
        entry("5", "Remote?", "SELECT ?u WHERE { SERVICE <urn:e> { ?u ?p ?o } }"),
        entry(
            "6", "How many?", "SELECT (COUNT(?u) AS ?n) WHERE { <urn:a> <urn:p> ?u }"
        ),
        entry("7", "Keine englische Frage", "ASK {}", language="de"),
    ]
}

FAKE = """
import json, sys
query = sys.argv[-1]
def rows(*r): print(json.dumps({"head": {"vars": ["u"]}, "results": {"bindings": list(r)}}))
if "urn:fail" in query: raise SystemExit(3)
if "urn:none" in query: rows()
elif "urn:blank" in query: rows({"u": {"type": "bnode", "value": "b"}})
else: rows({"u": {"type": "uri", "value": "urn:x"}})
"""


class Convert(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="qald-")
        self.addCleanup(self.tmp.cleanup)
        self.root = pathlib.Path(self.tmp.name)
        self.source = self.root / "split.json"
        self.source.write_text(json.dumps(FIXTURE))
        self.binary = self.root / "sparkles"
        self.binary.write_text("#!" + PYTHON + "\n" + FAKE)
        self.binary.chmod(0o700)

    def args(self, **kw):
        return types.SimpleNamespace(
            loc=None, sparkles=str(self.binary), release="2022.12.01", timeout=5, **kw
        )

    def test_conversion_without_a_database_keeps_every_english_question(self):
        out = qald.convert(self.args(), self.source)
        self.assertEqual(out["sourceQuestions"], 7)
        self.assertEqual(
            [q["id"] for q in out["questions"]], [f"qald9p-{i}" for i in range(1, 7)]
        )
        self.assertFalse(out["goldRecomputed"])
        self.assertEqual(out["dropped"]["noEnglish"]["ids"], ["qald9p-7"])
        first = out["questions"][0]
        self.assertEqual(first["kind"], "answerable")
        self.assertEqual(first["question"], "What is the time zone of Salt Lake City?")
        self.assertEqual(out["source"]["sha256"], qald.SHA256)
        self.assertEqual(out["source"]["license"], "CC BY 4.0")

    def test_recompute_drops_and_counts(self):
        args = self.args()
        args.loc = str(self.root)
        out = qald.convert(args, self.source)
        self.assertTrue(out["goldRecomputed"])
        self.assertEqual(out["dbpediaRelease"], "2022.12.01")
        self.assertEqual([q["id"] for q in out["questions"]], ["qald9p-1", "qald9p-6"])
        dropped = {k: v["ids"] for k, v in out["dropped"].items()}
        self.assertEqual(dropped["empty"], ["qald9p-2"])
        self.assertEqual(dropped["failed"], ["qald9p-3"])
        self.assertEqual(dropped["unsupported"], ["qald9p-4", "qald9p-5"])
        self.assertEqual(dropped["noEnglish"], ["qald9p-7"])
        self.assertEqual(len(out["goldBinarySha256"]), 64)

    def test_categories(self):
        cases = {
            "ASK { ?s ?p ?o }": "ask",
            "SELECT (COUNT(?s) AS ?n) WHERE { ?s ?p ?o }": "aggregate",
            "SELECT ?s WHERE { ?s ?p ?o } ORDER BY ?s LIMIT 1": "ordering",
            "SELECT ?s WHERE { ?s ?p ?o FILTER NOT EXISTS { ?s a ?c } }": "negation",
            "SELECT ?s WHERE { ?s ?p ?o }": "lookup",
            "SELECT ?s WHERE { ?s ?p ?o . ?o ?q ?r }": "join",
        }
        for query, label in cases.items():
            with self.subTest(query=query):
                self.assertEqual(qald.category(query), label)

    def test_a_split_with_another_hash_is_refused(self):
        sys.argv = [
            "qald9plus-convert",
            "--input",
            str(self.source),
            "--out",
            str(self.root / "set.json"),
        ]
        with self.assertRaises(Invalid):
            qald.main()
        self.assertFalse((self.root / "set.json").exists())


if __name__ == "__main__":
    unittest.main()
