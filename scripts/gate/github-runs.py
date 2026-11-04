#!/usr/bin/env python3
"""Report the latest finished GitHub Actions run of each workflow on main.

The batch gate runs on the gate hosts and runs none of the GitHub workflows, such as the
server artifact link, the wheel tests or the CI test shards. scripts/gate/gate.sh and
scripts/gate/nightly.sh call this before they start, so that a workflow that failed on
the last push is seen before the next batch goes out.

Usage:
  github-runs.py [--branch BRANCH] [--repo OWNER/NAME]

The workflows filter on paths, so a push runs only some of them. The report therefore
takes, for each workflow, its newest finished run on the branch, whatever commit that run
tested. It prints one line for each, with the commit and the failed jobs of a failed run,
and exits 0 when none failed, 1 when one failed and 2 when `gh` cannot list the runs. A
cancelled run is not a failure, and a run still in progress is left for the next report.
"""

import argparse
import json
import subprocess
import sys

FAILED = {"failure", "timed_out", "startup_failure", "action_required"}


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


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--branch", default="main")
    ap.add_argument("--repo", default="kclejeune/sparkles")
    args = ap.parse_args()
    try:
        runs = json.loads(gh(
            "run", "list", "--repo", args.repo, "--branch", args.branch, "--event", "push",
            "--limit", "100", "--json", "databaseId,workflowName,status,conclusion,createdAt,headSha,url",
        ))
        failed_jobs = {}
        for run in latest_runs(runs):
            if run["conclusion"] in FAILED:
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
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
