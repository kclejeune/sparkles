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


def driver_xml(avg, qmph=100.0, timeouts=0):
    queries = "".join(
        f'<query nr="{nr}"><executecount>10</executecount><avgresults>{x}</avgresults>'
        f"<timeoutcount>{timeouts}</timeoutcount></query>"
        for nr, x in avg.items()
    )
    return (
        "<?xml version='1.0'?><bsbm><querymix><qmph>%s</qmph><querymixruns>10</querymixruns>"
        "<actualtotalruntime>3.0</actualtotalruntime></querymix><queries>%s</queries></bsbm>" % (qmph, queries)
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
        self.run_qmph("--check", self.out)
        d = self.result()
        self.assertEqual(d["a"]["1"]["status"], "ok")
        self.assertEqual(d["a"]["4"]["status"], "ok")
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


if __name__ == "__main__":
    unittest.main()
