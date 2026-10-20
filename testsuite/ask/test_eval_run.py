"""The run file of scripts/eval-ask-anthropic."""

import copy
import importlib.machinery
import importlib.util
import json
import pathlib
import sys
import tempfile
import unittest

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE / "eval_support"))
from evaluator import Invalid

loader = importlib.machinery.SourceFileLoader(
    "run_under_test", str(HERE.parents[1] / "scripts/eval-ask-anthropic")
)
spec = importlib.util.spec_from_loader(loader.name, loader)
runner = importlib.util.module_from_spec(spec)
loader.exec_module(runner)

RUN = {
    "schema": 1,
    "binary": "/abs/sparkles",
    "binarySha256": "0" * 64,
    "ledger": "/abs/ledger/ledger.jsonl",
    "out": "/abs/out",
    "localCapNanodollars": 23_997_440_000,
    "carryover": {"name": "pilot", "nanodollars": 1_002_560_000},
    "effort": "medium",
    "maxOutputTokens": 4096,
    "requestTimeoutSecs": 120,
    "askDeadlineSecs": 300,
    "queryTimeoutSecs": 5,
    "askTokenBudget": 100000,
    "projectionMargin": 1.25,
    "priorCaseCostNanodollars": {"claude-haiku-5-5": 1},
    "sets": [
        {
            "name": "demo",
            "path": "testsuite/ask/demo.json",
            "models": ["claude-haiku-5-5"],
        }
    ],
}


class LoadRun(unittest.TestCase):
    def load(self, run):
        with tempfile.TemporaryDirectory() as d:
            path = pathlib.Path(d) / "run.json"
            path.write_text(json.dumps(run))
            return runner.load_run(path)

    def test_a_complete_run_file_loads(self):
        self.assertEqual(self.load(RUN)["effort"], "medium")

    def test_the_documented_example_loads(self):
        example = json.loads((HERE / "run.example.json").read_text())
        self.assertEqual(self.load(example)["askTokenBudget"], 100000)

    def test_a_real_run_names_the_pilot_carryover_and_stays_under_the_hard_cap(self):
        for change in (
            {"carryover": None},
            {"localCapNanodollars": 25_000_000_000},
            {"localCapNanodollars": 0},
        ):
            with self.subTest(change=change), self.assertRaises(Invalid):
                self.load(dict(copy.deepcopy(RUN), **change))


class LedgerCommand(unittest.TestCase):
    def run_command(self, run, **kw):
        import argparse
        import contextlib
        import io

        args = argparse.Namespace(
            held=False,
            reconcile=None,
            billed_nanodollars=None,
            evidence=None,
            allow_raise=False,
        )
        vars(args).update(kw)
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            self.assertEqual(runner.ledger_command(run, args), 0)
        return json.loads(out.getvalue())

    def test_the_carryover_is_reconciled_before_any_request(self):
        with tempfile.TemporaryDirectory() as d:
            run = dict(copy.deepcopy(RUN), ledger=str(pathlib.Path(d) / "l.jsonl"))
            held = self.run_command(run, held=True)["heldAtBound"]
            self.assertEqual(
                held, [{"target": "carryover", "name": "pilot", "cost": 1_002_560_000}]
            )
            with self.assertRaises(Invalid):
                self.run_command(run, reconcile="carryover", billed_nanodollars=1)
            done = self.run_command(
                run,
                reconcile="carryover",
                billed_nanodollars=1_600_000,
                evidence="Console cost of 2026-10-15",
            )
            self.assertEqual(done["upperLiabilityNanodollars"], 1_600_000)
            self.assertEqual(done["heldAtBound"], [])
            again = self.run_command(run, held=True)
            self.assertEqual(again["carryoverNanodollars"], 1_600_000)
            for target in ("carryover", "7", "x"):
                with self.subTest(target=target), self.assertRaises(Invalid):
                    self.run_command(
                        run,
                        reconcile=target,
                        billed_nanodollars=0,
                        evidence="Console cost of 2026-10-15",
                    )

    def test_a_raise_needs_allow_raise(self):
        with tempfile.TemporaryDirectory() as d:
            run = dict(copy.deepcopy(RUN), ledger=str(pathlib.Path(d) / "l.jsonl"))
            raised = {
                "reconcile": "carryover",
                "billed_nanodollars": 1_100_000_000,
                "evidence": "Console cost of 2026-10-15",
            }
            with self.assertRaises(Invalid):
                self.run_command(run, **raised)
            with self.assertRaises(Invalid):
                self.run_command(run, held=True, allow_raise=True)
            done = self.run_command(run, allow_raise=True, **raised)
            self.assertEqual(done["carryoverNanodollars"], 1_100_000_000)
            self.assertIs(done["reconciled"][0]["raise"], True)


if __name__ == "__main__":
    unittest.main()
