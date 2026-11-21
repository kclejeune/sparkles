"""A suite-level check that no test leaves a child process unwaited.

A subprocess.Popen whose process has not been waited for prints a ResourceWarning when it is
collected, and the collection can come during a later test, inside its captured stderr. The
discovery of scripts/bench-tests imports every module before any test runs, so importing
this module records where each Popen was created, and its one test, which sorts last, fails
with the creation stack of every Popen that was collected unwaited or is still unwaited at
the end of the suite. Under scripts/unittest-parallel.py each unit imports every module
too, reports its own leaks through parallel_export once its tests and teardown are done, and
the runner runs this module's test once, after every other unit, with those reports added
through parallel_absorb. The check needs no interpreter flags, so it runs under the plain
`python3 -m unittest discover -s scripts/bench-tests` of mise and CI.
"""
import git_isolation  # noqa: F401, first, so no fixture sees the host Git settings
import gc
import subprocess
import traceback
import unittest
import weakref

# Every Popen created since the import. Each one keeps its own creation stack, because a
# weak reference can be cleared before the object's __del__ runs.
_LIVE = weakref.WeakSet()
# Creation stacks of the Popen objects collected while their process was unwaited.
LEAKED = []

_init = subprocess.Popen.__init__
_del = subprocess.Popen.__del__


def _tracking_init(self, *args, **kwargs):
    self._created_at = ''.join(traceback.format_stack(limit=25)[:-1])
    _LIVE.add(self)
    _init(self, *args, **kwargs)


def _tracking_del(self, *args, **kwargs):
    # Popen.__del__ warns when no wait has set the returncode, before it polls the process,
    # so the same test here names the Popen that warns. One that is still running stays
    # alive in subprocess._active, so it is marked to be listed once.
    if getattr(self, '_child_created', False) and self.returncode is None:
        self._leaked = True
        LEAKED.append((getattr(self, 'args', None), getattr(self, '_created_at', '(created before the check was installed)\n')))
    _del(self, *args, **kwargs)


subprocess.Popen.__init__ = _tracking_init
subprocess.Popen.__del__ = _tracking_del


# Leaks that other processes of a parallel run reported, as (args, stack) pairs.
REPORTED = []


def unwaited():
    """The args and creation stacks of every leaked or still-unwaited Popen, after a collection,
    and of every one that another process of a parallel run reported."""
    gc.collect()
    found = list(REPORTED) + list(LEAKED)
    for proc in list(_LIVE):
        if getattr(proc, '_child_created', False) and proc.returncode is None and not getattr(proc, '_leaked', False):
            found.append((proc.args, proc._created_at))
    return found


def parallel_export():
    """This process's leaks, for scripts/unittest-parallel.py, which calls it in every unit
    after its tests and teardown and passes the results to parallel_absorb."""
    return [[repr(args), stack] for args, stack in unwaited()]


def parallel_absorb(exports):
    """Take the (unit, leaks) pairs of the other units of a parallel run. This module's own
    unit runs after all of them, so its one test sees the whole suite's leaks."""
    for unit, leaks in exports:
        REPORTED.extend(('%s (in unit %s)' % (args, unit), stack) for args, stack in leaks)


class ZzNoUnwaitedProcesses(unittest.TestCase):
    def test_no_test_left_a_process_unwaited(self):
        found = unwaited()
        self.assertFalse(found, 'processes left unwaited:\n' + '\n'.join(
            'args %r, created at\n%s' % (args, stack) for args, stack in found))


if __name__ == '__main__':
    unittest.main()
