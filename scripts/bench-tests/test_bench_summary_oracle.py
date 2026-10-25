"""A summary must rank accepted oracle answers and refuse stale verification receipts."""
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).parents[1] / "bench-summary.py"


class SummaryOracleTests(unittest.TestCase):
    def run_summary(self, validation=True, stale=False):
        name = "bi-q7-1"
        fingerprint = {"rows": 1, "exact": "different", "value": "different", "order": None,
                       "query_sha256": "query"}
        answers = {name: {"sparkles": {**fingerprint, "exact": "same", "value": "same"},
                          "jena-fuseki": {**fingerprint, "exact": "same", "value": "same"},
                          "fluree": fingerprint}}
        receipt = {"version": 1, "source_sha256": "source", "queries": {name: {"fluree": {
            "status": "ok", "policy": "legal-inner-limit-ties", "query_sha256": "stale" if stale else "query",
            "fingerprint": fingerprint}}}}
        with tempfile.TemporaryDirectory() as d:
            p = Path(d)
            (p / "answers.json").write_text(json.dumps(answers))
            (p / "manifest.json").write_text(json.dumps({"data": {"data": {"sha256": "source"}}}))
            (p / (name + ".json")).write_text(json.dumps({"results": [
                {"command": engine, "mean": time, "stddev": 0, "times": [time], "exit_codes": [0]}
                for engine, time in [("sparkles", .02), ("jena-fuseki", .03), ("fluree", .01)]]}))
            if validation:
                (p / "answer-validation.json").write_text(json.dumps(receipt))
            run = subprocess.run(["python3", str(SCRIPT), d, name], text=True, capture_output=True)
            summary = (p / "summary.md").read_text() if (p / "summary.md").exists() else ""
            return run, summary

    def test_legal_tied_answer_is_ranked_despite_different_fingerprint(self):
        run, summary = self.run_summary()
        self.assertEqual(run.returncode, 0, run.stderr)
        self.assertIn("**10.0 ± 0.0**", summary)
        self.assertIn("fluree passed the independent data oracle", summary)
        self.assertNotIn(" †", summary)

    def test_unverified_majority_mismatch_remains_excluded(self):
        run, summary = self.run_summary(validation=False)
        self.assertEqual(run.returncode, 0, run.stderr)
        self.assertIn("**20.0 ± 0.0**", summary)
        self.assertIn("10.0 ± 0.0 †", summary)

    def test_stale_verdict_stops_summary(self):
        run, _ = self.run_summary(stale=True)
        self.assertNotEqual(run.returncode, 0)
        self.assertIn("stale or invalid", run.stderr)
