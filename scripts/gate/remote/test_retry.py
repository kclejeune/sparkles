#!/usr/bin/env python3
"""Rerun a batch gate's failed Rust tests once each, alone, and decide whether they were flaky.

gate-inner.sh runs this in the gate's worktree after scripts/ci.sh and the ci-receipts
step, when the ci step failed and its evidence was retained. It forgives the ci step
only on positive evidence that the failure was the test task's alone and that every
failed test passes when it runs again by itself:

- The ci step ran to its end and failed on the test task alone. ci.exit is 1, the last
  line of ci.log is `CI failed: test`, and the retained summary.txt lists exactly the
  tasks that the gate selected, each with a final state that agrees with its state
  file. Every task and the shared prerequisites passed, and the test task failed. A
  run that was killed, stopped or interrupted, or that left a task waiting, declines.
- The retained JUnit manifest records the test task as selected, retained and failed,
  for this commit, with a report written during the task.
- The worktree is still at this commit, with no tracked change and no untracked file
  that git does not ignore, both before the reruns and after them.
- nextest's final summary in the test task's log is complete, with no test left unrun,
  and its failed and timed-out counts add up to the failures in the JUnit report.
- That count is between 1 and --max-failures. A larger count is not a flake.

Each failed test then runs alone with `mbx nextest run --no-capture --retries 0` and an
exact binary and test filter, within --seconds in all. Each rerun gets a new empty
TMPDIR, and target/tmp (CARGO_TARGET_TMPDIR) is emptied before it, so scratch that the
first run left there cannot make a rerun pass. Other ignored paths in the worktree and
state outside it are not reset. Each command runs in its own session, and when it runs
out of time every process left in that session is stopped, including those in other
process groups such as nextest's test processes. It passes only when its own JUnit
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
import signal
import subprocess
import sys
import tempfile
import time
import xml.etree.ElementTree as ET
from pathlib import Path

# Seconds between SIGTERM and SIGKILL for a command that ran out of time. nextest
# forwards SIGTERM to its tests and gives them 10 seconds before it kills them.
GRACE = 20

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


def full_tasks(ci_sh=Path("scripts/ci.sh")):
    """The tasks that scripts/ci.sh runs when it is given none, from its `full=(` list."""
    try:
        m = re.search(r"^\s*full=\(([^)]*)\)", ci_sh.read_text(), re.M)
    except OSError:
        m = None
    if not m or not m.group(1).split():
        raise Decline("cannot read scripts/ci.sh's full task list")
    return m.group(1).split()


def selected_tasks(out, ci_sh=Path("scripts/ci.sh")):
    """The ci tasks that the gate gave scripts/ci.sh, from selection.txt. Like
    gate-inner.sh, a missing, empty or `all` ci line means ci.sh's full list."""
    try:
        text = (out / "selection.txt").read_text()
    except OSError:
        raise Decline("the gate wrote no selection.txt") from None
    lines = [l for l in text.splitlines() if l == "ci" or l.startswith("ci ")]
    if len(lines) > 1:
        raise Decline("selection.txt has more than one ci line")
    tasks = lines[0].split()[1:] if lines else []
    if not tasks or tasks == ["all"]:
        return full_tasks(ci_sh)
    return tasks


def ci_complete(out, ci_sh=Path("scripts/ci.sh")):
    """Decline unless the ci step ran every selected task to a final state and failed on
    the test task alone. Its exit is 1, ci.sh's own failure exit, its log ends with
    `CI failed: test`, and the retained summary lists the selected tasks, and the shared
    prerequisites when there are any, each passed except the failed test task."""
    rec = out / "ci-receipts"
    try:
        code = (out / "ci.exit").read_text().strip()
    except OSError:
        raise Decline("the ci step has no exit code") from None
    if code != "1":
        raise Decline(f"the ci step exited {code}, not ci.sh's failure exit 1")
    try:
        log = (out / "ci.log").read_text(errors="replace").splitlines()
    except OSError:
        raise Decline("the ci step has no log") from None
    # Boxington's own report lines (mbx[gc], mbx[savings] and the like) can follow
    # ci.sh's last line, since its cache maintenance writes to the same log.
    last = [l for l in log if l.strip() and not l.startswith("mbx[")][-1:]
    if last != ["CI failed: test"]:
        raise Decline("the ci step's log does not end with ci.sh's `CI failed: test`")
    try:
        summary = (rec / "summary.txt").read_text().splitlines()
    except OSError:
        raise Decline("the ci receipts have no summary.txt") from None
    if not summary or not summary[0].startswith("CI started "):
        raise Decline("the ci summary has no header")
    rows = {}
    for line in summary[1:]:
        if not line.strip():
            continue
        parts = line.split(None, 1)
        if len(parts) != 2 or parts[0] in rows:
            raise Decline(f"the ci summary has a row this script does not understand: {line}")
        rows[parts[0]] = parts[1].split(" ", 1)[0]
    expected = selected_tasks(out, ci_sh)
    listed = [t for t in rows if t != "prepare"]
    if sorted(listed) != sorted(expected):
        raise Decline("the ci summary does not list exactly the selected tasks")
    for task, state in rows.items():
        want = "FAIL" if task == "test" else "pass"
        if state != want:
            raise Decline(f"the ci summary records {task} as {state}, not {want}")
        try:
            recorded = (rec / "state" / task.replace(":", "_")).read_text().split(" ", 1)[0].strip()
        except OSError:
            raise Decline(f"the ci receipts have no state for {task}") from None
        if recorded != state:
            raise Decline(f"the state of {task} does not agree with the ci summary")


def worktree_clean(commit):
    """Decline unless the worktree's HEAD is COMMIT, with no tracked change and no
    untracked file that git does not ignore."""
    try:
        head = subprocess.run(["git", "rev-parse", "HEAD"], capture_output=True, text=True, timeout=60)
        status = subprocess.run(["git", "status", "--porcelain", "--untracked-files=all"],
                                capture_output=True, text=True, timeout=300)
    except (OSError, subprocess.TimeoutExpired) as e:
        raise Decline(f"cannot read the worktree's state ({e})") from None
    if head.returncode != 0 or head.stdout.strip() != commit:
        raise Decline("the worktree is not at the gate's commit")
    if status.returncode != 0 or status.stdout.strip():
        raise Decline("the worktree has changed or untracked files: "
                      + " ".join(l[3:] for l in status.stdout.splitlines()[:5]))


def eligible(out, commit, max_failures):
    """The failed tests of the gate's ci step in OUT when this script may rerun them, and
    the selected packages of the test task. Raises Decline otherwise."""
    rec = out / "ci-receipts"
    ci_complete(out)
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


def session_pids(sid):
    """The processes whose session is SID."""
    out = []
    for d in Path("/proc").iterdir():
        if not d.name.isdigit():
            continue
        try:
            fields = (d / "stat").read_text().rsplit(")", 1)[1].split()
        except (OSError, IndexError):
            continue
        # After the command name: state, ppid, pgrp, session.
        if len(fields) > 3 and fields[3] == str(sid) and fields[0] != "Z":
            out.append(int(d.name))
    return out


def stop_session(proc, grace=None):
    """Stop PROC and every process left in its session, which it leads. SIGTERM first,
    to its process group and to every other process of the session, then SIGKILL after
    GRACE seconds to whatever remains, and reap PROC."""
    grace = GRACE if grace is None else grace

    def signal_all(sig):
        for pid in session_pids(proc.pid):
            try:
                os.kill(pid, sig)
            except ProcessLookupError:
                pass

    signal_all(signal.SIGTERM)
    end = time.monotonic() + grace
    while time.monotonic() < end and session_pids(proc.pid):
        try:
            proc.wait(timeout=0.2)
        except subprocess.TimeoutExpired:
            pass
    for _ in range(50):
        if not session_pids(proc.pid):
            break
        signal_all(signal.SIGKILL)
        try:
            proc.wait(timeout=0.2)
        except subprocess.TimeoutExpired:
            pass
    proc.wait()


def run_session(cmd, seconds, env=None, stdout=None, stderr=None):
    """Run CMD in a new session for at most SECONDS. Returns the exit code, or None when
    it ran out of time, after every process of its session is stopped. A signal that ends
    this script stops the session too, and so does a command that ends with processes of
    its session still running."""
    proc = subprocess.Popen(cmd, stdout=stdout, stderr=stderr, env=env, start_new_session=True)
    try:
        code = proc.wait(timeout=max(1, seconds))
    except subprocess.TimeoutExpired:
        stop_session(proc)
        return None
    except BaseException:
        stop_session(proc)
        raise
    if session_pids(proc.pid):
        stop_session(proc)
    return code


def run(cmd, log, seconds, env=None):
    """Run CMD with its output in LOG, for at most SECONDS, through run_session."""
    with open(log, "w") as f:
        f.write("$ " + " ".join(cmd) + "\n")
        f.flush()
        code = run_session(cmd, seconds, env=env, stdout=f, stderr=subprocess.STDOUT)
        if code is None:
            f.write(f"\nstopped after {seconds} s\n")
        return code


def run_capture(cmd, seconds, env=None):
    """Run CMD through run_session and return its exit code, or None when it ran out of
    time, and its standard output. Its standard error is discarded."""
    with tempfile.TemporaryFile("w+") as out:
        code = run_session(cmd, seconds, env=env, stdout=out, stderr=subprocess.DEVNULL)
        out.seek(0)
        return code, out.read()


def rebuilt(log_text):
    """Whether a log shows a workspace crate compiled anew. Cargo prints Compiling for a
    crate that Boxington restores from its cache too, so its cache line decides when it
    is there: a miss on any of its lines is a new compilation, which a rerun of the same build never needs."""
    misses = re.findall(r"mbx\[cache\]: \d+ hits?, (\d+) miss", log_text)
    if misses:
        return any(int(n) > 0 for n in misses)
    return re.search(r"^\s*Compiling sparkles[a-z0-9-]* v", log_text, re.M) is not None


def fresh_scratch(rdir, i, target_tmp):
    """A new empty TMPDIR for rerun I, and an empty TARGET_TMP. Returns the environment."""
    tmp = rdir / f"{i}.tmp"
    shutil.rmtree(tmp, ignore_errors=True)
    tmp.mkdir()
    if target_tmp.is_dir():
        for child in target_tmp.iterdir():
            if child.is_dir() and not child.is_symlink():
                shutil.rmtree(child)
            else:
                child.unlink()
    return dict(os.environ, TMPDIR=str(tmp.resolve()))


def main(argv=None, runner=run, junit_path=Path("target/nextest/default/junit.xml"),
         target_tmp=Path("target/tmp"), check_worktree=worktree_clean, discover=run_capture):
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

    # The gate's unit stops this script with SIGTERM. Raising here lets run() stop the
    # command's session before the script ends.
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    try:
        failures, packages = eligible(out, args.commit, args.max_failures)
        check_worktree(args.commit)
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
        env = fresh_scratch(rdir, i, target_tmp)
        code = runner(prefix + ["mbx", "nextest", "run", "--workspace", "--no-capture", "--retries", "0",
                                "--no-fail-fast", "-E", expr], log, min(left, 900), env=env)
        # A test's scratch, such as a store left by a timeout, stays out of the gate's output.
        shutil.rmtree(env["TMPDIR"], ignore_errors=True)
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
    code, stdout = discover(["python3", "scripts/changed-crates.py", "--cargo-args", "--doc"],
                            min(left, 300), env=dict(os.environ, SPARKLES_CHANGED_PACKAGES=packages))
    if code is None:
        return finish(1, "not forgiven: choosing the doctest packages ran out of time")
    pkgs = stdout.split()
    if code != 0 or not pkgs:
        return finish(1, "not forgiven: cannot choose the doctest packages")
    left = int(deadline - time.monotonic())
    if left <= 0:
        return finish(1, "not forgiven: no time left for the doctests")
    if pkgs != ["none"]:
        log = rdir / "doctests.log"
        env = fresh_scratch(rdir, "doctests", target_tmp)
        code = runner(prefix + ["mbx", "test", *pkgs, "--doc"], log, left, env=env)
        shutil.rmtree(env["TMPDIR"], ignore_errors=True)
        lines.append(f"{'PASS' if code == 0 else 'FAIL'} doctests {' '.join(pkgs)} (exit {code}, log {log.name})")
        if code != 0:
            return finish(1, "not forgiven: the doctests failed")
    try:
        check_worktree(args.commit)
    except Decline as e:
        return finish(1, f"not forgiven: after the reruns, {e}")
    (out / "ci.forgiven").write_text("".join(f"{b} {n}\n" for b, n in failures))
    return finish(0, f"forgiven: {len(failures)} flaky test{'s' if len(failures) != 1 else ''} passed alone")


if __name__ == "__main__":
    sys.exit(main())
