"""Synthetic root-decision schemas; never a real admission, key or upstream."""

import importlib.machinery
import importlib.util
import json
import pathlib
import sys
import tempfile
import unittest
from unittest import mock

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE / "eval_support"))
from anthropic_gate import AUTHORITY_HASHES
from evaluator import Invalid, digest

loader = importlib.machinery.SourceFileLoader(
    "admission_fixture", str(HERE.parents[1] / "scripts/eval-ask-anthropic")
)
spec = importlib.util.spec_from_loader(loader.name, loader)
entry = importlib.util.module_from_spec(spec)
loader.exec_module(entry)


class Admission(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="ask-admission-")
        self.addCleanup(self.tmp.cleanup)
        self.d = pathlib.Path(self.tmp.name)
        # Authority hashes are patched only for this labelled synthetic schema
        # control. Production hashes remain the five independently pinned inputs.
        self.authorities = {}
        self.fake_hashes = {}
        for name in AUTHORITY_HASHES:
            p = self.d / (name + ".json")
            p.write_text('{"synthetic":true}')
            self.authorities[name] = {"path": str(p), "sha256": digest(p)}
            self.fake_hashes[name] = digest(p)
        (self.d / "data").write_text("")
        (self.d / "set").write_text('{"data":"data"}')
        binary = self.d / "fake"
        binary.write_text("not an executable; validation only")
        self.a = {
            "schema": 2,
            "paid_run_admitted": True,
            "run_id": "SYNTHETIC_NOT_ADMITTED",
            "wallet_path": str(self.d / "wallet"),
            "grant_claim_path": str(self.d / "claim"),
            "output": str(self.d / "report"),
            "local_filesystem": "ext4",
            "wallet_parent_device": self.d.stat().st_dev,
            "authorities": self.authorities,
            "sources": {
                p: digest(entry.ROOT / p)
                for p in [
                    "scripts/eval-ask",
                    "scripts/eval-ask-anthropic",
                    "testsuite/ask/eval_support/evaluator.py",
                    "testsuite/ask/eval_support/anthropic_gate.py",
                ]
            },
            "runtime": entry.runtime_identity(),
            "binary": str(binary),
            "binary_sha256": digest(binary),
            "set": str(self.d / "set"),
            "set_sha256": digest(self.d / "set"),
            "data_sha256": digest(self.d / "data"),
            "only": ",".join("id" + str(i) for i in range(17)),
            "key_environment": "ANTHROPIC_API_KEY",
        }
        self.path = self.d / "admission.json"

    def validate(self):
        self.path.write_text(json.dumps(self.a))
        with mock.patch.dict(entry.AUTHORITY_HASHES, self.fake_hashes, clear=True):
            return entry.validate_admission(self.path, digest(self.path))

    def test_complete_synthetic_schema_control_no_execution(self):
        self.assertEqual(self.validate()["run_id"], "SYNTHETIC_NOT_ADMITTED")
        self.assertFalse(pathlib.Path(self.a["wallet_path"]).exists())

    def test_missing_unknown_stale_false_and_reused_zero_execution(self):
        original = json.loads(json.dumps(self.a))
        for field, value in [
            ("schema", 1),
            ("paid_run_admitted", False),
            ("runtime", {}),
            ("sources", {}),
            ("authorities", {}),
            ("wallet_parent_device", -1),
            ("local_filesystem", "unverified"),
            ("key_environment", "OTHER"),
            ("binary_sha256", "0" * 64),
        ]:
            self.a = dict(original)
            self.a[field] = value
            with self.subTest(field=field), self.assertRaises(Invalid):
                self.validate()
        self.a = dict(original)
        self.a["authorities"] = dict(original["authorities"])
        self.a["authorities"]["billing"] = {
            "path": str(self.d / "billing.json"),
            "sha256": "0" * 64,
        }
        with self.assertRaises(Invalid):
            self.validate()
        self.a = original
        pathlib.Path(self.a["wallet_path"]).mkdir()
        with self.assertRaises(Invalid):
            self.validate()
        self.assertFalse(pathlib.Path(self.a["grant_claim_path"]).exists())

    def test_literal_production_authorities_remain_required(self):
        self.path.write_text(json.dumps(self.a))
        with self.assertRaises(Invalid):
            entry.validate_admission(self.path, digest(self.path))
        with self.assertRaises(Invalid):
            entry.validate_admission(self.path, "0" * 64)


if __name__ == "__main__":
    unittest.main()
