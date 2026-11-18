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


def run_dir(d, name, cases):
    p = Path(d) / name
    p.mkdir()
    body = "".join(
        f'<testsuite name="{b}"><testcase name="{n}" time="{t}">{"<failure/>" if f else ""}</testcase></testsuite>'
        for b, n, t, f in cases)
    (p / "junit.xml").write_text(f"<testsuites>{body}</testsuites>")
    return str(p)


class Runs(unittest.TestCase):
    def test_qualification_sets_thresholds_from_the_spread(self):
        with tempfile.TemporaryDirectory() as d:
            runs = [run_dir(d, f"r{i}", [("core", "a", t, False), ("fmt", "b", 1.0, False)]) for i, t in enumerate([2.0, 2.5, 3.0])]
            t, why = tc.qualify(runs)
            self.assertIsNone(why)
            self.assertEqual(t["core a"]["threshold"], 6.0)
            self.assertEqual(t["fmt b"]["threshold"], 1.5)
            self.assertIsNone(tc.qualify(runs[:2])[0])
            bad = run_dir(d, "bad", [("core", "a", 2.0, True), ("fmt", "b", 1.0, False)])
            self.assertIsNone(tc.qualify(runs[:2] + [bad])[0])

    def test_a_check_reports_failures_slow_tests_and_unqualified_ones(self):
        with tempfile.TemporaryDirectory() as d:
            th = Path(d) / "t.json"
            th.write_text(json.dumps({"tests": {"core a": {"min": 1, "max": 2, "threshold": 5.0},
                                                "fmt b": {"min": 1, "max": 1, "threshold": 1.5}}}))
            ok = run_dir(d, "ok", [("core", "a", 4.0, False), ("fmt", "b", 1.2, False)])
            self.assertEqual(tc.main(["check", ok, "--thresholds", str(th)]), 0)
            bad = run_dir(d, "bad", [("core", "a", 6.0, False), ("fmt", "b", 1.0, True), ("x", "new", 0.1, False)])
            self.assertEqual(tc.main(["check", bad, "--thresholds", str(th), "--commit", "abc"]), 1)
            text = (Path(bad) / "regression.md").read_text()
            for want in ["SLOW core a", "FAIL fmt b", "UNQUALIFIED x new", "abc"]:
                self.assertIn(want, text)
            self.assertEqual(tc.main(["check", run_dir(d, "empty", [])]), 1)


if __name__ == "__main__":
    unittest.main()
