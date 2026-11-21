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
import-time skips, custom suites and class and module fixtures behave as they do serially.
An entry made only of default suites with no test does nothing and gets no process. The
whole suite runs in one process instead, and the runner says why, when test classes of one
module run in more than one entry, as when one test module imports another's class, since
they can share that module's state, or when any entry has a suite of a kind of its own,
since its run() can depend on everything run before it. State that test modules share in
some other way, such as through a helper module's globals, is not detected, so a test that
depends on another module's tests having run first can pass here and fail serially.

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

Discovery and each unit have deadlines, 300 s and 900 s by default, and every duration
must be finite. Each runs in its own session with its output in a file. When a deadline
passes, or the runner gets SIGINT, SIGTERM or SIGHUP, it starts no more units, sends SIGTERM
to the process group and session of every active unit and to every descendant it finds
under /proc, follows with SIGKILL after one grace period, 5 s by default, and reports the
units, including those that never started. A process is signalled only
while its start time still matches, so a reused pid is left alone. A process that left the
unit's tree before that, by being orphaned, is not found. An interrupted run exits 128 plus
the signal number. Discovery and each unit get SIGTERM from the kernel when the runner dies
without stopping them, as under SIGKILL, and then stop their own descendants the same way
and exit.
--keep DIR keeps the discovery, the plan, each unit's output and the result records in a
new directory under DIR. --jobs 1 runs the units one at a time, in the same processes.
"""

import argparse
import ctypes
import json
import math
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
        # The suite is kept with its name, so that its id is not reused by another.
        self.labels.setdefault(id(suite), (suite, module.__name__))
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
    if id(entry) in loader.labels:
        return loader.labels[id(entry)][1]
    for t in _leaves(entry):
        # Import failures and import-time skips stand in as unittest.loader._FailedTest.MODULE
        # and unittest.loader.ModuleSkipped.MODULE.
        if type(t).__module__ == "unittest.loader":
            return t.id().rsplit(".", 1)[-1]
        return t.id().rsplit(".", 2)[0]
    return f"entry-{index}"


def _overridden(cls, name):
    for k in cls.__mro__:
        if k is unittest.TestCase:
            return False
        if name in vars(k):
            return True
    return False


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
    for u, entry in zip(units, entries):
        u["inert"] = _inert(entry)
    exports = sorted(m for m in set(sys.modules) - before
                     if callable(getattr(sys.modules[m], "parallel_export", None)))
    return entries, {"units": units, "exports": exports, "serial": _shared(entries, units)}


def _inert(node):
    """Whether NODE is default suites only, with no test, so running it does nothing."""
    return type(node) is unittest.TestSuite and all(_inert(t) for t in node)


def _shared(entries, units):
    """Why the entries cannot run in separate processes, or None. Test classes of one module
    that run in more than one entry, as when one test module imports another's class, can
    share that module's state and fixtures, which only one process keeps. A suite of a kind
    of its own can depend on the result of everything run before it."""
    where = {}
    for i, entry in enumerate(entries):
        for t in _leaves(entry):
            module = type(t).__module__
            if module != "unittest.loader":
                where.setdefault(module, set()).add(i)

    def custom(node):
        if isinstance(node, unittest.TestSuite):
            if type(node) is not unittest.TestSuite:
                return type(node)
            for t in node:
                kind = custom(t)
                if kind:
                    return kind
        return None

    for module, found in where.items():
        if len(found) > 1:
            return f"test classes of {module} run in {len(found)} top-level entries"
    for u, entry in zip(units, entries):
        kind = custom(entry)
        if kind:
            return f"{u['label']} has a suite of kind {kind.__module__}.{kind.__qualname__}"
    return None


def _write_json(path, value):
    tmp = f"{path}.tmp"
    with open(tmp, "w") as f:
        json.dump(value, f)
    os.replace(tmp, path)


def discover_main(directory, out):
    # The caller has set up _die_with_parent before any test module is imported.
    _, description = discover(directory)
    _write_json(out, description)
    return 0


def _die_with_parent(parent, grace):
    """End this unit, and what it started, when the runner ends without stopping it, as when
    it is killed with SIGKILL. The kernel sends SIGTERM when the runner dies, and the handler
    stops this unit's descendants as the runner would have."""
    me = {os.getpid(): _start_of(os.getpid())}

    def on_term(signum, frame):
        found = _owned(me)
        found.pop(os.getpid(), None)
        stop_all(found, grace)
        os._exit(128 + signum)

    signal.signal(signal.SIGTERM, on_term)
    ctypes.CDLL(None, use_errno=True).prctl(1, signal.SIGTERM, 0, 0, 0)  # PR_SET_PDEATHSIG
    if os.getppid() != parent:
        on_term(signal.SIGTERM, None)


def worker_main(directory, plan_path, index, split, record_path, absorb_path):
    with open(plan_path) as f:
        plan = json.load(f)
    entries, description = discover(directory)
    record = {"complete": True, "index": index, "split": split}
    if description != plan:
        record.update(error="the suite discovered here differs from the runner's discovery", ran=[])
        _write_json(record_path, record)
        return 1
    entry = unittest.TestSuite(entries) if index < 0 else entries[index]
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


# The signals received. The handler only records them, and the supervising loops, which poll
# every 50 ms, raise Interrupted where every started process is already tracked.
_STOP = []


def _on_signal(signum, frame):
    _STOP.append(signum)


def _check_stop():
    if _STOP:
        raise Interrupted(_STOP[0])


def _process_table():
    """Each live process's (parent, group, session, start time), read from /proc."""
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
            table[int(d)] = (int(fields[1]), int(fields[2]), int(fields[3]), int(fields[19]))
    return table


def _start_of(pid):
    """PID's start time, which with the pid names one process, or None when it has ended."""
    return _process_table().get(pid, (0, 0, 0, None))[3]


def _owned(roots, table=None):
    """The processes that ROOTS, a {pid: start time} map, own, as {pid: start time}: each root
    still running as the process recorded, or ended, with its descendants and the members
    of its group and session and of its descendants'. A root whose pid now names another
    process is left out, so a reused pid is not taken for the one recorded."""
    table = _process_table() if table is None else table
    owned = {}
    for pid, start in roots.items():
        now = table.get(pid)
        if now is None or now[3] == start:
            owned[pid] = start
    changed = True
    while changed:
        changed = False
        for pid, (ppid, pgid, sid, start) in table.items():
            if pid not in owned and (ppid in owned or pgid in owned or sid in owned):
                owned[pid] = start
                changed = True
    owned.pop(os.getpid(), None)
    return owned


def _signal_all(found, sig):
    """Send SIG to each process of FOUND that is still the one found, by its start time."""
    table = _process_table()
    for pid, start in found.items():
        if start is None or table.get(pid, (0, 0, 0, None))[3] != start:
            continue
        try:
            os.kill(pid, sig)
        except (ProcessLookupError, PermissionError):
            pass


def stop_all(roots, grace):
    """Stop ROOTS, a {pid: start time} map, and everything they own: SIGTERM to all, then
    SIGKILL to what is left after one shared GRACE period. A process keeps the identity it
    was first found with."""
    seen = _owned(roots)

    def look(table=None):
        for pid, start in _owned(roots, table).items():
            seen.setdefault(pid, start)

    _signal_all(seen, signal.SIGTERM)
    end = time.monotonic() + grace
    while time.monotonic() < end:
        table = _process_table()
        look(table)
        if not any(table.get(pid, (0, 0, 0, None))[3] == start for pid, start in seen.items()):
            return
        time.sleep(0.05)
    look()
    _signal_all(seen, signal.SIGKILL)


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
        self.start = _start_of(self.proc.pid)
        self.halted = False

    def stop(self, grace):
        stop_all({self.proc.pid: self.start}, grace)
        self.reap(grace)

    def reap(self, grace):
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


def run_discovery(a, workdir, active):
    path = os.path.join(workdir, "discovery.json")
    job = Job("discovery", [sys.executable, RUNNER, "--_discover", str(os.getpid()), str(a.grace),
                            path, str(a.directory)],
              a.discover_timeout, workdir)
    active.append(job)
    try:
        while job.proc.poll() is None:
            _check_stop()
            if time.monotonic() >= job.end:
                job.stop(a.grace)
                print(f"unittest-parallel: discovery: deadline of {a.discover_timeout:g} s passed")
                sys.stdout.write(job.output())
                return None
            time.sleep(0.05)
    finally:
        if job.proc.returncode is not None:
            active.remove(job)
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
    if description["serial"]:
        print(f"unittest-parallel: the whole suite runs in one process, since {description['serial']}")
        return [("the whole suite", -1, -1)], []
    first, last = [], []
    for i, u in enumerate(units):
        if u["inert"]:
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
        parent, grace, out, directory = argv[1:]
        _die_with_parent(int(parent), float(grace))
        return discover_main(directory, out)
    if argv[:1] == ["--_unit"]:
        parent, grace, plan, index, split, record, absorb, directory = argv[1:]
        _die_with_parent(int(parent), float(grace))
        return worker_main(directory, plan, int(index), int(split), record, absorb)
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--jobs", type=int, default=min(8, os.cpu_count() or 1))
    ap.add_argument("--split", action="append", default=[])
    ap.add_argument("--unit-timeout", type=float, default=900.0)
    ap.add_argument("--discover-timeout", type=float, default=300.0)
    ap.add_argument("--grace", type=float, default=5.0)
    ap.add_argument("--keep", type=Path, help="keep the discovery, plan, outputs and result records in a new directory under this one")
    ap.add_argument("directory", type=Path)
    a = ap.parse_args(argv)
    if a.jobs < 1:
        ap.error("--jobs must be at least 1")
    for opt in ("unit_timeout", "discover_timeout", "grace"):
        if not (math.isfinite(getattr(a, opt)) and getattr(a, opt) > 0):
            ap.error(f"--{opt.replace('_', '-')} must be a finite number of seconds more than 0")
    old = {s: signal.signal(s, _on_signal) for s in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP)}
    if a.keep:
        a.keep.mkdir(parents=True, exist_ok=True)
        workdir = tempfile.mkdtemp(prefix="run-", dir=a.keep)
    else:
        workdir = tempfile.mkdtemp(prefix="unittest-parallel-")
    active = []
    try:
        return supervise(a, workdir, active)
    except Interrupted as e:
        names = " ".join(job.name for job in active)
        stop_all({job.proc.pid: job.start for job in active}, a.grace)
        for job in active:
            job.reap(a.grace)
        print(f"unittest-parallel: interrupted by signal {e.signum}, stopped: {names or 'nothing'}", flush=True)
        return 128 + e.signum
    finally:
        for s, h in old.items():
            signal.signal(s, h)
        if a.keep:
            print(f"unittest-parallel: discovery, plan, outputs and result records are in {workdir}")
        else:
            subprocess.run(["rm", "-rf", workdir], check=False)


def supervise(a, workdir, active):
    t0 = time.monotonic()
    description = run_discovery(a, workdir, active)
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
        job = Job(name, [sys.executable, RUNNER, "--_unit", str(os.getpid()), str(a.grace),
                         plan_path, str(index), str(split),
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
        if job.index < 0:
            leaves = [leaf for u in units for leaf in u["leaves"]]
        else:
            leaves = units[job.index]["leaves"]
        assigned = leaves if job.split < 0 else [leaves[job.split]]
        if job.timed_out:
            why = f"deadline of {a.unit_timeout:g} s passed"
        elif job.halted:
            why = "stopped when another unit passed its deadline"
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

    not_started = []
    halted = []

    def drain(queue, absorb_for):
        if halted:
            not_started.extend(w[0] for w in queue)
            return
        while queue or active:
            while queue and len(active) < a.jobs and not halted:
                _check_stop()
                start(queue.pop(0), absorb_for())
            time.sleep(0.05)
            _check_stop()
            late = [job for job in active if job.proc.poll() is None and time.monotonic() >= job.end]
            if late:
                # A unit past its deadline stops the run: no more units start, and the active
                # ones stop together, so hung units cannot take one deadline after another.
                for job in active:
                    job.timed_out = job in late
                    job.halted = job not in late
                halted.append(True)
                not_started.extend(w[0] for w in queue)
                queue.clear()
                stop_all({job.proc.pid: job.start for job in active}, a.grace)
                for job in active:
                    job.reap(a.grace)
            for job in list(active):
                if job.proc.poll() is not None or job.timed_out or job.halted:
                    active.remove(job)
                    finish(job)

    drain(list(first), lambda: "")
    absorb_path = ""
    if halted:
        not_started.extend(w[0] for w in last)
        last = []
    if last:
        absorb_path = os.path.join(workdir, "absorb.json")
        _write_json(absorb_path, absorbed)
    drain(list(last), lambda: absorb_path)
    work = len(first) + len(last)
    print(f"unittest-parallel: {totals['testsRun']} tests in {work} processes, {a.jobs} at a time, "
          f"{time.monotonic() - t0:.1f} s")
    if not_started:
        print(f"unittest-parallel: not started after a deadline: {' '.join(not_started)}")
    if failed:
        print(f"unittest-parallel: failed: {' '.join(failed)}")
        return 1
    if totals["testsRun"] == 0 and totals["skipped"] == 0:
        print("unittest-parallel: NO TESTS RAN")
        return 5
    return 0


if __name__ == "__main__":
    sys.exit(main())
