"""Check that the binding tasks' cache sources in mise.toml cover what the bindings compile.

py:lint, py:test, jvm:lint, jvm:rust-test, jvm:test and node:lint list the binding-deps
input group instead of every Rust file, so their cached passes survive changes to crates
the bindings do not build. That is only sound while binding-deps names every root crate
that a binding depends on, so this test follows the path dependencies in cargo metadata.
"""
import json
import os
import shutil
import subprocess
import tomllib
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BINDINGS = {"sparkles-py": "py", "sparkles-ffi": "ffi", "sparkles-node": "node-addon"}
NARROW = {
    "py:lint": "py", "py:test": "py", "jvm:lint": "ffi", "jvm:rust-test": "ffi", "jvm:test": "ffi",
    "node:lint": "node-addon",
}
# Directories besides src that a library reads at compile time (include_str! and the like).
COMPILED_DIRS = ["resources", "rules", "vocab", "diagnostics"]


def metadata(*args):
    cargo = shutil.which("mbx") or shutil.which("cargo")
    if cargo is None:
        raise unittest.SkipTest("needs mbx or cargo")
    out = subprocess.run(
        [cargo, "metadata", "--format-version", "1", "--no-deps", "--offline", *args],
        cwd=ROOT, check=True, capture_output=True, text=True,
    ).stdout
    return json.loads(out)


class BindingSources(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.mise = tomllib.loads((ROOT / "mise.toml").read_text())
        cls.groups = cls.mise["task_config"]["input_groups"]
        packages = {}
        for m in [metadata()] + [metadata("--manifest-path", f"crates/{b}/Cargo.toml") for b in BINDINGS]:
            for p in m["packages"]:
                rel = os.path.relpath(os.path.dirname(p["manifest_path"]), ROOT)
                packages[p["name"]] = (rel, [(d["name"], d.get("kind")) for d in p["dependencies"] if d.get("path")])
        cls.packages = packages

    def closure(self, binding):
        seen, todo = set(), [n for n, _ in self.packages[binding][1]]
        while todo:
            name = todo.pop()
            if name in seen:
                continue
            seen.add(name)
            todo += [n for n, kind in self.packages[name][1] if kind in (None, "build")]
        return seen

    def test_binding_deps_cover_every_crate_a_binding_compiles(self):
        group = set(self.groups["binding-deps"])
        for binding in BINDINGS:
            for name in sorted(self.closure(binding)):
                rel = self.packages[name][0]
                for want in [f"{rel}/*.toml", f"{rel}/src/**"]:
                    self.assertIn(want, group, f"{binding} compiles {name}")
                if (ROOT / rel / "build.rs").exists():
                    self.assertIn(f"{rel}/build.rs", group, f"{binding} compiles {name}")
                for d in COMPILED_DIRS:
                    if (ROOT / rel / d).is_dir():
                        self.assertIn(f"{rel}/{d}/**", group, f"{binding} compiles {name}")
        for f in ["Cargo.lock", "rust-toolchain.toml", "vendor/**"]:
            self.assertIn(f, group)

    def test_each_binding_group_covers_its_own_crate(self):
        for binding, name in BINDINGS.items():
            group = set(self.groups[name])
            rel = f"crates/{binding}"
            self.assertIn(f"{rel}/*.toml", group)
            self.assertIn(f"{rel}/Cargo.lock", group)
            self.assertIn(f"{rel}/src/**", group)
            for d in ["tests", "examples", "python"] + COMPILED_DIRS:
                if (ROOT / rel / d).is_dir():
                    self.assertIn(f"{rel}/{d}/**", group, binding)
            if (ROOT / rel / "build.rs").exists():
                self.assertIn(f"{rel}/build.rs", group, binding)

    def test_the_narrow_tasks_use_both_groups(self):
        tasks = self.mise["tasks"]
        for task, own in NARROW.items():
            sources = tasks[task]["sources"]
            self.assertIn("@group:binding-deps", sources, task)
            self.assertIn(f"@group:{own}", sources, task)
            self.assertNotIn("@group:rust", sources, task)
        # node:test starts a debug server, so it keeps every Rust input
        self.assertIn("@group:rust", tasks["node:test"]["sources"])


if __name__ == "__main__":
    unittest.main()
