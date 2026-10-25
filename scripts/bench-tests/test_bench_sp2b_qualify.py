"""Binding regressions for derived SP2Bench records qualified by a source oracle."""

import copy
import hashlib
import importlib.util
import json
import pathlib
import subprocess
import sys
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "bench-sp2b/qualify.py"
SPEC = importlib.util.spec_from_file_location("sp2b_qualify", SCRIPT)
qualifier = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(qualifier)
RECORD_SPEC = importlib.util.spec_from_file_location("sp2b_recorder", SCRIPT.parent.parent / "bench-record.py")
recorder = importlib.util.module_from_spec(RECORD_SPEC)
RECORD_SPEC.loader.exec_module(recorder)


class QualificationTests(unittest.TestCase):
    def fixture(self, directory):
        digest = "row-sha256-v1:" + "a" * 64
        fp = {"rows": 2, "exact": digest, "value": digest, "query_sha256": "b" * 64}
        record = {"record": 3, "suite": "sp2bench", "scale": "10m", "settings": {"resultCache": False, "warmup": 2, "runs": 2},
                  "manifest": {"host": {"name": "forge"}, "sparkles": {"sha256": "c" * 64, "binary_commit": "actual-build"},
                               "data": {"sp2bench-10m": {"sha256": "d" * 64}}},
                  "results": [{"engine": "sparkles", "query": "q4", "mode": "warm", "clients": 1, "status": "ok",
                               "samples_ms": [1000, 2000], "median_ms": 1500, "p95_ms": 2000,
                               "answer": {"rows": 2, "hash": digest, "agrees": None}}]}
        documents = {"raw_record": record, "raw_answers": {"q4": {"sparkles": fp}},
                     "oracle": {"version": 1, "method": "source-triple-reconstruction", "source_sha256": "d" * 64, "queries": {"q4": copy.deepcopy(fp)}},
                     "q4_timing": {"results": [{"command": "sparkles", "times": [1, 2], "exit_codes": [0, 0]}]}}
        bindings = {"bindings_version": 1, "scale": "10m", "host": "forge", "engine": "sparkles", "dataset": "sp2bench-10m",
                    "source_sha256": "d" * 64, "binary_sha256": "c" * 64, "binary_commit": "actual-build",
                    "settings": {"resultCache": False, "warmup": 2, "runs": 2},
                    "queries": {"q4": {"sha256": "b" * 64, "runs": 2, "timing_role": "q4_timing"}}, "inputs": {}}
        for role, document in documents.items():
            self.write(directory, bindings, role, document)
        return bindings, documents

    def write(self, directory, bindings, role, document):
        path = pathlib.Path(directory) / (role + ".json")
        raw = (json.dumps(document) + "\n").encode()
        path.write_bytes(raw)
        bindings["inputs"][role] = {"path": path.name, "sha256": hashlib.sha256(raw).hexdigest()}

    def test_qualification_is_deterministic_and_preserves_raw_inputs_and_samples(self):
        with tempfile.TemporaryDirectory() as directory:
            bindings, documents = self.fixture(directory)
            before = {p.name: p.read_bytes() for p in pathlib.Path(directory).glob("*.json")}
            first = qualifier.qualify(directory, bindings)
            self.assertEqual(first, qualifier.qualify(directory, bindings))
            self.assertEqual(before, {p.name: p.read_bytes() for p in pathlib.Path(directory).glob("*.json")})
            derived, receipt = first
            self.assertTrue(derived["results"][0]["answer"]["agrees"])
            self.assertIsNone(documents["raw_record"]["results"][0]["answer"]["agrees"])
            self.assertEqual(derived["results"][0]["samples_ms"], documents["raw_record"]["results"][0]["samples_ms"])
            self.assertEqual(derived["correctness"]["method"], "independent-data-oracle")
            self.assertEqual(receipt["correctness"]["canonicalization"]["fingerprint"], "row-sha256-v1")

    def test_changed_raw_source_fails_its_pinned_content_hash(self):
        with tempfile.TemporaryDirectory() as directory:
            bindings, _ = self.fixture(directory)
            pathlib.Path(directory, "raw_record.json").write_text('{}')
            with self.assertRaisesRegex(ValueError, "changed input: raw_record"):
                qualifier.qualify(directory, bindings)

    def test_stale_reported_timing_summaries_are_rejected(self):
        cases = [("raw_record", "median_ms"), ("raw_record", "p95_ms"),
                 ("q4_timing", "mean"), ("q4_timing", "stddev"), ("q4_timing", "median")]
        for role, field in cases:
            with self.subTest(role=role, field=field), tempfile.TemporaryDirectory() as directory:
                bindings, documents = self.fixture(directory)
                documents[role]["results"][0][field] = 99
                self.write(directory, bindings, role, documents[role])
                with self.assertRaisesRegex(ValueError, "timing summary"):
                    qualifier.qualify(directory, bindings)

    def test_recorder_rounding_boundary_is_accepted(self):
        with tempfile.TemporaryDirectory() as directory:
            bindings, documents = self.fixture(directory)
            times = [.1, .200001]
            documents["q4_timing"]["results"][0]["times"] = times
            answer = documents["raw_record"]["results"][0]["answer"]
            row = recorder.result("sparkles", "q4", "warm", times, [0, 0], answer)
            self.assertEqual(row["median_ms"], 150.0)
            documents["raw_record"]["results"] = [row]
            for role in ("raw_record", "q4_timing"):
                self.write(directory, bindings, role, documents[role])
            derived, _ = qualifier.qualify(directory, bindings)
            self.assertEqual(derived["results"][0]["median_ms"], row["median_ms"])

    def test_unknown_and_missing_record_versions_are_rejected(self):
        for version in (None, 99):
            with self.subTest(version=version), tempfile.TemporaryDirectory() as directory:
                bindings, documents = self.fixture(directory)
                if version is None:
                    documents["raw_record"].pop("record")
                else:
                    documents["raw_record"]["record"] = version
                self.write(directory, bindings, "raw_record", documents["raw_record"])
                with self.assertRaisesRegex(ValueError, "record version"):
                    qualifier.qualify(directory, bindings)

    def test_cli_rejects_output_aliases_before_changing_evidence(self):
        for kind in ("raw", "receipt-raw", "bindings", "symlink", "hardlink", "same-outputs", "linked-outputs"):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as directory:
                root = pathlib.Path(directory)
                bindings, _ = self.fixture(directory)
                binding_file = root / "bindings.json"
                binding_file.write_text(json.dumps(bindings))
                out, receipt = root / "derived.json", root / "receipt.json"
                if kind == "raw":
                    out = root / "raw_record.json"
                elif kind == "receipt-raw":
                    receipt = root / "raw_answers.json"
                elif kind == "bindings":
                    out = binding_file
                elif kind == "symlink":
                    out.symlink_to(root / "oracle.json")
                elif kind == "hardlink":
                    out.hardlink_to(root / "raw_record.json")
                elif kind == "same-outputs":
                    receipt = out
                elif kind == "linked-outputs":
                    out.write_text("existing output")
                    receipt.hardlink_to(out)
                before = {p.name: p.read_bytes() for p in root.glob("*.json")}
                run = subprocess.run([sys.executable, str(SCRIPT), str(binding_file), str(out), str(receipt)],
                                     capture_output=True, text=True)
                self.assertNotEqual(run.returncode, 0, run.stdout + run.stderr)
                self.assertIn("output collision", run.stderr)
                self.assertEqual(before, {name: (root / name).read_bytes() for name in before})

    def test_wrong_dataset_query_oracle_rows_binary_or_samples_cannot_be_admitted(self):
        cases = [
            ("raw_record", lambda x: x["manifest"]["data"]["sp2bench-10m"].update(sha256="e" * 64), "wrong benchmark dataset"),
            ("oracle", lambda x: x.update(source_sha256="e" * 64), "wrong oracle dataset"),
            ("oracle", lambda x: x.update(method="untrusted-count"), "unknown oracle contract"),
            ("raw_answers", lambda x: x["q4"]["sparkles"].update(query_sha256="e" * 64), "wrong query source"),
            ("oracle", lambda x: x["queries"]["q4"].update(query_sha256="e" * 64), "wrong query source"),
            ("oracle", lambda x: x["queries"]["q4"].update(rows=1), "answer differs"),
            ("raw_answers", lambda x: x["q4"]["sparkles"].update(rows=1), "answer differs"),
            ("raw_record", lambda x: x["manifest"]["sparkles"].update(sha256="e" * 64), "wrong runtime binary"),
            ("raw_record", lambda x: x["results"][0]["answer"].update(rows=1), "not the checked answer"),
            ("q4_timing", lambda x: x["results"][0].update(times=[1, 3]), "changed timing samples"),
            ("q4_timing", lambda x: x["results"][0].update(exit_codes=[0, 28]), "failed raw timing samples"),
        ]
        for role, mutate, message in cases:
            with self.subTest(role=role, message=message), tempfile.TemporaryDirectory() as directory:
                bindings, documents = self.fixture(directory)
                mutate(documents[role])
                self.write(directory, bindings, role, documents[role])
                with self.assertRaisesRegex(ValueError, message):
                    qualifier.qualify(directory, bindings)


if __name__ == "__main__":
    unittest.main()
