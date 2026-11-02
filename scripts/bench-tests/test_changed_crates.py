"""Exercise the changed-crates selection of `scripts/ci.sh --changed` on a fixed workspace."""
import importlib.util
import os
import subprocess
import tempfile
from pathlib import Path
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("changed_crates", ROOT / "scripts/changed-crates.py")
cc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cc)

# name: (directory, workspace dependencies)
PACKAGES = {
    "sparkles-core": ("crates/sparkles-core", set()),
    "sparkles-fmt": ("crates/sparkles-fmt", set()),
    "sparkles-shacl": ("crates/sparkles-shacl", {"sparkles-core"}),
    "sparkles-client": ("crates/sparkles-client", set()),
    "sparkles": ("crates/sparkles", {"sparkles-core", "sparkles-client"}),
    "sparkles-server": ("crates/sparkles-server", {"sparkles", "sparkles-fmt"}),
    "sparkles-fmt-wasm": ("crates/sparkles-fmt-wasm", {"sparkles-fmt"}),
}


class RenamedFiles(unittest.TestCase):
    def test_a_moved_file_reports_both_paths(self):
        with tempfile.TemporaryDirectory() as d:
            run = lambda *a: subprocess.run(["git", "-C", d, *a], check=True, capture_output=True, text=True)
            run("init", "-q", "-b", "main")
            run("config", "user.email", "t@example.invalid")
            run("config", "user.name", "t")
            run("config", "commit.gpgsign", "false")
            os.makedirs(os.path.join(d, "crates/sparkles-core/tests"))
            with open(os.path.join(d, "crates/sparkles-core/tests/f.rdfp"), "w") as f:
                f.write("fixture\n" * 20)
            run("add", ".")
            run("commit", "-q", "-m", "base")
            os.makedirs(os.path.join(d, "docs"))
            run("mv", "crates/sparkles-core/tests/f.rdfp", "docs/f.rdfp")
            run("commit", "-q", "-m", "move")
            cwd = os.getcwd()
            os.chdir(d)
            try:
                files = cc.changed_files("HEAD~1")
            finally:
                os.chdir(cwd)
            self.assertEqual(files, ["crates/sparkles-core/tests/f.rdfp", "docs/f.rdfp"])


class ChangedCrates(unittest.TestCase):
    def choose(self, *files):
        with mock.patch.object(cc, "workspace", return_value=PACKAGES), mock.patch.object(
            cc, "changed_files", return_value=sorted(files)
        ):
            return cc.choose("base")[0]

    def test_a_crate_and_its_reverse_dependencies(self):
        self.assertEqual(
            self.choose("crates/sparkles-fmt/src/lib.rs"),
            ["sparkles-fmt", "sparkles-fmt-wasm", "sparkles-server"],
        )

    def test_dependencies_are_followed_transitively(self):
        self.assertEqual(
            self.choose("crates/sparkles-client/tests/x.rs"),
            ["sparkles", "sparkles-client", "sparkles-server"],
        )

    def test_shared_roots_select_everything(self):
        for f in ["Cargo.lock", "Cargo.toml", "crates/sparkles-core/src/x.rs", ".config/nextest.toml",
                  "testsuite/a/b.ttl", "mise.toml", "rust-toolchain.toml"]:
            self.assertEqual(self.choose("crates/sparkles-fmt/src/lib.rs", f), "all", f)

    def test_unknown_files_select_everything(self):
        self.assertEqual(self.choose("examples/new.rs"), "all")

    def test_files_no_rust_test_reads(self):
        self.assertEqual(self.choose("docs/ROADMAP.md", "scripts/bench.sh", "js/client/x.ts"), "none")

    def test_files_read_from_outside_the_crates(self):
        self.assertEqual(self.choose("docs/openapi.json"), ["sparkles", "sparkles-client", "sparkles-server"])
        self.assertEqual(self.choose("ui/src/lib/x.svelte"), ["sparkles-fmt-wasm", "sparkles-server"])

    def test_binding_sources_that_the_parity_test_reads(self):
        self.assertEqual(self.choose("js/engine/src/index.ts"), ["sparkles", "sparkles-server"])
        self.assertEqual(self.choose("jvm/sparkles-jena/src/main/kotlin/A.kt"), ["sparkles", "sparkles-server"])
        self.assertEqual(self.choose("js/client/src/x.ts"), "none")

    def test_literal_paths_into_another_crate_count_as_dependencies(self):
        packages = dict(PACKAGES, **{"sparkles-shex": ("crates/sparkles-shex", {"sparkles-shacl"})})
        with mock.patch.object(cc, "workspace", return_value=packages), mock.patch.object(
            cc, "changed_files", return_value=["crates/sparkles-shacl/tests/draft_fixture/mod.rs"]
        ):
            self.assertIn("sparkles-shex", cc.choose("base")[0])
        self.assertIn("sparkles-shacl", cc.cross_reads(str(ROOT / "crates/sparkles-shex")))
        self.assertIn("sparkles-fmt", cc.cross_reads(str(ROOT / "crates/sparkles-server")))

    def test_binding_workspaces_outside_the_main_workspace(self):
        self.assertEqual(self.choose("crates/sparkles-py/src/lib.rs"), "none")

    def test_cargo_args(self):
        for value, want in [(None, "--workspace"), ("all", "--workspace"), ("", "none"),
                            ("none", "none"), ("a b", "-p a -p b")]:
            env = dict(os.environ)
            env.pop("SPARKLES_CHANGED_PACKAGES", None)
            if value is not None:
                env["SPARKLES_CHANGED_PACKAGES"] = value
            with mock.patch.dict(os.environ, env, clear=True):
                self.assertEqual(cc.cargo_args(), want, value)

    def test_nextest_arguments_build_the_workspace(self):
        with mock.patch.dict(os.environ, {"SPARKLES_CHANGED_PACKAGES": "a b"}):
            self.assertEqual(cc.cargo_args("nextest"), "--workspace -E package(a)|package(b)")

    def test_doc_arguments_keep_library_packages(self):
        with mock.patch.dict(os.environ, {"SPARKLES_CHANGED_PACKAGES": "sparkles-server sparkles"}), \
                mock.patch.object(cc, "workspace", return_value=PACKAGES), \
                mock.patch.object(cc, "LIBS", {"sparkles"}):
            self.assertEqual(cc.cargo_args("doc"), "-p sparkles")
        with mock.patch.dict(os.environ, {"SPARKLES_CHANGED_PACKAGES": "sparkles-server"}), \
                mock.patch.object(cc, "workspace", return_value=PACKAGES), \
                mock.patch.object(cc, "LIBS", set()):
            self.assertEqual(cc.cargo_args("doc"), "none")


if __name__ == "__main__":
    unittest.main()
