"""Tests of scripts/bench-bsbm/qmph.py: collecting driver runs and comparing their result counts.

    python3 -m unittest discover -s scripts/bench-tests
"""

import json
import os
import subprocess
import sys
import tempfile
import unittest

SCRIPTS = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
QMPH = os.path.join(SCRIPTS, "bench-bsbm", "qmph.py")


def driver_xml(avg, qmph=100.0, timeouts=0, runs=10):
    queries = "".join(
        f'<query nr="{nr}"><executecount>10</executecount><avgresults>{x}</avgresults>'
        f"<timeoutcount>{timeouts}</timeoutcount></query>"
        for nr, x in avg.items()
    )
    return (
        "<?xml version='1.0'?><bsbm><querymix><qmph>%s</qmph><querymixruns>%d</querymixruns>"
        "<actualtotalruntime>3.0</actualtotalruntime></querymix><queries>%s</queries></bsbm>" % (qmph, runs, queries)
    )


class QmphTests(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.out = os.path.join(self.dir.name, "qmph.json")

    def tearDown(self):
        self.dir.cleanup()

    def run_qmph(self, *args):
        return subprocess.run([sys.executable, "-I", QMPH, *args], capture_output=True, text=True, check=True)

    def add(self, engine, clients, avg, status=0, **kw):
        xml = os.path.join(self.dir.name, f"{engine}-{clients}.xml")
        with open(xml, "w") as f:
            f.write(driver_xml(avg, **kw))
        self.run_qmph(xml, self.out, engine, str(clients), "explore-mix", str(status))

    def result(self):
        with open(self.out) as f:
            return json.load(f)

    def test_add_records_average_results(self):
        self.add("a", 1, {"1": 5.0, "2": 0.0})
        run = self.result()["a"]["1"]
        self.assertEqual(run["status"], "ok")
        self.assertEqual(run["avgresults"], {"1": 5.0, "2": 0.0})

    def test_nonzero_exit_status_is_an_error(self):
        self.add("a", 1, {"1": 5.0}, status=255)
        self.assertEqual(self.result()["a"]["1"]["status"], "error")

    def test_missing_xml_is_an_error(self):
        self.run_qmph(os.path.join(self.dir.name, "nope.xml"), self.out, "a", "1", "explore-mix", "255")
        self.assertEqual(self.result()["a"]["1"]["status"], "error")

    def test_timeouts_are_recorded(self):
        self.add("a", 1, {"1": 5.0}, timeouts=2)
        self.assertEqual(self.result()["a"]["1"]["status"], "timeout")

    def test_check_flags_the_run_that_differs(self):
        self.add("a", 1, {"1": 5.0, "2": 7.0})
        self.add("a", 4, {"1": 5.0, "2": 7.0})
        self.add("b", 1, {"1": 5.0, "2": 0.0}, qmph=9999.0)
        self.add("c", 1, {"1": 5.0, "2": 7.0})
        self.run_qmph("--check", self.out)
        d = self.result()
        self.assertEqual(d["a"]["1"]["status"], "ok")
        self.assertEqual(d["a"]["4"]["status"], "ok")
        self.assertEqual(d["c"]["1"]["status"], "ok")
        self.assertEqual(d["b"]["1"]["status"], "wrong-answer")
        self.assertEqual(d["b"]["1"]["disagreeing_queries"], ["2"])

    def test_check_skips_the_named_queries(self):
        self.add("a", 1, {"1": 5.0, "9": 7.0})
        self.add("a", 4, {"1": 5.0, "9": 7.0})
        self.add("b", 1, {"1": 5.0, "9": 100.0})
        self.run_qmph("--check", self.out, "9")
        self.assertEqual(self.result()["b"]["1"]["status"], "ok")

    def test_check_leaves_agreeing_runs_and_ties_alone(self):
        self.add("a", 1, {"1": 5.0})
        self.add("b", 1, {"1": 6.0})
        self.run_qmph("--check", self.out)
        d = self.result()
        self.assertEqual(d["a"]["1"]["status"], "ok")
        self.assertEqual(d["b"]["1"]["status"], "ok")

    def test_failed_runs_do_not_vote(self):
        self.add("e1", 1, {"1": 0.0}, status=255)
        self.add("e1", 4, {"1": 0.0}, status=255)
        self.add("e2", 1, {"1": 20.0})
        self.run_qmph("--check", self.out)
        d = self.result()
        self.assertEqual(d["e2"]["1"]["status"], "ok")
        for c in ("1", "4"):
            self.assertEqual(d["e1"][c]["status"], "error")
            self.assertNotIn("disagreeing_queries", d["e1"][c])

    def test_a_timeout_keeps_its_status(self):
        self.add("a", 1, {"1": 5.0})
        self.add("b", 1, {"1": 5.0})
        self.add("c", 1, {"1": 0.0}, timeouts=1)
        self.run_qmph("--check", self.out)
        self.assertEqual(self.result()["c"]["1"]["status"], "timeout")

    def test_a_recheck_clears_a_stale_label(self):
        self.add("a", 1, {"1": 5.0})
        self.add("b", 1, {"1": 5.0})
        self.add("c", 1, {"1": 0.0})
        self.run_qmph("--check", self.out)
        self.assertEqual(self.result()["c"]["1"]["status"], "wrong-answer")
        self.add("a", 1, {"1": 0.0})
        self.add("b", 1, {"1": 0.0})
        self.run_qmph("--check", self.out)
        c = self.result()["c"]["1"]
        self.assertEqual(c["status"], "ok")
        self.assertNotIn("disagreeing_queries", c)

    def test_an_engine_votes_once(self):
        for clients in (1, 4, 16):
            self.add("A", clients, {"1": 0.0})
        self.add("B", 1, {"1": 5.0})
        self.add("C", 1, {"1": 5.0})
        self.run_qmph("--check", self.out)
        d = self.result()
        for clients in ("1", "4", "16"):
            self.assertEqual(d["A"][clients]["status"], "wrong-answer")
        self.assertEqual(d["B"]["1"]["status"], "ok")
        self.assertEqual(d["C"]["1"]["status"], "ok")

    def test_runs_with_different_mix_counts_are_not_compared(self):
        self.add("a", 1, {"1": 5.0}, runs=10)
        self.add("b", 1, {"1": 0.0}, runs=20)
        self.add("c", 1, {"1": 0.0}, runs=20)
        self.run_qmph("--check", self.out)
        d = self.result()
        self.assertEqual(d["a"]["1"]["status"], "ok")
        self.assertEqual(d["b"]["1"]["status"], "ok")

    def test_two_engines_that_disagree_are_a_tie_and_neither_is_flagged(self):
        self.add("a", 1, {"1": 5.0})
        self.add("a", 4, {"1": 5.0})
        self.add("b", 1, {"1": 0.0})
        self.run_qmph("--check", self.out)
        d = self.result()
        self.assertEqual(d["a"]["1"]["status"], "ok")
        self.assertEqual(d["b"]["1"]["status"], "ok")

    def test_one_engine_still_checks_its_runs_against_each_other(self):
        self.add("a", 1, {"1": 5.0})
        self.add("a", 4, {"1": 5.0})
        self.add("a", 16, {"1": 0.0})
        self.run_qmph("--check", self.out)
        d = self.result()["a"]
        self.assertEqual(d["16"]["status"], "wrong-answer")
        self.assertEqual(d["1"]["status"], "ok")


if __name__ == "__main__":
    unittest.main()
