"""Fetch the event's trusted API base after main's history is rewritten."""
import os
from pathlib import Path
import subprocess
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[2]


class ApiBaseFetch(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="api-event-base-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.remote = self.root / "remote.git"
        self.source = self.root / "source"
        self.checkout = self.root / "checkout"
        self.git("init", "--bare", str(self.remote))
        self.git("init", str(self.source))
        self.git("-C", str(self.source), "-c", "commit.gpgsign=false",
                 "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                 "commit", "--allow-empty", "-m", "shared parent")
        parent = self.git("-C", str(self.source), "rev-parse", "HEAD")
        self.git("-C", str(self.source), "-c", "commit.gpgsign=false",
                 "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                 "commit", "--allow-empty", "-m", "old main tip")
        self.base = self.git("-C", str(self.source), "rev-parse", "HEAD")
        self.git("-C", str(self.source), "reset", "--hard", parent)
        self.git("-C", str(self.source), "-c", "commit.gpgsign=false",
                 "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                 "commit", "--allow-empty", "-m", "amended main tip")
        self.git("-C", str(self.source), "remote", "add", "origin", str(self.remote))
        # Send the old tip first, then replace it. It remains an unadvertised object.
        self.git("-C", str(self.source), "push", "origin", self.base + ":refs/heads/main")
        self.git("-C", str(self.source), "push", "--force", "origin", "HEAD:refs/heads/main")
        self.git("clone", "--branch", "main", "--single-branch", "--no-local",
                 str(self.remote), str(self.checkout))

    def git(self, *args):
        return subprocess.check_output(["git", *args], stderr=subprocess.DEVNULL,
                                       text=True).strip()

    def fetch_base(self, base):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        script = textwrap.dedent(workflow.split("# BEGIN API EVENT BASE FETCH", 1)[1]
                                 .split("# END API EVENT BASE FETCH", 1)[0])
        return subprocess.run(["bash", "-euo", "pipefail", "-c", script],
                              cwd=self.checkout, env={**os.environ, "API_BASE_REF": base},
                              capture_output=True, text=True)

    def test_fetches_unadvertised_old_tip_without_changing_checkout(self):
        before = self.git("-C", str(self.checkout), "rev-parse", "HEAD")
        missing = subprocess.run(["git", "cat-file", "-e", self.base + "^{commit}"],
                                 cwd=self.checkout, capture_output=True)
        self.assertNotEqual(missing.returncode, 0)
        result = self.fetch_base(self.base)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.git("-C", str(self.checkout), "cat-file", "-e", self.base + "^{commit}")
        self.assertEqual(self.git("-C", str(self.checkout), "rev-parse", "HEAD"), before)

    def test_existing_base_needs_no_remote(self):
        base = self.git("-C", str(self.checkout), "rev-parse", "HEAD")
        self.git("-C", str(self.checkout), "remote", "remove", "origin")
        result = self.fetch_base(base)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_empty_base_needs_no_remote(self):
        self.git("-C", str(self.checkout), "remote", "remove", "origin")
        result = self.fetch_base("")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_unavailable_base_fails_closed(self):
        result = self.fetch_base("b" * 40)
        self.assertNotEqual(result.returncode, 0)

    def test_rejects_non_hash_before_fetch(self):
        for base in ("main", "--all", "a" * 39, "a" * 40 + "\n"):
            with self.subTest(base=base):
                result = self.fetch_base(base)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("explicit full commit hash", result.stderr)


if __name__ == "__main__":
    unittest.main()
