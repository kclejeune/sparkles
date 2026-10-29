"""A faster unqualified result must never become a ranked binding result."""

import copy
import importlib.util
from pathlib import Path
import json
import subprocess
import sys
import tempfile
import unittest

PATH = Path(__file__).resolve().parents[1] / "bench-bindings" / "scorecard.py"
SPEC = importlib.util.spec_from_file_location("binding_scorecard", PATH)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def fixture():
    answer = {
        "rows": 2, "hash": "a" * 16, "algorithm": "canonical-sha256-v1", "agrees": True,
        "verification": {"method": "data-oracle", "policy": "fixed-exact-terms",
                         "source_sha256": "b" * 64, "query_sha256": "c" * 64},
    }
    return {"record": 3, "suite": "bindings", "scale": "tiny",
            "settings": {"api_layer": "rust-native", "consumption": "all-terms",
                         "warmup": 1, "runs": 3, "timer": "wall", "resultCache": False},
            "manifest": {},
            "results": [{"engine": name, "query": "lookup", "mode": "warm", "clients": 1,
                         "durability": "memory", "status": "ok", "samples_ms": samples,
                         "answer": copy.deepcopy(answer)}
                        for name, samples in (("sparkles", [2, 3, 4]), ("peer", [5, 6, 7]))]}


class ScorecardTests(unittest.TestCase):
    def cases(self, value):
        return MODULE.build_scorecard(value, "sparkles", "peer")["cases"]

    def cli_case(self, value):
        with tempfile.TemporaryDirectory() as directory:
            record = Path(directory) / "record.json"
            record.write_text(json.dumps(value))
            result = subprocess.run([sys.executable, str(PATH), str(record),
                                     "--candidate", "sparkles", "--peer", "peer"],
                                    capture_output=True, text=True, timeout=15)
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)["cases"][0]

    def test_cli_rejects_matching_malformed_complete_fingerprints(self):
        for digest in ("done", "a" * 64, "A" * 16, "row-sha256-v1:abc"):
            with self.subTest(digest=digest):
                value = fixture()
                for row in value["results"]:
                    row["answer"]["hash"] = digest
                    row["answer"].pop("algorithm")
                case = self.cli_case(value)
                self.assertFalse(case["comparable"])
                self.assertIsNone(case["candidate_over_peer"])
                self.assertEqual(case["observations"]["sparkles"]["answer"]["hash"], digest)

    def test_cli_rejects_invalid_equal_cardinalities(self):
        for count in (-1, True, None, 1.5):
            with self.subTest(count=count):
                value = fixture()
                for row in value["results"]:
                    row["answer"]["rows"] = count
                case = self.cli_case(value)
                self.assertFalse(case["comparable"])
                self.assertIsNone(case["candidate_over_peer"])

    def test_cli_valid_empty_answers_and_both_digest_families(self):
        for algorithm, digest in (("canonical-sha256-v1", "0" * 16),
                                  ("row-sha256-v1", "row-sha256-v1:" + "0" * 64)):
            with self.subTest(algorithm=algorithm):
                value = fixture()
                for row in value["results"]:
                    row["answer"].update(rows=0, algorithm=algorithm, hash=digest)
                case = self.cli_case(value)
                self.assertTrue(case["comparable"])
                self.assertEqual(case["candidate_over_peer"], 0.5)

    def test_equal_semantics_use_raw_sample_medians(self):
        value = fixture()
        value["results"][0]["median_ms"] = 0.00001
        case = self.cases(value)[0]
        self.assertTrue(case["comparable"])
        self.assertEqual(case["candidate_over_peer"], 0.5)

    def test_fast_wrong_answer_is_not_a_win(self):
        value = fixture()
        value["results"][0].update(status="wrong-answer", samples_ms=[0.001])
        case = self.cases(value)[0]
        self.assertFalse(case["comparable"])
        self.assertIsNone(case["candidate_over_peer"])

    def test_full_consumption_and_api_layer_are_required(self):
        for field in ("consumption", "api_layer"):
            value = fixture()
            del value["settings"][field]
            self.assertFalse(self.cases(value)[0]["comparable"])
        value = fixture()
        value["results"][1]["api_layer"] = "rust-http"
        self.assertFalse(self.cases(value)[0]["comparable"])

    def test_majority_without_independent_oracle_is_not_enough(self):
        value = fixture()
        del value["results"][0]["answer"]["verification"]
        self.assertFalse(self.cases(value)[0]["comparable"])

    def test_unlike_fingerprints_or_evidence_cannot_pair(self):
        for field, replacement in (("hash", "d" * 64), ("rows", 3), ("algorithm", "row-count-v1")):
            value = fixture()
            value["results"][0]["answer"][field] = replacement
            self.assertFalse(self.cases(value)[0]["comparable"])
        for field in ("source_sha256", "query_sha256", "policy"):
            value = fixture()
            value["results"][0]["answer"]["verification"][field] = "d" * 64
            self.assertFalse(self.cases(value)[0]["comparable"])

    def test_missing_peer_timeout_and_nonfinite_samples_remain_visible(self):
        value = fixture()
        value["results"].pop()
        self.assertIn("peer:missing", self.cases(value)[0]["reasons"])
        for sample in (0, -1, float("nan"), float("inf"), True, 10 ** 1000):
            value = fixture()
            value["results"][0]["samples_ms"] = [sample]
            self.assertFalse(self.cases(value)[0]["comparable"])
        value = fixture()
        value["results"][1]["status"] = "timeout"
        self.assertFalse(self.cases(value)[0]["comparable"])

    def test_modes_and_durability_are_separate_cases(self):
        for field, replacement in (("mode", "cold"), ("durability", "synced")):
            value = fixture()
            value["results"][0][field] = replacement
            cases = self.cases(value)
            self.assertEqual(len(cases), 2)
            self.assertTrue(all(not case["comparable"] for case in cases))

    def test_duplicate_rows_and_unknown_versions_refuse(self):
        value = fixture()
        value["results"].append(copy.deepcopy(value["results"][0]))
        with self.assertRaisesRegex(ValueError, "duplicate"):
            self.cases(value)
        for version in (2, 4, "3", 3.0):
            value = fixture()
            value["record"] = version
            with self.assertRaisesRegex(ValueError, "version"):
                self.cases(value)

    def test_unknown_oracle_and_overflowing_ratio_are_unqualified(self):
        value = fixture()
        value["results"][0]["answer"]["verification"] = None
        self.assertFalse(self.cases(value)[0]["comparable"])
        value = fixture()
        value["results"][0]["samples_ms"] = [1e308]
        value["results"][1]["samples_ms"] = [1e-308]
        case = self.cases(value)[0]
        self.assertIsNone(case["candidate_over_peer"])
        self.assertIn("unrepresentable-ratio", case["reasons"])


if __name__ == "__main__":
    unittest.main()
