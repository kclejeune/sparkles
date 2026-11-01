"""Tests of scripts/bench-gate.py: gate records, the executor matrix converter and compare.

    python3 -m unittest discover -s scripts/bench-tests
"""

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent.parent
GATE = SCRIPTS / "bench-gate.py"


def run(*args):
    return subprocess.run([sys.executable, str(GATE), *map(str, args)], capture_output=True, text=True)


def metrics_record(d, name, rows, threshold=0.05, scale="1"):
    src = Path(d) / f"{name}.jsonl"
    src.write_text("".join(json.dumps(r) + "\n" for r in rows))
    out = Path(d) / f"{name}.json"
    r = run("metrics", src, "--gate", "insn-screen", "--out", out, "--threshold", threshold,
            "--manifest", "commit=abc", "--scale", scale)
    assert r.returncode == 0, r.stderr
    return out


class GateTests(unittest.TestCase):
    def test_metrics_builds_a_versioned_record(self):
        with tempfile.TemporaryDirectory() as d:
            out = metrics_record(d, "a", [
                {"key": "x", "value": 10, "unit": "instructions", "better": "lower"},
                {"check": "x/single-thread", "ok": True, "detail": "0 ms"},
            ])
            rec = json.loads(out.read_text())
            self.assertEqual(rec["gate_record"], 1)
            self.assertEqual(rec["manifest"]["commit"], "abc")
            self.assertEqual(rec["settings"]["threshold"], 0.05)
            self.assertEqual(len(rec["metrics"]), 1)
            self.assertEqual(rec["checks"][0]["ok"], True)

    def test_duplicate_metrics_are_refused(self):
        with tempfile.TemporaryDirectory() as d:
            src = Path(d) / "m.jsonl"
            row = json.dumps({"key": "x", "value": 1, "unit": "ms", "better": "lower"})
            src.write_text(row + "\n" + row + "\n")
            r = run("metrics", src, "--gate", "incr", "--out", Path(d) / "r.json")
            self.assertEqual(r.returncode, 2)
            self.assertIn("duplicate", r.stderr)

    def test_compare_flags_a_slowdown_at_the_threshold(self):
        with tempfile.TemporaryDirectory() as d:
            base = metrics_record(d, "base", [
                {"key": "slow", "value": 100, "unit": "ms", "better": "lower"},
                {"key": "same", "value": 100, "unit": "ms", "better": "lower"},
                {"key": "rate", "value": 100, "unit": "commits/s", "better": "higher"},
            ])
            cand = metrics_record(d, "cand", [
                {"key": "slow", "value": 105, "unit": "ms", "better": "lower"},
                {"key": "same", "value": 104, "unit": "ms", "better": "lower"},
                {"key": "rate", "value": 120, "unit": "commits/s", "better": "higher"},
            ])
            r = run("compare", base, cand)
            self.assertEqual(r.returncode, 1, r.stdout)
            self.assertIn("| slow | 100 | 105 | 1.050 | slower |", r.stdout)
            self.assertIn("| rate | 100 | 120 | 0.833 |  |", r.stdout)
            self.assertIn("1 flagged", r.stdout)
            self.assertEqual(run("compare", base, cand, "--threshold", "0.06").returncode, 0)
            self.assertEqual(run("compare", base, cand, "--keys", "same|rate").returncode, 0)

    def test_a_lower_rate_is_a_slowdown(self):
        with tempfile.TemporaryDirectory() as d:
            base = metrics_record(d, "base", [{"key": "rate", "value": 100, "unit": "/s", "better": "higher"}])
            cand = metrics_record(d, "cand", [{"key": "rate", "value": 90, "unit": "/s", "better": "higher"}])
            r = run("compare", base, cand)
            self.assertEqual(r.returncode, 1)
            self.assertIn("1.111 | slower", r.stdout)

    def test_missing_metrics_and_failed_checks_fail(self):
        with tempfile.TemporaryDirectory() as d:
            base = metrics_record(d, "base", [{"key": "a", "value": 1, "unit": "ms", "better": "lower"},
                                              {"key": "b", "value": 1, "unit": "ms", "better": "lower"}])
            cand = metrics_record(d, "cand", [{"key": "a", "value": 1, "unit": "ms", "better": "lower"}])
            r = run("compare", base, cand)
            self.assertEqual(r.returncode, 1)
            self.assertIn("| b | 1 |  |  | missing |", r.stdout)
            bad = metrics_record(d, "bad", [{"key": "a", "value": 1, "unit": "ms", "better": "lower"},
                                            {"key": "b", "value": 1, "unit": "ms", "better": "lower"},
                                            {"check": "answers", "ok": False}])
            r = run("compare", base, bad)
            self.assertEqual(r.returncode, 1)
            self.assertIn("Failed candidate checks: answers.", r.stdout)

    def test_short_queries_may_not_be_slower(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d) / "run"
            res = root / "people-100000-threads-12" / "results"
            res.mkdir(parents=True)
            (root / "queries").mkdir()
            (root / "queries" / "q.rq").write_text("SELECT * WHERE { ?s ?p ?o }")
            (root / "manifest.txt").write_text("host: forge\n")
            res.joinpath("answers.tsv").write_text("A\tq\t1\tx\nB\tq\t1\tx\n")
            def record(name, value):
                rows = ["round\tepoch\tvariant\tquery\trep\texec_ms\twall_ms"]
                rows += [f"1\t1\t{v}\tq\t1\t{value}\t{value}" for v in "AB"]
                res.joinpath("samples.tsv").write_text("\n".join(rows) + "\n")
                out = Path(d) / f"{name}.json"
                self.assertEqual(run("executors", root, "--out", out).returncode, 0)
                return out
            base, cand = record("base", 0.100), record("cand", 0.104)
            r = run("compare", base, cand)
            self.assertEqual(r.returncode, 1)
            self.assertIn("slower, short", r.stdout)

    def test_process_ranges_must_separate(self):
        with tempfile.TemporaryDirectory() as d:
            def runs(name, values):
                files = []
                for i, v in enumerate(values):
                    f = Path(d) / f"{name}{i}.jsonl"
                    f.write_text(json.dumps({"key": "x", "value": v, "unit": "instructions", "better": "lower"}) + "\n"
                                 + json.dumps({"check": "x/result", "ok": True, "fingerprint": "7"}) + "\n")
                    files.append(f)
                out = Path(d) / f"{name}.json"
                r = run("metrics", *files, "--gate", "insn-screen", "--out", out, "--threshold", 0)
                self.assertEqual(r.returncode, 0, r.stderr)
                return out
            base = runs("base", [100, 103, 101])
            rec = json.loads(base.read_text())
            self.assertEqual(rec["metrics"][0]["value"], 101)
            self.assertEqual(rec["metrics"][0]["range"], [100, 103])
            self.assertEqual(rec["settings"]["processes"], 3)
            self.assertEqual(run("compare", base, runs("overlap", [102, 104, 103])).returncode, 0)
            self.assertEqual(run("compare", base, runs("apart", [104, 105, 104])).returncode, 1)

    def test_floor_skips_short_baselines(self):
        with tempfile.TemporaryDirectory() as d:
            base = metrics_record(d, "base", [{"key": "tiny", "value": 0.1, "unit": "ms", "better": "lower"}])
            cand = metrics_record(d, "cand", [{"key": "tiny", "value": 0.2, "unit": "ms", "better": "lower"}])
            self.assertEqual(run("compare", base, cand).returncode, 1)
            self.assertEqual(run("compare", base, cand, "--floor", "0.3").returncode, 0)

    def test_unknown_record_versions_are_refused(self):
        with tempfile.TemporaryDirectory() as d:
            base = metrics_record(d, "base", [{"key": "a", "value": 1, "unit": "ms", "better": "lower"}])
            rec = json.loads(base.read_text())
            rec["gate_record"] = 2
            future = Path(d) / "future.json"
            future.write_text(json.dumps(rec))
            r = run("compare", base, future)
            self.assertEqual(r.returncode, 2)
            self.assertIn("unknown gate record version 2", r.stderr)

    def test_zero_is_an_improvement_in_the_better_direction(self):
        with tempfile.TemporaryDirectory() as d:
            base = metrics_record(d, "base", [{"key": "fallbacks", "value": 30, "unit": "q", "better": "lower"},
                                              {"key": "passed", "value": 0, "unit": "t", "better": "higher"}])
            good = metrics_record(d, "good", [{"key": "fallbacks", "value": 0, "unit": "q", "better": "lower"},
                                              {"key": "passed", "value": 5, "unit": "t", "better": "higher"}])
            bad = metrics_record(d, "bad", [{"key": "fallbacks", "value": 31, "unit": "q", "better": "lower"},
                                             {"key": "passed", "value": 0, "unit": "t", "better": "higher"}])
            self.assertEqual(run("compare", base, good).returncode, 0)
            zero = metrics_record(d, "zero", [{"key": "fallbacks", "value": 0, "unit": "q", "better": "lower"},
                                              {"key": "passed", "value": 0, "unit": "t", "better": "higher"}])
            self.assertEqual(run("compare", zero, bad).returncode, 1)
            self.assertIn("inf | slower", run("compare", zero, bad).stdout)

    def test_different_workloads_are_refused(self):
        with tempfile.TemporaryDirectory() as d:
            m = [{"key": "a", "value": 1, "unit": "ms", "better": "lower"}]
            base = metrics_record(d, "base", m, scale="20000 items")
            small = metrics_record(d, "small", m, scale="2000 items")
            r = run("compare", base, small)
            self.assertEqual(r.returncode, 2)
            self.assertIn("scale", r.stderr)
            self.assertEqual(run("compare", base, small, "--allow-different").returncode, 0)
            units = metrics_record(d, "units", [{"key": "a", "value": 1, "unit": "s", "better": "lower"}],
                                   scale="20000 items")
            self.assertIn("metric a", run("compare", base, units).stderr)

    def test_changed_fingerprints_fail(self):
        with tempfile.TemporaryDirectory() as d:
            m = {"key": "a", "value": 1, "unit": "ms", "better": "lower"}
            base = metrics_record(d, "base", [m, {"check": "q/answer", "ok": True, "fingerprint": "1 rows, x"}])
            same = metrics_record(d, "same", [m, {"check": "q/answer", "ok": True, "fingerprint": "1 rows, x"}])
            wrong = metrics_record(d, "wrong", [m, {"check": "q/answer", "ok": True, "fingerprint": "0 rows, y"}])
            self.assertEqual(run("compare", base, same).returncode, 0)
            r = run("compare", base, wrong)
            self.assertEqual(r.returncode, 1)
            self.assertIn("Changed from the baseline: q/answer", r.stdout)

    def test_executors_reads_each_cell_and_checks_answers(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d) / "run"
            (root / "people-100000-threads-1" / "results").mkdir(parents=True)
            (root / "manifest.txt").write_text("host: forge\nbinary commit: abc\nstarted: t0\n")
            res = root / "people-100000-threads-1" / "results"
            lines = ["round\tepoch\tvariant\tquery\trep\texec_ms\twall_ms"]
            for v, wall in (("A", 2.0), ("B", 1.0)):
                for rep in range(3):
                    lines.append(f"1\t1\t{v}\tcount-all\t{rep}\t{wall / 2}\t{wall + rep * 0.1}")
                    lines.append(f"1\t1\t{v}\tminus\t{rep}\t1\t3")
            res.joinpath("samples.tsv").write_text("\n".join(lines) + "\n")
            res.joinpath("answers.tsv").write_text(
                "A\tcount-all\t1\tx\nB\tcount-all\t1\tx\nA\tminus\t1\ty\nB\tminus\t1\tz\n")
            out = Path(d) / "record.json"
            r = run("executors", root, "--out", out)
            self.assertEqual(r.returncode, 0, r.stderr)
            rec = json.loads(out.read_text())
            keys = {m["key"]: m for m in rec["metrics"]}
            self.assertEqual(keys["1.05m-1t/eager/count-all/wall"]["value"], 2.1)
            self.assertEqual(keys["1.05m-1t/streaming/count-all/wall"]["value"], 1.1)
            self.assertEqual(keys["1.05m-1t/eager/count-all/exec"]["value"], 1.0)
            self.assertEqual(len(keys["1.05m-1t/streaming/minus/exec"]["samples"]), 3)
            self.assertEqual(rec["settings"]["short_below"], 0.3)
            fps = {c["name"]: c.get("fingerprint") for c in rec["checks"]}
            self.assertEqual(fps["1.05m-1t/count-all/answers"], "1 rows, x")
            checks = {c["name"]: c["ok"] for c in rec["checks"]}
            self.assertTrue(checks["1.05m-1t/count-all/answers"])
            self.assertFalse(checks["1.05m-1t/minus/answers"])
            self.assertIn("| count-all | 2.100 | 1.100 | 0.524 |", r.stdout)
            self.assertIn("minus (answers differ)", r.stdout)

    def test_unordered_limit_answers_agree_on_counts(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d) / "run"
            res = root / "people-100000-threads-1" / "results"
            res.mkdir(parents=True)
            (root / "queries").mkdir()
            (root / "queries" / "export.rq").write_text("SELECT * WHERE { ?s ?p ?o } LIMIT 5\n")
            (root / "manifest.txt").write_text("host: forge\n")
            res.joinpath("samples.tsv").write_text(
                "round\tepoch\tvariant\tquery\trep\texec_ms\twall_ms\n"
                "1\t1\tA\texport\t0\t1\t2\n1\t1\tB\texport\t0\t1\t2\n")
            res.joinpath("answers.tsv").write_text("A\texport\t5\tx\nB\texport\t5\ty\n")
            out = Path(d) / "record.json"
            self.assertEqual(run("executors", root, "--out", out).returncode, 0)
            rec = json.loads(out.read_text())
            self.assertEqual([(c["ok"], c["fingerprint"]) for c in rec["checks"]], [(True, "5")])
            self.assertEqual(run("compare", out, out).returncode, 0)

    def test_baseline_checks_must_be_in_the_candidate(self):
        with tempfile.TemporaryDirectory() as d:
            m = {"key": "a", "value": 1, "unit": "ms", "better": "lower"}
            base = metrics_record(d, "base", [m, {"check": "a/result", "ok": True, "detail": "7"}])
            bare = metrics_record(d, "bare", [m])
            r = run("compare", base, bare)
            self.assertEqual(r.returncode, 1)
            self.assertIn("lacks: a/result", r.stdout)

    def test_another_compiler_is_refused(self):
        with tempfile.TemporaryDirectory() as d:
            m = [{"key": "a", "value": 1, "unit": "ms", "better": "lower"}]
            base = metrics_record(d, "base", m)
            rec = json.loads(base.read_text())
            rec["manifest"]["rustc"] = "rustc 1.2.3"
            other = Path(d) / "other.json"
            other.write_text(json.dumps(rec))
            r = run("compare", base, other)
            self.assertEqual(r.returncode, 2)
            self.assertIn("manifest rustc", r.stderr)

    def test_records_are_validated_when_read(self):
        with tempfile.TemporaryDirectory() as d:
            m = [{"key": "a", "value": 1, "unit": "ms", "better": "lower"}]
            base = metrics_record(d, "base", m)
            bad = Path(d) / "bad.json"
            bad.write_text(base.read_text().replace('"value": 1', '"value": NaN'))
            self.assertEqual(run("compare", base, bad).returncode, 2)
            rec = json.loads(base.read_text())
            rec["metrics"].append(dict(rec["metrics"][0]))
            bad.write_text(json.dumps(rec))
            r = run("compare", base, bad)
            self.assertEqual(r.returncode, 2)
            self.assertIn("duplicate metric", r.stderr)

    def test_processes_must_agree_on_units_and_report_once(self):
        with tempfile.TemporaryDirectory() as d:
            a, b = Path(d) / "a.jsonl", Path(d) / "b.jsonl"
            a.write_text(json.dumps({"key": "x", "value": 100, "unit": "ms", "better": "lower"}) + "\n")
            b.write_text(json.dumps({"key": "x", "value": 100, "unit": "s", "better": "lower"}) + "\n")
            r = run("metrics", a, b, "--gate", "incr", "--out", Path(d) / "r.json")
            self.assertEqual(r.returncode, 2)
            self.assertIn("units", r.stderr)
            b.write_text(a.read_text() * 2)
            r = run("metrics", a, b, "--gate", "incr", "--out", Path(d) / "r.json")
            self.assertEqual(r.returncode, 2)
            self.assertIn("twice", r.stderr)

    def test_w3c_known_failures_pass(self):
        with tempfile.TemporaryDirectory() as d:
            log = Path(d) / "out.log"
            log.write_text(
                "Geo (streaming): 37 passed, 7 failed, 0 skipped (44 tests)\n"
                "Geo (streaming): 0 queries needed eager fallback\n"
                "Geo (streaming): 7 known failures, 0 unexpected failures\n"
                "test result: ok. 1 passed\n")
            out = Path(d) / "r.json"
            self.assertEqual(run("w3c", log, "--out", out).returncode, 0)
            self.assertEqual(run("compare", out, out).returncode, 0)
            log.write_text(log.read_text().replace("7 known failures, 0 unexpected", "6 known failures, 1 unexpected"))
            bad = Path(d) / "bad.json"
            run("w3c", log, "--out", bad)
            self.assertEqual(run("compare", out, bad).returncode, 1)


if __name__ == "__main__":
    unittest.main()
