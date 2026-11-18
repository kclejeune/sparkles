"""Tests of scripts/request-insn.py: its statistics, its summary and its counters.

    python3 -m unittest discover -s scripts/bench-tests -p test_request_insn.py
"""

import importlib.util
import os
import subprocess
import tempfile
import time
import unittest

SCRIPTS = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
spec = importlib.util.spec_from_file_location("request_insn", os.path.join(SCRIPTS, "request-insn.py"))
ri = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ri)

COLS = (
    "round epoch variant query mode reps instr_u instr_k cycles_u task_clock_ns ctx_switches "
    "page_faults migrations covered window_ms exec_ms wall_ms"
).split()


def write_samples(d, per_round):
    """per_round: list of (A values, B values) per round, two processes each, in counts per
    request of 10 requests."""
    with open(os.path.join(d, "samples.tsv"), "w") as f:
        f.write("\t".join(COLS) + "\n")
        epoch = 0
        for rnd, (va, vb) in enumerate(per_round, 1):
            for v, x in (("A", va[0]), ("B", vb[0]), ("B", vb[1]), ("A", va[1])):
                epoch += 1
                f.write("\t".join(map(str, [rnd, epoch, v, "q", "streaming", 10, x * 10, "", x * 20, 1000, 5, 0, 0, 1, "1.0", "0.01", "0.1"])) + "\n")
                f.write("\t".join(map(str, [rnd, epoch, v, "_idle", "-", 1, 100, "", 200, 10, 1, 0, 0, 1, "1.0", "nan", "nan"])) + "\n")


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

    def test_summary_reports_a_multiplexed_window(self):
        with tempfile.TemporaryDirectory() as d:
            write_samples(d, [((1000, 1001), (1000, 1002))] * 3)
            p = os.path.join(d, "samples.tsv")
            lines = open(p).read().splitlines()
            f = lines[1].split("\t")
            f[COLS.index("covered")] = "0"
            lines[1] = "\t".join(f)
            open(p, "w").write("\n".join(lines) + "\n")
            self.assertIn("FAIL: 1 windows had a multiplexed counter", ri.summarize(d, 1.0))


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
            p.kill()
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
        code = "import threading\ndef f():\n    s = 0\n    for i in range(40_000_000): s += i\nt = threading.Thread(target=f); t.start(); t.join()\n"
        p = subprocess.Popen(["python3", "-c", code])
        try:
            time.sleep(0.3)
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
        self.assertFalse(d["covered"])


if __name__ == "__main__":
    unittest.main()
