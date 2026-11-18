"""Exercise the host-timing test group: its definition, the gate's exclusion and the checks of its runs."""
import importlib.util
import json
import os
import re
import tempfile
import tomllib
from pathlib import Path
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, ROOT / path)
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


tc = load("timing_cohort", "scripts/timing-cohort.py")
cc = load("changed_crates_t", "scripts/changed-crates.py")


def cohort_filter():
    cfg = tomllib.loads((ROOT / ".config/nextest.toml").read_text())
    hits = [o for o in cfg["profile"]["default"]["overrides"] if o.get("test-group") == "host-timing"]
    return cfg, hits


class Definition(unittest.TestCase):
    def test_one_override_names_each_test_exactly(self):
        cfg, hits = cohort_filter()
        self.assertEqual(cfg["test-groups"]["host-timing"], {"max-threads": 1})
        self.assertEqual(len(hits), 1)
        terms = re.findall(r"\(package\(([a-z-]+)\)(?: & binary\(([a-z_]+)\))? & test\(=([A-Za-z0-9_:]+)\)\)", hits[0]["filter"])
        self.assertEqual(len(terms), hits[0]["filter"].count("test("))
        self.assertGreater(len(terms), 0)
        for package, binary, name in terms:
            crate = ROOT / "crates" / package
            fn = name.split("::")[-1]
            where = [crate / "tests" / f"{binary}.rs"] if binary else list((crate / "src").rglob("*.rs"))
            self.assertTrue(any(re.search(rf"\bfn {fn}\(", p.read_text()) for p in where), name)

    def test_the_gate_sets_the_exclusion_and_mise_keys_it(self):
        self.assertIn("export SPARKLES_TEST_EXCLUDE_GROUP=host-timing",
                      (ROOT / "scripts/gate/remote/gate-inner.sh").read_text())
        mise = tomllib.loads((ROOT / "mise.toml").read_text())
        self.assertIn("SPARKLES_TEST_EXCLUDE_GROUP", mise["task_config"]["global_env"])

    def test_the_exclusion_is_part_of_the_one_filter(self):
        cases = [
            ({}, "--workspace"),
            ({"SPARKLES_TEST_EXCLUDE_GROUP": "host-timing"}, "--workspace -E !group(host-timing)"),
            ({"SPARKLES_TEST_EXCLUDE_GROUP": "host-timing", "SPARKLES_CHANGED_PACKAGES": "a b"},
             "--workspace -E (package(a)|package(b))&!group(host-timing)"),
            ({"SPARKLES_CHANGED_PACKAGES": "a"}, "--workspace -E package(a)"),
            ({"SPARKLES_TEST_EXCLUDE_GROUP": "host-timing", "SPARKLES_CHANGED_PACKAGES": "none"}, "none"),
        ]
        for env, want in cases:
            with mock.patch.dict(os.environ, env, clear=True):
                self.assertEqual(cc.cargo_args("nextest"), want, env)
                self.assertNotIn("group", cc.cargo_args())
        with mock.patch.dict(os.environ, {"SPARKLES_TEST_EXCLUDE_GROUP": "a b"}, clear=True):
            with self.assertRaises(SystemExit):
                cc.cargo_args("nextest")


def run_dir(d, name, cases, selected=None, skipped=(), **ident):
    """A run directory with a JUnit report of CASES, (binary, test, seconds, failed), and a
    run.json that selects SELECTED, by default the cases' tests."""
    p = Path(d) / name
    p.mkdir()
    body = "".join(
        f'<testsuite name="{b}"><testcase name="{n}" time="{t}">'
        f'{"<failure/>" if f else ""}{"<skipped/>" if (b, n) in skipped else ""}</testcase></testsuite>'
        for b, n, t, f in cases)
    (p / "junit.xml").write_text(f"<testsuites>{body}</testsuites>")
    base = {"version": 1, "run_id": name, "commit": "c1", "worker": "w1", "rustc": "rustc 1",
            "build": "b1", "nextest_exit": 100 if any(f for *_, f in cases) else 0,
            "selected": sorted({f"{b} {n}" for b, n, *_ in cases} if selected is None else selected)}
    base.update(ident)
    (p / "run.json").write_text(json.dumps(base))
    return str(p)


def thresholds_file(d, tests, **over):
    th = Path(d) / "t.json"
    body = {"version": 1, "commit": "c1", "worker": "w1", "rustc": "rustc 1", "build": "b1", "tests": tests}
    body.update(over)
    th.write_text(json.dumps(body))
    return str(th)


TWO = {"core a": {"min": 1, "max": 2, "threshold": 5.0}, "fmt b": {"min": 1, "max": 1, "threshold": 1.5}}


class Runs(unittest.TestCase):
    def test_qualification_sets_thresholds_from_the_spread(self):
        with tempfile.TemporaryDirectory() as d:
            runs = [run_dir(d, f"r{i}", [("core", "a", t, False), ("fmt", "b", 1.0, False)]) for i, t in enumerate([2.0, 2.5, 3.0])]
            q, why = tc.qualify(runs)
            self.assertIsNone(why)
            self.assertEqual(q["tests"]["core a"]["threshold"], 6.0)
            self.assertEqual(q["tests"]["fmt b"]["threshold"], 1.5)
            self.assertIsNone(tc.qualify(runs[:2])[0])
            bad = run_dir(d, "bad", [("core", "a", 2.0, True), ("fmt", "b", 1.0, False)])
            self.assertIsNone(tc.qualify(runs[:2] + [bad])[0])
            out = Path(d) / "th.json"
            self.assertEqual(tc.main(["qualify", *runs, "--out", str(out)]), 0)
            written = json.loads(out.read_text())
            self.assertEqual((written["version"], written["worker"], written["commit"]), (1, "w1", "c1"))
            self.assertEqual([r["run_id"] for r in written["runs"]], ["r0", "r1", "r2"])

    def test_qualification_refuses_runs_that_are_not_independent_or_alike(self):
        cases = [("core", "a", 1.0, False)]
        with tempfile.TemporaryDirectory() as d:
            r0, r1 = run_dir(d, "r0", cases), run_dir(d, "r1", cases)
            # one directory three times, and two directories with one run ID
            self.assertIn("twice", tc.qualify([r0, r0, r0])[1])
            self.assertIn("run ID", tc.qualify([r0, r1, run_dir(d, "r2", cases, run_id="r1")])[1])
            for key, value in [("commit", "c2"), ("worker", "w2"), ("rustc", "rustc 2"), ("build", "b2")]:
                other = run_dir(d, f"x-{key}", cases, **{key: value})
                self.assertIn(key, tc.qualify([r0, r1, other])[1])
            old = run_dir(d, "old", cases, version=2)
            self.assertIn("version", tc.qualify([r0, r1, old])[1])
            nojson = Path(run_dir(d, "nojson", cases))
            (nojson / "run.json").unlink()
            self.assertIn("run.json", tc.qualify([r0, r1, str(nojson)])[1])

    def test_qualification_refuses_incomplete_coverage(self):
        cases = [("core", "a", 1.0, False), ("fmt", "b", 1.0, False)]
        with tempfile.TemporaryDirectory() as d:
            good = [run_dir(d, f"g{i}", cases) for i in range(2)]
            # three identical partial runs: the report lacks a selected test
            partial = [run_dir(d, f"p{i}", cases[:1], selected=["core a", "fmt b"]) for i in range(3)]
            self.assertIn("MISSING fmt b", tc.qualify(partial)[1])
            skipped = run_dir(d, "s", cases, skipped={("fmt", "b")})
            self.assertIn("SKIPPED fmt b", tc.qualify(good + [skipped])[1])
            dup = run_dir(d, "dup", cases + [("fmt", "b", 1.0, False)])
            self.assertIn("DUPLICATE fmt b", tc.qualify(good + [dup])[1])
            cut = run_dir(d, "cut", cases, nextest_exit=137)
            self.assertIn("INCOMPLETE", tc.qualify(good + [cut])[1])
            fewer = run_dir(d, "fewer", cases[:1])
            self.assertIn("same tests", tc.qualify(good + [fewer])[1])
            empty = [run_dir(d, f"e{i}", []) for i in range(3)]
            self.assertIn("EMPTY", tc.qualify(empty)[1])

    def test_a_check_reports_failures_slow_tests_and_unqualified_ones(self):
        with tempfile.TemporaryDirectory() as d:
            th = thresholds_file(d, TWO)
            ok = run_dir(d, "ok", [("core", "a", 4.0, False), ("fmt", "b", 1.2, False)], commit="c9")
            self.assertEqual(tc.main(["check", ok, "--thresholds", th]), 0)
            bad = run_dir(d, "bad", [("core", "a", 6.0, False), ("fmt", "b", 1.0, True), ("x", "new", 0.1, False)])
            self.assertEqual(tc.main(["check", bad, "--thresholds", th, "--commit", "abc"]), 1)
            text = (Path(bad) / "regression.md").read_text()
            for want in ["SLOW core a", "FAIL fmt b", "UNQUALIFIED x new", "abc"]:
                self.assertIn(want, text)
            self.assertEqual(tc.main(["check", run_dir(d, "empty", [])]), 1)

    def test_a_check_counts_missing_skipped_and_duplicate_tests(self):
        with tempfile.TemporaryDirectory() as d:
            th = thresholds_file(d, TWO)
            # the report holds only a passing core a: fmt b was qualified and selected
            missing = run_dir(d, "missing", [("core", "a", 1.0, False)], selected=["core a", "fmt b"])
            self.assertEqual(tc.check(missing, tc.load_thresholds(th)), ["MISSING fmt b (selected, not in the report)"])
            # fmt b has a threshold and the group no longer selects it
            gone = run_dir(d, "gone", [("core", "a", 1.0, False)])
            self.assertIn("UNSELECTED-QUALIFIED fmt b", tc.check(gone, tc.load_thresholds(th))[0])
            skipped = run_dir(d, "skipped", [("core", "a", 0, False), ("fmt", "b", 0, False)], skipped={("core", "a")})
            self.assertEqual(tc.check(skipped, tc.load_thresholds(th)), ["SKIPPED core a"])
            self.assertEqual(tc.check(skipped, None), ["SKIPPED core a"])
            dup = run_dir(d, "dup", [("core", "a", 1.0, False), ("core", "a", 1.0, False), ("fmt", "b", 1.0, False)])
            self.assertEqual(tc.check(dup, tc.load_thresholds(th)), ["DUPLICATE core a (listed 2 times)"])
            cut = run_dir(d, "cut", [("core", "a", 1.0, False), ("fmt", "b", 1.0, False)], nextest_exit=137)
            self.assertEqual(tc.main(["check", cut, "--thresholds", th]), 1)

    def test_a_check_refuses_another_worker_build_or_format(self):
        cases = [("core", "a", 1.0, False), ("fmt", "b", 1.0, False)]
        with tempfile.TemporaryDirectory() as d:
            th = thresholds_file(d, TWO)
            for key, value in [("worker", "w2"), ("rustc", "rustc 2"), ("build", "b2")]:
                self.assertEqual(tc.main(["check", run_dir(d, key, cases, **{key: value}), "--thresholds", th]), 2)
            ok = run_dir(d, "ok", cases)
            self.assertEqual(tc.main(["check", ok, "--thresholds", thresholds_file(d, TWO, version=9)]), 2)
            self.assertEqual(tc.main(["check", run_dir(d, "v", cases, version=0), "--thresholds", th]), 2)

    def test_record_writes_the_identity_and_the_selected_tests(self):
        with tempfile.TemporaryDirectory() as d:
            run = Path(d) / "run"
            run.mkdir()
            (run / "commit").write_text("abc\n")
            (run / "rustc.txt").write_text("rustc 1.99.1\n")
            (run / "build.txt").write_text("mbx 1\n")
            listing = {"rust-suites": {"sparkles-core": {"binary-id": "sparkles-core", "testcases": {
                "a": {"filter-match": {"status": "matches"}},
                "b": {"filter-match": {"status": "mismatch", "reason": "expression"}}}},
                "sparkles::nesting": {"binary-id": "sparkles::nesting", "testcases": {
                    "c": {"filter-match": {"status": "matches"}}}}}}
            (Path(d) / "list.json").write_text(json.dumps(listing))
            self.assertEqual(tc.main(["record", str(run), "--nextest-exit", "0", "--list", str(Path(d) / "list.json"),
                                      "--worker", "boat:bx_1"]), 0)
            ident = json.loads((run / "run.json").read_text())
            self.assertEqual(ident["selected"], ["sparkles-core a", "sparkles::nesting c"])
            self.assertEqual((ident["version"], ident["commit"], ident["worker"], ident["nextest_exit"]),
                             (1, "abc", "boat:bx_1", 0))
            other = Path(d) / "run2"
            other.mkdir()
            for f in ("commit", "rustc.txt", "build.txt"):
                (other / f).write_text((run / f).read_text())
            tc.record(other, 0, Path(d) / "list.json", "boat:bx_1")
            self.assertNotEqual(json.loads((other / "run.json").read_text())["run_id"], ident["run_id"])


if __name__ == "__main__":
    unittest.main()
