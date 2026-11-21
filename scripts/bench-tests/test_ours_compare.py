"""Tests of scripts/elk-compare/ours-compare.py: which reports claim a complete answer.

    python3 -m unittest discover -s scripts/bench-tests
"""

import git_isolation  # noqa: F401, first, so no fixture sees the host Git settings
import json
import os
import subprocess
import sys
import tempfile
import unittest

SCRIPTS = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PATH = os.path.join(SCRIPTS, "elk-compare", "ours-compare.py")
EX = "http://example.org/h#"


class ClaimTests(unittest.TestCase):
    def judge(self, **fields):
        """The outcome of a run whose taxonomy misses the reference's K ⊑ Ans."""
        with tempfile.TemporaryDirectory() as d:
            ref = os.path.join(d, "hermit.owl")
            with open(ref, "w") as f:
                f.write(f"SubClassOf(<{EX}K> <{EX}Ans>)\n")
            tax = os.path.join(d, "ours.txt")
            with open(tax, "w") as f:
                f.write(f"EQ {EX}K\nEQ {EX}Ans\n")
            report = os.path.join(d, "report.json")
            base = {"complete": True, "outside": 0, "rules": 0, "partial": {}, "tier": "disjunctive"}
            with open(report, "w") as f:
                json.dump({**base, **fields}, f)
            out = subprocess.run(
                [sys.executable, PATH, "--status", "0", "--name", "n", "--hermit", ref,
                 "--report", report, "--taxonomy", tax],
                check=True, capture_output=True, text=True,
            ).stdout
            return json.loads(out)["outcome"]

    def test_a_complete_report_that_misses_a_subsumption_differs(self):
        self.assertEqual(self.judge(dlRootedDeclined=False), "differs")

    def test_a_declined_rooted_report_claims_nothing(self):
        self.assertEqual(self.judge(dlRootedDeclined=True), "sound")


if __name__ == "__main__":
    unittest.main()
