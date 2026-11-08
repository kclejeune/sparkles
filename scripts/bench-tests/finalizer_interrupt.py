"""Run finalizer_interrupt_worker.py with a foreign sentinel and close what it leaves."""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile

WORKER = Path(__file__).with_name('finalizer_interrupt_worker.py')


def run(target, phase, count, leader):
    """Return the worker's outcome and whether a foreign session survived it."""
    with tempfile.TemporaryDirectory(prefix='finalizer-interrupt-') as tmp:
        work = Path(tmp)
        foreign = subprocess.Popen(['sleep', '20'], start_new_session=True)
        try:
            done = subprocess.run([sys.executable, '-B', str(WORKER), target, phase, str(count), leader, str(work)],
                                  stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
                                  timeout=30, start_new_session=True)
            outcome = json.loads((work / 'outcome.json').read_text())
            outcome['worker_exit'] = done.returncode
            outcome['foreign_alive'] = foreign.poll() is None
            return outcome
        finally:
            foreign.kill(); foreign.wait()
            ready = work / 'ready'
            if ready.exists():
                # Close only the exact fixture process this worker created.
                try:os.kill(int(ready.read_text()), signal.SIGKILL)
                except ProcessLookupError:pass
