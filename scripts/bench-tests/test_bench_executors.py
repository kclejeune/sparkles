"""Tests of scripts/bench-executors.sh and its arms through scripts/nsc-bench-remote.sh,
with a stub server in place of sparkles, and of `bench-gate.py compare --best-of`.

    python3 -m unittest discover -s scripts/bench-tests -p test_bench_executors.py
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent.parent
STUB = Path(__file__).resolve().parent / "fixtures" / "stub-sparkles.py"
GATE = SCRIPTS / "bench-gate.py"
NEEDS = ("taskset", "zstd", "curl")


def stub(d, identity, exec_ms=1.0):
    path = Path(d) / identity / "sparkles"
    path.parent.mkdir(parents=True)
    path.write_text(STUB.read_text().replace('IDENTITY = "stub"', f'IDENTITY = "{identity}"')
                    .replace("EXEC_MS = 1.0", f"EXEC_MS = {exec_ms}"))
    path.chmod(0o755)
    return path


def gate(*args):
    return subprocess.run([sys.executable, str(GATE), *map(str, args)], capture_output=True, text=True)


def executors(d, *args, port):
    return subprocess.run(
        ["bash", str(SCRIPTS / "bench-executors.sh"), "--force", "--scales", "10", "--threads", "2",
         "--reps", "2", "--warmup", "1", "--queries", "count-all", "--data-dir", str(Path(d) / "data"),
         "--port", str(port), *map(str, args)],
        capture_output=True, text=True, timeout=300)


def epochs(cell):
    """The variant of each server process, in order."""
    seen = {}
    for line in (cell / "results" / "samples.tsv").read_text().splitlines()[1:]:
        p = line.split("\t")
        seen.setdefault(int(p[1]), p[2])
    return [seen[e] for e in sorted(seen)]


@unittest.skipUnless(all(shutil.which(t) for t in NEEDS) and (os.cpu_count() or 1) >= 3,
                     "needs taskset, zstd, curl and three CPUs")
class ExecutorsTests(unittest.TestCase):
    def test_four_arms_interleave_two_binaries_with_a_store_each(self):
        with tempfile.TemporaryDirectory() as d:
            base, tip = stub(d, "base"), stub(d, "tip")
            out = Path(d) / "out"
            r = executors(d, "--out", out, "--rounds", "2", "--commit", "t1", "--build-config", "{}",
                          "--base", base, "--base-commit", "b1", "--base-build-config", "{}", tip,
                          port=4291)
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            cell = out / "people-10-threads-2"
            # the binaries alternate, and the second round starts with the tip
            self.assertEqual(epochs(cell), list("ABCDDCBA") + list("BADCCDAB"))
            # every arm ran its own binary and mode, and each binary served its own store
            seen = set()
            for line in (cell / "requests.tsv").read_text().splitlines():
                identity, owner, store, mode, _ = line.split("\t")
                seen.add((store, identity, owner, mode))
            self.assertEqual(seen, {("stores/A", "base", "base", "eager"),
                                    ("stores/B", "tip", "tip", "-"),
                                    ("stores/C", "base", "base", "streaming"),
                                    ("stores/D", "tip", "tip", "streaming")})
            self.assertEqual(sorted(p.name for p in (cell / "stores").iterdir()) if (cell / "stores").exists() else [], [])
            rec = json.loads((out / "record.json").read_text())
            keys = {m["key"] for m in rec["metrics"]}
            for arm in ("base-eager", "base-streaming", "tip-default", "tip-streaming"):
                self.assertIn(f"0.000105m-2t/{arm}/count-all/exec", keys)
            self.assertEqual(rec["manifest"]["base_binary_commit"], "b1")
            self.assertEqual(rec["manifest"]["binary_commit"], "t1")
            checks = {c["name"]: c for c in rec["checks"]}
            self.assertTrue(checks["0.000105m-2t/count-all/answers"]["ok"])
            self.assertEqual(set(checks["0.000105m-2t/count-all/answers"]["detail"]),
                             {"base-eager", "base-streaming", "tip-default", "tip-streaming"})
            self.assertIn("tip-default / best base", (out / "summary.md").read_text())
            # a stub is no ELF binary, so its compilers are unknown, which compare refuses
            c = gate("compare", out / "record.json", "--best-of", "base-eager,base-streaming")
            self.assertEqual(c.returncode, 2, c.stdout + c.stderr)
            c = gate("compare", out / "record.json", "--best-of", "base-eager,base-streaming", "--allow-different",
                     "--keys", "/exec$")
            self.assertEqual(c.returncode, 0, c.stdout + c.stderr)
            self.assertIn("0.000105m-2t/tip-default/count-all/exec", c.stdout)
            self.assertIn("0.000105m-2t/tip-streaming/count-all/exec", c.stdout)
            self.assertNotIn("/base-eager/count-all/exec |", c.stdout)

    def test_one_binary_keeps_the_eager_and_streaming_record(self):
        with tempfile.TemporaryDirectory() as d:
            tip = stub(d, "tip")
            out = Path(d) / "out"
            r = executors(d, "--out", out, "--rounds", "2", "--build-config", "{}", tip, port=4293)
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            cell = out / "people-10-threads-2"
            self.assertEqual(epochs(cell), list("ABBA") * 2)
            modes = {(l.split("\t")[2], l.split("\t")[3]) for l in (cell / "requests.tsv").read_text().splitlines()}
            self.assertEqual(modes, {("stores/A", "eager"), ("stores/B", "streaming")})
            rec = json.loads((out / "record.json").read_text())
            # the settings that compare matches against earlier records are unchanged
            self.assertEqual(rec["settings"]["variants"],
                             "A execution=eager, B execution=streaming, one binary and one shared store")
            self.assertIn("2 rounds of A,B,B,A,", rec["settings"]["run"])
            self.assertIn("rounds of A,B,B,A,", (cell / "manifest.txt").read_text())
            self.assertIn("execution eager,streaming", (cell / "manifest.txt").read_text())
            self.assertEqual({m["key"].split("/")[1] for m in rec["metrics"]}, {"eager", "streaming"})
            self.assertNotIn("base_binary", rec["manifest"])
            self.assertIn("| query | eager ms | streaming ms | streaming / eager |", (out / "summary.md").read_text())

    def test_other_modes_of_one_binary_run_in_order_and_reverse(self):
        with tempfile.TemporaryDirectory() as d:
            tip = stub(d, "tip")
            out = Path(d) / "out"
            r = executors(d, "--out", out, "--rounds", "1", "--modes", "eager streaming collect", tip, port=4295)
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            cell = out / "people-10-threads-2"
            self.assertEqual(epochs(cell), list("ABCCBA"))
            rec = json.loads((out / "record.json").read_text())
            self.assertEqual({m["key"].split("/")[1] for m in rec["metrics"]}, {"eager", "streaming", "collect"})
            # one binary has one toolchain, so its arms compare without --allow-different
            c = gate("compare", out / "record.json", "--best-of", "eager", "--arms", "collect", "--keys", "/exec$")
            self.assertEqual(c.returncode, 0, c.stdout + c.stderr)

    def test_refusals(self):
        with tempfile.TemporaryDirectory() as d:
            base, tip = stub(d, "base"), stub(d, "tip")
            r = executors(d, "--out", Path(d) / "o1", "--rounds", "1", "--base", base, tip, port=4297)
            self.assertNotEqual(r.returncode, 0)
            self.assertIn("must be even", r.stderr)
            r = executors(d, "--out", Path(d) / "o2", "--modes", "eager eager", tip, port=4297)
            self.assertIn("named twice", r.stderr)
            r = executors(d, "--out", Path(d) / "o3", "--base-commit", "x", tip, port=4297)
            self.assertIn("need --base", r.stderr)


class RemoteRunnerTests(unittest.TestCase):
    def runner(self, env):
        d = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, d, True)
        shutil.copy(SCRIPTS / "nsc-bench-remote.sh", d)
        (d / "queries").mkdir()
        (d / "run.env").write_text("SERVER_CPU=0\nCLIENT_CPU=0\nSPIN=0\nDYNAMIC_A=0\nDYNAMIC_B=0\n" + env)
        (d / "bin-A").write_text("a\n")
        (d / "bin-B").write_text("b\n")
        (d / "data.nt.zst").write_text("")
        subprocess.run(["bash", str(d / "nsc-bench-remote.sh")], capture_output=True, text=True, timeout=60)
        return (d / "results" / "done").read_text() if (d / "results" / "done").exists() else ""

    def test_the_runner_refuses_inconsistent_arms(self):
        self.assertIn("EXECUTION has 3 modes for 2 variants", self.runner("EXECUTION=a,b,c\n"))
        self.assertIn("leaves out B", self.runner('ORDERS="A A"\n'))
        self.assertIn("not a variant with its own binary",
                      self.runner('VARIANTS="A B C"\nBINARY_OF_B=C\nBINARY_OF_C=A\n'))
        self.assertIn("ORDERS names Z", self.runner('ORDERS="A B Z"\n'))


class BestOfTests(unittest.TestCase):
    def record(self, d, name, metrics, manifest=None):
        rec = {"gate_record": 1, "gate": "executors", "kind": "gate-record", "suite": "synthetic", "scale": "s",
               "host": {"name": "atlas"},
               "manifest": manifest or {"compilers": "rustc 1", "build_config": "{}", "base_compilers": "rustc 1",
                                        "base_build_config": "{}"},
               "settings": {"threshold": 0.05, "short_below": 0.3, "short_threshold": 0.0},
               "metrics": [{"key": k, "value": v, "unit": "ms", "better": "lower"} for k, v in metrics.items()],
               "checks": []}
        path = Path(d) / f"{name}.json"
        path.write_text(json.dumps(rec))
        return path

    def test_each_tip_arm_is_scored_against_the_faster_base_arm(self):
        with tempfile.TemporaryDirectory() as d:
            rec = self.record(d, "r", {"c/base-eager/q/exec": 10.0, "c/base-streaming/q/exec": 20.0,
                                       "c/tip-default/q/exec": 10.4, "c/tip-streaming/q/exec": 11.0})
            r = gate("compare", rec, "--best-of", "base-eager,base-streaming")
            self.assertEqual(r.returncode, 1, r.stdout)
            self.assertIn("| c/tip-default/q/exec | 10 | 10.4 | 1.040 |  | base-eager |", r.stdout)
            self.assertIn("| c/tip-streaming/q/exec | 10 | 11 | 1.100 | slower | base-eager |", r.stdout)
            self.assertEqual(gate("compare", rec, "--best-of", "base-eager,base-streaming",
                                  "--arms", "tip-default").returncode, 0)

    def test_short_queries_use_the_faster_base_value(self):
        with tempfile.TemporaryDirectory() as d:
            rec = self.record(d, "r", {"c/base-eager/q/exec": 0.2, "c/base-streaming/q/exec": 0.4,
                                       "c/tip-default/q/exec": 0.21})
            r = gate("compare", rec, "--best-of", "base-eager,base-streaming")
            self.assertEqual(r.returncode, 1)
            self.assertIn("slower, short", r.stdout)

    def test_a_missing_base_arm_fails(self):
        with tempfile.TemporaryDirectory() as d:
            rec = self.record(d, "r", {"c/base-eager/q/exec": 1.0, "c/tip-default/q/exec": 1.0})
            r = gate("compare", rec, "--best-of", "base-eager,base-streaming")
            self.assertEqual(r.returncode, 1)
            self.assertIn("baseline lacks base-streaming", r.stdout)

    def test_a_missing_tip_metric_fails(self):
        with tempfile.TemporaryDirectory() as d:
            rec = self.record(d, "r", {"c/base-eager/q/exec": 1.0, "c/base-streaming/q/exec": 1.0,
                                       "c/tip-default/q/exec": 1.0, "c/base-eager/p/exec": 1.0,
                                       "c/base-streaming/p/exec": 1.0})
            r = gate("compare", rec, "--best-of", "base-eager,base-streaming")
            self.assertEqual(r.returncode, 1)
            self.assertIn("| c/tip-default/p/exec | 1 |  |  | missing |", r.stdout)

    def test_two_binaries_of_other_toolchains_are_refused(self):
        with tempfile.TemporaryDirectory() as d:
            rec = self.record(d, "r", {"c/base-eager/q/exec": 1.0, "c/tip-default/q/exec": 1.0},
                              {"compilers": "rustc 2", "build_config": "{}", "base_compilers": "rustc 1",
                               "base_build_config": "{}"})
            r = gate("compare", rec, "--best-of", "base-eager")
            self.assertEqual(r.returncode, 2)
            self.assertIn("manifest compilers", r.stderr)

    def test_a_later_record_is_scored_against_both_modes_of_a_baseline(self):
        with tempfile.TemporaryDirectory() as d:
            m = {"compilers": "rustc 1", "build_config": "{}"}
            old = self.record(d, "old", {"c/eager/q/exec": 5.0, "c/streaming/q/exec": 4.0}, m)
            new = self.record(d, "new", {"c/eager/q/exec": 4.1, "c/streaming/q/exec": 4.0}, m)
            r = gate("compare", old, new, "--best-of", "eager,streaming")
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            self.assertIn("| c/eager/q/exec | 4 | 4.1 | 1.025 |  | streaming |", r.stdout)
            new = self.record(d, "new", {"c/eager/q/exec": 4.3, "c/streaming/q/exec": 4.0}, m)
            self.assertEqual(gate("compare", old, new, "--best-of", "eager,streaming").returncode, 1)

    def test_compare_without_a_candidate_needs_best_of(self):
        with tempfile.TemporaryDirectory() as d:
            rec = self.record(d, "r", {"c/eager/q/exec": 1.0})
            r = gate("compare", rec)
            self.assertEqual(r.returncode, 2)
            self.assertEqual(gate("compare", rec, rec).returncode, 0)


if __name__ == "__main__":
    unittest.main()
