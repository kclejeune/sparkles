"""scripts/eval-ask-rescore on a saved run, with a fake query binary."""

import hashlib
import importlib.machinery
import importlib.util
import json
import pathlib
import sys
import tempfile
import unittest

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE / "eval_support"))
from evaluator import POLICY, Invalid

loader = importlib.machinery.SourceFileLoader(
    "rescore_under_test", str(HERE.parents[1] / "scripts/eval-ask-rescore")
)
spec = importlib.util.spec_from_loader(loader.name, loader)
rescore = importlib.util.module_from_spec(spec)
loader.exec_module(rescore)
PYTHON = str(pathlib.Path(sys.base_prefix) / "bin/python3")

# The gold returns two people. The saved answer returns them with a name beside each,
# which the first policy scored as wrong because the widths differ.
FAKE = """
import json, sys
q = sys.argv[-1]
people = ["urn:a", "urn:b"]
if "GOLD" in q:
    rows = [{"p": {"type": "uri", "value": p}} for p in people]
    names = ["p"]
else:
    rows = [{"p": {"type": "uri", "value": p}, "n": {"type": "literal", "value": p[-1]}}
            for p in people]
    names = ["p", "n"]
print(json.dumps({"head": {"vars": names}, "results": {"bindings": rows}}))
"""


def sha(path):
    return hashlib.sha256(pathlib.Path(path).read_bytes()).hexdigest()


class Rescore(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="ask-rescore-")
        self.addCleanup(self.tmp.cleanup)
        root = self.root = pathlib.Path(self.tmp.name)
        self.binary = root / "sparkles"
        self.binary.write_text("#!" + PYTHON + "\n" + FAKE)
        self.binary.chmod(0o700)
        (root / "d.ttl").write_text("")
        self.set = root / "set.json"
        self.set.write_text(
            json.dumps(
                {
                    "data": "d.ttl",
                    "questions": [
                        {
                            "id": "q1",
                            "kind": "answerable",
                            "category": "join",
                            "question": "Who?",
                            "gold": "SELECT ?p WHERE { GOLD }",
                        }
                    ],
                }
            )
        )
        run = root / "run" / "out"
        (run / "set1").mkdir(parents=True)
        (run / "models.json").write_text(
            json.dumps({"models": {"providers": {"p": {"models": {"m": {}}}}}})
        )
        answer = {
            "outcome": "answered",
            "result": {"query": "SELECT ?p ?n WHERE { WIDE }", "attempt": 1},
            "attempts": [
                {"query": "SELECT ?p ?n WHERE { WIDE }", "check": {"ok": True}}
            ],
            "usage": {"inputTokens": 10, "outputTokens": 5},
        }
        old = [False, [0.0, 0.0, 0.0], {"kind": "select", "mapping": None}]
        (run / "set1" / "c1.json").write_text(
            json.dumps({"id": "q1", "answer": answer, "score": old})
        )
        self.report = run / "set1" / "report.json"
        self.report.write_text(
            json.dumps(
                {
                    "schema": 3,
                    "scoringPolicy": "rdf-term-set-global-columns-v1",
                    "set": str(self.set),
                    "setSha256": sha(self.set),
                    "binarySha256": sha(self.binary),
                    "dataLoc": None,
                    "pairs": ["p/m"],
                    "notRun": [],
                    "cases": [
                        {
                            "case": "c1",
                            "pair": "p/m",
                            "id": "q1",
                            "correct": False,
                            "prf": [0.0, 0.0, 0.0],
                            "detail": old[2],
                        }
                    ],
                    "rows": [
                        {
                            "pair": "p/m",
                            "executionAccuracy": {
                                "numerator": 0,
                                "denominator": 1,
                                "value": 0.0,
                            },
                        }
                    ],
                }
            )
        )

    def test_extra_columns_are_rescored_from_the_saved_answers(self):
        out = self.root / "rescored"
        report = rescore.rescore(self.report, self.binary, out)
        self.assertEqual(report["scoringPolicy"], POLICY)
        self.assertTrue(report["cases"][0]["correct"])
        self.assertEqual(report["cases"][0]["before"]["correct"], False)
        self.assertEqual(report["cases"][0]["detail"]["extraColumns"], 1)
        [row] = report["executionAccuracyBeforeAndAfter"]
        self.assertEqual(row["before"]["numerator"], 0)
        self.assertEqual(row["after"]["numerator"], 1)
        self.assertEqual(report["rescoredFrom"]["reportSha256"], sha(self.report))
        self.assertTrue((out / "report.json").is_file())

    def test_a_changed_set_or_binary_is_refused(self):
        self.set.write_text(self.set.read_text() + " ")
        with self.assertRaises(Invalid):
            rescore.rescore(self.report, self.binary, self.root / "a")


if __name__ == "__main__":
    unittest.main()
