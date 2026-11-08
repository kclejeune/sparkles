"""Run finalizer_interrupt_worker.py with a foreign sentinel and close what it leaves."""
import ctypes
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile

WORKER = Path(__file__).with_name('finalizer_interrupt_worker.py')


def _close_leftover(ready):
    """Kill and reap the worker's fixture only if it is still that exact process."""
    if not ready.exists():
        return
    pid = int(ready.read_text())
    try:
        state = Path('/proc', str(pid), 'stat').read_text().split(') ', 1)[1].split()[0]
        # An exited fixture adopted here is reaped without signalling. A live one
        # is signalled only when its command line names the unique ready path,
        # so a recycled pid never matches.
        if state != 'Z':
            if str(ready).encode() not in Path('/proc', str(pid), 'cmdline').read_bytes():
                return
            os.kill(pid, signal.SIGKILL)
    except (FileNotFoundError, ProcessLookupError):
        return
    try:
        os.waitpid(pid, 0)
    except ChildProcessError:
        pass


def run(target, phase, count, leader, handler='default'):
    """Return the worker's outcome and whether a foreign session survived it."""
    libc = ctypes.CDLL(None)
    previous = ctypes.c_int()
    libc.prctl(37, ctypes.byref(previous), 0, 0, 0)
    # Adopt fixtures a failing worker orphans so that they can be reaped here.
    libc.prctl(36, 1, 0, 0, 0)
    try:
        with tempfile.TemporaryDirectory(prefix='finalizer-interrupt-') as tmp:
            work = Path(tmp)
            foreign = subprocess.Popen(['sleep', '20'], start_new_session=True)
            try:
                done = subprocess.run([sys.executable, '-B', str(WORKER), target, phase, str(count), leader, str(work), handler],
                                      stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
                                      timeout=30, start_new_session=True)
                outcome = json.loads((work / 'outcome.json').read_text())
                outcome['worker_exit'] = done.returncode
                outcome['foreign_alive'] = foreign.poll() is None
                return outcome
            finally:
                foreign.kill(); foreign.wait()
                _close_leftover(work / 'ready')
    finally:
        libc.prctl(36, previous.value, 0, 0, 0)
