#!/usr/bin/env python3
"""Run a unittest directory serially and with scripts/unittest-parallel.py, and fail when they disagree.

    python3 scripts/unittest-compare.py [RUNNER OPTION]... DIR

The serial arm is `python3 -m unittest discover -s DIR`. The parallel arm is
scripts/unittest-parallel.py with the given options and DIR, and it starts after the serial
arm ends, so the two never compete for the host. Each arm gives an exit status and the
number of tests it ran and skipped. The serial arm's counts come from unittest's closing
summary on its standard error, which a test's own printed output cannot mask, and the
parallel arm's from the result records of all its units, which the runner keeps for this
with --keep. A runner that ran no unit, because the suite has no test, counts none, and a
runner that replaced itself with serial discovery, on a host it cannot supervise from, is
counted from that discovery's summary. The run exits 1 and names each difference when the arms
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
    """The tests run and skipped that unittest's closing summary in TEXT, an arm's standard
    error, reports, or None when TEXT has no summary. The last summary is unittest's own,
    after anything a test wrote there."""
    ran = _RAN.findall(text)
    status = _STATUS.findall(text)
    if not ran or not status:
        return None
    details = dict(part.split("=", 1) for part in status[-1][1].split(", ") if "=" in part)
    return int(ran[-1]), int(details.get("skipped", 0))


_NO_UNITS = re.compile(r"^unittest-parallel: 0 tests in 0 processes, ", re.M)
_FELL_BACK = re.compile(r"^unittest-parallel: .*, so the suite runs serially with unittest discover$", re.M)


def parallel_counts(keep, out, err):
    """The tests run and skipped in the result records under KEEP, or None when there are
    none and the runner's standard output OUT does not say why. A runner that started no
    unit counts none, and one that ran the suite serially instead is counted from the
    summary in its standard error ERR."""
    records = glob.glob(os.path.join(keep, "*", "record-*.json"))
    if not records:
        if _FELL_BACK.search(out):
            return serial_counts(err)
        return (0, 0) if _NO_UNITS.search(out) else None
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


def _terminate():
    for proc in _CURRENT:
        try:
            os.killpg(proc.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass


def _on_signal(signum, frame):
    """Record SIGNUM and pass SIGTERM to the running arm's process group. The serial arm ends
    at once, and the parallel runner stops its own units first."""
    _STOP.append(signum)
    _terminate()


def _stopped(name):
    if _STOP:
        print(f"unittest-compare: {name}: stopped by signal {_STOP[0]}", flush=True)
        raise SystemExit(128 + _STOP[0])


def run_arm(name, argv):
    """Run one arm in its own process group, print its standard output and then its standard
    error, and return its exit status and both texts."""
    _stopped(name)
    print(f"unittest-compare: {name}: {' '.join(argv)}", flush=True)
    with tempfile.TemporaryFile("w+") as out, tempfile.TemporaryFile("w+") as err:
        proc = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=out, stderr=err, start_new_session=True)
        _CURRENT.append(proc)
        # A signal that came before the arm was registered has not reached it.
        if _STOP:
            _terminate()
        code = proc.wait()
        _CURRENT.remove(proc)
        _stopped(name)
        out.seek(0)
        err.seek(0)
        texts = out.read(), err.read()
    sys.stdout.write(texts[0])
    sys.stdout.write(texts[1])
    print(f"unittest-compare: {name}: exit {code}", flush=True)
    return code, *texts


def main(argv=None):
    argv = sys.argv[1:] if argv is None else argv
    if not argv or argv[-1].startswith("-") or any(a == "--keep" or a.startswith("--keep=") for a in argv):
        raise SystemExit("usage: unittest-compare.py [RUNNER OPTION]... DIR (without --keep)")
    for s in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        signal.signal(s, _on_signal)
    directory = argv[-1]
    warn = [f"-W{w}" for w in sys.warnoptions]
    s_exit, _, s_err = run_arm("serial", [sys.executable, *warn, "-m", "unittest", "discover", "-s", directory])
    keep = tempfile.mkdtemp(prefix="unittest-compare-")
    try:
        p_exit, p_out, p_err = run_arm("parallel", [sys.executable, *warn, RUNNER, "--keep", keep, *argv])
        p_counts = parallel_counts(keep, p_out, p_err)
    finally:
        shutil.rmtree(keep, ignore_errors=True)
    s_counts = serial_counts(s_err)
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
