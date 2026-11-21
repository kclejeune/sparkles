"""Tests of scripts/unittest-compare.py, the nightly gate's comparison of the serial and
parallel runs of the script tests."""

import git_isolation  # noqa: F401, first, so no fixture sees the host Git settings
import importlib.util
import shutil
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
SCRIPT = HERE.parent / "unittest-compare.py"
_spec = importlib.util.spec_from_file_location("unittest_compare", SCRIPT)
compare = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(compare)


class Counts(unittest.TestCase):
    def test_the_last_summary_is_unittests_own(self):
        text = "Ran 9 tests in 0.1s\nOK\n...\nRan 3 tests in 0.002s\n\nOK (skipped=1)\n"
        self.assertEqual(compare.serial_counts(text), (3, 1))

    def test_a_failed_summary_gives_its_skips(self):
        text = "Ran 1 test in 0.0s\n\nFAILED (failures=1, errors=2, skipped=4)\n"
        self.assertEqual(compare.serial_counts(text), (1, 4))

    def test_no_tests_and_no_summary(self):
        self.assertEqual(compare.serial_counts("Ran 0 tests in 0.000s\n\nNO TESTS RAN\n"), (0, 0))
        self.assertIsNone(compare.serial_counts("Traceback (most recent call last):\n"))

    def test_records_are_summed(self):
        d = Path(tempfile.mkdtemp(prefix="unittest-compare-"))
        self.addCleanup(shutil.rmtree, d, True)
        self.assertIsNone(compare.parallel_counts(str(d)))
        (d / "run-x").mkdir()
        (d / "run-x" / "record-0.json").write_text('{"testsRun": 2, "skipped": 1}')
        (d / "run-x" / "record-1.json").write_text('{"testsRun": 3, "skipped": 0}')
        self.assertEqual(compare.parallel_counts(str(d)), (5, 1))


class Differences(unittest.TestCase):
    def test_equal_arms_have_none(self):
        self.assertEqual(compare.differences((0, (4, 1)), (0, (4, 1))), [])
        self.assertEqual(compare.differences((1, (4, 0)), (1, (4, 0))), [])

    def test_each_difference_is_named(self):
        self.assertEqual(compare.differences((1, (4, 0)), (0, (4, 0))),
                         ["serial discovery exited 1 and the parallel runner 0"])
        self.assertEqual(compare.differences((0, (4, 1)), (0, (3, 2))),
                         ["serial discovery ran 4 tests and the parallel runner 3",
                          "serial discovery skipped 1 tests and the parallel runner 2"])
        self.assertEqual(compare.differences((1, None), (1, None)),
                         ["serial discovery printed no summary", "the parallel runner left no result record"])


class EndToEnd(unittest.TestCase):
    def suite(self, **modules):
        d = Path(tempfile.mkdtemp(prefix="unittest-compare-"))
        self.addCleanup(shutil.rmtree, d, True)
        for name, body in modules.items():
            (d / f"{name}.py").write_text(textwrap.dedent(body))
        return d

    def run_compare(self, *args):
        return subprocess.run([sys.executable, str(SCRIPT), *args], stdin=subprocess.DEVNULL,
                              capture_output=True, text=True, timeout=120)

    def test_arms_that_agree_pass(self):
        d = self.suite(test_a="""
            import unittest
            class A(unittest.TestCase):
                def test_ok(self):
                    pass
                @unittest.skip("not here")
                def test_skip(self):
                    pass
            """, test_b="""
            import unittest
            class B(unittest.TestCase):
                def test_ok(self):
                    pass
            """)
        p = self.run_compare(str(d))
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        self.assertIn("both arms exited 0 with 3 tests run and 1 skipped", p.stdout)

    def test_a_failing_suite_fails_in_both_arms(self):
        d = self.suite(test_a="""
            import unittest
            class A(unittest.TestCase):
                def test_bad(self):
                    self.fail("bad")
            """)
        p = self.run_compare(str(d))
        self.assertEqual(p.returncode, 1, p.stdout + p.stderr)
        self.assertIn("both arms exited 1 with 1 tests run and 0 skipped", p.stdout)

    def test_state_shared_through_a_helper_module_is_caught(self):
        # Serially test_a runs first and changes the helper's global, which test_b reads. In
        # the parallel runner each module runs in its own process, so test_b passes there.
        d = self.suite(helper="VALUE = 1\n", test_a="""
            import unittest, helper
            class A(unittest.TestCase):
                def test_set(self):
                    helper.VALUE = 2
            """, test_b="""
            import unittest, helper
            class B(unittest.TestCase):
                def test_read(self):
                    self.assertEqual(helper.VALUE, 1)
            """)
        p = self.run_compare(str(d))
        self.assertEqual(p.returncode, 1, p.stdout + p.stderr)
        self.assertIn("DIFFERENT: serial discovery exited 1 and the parallel runner 0", p.stdout)

    def test_runner_options_pass_through_and_keep_is_refused(self):
        d = self.suite(test_a="""
            import unittest
            class A(unittest.TestCase):
                def test_one(self):
                    pass
                def test_two(self):
                    pass
            """)
        p = self.run_compare("--split", "test_a", str(d))
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        self.assertIn("2 tests in 2 processes", p.stdout)
        p = self.run_compare("--keep", str(d), str(d))
        self.assertNotEqual(p.returncode, 0)
        self.assertIn("without --keep", p.stderr)


if __name__ == "__main__":
    unittest.main()
