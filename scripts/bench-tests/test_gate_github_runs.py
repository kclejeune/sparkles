"""Exercise the report of the GitHub workflow runs on main (scripts/gate/github-runs.py)."""
import importlib.util
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("github_runs", ROOT / "scripts/gate/github-runs.py")
gr = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gr)


def run(id, workflow, conclusion, created, status="completed", sha="0123456789abcdef"):
    return {
        "databaseId": id, "workflowName": workflow, "status": status, "conclusion": conclusion,
        "createdAt": created, "headSha": sha, "url": f"https://example.invalid/{id}",
    }


class GithubRuns(unittest.TestCase):
    def test_the_newest_finished_run_of_each_workflow_decides(self):
        runs = [
            run(1, "CI", "failure", "2026-11-01T00:00:00Z"),
            run(2, "CI", "success", "2026-11-02T00:00:00Z"),
            run(3, "CI", "", "2026-11-03T00:00:00Z", status="in_progress"),
            run(4, "Python wheels", "failure", "2026-11-02T00:00:00Z", sha="fedcba9876543210"),
        ]
        lines, bad = gr.summarize(runs, {4: ["Linux manylinux x86_64"]})
        self.assertTrue(bad)
        self.assertEqual(lines, [
            "  CI: success on 01234567",
            "  Python wheels: failure on fedcba98 (Linux manylinux x86_64) https://example.invalid/4",
        ])

    def test_a_cancelled_run_is_not_a_failure(self):
        lines, bad = gr.summarize([run(1, "CI", "cancelled", "2026-11-01T00:00:00Z")], {})
        self.assertFalse(bad)
        self.assertEqual(lines, ["  CI: cancelled on 01234567"])


if __name__ == "__main__":
    unittest.main()
