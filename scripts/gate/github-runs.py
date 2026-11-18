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

It also follows failure streaks. DIR/state.json keeps, for each repository, branch and
workflow, the newest run it has processed and the failed runs in a row since the last
pass, so each run counts once and a report older than the state changes nothing. When the
fetched runs do not reach back to the processed run, it fetches further back. When a
streak reaches two failed runs that were not cancelled, the report writes a roadmap row
draft to DIR/drafts/ and prints its path. A streak opens one draft, and the workflow's
next passing run closes it. A streak that opened and closed between two reports opens
none. DIR defaults to
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
# Finished runs fetched for each workflow when following streaks, and the most fetched
# when that does not reach back to the run the last report processed.
HISTORY = 20
HISTORY_MAX = 200
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


def draft_row(repo, branch, workflow, streak, jobs):
    """A roadmap row draft for a workflow with STREAK's count of failed runs in a row."""
    first, newest = streak["first"], streak["newest"]
    return (
        f"| NNN | **The {workflow} workflow fails on {branch}.** Its last {streak['count']} finished runs "
        f"in {repo} failed, from {first['headSha'][:8]} ({first['url']}) to {newest['headSha'][:8]} "
        f"({newest['url']}), in {', '.join(jobs) or 'the jobs that the run lists'}. The batch gate does "
        f"not run this workflow. Find the change that broke it and fix it forward, or record why it "
        f"can wait. | [DEVELOPMENT](DEVELOPMENT.md#nightly-gate) | S |"
    )


def follow_streaks(repo, branch, runs, state_dir, more=None, jobs=None):
    """Advance each workflow's streak with the runs that the last report did not process,
    open a draft for a streak that now has two or more failed runs and no draft, and close
    a streak at a passing run. Returns the lines to print.

    The state keeps, for each repository, branch and workflow, the newest run processed,
    so a report that is older than the state changes nothing, and a run is counted once.
    When the fetched runs do not reach back to that run, MORE(workflow name, limit)
    fetches a longer history. JOBS(run ID) names a failed run's failed jobs for a draft.
    Neither may raise into the report; their errors only shorten a line."""
    state_dir = Path(state_dir)
    (state_dir / "drafts").mkdir(parents=True, exist_ok=True)
    lines = []
    with open(state_dir / ".lock", "w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        path = state_dir / "state.json"
        try:
            state = json.loads(path.read_text())
        except FileNotFoundError:
            state = {"version": STATE_VERSION, "scopes": {}}
        if state.get("version") != STATE_VERSION or "scopes" not in state:
            raise ValueError(f"{path} is version {state.get('version')!r}, and this script reads {STATE_VERSION}")
        scope = state["scopes"].setdefault(f"{repo}@{branch}", {})
        by_workflow = {}
        for r in runs:
            if r["status"] == "completed":
                by_workflow.setdefault(workflow_key(r), []).append(r)
        for key in sorted(by_workflow):
            mine = sorted(by_workflow[key], key=order)
            name = mine[-1]["workflowName"]
            st = scope.get(key)
            seen = tuple(st["seen"]) if st else None
            if seen is not None and order(mine[-1]) <= seen:
                continue  # an older or equal report: nothing new
            if seen is not None and order(mine[0]) > seen and more is not None:
                # The window does not reach the last processed run, so runs may be missing
                # between them. Fetch further back, up to HISTORY_MAX runs.
                limit = len(mine)
                while order(mine[0]) > seen and limit < HISTORY_MAX:
                    limit = min(HISTORY_MAX, max(limit * 4, HISTORY))
                    try:
                        longer = [r for r in more(name, limit) if workflow_key(r) == key and r["status"] == "completed"]
                    except Exception as e:  # noqa: BLE001 - the follower never fails the report
                        lines.append(f"  {name}: could not fetch more runs ({e}); counting the window only")
                        break
                    if len(longer) <= len(mine):
                        break
                    mine = sorted(longer, key=order)
            gap = seen is not None and order(mine[0]) > seen
            if st is None or gap:
                # No earlier report, or one that the history no longer reaches: count from
                # the window alone, keeping an open streak's draft.
                st = {"seen": None, "count": 0, "first": None, "newest": None,
                      "draft": st.get("draft") if st and gap else None}
                if gap:
                    lines.append(f"  {name}: the runs since the last report exceed {HISTORY_MAX}; counting the window only")
            new = [r for r in mine if st["seen"] is None or order(r) > tuple(st["seen"])]
            for r in new:
                if r["conclusion"] in NEUTRAL:
                    continue
                if r["conclusion"] in FAILED:
                    brief = {k: r[k] for k in ("databaseId", "headSha", "url")}
                    st["first"] = st["first"] or brief
                    st["newest"] = brief
                    st["count"] += 1
                else:
                    if st.get("draft"):
                        since = st["first"]["databaseId"] if st.get("first") else "before the fetched runs"
                        lines.append(f"  {name}: passed again, streak from run {since} closed")
                    st.update(count=0, first=None, newest=None, draft=None)
            st["seen"] = list(order(mine[-1]))
            if st["count"] >= 2 and not st.get("draft"):
                found = []
                if jobs is not None:
                    try:
                        found = jobs(st["newest"]["databaseId"])
                    except Exception:  # noqa: BLE001 - a draft without job names still helps
                        found = []
                slug = re.sub(r"[^a-z0-9]+", "-", f"{repo}-{branch}-{name}".lower()).strip("-")
                draft = state_dir / "drafts" / f"{slug}-{st['first']['databaseId']}.md"
                draft.write_text("Add this row to the roadmap's fix-forward items with the next free number:\n\n"
                                 + draft_row(repo, branch, name, st, found) + "\n")
                st["draft"] = str(draft)
                lines.append(f"  {name}: failed {st['count']} times in a row; roadmap row draft in {draft}")
            scope[key] = st
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
