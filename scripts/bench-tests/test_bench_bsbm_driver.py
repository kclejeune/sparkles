"""Bound BSBM driver cells without downloads, engines or a Java installation."""

import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest


ROOT = Path(__file__).resolve().parents[2]
DRIVER = ROOT / "scripts/bench-bsbm/driver.py"


class BsbmDriverTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="bsbm driver ")
        self.addCleanup(self.tmp.cleanup)
        self.work = Path(self.tmp.name)
        self.xml = self.work / "driver.xml"
        self.out = self.work / "qmph.json"
        self.fake = self.work / "fake java"
        self.fake.write_text("""#!/usr/bin/env python3
import json, os, pathlib, signal, subprocess, sys, time
mode = sys.argv[-1].rsplit('/', 1)[-1]
if mode != 'child':
    pathlib.Path(os.environ['DRIVER_PID']).write_text(str(os.getpid()))
if mode == 'child':
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    pathlib.Path(os.environ['CHILD_PID']).write_text(str(os.getpid()))
    time.sleep(100)
elif mode in ('tree', 'tree-exits'):
    def terminated(*_):
        pathlib.Path(os.environ['TERM_SEEN']).write_text('term')
        if mode == 'tree-exits':
            sys.exit(0)
    signal.signal(signal.SIGTERM, terminated)
    subprocess.Popen([sys.executable, __file__, 'child'])
    time.sleep(100)
elif mode == 'hang':
    time.sleep(100)
elif mode == 'failure':
    sys.exit(7)
elif mode == 'exit137':
    sys.exit(137)
else:
    xml = pathlib.Path(sys.argv[sys.argv.index('-o') + 1])
    if mode == 'invalidxml':
        xml.write_text('<unfinished')
    else:
        xml.write_text('<bsbm><querymix><qmph>123</qmph><querymixruns>500</querymixruns>'
                       '<actualtotalruntime>2</actualtotalruntime></querymix></bsbm>')
    with open(os.environ['CALLS'], 'a') as f:
        f.write(json.dumps(sys.argv[1:]) + '\\n')
""")
        self.fake.chmod(0o755)
        self.env = dict(os.environ, CALLS=str(self.work / "calls"), CHILD_PID=str(self.work / "child-pid"),
                        DRIVER_PID=str(self.work / "driver-pid"), TERM_SEEN=str(self.work / "term-seen"))

    def command(self, mode, timeout="0.5"):
        return [sys.executable, "-I", str(DRIVER), "--timeout", timeout, "--grace", "0.2",
                "--cwd", str(self.work), "--xml", str(self.xml), "--out", str(self.out),
                "--engine", "fake", "--clients", "1", "--name", "bi-mix", "--",
                sys.executable, str(self.fake), "-o", str(self.xml), mode]

    def run_driver(self, mode, timeout="0.5"):
        started = time.monotonic()
        process = subprocess.run(self.command(mode, timeout), env=self.env,
                                 capture_output=True, text=True, timeout=10)
        self.assertEqual(process.returncode, 0, process.stderr)
        self.assertLess(time.monotonic() - started, 5)
        return json.loads(self.out.read_text())["fake"]["1"]

    def test_success_preserves_complete_run(self):
        entry = self.run_driver("success")
        self.assertEqual(entry["status"], "ok")
        self.assertEqual(entry["runs"], 500)
        self.assertEqual(entry["qmph"], 123)

    def test_failures_and_ordinary_exit_137_are_errors(self):
        for mode in ("failure", "exit137", "invalidxml"):
            with self.subTest(mode=mode):
                entry = self.run_driver(mode)
                self.assertEqual(entry["status"], "error")
                self.assertIsNone(entry["qmph"])

    def test_wall_timeout_is_explicit(self):
        entry = self.run_driver("hang")
        self.assertEqual(entry["status"], "timeout")
        self.assertIsNone(entry["qmph"])
        self.assertIn("wall budget", entry["status_reasons"][0])

    def assert_child_stopped(self):
        marker = Path(self.env["CHILD_PID"])
        self.assertTrue(marker.exists(), "the fake driver must start its child before its deadline")
        pid = int(marker.read_text())

        def cleanup():
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass

        self.addCleanup(cleanup)
        # An orphan can remain a zombie until init reaps it, but it cannot keep running.
        for _ in range(20):
            status = subprocess.run(["ps", "-o", "stat=", "-p", str(pid)], capture_output=True, text=True)
            if not status.stdout.strip() or status.stdout.lstrip().startswith("Z"):
                return
            time.sleep(0.05)
        self.fail("driver child survived process-group termination")

    def test_sigkill_cleans_up_a_driver_and_child_that_ignore_term(self):
        entry = self.run_driver("tree", timeout="1")
        self.assertEqual(entry["status"], "timeout")
        self.assert_child_stopped()

    def test_sigkill_cleans_up_children_even_when_the_leader_exits_on_term(self):
        entry = self.run_driver("tree-exits", timeout="1")
        self.assertEqual(entry["status"], "timeout")
        self.assert_child_stopped()

    def test_outer_termination_cleans_up_the_driver_group(self):
        process = subprocess.Popen(self.command("tree", timeout="100"), env=self.env,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            marker = Path(self.env["CHILD_PID"])
            deadline = time.monotonic() + 5
            while not marker.exists() and time.monotonic() < deadline:
                time.sleep(0.01)
            self.assertTrue(marker.exists())
            process.terminate()
            _, error = process.communicate(timeout=5)
            self.assertEqual(process.returncode, 143, error)
            self.assert_child_stopped()
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=5)

    def test_outer_termination_during_timeout_grace_still_kills_the_group(self):
        command = self.command("tree", timeout="0.5")
        command[command.index("--grace") + 1] = "1"
        process = subprocess.Popen(command, env=self.env, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, text=True)
        try:
            marker = Path(self.env["TERM_SEEN"])
            deadline = time.monotonic() + 5
            while not marker.exists() and time.monotonic() < deadline:
                time.sleep(0.01)
            self.assertTrue(marker.exists(), "the timeout must enter its TERM grace wait")
            process.terminate()
            time.sleep(0.05)
            if process.poll() is None:
                process.terminate()
            self.assertEqual(process.wait(timeout=5), 143)
            self.assert_child_stopped()
            self.assertFalse(self.out.exists(), "an interrupted cell must not publish throughput")
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=5)
            pid_file = Path(self.env["DRIVER_PID"])
            if pid_file.exists():
                try:
                    os.killpg(int(pid_file.read_text()), signal.SIGKILL)
                except ProcessLookupError:
                    pass
            process.communicate(timeout=5)

    def test_invalid_wall_budgets_are_rejected(self):
        for value in ("0", "-1", "nan", "inf", "oops"):
            with self.subTest(value=value):
                process = subprocess.run(self.command("success", value), capture_output=True, text=True)
                self.assertEqual(process.returncode, 2)
                self.assertFalse(self.out.exists())

    def test_shell_validates_both_limits_before_downloads(self):
        for name in ("BSBM_MAX_TIME", "BSBM_KILL_AFTER"):
            for value in ("0", "-1", "nan", "inf", "oops"):
                with self.subTest(name=name, value=value):
                    work = self.work / "uncreated"
                    process = subprocess.run(["bash", str(ROOT / "scripts/bench-bsbm.sh"), "2000", str(work)],
                                             env=self.env | {"SPARKLES_ALLOW_GPL_BENCHMARK": "1", name: value},
                                             capture_output=True, text=True, timeout=5)
                    self.assertEqual(process.returncode, 2)
                    self.assertIn(name + " must be a positive", process.stderr)
                    self.assertFalse(work.exists())

    def test_shell_loop_continues_and_retains_mix_counts(self):
        source = (ROOT / "scripts/bench-bsbm.sh").read_text()
        function = source[source.index("bsbm_qmph() {\n"):source.index("\nSUITE_EXTRA=bsbm_qmph")]
        (self.work / "bsbm").mkdir()
        (self.work / "results").mkdir()
        (self.work / "logs").mkdir()
        extra = self.work / "results/extra.json"
        extra.write_text(json.dumps({"settings": {"preserved": True}, "results": []}))
        script = function + """
declare -A NAME=([slow]=slow [fast]=fast) URL=([slow]=localhost/hang [fast]=localhost/success)
ENGINES='slow fast' MIXES=std CLIENTS='1 4' USECASE=bi UCF=bi.txt RDFXML_NRS='' DEAD_ENGINES=''
BSBM_WARMUP=50 BSBM_RUNS=500 DRIVER_TIMEOUT_MS=300000 BSBM_MAX_TIME=0.5 BSBM_KILL_AFTER=0.2
revive() { echo "$1 $2" >> revived; }
bsbm_qmph
"""
        process = subprocess.run(["bash", "-euc", script], cwd=self.work,
                                 env=self.env | {"ROOT": str(ROOT), "WORK": str(self.work), "JAVA": str(self.fake)},
                                 capture_output=True, text=True, timeout=10)
        self.assertEqual(process.returncode, 0, process.stderr)
        data = json.loads((self.work / "results/qmph.json").read_text())
        self.assertEqual({c: e["status"] for c, e in data["slow"].items()}, {"1": "timeout", "4": "timeout"})
        self.assertEqual({c: e["status"] for c, e in data["fast"].items()}, {"1": "ok", "4": "ok"})
        self.assertEqual(len((self.work / "revived").read_text().splitlines()), 4)
        self.assertIn("slow: bi-mix x1 (whole driver wall budget 0.5s)", process.stdout)
        self.assertIn("fast: bi-mix x4 (whole driver wall budget 0.5s)", process.stdout)
        expected = {"preserved": True, "bsbmMaxTimeSeconds": 0.5, "bsbmKillAfterSeconds": 0.2,
                    "bsbmWarmup": 50, "bsbmRuns": 500, "bsbmClients": [1, 4]}
        self.assertEqual(json.loads(extra.read_text())["settings"], expected)
        self.assertEqual(json.loads(extra.read_text())["results"], [])
        record = self.work / "record.json"
        build = subprocess.run([sys.executable, str(ROOT / "scripts/bench-record.py"), "build",
                                "--suite", "bsbm-bi", "--scale", "tiny", "--results", str(extra.parent),
                                "--out", str(record)], capture_output=True, text=True)
        self.assertEqual(build.returncode, 0, build.stderr)
        settings = json.loads(record.read_text())["settings"]
        self.assertEqual({name: settings[name] for name in expected}, expected)
        for line in (self.work / "calls").read_text().splitlines():
            args = json.loads(line)
            self.assertEqual(args[args.index("-runs") + 1], "500")
            self.assertEqual(args[args.index("-w") + 1], "50")


if __name__ == "__main__":
    unittest.main()
