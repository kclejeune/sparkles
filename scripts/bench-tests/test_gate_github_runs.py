"""Exercise the report of the GitHub workflow runs on main (scripts/gate/github-runs.py)."""
import importlib.util
import json
import tempfile
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


def history(*conclusions, workflow="CI", start=10):
    """Finished runs of WORKFLOW, oldest first, one per conclusion."""
    return [run(start + i, workflow, c, f"2026-11-{1 + i:02d}T00:00:00Z", sha=f"{start + i:08d}abcdef")
            for i, c in enumerate(conclusions)]


class Streaks(unittest.TestCase):
    def follow(self, d, runs):
        return gr.follow_streaks(runs, {}, d)

    def drafts(self, d):
        from pathlib import Path
        return sorted(p.name for p in (Path(d) / "drafts").iterdir())

    def test_the_second_failure_in_a_row_opens_one_draft(self):
        with tempfile.TemporaryDirectory() as d:
            self.assertEqual(self.follow(d, history("success", "failure")), [])
            self.assertEqual(self.drafts(d), [])
            lines = self.follow(d, history("success", "failure", "cancelled", "failure"))
            self.assertEqual(len(lines), 1)
            self.assertIn("failed 2 times in a row", lines[0])
            self.assertEqual(self.drafts(d), ["ci-11.md"])
            row = (gr.Path(d) / "drafts" / "ci-11.md").read_text()
            self.assertIn("| NNN | **The CI workflow fails on main.**", row)
            self.assertIn("00000011", row)
            self.assertIn("00000013", row)
            # the same streak, longer and later, writes no second draft
            self.assertEqual(self.follow(d, history("success", "failure", "cancelled", "failure", "failure")), [])
            # even when its first failure has left the fetched history
            self.assertEqual(self.follow(d, history(*["failure"] * 6, start=30)), [])
            self.assertEqual(self.drafts(d), ["ci-11.md"])
            state = json.loads((gr.Path(d) / "state.json").read_text())
            self.assertEqual((state["version"], state["open"]["CI"]["first"], state["open"]["CI"]["newest"]), (1, 11, 35))

    def test_a_pass_closes_the_streak_and_a_later_streak_opens_another(self):
        with tempfile.TemporaryDirectory() as d:
            self.follow(d, history("failure", "failure"))
            self.assertIn("closed", self.follow(d, history("failure", "failure", "success"))[0])
            self.assertEqual(json.loads((gr.Path(d) / "state.json").read_text())["open"], {})
            lines = self.follow(d, history("failure", "failure", "success", "failure", "failure"))
            self.assertEqual(len(lines), 1)
            self.assertEqual(self.drafts(d), ["ci-10.md", "ci-13.md"])

    def test_a_pass_between_reports_ends_the_open_streak(self):
        with tempfile.TemporaryDirectory() as d:
            self.follow(d, history("failure", "failure"))
            # the next report sees a pass and then two new failures, with the first streak
            # out of view
            lines = self.follow(d, history("success", "failure", "failure", start=20))
            self.assertIn("closed", lines[0])
            self.assertIn("failed 2 times in a row", lines[1])
            self.assertEqual(self.drafts(d), ["ci-10.md", "ci-21.md"])

    def test_cancelled_runs_and_other_workflows_neither_open_nor_close_a_streak(self):
        with tempfile.TemporaryDirectory() as d:
            self.follow(d, history("failure", "failure"))
            runs = history("failure", "failure", "cancelled", "cancelled") + history("success", workflow="Nix", start=50)
            self.assertEqual(self.follow(d, runs), [])
            self.assertEqual(list(json.loads((gr.Path(d) / "state.json").read_text())["open"]), ["CI"])
            self.assertEqual(self.follow(d, history("cancelled", start=60)), [])
            self.assertEqual(list(json.loads((gr.Path(d) / "state.json").read_text())["open"]), ["CI"])

    def test_a_state_file_of_another_version_is_refused(self):
        with tempfile.TemporaryDirectory() as d:
            (gr.Path(d) / "state.json").write_text(json.dumps({"version": 2, "open": {}}))
            with self.assertRaises(ValueError):
                self.follow(d, history("failure", "failure"))


if __name__ == "__main__":
    unittest.main()
