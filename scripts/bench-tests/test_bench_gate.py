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
            "--manifest", "commit=abc", "--manifest", "compilers=rustc version 1", "--manifest", "build_config={}", "--scale", scale)
    assert r.returncode == 0, r.stderr
    return out


class GateTests(unittest.TestCase):
    def test_native_digest_refuses_directional_literals(self):
        with tempfile.TemporaryDirectory() as d:
            query, answer = Path(d) / "q.rq", Path(d) / "answer.json"
            query.write_text("SELECT ?x WHERE { ?s ?p ?x }")
            for direction in ("ltr", "rtl", "up", None):
                answer.write_text(json.dumps({"queryType": "SELECT", "vars": ["x"], "rows": [[{
                    "type": "literal", "value": "text", "xml:lang": "en", "its:dir": direction,
                }]]}))
                result = run("native-digest", query, answer)
                with self.subTest(direction=direction):
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("directional", result.stderr)

    def test_native_digest_rejects_graph_and_unknown_kinds(self):
        with tempfile.TemporaryDirectory() as d:
            query, answer = Path(d) / "q.rq", Path(d) / "answer.json"
            query.write_text("SELECT ?s WHERE { ?s ?p ?o }")
            for kind in ("CONSTRUCT", "DESCRIBE", "UNKNOWN", None):
                answer.write_text(json.dumps({"queryType": kind, "triples": []}))
                result = run("native-digest", query, answer)
                with self.subTest(kind=kind):
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("SELECT and ASK", result.stderr)

    def test_native_digest_requires_an_actual_ask_boolean(self):
        with tempfile.TemporaryDirectory() as d:
            query, answer = Path(d) / "q.rq", Path(d) / "answer.json"
            query.write_text("ASK { ?s ?p ?o }")
            for value in (True, False):
                answer.write_text(json.dumps({"queryType": "ASK", "boolean": value}))
                result = run("native-digest", query, answer)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout.strip(), f"ask {value}")
            for value in (None, 0, 1, "false"):
                answer.write_text(json.dumps({"queryType": "ASK", "boolean": value}))
                with self.subTest(value=value):
                    self.assertNotEqual(run("native-digest", query, answer).returncode, 0)

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
            (root / "manifest.txt").write_text("host: forge\ncompilers: rustc version 1\nbuild config: {}\n")
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
                r = run("metrics", *files, "--gate", "insn-screen", "--out", out, "--threshold", 0,
                        "--manifest", "compilers=rustc version 1", "--manifest", "build_config={}")
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
            (root / "manifest.txt").write_text("host: forge\ncompilers: rustc version 1\nbuild config: {}\n")
            res.joinpath("samples.tsv").write_text(
                "round\tepoch\tvariant\tquery\trep\texec_ms\twall_ms\n"
                "1\t1\tA\texport\t0\t1\t2\n1\t1\tB\texport\t0\t1\t2\n")
            res.joinpath("answers.tsv").write_text("A\texport\t5\tx\nB\texport\t5\ty\n")
            out = Path(d) / "record.json"
            self.assertEqual(run("executors", root, "--out", out).returncode, 0)
            rec = json.loads(out.read_text())
            self.assertEqual([(c["ok"], c["fingerprint"]) for c in rec["checks"]], [(True, "5 rows")])
            self.assertEqual(run("compare", out, out).returncode, 0)
            # one mode repeating a row keeps the count but not the distinct rows
            res.joinpath("answers.tsv").write_text(
                "A\texport\t5\tx\tdistinct:5\nB\texport\t5\ty\tdistinct:1\n")
            self.assertEqual(run("executors", root, "--out", out).returncode, 0)
            rec = json.loads(out.read_text())
            self.assertEqual([(c["ok"], c["fingerprint"]) for c in rec["checks"]], [(False, "5 rows, distinct:5")])

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
            rec["manifest"]["compilers"] = "GCC: (GNU) 16; rustc version 1"
            other = Path(d) / "other.json"
            other.write_text(json.dumps(rec))
            r = run("compare", base, other)
            self.assertEqual(r.returncode, 2)
            self.assertIn("manifest compilers", r.stderr)
            del rec["manifest"]["compilers"]
            other.write_text(json.dumps(rec))
            r = run("compare", base, other)
            self.assertEqual(r.returncode, 2)
            self.assertIn("compilers: unknown in the candidate", r.stderr)
            rec = json.loads(base.read_text())
            rec["manifest"]["build_config"] = '{"env":{"CARGO_PROFILE_RELEASE_LTO":"off"}}'
            other.write_text(json.dumps(rec))
            self.assertIn("manifest build_config", run("compare", base, other).stderr)
            del rec["manifest"]["build_config"]
            other.write_text(json.dumps(rec))
            self.assertIn("build_config: unknown in the candidate", run("compare", base, other).stderr)

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

    def test_keys_select_metrics_but_not_checks(self):
        with tempfile.TemporaryDirectory() as d:
            m = {"key": "q1", "value": 1, "unit": "ms", "better": "lower"}
            base = metrics_record(d, "base", [m, {"check": "q1/answer", "ok": True, "fingerprint": "1 rows, x"}])
            wrong = metrics_record(d, "wrong", [m, {"check": "q1/answer", "ok": True, "fingerprint": "0 rows, y"}])
            self.assertEqual(run("compare", base, wrong, "--keys", "^q1$").returncode, 1)

    def test_a_malformed_process_check_is_refused(self):
        with tempfile.TemporaryDirectory() as d:
            a, b = Path(d) / "a.jsonl", Path(d) / "b.jsonl"
            a.write_text(json.dumps({"check": "c", "ok": True}) + "\n")
            b.write_text(json.dumps({"check": "c", "ok": "false"}) + "\n")
            r = run("metrics", a, b, "--gate", "incr", "--out", Path(d) / "r.json")
            self.assertEqual(r.returncode, 2)
            self.assertIn("ok must be", r.stderr)

    def test_order_digests_respect_ties_and_order(self):
        with tempfile.TemporaryDirectory() as d:
            q = Path(d) / "q.rq"
            q.write_text("SELECT ?p ?n WHERE { ?p <name> ?n } ORDER BY ?n")
            def digest(rows):
                t = Path(d) / "a.tsv"
                t.write_text("?p\t?n\n" + "".join(f"{p}\t{n}\n" for p, n in rows))
                return run("order-digest", q, t).stdout.strip()
            a = digest([("<a>", '"x"'), ("<b>", '"x"'), ("<c>", '"y"')])
            self.assertEqual(a, digest([("<b>", '"x"'), ("<a>", '"x"'), ("<c>", '"y"')]))
            self.assertNotEqual(a, digest([("<c>", '"y"'), ("<a>", '"x"'), ("<b>", '"x"')]))
            # an expression key orders every row
            q.write_text("SELECT ?p ?a WHERE { ?p <age> ?a } ORDER BY DESC(ABS(?a - 50)) ?p LIMIT 10")
            e = digest([("<a>", "90"), ("<b>", "10"), ("<c>", "50")])
            self.assertTrue(e.startswith("rows:"))
            self.assertNotEqual(e, digest([("<c>", "50"), ("<b>", "10"), ("<a>", "90")]))
            q.write_text("SELECT ?p WHERE { ?p <age> ?a }")
            self.assertTrue(digest([("<a>", "90"), ("<a>", "90")]).startswith("distinct:1 schema:"))

    def test_executors_compares_the_order_of_ordered_answers(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d) / "run"
            res = root / "people-100000-threads-1" / "results"
            res.mkdir(parents=True)
            (root / "manifest.txt").write_text("host: forge\ncompilers: rustc version 1\nbuild config: {}\n")
            res.joinpath("samples.tsv").write_text(
                "round\tepoch\tvariant\tquery\trep\texec_ms\twall_ms\n"
                "1\t1\tA\tsorted\t0\t1\t2\n1\t1\tB\tsorted\t0\t1\t2\n")
            res.joinpath("answers.tsv").write_text("A\tsorted\t3\tx\to1\nB\tsorted\t3\tx\to2\n")
            out = Path(d) / "record.json"
            self.assertEqual(run("executors", root, "--out", out).returncode, 0)
            rec = json.loads(out.read_text())
            self.assertEqual([(c["ok"], c["fingerprint"]) for c in rec["checks"]], [(False, "3 rows, x, order o1")])

    def test_compilers_come_from_the_elf_comment(self):
        with tempfile.TemporaryDirectory() as d:
            text = Path(d) / "t.txt"
            text.write_text("not ELF")
            self.assertEqual(run("compilers", text).returncode, 2)

    def test_build_config_sees_profile_overrides(self):
        env = dict(os.environ, CARGO_PROFILE_RELEASE_LTO="off")
        a = run("build-config", "--root", SCRIPTS.parent, "--cargo", "build").stdout
        b = subprocess.run([sys.executable, str(GATE), "build-config", "--root", str(SCRIPTS.parent), "--cargo", "build"],
                           capture_output=True, text=True, env=env).stdout
        self.assertIn('"release"', a)
        self.assertNotEqual(a, b)

    def test_build_config_reads_cargo_home_and_incremental(self):
        with tempfile.TemporaryDirectory() as d:
            home = Path(d) / "cargo"
            home.mkdir()
            (home / "config").write_text("[profile.release]\nopt-level = 0\n")
            env = dict(os.environ, CARGO_HOME=str(home))
            def config(**extra):
                return subprocess.run([sys.executable, str(GATE), "build-config", "--root", str(SCRIPTS.parent), "--cargo", "build"],
                                      capture_output=True, text=True, env=dict(env, **extra)).stdout
            a = config()
            self.assertIn('"opt-level":0', a)
            (home / "config").write_text("[profile.release]\nopt-level = 3\n")
            self.assertNotEqual(a, config())
            self.assertNotEqual(config(), config(CARGO_INCREMENTAL="1"))

    @unittest.skipUnless(__import__("shutil").which("mbx"), "needs mbx")
    def test_build_config_holds_the_boxington_policy(self):
        def config(**extra):
            return subprocess.run([sys.executable, str(GATE), "build-config", "--root", str(SCRIPTS.parent), "--cargo", "build"],
                                  capture_output=True, text=True, env=dict(os.environ, **extra)).stdout
        self.assertNotEqual(config(MBX_INCREMENTAL="off"), config(MBX_INCREMENTAL="on"))

    def test_build_config_reads_cargo_env_tables(self):
        with tempfile.TemporaryDirectory() as d:
            (Path(d) / "Cargo.toml").write_text("[profile.release]\nlto = true\n")
            (Path(d) / ".cargo").mkdir()
            cfg = Path(d) / ".cargo" / "config.toml"
            cfg.write_text('[env]\nCFLAGS = { value = "-O0", force = true }\n')
            a = run("build-config", "--root", d, "--cargo", "build").stdout
            cfg.write_text('[env]\nCFLAGS = { value = "-O3", force = true }\n')
            self.assertNotEqual(a, run("build-config", "--root", d, "--cargo", "build").stdout)

    def test_matrix_queries_must_match(self):
        with tempfile.TemporaryDirectory() as d:
            def record(name, text):
                root = Path(d) / name
                res = root / "people-100000-threads-1" / "results"
                res.mkdir(parents=True)
                (root / "queries").mkdir()
                (root / "queries" / "count-all.rq").write_text(text)
                (root / "manifest.txt").write_text("host: forge\ncompilers: rustc version 1\nbuild config: {}\n")
                res.joinpath("samples.tsv").write_text(
                    "round\tepoch\tvariant\tquery\trep\texec_ms\twall_ms\n"
                    "1\t1\tA\tcount-all\t0\t1\t2\n1\t1\tB\tcount-all\t0\t1\t2\n")
                res.joinpath("answers.tsv").write_text("A\tcount-all\t1\tx\nB\tcount-all\t1\tx\n")
                out = Path(d) / f"{name}.json"
                self.assertEqual(run("executors", root, "--out", out).returncode, 0)
                return out
            a = record("a", "SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }")
            b = record("b", "SELECT (COUNT(?s) AS ?n) WHERE { ?s ?p ?o }")
            self.assertEqual(run("compare", a, a).returncode, 0)
            r = run("compare", a, b)
            self.assertEqual(r.returncode, 2)
            self.assertIn("queries_sha256", r.stderr)

    def test_timed_answers_must_agree_across_server_processes(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d) / "run"
            res = root / "people-100000-threads-1" / "results"
            res.mkdir(parents=True)
            (root / "queries").mkdir()
            q = root / "queries" / "sorted.rq"
            q.write_text("SELECT ?p ?n WHERE { ?p <name> ?n } ORDER BY ?n")
            (root / "manifest.txt").write_text("host: forge\ncompilers: rustc version 1\nbuild config: {}\n")
            res.joinpath("samples.tsv").write_text(
                "round\tepoch\tvariant\tquery\trep\texec_ms\twall_ms\n"
                "1\t1\tA\tsorted\t0\t1\t2\n1\t2\tB\tsorted\t0\t1\t2\n")
            res.joinpath("answers.tsv").write_text("A\tsorted\t2\tx\tk\nB\tsorted\t2\tx\tk\n")
            def native(rows):
                f = Path(d) / "resp.json"
                f.write_text(json.dumps({"queryType": "SELECT", "vars": ["p", "n"], "rows": [
                    [{"type": "uri", "value": p}, {"type": "literal", "value": n}] for p, n in rows]}))
                return run("native-digest", q, f).stdout.strip()
            good = native([("a", "x"), ("b", "y")])
            self.assertTrue(good.startswith("2 rows, "))
            self.assertNotEqual(good, native([("b", "y"), ("a", "x")]))
            f = Path(d) / "resp.json"
            body = json.loads(f.read_text())
            body["vars"] = ["p", "other"]
            f.write_text(json.dumps(body))
            self.assertNotEqual(run("native-digest", q, f).stdout.split(", ")[1], good.split(", ")[1])
            # an unordered LIMIT keeps the count and the schema
            limit = Path(d) / "limit.rq"
            limit.write_text("SELECT ?p ?n WHERE { ?p <name> ?n } LIMIT 2")
            renamed = run("native-digest", limit, f).stdout.strip()
            body["vars"] = ["p", "n"]
            f.write_text(json.dumps(body))
            plain = run("native-digest", limit, f).stdout.strip()
            self.assertNotEqual(",".join(renamed.split(",")[:2]), ",".join(plain.split(",")[:2]))
            body["vars"] = ["p"]
            f.write_text(json.dumps(body))
            self.assertFalse(run("native-digest", q, f).stdout.startswith("2 rows"))
            out = Path(d) / "record.json"
            res.joinpath("warm-answers.tsv").write_text(f"1\tA\tsorted\t{good}\n2\tB\tsorted\t{good}\n")
            self.assertEqual(run("executors", root, "--out", out).returncode, 0)
            checks = {c["name"]: c for c in json.loads(out.read_text())["checks"]}
            self.assertTrue(checks["1.05m-1t/sorted/timed-answers"]["ok"])
            # a wrong answer in an earlier timed repetition fails, though the last is right
            res.joinpath("warm-answers.tsv").write_text(
                f"1\tA\tsorted\t1\t0 rows, e\n1\tA\tsorted\t2\t{good}\n2\tB\tsorted\t1\t{good}\n")
            self.assertEqual(run("executors", root, "--out", out).returncode, 0)
            checks = {c["name"]: c for c in json.loads(out.read_text())["checks"]}
            self.assertFalse(checks["1.05m-1t/sorted/timed-answers"]["ok"])

    def test_runtime_switches_are_part_of_the_workload(self):
        with tempfile.TemporaryDirectory() as d:
            src = Path(d) / "m.jsonl"
            src.write_text(json.dumps({"key": "a", "value": 1, "unit": "ms", "better": "lower"}) + "\n")
            def record(name, **env):
                out = Path(d) / f"{name}.json"
                r = subprocess.run([sys.executable, str(GATE), "metrics", str(src), "--gate", "incr", "--out", str(out),
                                    "--manifest", "compilers=c", "--manifest", "build_config={}"],
                                   capture_output=True, text=True,
                                   env={k: v for k, v in dict(os.environ, **env).items()})
                self.assertEqual(r.returncode, 0, r.stderr)
                return out
            plain = record("plain")
            switched = record("switched", SPARKLES_DISABLE_OPTIMIZATIONS="metadata_counts")
            self.assertEqual(run("compare", plain, plain).returncode, 0)
            r = run("compare", plain, switched)
            self.assertEqual(r.returncode, 2)
            self.assertIn("runtime_env", r.stderr)

    def test_build_config_holds_the_cargo_arguments(self):
        a = run("build-config", "--root", SCRIPTS.parent, "--cargo", "build -p sparkles-server").stdout
        b = run("build-config", "--root", SCRIPTS.parent, "--cargo",
                "build -p sparkles-server --no-default-features --features fmt").stdout
        self.assertNotEqual(a, b)

    def test_a_baseline_with_failed_checks_is_refused(self):
        with tempfile.TemporaryDirectory() as d:
            m = {"key": "a", "value": 1, "unit": "ms", "better": "lower"}
            bad = metrics_record(d, "bad", [m, {"check": "q/answers", "ok": False, "fingerprint": "1 rows, x"}])
            good = metrics_record(d, "good", [m, {"check": "q/answers", "ok": True, "fingerprint": "1 rows, x"}])
            r = run("compare", bad, good)
            self.assertEqual(r.returncode, 2)
            self.assertIn("baseline has failed checks", r.stderr)

    def test_unordered_exports_must_be_triples_of_the_input(self):
        with tempfile.TemporaryDirectory() as d:
            data = Path(d) / "data.nt"
            data.write_text('<a> <p> "x"@en .\n<a> <q> "5"^^<http://www.w3.org/2001/XMLSchema#integer> .\n<b> <p> <c> .\n')
            q = Path(d) / "q.rq"
            q.write_text("PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> SELECT ?s ?p ?o WHERE { ?s ?p ?o } LIMIT 3")
            t = Path(d) / "a.tsv"
            t.write_text('?s\t?p\t?o\n<a>\t<p>\t"x"@en\n<a>\t<q>\t5\n')
            ok = run("order-digest", q, t, "--data", data).stdout.split()
            self.assertEqual((ok[0], ok[2]), ("distinct:2", "missing:0"))
            # a permuted header changes the schema
            t.write_text('?o\t?p\t?s\n<a>\t<p>\t"x"@en\n<a>\t<q>\t5\n')
            self.assertNotEqual(run("order-digest", q, t, "--data", data).stdout.split()[1], ok[1])
            t.write_text('?s\t?p\t?o\n<z>\t<p>\t"x"@en\n<a>\t<q>\t5\n')
            self.assertEqual(run("order-digest", q, t, "--data", data).stdout.split()[2], "missing:1")
            # a three-column answer that is not a triple export is not checked against the input
            oc = Path(d) / "oc.rq"
            oc.write_text("SELECT ?p ?o ?c WHERE { ?p <knows> ?o OPTIONAL { ?o <c> ?c } } LIMIT 3")
            t.write_text('?p\t?o\t?c\n<z>\t<y>\t<x>\n')
            self.assertNotIn("missing", run("order-digest", oc, t, "--data", data).stdout)
            f = Path(d) / "resp.json"
            def native(s_):
                f.write_text(json.dumps({"queryType": "SELECT", "vars": ["s", "p", "o"], "rows": [
                    [{"type": "uri", "value": s_}, {"type": "uri", "value": "p"},
                     {"type": "literal", "value": "x", "xml:lang": "en"}]]}))
                return run("native-digest", q, f, "--data", data).stdout
            self.assertIn("missing 0", native("a"))
            self.assertIn("missing 1", native("z"))
            # several answers in one call give a line each, in order, and the check against
            # the input of a repeated answer still holds for a changed one between them
            files = []
            for i, s_ in enumerate(["a", "z", "a"]):
                g = Path(d) / f"timed-{i}.json"
                g.write_text(json.dumps({"queryType": "SELECT", "vars": ["s", "p", "o"], "rows": [
                    [{"type": "uri", "value": s_}, {"type": "uri", "value": "p"},
                     {"type": "literal", "value": "x", "xml:lang": "en"}]],
                    "meta": {"timing": {"execMs": i}}}))
                files.append(g)
            files.append(Path(d) / "absent.json")
            lines = run("native-digest", q, *files, "--data", data).stdout.splitlines()
            self.assertEqual(len(lines), 4)
            self.assertEqual(lines[0], lines[2])
            self.assertIn("missing 0", lines[0])
            self.assertIn("missing 1", lines[1])
            self.assertEqual(lines[3], "unreadable")
            # values that hold the delimiters of a tab-joined encoding cannot make another
            # answer's rows look alike, nor share its check against the input
            u = lambda v: {"type": "uri", "value": v}
            lit = lambda v: {"type": "literal", "value": v}
            rows_a = [[u("a"), u("p"), lit("x")], [u("b"), u("p"), lit("y||\nuri|c||\turi|p||\tliteral|z")]]
            rows_b = [[u("a"), u("p"), lit("x||\nuri|b||\turi|p||\tliteral|y")], [u("c"), u("p"), lit("z")]]
            pair = []
            for name, rows in (("ca", rows_a), ("cb", rows_b)):
                g = Path(d) / f"{name}.json"
                g.write_text(json.dumps({"queryType": "SELECT", "vars": ["s", "p", "o"], "rows": rows}))
                pair.append(g)
            alone = [run("native-digest", q, g, "--data", data).stdout.strip() for g in pair]
            both = run("native-digest", q, *pair, "--data", data).stdout.splitlines()
            self.assertEqual(both, alone)
            self.assertNotEqual(alone[0].split(", ")[-1], alone[1].split(", ")[-1])

    def test_cell_summary_reads_order_digests(self):
        # a matrix cell's answers.tsv has a fifth column of order digests
        with tempfile.TemporaryDirectory() as d:
            res = Path(d) / "results"
            res.mkdir()
            res.joinpath("samples.tsv").write_text(
                "round\tepoch\tvariant\tquery\trep\texec_ms\twall_ms\n"
                "1\t1\tA\tsorted\t1\t1\t2\n1\t2\tB\tsorted\t1\t1\t2\n")
            res.joinpath("answers.tsv").write_text("A\tsorted\t2\tx\tkeys:k\nB\tsorted\t2\tx\tkeys:j\n")
            r = subprocess.run([sys.executable, str(SCRIPTS / "nsc-bench-summary.py"), d],
                               capture_output=True, text=True)
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertIn("Answers differ or failed for: sorted", r.stdout)


if __name__ == "__main__":
    unittest.main()
