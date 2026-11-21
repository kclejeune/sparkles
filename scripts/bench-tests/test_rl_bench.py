"""Check scripts/rl-reference/bench.sh: every helper is defined before the script first
calls it, every `sparkles infer` of it and of compare.sh takes MAX_INFERRED as its
--max-inferred, and its capped runner classifies a run's end and leaves no process behind.
The runner tests need a systemd user manager and GNU time, and are skipped without them."""

import process_checks
import os
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BENCH = ROOT / "scripts" / "rl-reference" / "bench.sh"
COMPARE = ROOT / "scripts" / "rl-reference" / "compare.sh"
TEXT = BENCH.read_text()


def gnu_time():
    for c in (shutil.which("gtime"), "/usr/bin/time", shutil.which("time")):
        if c and os.path.exists(c):
            r = subprocess.run([c, "-v", "true"], capture_output=True, text=True)
            if r.returncode == 0 and "Maximum resident set size" in r.stderr:
                return c
    return None


def user_systemd():
    if not shutil.which("systemd-run"):
        return False
    r = subprocess.run(
        ["systemd-run", "--user", "--scope", "-q", "true"], capture_output=True, timeout=30
    )
    return r.returncode == 0


GTIME = gnu_time()
SCOPED = user_systemd()


class Order(unittest.TestCase):
    def test_helpers_are_defined_before_their_first_call(self):
        lines = TEXT.splitlines()
        defs = {}
        for i, line in enumerate(lines):
            m = re.match(r"([a-z_]+)\(\) \{", line)
            if m:
                defs[m.group(1)] = i
        self.assertIn("capped", defs)
        depth = 0
        for i, line in enumerate(lines):
            if re.match(r"[a-z_]+\(\) \{", line):
                depth += not line.rstrip().endswith("}")
                continue
            if depth and line == "}":
                depth -= 1
                continue
            if depth or line.lstrip().startswith("#"):
                continue
            for name, at in defs.items():
                if re.match(rf"\s*(if !? *)?{name}\b(?!\()", line):
                    self.assertLess(at, i, f"{name} is called at line {i + 1} before its definition")


class InferredLimit(unittest.TestCase):
    def test_every_infer_passes_the_limit(self):
        for script in (BENCH, COMPARE):
            calls = [
                line for line in script.read_text().splitlines()
                if re.search(r'"\$SPARKLES" infer\b', line) and not line.lstrip().startswith("#")
            ]
            self.assertTrue(calls, f"{script.name} has no infer call")
            for line in calls:
                self.assertIn('--max-inferred "$MAX_INFERRED"', line, f"{script.name}: {line.strip()}")
            self.assertIn("MAX_INFERRED=${MAX_INFERRED:-0}", script.read_text())
        self.assertIn("export SPARKLES NMO MAX_INFERRED", TEXT)

    def test_usage_prints_the_whole_header(self):
        for script in (BENCH, COMPARE):
            lines = script.read_text().splitlines()
            end = next(i for i, ln in enumerate(lines) if not ln.startswith("#"))
            self.assertIn(f"sed -n '2,{end}p'", script.read_text(), script.name)

    def test_compare_passes_the_limit_to_sparkles(self):
        # a stand-in for sparkles records its arguments and fails the infer step
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            fake = d / "sparkles"
            fake.write_text('#!/bin/sh\necho "$@" >> "$ARGS_LOG"\n[ "$1" != infer ]\n')
            fake.chmod(0o755)
            (d / "in.nt").write_text("<urn:s> <urn:p> <urn:o> .\n")
            for env, want in (({}, "0"), ({"MAX_INFERRED": "123456789"}, "123456789")):
                log = d / "args.log"
                log.unlink(missing_ok=True)
                e = dict(os.environ, SPARKLES=str(fake), NMO="/bin/false", ARGS_LOG=str(log))
                e.pop("MAX_INFERRED", None)
                e.update(env)
                r = subprocess.run(["bash", str(COMPARE), str(d / "in.nt"), str(d / "work")], env=e,
                                   capture_output=True, text=True, timeout=60)
                self.assertEqual(r.returncode, 2, r.stderr)
                self.assertIn("sparkles infer failed", r.stderr)
                infer = [ln for ln in log.read_text().splitlines() if ln.startswith("infer ")]
                self.assertEqual(infer, [f"infer --loc db --profile owl-rl --max-inferred {want}"])


@unittest.skipUnless(GTIME and SCOPED, "needs GNU time and a systemd user manager")
class Capped(unittest.TestCase):
    def capped(self, *cmd, timer=False, run_timeout=3, mem="200M"):
        fn = re.search(r"^capped\(\) \{.*?^\}$", TEXT, re.S | re.M).group(0)
        with tempfile.TemporaryDirectory() as d:
            (Path(d) / "logs").mkdir()
            script = fn + '\ncapped t ' + ("-t logs/t.time " if timer else "") + '"$@" || true\necho "$CAPPED_STATUS"\n'
            env = dict(os.environ, GTIME=GTIME, MEM_MAX=mem, RUN_TIMEOUT=str(run_timeout), KILL_AFTER="1")
            r = subprocess.run(["bash", "-c", script, "t", *cmd], cwd=d, env=env, capture_output=True, text=True, timeout=60)
            return r.stdout.strip().splitlines()[-1], d

    def test_statuses(self):
        self.assertEqual(self.capped("true")[0], "ok")
        self.assertEqual(self.capped("false")[0], "error")
        self.assertEqual(self.capped("sleep", "10")[0], "timeout")
        self.assertEqual(self.capped("sh", "-c", "kill -9 $$")[0], "killed")
        self.assertEqual(self.capped("/nonexistent/engine")[0], "error")
        grow = "b = []\nwhile True: b.append(bytearray(32 << 20))"
        self.assertEqual(self.capped("python3", "-c", grow, mem="128M")[0], "oom")

    def test_a_timed_run_that_ignores_term_leaves_nothing_behind(self):
        with tempfile.TemporaryDirectory() as d:
            pids = Path(d) / "pids"
            # the engine and a child of its own ignore TERM, and each records its pid
            engine = (
                f"trap '' TERM; sh -c 'echo $$ >> {pids}; exec sleep 60' &"
                f" echo $$ >> {pids}; sleep 60"
            )
            st, _ = self.capped("sh", "-c", engine, timer=True, run_timeout=1)
            self.assertEqual(st, "timeout")
            for pid in pids.read_text().split():
                self.assertTrue(process_checks.ended(int(pid)), f"process {pid} survived")


@unittest.skipUnless(GTIME and SCOPED, "needs GNU time and a systemd user manager")
class Verified(unittest.TestCase):
    """A timed run counts as verified only when it finished and derived the expected count."""

    def run_one(self, cmd, env):
        fns = "\n".join(
            [re.search(r"^log\(\) \{.*\}$", TEXT, re.M).group(0)]
            + [re.search(rf"^{name}\(\) \{{.*?^\}}$", TEXT, re.S | re.M).group(0) for name in ("die", "capped", "run")]
        )
        unverified = re.search(r"^UNVERIFIED=0\nunverified\(\) \{.*?^\}$", TEXT, re.S | re.M).group(0)
        with tempfile.TemporaryDirectory() as d:
            (Path(d) / "logs").mkdir()
            (Path(d) / "results").mkdir()
            script = f"{fns}\n{unverified}\nRECORD=results/runs.jsonl\nrun r-r1 1 1 {cmd}\necho \"rc=$? unverified=$UNVERIFIED\"\n"
            e = dict(os.environ, GTIME=GTIME, MEM_MAX="200M", RUN_TIMEOUT="2", KILL_AFTER="1", PYTHON="python3", OUT=d, **env)
            r = subprocess.run(["bash", "-c", script], cwd=d, env=e, capture_output=True, text=True, timeout=60)
            path = Path(d) / "results" / "unverified.txt"
            return r, path.read_text() if path.exists() else ""

    def test_a_run_with_the_expected_count_is_verified(self):
        r, why = self.run_one("echo '5 inferred triples'", {"EXPECT": "5"})
        self.assertIn("rc=0 unverified=0", r.stdout, r.stderr)
        self.assertEqual(why, "")

    def test_a_run_that_did_not_finish_is_unverified(self):
        r, why = self.run_one("sleep 10", {"EXPECT": "5"})
        self.assertIn("unverified=1", r.stdout, r.stderr)
        self.assertIn("r-r1: it ended with timeout", why)

    def test_a_run_without_an_expected_count_is_unverified(self):
        r, why = self.run_one("echo '5 inferred triples'", {})
        self.assertIn("unverified=1", r.stdout, r.stderr)
        r, why = self.run_one("echo '5 inferred triples'", {"REFERENCE": "1"})
        self.assertIn("unverified=0", r.stdout, r.stderr)

    def test_a_run_with_another_count_stops_the_benchmark(self):
        r, _ = self.run_one("echo '4 inferred triples'", {"EXPECT": "5"})
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("derived 4 triples, not 5", r.stderr)


if __name__ == "__main__":
    unittest.main()
