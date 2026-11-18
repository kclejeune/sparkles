#!/usr/bin/env python3
"""Report the latest finished GitHub Actions run of each workflow on main.

The batch gate runs on the gate hosts and runs none of the GitHub workflows, such as the
server artifact link, the wheel tests or the CI test shards. scripts/gate/gate.sh and
scripts/gate/nightly.sh call this before they start, so that a workflow that failed on
the last push is seen before the next batch goes out.

Usage:
  github-runs.py [--branch BRANCH] [--repo OWNER/NAME] [--streaks DIR | --no-streaks]

The workflows filter on paths, so a push runs only some of them. The report therefore
takes, for each active workflow, its newest finished run on the branch, from a push or a
manual dispatch, whatever commit that run tested. It prints one line for each, with the commit and the failed jobs of a failed run,
and exits 0 when none failed, 1 when one failed and 2 when `gh` cannot list the runs. A
cancelled run is not a failure, and a run still in progress is left for the next report.

It also follows failure streaks. When a workflow's two newest finished runs that were not
cancelled both failed, the workflow is red on the branch, and the first report that sees
the streak writes a roadmap row draft to DIR/drafts/ and prints its path. DIR/state.json
remembers each open streak by its first failed run, so later reports of the same streak
write no new draft, and the workflow's next passing run closes it. DIR defaults to
.artifacts/gates/github-streaks in the main checkout, which every worktree shares. The
script never edits the roadmap, commits or pushes, and a streak does not change its exit
status.
"""

import argparse
import fcntl
import json
import os
import re
import subprocess
import sys
from pathlib import Path

FAILED = {"failure", "timed_out", "startup_failure", "action_required"}
# Runs that say nothing about the workflow's state, which a streak passes over.
NEUTRAL = {"cancelled", "skipped", "neutral", "stale"}
# Finished runs fetched for each workflow, enough to see two that were not cancelled.
HISTORY = 6
STATE_VERSION = 1


def gh(*args):
    return subprocess.run(["gh", *args], check=True, capture_output=True, text=True).stdout


def latest_runs(runs):
    """The newest finished run of each workflow."""
    newest = {}
    for run in runs:
        if run["status"] != "completed":
            continue
        name = run["workflowName"]
        if name not in newest or run["createdAt"] > newest[name]["createdAt"]:
            newest[name] = run
    return sorted(newest.values(), key=lambda r: r["workflowName"])


def summarize(runs, failed_jobs):
    """Lines describing RUNS and whether any failed. FAILED_JOBS maps a run ID to the
    names of its failed jobs."""
    lines = []
    bad = False
    for run in latest_runs(runs):
        line = f"  {run['workflowName']}: {run['conclusion']} on {run['headSha'][:8]}"
        if run["conclusion"] in FAILED:
            bad = True
            jobs = failed_jobs.get(run["databaseId"], [])
            if jobs:
                line += " (" + ", ".join(jobs) + ")"
            line += f" {run['url']}"
        lines.append(line)
    return lines, bad


def streak(runs, workflow):
    """The failed runs, newest first, at the head of WORKFLOW's finished history, passing
    over cancelled runs. Empty when its newest such run did not fail."""
    mine = sorted((r for r in runs if r["workflowName"] == workflow and r["status"] == "completed"
                   and r["conclusion"] not in NEUTRAL), key=lambda r: r["createdAt"], reverse=True)
    out = []
    for r in mine:
        if r["conclusion"] not in FAILED:
            break
        out.append(r)
    return out


def draft_row(workflow, failed, failed_jobs):
    """A roadmap row draft for a workflow that failed twice or more in a row."""
    first, newest = failed[-1], failed[0]
    jobs = ", ".join(failed_jobs.get(newest["databaseId"], [])) or "see the run"
    return (
        f"| NNN | **The {workflow} workflow fails on main.** Its last {len(failed)} finished runs "
        f"failed, from {first['headSha'][:8]} ({first['url']}) to {newest['headSha'][:8]} "
        f"({newest['url']}), in {jobs}. The batch gate does not run this workflow. Find the change "
        f"that broke it and fix it forward, or record why it can wait. "
        f"| [DEVELOPMENT](DEVELOPMENT.md#nightly-gate) | S |"
    )


def follow_streaks(runs, failed_jobs, state_dir):
    """Open a draft for each new streak of two or more failed runs and close the streaks
    whose workflow passed. Returns the lines to print."""
    state_dir = Path(state_dir)
    (state_dir / "drafts").mkdir(parents=True, exist_ok=True)
    lines = []
    with open(state_dir / ".lock", "w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        path = state_dir / "state.json"
        try:
            state = json.loads(path.read_text())
        except FileNotFoundError:
            state = {"version": STATE_VERSION, "open": {}}
        if state.get("version") != STATE_VERSION:
            raise ValueError(f"{path} is version {state.get('version')!r}, and this script reads {STATE_VERSION}")
        opened = state["open"]
        for workflow in sorted({r["workflowName"] for r in runs}):
            failed = streak(runs, workflow)
            seen = sum(1 for r in runs if r["workflowName"] == workflow and r["status"] == "completed"
                       and r["conclusion"] not in NEUTRAL)
            known = opened.get(workflow)
            # A passing run in view below the failures, or at their head, ends a streak.
            ended = known is not None and seen > 0 and (
                not failed or (len(failed) < seen and known["first"] not in {r["databaseId"] for r in failed}))
            if ended:
                lines.append(f"  {workflow}: passed again, streak from run {known['first']} closed")
                del opened[workflow]
                known = None
            if not failed:
                continue
            if known is not None:
                # The same streak, while its first run is still in view or its failures
                # continue past the fetched history.
                known["newest"] = failed[0]["databaseId"]
                continue
            if len(failed) < 2:
                continue
            slug = re.sub(r"[^a-z0-9]+", "-", workflow.lower()).strip("-")
            draft = state_dir / "drafts" / f"{slug}-{failed[-1]['databaseId']}.md"
            draft.write_text("Add this row to the roadmap's fix-forward items with the next free number:\n\n"
                             + draft_row(workflow, failed, failed_jobs) + "\n")
            opened[workflow] = {"first": failed[-1]["databaseId"], "newest": failed[0]["databaseId"],
                                "draft": str(draft)}
            lines.append(f"  {workflow}: failed {len(failed)} times in a row; roadmap row draft in {draft}")
        tmp = path.with_suffix(".tmp")
        tmp.write_text(json.dumps(state, indent=2, sort_keys=True) + "\n")
        os.replace(tmp, path)
    return lines


def default_streak_dir():
    common = subprocess.run(["git", "rev-parse", "--path-format=absolute", "--git-common-dir"],
                            check=True, capture_output=True, text=True,
                            cwd=Path(__file__).resolve().parent).stdout.strip()
    return Path(common).parent / ".artifacts/gates/github-streaks"


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--branch", default="main")
    ap.add_argument("--repo", default="kclejeune/sparkles")
    ap.add_argument("--streaks", help="the directory of the streak state and drafts")
    ap.add_argument("--no-streaks", action="store_true", help="do not follow failure streaks")
    args = ap.parse_args()
    try:
        # One query per workflow, so that a workflow that runs rarely is not crowded out
        # by the runs of the others. Pushes and manual dispatches on the branch both count.
        workflows = json.loads(gh("workflow", "list", "--repo", args.repo, "--json", "name,state"))
        runs = []
        for wf in workflows:
            if wf["state"] != "active":
                continue
            runs += json.loads(gh(
                "run", "list", "--repo", args.repo, "--workflow", wf["name"], "--branch", args.branch,
                "--status", "completed", "--limit", str(HISTORY),
                "--json", "databaseId,workflowName,status,conclusion,createdAt,headSha,url",
            ))
        failed_jobs = {}
        heads = [streak(runs, w)[:1] for w in {r["workflowName"] for r in runs}]
        for run in latest_runs(runs) + [r for h in heads for r in h]:
            if run["conclusion"] in FAILED and run["databaseId"] not in failed_jobs:
                view = json.loads(gh("run", "view", "--repo", args.repo, str(run["databaseId"]), "--json", "jobs"))
                failed_jobs[run["databaseId"]] = [
                    j["name"] for j in view["jobs"] if j["conclusion"] in FAILED
                ]
    except (OSError, subprocess.CalledProcessError, json.JSONDecodeError) as e:
        detail = getattr(e, "stderr", "") or str(e)
        print(f"github runs on {args.branch}: gh could not list them: {detail.strip()}")
        return 2
    if not latest_runs(runs):
        print(f"github runs on {args.branch}: none finished")
        return 0
    lines, bad = summarize(runs, failed_jobs)
    print(f"github runs on {args.branch}, the newest finished run of each workflow: {'FAILED' if bad else 'no failures'}")
    print("\n".join(lines))
    if not args.no_streaks:
        try:
            notes = follow_streaks(runs, failed_jobs, args.streaks or default_streak_dir())
        except (OSError, ValueError, subprocess.CalledProcessError) as e:
            notes = [f"  the failure streaks could not be followed: {e}"]
        if notes:
            print("failure streaks:")
            print("\n".join(notes))
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
