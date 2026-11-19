"""Tests of scripts/x08-gates/latency.sh and combine.py, in the pool form and the
base-and-tip form, with a stub server in place of sparkles.

    python3 -m unittest discover -s scripts/bench-tests -p test_x08_latency.py
"""

import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent.parent
GATES = SCRIPTS / "x08-gates"
STUB = Path(__file__).resolve().parent / "fixtures" / "stub-sparkles.py"


def samples(path, ratio, rounds=12, a_ms=0.1, query="q"):
    lines = ["round\tepoch\tvariant\tquery\trep\texec_ms\twall_ms"]
    for r in range(1, rounds + 1):
        for rep in range(3):
            lines.append(f"{r}\t{r}\tA\t{query}\t{rep}\t{a_ms}\t1")
            lines.append(f"{r}\t{r}\tB\t{query}\t{rep}\t{a_ms * ratio}\t1")
    Path(path).write_text("\n".join(lines) + "\n")
    return path


def combine(*args):
    return subprocess.run([sys.executable, str(GATES / "combine.py"), *map(str, args)],
                          capture_output=True, text=True)


class CombineTests(unittest.TestCase):
    def test_base_and_tip_labels_estimate_the_tip_over_the_base(self):
        with tempfile.TemporaryDirectory() as d:
            # the tip is 10% slower and the second position costs 2%
            second = samples(Path(d) / "s.tsv", 1.10 * 1.02, a_ms=0.1)
            first = samples(Path(d) / "f.tsv", 1.02 / 1.10, a_ms=0.11)
            r = combine("--labels", "base,tip", second, first)
            self.assertEqual(r.returncode, 1, r.stdout)
            self.assertIn("| query | base ms | tip/base |", r.stdout)
            self.assertIn("| q | 0.1000 | 1.1000 |", r.stdout)
            self.assertIn("q: tip/base 1.1000", r.stdout)
            second = samples(Path(d) / "s.tsv", 1.02, a_ms=0.1)
            first = samples(Path(d) / "f.tsv", 1.02, a_ms=0.1)
            r = combine("--labels", "base,tip", second, first)
            self.assertEqual(r.returncode, 0, r.stdout)

    def test_the_pool_form_keeps_its_table(self):
        with tempfile.TemporaryDirectory() as d:
            r = combine(samples(Path(d) / "s.tsv", 1.0), samples(Path(d) / "f.tsv", 1.0))
            self.assertEqual(r.returncode, 0, r.stdout)
            self.assertIn("| query | pool off ms | on/off |", r.stdout)
            self.assertIn("| pool-on-second ratio | pool-on-first ratio |", r.stdout)


@unittest.skipUnless(all(shutil.which(t) for t in ("taskset", "chrt", "zstd", "curl")) and (os.cpu_count() or 1) >= 5,
                     "needs taskset, chrt, zstd, curl and five CPUs")
class LatencyTests(unittest.TestCase):
    def work(self, d):
        w = Path(d) / "work"
        (w / "queries").mkdir(parents=True)
        (w / "queries" / "count-all.rq").write_text("SELECT (COUNT(*) AS ?c) WHERE { ?s ?p ?o }")
        (w / "data.nt.zst").write_bytes(subprocess.run(["zstd", "-q", "-c"], input=b"", capture_output=True).stdout)
        shutil.copy(SCRIPTS / "nsc-bench-remote.sh", w)
        bins = {}
        for name in ("base", "tip"):
            p = Path(d) / name / "sparkles"
            p.parent.mkdir()
            p.write_text(STUB.read_text().replace('IDENTITY = "stub"', f'IDENTITY = "{name}"'))
            p.chmod(0o755)
            bins[name] = p
        return w, bins

    def test_the_base_and_tip_form_runs_each_binary_in_both_positions(self):
        with tempfile.TemporaryDirectory() as d:
            w, bins = self.work(d)
            extra = Path(d) / "ask-shape.rq"
            extra.write_text("ASK { ?s ?p ?o }")
            r = subprocess.run(
                ["bash", str(GATES / "latency.sh"), "--force", "--base", bins["base"], "--tip", bins["tip"],
                 "--base-execution", "eager", "--queries", f"count-all {extra}", "--rounds", "10", "--reps", "2",
                 "--port", "4301", w], capture_output=True, text=True, timeout=600)
            # the stub answers in a fixed time, so the gate passes
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            self.assertIn("| query | base ms | tip/base |", r.stdout)
            self.assertIn("| ask-shape |", r.stdout)
            seen = {}
            for run in ("tip-second", "tip-first", "same"):
                for line in (w / "runs" / run / "requests.tsv").read_text().splitlines():
                    identity, owner, store, mode, _ = line.split("\t")
                    seen.setdefault(run, set()).add((store, identity, owner, mode))
            self.assertEqual(seen["tip-second"], {("stores/A", "base", "base", "eager"), ("stores/B", "tip", "tip", "-")})
            self.assertEqual(seen["tip-first"], {("stores/A", "tip", "tip", "-"), ("stores/B", "base", "base", "eager")})
            self.assertEqual(seen["same"], {("stores/A", "tip", "tip", "-"), ("stores/B", "tip", "tip", "-")})
            self.assertIn("execution eager", (w / "runs-manifest.txt").read_text())
            self.assertIn("the answers agree", r.stdout)
            # each binary loaded its own store, and the run against itself one
            for run, stores in (("tip-second", ["A", "B"]), ("tip-first", ["A", "B"]), ("same", ["A"])):
                loads = (w / "runs" / run / "results" / "load.tsv").read_text().splitlines()
                self.assertEqual([l.split("\t")[0] for l in loads], stores)

    def test_an_inherited_environment_cannot_change_the_arms(self):
        with tempfile.TemporaryDirectory() as d:
            w, bins = self.work(d)
            env = dict(os.environ, BINARY_OF_B="A", SHARE_STORE="1", EXECUTION="streaming")
            r = subprocess.run(
                ["bash", str(GATES / "latency.sh"), "--force", "--base", bins["base"], "--tip", bins["tip"], "--queries",
                 "count-all", "--rounds", "10", "--reps", "1", "--port", "4303", w],
                capture_output=True, text=True, timeout=600, env=env)
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            ids = {tuple(l.split("\t")[:2]) for l in (w / "runs" / "tip-second" / "requests.tsv").read_text().splitlines()}
            self.assertEqual(ids, {("base", "base"), ("tip", "tip")})

    def test_disagreeing_answers_fail_the_gate(self):
        with tempfile.TemporaryDirectory() as d:
            w, bins = self.work(d)
            bins["tip"].write_text(bins["tip"].read_text().replace('b"?c\\n1\\n"', 'b"?c\\n2\\n"'))
            r = subprocess.run(
                ["bash", str(GATES / "latency.sh"), "--force", "--base", bins["base"], "--tip", bins["tip"], "--queries",
                 "count-all", "--rounds", "10", "--reps", "1", "--port", "4305", w],
                capture_output=True, text=True, timeout=600)
            self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
            self.assertIn("the untimed answers differ", r.stdout)

    def test_refusals(self):
        with tempfile.TemporaryDirectory() as d:
            r = subprocess.run(["bash", str(GATES / "latency.sh"), "--base", "/bin/true", d],
                               capture_output=True, text=True)
            self.assertIn("needs --base and --tip", r.stderr)
            r = subprocess.run(["bash", str(GATES / "latency.sh"), "--base", "/bin/true", "--tip", "/bin/true",
                                "--rounds", "4", d], capture_output=True, text=True)
            self.assertIn("at least 10 rounds", r.stderr)
            w, bins = self.work(d)
            r = subprocess.run(["bash", str(GATES / "latency.sh"), "--base", bins["base"], "--tip", bins["tip"], w],
                               capture_output=True, text=True)
            self.assertIn("names no compilers", r.stderr)
            for q, message in (("/no/such.rq", "no query file"), (f"{bins['tip']}", "must end in .rq"),
                               (f"count-all {w}/queries/count-all.rq", "two queries are named count-all")):
                r = subprocess.run(["bash", str(GATES / "latency.sh"), "--force", "--base", bins["base"], "--tip", bins["tip"],
                                    "--queries", q, w], capture_output=True, text=True)
                self.assertIn(message, r.stderr)


if __name__ == "__main__":
    unittest.main()
