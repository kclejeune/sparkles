"""Exercise the batch gate's rerun of failed tests (scripts/gate/remote/test_retry.py)."""
import importlib.util
import json
import os
import tempfile
from pathlib import Path
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("test_retry", ROOT / "scripts/gate/remote/test_retry.py")
tr = importlib.util.module_from_spec(spec)
spec.loader.exec_module(tr)

COMMIT = "a" * 40
FAILED = [("sparkles-core", "store::backup::tests::captures"), ("sparkles-server::bin/sparkles", "http::obs")]


def junit(cases):
    """A JUnit report of CASES, each (binary, name, outcome) with outcome pass, fail or timeout."""
    suites = {}
    for b, n, o in cases:
        body = {"pass": "", "fail": '<failure type="test failure"/>', "timeout": '<failure type="timeout"/>',
                "flaky": "<flakyFailure/>"}[o]
        suites.setdefault(b, []).append(f'<testcase name="{n}" classname="{b}">{body}</testcase>')
    return "<testsuites>" + "".join(
        f'<testsuite name="{b}">{"".join(cs)}</testsuite>' for b, cs in suites.items()) + "</testsuites>"


def gate_out(d, *, failed=FAILED, summary=None, manifest=None, states=None):
    out = Path(d)
    rec = out / "ci-receipts"
    (rec / "junit/test/default").mkdir(parents=True)
    (rec / "state").mkdir()
    (rec / "logs").mkdir()
    cases = [(b, n, "fail") for b, n in failed] + [("sparkles-core", "ok", "pass"), ("sparkles-fmt", "f", "flaky")]
    (rec / "junit/test/default/junit.xml").write_text(junit(cases))
    m = {
        "version": 1, "task": "test", "source_commit": COMMIT, "source_dirty_status": "",
        "selection_packages": "sparkles-core sparkles-server", "selected": True, "status": "retained",
        "task_exit": 100,
        "reports": [{"profile": "default", "retained_path": "test/default/junit.xml",
                     "freshness": "new-or-changed-since-task-start"}],
    }
    m.update(manifest or {})
    (rec / "junit/manifest.json").write_text(json.dumps(m))
    for k, v in (states or {"prepare": "pass 10s", "lint": "pass 100s", "test": "FAIL 900s (exit 1) log"}).items():
        (rec / "state" / k).write_text(v + "\n")
    if summary is None:
        summary = f"     Summary [ 900.0s] 9000 tests run: 8998 passed (60 slow), {len(failed)} failed, 54 skipped"
    (rec / "logs/test.log").write_text("FAIL [ 1s] x\n" + summary + "\n")
    return out


class Eligibility(unittest.TestCase):
    def test_a_complete_run_with_few_failures_is_eligible(self):
        with tempfile.TemporaryDirectory() as d:
            failures, pkgs = tr.eligible(gate_out(d), COMMIT, 8)
        self.assertEqual(sorted(failures), sorted(FAILED))
        self.assertEqual(pkgs, "sparkles-core sparkles-server")

    def test_timeouts_count_as_failures(self):
        with tempfile.TemporaryDirectory() as d:
            s = "  Summary [1.0s] 10 tests run: 8 passed, 1 failed, 1 timed out, 2 skipped"
            self.assertEqual(len(tr.eligible(gate_out(d, summary=s), COMMIT, 8)[0]), 2)

    def test_declines(self):
        cases = {
            "a stale report": dict(manifest={"reports": [{"profile": "default", "retained_path": "test/default/junit.xml",
                                                          "freshness": "unchanged-since-task-start"}]}),
            "another commit": dict(manifest={"source_commit": "b" * 40}),
            "a passing task": dict(manifest={"task_exit": 0}),
            "a task that was not selected": dict(manifest={"selected": False}),
            "another failed step": dict(states={"prepare": "pass", "lint": "FAIL 1s", "test": "FAIL 2s"}),
            "a test task that passed": dict(states={"test": "pass 2s"}),
            "a cancelled run": dict(summary="  Summary [1.0s] 10/9000 tests run: 8 passed, 2 failed, 0 skipped"),
            "an unknown category": dict(summary="  Summary [1.0s] 10 tests run: 8 passed, 2 exec failed"),
            "no summary": dict(summary="error: build failed"),
            "a count mismatch": dict(summary="  Summary [1.0s] 10 tests run: 7 passed, 3 failed"),
            "no failure": dict(failed=[], summary="  Summary [1.0s] 10 tests run: 10 passed"),
            "too many failures": dict(failed=[("b", f"t{i}") for i in range(9)],
                                      summary="  Summary [1.0s] 20 tests run: 11 passed, 9 failed"),
        }
        for what, kw in cases.items():
            with tempfile.TemporaryDirectory() as d, self.subTest(what):
                with self.assertRaises(tr.Decline):
                    tr.eligible(gate_out(d, **kw), COMMIT, 8)

    def test_a_missing_manifest_declines(self):
        with tempfile.TemporaryDirectory() as d:
            out = gate_out(d)
            (out / "ci-receipts/junit/manifest.json").unlink()
            with self.assertRaises(tr.Decline):
                tr.eligible(out, COMMIT, 8)

    def test_filters_are_exact(self):
        self.assertEqual(tr.filterset("sparkles-core::w3c", "a::b"), "binary_id(=sparkles-core::w3c) & test(=a::b)")
        with self.assertRaises(tr.Decline):
            tr.filterset("b", "case(1)")


class Rerun(unittest.TestCase):
    def run_main(self, d, outcome, doctest_exit=0, rebuild=False, extra=()):
        """Run main with a fake runner. OUTCOME maps a test name to the cases its rerun's
        report lists and the exit code."""
        out = gate_out(d)
        junit_path = Path(d) / "target-junit.xml"
        calls = []

        def runner(cmd, log, seconds):
            calls.append(cmd)
            if "--doc" in cmd:
                log.write_text("doctests\n")
                return doctest_exit
            name = cmd[cmd.index("-E") + 1].split("test(=")[1].rstrip(")")
            cases, code = outcome[name]
            junit_path.write_text(junit(cases))
            log.write_text(("   Compiling sparkles-core v0.1.0\n" if rebuild else "") + "Summary\n")
            return code

        cwd = os.getcwd()
        os.chdir(ROOT)
        try:
            with mock.patch.object(tr.time, "monotonic", return_value=0):
                code = tr.main(["--out", str(out), "--commit", COMMIT, *extra], runner=runner, junit_path=junit_path)
        finally:
            os.chdir(cwd)
        return code, out, calls

    def passing(self):
        return {n: ([(b, n, "pass")], 0) for b, n in FAILED}

    def test_tests_that_pass_alone_are_forgiven(self):
        with tempfile.TemporaryDirectory() as d:
            code, out, calls = self.run_main(d, self.passing())
            self.assertEqual(code, 0)
            self.assertEqual(sorted((out / "ci.forgiven").read_text().splitlines()), sorted(f"{b} {n}" for b, n in FAILED))
            self.assertIn("forgiven", (out / "test-retry.txt").read_text())
            self.assertTrue((out / "test-retry/1-junit.xml").exists())
            self.assertEqual(sum("--doc" in c for c in calls), 1)
            doc = [c for c in calls if "--doc" in c][0]
            # sparkles-server has no library, so only sparkles-core has doctests.
            self.assertEqual(doc[-5:], ["mbx", "test", "-p", "sparkles-core", "--doc"])
            for c in calls:
                if "--doc" not in c:
                    self.assertIn("--no-capture", c)
                    self.assertEqual(c[c.index("--retries") + 1], "0")

    def test_a_test_that_fails_again_is_not_forgiven(self):
        o = self.passing()
        o[FAILED[1][1]] = ([(FAILED[1][0], FAILED[1][1], "fail")], 100)
        with tempfile.TemporaryDirectory() as d:
            code, out, _ = self.run_main(d, o)
            self.assertEqual(code, 1)
            self.assertFalse((out / "ci.forgiven").exists())

    def test_a_filter_that_runs_nothing_or_more_is_not_a_pass(self):
        for cases in ([], [(FAILED[0][0], FAILED[0][1], "pass"), ("x", "y", "pass")]):
            o = self.passing()
            o[FAILED[0][1]] = (cases, 0)
            with tempfile.TemporaryDirectory() as d:
                code, out, _ = self.run_main(d, o)
                self.assertEqual(code, 1, cases)
                self.assertFalse((out / "ci.forgiven").exists())

    def test_a_rebuild_or_a_failed_doctest_is_not_forgiven(self):
        with tempfile.TemporaryDirectory() as d:
            self.assertEqual(self.run_main(d, self.passing(), rebuild=True)[0], 1)
        with tempfile.TemporaryDirectory() as d:
            code, out, _ = self.run_main(d, self.passing(), doctest_exit=101)
            self.assertEqual(code, 1)
            self.assertFalse((out / "ci.forgiven").exists())

    def test_an_ineligible_failure_runs_nothing(self):
        with tempfile.TemporaryDirectory() as d:
            out = gate_out(d, states={"lint": "FAIL", "test": "FAIL"})
            calls = []
            code = tr.main(["--out", str(out), "--commit", COMMIT], runner=lambda *a: calls.append(a) or 0)
            self.assertEqual(code, 1)
            self.assertEqual(calls, [])
            self.assertIn("not rerun", (out / "test-retry.txt").read_text())


if __name__ == "__main__":
    unittest.main()
