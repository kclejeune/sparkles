"""Tests of scripts/unittest-compare.py, the nightly gate's comparison of the serial and
parallel runs of the script tests."""

import git_isolation  # noqa: F401, first, so no fixture sees the host Git settings
import importlib.util
import os
import shutil
import signal
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
        self.assertIsNone(compare.parallel_counts(str(d), "", ""))
        (d / "run-x").mkdir()
        (d / "run-x" / "record-0.json").write_text('{"testsRun": 2, "skipped": 1}')
        (d / "run-x" / "record-1.json").write_text('{"testsRun": 3, "skipped": 0}')
        self.assertEqual(compare.parallel_counts(str(d), "", ""), (5, 1))

    def test_a_runner_with_no_records_counts_only_when_it_says_why(self):
        # Review r11 finding 5: an empty suite starts no unit, and a host the runner cannot
        # supervise from runs the suite serially, and neither leaves a record.
        d = Path(tempfile.mkdtemp(prefix="unittest-compare-"))
        self.addCleanup(shutil.rmtree, d, True)
        empty = "unittest-parallel: 0 tests in 0 processes, 8 at a time, 0.1 s\nunittest-parallel: NO TESTS RAN\n"
        self.assertEqual(compare.parallel_counts(str(d), empty, ""), (0, 0))
        fell_back = "unittest-parallel: this host has no /proc, so the suite runs serially with unittest discover\n"
        self.assertEqual(compare.parallel_counts(str(d), fell_back, "Ran 2 tests in 0.0s\n\nOK (skipped=1)\n"), (2, 1))
        self.assertIsNone(compare.parallel_counts(str(d), fell_back, ""))
        self.assertIsNone(compare.parallel_counts(str(d), "unittest-parallel: discovery failed with exit 1\n", ""))


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

    def test_a_summary_that_a_test_prints_is_not_taken_for_unittests(self):
        # Review r11 finding 1: a test's buffered standard output can come after unittest's
        # summary, so only the standard error's summary counts.
        d = self.suite(test_a="""
            import unittest
            class A(unittest.TestCase):
                def test_ok(self):
                    print("Ran 9 tests in 0.001s\\n\\nOK (skipped=8)")
            """)
        p = self.run_compare(str(d))
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        self.assertIn("both arms exited 0 with 1 tests run and 0 skipped", p.stdout)

    def test_an_empty_suite_exits_5_in_both_arms(self):
        # Review r11 finding 5.
        d = self.suite()
        p = self.run_compare(str(d))
        self.assertEqual(p.returncode, 5, p.stdout + p.stderr)
        self.assertIn("both arms exited 5 with 0 tests run and 0 skipped", p.stdout)

    def test_a_signal_while_an_arm_starts_stops_it(self):
        # Review r11 finding 4: a signal between starting an arm and registering it still
        # reaches the arm, which would otherwise run to its end.
        d = self.suite(test_a="""
            import time, unittest
            class A(unittest.TestCase):
                def test_slow(self):
                    time.sleep(60)
            """)
        probe = textwrap.dedent(f"""
            import importlib.util, os, signal, sys
            spec = importlib.util.spec_from_file_location("compare", {str(SCRIPT)!r})
            m = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(m)
            original = m.subprocess.Popen

            def launch(*args, **kwargs):
                proc = original(*args, **kwargs)
                print("pid", proc.pid, flush=True)
                os.kill(os.getpid(), signal.SIGTERM)
                return proc

            m.subprocess.Popen = launch
            sys.exit(m.main([{str(d)!r}]))
            """)
        p = subprocess.run([sys.executable, "-c", probe], stdin=subprocess.DEVNULL, capture_output=True,
                           text=True, timeout=30)
        self.assertEqual(p.returncode, 128 + signal.SIGTERM, p.stdout + p.stderr)
        pid = int(p.stdout.split("pid ", 1)[1].split()[0])
        with self.assertRaises(ProcessLookupError):
            os.kill(pid, 0)

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
