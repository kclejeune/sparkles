"""Tests of scripts/request-insn.py: its statistics, its summary and its counters.

    python3 -m unittest discover -s scripts/bench-tests -p test_request_insn.py
"""

import importlib.util
import process_checks
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest

SCRIPTS = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
spec = importlib.util.spec_from_file_location("request_insn", os.path.join(SCRIPTS, "request-insn.py"))
ri = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ri)

COLS = (
    "round epoch variant query mode reps instr_u instr_k cycles_u task_clock_ns ctx_switches "
    "page_faults migrations covered window_ms exec_ms wall_ms sched_ns run_frac"
).split()


def write_samples(d, per_round):
    """per_round: list of (A values, B values) per round, two processes each, in counts per
    request of 10 requests. A finished run's manifest goes beside them."""
    with open(os.path.join(d, "manifest.json"), "w") as f:
        json.dump({"queries": ["q"], "modes": ["streaming"], "complete": True, "problems": []}, f)
    with open(os.path.join(d, "samples.tsv"), "w") as f:
        f.write("\t".join(COLS) + "\n")
        epoch = 0
        for rnd, (va, vb) in enumerate(per_round, 1):
            for v, x in (("A", va[0]), ("B", vb[0]), ("B", vb[1]), ("A", va[1])):
                epoch += 1
                f.write("\t".join(map(str, [rnd, epoch, v, "q", "streaming", 10, x * 10, "", x * 20, 1000, 5, 0, 0, 1, "1.0", "0.01", "0.1", 1000, "1.0"])) + "\n")
                f.write("\t".join(map(str, [rnd, epoch, v, "_idle", "-", 1, 100, "", 200, 10, 1, 0, 0, 1, "1.0", "nan", "nan", 10, "1.0"])) + "\n")


class Statistics(unittest.TestCase):
    def test_bimodal_needs_a_wide_lone_gap(self):
        self.assertTrue(ri.bimodal([100, 100.1, 100.2, 103, 103.1, 103.2]))
        self.assertFalse(ri.bimodal([100, 100.5, 101, 101.5, 102, 102.5]))
        # one outlier is not two groups
        self.assertFalse(ri.bimodal([100, 100.1, 100.2, 100.3, 110]))
        self.assertFalse(ri.bimodal([100, 103, 103]))

    def test_round_ratio_uses_medians_of_processes(self):
        rows = [
            {"round": 1, "variant": "A", "m": 100}, {"round": 1, "variant": "A", "m": 102},
            {"round": 1, "variant": "B", "m": 110}, {"round": 1, "variant": "B", "m": 112},
        ]
        self.assertAlmostEqual(ri.per_round_ratios(rows, "m")[0], 111 / 101)

    def test_summary_flags_only_a_change_outside_the_threshold(self):
        with tempfile.TemporaryDirectory() as d:
            write_samples(d, [((1000, 1001), (1000, 1002))] * 6)
            text = ri.summarize(d, 1.0)
            self.assertIn("0 of 1 flagged", text)
        with tempfile.TemporaryDirectory() as d:
            write_samples(d, [((1000, 1001), (1030, 1032))] * 6)
            text = ri.summarize(d, 1.0)
            self.assertIn("1 of 1 flagged: q streaming", text)
            self.assertIn("| 1.0305 |", text)

    def test_a_run_missing_a_build_is_invalid_not_clean(self):
        with tempfile.TemporaryDirectory() as d:
            write_samples(d, [((1000, 1001), (1000, 1002))] * 6)
            p = os.path.join(d, "samples.tsv")
            lines = open(p).read().splitlines()
            # keep only the first epoch, as after an interruption
            open(p, "w").write("\n".join(lines[:3]) + "\n")
            text, verdict = ri.summary(d, 1.0)
            self.assertEqual(verdict, "invalid")
            self.assertIn("INCOMPLETE", text)

    def test_a_run_without_a_finished_manifest_is_invalid(self):
        with tempfile.TemporaryDirectory() as d:
            open(os.path.join(d, "samples.tsv"), "w").write("\t".join(COLS) + "\n")
            self.assertEqual(ri.summary(d, 1.0)[1], "invalid")
            write_samples(d, [((1000, 1001), (1000, 1002))] * 6)
            json.dump({"queries": ["q"], "modes": ["streaming"], "complete": False},
                      open(os.path.join(d, "manifest.json"), "w"))
            self.assertEqual(ri.summary(d, 1.0)[1], "invalid")

    def test_a_split_side_is_inconclusive_even_when_the_median_passes(self):
        with tempfile.TemporaryDirectory() as d:
            rounds = [((100_000_000, 100_000_000), (100_000_000, 100_000_000))] * 7
            rounds.append(((100_000_000, 100_000_000), (200_000_000, 200_000_000)))
            write_samples(d, rounds)
            text, verdict = ri.summary(d, 1.0)
            self.assertEqual(verdict, "flagged")
            self.assertIn("| SPLIT |", text)

    def test_summary_reports_a_multiplexed_window(self):
        with tempfile.TemporaryDirectory() as d:
            write_samples(d, [((1000, 1001), (1000, 1002))] * 3)
            p = os.path.join(d, "samples.tsv")
            lines = open(p).read().splitlines()
            f = lines[1].split("\t")
            f[COLS.index("covered")] = "0"
            lines[1] = "\t".join(f)
            open(p, "w").write("\n".join(lines) + "\n")
            text, verdict = ri.summary(d, 1.0)
            self.assertIn("FAIL: 1 windows were not fully counted", text)
            self.assertEqual(verdict, "invalid")


@unittest.skipUnless(os.path.exists("/proc/sys/kernel/perf_event_paranoid"), "needs Linux")
class Counters(unittest.TestCase):
    def test_inherited_counters_cover_threads_started_after_exec(self):
        # a child that starts a thread doing a known loop, after the counters open
        code = "import threading\ndef f():\n    s = 0\n    for i in range(2_000_000): s += i\nt = threading.Thread(target=f); t.start(); t.join()\n"
        r, w = os.pipe()
        p = subprocess.Popen(["sh", "-c", 'read _ || exit 1; exec "$@" < /dev/null', "sh", "python3", "-c", code], stdin=r)
        os.close(r)
        try:
            c = ri.Counters(p.pid, False)
        except OSError as e:
            # A runner without counters skips here, and a process left unwaited would warn
            # when a later test collects it.
            p.kill()
            p.wait()
            os.close(w)
            self.skipTest(f"no counters here: {e}")
        c0 = c.read()
        os.write(w, b"go\n")
        os.close(w)
        p.wait()
        d = ri.delta(c0, c.read(), ri.core_pmus()[0][0])
        c.close()
        # two million loop iterations of the interpreter take far more than 20M instructions
        self.assertGreater(d["instr_u"], 20_000_000)
        self.assertGreater(d["task_clock_ns"], 0)

    def test_a_thread_started_before_the_counters_is_reported_uncovered(self):
        # the child says when its thread runs, and only then are the counters opened
        code = (
            "import sys, threading\n"
            "def f():\n    s = 0\n    for i in range(80_000_000): s += i\n"
            "t = threading.Thread(target=f); t.start()\n"
            "print('started', flush=True); t.join()\n"
        )
        p = subprocess.Popen(["python3", "-c", code], stdout=subprocess.PIPE, text=True)
        try:
            self.assertEqual(p.stdout.readline().strip(), "started")
            time.sleep(0.05)
            try:
                c = ri.Counters(p.pid, False)
            except OSError as e:
                self.skipTest(f"no counters here: {e}")
            c0 = c.read()
            time.sleep(0.3)
            d = ri.delta(c0, c.read(), ri.core_pmus()[0][0])
            c.close()
        finally:
            p.kill()
            p.wait()
            p.stdout.close()
        self.assertFalse(d["covered"])


@unittest.skipUnless(os.path.exists("/proc/self/stat") and shutil.which("taskset") and shutil.which("chrt"),
                     "needs Linux, taskset and chrt")
class Spinners(unittest.TestCase):
    def test_a_run_killed_outright_leaves_no_busy_loop(self):
        # A run whose own cleanup cannot run, as after SIGKILL, still takes its loops with it.
        with tempfile.TemporaryDirectory() as d:
            pids = os.path.join(d, "pids")
            code = (
                "import importlib.util, os, sys, time\n"
                "spec = importlib.util.spec_from_file_location('ri', sys.argv[1])\n"
                "ri = importlib.util.module_from_spec(spec); spec.loader.exec_module(ri)\n"
                "out = []; cpu = min(os.sched_getaffinity(0))\n"
                "ri.spinners([cpu, cpu], out)\n"
                "open(sys.argv[2] + '.tmp', 'w').write(' '.join(str(p.pid) for p in out))\n"
                "os.rename(sys.argv[2] + '.tmp', sys.argv[2])\n"
                "time.sleep(60)\n"
            )
            run = subprocess.Popen([sys.executable, "-c", code, os.path.join(SCRIPTS, "request-insn.py"), pids])
            loops = []
            try:
                deadline = time.monotonic() + 30
                while not os.path.exists(pids):
                    self.assertIsNone(run.poll(), "the run ended before it started its loops")
                    self.assertLess(time.monotonic(), deadline, "the loops did not start")
                    time.sleep(0.02)
                loops = [int(x) for x in open(pids).read().split()]
                self.assertEqual(len(loops), 2)
                run.send_signal(signal.SIGKILL)
                run.wait(timeout=10)
                for pid in loops:
                    self.assertTrue(process_checks.ended(pid), f"busy loop {pid} outlived its run")
            finally:
                if run.poll() is None:
                    run.kill()
                    run.wait()
                for pid in loops:
                    try:
                        os.kill(pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass


if __name__ == "__main__":
    unittest.main()
