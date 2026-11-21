"""Exercise the batch gate's rerun of failed tests (scripts/gate/remote/test_retry.py)."""
import git_isolation  # noqa: F401, first, so no fixture sees the host Git settings
import importlib.util
import json
import os
import signal
import subprocess
import sys
import tempfile
import textwrap
import time
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


DEFAULT_STATES = {"prepare": "pass 10s", "lint": "pass 100s", "test": "FAIL 900s (exit 1) log"}


def gate_out(d, *, failed=FAILED, summary=None, manifest=None, states=None, rows=None, selection="ci lint test",
             ci_exit="1", ci_log="CI failed: test"):
    """A gate's out directory after a ci step that failed on the test task. STATES are the
    state files, and ROWS the summary.txt rows, which default to the states."""
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
    states = DEFAULT_STATES if states is None else states
    for k, v in states.items():
        (rec / "state" / k.replace(":", "_")).write_text(v + "\n")
    rows = states if rows is None else rows
    (rec / "summary.txt").write_text("CI started 10:00:00, 900s elapsed\n\n"
                                     + "".join(f"{k:<20} {v}\n" for k, v in rows.items()))
    if summary is None:
        summary = f"     Summary [ 900.0s] 9000 tests run: 8998 passed (60 slow), {len(failed)} failed, 54 skipped"
    (rec / "logs/test.log").write_text("FAIL [ 1s] x\n" + summary + "\n")
    if selection is not None:
        (out / "selection.txt").write_text(selection + "\nconformance\nspill\npackages sparkles-core\n")
    if ci_exit is not None:
        (out / "ci.exit").write_text(ci_exit + "\n")
    (out / "ci.log").write_text("==== test (last 40 lines)\nerror\n\n" + ci_log + "\n")
    return out


def clean(_commit):
    """A worktree check that always finds the worktree clean."""


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
            "a failed prerequisite": dict(states={"prepare": "FAIL 1s", "lint": "pass 1s", "test": "FAIL 2s"}),
            "a test task that passed": dict(states={"prepare": "pass", "lint": "pass", "test": "pass 2s"}),
            "a killed ci step": dict(ci_exit="137"),
            "an interrupted ci step": dict(ci_exit="130"),
            "no ci exit": dict(ci_exit=None),
            "a log that does not end with the test failure": dict(ci_log="CI failed: lint test"),
            "a log cut short": dict(ci_log="PASS lint 100s"),
            "a selected task left waiting": dict(selection="ci lint test ui:test", rows=dict(DEFAULT_STATES, **{"ui:test": "waiting"})),
            "a selected task left running": dict(selection="ci lint test ui:test",
                                                 rows=dict(DEFAULT_STATES, **{"ui:test": "running since 10:01:00"})),
            "a selected task stopped": dict(selection="ci lint test ui:test", states=dict(DEFAULT_STATES, **{"ui:test": "stopped"})),
            "a selected task skipped": dict(selection="ci lint test ui:test", states=dict(DEFAULT_STATES, **{"ui:test": "skipped"})),
            "a selected task missing from the summary": dict(selection="ci lint test ui:test"),
            "a summary row that disagrees with its state": dict(rows=dict(DEFAULT_STATES, lint="FAIL 1s")),
            "a state file missing": dict(states={"prepare": "pass", "test": "FAIL 2s"},
                                         rows=DEFAULT_STATES),
            "no selection": dict(selection=None),
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

    def test_boxington_lines_after_the_failure_line_are_ignored(self):
        with tempfile.TemporaryDirectory() as d:
            out = gate_out(d, ci_log="CI failed: test\nmbx[gc]: evicted 282 objects\nmbx[savings]: 1 TiB")
            self.assertTrue(tr.eligible(out, COMMIT, 8)[0])
        with tempfile.TemporaryDirectory() as d, self.assertRaises(tr.Decline):
            tr.eligible(gate_out(d, ci_log="CI failed: test\nsomething else"), COMMIT, 8)

    def test_a_missing_manifest_declines(self):
        with tempfile.TemporaryDirectory() as d:
            out = gate_out(d)
            (out / "ci-receipts/junit/manifest.json").unlink()
            with self.assertRaises(tr.Decline):
                tr.eligible(out, COMMIT, 8)

    def test_the_full_list_is_read_from_ci_sh(self):
        full = tr.full_tasks(ROOT / "scripts/ci.sh")
        self.assertIn("test", full)
        self.assertIn("fmt:check", full)
        with tempfile.TemporaryDirectory() as d:
            for selection in ("ci all", "every check with --force (full gate)", "ci"):
                (Path(d) / "selection.txt").write_text(selection + "\n")
                self.assertEqual(tr.selected_tasks(Path(d), ROOT / "scripts/ci.sh"), full, selection)

    def test_a_full_run_is_eligible_when_every_task_passed(self):
        full = tr.full_tasks(ROOT / "scripts/ci.sh")
        states = {"prepare": "pass 1s", **{t: ("FAIL 2s" if t == "test" else "pass 1s") for t in full}}
        with tempfile.TemporaryDirectory() as d:
            cwd = os.getcwd()
            os.chdir(ROOT)
            try:
                self.assertTrue(tr.eligible(gate_out(d, states=states, selection="ci all"), COMMIT, 8)[0])
            finally:
                os.chdir(cwd)

    def test_filters_are_exact(self):
        self.assertEqual(tr.filterset("sparkles-core::w3c", "a::b"), "binary_id(=sparkles-core::w3c) & test(=a::b)")
        with self.assertRaises(tr.Decline):
            tr.filterset("b", "case(1)")


class Rerun(unittest.TestCase):
    def run_main(self, d, outcome, doctest_exit=0, rebuild=False, extra=(), out=None, check_worktree=clean,
                 target_tmp=None, envs=None):
        """Run main with a fake runner. OUTCOME maps a test name to the cases its rerun's
        report lists and the exit code."""
        out = out or gate_out(d)
        junit_path = Path(d) / "target-junit.xml"
        calls = []

        def runner(cmd, log, seconds, env=None):
            calls.append(cmd)
            if envs is not None:
                envs.append(env)
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
                code = tr.main(["--out", str(out), "--commit", COMMIT, *extra], runner=runner, junit_path=junit_path,
                               target_tmp=target_tmp or Path(d) / "target-tmp", check_worktree=check_worktree)
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

    def test_a_rebuild_is_a_cache_miss(self):
        self.assertFalse(tr.rebuilt("   Compiling sparkles-core v0.1.0\nmbx[cache]: 3 hits, 0 misses; 0 B\n"))
        self.assertTrue(tr.rebuilt("   Compiling sparkles-core v0.1.0\nmbx[cache]: 3 hits, 1 miss; 0 B\n"))
        self.assertTrue(tr.rebuilt("   Compiling sparkles-core v0.1.0\n"))
        self.assertFalse(tr.rebuilt("   Compiling serde v1.0.0\n    Finished\n"))

    def test_an_ineligible_failure_runs_nothing(self):
        with tempfile.TemporaryDirectory() as d:
            out = gate_out(d, states={"lint": "FAIL", "test": "FAIL"})
            calls = []
            code = tr.main(["--out", str(out), "--commit", COMMIT], runner=lambda *a, **k: calls.append(a) or 0,
                           check_worktree=clean)
            self.assertEqual(code, 1)
            self.assertEqual(calls, [])
            self.assertIn("not rerun", (out / "test-retry.txt").read_text())

    def test_an_interrupted_ci_run_is_not_forgiven_even_when_the_reruns_pass(self):
        # ci.sh was killed after the test task failed, before ui:test and licenses:check
        # started. Both reruns and the doctests would pass.
        with tempfile.TemporaryDirectory() as d:
            out = gate_out(d, selection="ci lint test ui:test licenses:check", ci_exit="137",
                           ci_log="FAIL test 900s  log: x",
                           rows=dict(DEFAULT_STATES, **{"ui:test": "waiting", "licenses:check": "waiting"}))
            code, out, calls = self.run_main(d, self.passing(), out=out)
            self.assertEqual(code, 1)
            self.assertEqual(calls, [])
            self.assertFalse((out / "ci.forgiven").exists())

    def test_a_worktree_changed_by_the_first_run_is_not_rerun(self):
        def dirty(_commit):
            raise tr.Decline("the worktree has changed or untracked files: fixture.ttl")
        with tempfile.TemporaryDirectory() as d:
            code, out, calls = self.run_main(d, self.passing(), check_worktree=dirty)
            self.assertEqual(code, 1)
            self.assertEqual(calls, [])
            self.assertFalse((out / "ci.forgiven").exists())

    def test_a_worktree_changed_by_a_rerun_is_not_forgiven(self):
        seen = []

        def dirty_after_reruns(_commit):
            seen.append(1)
            if len(seen) > 1:
                raise tr.Decline("the worktree has changed or untracked files: fixture.ttl")
        with tempfile.TemporaryDirectory() as d:
            code, out, calls = self.run_main(d, self.passing(), check_worktree=dirty_after_reruns)
            self.assertEqual(code, 1)
            self.assertTrue(calls)
            self.assertFalse((out / "ci.forgiven").exists())
            self.assertIn("after the reruns", (out / "test-retry.txt").read_text())

    def test_each_rerun_gets_new_scratch(self):
        with tempfile.TemporaryDirectory() as d:
            target_tmp = Path(d) / "target-tmp"
            (target_tmp / "power-loss").mkdir(parents=True)
            (target_tmp / "power-loss/left-by-the-first-run").write_text("x")
            (target_tmp / "marker").write_text("x")
            envs = []
            seen = []
            o = self.passing()

            def watch(_commit):
                seen.append(sorted(p.name for p in target_tmp.iterdir()))
            code, out, _ = self.run_main(d, o, target_tmp=target_tmp, envs=envs, check_worktree=watch)
            self.assertEqual(code, 0)
            self.assertEqual(list(target_tmp.iterdir()), [])
            tmps = [e["TMPDIR"] for e in envs]
            self.assertEqual(len(set(tmps)), len(tmps))
            for t in tmps:
                self.assertTrue(t.startswith(str((out / "test-retry").resolve())), t)
                self.assertFalse(Path(t).exists(), t)

    def test_a_signal_during_a_rerun_removes_its_scratch(self):
        with tempfile.TemporaryDirectory() as d:
            envs = []
            o = self.passing()
            out = gate_out(d)

            def runner(cmd, log, seconds, env=None):
                envs.append(env)
                Path(env["TMPDIR"], "store").mkdir()
                raise SystemExit(143)
            cwd = os.getcwd()
            os.chdir(ROOT)
            try:
                with self.assertRaises(SystemExit):
                    tr.main(["--out", str(out), "--commit", COMMIT], runner=runner,
                            junit_path=Path(d) / "j.xml", target_tmp=Path(d) / "tt", check_worktree=clean)
            finally:
                os.chdir(cwd)
            self.assertFalse(Path(envs[0]["TMPDIR"]).exists())
            self.assertFalse((out / "ci.forgiven").exists())

    def test_a_real_check_of_the_worktree(self):
        with tempfile.TemporaryDirectory() as d:
            env = dict(os.environ, GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_NOSYSTEM="1")
            git = lambda *a: subprocess.run(["git", "-C", d, *a], check=True, capture_output=True, text=True, env=env)
            git("init", "-q")
            Path(d, ".gitignore").write_text("target/\n")
            Path(d, "a").write_text("a\n")
            git("add", ".")
            git("-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "commit", "-qm", "c")
            head = git("rev-parse", "HEAD").stdout.strip()
            cwd = os.getcwd()
            os.chdir(d)
            try:
                tr.worktree_clean(head)
                Path(d, "target").mkdir()
                Path(d, "target/ignored").write_text("x")
                tr.worktree_clean(head)
                with self.assertRaises(tr.Decline):
                    tr.worktree_clean("b" * 40)
                Path(d, "fixture.ttl").write_text("x")
                with self.assertRaises(tr.Decline):
                    tr.worktree_clean(head)
                Path(d, "fixture.ttl").unlink()
                Path(d, "a").write_text("changed\n")
                with self.assertRaises(tr.Decline):
                    tr.worktree_clean(head)
            finally:
                os.chdir(cwd)


class Timeout(unittest.TestCase):
    """run() against a real process tree."""

    def test_a_timeout_stops_descendants_in_other_process_groups(self):
        with tempfile.TemporaryDirectory() as d:
            beat = Path(d) / "beat"
            pidfile = Path(d) / "grandchild.pid"
            child = textwrap.dedent(f"""
                import os, signal, time
                os.setpgid(0, 0)
                signal.signal(signal.SIGTERM, signal.SIG_IGN)
                open({str(pidfile)!r}, "w").write(str(os.getpid()))
                while True:
                    open({str(beat)!r}, "a").write("x")
                    time.sleep(0.05)
            """)
            parent = textwrap.dedent(f"""
                import subprocess, sys, time
                subprocess.Popen([sys.executable, "-c", {child!r}])
                time.sleep(60)
            """)
            with mock.patch.object(tr, "GRACE", 1):
                t0 = time.monotonic()
                code = tr.run([sys.executable, "-c", parent], Path(d) / "log", 2)
                self.assertLess(time.monotonic() - t0, 20)
            self.assertIsNone(code)
            pid = int(pidfile.read_text())
            # The grandchild left the parent's process group and ignored SIGTERM, and is gone.
            # Its new parent reaps it, which a loaded host can take a moment to schedule, so a
            # zombie counts as gone.
            end = time.monotonic() + 10
            while True:
                try:
                    state = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[0]
                except FileNotFoundError:
                    break
                if state == "Z":
                    break
                self.assertLess(time.monotonic(), end, f"process {pid} is still running")
                time.sleep(0.05)
            size = beat.stat().st_size
            time.sleep(0.3)
            self.assertEqual(beat.stat().st_size, size)
            self.assertIn("stopped after 2 s", (Path(d) / "log").read_text())

    def discovery(self, d, signal_helper):
        """Run main with passing reruns and a package discovery that leaves a child in
        another process group and blocks. With SIGNAL_HELPER, SIGTERM reaches the helper
        during discovery; otherwise discovery runs out of time."""
        pidfile = Path(d) / "discovery-child.pid"
        script = textwrap.dedent(f"""
            import os, subprocess, sys, time
            p = subprocess.Popen([sys.executable, "-c", "import os, signal, time; os.setpgid(0, 0); "
                                  "signal.signal(signal.SIGTERM, signal.SIG_IGN); time.sleep(120)"])
            open({str(pidfile)!r}, "w").write(str(p.pid))
            time.sleep(120)
        """)
        out = gate_out(Path(d) / "out")
        junit_path = Path(d) / "target-junit.xml"

        def runner(cmd, log, seconds, env=None):
            name = cmd[cmd.index("-E") + 1].split("test(=")[1].rstrip(")")
            b = next(b for b, n in FAILED if n == name)
            junit_path.write_text(junit([(b, name, "pass")]))
            log.write_text("Summary\n")
            return 0

        def discover(cmd, seconds, env=None):
            if signal_helper:
                def later():
                    while not pidfile.exists():
                        time.sleep(0.05)
                    os.kill(os.getpid(), signal.SIGTERM)
                import threading
                threading.Thread(target=later, daemon=True).start()
                seconds = 60
            else:
                seconds = 2
            return tr.run_capture([sys.executable, "-c", script], seconds, env=env)

        old_handler = signal.getsignal(signal.SIGTERM)
        try:
            with mock.patch.object(tr, "GRACE", 1):
                try:
                    code = tr.main(["--out", str(out), "--commit", COMMIT], runner=runner, junit_path=junit_path,
                                   target_tmp=Path(d) / "target-tmp", check_worktree=clean, discover=discover)
                except SystemExit as e:
                    code = e.code
        finally:
            signal.signal(signal.SIGTERM, old_handler)
        with self.assertRaises(ProcessLookupError):
            os.kill(int(pidfile.read_text()), 0)
        self.assertFalse((out / "ci.forgiven").exists())
        return code

    def test_a_discovery_that_runs_out_of_time_leaves_nothing_behind(self):
        with tempfile.TemporaryDirectory() as d:
            self.assertEqual(self.discovery(d, signal_helper=False), 1)

    def test_sigterm_during_discovery_leaves_nothing_behind(self):
        with tempfile.TemporaryDirectory() as d:
            self.assertEqual(self.discovery(d, signal_helper=True), 143)

    def test_a_command_that_ends_leaves_nothing_behind(self):
        with tempfile.TemporaryDirectory() as d:
            pidfile = Path(d) / "pid"
            parent = textwrap.dedent(f"""
                import subprocess, sys
                p = subprocess.Popen([sys.executable, "-c", "import os, time; os.setpgid(0, 0); time.sleep(60)"])
                open({str(pidfile)!r}, "w").write(str(p.pid))
            """)
            with mock.patch.object(tr, "GRACE", 1):
                self.assertEqual(tr.run([sys.executable, "-c", parent], Path(d) / "log", 30), 0)
            with self.assertRaises(ProcessLookupError):
                os.kill(int(pidfile.read_text()), 0)


if __name__ == "__main__":
    unittest.main()
