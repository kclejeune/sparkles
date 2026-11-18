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
    """Finished runs of WORKFLOW, oldest first, one per conclusion, numbered from START."""
    return [run(start + i, workflow, c, f"2026-11-{1 + (start + i) // 24:02d}T{(start + i) % 24:02d}:00:00Z",
                sha=f"{start + i:08d}abcdef")
            for i, c in enumerate(conclusions)]


class Streaks(unittest.TestCase):
    def follow(self, d, runs, more=None, jobs=None, repo="o/r", branch="main"):
        return gr.follow_streaks(repo, branch, runs, d, more=more, jobs=jobs)

    def drafts(self, d):
        return sorted(p.name for p in (gr.Path(d) / "drafts").glob("*.md"))

    def record(self, d, name):
        return json.loads((gr.Path(d) / "drafts" / name).with_suffix(".json").read_text())

    def test_the_second_failure_in_a_row_opens_one_draft(self):
        all_runs = history("success", "failure", "cancelled", "failure", "failure")
        with tempfile.TemporaryDirectory() as d:
            self.assertEqual(self.follow(d, all_runs[:2]), [])
            self.assertEqual(self.drafts(d), [])
            lines = self.follow(d, all_runs[:4], jobs=lambda i: ["Build tests"])
            self.assertEqual(len(lines), 1)
            self.assertIn("failed 2 times in a row", lines[0])
            self.assertEqual(self.drafts(d), ["o-r-main-ci-11.md"])
            row = (gr.Path(d) / "drafts" / "o-r-main-ci-11.md").read_text()
            for want in ["| NNN | **The CI workflow fails on main.**", "in o/r", "00000011", "00000013", "Build tests"]:
                self.assertIn(want, row)
            # the same streak, longer and later, writes no second draft
            self.assertEqual(self.follow(d, all_runs), [])
            self.assertEqual(self.drafts(d), ["o-r-main-ci-11.md"])
            rec = self.record(d, "o-r-main-ci-11.md")
            self.assertEqual((rec["version"], rec["runs"], rec["closed"]), (1, [11, 13, 14], False))

    def test_a_streak_longer_than_the_window_stays_one_streak(self):
        runs = history("success", "failure", "failure", *["failure"] * 300)
        with tempfile.TemporaryDirectory() as d:
            self.follow(d, runs[:3])
            for end in (40, 120, 250, len(runs)):
                more = lambda name, limit, end=end: runs[:end][-limit:]
                self.assertEqual(self.follow(d, runs[end - 20:end], more=more), [])
            self.assertEqual(self.drafts(d), ["o-r-main-ci-11.md"])

    def test_a_pass_behind_the_window_is_found_by_fetching_further_back(self):
        # F1 F2 opened a draft, then S3 and F4..F25 ran; the report fetches only the newest 20
        runs = history("failure", "failure", "success", *["failure"] * 22)
        with tempfile.TemporaryDirectory() as d:
            self.follow(d, runs[:2])
            calls = []
            def more(name, limit):
                calls.append(limit)
                return runs[-limit:]
            lines = self.follow(d, runs[-20:], more=more)
            self.assertEqual(calls, [80])
            self.assertIn("closed", lines[0])
            self.assertIn("failed 22 times in a row", lines[1])
            self.assertEqual(self.drafts(d), ["o-r-main-ci-10.md", "o-r-main-ci-13.md"])

    def test_first_reports_dominated_by_cancelled_runs_fetch_further_back(self):
        runs = history("success", "failure", *["cancelled"] * 25, "failure")
        with tempfile.TemporaryDirectory() as d:
            lines = self.follow(d, runs[-20:], more=lambda name, limit: runs[-limit:])
            self.assertIn("failed 2 times in a row", lines[0])
            self.assertEqual(self.drafts(d), ["o-r-main-ci-11.md"])

    def test_a_failed_fetch_counts_the_window_and_the_next_report_recovers(self):
        runs = history("failure", "failure", "success", *["failure"] * 22)
        def boom(*a):
            raise RuntimeError("gh down")
        with tempfile.TemporaryDirectory() as d:
            self.follow(d, runs[:2])
            lines = self.follow(d, runs[-20:], more=boom)
            self.assertIn("could not fetch older runs", lines[0])
            # the window alone already shows a streak of 20 that shares no run with the
            # first draft, so it opens its own draft, and the next report changes nothing
            self.assertIn("or more times in a row", lines[-1])
            self.assertEqual(self.drafts(d), ["o-r-main-ci-10.md", "o-r-main-ci-15.md"])
            self.assertEqual(self.follow(d, runs[-20:], more=lambda name, limit: runs[-limit:]), [])
            self.assertEqual(self.drafts(d), ["o-r-main-ci-10.md", "o-r-main-ci-15.md"])

    def test_a_late_completion_is_counted(self):
        # run 10 was still running when run 11 failed; then run 10 failed too
        early = history("success", start=9) + history("failure", start=11)
        late = early + history("failure", start=10)
        with tempfile.TemporaryDirectory() as d:
            self.assertEqual(self.follow(d, early), [])
            self.assertIn("failed 2 times", self.follow(d, late)[0])
            self.assertEqual(self.drafts(d), ["o-r-main-ci-10.md"])
            # and a late older failure that extends a streak backwards opens no second draft
            older = late + history("failure", start=8)
            self.assertEqual(self.follow(d, [r for r in older if r["databaseId"] != 9]), [])
            self.assertEqual(self.drafts(d), ["o-r-main-ci-10.md"])

    def test_a_rerun_that_passes_closes_the_streak(self):
        runs = history("success", "failure", "failure")
        with tempfile.TemporaryDirectory() as d:
            self.follow(d, runs)
            rerun = runs[:2] + [dict(runs[2], conclusion="success")]
            self.assertIn("closed", self.follow(d, rerun)[0])
            self.assertTrue(self.record(d, "o-r-main-ci-11.md")["closed"])
            self.assertEqual(self.follow(d, rerun), [])

    def test_an_older_report_opens_no_second_draft(self):
        runs = history("success", "failure", "failure", *["failure"] * 30)
        with tempfile.TemporaryDirectory() as d:
            self.follow(d, runs[:3])
            self.follow(d, runs[:1])  # a stale snapshot of the pass
            self.assertEqual(self.follow(d, runs[-20:], more=lambda name, limit: runs[-limit:]), [])
            self.assertEqual(self.drafts(d), ["o-r-main-ci-11.md"])

    def test_a_streak_that_ended_between_reports_opens_no_draft(self):
        runs = history("success", "failure", "failure", "success")
        with tempfile.TemporaryDirectory() as d:
            self.follow(d, runs[:1])
            self.assertEqual(self.follow(d, runs), [])
            self.assertEqual(self.drafts(d), [])

    def test_repositories_and_branches_keep_their_own_streaks(self):
        with tempfile.TemporaryDirectory() as d:
            self.follow(d, history("failure", "failure"))
            self.assertEqual(len(self.follow(d, history("failure", "failure"), branch="dev")), 1)
            self.assertEqual(len(self.follow(d, history("failure", "failure"), repo="o/other")), 1)
            self.assertEqual(self.drafts(d), ["o-other-main-ci-10.md", "o-r-dev-ci-10.md", "o-r-main-ci-10.md"])
            self.assertIn("fails on dev", (gr.Path(d) / "drafts" / "o-r-dev-ci-10.md").read_text())
            self.follow(d, history("failure", "failure", "success"), branch="dev")
            self.assertFalse(self.record(d, "o-r-main-ci-10.md")["closed"])
            self.assertTrue(self.record(d, "o-r-dev-ci-10.md")["closed"])

    def test_a_workflow_id_keeps_a_renamed_workflow_one_streak(self):
        a = [dict(r, workflowDatabaseId=7) for r in history("failure")]
        b = [dict(r, workflowDatabaseId=7, workflowName="CI renamed") for r in history("failure", start=11)]
        with tempfile.TemporaryDirectory() as d:
            self.follow(d, a)
            self.assertIn("failed 2 times", self.follow(d, a + b)[0])

    def test_cancelled_runs_and_other_workflows_neither_open_nor_close_a_streak(self):
        with tempfile.TemporaryDirectory() as d:
            self.follow(d, history("failure", "failure"))
            runs = history("failure", "failure", "cancelled", "cancelled") + history("success", workflow="Nix", start=50)
            self.assertEqual(self.follow(d, runs), [])
            self.assertFalse(self.record(d, "o-r-main-ci-10.md")["closed"])

    def test_a_failed_jobs_lookup_only_shortens_the_draft(self):
        def boom(*a):
            raise RuntimeError("gh down")
        with tempfile.TemporaryDirectory() as d:
            self.assertIn("failed 2 times", self.follow(d, history("failure", "failure"), jobs=boom)[0])

    def test_a_record_of_another_version_is_refused(self):
        with tempfile.TemporaryDirectory() as d:
            (gr.Path(d) / "drafts").mkdir()
            (gr.Path(d) / "drafts" / "x.json").write_text(json.dumps({"version": 2}))
            with self.assertRaises(ValueError):
                self.follow(d, history("failure", "failure"))


class ReportStatus(unittest.TestCase):
    """The streak follower never changes the report's exit status."""

    def run_main(self, argv, runs, view_fails):
        from unittest import mock
        def gh(*args):
            if args[:2] == ("workflow", "list"):
                return json.dumps([{"name": "CI", "state": "active"}])
            if args[:2] == ("run", "list"):
                limit = int(args[args.index("--limit") + 1])
                return json.dumps(sorted(runs, key=gr.order, reverse=True)[:limit])
            if args[:2] == ("run", "view"):
                if view_fails:
                    raise gr.subprocess.CalledProcessError(1, "gh", stderr="boom")
                return json.dumps({"jobs": [{"name": "j", "conclusion": "failure"}]})
            raise AssertionError(args)
        with mock.patch.object(gr, "gh", gh), mock.patch.object(gr.sys, "argv", ["github-runs.py", *argv]), \
                mock.patch("builtins.print"):
            return gr.main()

    def test_a_cancelled_head_after_failures_keeps_its_status(self):
        runs = history("failure", "failure", "cancelled")
        with tempfile.TemporaryDirectory() as d:
            self.assertEqual(self.run_main(["--no-streaks"], runs, view_fails=True), 0)
            self.assertEqual(self.run_main(["--streaks", d], runs, view_fails=True), 0)
            self.assertEqual(len(list((gr.Path(d) / "drafts").glob("*.md"))), 1)
            self.assertEqual(self.run_main(["--streaks", d], runs + history("failure", start=13), view_fails=False), 1)


if __name__ == "__main__":
    unittest.main()
