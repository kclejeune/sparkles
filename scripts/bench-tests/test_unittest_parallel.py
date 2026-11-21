"""Tests of scripts/unittest-parallel.py, the parallel runner of test:bench-scripts.

    python3 -m unittest discover -s scripts/bench-tests -p test_unittest_parallel.py
"""

import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

RUNNER = Path(__file__).resolve().parent.parent / "unittest-parallel.py"

PASS = """
import unittest
import helper

class A(unittest.TestCase):
    def test_one(self):
        self.assertEqual(helper.VALUE, 1)

    def test_two(self):
        pass
"""

FAIL = """
import unittest

class B(unittest.TestCase):
    def test_bad(self):
        self.fail("expected failure")
"""


class Runner(unittest.TestCase):
    def suite(self, **modules):
        d = Path(tempfile.mkdtemp())
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", str(d)], check=False))
        (d / "helper.py").write_text("VALUE = 1\n")
        for name, body in modules.items():
            (d / f"{name}.py").write_text(textwrap.dedent(body))
        return d

    def run_runner(self, *args, cwd=None):
        return subprocess.run([sys.executable, str(RUNNER), *args], cwd=cwd, capture_output=True, text=True, timeout=120)

    def test_all_modules_pass_and_their_tests_are_counted(self):
        d = self.suite(test_a=PASS, test_c=PASS)
        p = self.run_runner("--jobs", "2", str(d))
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        self.assertIn("4 tests in 2 processes, 2 at a time", p.stdout)

    def test_a_failing_module_fails_the_run_and_is_named(self):
        d = self.suite(test_a=PASS, test_b=FAIL)
        p = self.run_runner(str(d))
        self.assertEqual(p.returncode, 1)
        self.assertIn("expected failure", p.stdout)
        self.assertIn("failed: test_b", p.stdout)

    def test_a_split_module_runs_one_process_per_test(self):
        d = self.suite(test_a=PASS, test_c=PASS)
        p = self.run_runner("--split", "test_a", str(d))
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        self.assertIn("4 tests in 3 processes", p.stdout)
        self.assertIn("=== test_a.A.test_one: exit 0, 1 tests", p.stdout)

    def test_a_module_that_does_not_import_fails_the_run(self):
        d = self.suite(test_a=PASS, test_e="import no_such_module_here\n")
        p = self.run_runner(str(d))
        self.assertEqual(p.returncode, 1)
        self.assertIn("failed: test_e", p.stdout)

    def test_the_units_cover_what_discover_finds_in_packages_too(self):
        d = self.suite(test_a=PASS, testb=PASS)
        (d / "pkg").mkdir()
        (d / "pkg" / "__init__.py").write_text("")
        (d / "pkg" / "test_inner.py").write_text(textwrap.dedent(FAIL))
        serial = subprocess.run([sys.executable, "-m", "unittest", "discover", "-s", str(d)], capture_output=True, text=True, timeout=120)
        self.assertIn("Ran 5 tests", serial.stderr)
        p = self.run_runner(str(d))
        self.assertIn("5 tests in 3 processes", p.stdout)
        self.assertIn("failed: pkg.test_inner", p.stdout)

    def test_an_unknown_split_module_is_refused(self):
        d = self.suite(test_a=PASS)
        p = self.run_runner("--split", "test_x", str(d))
        self.assertNotEqual(p.returncode, 0)
        self.assertIn("no module test_x", p.stderr)

    def test_units_run_from_the_callers_directory(self):
        d = self.suite(test_w="""
import os, unittest

class W(unittest.TestCase):
    def test_cwd(self):
        self.assertTrue(os.path.exists("marker"))
""")
        work = Path(tempfile.mkdtemp())
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", str(work)], check=False))
        (work / "marker").write_text("")
        p = self.run_runner(str(d), cwd=work)
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)


if __name__ == "__main__":
    unittest.main()
