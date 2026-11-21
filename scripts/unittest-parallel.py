#!/usr/bin/env python3
"""Run a directory of unittest modules with one process per module, several at a time.

    python3 scripts/unittest-parallel.py [--jobs N] [--split MODULE]... DIR

The runner loads the suite the way `python3 -m unittest discover -s DIR` does and runs each
of its modules as `python3 -m unittest MODULE` with DIR first on the module path, from the
caller's working directory. It fails when the processes together run another number of
tests than discover finds. A module named with --split runs one process per test instead, for modules
whose few tests take most of the suite's time. The units start longest first, using the
split tests and then the modules by file size as the estimate, and each unit's output is
printed whole when it ends. The exit status is 1 when any unit failed or found no tests,
and 0 otherwise. --jobs 1 runs the units one at a time, in the same processes.
"""

import argparse
import os
import re
import subprocess
import sys
import time
import unittest
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

RAN = re.compile(r"^Ran (\d+) tests? in ", re.M)


def discovered(directory):
    """The test ids that `discover -s DIR` finds, in its order. A module that fails to import
    appears as discover's stand-in test under the module's own name."""
    sys.path.insert(0, str(directory))
    try:
        suite = unittest.defaultTestLoader.discover(str(directory))
    finally:
        sys.path.remove(str(directory))
    ids = []

    def walk(s):
        for t in s:
            if isinstance(t, unittest.TestSuite):
                walk(t)
            else:
                ids.append(t.id())

    walk(suite)
    return ids


def module_of(test_id):
    # A loader stand-in is unittest.loader._FailedTest.MODULE; a test is MODULE.CLASS.METHOD.
    if test_id.startswith("unittest.loader._FailedTest."):
        return test_id[len("unittest.loader._FailedTest."):]
    return test_id.rsplit(".", 2)[0]


def units(directory, split):
    """The processes to run, longest first, and the number of tests discover finds."""
    ids = discovered(directory)
    mods = list(dict.fromkeys(module_of(t) for t in ids))
    missing = [m for m in split if m not in mods]
    if missing:
        raise SystemExit(f"unittest-parallel: no module {', '.join(missing)} in {directory}")
    code = "import sys, unittest; sys.path.insert(0, sys.argv[1]); unittest.main(module=None, argv=['unittest', *sys.argv[2:]])"

    def unit(name):
        return (name, [sys.executable, "-c", code, str(directory), name])

    out = [unit(t) for t in ids if module_of(t) in split]

    def size(m):
        f = getattr(sys.modules.get(m), "__file__", None)
        return os.path.getsize(f) if f and os.path.exists(f) else 0

    out += [unit(m) for m in sorted((m for m in mods if m not in split), key=lambda m: -size(m))]
    return out, len(ids)


def run(unit):
    name, argv = unit
    t0 = time.monotonic()
    p = subprocess.run(argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, errors="replace")
    return name, p.returncode, p.stdout, time.monotonic() - t0


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--jobs", type=int, default=min(8, os.cpu_count() or 1))
    ap.add_argument("--split", action="append", default=[])
    ap.add_argument("directory", type=Path)
    a = ap.parse_args(argv)
    if a.jobs < 1:
        ap.error("--jobs must be at least 1")
    work, expected = units(a.directory, a.split)
    t0 = time.monotonic()
    failed, total = [], 0
    with ThreadPoolExecutor(a.jobs) as pool:
        for name, rc, text, secs in pool.map(run, work):
            n = sum(int(x) for x in RAN.findall(text))
            total += n
            print(f"=== {name}: exit {rc}, {n} tests, {secs:.1f} s", flush=True)
            sys.stdout.write(text)
            if rc != 0 or n == 0:
                failed.append(name)
    print(f"unittest-parallel: {total} tests in {len(work)} processes, {a.jobs} at a time, {time.monotonic() - t0:.1f} s")
    if total != expected:
        print(f"unittest-parallel: discover finds {expected} tests, and the processes ran {total}")
        return 1
    if failed:
        print(f"unittest-parallel: failed: {' '.join(failed)}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
