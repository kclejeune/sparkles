#!/usr/bin/env python3
"""Run a unittest directory serially and with scripts/unittest-parallel.py, and fail when they disagree.

    python3 scripts/unittest-compare.py [RUNNER OPTION]... DIR

The serial arm is `python3 -m unittest discover -s DIR`. The parallel arm is
scripts/unittest-parallel.py with the given options and DIR, and it starts after the serial
arm ends, so the two never compete for the host. Each arm gives an exit status and the
number of tests it ran and skipped. The serial arm's counts come from unittest's closing
summary, and the parallel arm's from the result records of all its units, which the runner
keeps for this with --keep. The run exits 1 and names each difference when the arms
disagree, and otherwise exits as both arms did. SIGINT, SIGTERM or SIGHUP sends SIGTERM to
the running arm's process group and ends the run with 128 plus the signal number once the
arm has ended.

The nightly gate runs this on scripts/bench-tests. The parallel runner reaches the serial
verdict only under assumptions that it does not check, which its own description lists,
and this comparison is the backstop for them.
"""

import glob
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile

RUNNER = os.path.join(os.path.dirname(os.path.abspath(__file__)), "unittest-parallel.py")

_RAN = re.compile(r"^Ran (\d+) tests? in ", re.M)
_STATUS = re.compile(r"^(OK|FAILED|NO TESTS RAN)(?: \((.*)\))?$", re.M)


def serial_counts(text):
    """The tests run and skipped that unittest's closing summary in TEXT reports, or None when
    TEXT has no summary. The last summary is unittest's own, after anything a test printed."""
    ran = _RAN.findall(text)
    status = _STATUS.findall(text)
    if not ran or not status:
        return None
    details = dict(part.split("=", 1) for part in status[-1][1].split(", ") if "=" in part)
    return int(ran[-1]), int(details.get("skipped", 0))


def parallel_counts(keep):
    """The tests run and skipped in the result records under KEEP, or None when it has none."""
    records = glob.glob(os.path.join(keep, "*", "record-*.json"))
    if not records:
        return None
    ran = skipped = 0
    for path in records:
        with open(path) as f:
            record = json.load(f)
        ran += record.get("testsRun", 0)
        skipped += record.get("skipped", 0)
    return ran, skipped


def differences(serial, parallel):
    """How the arms' (exit status, counts) differ, as sentences, or an empty list."""
    (s_exit, s_counts), (p_exit, p_counts) = serial, parallel
    found = []
    if s_exit != p_exit:
        found.append(f"serial discovery exited {s_exit} and the parallel runner {p_exit}")
    if s_counts is None:
        found.append("serial discovery printed no summary")
    if p_counts is None:
        found.append("the parallel runner left no result record")
    if s_counts and p_counts:
        for name, s, p in zip(("ran", "skipped"), s_counts, p_counts):
            if s != p:
                found.append(f"serial discovery {name} {s} tests and the parallel runner {p}")
    return found


_CURRENT = []
_STOP = []


def _on_signal(signum, frame):
    """Record SIGNUM and pass SIGTERM to the running arm's process group. The serial arm ends
    at once, and the parallel runner stops its own units first."""
    _STOP.append(signum)
    for proc in _CURRENT:
        try:
            os.killpg(proc.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass


def run_arm(name, argv):
    """Run one arm in its own process group, print its output whole, and return its exit
    status and output."""
    print(f"unittest-compare: {name}: {' '.join(argv)}", flush=True)
    with tempfile.TemporaryFile("w+") as out:
        proc = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=out, stderr=subprocess.STDOUT,
                                start_new_session=True)
        _CURRENT.append(proc)
        code = proc.wait()
        _CURRENT.remove(proc)
        if _STOP:
            print(f"unittest-compare: {name}: stopped by signal {_STOP[0]}", flush=True)
            raise SystemExit(128 + _STOP[0])
        out.seek(0)
        text = out.read()
    sys.stdout.write(text)
    print(f"unittest-compare: {name}: exit {code}", flush=True)
    return code, text


def main(argv=None):
    argv = sys.argv[1:] if argv is None else argv
    if not argv or argv[-1].startswith("-") or any(a == "--keep" or a.startswith("--keep=") for a in argv):
        raise SystemExit("usage: unittest-compare.py [RUNNER OPTION]... DIR (without --keep)")
    for s in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        signal.signal(s, _on_signal)
    directory = argv[-1]
    warn = [f"-W{w}" for w in sys.warnoptions]
    s_exit, s_text = run_arm("serial", [sys.executable, *warn, "-m", "unittest", "discover", "-s", directory])
    keep = tempfile.mkdtemp(prefix="unittest-compare-")
    try:
        p_exit, _ = run_arm("parallel", [sys.executable, *warn, RUNNER, "--keep", keep, *argv])
        p_counts = parallel_counts(keep)
    finally:
        shutil.rmtree(keep, ignore_errors=True)
    s_counts = serial_counts(s_text)
    found = differences((s_exit, s_counts), (p_exit, p_counts))
    if found:
        for line in found:
            print(f"unittest-compare: DIFFERENT: {line}")
        return 1
    ran, skipped = s_counts
    print(f"unittest-compare: both arms exited {s_exit} with {ran} tests run and {skipped} skipped")
    return s_exit


if __name__ == "__main__":
    sys.exit(main())
