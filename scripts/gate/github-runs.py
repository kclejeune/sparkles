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

It also follows failure streaks. Each report works out, from each workflow's history on
the branch, the failed runs since its newest run that neither failed nor was cancelled,
and fetches further back when the fetched runs do not reach such a run. When there are
two or more, it writes a roadmap row draft to DIR/drafts/ and prints its path, unless the
record kept beside an earlier draft shares one of the failed runs, in which case that
draft already covers the streak. A run that finished late or a rerun that changed its
result counts as it now stands. DIR defaults to .artifacts/gates/github-streaks in the
main checkout, which every worktree shares. The script never edits the roadmap, commits
or pushes, and neither a streak nor an error in following one changes its exit status.
"""

import argparse
import fcntl
import json
import re
import subprocess
import sys
from pathlib import Path

FAILED = {"failure", "timed_out", "startup_failure", "action_required"}
# Runs that say nothing about the workflow's state, which a streak passes over.
NEUTRAL = {"cancelled", "skipped", "neutral", "stale"}
# Finished runs fetched for each workflow when following streaks, and the most fetched
# when that does not reach back to the run the last report processed.
HISTORY = 20
HISTORY_MAX = 200
# The version of the record kept beside each draft.
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


def order(run):
    return (run["createdAt"], run["databaseId"])


def workflow_key(run):
    """A workflow's stable identity: its database ID when gh reports one, else its name."""
    wid = run.get("workflowDatabaseId")
    return f"id:{wid}" if wid is not None else f"name:{run['workflowName']}"


def draft_row(repo, branch, workflow, failed, jobs):
    """A roadmap row draft for a workflow whose runs FAILED, oldest first, failed in a row."""
    first, newest = failed[0], failed[-1]
    return (
        f"| NNN | **The {workflow} workflow fails on {branch}.** Its last {len(failed)} finished runs "
        f"in {repo} failed, from {first['headSha'][:8]} ({first['url']}) to {newest['headSha'][:8]} "
        f"({newest['url']}), in {', '.join(jobs) or 'the jobs that the run lists'}. The batch gate does "
        f"not run this workflow. Find the change that broke it and fix it forward, or record why it "
        f"can wait. | [DEVELOPMENT](DEVELOPMENT.md#nightly-gate) | S |"
    )


def current_streak(mine):
    """The failed runs, oldest first, after the newest finished run of MINE that neither
    failed nor was cancelled, and whether that boundary run is among MINE."""
    meaningful = [r for r in sorted(mine, key=order) if r["conclusion"] not in NEUTRAL]
    out = []
    for r in reversed(meaningful):
        if r["conclusion"] not in FAILED:
            return list(reversed(out)), True
        out.append(r)
    return list(reversed(out)), False


def follow_streaks(repo, branch, runs, state_dir, more=None, jobs=None):
    """Write a roadmap row draft for each workflow whose newest finished runs that were not
    cancelled include two or more failures in a row, unless a draft already covers that
    streak. Returns the lines to print.

    Each report works the streak out again from GitHub's history, so a run that finished
    late or a rerun that changed a result is counted as it now stands. When the fetched
    runs show no run that ended the streak, MORE(workflow name, limit) fetches further
    back, up to HISTORY_MAX runs. Each draft has a record beside it with its repository,
    branch, workflow and the IDs of the streak's failed runs, which each report extends.
    A streak whose failed runs meet a record's is that record's streak, so a streak opens
    one draft however its first run appears. JOBS(run ID) names a failed run's failed
    jobs. Errors of MORE and JOBS only shorten a line."""
    state_dir = Path(state_dir)
    drafts = state_dir / "drafts"
    drafts.mkdir(parents=True, exist_ok=True)
    lines = []
    by_workflow = {}
    for r in runs:
        if r["status"] == "completed":
            by_workflow.setdefault(workflow_key(r), []).append(r)
    with open(state_dir / ".lock", "w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        records = {}
        for f in sorted(drafts.glob("*.json")):
            rec = json.loads(f.read_text())
            if rec.get("version") != STATE_VERSION:
                raise ValueError(f"{f} is version {rec.get('version')!r}, and this script reads version {STATE_VERSION}")
            if (rec["repo"], rec["branch"]) == (repo, branch):
                records.setdefault(rec["workflow"], []).append((f, rec))
        for key in sorted(by_workflow):
            mine = by_workflow[key]
            name = max(mine, key=order)["workflowName"]
            failed, bounded = current_streak(mine)
            # A window shorter than the fetch limit holds the whole history.
            limit = len(mine)
            full = limit >= HISTORY
            while failed and not bounded and full and limit < HISTORY_MAX and more is not None:
                limit = min(HISTORY_MAX, limit * 4)
                try:
                    longer = [r for r in more(name, limit) if workflow_key(r) == key and r["status"] == "completed"]
                except Exception as e:  # noqa: BLE001 - the follower never fails the report
                    lines.append(f"  {name}: could not fetch older runs ({e}); counting the fetched runs only")
                    break
                full = len(longer) >= limit
                if len(longer) <= len(mine):
                    break
                mine = longer
                failed, bounded = current_streak(mine)
            ids = {r["databaseId"] for r in failed}
            mine_records = records.get(key, [])
            match = [(f, rec) for f, rec in mine_records if ids & set(rec["runs"])]
            for f, rec in mine_records:
                if (f, rec) in match or rec.get("closed"):
                    continue
                # a record whose streak shares no run with the current one has ended
                rec["closed"] = True
                f.write_text(json.dumps(rec, indent=2, sort_keys=True) + "\n")
                lines.append(f"  {name}: passed again, streak of {Path(rec['draft']).name} closed")
            if match:
                for f, rec in match:
                    rec["runs"] = sorted(set(rec["runs"]) | ids)[-HISTORY_MAX:]
                    rec["closed"] = False
                    f.write_text(json.dumps(rec, indent=2, sort_keys=True) + "\n")
                continue
            if len(failed) < 2:
                continue
            found = []
            if jobs is not None:
                try:
                    found = jobs(failed[-1]["databaseId"])
                except Exception:  # noqa: BLE001 - a draft without job names still helps
                    found = []
            slug = re.sub(r"[^a-z0-9]+", "-", f"{repo}-{branch}-{name}".lower()).strip("-")
            draft = drafts / f"{slug}-{failed[0]['databaseId']}.md"
            draft.write_text("Add this row to the roadmap's fix-forward items with the next free number:\n\n"
                             + draft_row(repo, branch, name, failed, found) + "\n")
            rec = {"version": STATE_VERSION, "repo": repo, "branch": branch, "workflow": key,
                   "name": name, "draft": str(draft), "runs": sorted(ids), "closed": False}
            draft.with_suffix(".json").write_text(json.dumps(rec, indent=2, sort_keys=True) + "\n")
            more_runs = " or more" if not bounded and full else ""
            lines.append(f"  {name}: failed {len(failed)}{more_runs} times in a row; roadmap row draft in {draft}")
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
    fields = "databaseId,workflowName,workflowDatabaseId,status,conclusion,createdAt,headSha,url"

    def run_list(workflow, limit):
        return json.loads(gh(
            "run", "list", "--repo", args.repo, "--workflow", workflow, "--branch", args.branch,
            "--status", "completed", "--limit", str(limit), "--json", fields,
        ))

    def failed_jobs_of(run_id):
        view = json.loads(gh("run", "view", "--repo", args.repo, str(run_id), "--json", "jobs"))
        return [j["name"] for j in view["jobs"] if j["conclusion"] in FAILED]

    try:
        # One query per workflow, so that a workflow that runs rarely is not crowded out
        # by the runs of the others. Pushes and manual dispatches on the branch both count.
        workflows = json.loads(gh("workflow", "list", "--repo", args.repo, "--json", "name,state"))
        runs = []
        for wf in workflows:
            if wf["state"] != "active":
                continue
            runs += run_list(wf["name"], 1 if args.no_streaks else HISTORY)
        failed_jobs = {}
        for run in latest_runs(runs):
            if run["conclusion"] in FAILED:
                failed_jobs[run["databaseId"]] = failed_jobs_of(run["databaseId"])
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
            notes = follow_streaks(args.repo, args.branch, runs, args.streaks or default_streak_dir(),
                                   more=run_list, jobs=lambda i: failed_jobs.get(i) or failed_jobs_of(i))
        except Exception as e:  # noqa: BLE001 - the follower never changes the report's status
            notes = [f"  the failure streaks could not be followed: {e}"]
        if notes:
            print("failure streaks:")
            print("\n".join(notes))
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
