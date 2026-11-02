"""Exercise the batch gate's selection of checks from a batch's changed files (scripts/gate/select-checks.py)."""
import importlib.util
import os
import re
import subprocess
import tempfile
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("gate_select", ROOT / "scripts/gate/select-checks.py")
gs = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gs)

BASE = ["fmt:check", "lint:shell", "lint:doc-paths", "api:check", "test:bench-scripts", "client:types:check"]


class Selection(unittest.TestCase):
    def sel(self, files, packages="none"):
        return gs.select(sorted(files), packages)[0]

    def ci(self, files, packages="none"):
        return self.sel(files, packages)["ci"]

    def test_the_task_list_matches_ci_sh(self):
        text = (ROOT / "scripts/ci.sh").read_text()
        block = re.search(r"\n  full=\(\n(.*?)\n  \)", text, re.S).group(1)
        self.assertEqual(block.split(), gs.CI_TASKS)
        for group, tasks in gs.GROUPS.items():
            for t in tasks:
                self.assertIn(t, gs.CI_TASKS, group)

    def test_documentation_runs_only_the_cheap_checks(self):
        s = self.sel(["docs/ROADMAP.md", "docs/specs/X01.md", "CHANGELOG.md", ".github/workflows/ci.yml"])
        self.assertEqual(s["ci"], [t for t in gs.CI_TASKS if t in BASE])
        self.assertEqual(s["conformance"], [])
        self.assertEqual(s["spill"], [])
        self.assertEqual(s["packages"], "none")

    def test_the_rust_tasks_follow_the_chosen_packages(self):
        ci = self.ci(["crates/sparkles-office/src/lib.rs"], ["sparkles-office", "sparkles-server"])
        for t in ["lint", "test", "licenses:check", "test:quickstart"]:
            self.assertIn(t, ci)
        self.assertNotIn("fmt:wasm", ci)
        self.assertNotIn("py:test", ci)
        ci = self.ci(["crates/sparkles-fmt/src/lib.rs"], ["sparkles-fmt", "sparkles-fmt-wasm", "sparkles-server"])
        self.assertIn("fmt:wasm", ci)

    def test_engine_changes_outside_sparql_skip_conformance_and_bindings(self):
        s = self.sel(["crates/sparkles-core/src/store/compaction.rs"], "all")
        self.assertEqual(s["conformance"], [])
        self.assertEqual(s["spill"], [])
        for t in ["lint", "test", "test:quickstart", "fmt:wasm"]:
            self.assertIn(t, s["ci"])
        for t in ["py:lint", "py:test", "jvm:test", "node:test", "ui:test"]:
            self.assertNotIn(t, s["ci"])

    def test_sparql_code_runs_the_conformance_suites(self):
        s = self.sel(["crates/sparkles-core/src/sparql/plan.rs"], "all")
        self.assertEqual(s["conformance"], gs.CONFORMANCE)
        self.assertEqual(s["spill"], [])
        s = self.sel(["crates/sparkles-core/src/sparql/cursor/joinspill.rs"], "all")
        self.assertEqual(s["conformance"], gs.CONFORMANCE)
        self.assertEqual(s["spill"], gs.SPILL)
        self.assertEqual(self.sel(["crates/sparkles-core/src/spill.rs"], "all")["spill"], gs.SPILL)

    def test_unit_test_files_do_not_run_the_suites(self):
        for f in ["crates/sparkles-core/src/sparql/join_tests.rs", "crates/sparkles-core/src/sparql/tests.rs",
                  "crates/sparkles-core/src/sparql/cursor/tests.rs"]:
            s = self.sel([f], "all")
            self.assertEqual((s["conformance"], s["spill"]), ([], []), f)
            self.assertIn("test", s["ci"], f)

    def test_shacl_and_shex_run_the_suites(self):
        for f in ["crates/sparkles-shacl/src/lib.rs", "crates/sparkles-shex/tests/known-failures.txt",
                  "crates/sparkles-shaclc/src/lib.rs", "testsuite/jena-arq/x.ttl"]:
            self.assertEqual(self.sel([f], ["sparkles-shacl"])["conformance"], gs.CONFORMANCE, f)
        self.assertEqual(self.sel(["crates/sparkles-shacl/tests/forms.rs"], ["sparkles-shacl"])["conformance"], [])

    def test_each_binding_runs_on_its_own_files(self):
        cases = {
            "crates/sparkles-py/src/lib.rs": ["py:lint", "py:test"],
            "scripts/py-test.sh": ["py:lint", "py:test"],
            "jvm/sparkles-jena/src/main/kotlin/A.kt": ["jvm:lint", "jvm:rust-test", "jvm:test"],
            "crates/sparkles-ffi/src/lib.rs": ["jvm:lint", "jvm:rust-test", "jvm:test"],
            "js/client/src/x.ts": ["node:fmt:check", "node:lint", "node:test", "node:client:check"],
            "crates/sparkles-node/src/lib.rs": ["node:fmt:check", "node:lint", "node:test", "node:client:check"],
        }
        binding = {t for ts in cases.values() for t in ts}
        for f, want in cases.items():
            got = [t for t in self.ci([f]) if t in binding]
            self.assertEqual(got, want, f)

    def test_the_public_surface_runs_every_binding(self):
        for f in ["crates/sparkles/src/lib.rs", "crates/sparkles/bindings.toml", "docs/openapi.json",
                  "docs/api-compatibility.toml", "testsuite/api/x.json"]:
            ci = self.ci([f])
            for t in ["py:test", "jvm:test", "node:test"]:
                self.assertIn(t, ci, f)

    def test_ui_runs_the_ui_checks(self):
        ci = self.ci(["ui/src/lib/x.svelte"], ["sparkles-fmt-wasm", "sparkles-server"])
        for t in ["ui:fmt:check", "ui:check", "ui:test", "licenses:check", "lint", "test"]:
            self.assertIn(t, ci)
        self.assertNotIn("ui:test", self.ci(["crates/sparkles-server/src/main.rs"], ["sparkles-server"]))

    def test_build_configuration_and_ci_scripts_run_everything(self):
        for f in ["Cargo.lock", "Cargo.toml", "rust-toolchain.toml", "mise.toml", ".config/nextest.toml",
                  "scripts/ci.sh", "scripts/gate/gate.sh", "scripts/changed-crates.py", "vendor/spargebra/src/x.rs"]:
            s = self.sel(["docs/x.md", f])
            self.assertEqual(s["ci"], "all", f)
            self.assertEqual(s["conformance"], gs.CONFORMANCE, f)
            self.assertEqual(s["spill"], gs.SPILL, f)
            self.assertEqual(s["packages"], "all", f)

    def test_unknown_files_and_an_empty_change_run_everything(self):
        self.assertEqual(self.ci(["examples/new.rs"]), "all")
        self.assertEqual(self.ci(["bench/x.toml"]), "all")
        self.assertEqual(self.ci([]), "all")

    def test_paths_only_leaves_out_the_rust_tasks(self):
        s = gs.select(["crates/sparkles-core/src/sparql/plan.rs"], None)[0]
        self.assertNotIn("packages", s)
        self.assertNotIn("test", s["ci"])
        self.assertEqual(s["conformance"], gs.CONFORMANCE)


class CommitRange(unittest.TestCase):
    def test_a_revision_compares_commits_and_ignores_the_working_tree(self):
        with tempfile.TemporaryDirectory() as d:
            run = lambda *a: subprocess.run(["git", "-C", d, *a], check=True, capture_output=True, text=True)
            run("init", "-q", "-b", "main")
            run("config", "user.email", "t@example.invalid")
            run("config", "user.name", "t")
            run("config", "commit.gpgsign", "false")
            Path(d, "a.md").write_text("a\n")
            run("add", ".")
            run("commit", "-q", "-m", "base")
            Path(d, "docs").mkdir()
            Path(d, "docs/b.md").write_text("b\n")
            run("add", ".")
            run("commit", "-q", "-m", "change")
            Path(d, "untracked.rs").write_text("x\n")
            Path(d, "a.md").write_text("edited\n")
            cwd = os.getcwd()
            os.chdir(d)
            try:
                self.assertEqual(gs.cc.changed_files("HEAD~1", "HEAD"), ["docs/b.md"])
                self.assertEqual(gs.cc.changed_files("HEAD~1"), ["a.md", "docs/b.md", "untracked.rs"])
                out = subprocess.run(
                    ["python3", str(ROOT / "scripts/gate/select-checks.py"), "--base", "HEAD~1", "--paths-only"],
                    check=True, capture_output=True, text=True,
                ).stdout.splitlines()
            finally:
                os.chdir(cwd)
            self.assertEqual(out, ["ci " + " ".join(t for t in gs.CI_TASKS if t in BASE), "conformance", "spill"])


if __name__ == "__main__":
    unittest.main()
