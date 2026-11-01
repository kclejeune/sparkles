"""Tests of scripts/bench_hyperfine.py and its readers on real hyperfine 1.20 and 2.0 output.

The files in hyperfine/ are unedited exports. v1*.json come from hyperfine 1.20.0 and
v2*.json from hyperfine 2.0.0, each pair from the same invocation:

    v.json         --shell=sh --warmup 1 --runs 3 --ignore-failure --prepare true
                   --command-name sparkles "sleep 0.01 > /dev/null"
                   --command-name jena-fuseki "sleep 0.01; exit 28"
    v-scan.json    --shell=sh --runs 2 --ignore-failure -P x 0 1 "sleep 0.01; exit {x}"
    v-none.json    -N --runs 2 --command-name q-exact-true "sleep 0.01"
    v-single.json  --shell=sh --runs 1 --command-name load "sleep 0.01"
"""

import copy
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

SCRIPTS = Path(__file__).resolve().parents[1]
FIXTURES = Path(__file__).resolve().parent / "hyperfine"
sys.path.insert(0, str(SCRIPTS))
import bench_hyperfine  # noqa: E402

V1_FIELDS = {"command", "mean", "stddev", "median", "user", "system", "min", "max", "times",
             "memory_usage_byte", "exit_codes"}


def raw(name):
    return json.loads((FIXTURES / name).read_text())


class NormalizeTests(unittest.TestCase):
    def test_hyperfine_1_passes_through_unchanged(self):
        for name in ("v1.json", "v1-scan.json", "v1-none.json", "v1-single.json"):
            with self.subTest(name=name):
                self.assertEqual(bench_hyperfine.normalize(raw(name)), raw(name))

    def test_hyperfine_2_gives_the_fields_of_hyperfine_1(self):
        for suffix in ("", "-scan", "-none", "-single"):
            with self.subTest(suffix=suffix):
                old = raw(f"v1{suffix}.json")
                new = bench_hyperfine.normalize(raw(f"v2{suffix}.json"))
                self.assertEqual(set(new), {"results"})
                self.assertEqual([r["command"] for r in new["results"]], [r["command"] for r in old["results"]])
                for a, b in zip(old["results"], new["results"]):
                    self.assertEqual(set(b) - {"hyperfine_version"}, set(a))
                    self.assertEqual(b["hyperfine_version"], "2.0.0")
                    self.assertEqual(len(b["times"]), len(a["times"]))
                    self.assertEqual(b["exit_codes"], a["exit_codes"])
                    self.assertEqual(b.get("parameters"), a.get("parameters"))

    def test_hyperfine_2_values_come_from_the_runs_and_the_wall_clock_summary(self):
        doc = raw("v2.json")
        for r, n in zip(doc["results"], bench_hyperfine.normalize(doc)["results"]):
            wall = r["summary"]["time_wall_clock"]
            self.assertEqual(n["command"], r["name"])
            self.assertEqual(n["times"], [m["time_wall_clock"]["value"] for m in r["measurements"]])
            self.assertEqual(n["exit_codes"], [m["exit_code"] for m in r["measurements"]])
            for key in ("mean", "stddev", "median", "min", "max"):
                self.assertEqual(n[key], wall[key])
            self.assertEqual(n["user"], r["summary"]["time_user"]["mean"])
            self.assertEqual(n["system"], r["summary"]["time_system"]["mean"])
            self.assertTrue(all(isinstance(x, int) for x in n["memory_usage_byte"]))

    def test_failed_runs_under_ignore_failure_keep_their_exit_codes(self):
        n = bench_hyperfine.normalize(raw("v2.json"))
        self.assertEqual({r["command"]: r["exit_codes"] for r in n["results"]},
                         {"sparkles": [0, 0, 0], "jena-fuseki": [28, 28, 28]})

    def test_a_single_run_has_no_standard_deviation(self):
        (r,) = bench_hyperfine.normalize(raw("v2-single.json"))["results"]
        self.assertIsNone(r["stddev"])
        self.assertEqual(r["median"], r["times"][0])

    def test_a_missing_summary_is_computed_from_the_runs(self):
        doc = raw("v2.json")
        for r in doc["results"]:
            del r["summary"]
        expected = bench_hyperfine.normalize(raw("v2.json"))["results"]
        for a, b in zip(expected, bench_hyperfine.normalize(doc)["results"]):
            for key in ("mean", "median", "min", "max", "stddev", "user", "system"):
                self.assertAlmostEqual(a[key], b[key], places=12)

    def test_normalizing_is_idempotent(self):
        once = bench_hyperfine.normalize(raw("v2.json"))
        self.assertEqual(bench_hyperfine.normalize(copy.deepcopy(once)), once)

    def test_unknown_schema_versions_are_refused(self):
        for version in (0, 3, "2"):
            with self.subTest(version=version):
                doc = raw("v2.json")
                doc["schema_version"] = version
                with self.assertRaisesRegex(bench_hyperfine.FormatError, "schema_version"):
                    bench_hyperfine.normalize(doc)
        with self.assertRaises(bench_hyperfine.FormatError):
            bench_hyperfine.normalize({"results": {}})

    def test_a_run_without_wall_clock_time_is_refused(self):
        doc = raw("v2.json")
        del doc["results"][0]["measurements"][1]["time_wall_clock"]
        with self.assertRaisesRegex(bench_hyperfine.FormatError, "time_wall_clock"):
            bench_hyperfine.normalize(doc)


class MergeTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.d = Path(self.tmp.name)

    def cli(self, *args):
        return subprocess.run([sys.executable, "-I", str(SCRIPTS / "bench_hyperfine.py"), *args],
                              capture_output=True, text=True)

    def test_merges_never_mix_the_formats(self):
        # Either order of an older and a newer results file gives the hyperfine 1 layout.
        for dest_name, new_name in (("v1.json", "v2.json"), ("v2.json", "v1.json"), ("v2.json", "v2-none.json")):
            with self.subTest(dest=dest_name, new=new_name):
                dest, new = self.d / "dest.json", self.d / "new.json"
                shutil.copy(FIXTURES / dest_name, dest)
                shutil.copy(FIXTURES / new_name, new)
                p = self.cli("merge", str(new), str(dest))
                self.assertEqual(p.returncode, 0, p.stderr)
                merged = json.loads(dest.read_text())
                self.assertNotIn("schema_version", merged)
                for r in merged["results"]:
                    self.assertNotIn("measurements", r)
                    self.assertLessEqual(V1_FIELDS, set(r))
                names = [r["command"] for r in merged["results"]]
                self.assertEqual(len(names), len(set(names)))
                fresh = {r["command"]: r for r in bench_hyperfine.normalize(raw(new_name))["results"]}
                for r in merged["results"]:
                    if r["command"] in fresh:
                        self.assertEqual(r, fresh[r["command"]])

    def test_merge_into_a_missing_file_and_from_a_missing_file(self):
        dest = self.d / "dest.json"
        shutil.copy(FIXTURES / "v2.json", self.d / "new.json")
        self.assertEqual(self.cli("merge", str(self.d / "new.json"), str(dest)).returncode, 0)
        self.assertEqual(json.loads(dest.read_text()), bench_hyperfine.normalize(raw("v2.json")))
        p = self.cli("merge", str(self.d / "absent.json"), str(dest))
        self.assertNotEqual(p.returncode, 0)

    def test_merge_refuses_an_unknown_schema_and_leaves_the_destination(self):
        doc = raw("v2.json")
        doc["schema_version"] = 3
        (self.d / "new.json").write_text(json.dumps(doc))
        shutil.copy(FIXTURES / "v1.json", self.d / "dest.json")
        p = self.cli("merge", str(self.d / "new.json"), str(self.d / "dest.json"))
        self.assertNotEqual(p.returncode, 0)
        self.assertIn("schema_version", p.stderr)
        self.assertEqual(json.loads((self.d / "dest.json").read_text()), raw("v1.json"))

    def test_has_finds_names_in_either_format(self):
        for name in ("v1.json", "v2.json"):
            with self.subTest(name=name):
                f = str(FIXTURES / name)
                self.assertEqual(self.cli("has", f, "sparkles", "jena-fuseki").returncode, 0)
                self.assertEqual(self.cli("has", f, "sparkles", "qlever").returncode, 1)

    def test_shell_merge_helpers_use_the_module(self):
        # Every runner's merge goes through bench_hyperfine.py, which writes the 1.x layout.
        for script in ("bench-lib.sh", "bench-billion.sh", "bench-geo.sh", "bench-text.sh",
                       "bench-watdiv.sh", "bench-shacl-write.sh"):
            with self.subTest(script=script):
                source = (SCRIPTS / script).read_text()
                body = "merge() {" + source.split("\nmerge() {", 1)[1].split("\n}", 1)[0] + "\n}"
                dest, new = self.d / "dest.json", self.d / "new.json"
                shutil.copy(FIXTURES / "v1.json", dest)
                shutil.copy(FIXTURES / "v2-none.json", new)
                p = subprocess.run(["bash", "-euc", body + '\nmerge "$1" "$2"', "bash", str(new), str(dest)],
                                   env=os.environ | {"ROOT": str(SCRIPTS.parent)}, capture_output=True, text=True)
                self.assertEqual(p.returncode, 0, p.stderr)
                self.assertFalse(new.exists())
                merged = json.loads(dest.read_text())
                self.assertEqual([r["command"] for r in merged["results"]], ["sparkles", "jena-fuseki", "q-exact-true"])


class ReaderTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.d = Path(self.tmp.name)

    def build(self, query_file):
        res = self.d / query_file / "results"
        res.mkdir(parents=True)
        shutil.copy(FIXTURES / query_file, res / "q1.json")
        p = subprocess.run([sys.executable, str(SCRIPTS / "bench-record.py"), "build", "--suite", "test", "--scale", "1",
                            "--results", str(res), "--manifest", "", "--out", str(res / "record.json"), "--queries", "q1"],
                           capture_output=True, text=True)
        self.assertEqual(p.returncode, 0, p.stderr)
        return json.loads((res / "record.json").read_text()), res

    def test_record_builder_reads_both_formats_alike(self):
        old, _ = self.build("v1.json")
        new, _ = self.build("v2.json")
        shape = lambda rec: [(r["engine"], r["mode"], r["status"], len(r["samples_ms"])) for r in rec["results"]]
        self.assertEqual(shape(old), shape(new))
        self.assertEqual(shape(new), [("sparkles", "warm", "ok", 3), ("jena", "warm", "timeout", 0)])

    def test_summary_reads_hyperfine_2(self):
        _, res = self.build("v2.json")
        p = subprocess.run([sys.executable, str(SCRIPTS / "bench-summary.py"), str(res), "q1"],
                           capture_output=True, text=True)
        self.assertEqual(p.returncode, 0, p.stderr)
        table = (res / "summary.md").read_text() if (res / "summary.md").exists() else p.stdout
        self.assertIn("3/3 runs failed", table)

    def test_watdiv_summary_reads_hyperfine_2(self):
        res = self.d / "watdiv"
        res.mkdir()
        shutil.copy(FIXTURES / "v2.json", res / "C1-1.json")
        p = subprocess.run([sys.executable, str(SCRIPTS / "bench-watdiv" / "summary.py"), str(res), "1", "scale=1", "C1-1"],
                           capture_output=True, text=True)
        self.assertEqual(p.returncode, 0, p.stderr)

    def test_sp2b_qualifier_reads_a_hyperfine_2_timing_source(self):
        spec = importlib.util.spec_from_file_location("sp2b_qualify_tests", Path(__file__).with_name("test_bench_sp2b_qualify.py"))
        tests = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(tests)
        case = tests.QualificationTests()
        v2 = raw("v2.json")
        result = copy.deepcopy(v2["results"][0])
        result["name"] = "sparkles"
        result["measurements"] = result["measurements"][:2]
        for m, t in zip(result["measurements"], (1, 2)):
            m["time_wall_clock"]["value"] = t
        result["summary"]["time_wall_clock"] = {"unit": "second", "count": 2, "mean": 1.5, "stddev": 0.5 ** 0.5,
                                                "median": 1.5, "min": 1, "max": 2}
        v2["results"] = [result]
        with tempfile.TemporaryDirectory() as directory:
            bindings, documents = case.fixture(directory)
            case.write(directory, bindings, "q4_timing", v2)
            derived, _ = tests.qualifier.qualify(directory, bindings)
            self.assertTrue(derived["results"][0]["answer"]["agrees"])
            result["summary"]["time_wall_clock"]["mean"] = 1.6
            case.write(directory, bindings, "q4_timing", v2)
            with self.assertRaisesRegex(ValueError, "stale raw timing summary: mean"):
                tests.qualifier.qualify(directory, bindings)


if __name__ == "__main__":
    unittest.main()
