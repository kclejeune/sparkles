import importlib.util
import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("gha_cache_prune", ROOT / "scripts/gha-cache-prune.py")
prune = importlib.util.module_from_spec(spec)
spec.loader.exec_module(prune)

SHA_A = "a" * 40
SHA_B = "b" * 40
TOOLCHAIN = "3b2f647327c08c376853515bbc647b46b3735f2ea0a24957b92469f142004fed"


def entry(id_, key, created):
    return {"id": id_, "key": key, "created_at": created, "size_in_bytes": 1, "ref": "refs/heads/main"}


class FamilyTest(unittest.TestCase):
    def test_strips_commit_and_rust_cache_suffixes(self):
        self.assertEqual(prune.family(f"mbx-store-tests-Linux-X64-mbx1.29.1-{TOOLCHAIN}-{SHA_A}"),
                         f"mbx-store-tests-Linux-X64-mbx1.29.1-{TOOLCHAIN}")
        self.assertEqual(prune.family("v0-rust-tests-Linux-x64-f59e8b09-814e10a8"),
                         "v0-rust-tests-Linux-x64")

    def test_keys_without_a_known_suffix_have_no_family(self):
        for key in ("mbx-store-tests-Linux-X64-mbx1.29.1-short", "v0-rust-tests-Linux-x64-f59e8b09",
                    "node-musl-tools-Linux-X64-" + "c" * 64, "gradle-home-v1|Linux-X64-" + SHA_A,
                    "mbx-store-" + SHA_A):
            with self.subTest(key=key):
                self.assertIsNone(prune.family(key))


class SupersededTest(unittest.TestCase):
    def test_keeps_the_newest_entry_of_each_family(self):
        tests_old = entry(1, f"mbx-store-tests-L-{TOOLCHAIN}-{SHA_A}", "2026-11-15T17:00:00Z")
        tests_new = entry(2, f"mbx-store-tests-L-{TOOLCHAIN}-{SHA_B}", "2026-11-17T21:00:00Z")
        clippy_mac = entry(3, f"mbx-store-clippy-macos-arm64-{TOOLCHAIN}-{SHA_A}", "2026-11-10T00:00:00Z")
        clippy_linux = entry(4, f"mbx-store-clippy-linux-amd64-{TOOLCHAIN}-{SHA_B}", "2026-11-17T21:00:00Z")
        rust_old = entry(5, "v0-rust-tests-Linux-x64-f59e8b09-814e10a8", "2026-11-01T00:00:00Z")
        rust_new = entry(6, "v0-rust-tests-Linux-x64-f59e8b09-ae636f9a", "2026-11-17T00:00:00Z")
        doomed = prune.superseded([tests_new, clippy_mac, rust_new, tests_old, clippy_linux, rust_old],
                                  ["mbx-store-", "v0-rust-"])
        self.assertEqual(sorted(e["id"] for e in doomed), [1, 5])

    def test_only_selected_prefixes_are_touched(self):
        old = entry(1, f"mbx-store-tests-L-{SHA_A}", "2026-11-15T17:00:00Z")
        new = entry(2, f"mbx-store-tests-L-{SHA_B}", "2026-11-17T21:00:00Z")
        self.assertEqual(prune.superseded([old, new], ["mbx-store-clippy-"]), [])
        self.assertEqual([e["id"] for e in prune.superseded([old, new], ["mbx-store-tests-"])], [1])

    def test_a_single_entry_is_kept(self):
        only = entry(1, f"mbx-store-jvm-linux-{SHA_A}", "2026-11-15T17:00:00Z")
        self.assertEqual(prune.superseded([only], ["mbx-store-"]), [])


class WorkflowTest(unittest.TestCase):
    """Each workflow that saves per-commit caches prunes them, from main only."""

    def test_saving_workflows_prune_their_families_on_main(self):
        expected = {
            "ci.yml": ["mbx-store-clippy-", "v0-rust-clippy-", "mbx-store-tests-", "v0-rust-tests-"],
            "jvm-bindings.yml": ["mbx-store-jvm-", "v0-rust-jvm-"],
            "node-packages.yml": ["mbx-store-node-", "v0-rust-node-"],
            "python-wheels.yml": ["mbx-store-python-"],
        }
        for name, prefixes in expected.items():
            with self.subTest(workflow=name):
                workflow = (ROOT / ".github/workflows" / name).read_text()
                calls = re.findall(r"scripts/gha-cache-prune\.py ([^\n]+)", workflow)
                self.assertTrue(calls)
                self.assertEqual(sorted(p for call in calls for p in call.split()), sorted(prefixes))
                job = workflow.split("  prune-caches:", 1)[1]
                self.assertIn("github.ref == 'refs/heads/main'", job.split("steps:", 1)[0])
                self.assertIn("actions: write", job)

    def test_server_candidates_keep_no_store(self):
        workflow = (ROOT / ".github/workflows/server-artifacts.yml").read_text()
        self.assertNotIn("actions/cache", workflow)


if __name__ == "__main__":
    unittest.main()
