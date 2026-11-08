"""Finite teardown of one owned process group that a Ctrl-C cannot abandon.

The caller created the group with ``start_new_session=True``, so its id equals the
leader's pid. Teardown sends TERM and then KILL to that group only, waits for the
leader, reaps group members this process may reap and checks that the group is
empty. Every stage runs against one cleanup deadline that is fixed when teardown
starts and is never reset.

SIGINT is deferred for the whole teardown. Each SIGINT that arrives is counted and
cleanup continues, so repeated cancellation neither skips a stage nor lengthens the
window. A KeyboardInterrupt raised by a stage itself, such as one that arrived just
before the deferral was installed, is counted the same way and the next stage still
runs. The caller's streams and selector are closed in an unconditional inner finally.
After teardown the previous SIGINT handler is restored. With Python's default
handler the count is returned for the caller to propagate; any other handler gets
the signal redelivered once.
"""
import os
import signal
import threading
import time

TERM_GRACE = 2
KILL_GRACE = 3
WINDOW = 5


class _Deferred:
    def __init__(self):
        self.count = 0
        self.previous = None
        self.installed = False

    def record(self, signum, frame):
        self.count += 1

    def install(self):
        if self.installed or threading.current_thread() is not threading.main_thread():
            return
        self.previous = signal.getsignal(signal.SIGINT)
        if self.previous is None:
            # A handler installed outside Python cannot be restored from here.
            return
        # Mark first so an interrupt between these lines still restores.
        self.installed = True
        signal.signal(signal.SIGINT, self.record)

    def restore(self):
        if not self.installed:
            return self.count
        signal.signal(signal.SIGINT, self.previous)
        self.installed = False
        if self.previous is signal.default_int_handler:
            return self.count
        if self.count and self.previous is not signal.SIG_IGN:
            signal.raise_signal(signal.SIGINT)
        return 0


def _stage(deferred, action, *args):
    """Run one teardown stage, counting rather than propagating an interrupt."""
    try:
        return action(*args)
    except KeyboardInterrupt:
        deferred.count += 1
        return None


def _signal(pid, sig, problems):
    try:
        os.killpg(pid, sig)
    except ProcessLookupError:
        pass
    except OSError as problem:
        problems.append(type(problem).__name__)


def _wait(process, grace, end, sig, problems, timeout_error):
    try:
        process.wait(timeout=max(0.001, min(grace, end - time.monotonic())))
    except timeout_error:
        if sig == signal.SIGKILL:
            problems.append('transport leader closure unconfirmed')


def _probe(pid, end, problems):
    """Return True once the group is proved empty or closure is unresolved."""
    # Reap only children in this invocation's group when this process is
    # their parent or subreaper. Otherwise the system owner must reap them.
    try:
        while os.waitpid(-pid, os.WNOHANG)[0] > 0:
            pass
    except ChildProcessError:
        pass
    try:
        os.killpg(pid, 0)
    except ProcessLookupError:
        return True
    except OSError as problem:
        problems.append(type(problem).__name__)
        return True
    if time.monotonic() >= end:
        problems.append('owned invocation closure unconfirmed')
        return True
    time.sleep(0.01)
    return False


def _attempt(action, problems):
    # A failed caller step, such as restoring a source descriptor, must not
    # skip group closure. It is reported as unresolved cleanup instead.
    try:
        action()
    except Exception as problem:
        problems.append('prelude ' + type(problem).__name__)


def _close(resource, problems):
    close = getattr(resource, 'close', None)
    if close is None or getattr(resource, 'closed', False):
        return
    try:
        close()
    except (OSError, ValueError) as problem:
        problems.append('stream close ' + type(problem).__name__)


def finish(process, closables=(), prelude=(), timeout_error=Exception):
    """Close the owned group and every closable within one fixed window.

    Returns ``(problems, interrupts)``. ``problems`` lists unresolved closure and
    ``interrupts`` counts deferred SIGINTs. Neither is raised here, so the caller
    keeps its primary outcome and decides how to report both.
    """
    deferred = _Deferred()
    problems = []
    try:
        try:
            _stage(deferred, deferred.install)
            for action in prelude:
                _stage(deferred, _attempt, action, problems)
            end = _stage(deferred, lambda: time.monotonic() + WINDOW)
            if end is None:
                end = time.monotonic() + WINDOW
            # A zero-exit leader is not proof that its invocation is empty.
            for sig, grace in ((signal.SIGTERM, TERM_GRACE), (signal.SIGKILL, KILL_GRACE)):
                _stage(deferred, _signal, process.pid, sig, problems)
                _stage(deferred, _wait, process, grace, end, sig, problems, timeout_error)
            while True:
                done = _stage(deferred, _probe, process.pid, end, problems)
                if done:
                    break
                if done is None and time.monotonic() >= end:
                    problems.append('owned invocation closure unconfirmed')
                    break
        finally:
            for resource in closables:
                _stage(deferred, _close, resource, problems)
    finally:
        interrupts = deferred.restore()
    return problems, interrupts
