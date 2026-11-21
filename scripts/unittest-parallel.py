#!/usr/bin/env python3
"""Run a directory of unittest modules in several processes at once, with serial discovery's verdict.

    python3 scripts/unittest-parallel.py [--jobs N] [--split MODULE]... [--unit-timeout S]
        [--discover-timeout S] [--grace S] DIR

The runner first discovers the suite in a child process, the way `python3 -m unittest
discover -s DIR` does from the caller's working directory, and records the discovered tree:
one top-level entry for each module or package that discovery loads, and the id of every
test occurrence in order. The runner's own process imports no test module. Each unit then
runs in its own process, which discovers the whole suite again, checks that its tree matches
the recorded one, and runs one top-level entry exactly as discovered. So every module is
imported in every unit, as in serial discovery, and load_tests hooks, import errors and
import-time skips, test classes imported into other modules, custom suites and class and
module fixtures behave as they do serially.

A module named with --split runs one process per test instead, for modules whose few tests
take most of the suite's time. The split is taken only when the module's entry is plain,
which means default suites holding only the module's own test classes, with no class or
module fixtures, no load_tests suite of its own kind and no repeated test. Otherwise the
module runs whole and the runner says why. Units start longest first, using the split tests
and then the modules by file size as the estimate, and each unit's output is printed whole
when it ends.

Each unit writes a result record beside its output: the occurrences it ran, unittest's
counts and verdict, and a completion mark. A unit fails when its exit status is not 0, its
record is missing, incomplete or for other occurrences, or unittest's result was not
successful. A test's own output never counts. The run exits 1 when any unit failed or
discovery failed, 5 when no test ran and none was skipped, as unittest does, and 0
otherwise. Tests run with warnings set to 'default' unless the interpreter was given -W.

A test module can share evidence across units. When a module defines parallel_export(),
each unit calls it after its tests and teardown and puts the result in its record, and a
missing export fails the unit. A module that defines parallel_absorb(exports) runs after
every other unit has ended, and its unit calls parallel_absorb with the list of
(unit, export) pairs before its tests start. The suite's process-leak check uses this, so
its one test runs once, last, on the evidence of every unit.

Discovery and each unit have deadlines, 300 s and 900 s by default. Each runs in its own
session with its output in a file. When a deadline passes, or the runner gets SIGINT or
SIGTERM, it stops admitting units, sends SIGTERM to the unit's process group and to every
descendant it finds under /proc, follows with SIGKILL after the grace period, 5 s by
default, and reports the unit. A process that left the unit's tree before that, by being
orphaned, is not found. An interrupted run exits 128 plus the signal number.
--jobs 1 runs the units one at a time, in the same processes.
"""

import argparse
import json
import os
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

RUNNER = os.path.abspath(__file__)


# ----- discovery, in the discovery child and in every unit ---------------------------------

class _TaggingLoader(unittest.TestLoader):
    """A loader that remembers which module produced each suite, to name the units."""

    def __init__(self):
        super().__init__()
        self.labels = {}

    def loadTestsFromModule(self, module, *args, **kwargs):
        suite = super().loadTestsFromModule(module, *args, **kwargs)
        self.labels.setdefault(id(suite), module.__name__)
        return suite


def _tree(node):
    if isinstance(node, unittest.TestSuite):
        cls = type(node)
        return {"suite": f"{cls.__module__}.{cls.__qualname__}", "tests": [_tree(t) for t in node]}
    return node.id()


def _leaves(node):
    if isinstance(node, unittest.TestSuite):
        for t in node:
            yield from _leaves(t)
    else:
        yield node


def _label(loader, entry, index):
    name = loader.labels.get(id(entry))
    if name:
        return name
    for t in _leaves(entry):
        # Import failures and import-time skips stand in as unittest.loader._FailedTest.MODULE
        # and unittest.loader.ModuleSkipped.MODULE.
        if type(t).__module__ == "unittest.loader":
            return t.id().rsplit(".", 1)[-1]
        return t.id().rsplit(".", 2)[0]
    return f"entry-{index}"


def _overridden(cls, name):
    return getattr(cls, name).__func__ is not getattr(unittest.TestCase, name).__func__


def _plain(label, entry):
    """Why ENTRY cannot be split into one process per test, or None when it can."""
    module = sys.modules.get(label)
    if module is None:
        return "it is not a module of its own"
    for hook in ("setUpModule", "tearDownModule", "parallel_absorb"):
        if hasattr(module, hook):
            return f"it defines {hook}"
    if unittest.case._module_cleanups:
        return "a module cleanup is registered"
    seen = set()

    def walk(node):
        if isinstance(node, unittest.TestSuite):
            if type(node) is not unittest.TestSuite:
                return f"it has a suite of kind {type(node).__qualname__}"
            for t in node:
                why = walk(t)
                if why:
                    return why
            return None
        cls = type(node)
        if cls.__module__ != label:
            return f"it runs {cls.__module__}.{cls.__qualname__} from another module"
        for name in ("setUpClass", "tearDownClass"):
            if _overridden(cls, name):
                return f"{cls.__qualname__} has {name}"
        if getattr(cls, "_class_cleanups", None):
            return f"{cls.__qualname__} has a class cleanup"
        if node.id() in seen:
            return f"it runs {node.id()} more than once"
        seen.add(node.id())
        return None

    return walk(entry)


def discover(directory):
    """Discover DIRECTORY as `python3 -m unittest discover -s DIRECTORY` does, and return the
    loader, the top-level entries and their description."""
    # `python3 -m unittest` puts the working directory first on the path, where a script puts
    # its own directory.
    sys.path[0] = os.getcwd()
    before = set(sys.modules)
    loader = _TaggingLoader()
    suite = loader.discover(str(directory))
    entries = list(suite)
    units = []
    for i, entry in enumerate(entries):
        label = _label(loader, entry, i)
        module = sys.modules.get(label)
        f = getattr(module, "__file__", None)
        units.append({
            "label": label,
            "tree": _tree(entry),
            "leaves": [t.id() for t in _leaves(entry)],
            "split": _plain(label, entry),
            "size": os.path.getsize(f) if f and os.path.exists(f) else 0,
            "absorb": callable(getattr(module, "parallel_absorb", None)),
        })
    exports = sorted(m for m in set(sys.modules) - before
                     if callable(getattr(sys.modules[m], "parallel_export", None)))
    return entries, {"units": units, "exports": exports}


def _write_json(path, value):
    tmp = f"{path}.tmp"
    with open(tmp, "w") as f:
        json.dump(value, f)
    os.replace(tmp, path)


def discover_main(directory, out):
    _, description = discover(directory)
    _write_json(out, description)
    return 0


def worker_main(directory, plan_path, index, split, record_path, absorb_path):
    with open(plan_path) as f:
        plan = json.load(f)
    entries, description = discover(directory)
    record = {"complete": True, "index": index, "split": split}
    if description != plan:
        record.update(error="the suite discovered here differs from the runner's discovery", ran=[])
        _write_json(record_path, record)
        return 1
    entry = entries[index]
    if split >= 0:
        entry = unittest.TestSuite([list(_leaves(entry))[split]])
    if absorb_path:
        with open(absorb_path) as f:
            exports = json.load(f)
        label = plan["units"][index]["label"]
        sys.modules[label].parallel_absorb(exports.get(label, []))
    # A suite drops each test once it has run, so the occurrences are listed first.
    ran = [t.id() for t in _leaves(entry)]
    warnings = None if sys.warnoptions else "default"
    result = unittest.TextTestRunner(stream=sys.stderr, warnings=warnings).run(entry)
    record.update(
        ran=ran,
        testsRun=result.testsRun,
        failures=len(result.failures),
        errors=len(result.errors),
        skipped=len(result.skipped),
        expectedFailures=len(result.expectedFailures),
        unexpectedSuccesses=len(result.unexpectedSuccesses),
        successful=result.wasSuccessful(),
        exports={m: sys.modules[m].parallel_export() for m in plan["exports"]},
    )
    sys.stderr.flush()
    _write_json(record_path, record)
    return 0 if result.wasSuccessful() else 1


# ----- supervision, in the runner ----------------------------------------------------------

class Interrupted(Exception):
    def __init__(self, signum):
        super().__init__(signum)
        self.signum = signum


def _on_signal(signum, frame):
    raise Interrupted(signum)


def _process_table():
    """Each live process's (parent, group, session), read from /proc."""
    table = {}
    for d in os.listdir("/proc"):
        if not d.isdigit():
            continue
        try:
            with open(f"/proc/{d}/stat") as f:
                fields = f.read().rsplit(")", 1)[1].split()
        except OSError:
            continue
        if fields[0] != "Z":
            table[int(d)] = (int(fields[1]), int(fields[2]), int(fields[3]))
    return table


def _owned(root):
    """ROOT, its descendants, and the members of its group and session and of theirs."""
    table = _process_table()
    owned = {root} if root in table else set()
    changed = True
    while changed:
        changed = False
        for pid, (ppid, pgid, sid) in table.items():
            if pid not in owned and (ppid in owned or pgid in owned or sid in owned):
                owned.add(pid)
                changed = True
    owned.discard(os.getpid())
    return owned


def _signal_all(pids, sig):
    for pid in pids:
        try:
            os.kill(pid, sig)
        except (ProcessLookupError, PermissionError):
            pass


class Job:
    """One child process of the runner, in its own session, with its output in a file."""

    count = 0

    def __init__(self, name, argv, deadline, workdir):
        self.name = name
        Job.count += 1
        self.out = open(os.path.join(workdir, f"{Job.count}.out"), "w+")
        self.t0 = time.monotonic()
        self.end = self.t0 + deadline
        self.deadline = deadline
        self.timed_out = False
        self.proc = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=self.out,
                                     stderr=subprocess.STDOUT, start_new_session=True)

    def stop(self, grace):
        """Stop the process and everything it owns: SIGTERM, then SIGKILL after GRACE."""
        root = self.proc.pid
        seen = _owned(root)
        _signal_all(seen, signal.SIGTERM)
        end = time.monotonic() + grace
        while time.monotonic() < end:
            seen |= _owned(root)
            if not (_process_table().keys() & seen):
                break
            time.sleep(0.05)
        seen |= _owned(root)
        _signal_all(seen, signal.SIGKILL)
        try:
            self.proc.wait(timeout=grace + 5)
        except subprocess.TimeoutExpired:
            pass

    def output(self):
        self.out.seek(0)
        text = self.out.read()
        self.out.close()
        return text

    def seconds(self):
        return time.monotonic() - self.t0


def run_discovery(a, workdir):
    path = os.path.join(workdir, "discovery.json")
    job = Job("discovery", [sys.executable, RUNNER, "--_discover", path, str(a.directory)],
              a.discover_timeout, workdir)
    try:
        while job.proc.poll() is None:
            if time.monotonic() >= job.end:
                job.stop(a.grace)
                print(f"unittest-parallel: discovery: deadline of {a.discover_timeout:g} s passed")
                sys.stdout.write(job.output())
                return None
            time.sleep(0.05)
    except Interrupted:
        job.stop(a.grace)
        raise
    text = job.output()
    if job.proc.returncode != 0 or not os.path.exists(path):
        print(f"unittest-parallel: discovery failed with exit {job.proc.returncode}")
        sys.stdout.write(text)
        return None
    if text:
        sys.stdout.write(text)
    with open(path) as f:
        return json.load(f)


def plan_units(description, split):
    """The units to run: (name, entry index, leaf index or -1), longest first, with the
    absorbing modules' units last."""
    units = description["units"]
    labels = [u["label"] for u in units]
    missing = [m for m in split if m not in labels]
    if missing:
        raise SystemExit(f"unittest-parallel: no module {', '.join(missing)} in the suite")
    first, last = [], []
    for i, u in enumerate(units):
        if not u["leaves"]:
            continue
        if u["label"] in split and u["split"] is None:
            first += [(leaf, i, j, 1 << 40) for j, leaf in enumerate(u["leaves"])]
            continue
        if u["label"] in split:
            print(f"unittest-parallel: --split {u['label']} runs whole, since {u['split']}")
        (last if u["absorb"] else first).append((u["label"], i, -1, u["size"]))
    first.sort(key=lambda w: -w[3])
    return [w[:3] for w in first], [w[:3] for w in last]


def check(record, assigned, exports):
    """Why a unit's RECORD fails, or None when it passed."""
    if record is None:
        return "it ended with no result record"
    if not record.get("complete"):
        return "its result record is incomplete"
    if record.get("error"):
        return record["error"]
    if record.get("ran") != assigned:
        return "its result record is for other tests than it was assigned"
    missing = [m for m in exports if m not in record.get("exports", {})]
    if missing:
        return f"its result record lacks the export of {', '.join(missing)}"
    return None  # a failed test is in unittest's own report


def main(argv=None):
    if argv is None:
        argv = sys.argv[1:]
    if argv[:1] == ["--_discover"]:
        return discover_main(argv[2], argv[1])
    if argv[:1] == ["--_unit"]:
        plan, index, split, record, absorb, directory = argv[1:]
        return worker_main(directory, plan, int(index), int(split), record, absorb)
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--jobs", type=int, default=min(8, os.cpu_count() or 1))
    ap.add_argument("--split", action="append", default=[])
    ap.add_argument("--unit-timeout", type=float, default=900.0)
    ap.add_argument("--discover-timeout", type=float, default=300.0)
    ap.add_argument("--grace", type=float, default=5.0)
    ap.add_argument("directory", type=Path)
    a = ap.parse_args(argv)
    if a.jobs < 1:
        ap.error("--jobs must be at least 1")
    for opt in ("unit_timeout", "discover_timeout", "grace"):
        if not getattr(a, opt) > 0:
            ap.error(f"--{opt.replace('_', '-')} must be more than 0")
    old = {s: signal.signal(s, _on_signal) for s in (signal.SIGINT, signal.SIGTERM)}
    workdir = tempfile.mkdtemp(prefix="unittest-parallel-")
    active = []
    try:
        return supervise(a, workdir, active)
    except Interrupted as e:
        for s in old:
            signal.signal(s, signal.SIG_IGN)
        names = " ".join(job.name for job in active)
        for job in active:
            job.stop(a.grace)
        print(f"unittest-parallel: interrupted by signal {e.signum}, stopped: {names or 'nothing'}", flush=True)
        return 128 + e.signum
    finally:
        for s, h in old.items():
            signal.signal(s, h)
        subprocess.run(["rm", "-rf", workdir], check=False)


def supervise(a, workdir, active):
    t0 = time.monotonic()
    description = run_discovery(a, workdir)
    if description is None:
        return 1
    first, last = plan_units(description, a.split)
    plan_path = os.path.join(workdir, "plan.json")
    _write_json(plan_path, description)
    units = description["units"]
    exports = description["exports"]
    absorbed = {}  # module -> [(unit, export)]
    failed = []
    totals = {"testsRun": 0, "skipped": 0}
    count = 0

    def start(work, absorb):
        nonlocal count
        name, index, split = work
        record = os.path.join(workdir, f"record-{count}.json")
        count += 1
        job = Job(name, [sys.executable, RUNNER, "--_unit", plan_path, str(index), str(split),
                         record, absorb, str(a.directory)], a.unit_timeout, workdir)
        job.record, job.index, job.split = record, index, split
        active.append(job)

    def finish(job):
        text = job.output()
        record = None
        if os.path.exists(job.record):
            try:
                with open(job.record) as f:
                    record = json.load(f)
            except ValueError:
                record = {"complete": False}
        leaves = units[job.index]["leaves"]
        assigned = leaves if job.split < 0 else [leaves[job.split]]
        if job.timed_out:
            why = f"deadline of {a.unit_timeout:g} s passed"
        else:
            why = check(record, assigned, exports)
        n = record.get("testsRun", 0) if record else 0
        print(f"=== {job.name}: exit {job.proc.returncode}, {n} tests, {job.seconds():.1f} s", flush=True)
        sys.stdout.write(text)
        if not why and job.proc.returncode != 0 and record.get("successful"):
            why = f"its tests passed but it exited {job.proc.returncode}"
        if why:
            print(f"unittest-parallel: {job.name}: {why}")
        if why or job.proc.returncode != 0 or not record.get("successful"):
            failed.append(job.name)
        if record and not why:
            totals["testsRun"] += record["testsRun"]
            totals["skipped"] += record["skipped"]
            for m, value in record.get("exports", {}).items():
                absorbed.setdefault(m, []).append((job.name, value))

    def drain(queue, absorb_for):
        while queue or active:
            while queue and len(active) < a.jobs:
                start(queue.pop(0), absorb_for())
            time.sleep(0.05)
            for job in list(active):
                if job.proc.poll() is None and time.monotonic() >= job.end:
                    job.timed_out = True
                    job.stop(a.grace)
                if job.proc.poll() is not None:
                    active.remove(job)
                    finish(job)

    drain(list(first), lambda: "")
    absorb_path = ""
    if last:
        absorb_path = os.path.join(workdir, "absorb.json")
        _write_json(absorb_path, absorbed)
    drain(list(last), lambda: absorb_path)
    work = len(first) + len(last)
    print(f"unittest-parallel: {totals['testsRun']} tests in {work} processes, {a.jobs} at a time, "
          f"{time.monotonic() - t0:.1f} s")
    if failed:
        print(f"unittest-parallel: failed: {' '.join(failed)}")
        return 1
    if totals["testsRun"] == 0 and totals["skipped"] == 0:
        print("unittest-parallel: NO TESTS RAN")
        return 5
    return 0


if __name__ == "__main__":
    sys.exit(main())
