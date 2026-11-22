"""Liveness checks for script tests that assert a process they stopped is gone.

A process killed or orphaned by the code under test stays a zombie until its parent or
the subreaper reaps it, and kill(pid, 0) still finds a zombie. On a loaded host that reap,
and the kill itself, can take well over a second, so a fixed pause or an immediate
kill(pid, 0) fails a test whose code behaved. ended() waits a bounded time and counts a
zombie as ended, since a zombie runs nothing.
"""
import os
from pathlib import Path
import time


def state(pid):
    """The process state letter of PID, or None when no such process exists."""
    try:
        return Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()[0]
    except (FileNotFoundError, ProcessLookupError, IndexError):
        pass
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return None
    except PermissionError:
        pass
    return '?'


def ended(pid, seconds=10.0, poll=0.05):
    """Wait up to SECONDS for PID to exit or become a zombie, and say whether it did."""
    end = time.monotonic() + seconds
    while True:
        if state(pid) in (None, 'Z'):
            return True
        if time.monotonic() >= end:
            return False
        time.sleep(poll)
