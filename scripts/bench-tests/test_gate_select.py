"""Exercise the batch gate's selection of checks from a batch's changed files (scripts/gate/select-checks.py)."""
import git_isolation  # noqa: F401, first, so no fixture sees the host Git settings
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

spec2 = importlib.util.spec_from_file_location("ccrates", ROOT / "scripts/bench-tests/test_changed_crates.py")
fixtures = importlib.util.module_from_spec(spec2)
spec2.loader.exec_module(fixtures)
LOCK, MISE = fixtures.LOCK, fixtures.MISE

# The files of batch 31, OWL classification and documentation, whose screen was skipped.
BATCH_31 = [
    "AGENTS.md", "CHANGELOG.md", "crates/sparkles-owl/src/classify/dl_reduce.rs",
    "crates/sparkles-owl/src/classify/index.rs", "crates/sparkles-owl/src/classify/mod.rs",
    "crates/sparkles-owl/src/classify/query.rs", "crates/sparkles-owl/tests/dl_nominals.rs",
    "docs/BENCHMARKING.md", "docs/FEATURES.md", "docs/ROADMAP.md", "docs/reasoning/README.md",
    "docs/specs/F24-owl-classification/SPEC.md",
]

BASE = ["fmt:check", "lint:shell", "lint:doc-paths", "api:check", "test:bench-scripts", "client:types:check"]


class Selection(unittest.TestCase):
    def sel(self, files, packages="none", texts=None):
        read = (lambda path: texts[path]) if texts is not None else None
        return gs.select(sorted(files), packages, read)[0]

    def ci(self, files, packages="none", texts=None):
        return self.sel(files, packages, texts)["ci"]

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

    def test_the_workflow_table_matches_the_workflows(self):
        def push_paths(wf):
            out, push, inside = [], False, False
            for line in (ROOT / ".github/workflows" / wf).read_text().splitlines():
                if line.startswith("  push:"):
                    push = True
                elif push and "paths: &paths" in line:
                    inside = True
                elif inside:
                    m = re.match(r"^\s*- ['\"]?([^'\"]+?)['\"]?\s*$", line)
                    if m:
                        out.append(m.group(1))
                    elif not re.match(r"^\s*#", line):
                        break
            return out
        files = {p.name for p in (ROOT / ".github/workflows").glob("*.yml")}
        mapped = {Path(wf).name for wf, _, _ in gs.WORKFLOWS}
        self.assertEqual(files, mapped | gs.WORKFLOW_UNMAPPED)
        self.assertFalse(mapped & gs.WORKFLOW_UNMAPPED)
        for wf, group, patterns in gs.WORKFLOWS:
            self.assertIn(group, gs.GROUPS)
            self.assertEqual(patterns, push_paths(Path(wf).name), wf)

    def test_workflow_paths_follow_github_rules(self):
        py = next(p for wf, g, p in gs.WORKFLOWS if g == "py")
        self.assertTrue(gs.workflow_selects("crates/sparkles-core/src/lib.rs", py))
        self.assertFalse(gs.workflow_selects("crates/sparkles-core/tests/w3c.rs", py))
        self.assertTrue(gs.workflow_selects("crates/sparkles-core/tests/patch/a.rdfp", py))
        self.assertTrue(gs.workflow_selects("crates/sparkles-py/tests/test_x.py", py))
        self.assertFalse(gs.workflow_selects("crates/sparkles-owl/src/lib.rs", py))
        # `*` stays within one directory, unlike fnmatch's.
        self.assertFalse(gs.workflow_selects("crates/sparkles-core/src/a/tests/b.rs", ["!crates/sparkles*/tests/**"]))
        self.assertTrue(gs.workflow_selects("crates/sparkles-core/src/a/tests/b.rs",
                                            ["crates/**", "!crates/sparkles*/tests/**"]))

    def test_a_workflow_and_its_own_scripts_run_its_local_checks(self):
        for f, group in [(".github/workflows/python-wheels.yml", "py"), ("scripts/py-sdist.py", "py"),
                         ("scripts/py-wheel-test.sh", "py"), (".github/workflows/jvm-bindings.yml", "jvm"),
                         (".github/workflows/node-packages.yml", "node"), ("scripts/ci-node-musl.sh", "node")]:
            ci = self.ci([f])
            for t in gs.GROUPS[group]:
                self.assertIn(t, ci, f)
        ci = self.ci(["scripts/install-mbx-container.sh"])
        for t in ["py:test", "node:test"]:
            self.assertIn(t, ci)
        self.assertNotIn("jvm:test", ci)
        # The other workflows and the documentation select no binding.
        for f in [".github/workflows/ci.yml", ".github/workflows/server-artifacts.yml", "docs/FEATURES.md"]:
            for t in ["py:test", "jvm:test", "node:test"]:
                self.assertNotIn(t, self.ci([f]), f)

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
            "ui/scripts/seed-design.mjs": gs.GROUPS["node"],
            "ui/scripts/ask-stub.mjs": gs.GROUPS["node"],
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
        for f in ["Cargo.toml", "rust-toolchain.toml", ".config/nextest.toml",
                  "scripts/ci.sh", "scripts/gate/gate.sh", "scripts/changed-crates.py", "vendor/spargebra/src/x.rs"]:
            s = self.sel(["docs/x.md", f])
            self.assertEqual(s["ci"], "all", f)
            self.assertEqual(s["conformance"], gs.CONFORMANCE, f)
            self.assertEqual(s["spill"], gs.SPILL, f)
            self.assertEqual(s["packages"], "all", f)

    def test_lock_and_mise_changes_run_everything_without_a_reader(self):
        for f in ["Cargo.lock", "mise.toml"]:
            self.assertEqual(self.sel(["docs/x.md", f])["ci"], "all", f)

    def test_a_lock_change_runs_the_rust_tasks_of_the_packages_it_reaches(self):
        bumped = LOCK.replace('checksum = "bb"', 'checksum = "bd"')
        s = self.sel(["Cargo.lock"], ["sparkles-fmt", "sparkles-server"], {"Cargo.lock": (LOCK, bumped)})
        for t in ["lint", "test", "licenses:check", "test:quickstart"] + BASE:
            self.assertIn(t, s["ci"])
        for t in ["py:test", "jvm:test", "node:test", "ui:test"]:
            self.assertNotIn(t, s["ci"])
        self.assertEqual(s["conformance"], [])
        self.assertEqual(s["spill"], [])
        core = LOCK.replace('checksum = "aa"', 'checksum = "ad"')
        s = self.sel(["Cargo.lock"], "all", {"Cargo.lock": (LOCK, core)})
        self.assertEqual(s["conformance"], gs.CONFORMANCE)
        self.assertEqual(s["spill"], gs.SPILL)
        self.assertEqual(self.sel(["Cargo.lock"], "all", {"Cargo.lock": (LOCK, "bad [")})["ci"], "all")

    def test_a_mise_change_runs_the_tasks_whose_definitions_it_changes(self):
        texts = lambda new: {"mise.toml": (MISE, new)}
        ci = self.ci(["mise.toml"], "none", texts(MISE.replace('"pytest"', '"pytest -x"')))
        self.assertEqual(ci, [t for t in gs.CI_TASKS if t in BASE + ["py:test"]])
        ci = self.ci(["mise.toml"], "none", texts(MISE.replace("bench.sh", "bench.sh -q")))
        self.assertEqual(ci, [t for t in gs.CI_TASKS if t in BASE])
        # ui:build is a prerequisite that test names, so the test task runs.
        self.assertIn("test", self.ci(["mise.toml"], "all", texts(MISE.replace("pnpm build", "pnpm b"))))
        w3c = MISE + '\n[tasks."test:w3c"]\nrun = "mbx test"\n'
        s = gs.select(["mise.toml"], "none", lambda p: (MISE, w3c))[0]
        self.assertEqual(s["conformance"], gs.CONFORMANCE)
        self.assertEqual(self.ci(["mise.toml"], "all", texts(MISE.replace('"1.0"', '"1.1"'))), "all")

    def test_the_screen_skips_only_changes_that_cannot_reach_query_execution(self):
        self.assertEqual(self.sel(BATCH_31)["screen"][0], False)
        self.assertEqual(self.sel(["docs/ROADMAP.md", "crates/sparkles-core/src/sparql_tests.rs",
                                   "crates/sparkles-core/tests/w3c.rs", ".github/workflows/ci.yml",
                                   "scripts/gate/gate.sh", "crates/sparkles-py/src/lib.rs"])["screen"][0], False)
        for f in ["crates/sparkles-core/src/sparql/plan.rs", "crates/sparkles-server/src/http.rs",
                  "crates/sparkles/src/lib.rs", "crates/sparkles-reasoner/src/lib.rs", "Cargo.lock",
                  "Cargo.toml", "rust-toolchain.toml", "mise.toml", "scripts/bench.sh", "scripts/nsc-bench.sh",
                  "scripts/boat/controller.py", "docs/openapi.json", "ui/src/x.svelte", "examples/new.rs",
                  "crates/sparkles-new/src/lib.rs"]:
            run, why = self.sel(BATCH_31 + [f])["screen"]
            self.assertTrue(run, f)
            self.assertIn(f, why)
        self.assertTrue(self.sel([])["screen"][0])
        self.assertTrue(gs.select(["Cargo.lock"], None)[0]["screen"][0])

    def test_the_prerequisites_match_ci_sh(self):
        text = (ROOT / "scripts/ci.sh").read_text()
        block = re.search(r'for t in "\$\{tasks\[@\]\}"; do\n    case "\$t" in\n(.*?)\n    esac', text, re.S).group(1)
        want = {}
        for pats, prep in re.findall(r"^\s*([^)\n]+)\) prep\+=\(([^)]+)\) ;;", block, re.M):
            for t in pats.split("|"):
                want.setdefault(t.strip(), []).extend(prep.split())
        self.assertEqual(want, gs.PREP_TASKS)

    def test_a_prerequisite_change_runs_the_tasks_that_ci_builds_it_for(self):
        new = MISE.replace('run = "pnpm build"', 'run = "pnpm b"')
        ci = gs.select(["mise.toml"], "none", lambda p: (MISE, new))[0]["ci"]
        self.assertIn("test:quickstart", ci)
        self.assertIn("ui:test", ci)

    def test_a_broken_jvm_native_task_runs_the_jvm_test(self):
        # jvm:test does not name jvm:native in mise.toml, but scripts/ci.sh builds it first.
        real = (ROOT / "mise.toml").read_text()
        broken = real.replace('run = "mbx sparkles-task -- scripts/jvm-native.sh"', 'run = "false"', 1)
        self.assertNotEqual(real, broken)
        ci = gs.select(["mise.toml"], "none", lambda p: (real, broken))[0]["ci"]
        self.assertIn("jvm:test", ci)

    def test_manifests_build_scripts_and_nested_test_directories_run_the_screen(self):
        for f in ["crates/sparkles-owl/Cargo.toml", "crates/sparkles-shacl/build.rs",
                  "crates/sparkles-core/src/sparql/tests/x.rs", "crates/sparkles-server/src/fuzz/a.rs"]:
            self.assertTrue(self.sel([f])["screen"][0], f)
        self.assertFalse(self.sel(["crates/sparkles-core/tests/support/x.rs", "crates/sparkles-core/benches/b.rs"])["screen"][0])

    def test_the_scripts_the_screen_reaches_run_it(self):
        # Follow every script name that the screen's scripts mention, by base name, and
        # stop at the scripts that SCREEN_SKIP lists. The ones reached that way are the CI
        # and gate scripts that the remote CI runner names, checked by hand, and any other
        # is a script of the screen that SCREEN_SKIP must not list.
        scripts = {p.relative_to(ROOT).as_posix() for p in (ROOT / "scripts").rglob("*") if p.is_file()}
        by_name = {}
        for p in scripts:
            by_name.setdefault(os.path.basename(p), set()).add(p)
        match = lambda p, pats: any(gs.fnmatch.fnmatchcase(p, pat) for pat in pats)
        skipped = lambda p: match(p, gs.SCREEN_SKIP) and not match(p, gs.SCREEN_RUN)
        todo, seen, reached = ["scripts/bench-remote.sh", "scripts/nsc-bench.sh"], set(), set()
        while todo:
            p = todo.pop()
            if p in seen:
                continue
            seen.add(p)
            if skipped(p):
                reached.add(p)
                continue
            text = (ROOT / p).read_text(errors="replace")
            for name in set(re.findall(r"[A-Za-z0-9_.-]+\.(?:sh|py)\b", text)):
                todo.extend(by_name.get(name, ()))
        self.assertEqual(sorted(reached), [
            "scripts/ci.sh", "scripts/gate/gate.sh", "scripts/gate/remote/ci-job.sh",
            "scripts/gate/remote/gate-start.sh", "scripts/gate/setup-host.sh", "scripts/premerge.sh",
        ])
        for f in ["scripts/bench.sh", "scripts/nsc-bench.sh", "scripts/bench-remote.sh", "scripts/gate/config.sh",
                  "scripts/new-tool.sh"]:
            self.assertTrue(self.sel([f])["screen"][0], f)

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
            self.assertEqual(out, ["ci " + " ".join(t for t in gs.CI_TASKS if t in BASE), "conformance", "spill",
                                   "screen skip (no changed file reaches query execution or the screen)"])


if __name__ == "__main__":
    unittest.main()
