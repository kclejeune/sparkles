#!/usr/bin/env python3
"""Rerun a batch gate's failed Rust tests once each, alone, and decide whether they were flaky.

gate-inner.sh runs this in the gate's worktree after scripts/ci.sh and the ci-receipts
step, when the ci step failed and its evidence was retained. It forgives the ci step
only on positive evidence that the failure was the test task's alone and that every
failed test passes when it runs again by itself:

- The retained JUnit manifest records the test task as selected, retained and failed,
  for this commit, with a report written during the task.
- Every other ci task and the shared prerequisites passed, and the test task failed.
- nextest's final summary in the test task's log is complete, with no test left unrun,
  and its failed and timed-out counts add up to the failures in the JUnit report.
- That count is between 1 and --max-failures. A larger count is not a flake.

Each failed test then runs alone with `mbx nextest run --no-capture --retries 0` and an
exact binary and test filter, within --seconds in all. It passes only when its own JUnit
report lists exactly that test, passed, and its log shows no new compilation.
Since a failed nextest step ends the test task before its doctests, the doctests of the
task's packages then run too. When everything passes, the script writes ci.forgiven with
one `binary test` line per flaky test and exits 0. Otherwise it writes the reason to
test-retry.txt and exits 1, and the ci step's failure stands. ci.exit is never changed.

Usage: test_retry.py --out OUT --commit FULL [--max-failures N] [--seconds S]
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import time
import xml.etree.ElementTree as ET
from pathlib import Path

# The categories of nextest's final summary line that this script understands.
SUMMARY = re.compile(r"^\s*Summary \[[^\]]*\]\s+(\d+) tests? run: (.*)$")
KNOWN = {"passed", "failed", "timed out", "skipped", "flaky", "leaky", "slow"}


class Decline(Exception):
    """The failure is not one this script may forgive."""


def summary_counts(log_text):
    """The counts of nextest's last final summary line in LOG_TEXT, as a dict from
    category to number. A summary such as `10/20 tests run`, which a cancelled or
    interrupted run prints, or one with a category this script does not know, declines."""
    lines = [l for l in log_text.splitlines() if l.lstrip().startswith("Summary [")]
    if not lines:
        raise Decline("the test log has no nextest summary")
    m = SUMMARY.match(lines[-1])
    if not m:
        raise Decline(f"the nextest summary is not a complete run: {lines[-1].strip()}")
    counts = {"run": int(m.group(1))}
    # "9395 passed (66 slow), 3 failed, 2 timed out, 54 skipped"
    for part in re.sub(r"\(([^)]*)\)", r", \1", m.group(2)).split(","):
        part = part.strip()
        if not part:
            continue
        pm = re.match(r"^(\d+) (.+)$", part)
        if not pm or pm.group(2) not in KNOWN:
            raise Decline(f"the nextest summary has a part this script does not know: {part}")
        counts[pm.group(2)] = int(pm.group(1))
    return counts


def junit_failures(path):
    """The (binary id, test name) of every test case that failed in the JUnit report."""
    root = ET.parse(path).getroot()
    out = []
    for suite in root.iter("testsuite"):
        for case in suite.iter("testcase"):
            if case.find("failure") is not None or case.find("error") is not None:
                out.append((suite.get("name", ""), case.get("name", "")))
    return out


def junit_cases(path):
    """Every (binary id, test name, passed) in a JUnit report."""
    root = ET.parse(path).getroot()
    return [
        (s.get("name", ""), c.get("name", ""), c.find("failure") is None and c.find("error") is None
         and c.find("skipped") is None)
        for s in root.iter("testsuite") for c in s.iter("testcase")
    ]


def eligible(out, commit, max_failures):
    """The failed tests of the gate's ci step in OUT when this script may rerun them, and
    the selected packages of the test task. Raises Decline otherwise."""
    rec = out / "ci-receipts"
    try:
        manifest = json.loads((rec / "junit/manifest.json").read_text())
    except (OSError, ValueError) as e:
        raise Decline(f"no readable JUnit manifest ({e})") from None
    if manifest.get("version") != 1 or manifest.get("task") != "test":
        raise Decline("the JUnit manifest is not a version 1 manifest of the test task")
    if manifest.get("selected") is not True or manifest.get("status") != "retained":
        raise Decline("the test task's report was not selected and retained")
    if manifest.get("task_exit") in (0, None):
        raise Decline("the test task did not fail")
    if manifest.get("source_commit") != commit or manifest.get("source_dirty_status"):
        raise Decline("the JUnit manifest names another commit or a dirty worktree")
    reports = manifest.get("reports") or []
    if len(reports) != 1 or reports[0].get("profile") != "default" \
            or reports[0].get("freshness") != "new-or-changed-since-task-start":
        raise Decline("the test task did not write one fresh report of the default profile")
    junit = rec / "junit" / reports[0]["retained_path"]
    states = {}
    for f in sorted((rec / "state").glob("*")):
        states[f.name] = f.read_text().split(" ", 1)[0].strip()
    if states.get("test") != "FAIL":
        raise Decline("the ci summary does not record the test task as failed")
    others = sorted(k for k, v in states.items() if k != "test" and v != "pass")
    if others:
        raise Decline(f"other ci steps did not pass: {' '.join(others)}")
    counts = summary_counts((rec / "logs/test.log").read_text(errors="replace"))
    try:
        failures = junit_failures(junit)
    except (OSError, ET.ParseError) as e:
        raise Decline(f"the JUnit report does not parse ({e})") from None
    expected = counts.get("failed", 0) + counts.get("timed out", 0)
    if not failures or len(failures) != expected:
        raise Decline(f"the JUnit report lists {len(failures)} failures and nextest's summary {expected}")
    if len(set(failures)) != len(failures):
        raise Decline("the JUnit report lists a failed test twice")
    if len(failures) > max_failures:
        raise Decline(f"{len(failures)} tests failed, more than the {max_failures} a rerun may forgive")
    return failures, manifest.get("selection_packages") or "all"


def filterset(binary, name):
    """An exact nextest filter for one test."""
    for v in (binary, name):
        if any(c in v for c in "()\n"):
            raise Decline(f"cannot write an exact filter for {v!r}")
    return f"binary_id(={binary}) & test(={name})"


def run(cmd, log, seconds):
    """Run CMD with its output in LOG, for at most SECONDS. Returns the exit code, or
    None when it ran out of time."""
    with open(log, "w") as f:
        f.write("$ " + " ".join(cmd) + "\n")
        f.flush()
        try:
            return subprocess.run(cmd, stdout=f, stderr=subprocess.STDOUT, timeout=max(1, seconds)).returncode
        except subprocess.TimeoutExpired:
            f.write(f"\nstopped after {seconds} s\n")
            return None


def rebuilt(log_text):
    """Whether a log shows a workspace crate compiled anew. Cargo prints Compiling for a
    crate that Boxington restores from its cache too, so its cache line decides when it
    is there: a miss is a new compilation, which a rerun of the same build never needs."""
    m = re.search(r"mbx\[cache\]: \d+ hits?, (\d+) miss", log_text)
    if m:
        return int(m.group(1)) > 0
    return re.search(r"^\s*Compiling sparkles[a-z0-9-]* v", log_text, re.M) is not None


def main(argv=None, runner=run, junit_path=Path("target/nextest/default/junit.xml")):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--commit", required=True)
    ap.add_argument("--max-failures", type=int, default=8)
    ap.add_argument("--seconds", type=int, default=2400)
    args = ap.parse_args(argv)
    out = args.out
    report = out / "test-retry.txt"
    rdir = out / "test-retry"
    deadline = time.monotonic() + args.seconds
    lines = []
    (out / "ci.forgiven").unlink(missing_ok=True)

    def finish(code, reason):
        lines.append(reason)
        report.write_text("\n".join(lines) + "\n")
        print(reason)
        return code

    try:
        failures, packages = eligible(out, args.commit, args.max_failures)
    except Decline as e:
        return finish(1, f"not rerun: {e}")
    rdir.mkdir(exist_ok=True)
    prefix = ["mise", "exec", "--"]
    for i, (binary, name) in enumerate(failures, 1):
        try:
            expr = filterset(binary, name)
        except Decline as e:
            return finish(1, f"not forgiven: {e}")
        left = int(deadline - time.monotonic())
        if left <= 0:
            return finish(1, "not forgiven: the reruns ran out of time")
        log = rdir / f"{i}.log"
        if junit_path.exists():
            junit_path.unlink()
        code = runner(prefix + ["mbx", "nextest", "run", "--workspace", "--no-capture", "--retries", "0",
                                "--no-fail-fast", "-E", expr], log, min(left, 900))
        text = log.read_text(errors="replace") if log.exists() else ""
        cases = []
        if junit_path.exists():
            shutil.copy(junit_path, rdir / f"{i}-junit.xml")
            try:
                cases = junit_cases(junit_path)
            except ET.ParseError:
                cases = []
        ok = code == 0 and cases == [(binary, name, True)] and not rebuilt(text)
        lines.append(f"{'PASS' if ok else 'FAIL'} {binary} {name} (exit {code}, log {log.name})")
        if not ok:
            why = "rebuilt a workspace crate" if rebuilt(text) else "failed again or did not run alone"
            return finish(1, f"not forgiven: {binary} {name} {why}")
    left = int(deadline - time.monotonic())
    if left <= 0:
        return finish(1, "not forgiven: no time left for the doctests")
    env_pkgs = subprocess.run(
        ["python3", "scripts/changed-crates.py", "--cargo-args", "--doc"],
        env=dict(os.environ, SPARKLES_CHANGED_PACKAGES=packages), capture_output=True, text=True,
    )
    pkgs = env_pkgs.stdout.split()
    if env_pkgs.returncode != 0 or not pkgs:
        return finish(1, "not forgiven: cannot choose the doctest packages")
    if pkgs != ["none"]:
        log = rdir / "doctests.log"
        code = runner(prefix + ["mbx", "test", *pkgs, "--doc"], log, left)
        lines.append(f"{'PASS' if code == 0 else 'FAIL'} doctests {' '.join(pkgs)} (exit {code}, log {log.name})")
        if code != 0:
            return finish(1, "not forgiven: the doctests failed")
    (out / "ci.forgiven").write_text("".join(f"{b} {n}\n" for b, n in failures))
    return finish(0, f"forgiven: {len(failures)} flaky test{'s' if len(failures) != 1 else ''} passed alone")


if __name__ == "__main__":
    sys.exit(main())
