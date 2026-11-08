"""Deliver real SIGINTs to this worker while an owned transport is being torn down.

Usage: finalizer_interrupt_worker.py TARGET PHASE COUNT LEADER WORK

TARGET is exchange or bounded. PHASE names the teardown step at which COUNT
SIGINTs are sent to this process with os.kill: term_wait, term, kill or probe.
LEADER is timeout for a TERM-ignoring leader that outlives a tiny deadline, or a
numeric exit for a leader that leaves one TERM-ignoring descendant behind. The
worker writes outcome.json into WORK. It starts no provider, network or manager.
"""
import ctypes
import importlib.util
import io
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

target, phase, count, leader, work = sys.argv[1:]
count = int(count)
work = Path(work)
root = Path(__file__).resolve().parents[2]
signal.signal(signal.SIGINT, signal.default_int_handler)
ctypes.CDLL(None).prctl(36, 1, 0, 0, 0)

if target == 'exchange':
    spec = importlib.util.spec_from_file_location('rx_worker', root / 'scripts/remote-executor.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
else:
    sys.path.insert(0, str(root / 'scripts/boat'))
    import controller as module

fired = []


def interrupt(where):
    if where == phase and not fired:
        fired.append(where)
        for _ in range(count):
            os.kill(os.getpid(), signal.SIGINT)


created = []
Original = subprocess.Popen


class Observed(Original):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        created.append(self)

    def wait(self, *args, **kwargs):
        if (kwargs.get('timeout') or 0) > 1 and self.poll() is None and created and self is created[0]:
            interrupt('term_wait')
        return super().wait(*args, **kwargs)


real_killpg = os.killpg


def killpg(pid, sig):
    if created and pid == created[0].pid:
        interrupt({signal.SIGTERM: 'term', signal.SIGKILL: 'kill', 0: 'probe'}.get(sig))
    return real_killpg(pid, sig)


subprocess.Popen = Observed
os.killpg = killpg
ready = work / 'ready'
if leader == 'timeout':
    argv = [sys.executable, '-c', "import os,signal,time,pathlib;signal.signal(signal.SIGTERM,signal.SIG_IGN);pathlib.Path(" + repr(str(ready)) + ").write_text(str(os.getpid()));time.sleep(5)"]
    seconds = 0.3
else:
    child = "import os,signal,time,pathlib;signal.signal(signal.SIGTERM,signal.SIG_IGN);pathlib.Path(" + repr(str(ready)) + ").write_text(str(os.getpid()));time.sleep(5)"
    argv = [sys.executable, '-c', "import sys,subprocess,pathlib,time;p=subprocess.Popen([sys.executable,'-c'," + repr(child) + "],stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);end=time.monotonic()+2\nwhile not pathlib.Path(" + repr(str(ready)) + ").exists():\n if time.monotonic()>end:raise SystemExit(19)\n time.sleep(.005)\nprint('{}');raise SystemExit(" + leader + ")"]
    seconds = 3
outcome = {}
try:
    if target == 'exchange':
        outcome['returned'] = module.exchange(argv, io.BytesIO(), source=io.BytesIO(b'x'), deadline=time.monotonic() + seconds)
    else:
        outcome['returned'] = module.bounded(argv, seconds)
except BaseException as error:
    outcome.update(exception=type(error).__name__, message=str(error),
                   native_exit=getattr(error, 'native_exit', None),
                   cleanup_errors=getattr(error, 'cleanup_errors', None),
                   cleanup_interrupted=getattr(error, 'cleanup_interrupted', None))
outcome['fired'] = fired
outcome['leader_pid'] = created[0].pid if created else None
outcome['streams_closed'] = [s.closed for s in (created[0].stdin, created[0].stdout, created[0].stderr) if s is not None] if created else None
pid = int(ready.read_text()) if ready.exists() else None
outcome['ready_pid'] = pid
alive = False
if pid is not None:
    try:
        state = Path('/proc', str(pid), 'stat').read_text().split(') ', 1)[1].split()[0]
        alive = state not in ('Z', 'X')
    except (FileNotFoundError, ProcessLookupError):
        alive = False
outcome['ready_alive_after_return'] = alive
(work / 'outcome.json').write_text(json.dumps(outcome))
