"""Tests of scripts/unittest-parallel.py, the parallel runner of test:bench-scripts.

    python3 -m unittest discover -s scripts/bench-tests -p test_unittest_parallel.py

Most tests build a small suite in a temporary directory and compare the runner's verdict
with the exit status of a fresh `python3 -m unittest discover -s DIR` of the same suite, so
the runner is held to serial discovery's verdict rather than to counts. The deadline tests
check that a hung unit, a hung discovery and an interrupted run end in bounded time, that
the processes the tests started are gone afterwards, and that an unrelated process lives.
"""

import git_isolation  # noqa: F401, first, so no fixture sees the host Git settings
import importlib.util
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import textwrap
import time
import unittest
from unittest import mock

import process_checks

HERE = Path(__file__).resolve().parent
# UNITTEST_PARALLEL_RUNNER points the tests at another copy of the runner, such as an older one.
RUNNER = Path(os.environ.get("UNITTEST_PARALLEL_RUNNER", HERE.parent / "unittest-parallel.py"))

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

ONE = """
import unittest

class One(unittest.TestCase):
    def test_ok(self):
        pass
"""


def write(d, name, body):
    p = d / name
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(textwrap.dedent(body))


class Base(unittest.TestCase):
    def suite(self, **modules):
        d = Path(tempfile.mkdtemp(prefix="unittest-parallel-"))
        self.addCleanup(shutil.rmtree, d, True)
        (d / "helper.py").write_text("VALUE = 1\n")
        for name, body in modules.items():
            write(d, f"{name}.py", body)
        return d

    def run_runner(self, *args, cwd=None, timeout=120):
        return subprocess.run([sys.executable, str(RUNNER), *args], cwd=cwd, stdin=subprocess.DEVNULL,
                              capture_output=True, text=True, timeout=timeout)

    def serial(self, d, cwd=None):
        return subprocess.run([sys.executable, "-m", "unittest", "discover", "-s", str(d)], cwd=cwd,
                              stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=120)

    def same_verdict(self, d, *args, expect):
        """Run serial discovery and the runner on D, and check both exit with EXPECT."""
        s = self.serial(d)
        self.assertEqual(s.returncode, expect, "serial discovery:\n" + s.stdout + s.stderr)
        p = self.run_runner(*args, str(d))
        self.assertEqual(p.returncode, expect, "runner:\n" + p.stdout + p.stderr)
        return s, p


class Runner(Base):
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
        self.same_verdict(d, expect=1)
        p = self.run_runner(str(d))
        self.assertIn("failed: test_e", p.stdout)

    def test_the_units_cover_what_discover_finds_in_packages_too(self):
        d = self.suite(test_a=PASS, testb=PASS)
        write(d, "pkg/__init__.py", "")
        write(d, "pkg/test_inner.py", FAIL)
        s, p = self.same_verdict(d, expect=1)
        self.assertIn("Ran 5 tests", s.stderr)
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
        self.addCleanup(shutil.rmtree, work, True)
        (work / "marker").write_text("")
        p = self.run_runner(str(d), cwd=work)
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)


class Discovery(Base):
    """The runner runs what serial discovery runs and reaches its verdict."""

    def test_load_tests_sees_the_discovery_pattern(self):
        # Serial discovery passes the pattern to load_tests, which picks the failing test.
        d = self.suite(test_a=ONE, test_lt="""
            import unittest

            class Good(unittest.TestCase):
                def test_good(self):
                    pass

            class Bad(unittest.TestCase):
                def test_bad(self):
                    self.fail("the discovered test")

            def load_tests(loader, tests, pattern):
                cls = Bad if pattern is not None else Good
                return loader.loadTestsFromTestCase(cls)
            """)
        self.same_verdict(d, expect=1)

    def test_a_load_tests_error_under_discovery_fails(self):
        d = self.suite(test_a=ONE, test_lt="""
            import unittest

            class Good(unittest.TestCase):
                def test_good(self):
                    pass

            def load_tests(loader, tests, pattern):
                if pattern is not None:
                    raise RuntimeError("only under discovery")
                return tests
            """)
        self.same_verdict(d, expect=1)

    def test_a_module_skipped_at_import_passes(self):
        d = self.suite(test_a=ONE, test_skip="""
            import unittest
            raise unittest.SkipTest("not here")
            """)
        self.same_verdict(d, expect=0)

    def test_fixture_skips_pass_alongside_passing_tests(self):
        d = self.suite(test_a=ONE, test_cls="""
            import unittest

            class C(unittest.TestCase):
                @classmethod
                def setUpClass(cls):
                    raise unittest.SkipTest("class fixture")

                def test_x(self):
                    pass
            """, test_mod="""
            import unittest

            def setUpModule():
                raise unittest.SkipTest("module fixture")

            class M(unittest.TestCase):
                def test_y(self):
                    pass
            """)
        self.same_verdict(d, expect=0)

    def test_a_suite_of_only_fixture_skips_passes(self):
        d = self.suite(test_mod="""
            import unittest

            def setUpModule():
                raise unittest.SkipTest("module fixture")

            class M(unittest.TestCase):
                def test_y(self):
                    pass
            """)
        self.same_verdict(d, expect=0)

    def test_an_imported_test_class_runs_in_both_modules(self):
        d = self.suite(test_a="""
            import unittest

            class A(unittest.TestCase):
                def test_ok(self):
                    pass
            """, test_b="""
            from test_a import A
            """)
        s, p = self.same_verdict(d, expect=0)
        self.assertIn("Ran 2 tests", s.stderr)
        self.assertIn("2 tests in ", p.stdout)

    def test_an_imported_failing_class_fails_where_it_is_imported_too(self):
        # The class runs and fails twice in serial discovery, so a failure counts twice.
        d = self.suite(test_a=FAIL, test_b="from test_a import B\n")
        s, p = self.same_verdict(d, expect=1)
        self.assertIn("failures=2", s.stderr)
        self.assertIn("the whole suite runs in one process, since test classes of test_a run in 2", p.stdout)
        self.assertIn("failed: the whole suite", p.stdout)

    def test_an_imported_class_keeps_its_state_across_occurrences(self):
        # The second occurrence sees the first one's count in serial discovery and fails.
        d = self.suite(test_a="""
            import unittest

            COUNT = []

            class A(unittest.TestCase):
                def test_once(self):
                    COUNT.append(1)
                    self.assertEqual(len(COUNT), 1)
            """, test_b="from test_a import A\n", test_c=ONE)
        self.same_verdict(d, expect=1)
        self.same_verdict(d, "--jobs", "1", expect=1)

    def test_classes_of_one_module_share_its_state_across_modules(self):
        d = self.suite(shared="""
            import unittest

            COUNT = []

            class A(unittest.TestCase):
                def test_a(self):
                    COUNT.append(1)
                    self.assertEqual(len(COUNT), 1)

            class B(unittest.TestCase):
                def test_b(self):
                    COUNT.append(1)
                    self.assertEqual(len(COUNT), 1)
            """, test_one="from shared import A\n", test_two="from shared import B\n")
        self.same_verdict(d, expect=1)

    def test_a_custom_suite_sees_the_tests_run_before_it(self):
        d = self.suite(test_a=ONE, test_gen="""
            import unittest

            class Checking(unittest.TestSuite):
                def run(self, result, debug=False):
                    if result.testsRun > 0:
                        result.addError(self, (RuntimeError, RuntimeError("ran after others"), None))
                    return result

                def id(self):
                    return "test_gen.Checking"

                def __str__(self):
                    return "test_gen.Checking"

                def shortDescription(self):
                    return None

            def load_tests(loader, tests, pattern):
                return Checking()
            """)
        self.same_verdict(d, expect=1)

    def test_an_empty_custom_suite_still_runs(self):
        d = self.suite(test_a=ONE, test_gen="""
            import unittest

            class Checking(unittest.TestSuite):
                def run(self, result, debug=False):
                    result.testsRun += 1
                    result.addFailure(self, (AssertionError, AssertionError("checked"), None))
                    return result

                def id(self):
                    return "test_gen.Checking"

                def __str__(self):
                    return "test_gen.Checking"

                def shortDescription(self):
                    return None

            def load_tests(loader, tests, pattern):
                return Checking()
            """)
        self.same_verdict(d, expect=1)

    def test_a_default_suite_with_a_run_of_its_own_still_runs(self):
        # Validation finding F1: an ordinary TestSuite given its own run() is not inert.
        d = self.suite(test_a=ONE, test_z="""
            import unittest

            def fail():
                raise AssertionError("generated check must run")

            def load_tests(loader, tests, pattern):
                suite = unittest.TestSuite()
                def run(result, debug=False):
                    return unittest.TestSuite([unittest.FunctionTestCase(fail)]).run(result)
                suite.run = run
                return suite
            """)
        s, p = self.same_verdict(d, expect=1)
        self.assertIn("Ran 2 tests", s.stderr)
        self.assertIn("the whole suite runs in one process, since test_z has a suite with attributes of its own: run", p.stdout)
        self.assertIn("2 tests in 1 processes", p.stdout)

    def test_a_nonempty_default_suite_with_a_run_of_its_own_sees_the_tests_before_it(self):
        d = self.suite(test_a=ONE, test_z="""
            import unittest

            class T(unittest.TestCase):
                def test_x(self):
                    pass

            def load_tests(loader, tests, pattern):
                suite = unittest.TestSuite(tests)
                plain = suite.run
                def run(result, debug=False):
                    if result.testsRun == 0:
                        result.addError(suite, (RuntimeError, RuntimeError("ran first"), None))
                    return plain(result, debug)
                suite.run = run
                return suite
            """)
        # Serial discovery runs test_a first, so the hook sees one test and passes.
        s, p = self.same_verdict(d, expect=0)
        self.assertIn("the whole suite runs in one process", p.stdout)

    def test_a_suite_class_changed_at_import_runs_as_serial_discovery_does(self):
        d = self.suite(test_a=ONE, test_z="""
            import unittest

            plain = unittest.TestSuite.run

            def run(self, result, debug=False):
                if not self._tests:
                    result.addError(self, (RuntimeError, RuntimeError("an empty suite ran"), None))
                return plain(self, result, debug)

            unittest.TestSuite.run = run

            def load_tests(loader, tests, pattern):
                return unittest.TestSuite()
            """)
        s, p = self.same_verdict(d, expect=1)
        self.assertIn("the whole suite runs in one process, since unittest.TestSuite.run was changed", p.stdout)

    def test_a_repeated_occurrence_runs_each_time(self):
        d = self.suite(test_rep="""
            import unittest

            RUNS = []

            class R(unittest.TestCase):
                def test_x(self):
                    RUNS.append(1)
                    self.assertLessEqual(len(RUNS), 1, "ran a second time")

            def load_tests(loader, tests, pattern):
                return unittest.TestSuite([R("test_x"), R("test_x")])
            """)
        self.same_verdict(d, expect=1)

    def test_a_package_load_tests_decides_its_tests(self):
        d = self.suite(test_a=ONE)
        write(d, "pkg/__init__.py", """
            import os, unittest

            HOOK_RAN = False

            def load_tests(loader, tests, pattern):
                global HOOK_RAN
                HOOK_RAN = True
                here = os.path.dirname(__file__)
                return loader.discover(here, pattern="test_good*.py")
            """)
        write(d, "pkg/test_good.py", """
            import unittest
            import pkg

            class G(unittest.TestCase):
                def test_hook_ran(self):
                    self.assertTrue(pkg.HOOK_RAN)
            """)
        write(d, "pkg/test_other.py", FAIL)
        self.same_verdict(d, expect=0)

    def test_a_custom_suite_keeps_its_behaviour_when_a_split_is_asked(self):
        d = self.suite(test_a=ONE, test_custom="""
            import os, unittest

            class Ready(unittest.TestSuite):
                def run(self, result, debug=False):
                    os.environ["CUSTOM_SUITE_READY"] = "1"
                    return super().run(result, debug)

            class T(unittest.TestCase):
                def test_one(self):
                    self.assertEqual(os.environ.get("CUSTOM_SUITE_READY"), "1")

                def test_two(self):
                    self.assertEqual(os.environ.get("CUSTOM_SUITE_READY"), "1")

            def load_tests(loader, tests, pattern):
                return Ready(loader.loadTestsFromTestCase(T))
            """)
        s, p = self.same_verdict(d, "--split", "test_custom", expect=0)
        self.assertIn("the whole suite runs in one process, since test_custom has a suite of kind", p.stdout)

    def test_a_split_module_with_a_class_fixture_runs_whole(self):
        d = self.suite(test_fix="""
            import unittest

            class F(unittest.TestCase):
                @classmethod
                def setUpClass(cls):
                    cls.ready = True

                def test_one(self):
                    self.assertTrue(self.ready)

                def test_two(self):
                    self.assertTrue(self.ready)
            """)
        s, p = self.same_verdict(d, "--split", "test_fix", expect=0)
        self.assertIn("--split test_fix runs whole", p.stdout)
        self.assertIn("2 tests in 1 processes", p.stdout)

    def test_test_output_that_looks_like_a_summary_is_not_counted(self):
        d = self.suite(test_out="""
            import sys, unittest

            class O(unittest.TestCase):
                def test_prints(self):
                    sys.stderr.write("Ran 1 test in 0.001s\\n")
            """)
        s, p = self.same_verdict(d, expect=0)
        self.assertIn("1 tests in 1 processes", p.stdout)

    def test_an_empty_suite_exits_5(self):
        d = self.suite(test_none="VALUE = 1\n")
        self.same_verdict(d, expect=5)

    def test_a_hook_that_returns_an_empty_suite_exits_5(self):
        d = self.suite(test_empty="""
            import unittest

            class T(unittest.TestCase):
                def test_x(self):
                    pass

            def load_tests(loader, tests, pattern):
                return unittest.TestSuite()
            """)
        self.same_verdict(d, expect=5)

    def test_a_unit_that_dies_without_its_result_record_fails(self):
        # Serial discovery would exit 0 here, since the process ends before unittest reports.
        d = self.suite(test_a=ONE, test_die="""
            import os, sys, unittest

            class D(unittest.TestCase):
                def test_exit(self):
                    sys.stderr.write("Ran 1 test in 0.001s\\n\\nOK\\n")
                    sys.stderr.flush()
                    os._exit(0)
            """)
        p = self.run_runner(str(d))
        self.assertEqual(p.returncode, 1, p.stdout + p.stderr)
        self.assertIn("no result record", p.stdout)
        self.assertIn("failed: test_die", p.stdout)

    def test_failures_errors_skips_and_expected_failures_keep_their_verdicts(self):
        d = self.suite(test_kinds="""
            import unittest

            class K(unittest.TestCase):
                @unittest.skip("skipped")
                def test_skip(self):
                    pass

                @unittest.expectedFailure
                def test_xfail(self):
                    self.fail("expected")

                def test_ok(self):
                    pass
            """)
        self.same_verdict(d, expect=0)
        d = self.suite(test_u="""
            import unittest

            class U(unittest.TestCase):
                @unittest.expectedFailure
                def test_unexpected_success(self):
                    pass
            """)
        self.same_verdict(d, expect=1)
        d = self.suite(test_err="""
            import unittest

            class E(unittest.TestCase):
                def test_error(self):
                    raise RuntimeError("an error")
            """)
        self.same_verdict(d, expect=1)


LEAK_HEADER = """
import git_isolation  # noqa: F401
import subprocess, unittest
"""


class Leaks(Base):
    """The suite's process-leak check, copied unchanged, sees leaks from every worker."""

    def leak_suite(self, **modules):
        d = self.suite(**modules)
        for name in ("git_isolation.py", "test_zz_process_leaks.py"):
            shutil.copy(HERE / name, d / name)
        return d

    def leaks(self, *args, **modules):
        d = self.leak_suite(**modules)
        s, p = self.same_verdict(d, *args, expect=1)
        for out in (s.stderr, p.stdout):
            self.assertIn("processes left unwaited", out)
            self.assertIn("leaky_", out)
        return p

    def test_a_retained_unwaited_process_is_found(self):
        self.leaks(test_a=ONE, test_keep=LEAK_HEADER + """
KEPT = []

class K(unittest.TestCase):
    def test_keep(self):
        KEPT.append(leaky_start())

def leaky_start():
    return subprocess.Popen(["true"])
""")

    def test_a_discarded_unwaited_process_is_found(self):
        self.leaks(test_a=ONE, test_drop=LEAK_HEADER + """
class D(unittest.TestCase):
    def test_drop(self):
        leaky_start()

def leaky_start():
    subprocess.Popen(["true"])
""")

    def test_a_process_left_by_teardown_or_a_cleanup_is_found(self):
        self.leaks(test_a=ONE, test_td=LEAK_HEADER + """
KEPT = []

def leaky_start():
    KEPT.append(subprocess.Popen(["true"]))

class T(unittest.TestCase):
    @classmethod
    def tearDownClass(cls):
        leaky_start()

    def test_x(self):
        self.addCleanup(leaky_start)
""")

    def test_a_leak_from_a_split_test_is_found(self):
        p = self.leaks("--split", "test_sp", test_a=ONE, test_sp=LEAK_HEADER + """
class S(unittest.TestCase):
    def test_one(self):
        pass

    def test_two(self):
        leaky_start()

def leaky_start():
    subprocess.Popen(["true"])
""")
        self.assertIn("=== test_sp.S.test_two", p.stdout)

    def test_waited_processes_pass(self):
        d = self.leak_suite(test_a=ONE, test_wait=LEAK_HEADER + """
class W(unittest.TestCase):
    def test_run(self):
        subprocess.run(["true"], check=True)

    def test_wait(self):
        p = subprocess.Popen(["true"])
        p.wait()
""")
        self.same_verdict(d, "--split", "test_wait", expect=0)


HANG = """
import os, signal, subprocess, sys, threading, unittest
from pathlib import Path

PIDS = Path({pids!r})

def record(name, pid):
    (PIDS / f"{{name}}.{{pid}}").write_text("")

class H(unittest.TestCase):
    def test_hang(self):
        record("worker", os.getpid())
        # A child that ignores TERM and keeps the worker's output, and one in its own session.
        stubborn = subprocess.Popen(["sh", "-c", "trap '' TERM; while :; do sleep 1; done"])
        record("stubborn", stubborn.pid)
        alone = subprocess.Popen(["sleep", "1000"], start_new_session=True, stdout=subprocess.DEVNULL)
        record("session", alone.pid)
        threading.Event().wait()
"""


class Deadlines(Base):
    """Hung work ends in bounded time, owned processes die, and others live."""

    def setUp(self):
        self.pids = Path(tempfile.mkdtemp(prefix="unittest-parallel-pids-"))
        self.addCleanup(shutil.rmtree, self.pids, True)
        self.addCleanup(self.kill_recorded)
        self.sentinel = subprocess.Popen(["sleep", "1000"], start_new_session=True)
        self.addCleanup(self.end_sentinel)

    def end_sentinel(self):
        self.sentinel.kill()
        self.sentinel.wait(timeout=30)

    def recorded(self):
        return [int(p.name.split(".")[1]) for p in self.pids.iterdir()]

    def kill_recorded(self):
        for pid in self.recorded():
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass

    def hang_suite(self, *names):
        return self.suite(test_a=ONE, **{n: HANG.format(pids=str(self.pids)) for n in names})

    def assert_owned_gone(self, count):
        pids = self.recorded()
        self.assertEqual(len(pids), count, sorted(p.name for p in self.pids.iterdir()))
        alive = [pid for pid in pids if not process_checks.ended(pid, seconds=10)]
        self.assertEqual(alive, [])
        self.assertIsNone(self.sentinel.poll())

    def test_a_hung_test_is_stopped_at_the_unit_deadline(self):
        d = self.hang_suite("test_hang")
        t0 = time.monotonic()
        p = self.run_runner("--unit-timeout", "4", "--grace", "1", str(d), timeout=60)
        self.assertLess(time.monotonic() - t0, 30)
        self.assertEqual(p.returncode, 1, p.stdout + p.stderr)
        self.assertIn("test_hang: deadline of 4 s passed", p.stdout)
        self.assertIn("failed: test_hang", p.stdout)
        self.assert_owned_gone(3)

    def test_a_hung_discovery_is_stopped_at_its_deadline(self):
        d = self.suite(test_a=ONE, test_slow=f"""
            import os, threading
            from pathlib import Path
            (Path({str(self.pids)!r}) / f"discover.{{os.getpid()}}").write_text("")
            threading.Event().wait()
            """)
        t0 = time.monotonic()
        p = self.run_runner("--discover-timeout", "4", "--grace", "1", str(d), timeout=60)
        self.assertLess(time.monotonic() - t0, 30)
        self.assertEqual(p.returncode, 1, p.stdout + p.stderr)
        self.assertIn("discovery: deadline of 4 s passed", p.stdout)
        self.assert_owned_gone(1)

    def interrupt(self, sig):
        d = self.hang_suite("test_h1", "test_h2")
        p = subprocess.Popen([sys.executable, str(RUNNER), "--jobs", "3", "--grace", "1", str(d)],
                             stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        try:
            end = time.monotonic() + 60
            while len(self.recorded()) < 6 and time.monotonic() < end:
                time.sleep(0.1)
            self.assertEqual(len(self.recorded()), 6, sorted(p.name for p in self.pids.iterdir()))
            t0 = time.monotonic()
            p.send_signal(sig)
            out, _ = p.communicate(timeout=30)
            self.assertLess(time.monotonic() - t0, 20)
        finally:
            if p.poll() is None:
                p.kill()
                p.communicate()
        self.assertEqual(p.returncode, 128 + sig, out)
        self.assertIn("interrupted", out)
        self.assert_owned_gone(6)

    def test_an_interrupt_stops_every_active_unit(self):
        self.interrupt(signal.SIGINT)

    def test_a_terminate_stops_every_active_unit(self):
        self.interrupt(signal.SIGTERM)

    def test_units_end_when_the_runner_is_killed(self):
        d = self.hang_suite("test_hang")
        p = subprocess.Popen([sys.executable, str(RUNNER), "--grace", "1", str(d)],
                             stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            end = time.monotonic() + 60
            while len(self.recorded()) < 3 and time.monotonic() < end:
                time.sleep(0.1)
            self.assertEqual(len(self.recorded()), 3, sorted(p.name for p in self.pids.iterdir()))
        finally:
            p.kill()
            p.wait(timeout=30)
        self.assert_owned_gone(3)

    def test_kept_records_hold_the_plan_and_each_units_result(self):
        d = self.suite(test_a=ONE, test_b=PASS)
        keep = Path(tempfile.mkdtemp(prefix="unittest-parallel-keep-"))
        self.addCleanup(shutil.rmtree, keep, True)
        p = self.run_runner("--keep", str(keep), str(d))
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        [run] = keep.iterdir()
        self.assertTrue((run / "plan.json").exists())
        self.assertEqual(len(list(run.glob("record-*.json"))), 2)

    def test_a_deadline_stops_the_run_and_admits_no_more_units(self):
        marker = self.pids.parent / f"{self.pids.name}-marker"
        self.addCleanup(lambda: marker.unlink(missing_ok=True))
        # The hung module is the larger file, so it starts first.
        d = self.suite(test_hang=HANG.format(pids=str(self.pids)), test_mark=f"""
            import unittest
            from pathlib import Path

            class M(unittest.TestCase):
                def test_mark(self):
                    Path({str(marker)!r}).write_text("")
            """)
        p = self.run_runner("--jobs", "1", "--unit-timeout", "4", "--grace", "1", str(d), timeout=60)
        self.assertEqual(p.returncode, 1, p.stdout + p.stderr)
        self.assertIn("not started after a deadline: test_mark", p.stdout)
        self.assertFalse(marker.exists())
        self.assert_owned_gone(3)

    def test_a_deadline_also_holds_back_the_absorbing_units(self):
        marker = self.pids.parent / f"{self.pids.name}-marker"
        self.addCleanup(lambda: marker.unlink(missing_ok=True))
        d = self.suite(test_hang=HANG.format(pids=str(self.pids)), test_zz_last=f"""
            import unittest
            from pathlib import Path

            def parallel_absorb(exports):
                pass

            class L(unittest.TestCase):
                def test_mark(self):
                    Path({str(marker)!r}).write_text("")
            """)
        p = self.run_runner("--unit-timeout", "4", "--grace", "1", str(d), timeout=60)
        self.assertEqual(p.returncode, 1, p.stdout + p.stderr)
        self.assertIn("not started after a deadline: test_zz_last", p.stdout)
        self.assertFalse(marker.exists())
        self.assert_owned_gone(3)

    def test_discovery_ends_when_the_runner_is_killed(self):
        d = self.suite(test_a=ONE, test_slow=f"""
            import os, threading
            from pathlib import Path
            (Path({str(self.pids)!r}) / f"discover.{{os.getpid()}}").write_text("")
            threading.Event().wait()
            """)
        p = subprocess.Popen([sys.executable, str(RUNNER), "--grace", "1", str(d)],
                             stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            end = time.monotonic() + 60
            while not self.recorded() and time.monotonic() < end:
                time.sleep(0.1)
            self.assertEqual(len(self.recorded()), 1)
        finally:
            p.kill()
            p.wait(timeout=30)
        self.assert_owned_gone(1)

    def test_deadlines_must_be_positive_and_finite(self):
        d = self.suite(test_a=ONE)
        for opt in ("--unit-timeout", "--discover-timeout", "--grace"):
            for value in ("0", "inf", "nan"):
                p = self.run_runner(opt, value, str(d))
                self.assertEqual(p.returncode, 2, opt + value + p.stderr)


class Identity(unittest.TestCase):
    """The process search keeps each process's identity, so a reused pid is never signalled."""

    def setUp(self):
        spec = importlib.util.spec_from_file_location("unittest_parallel_under_test", RUNNER)
        self.rp = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.rp)

    def test_a_reused_root_pid_is_not_signalled(self):
        # Root 4000001 started at 10 and its child 4000002 at 11. During the grace period both
        # pids come to name new processes, started at 50 and 51, which must be left alone.
        tables = [{4000001: (1, 4000001, 4000001, 10), 4000002: (4000001, 4000001, 4000001, 11)}] * 2
        later = {4000001: (1, 4000001, 4000001, 50), 4000002: (4000001, 4000001, 4000001, 51)}
        sent = []
        current = []

        def table():
            current[:] = [tables.pop(0) if tables else later]
            return current[0]

        def kill(pid, sig):
            # The start time of the process the pid named when the runner last looked.
            sent.append((pid, sig, current[0][pid][3]))

        with mock.patch.object(self.rp, "_process_table", table), mock.patch.object(self.rp.os, "kill", kill):
            self.rp.stop_all({4000001: 10}, 0.2)
        self.assertTrue(sent)
        self.assertEqual([(pid, sig) for pid, sig, start in sent if start >= 50], [])
        self.assertEqual({pid for pid, sig, _ in sent if sig == signal.SIGTERM}, {4000001, 4000002})

    def test_a_root_whose_pid_names_another_process_owns_nothing(self):
        table = {4000001: (1, 4000001, 4000001, 50), 4000002: (4000001, 4000001, 4000001, 51)}
        self.assertEqual(self.rp._owned({4000001: 10}, table), {})


if __name__ == "__main__":
    unittest.main()
