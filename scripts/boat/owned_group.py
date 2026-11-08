"""Finite teardown of one owned process group that a Ctrl-C cannot abandon.

The caller created the group with ``start_new_session=True``, so its id equals the
leader's pid. Teardown sends TERM and then KILL to that group only, waits for the
leader, reaps group members this process may reap and checks that the group is
empty. Every stage runs against one cleanup deadline that is fixed when teardown
starts and is never reset.

The caller installs a ``Cancellation`` before it starts the child. From then until
teardown ends, SIGINT never raises asynchronously. The handler only records the
signal. While the transfer runs, the caller calls ``check`` at its loop points, which
hands the signal to the previous handler there, so the default handler raises
KeyboardInterrupt at a known place. Once teardown starts, every SIGINT is counted and
cleanup continues, so repeated cancellation neither skips a stage nor lengthens the
window. A KeyboardInterrupt raised by a stage itself is counted and that stage is
attempted again, up to three times. Streams and the selector close in an
unconditional inner finally, and the previous handler is restored afterwards.
``conclude`` then attaches the count to a primary failure or, when there is none,
cancels the call once closure has finished.
"""
import os
import signal
import threading
import time

TERM_GRACE = 2
KILL_GRACE = 3
WINDOW = 5


class Cancellation:
    """Record SIGINT for one owned invocation instead of raising asynchronously."""

    def __init__(self):
        self.pending = 0
        self.teardown = 0
        self.in_teardown = False
        self.previous = None
        self.installed = False

    def record(self, signum, frame):
        if self.in_teardown:
            self.teardown += 1
        else:
            self.pending += 1

    def install(self):
        if self.installed or threading.current_thread() is not threading.main_thread():
            return self
        previous = signal.getsignal(signal.SIGINT)
        # SIG_IGN never interrupts, and a handler installed outside Python cannot be
        # restored from here, so both are left in place.
        if previous is None or previous is signal.SIG_IGN:
            return self
        self.previous = previous
        self.installed = True
        signal.signal(signal.SIGINT, self.record)
        return self

    def _default(self):
        return self.previous is signal.default_int_handler or self.previous is signal.SIG_DFL

    def check(self):
        """Hand a SIGINT received during the transfer to the previous handler."""
        if not self.pending or self.in_teardown:
            return
        self.pending = 0
        if self._default():
            raise KeyboardInterrupt()
        if callable(self.previous):
            self.previous(signal.SIGINT, None)

    def begin_teardown(self):
        self.in_teardown = True
        self.teardown += self.pending
        self.pending = 0

    def restore(self):
        if self.installed:
            self.installed = False
            signal.signal(signal.SIGINT, self.previous)


ATTEMPTS = 3


def _stage(cancel, problems, name, action, *args):
    """Run one stage, attempting it again after an interrupt a fixed number of times."""
    for _ in range(ATTEMPTS):
        try:
            return action(*args)
        except KeyboardInterrupt:
            cancel.teardown += 1
    problems.append(name + ' interrupted')
    return None


def _attempt(action, problems):
    # A failed caller step, such as restoring a source descriptor, must not
    # skip group closure. It is reported as unresolved cleanup instead.
    try:
        action()
    except Exception as problem:
        problems.append('prelude ' + type(problem).__name__)


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


def _close(resource, problems):
    close = getattr(resource, 'close', None)
    if close is None or getattr(resource, 'closed', False):
        return
    try:
        close()
    except (OSError, ValueError) as problem:
        problems.append('stream close ' + type(problem).__name__)


def finish(process, closables=(), prelude=(), timeout_error=Exception, cancel=None):
    """Close the owned group and every closable within one fixed window.

    Returns ``(problems, interrupts)``. ``problems`` lists unresolved closure and
    ``interrupts`` counts cancellations seen during teardown. Neither is raised
    here, so the caller keeps its primary outcome and reports both with
    ``conclude``. The cancellation's handler is restored before returning.
    """
    if cancel is None:
        cancel = Cancellation().install()
    cancel.begin_teardown()
    problems = []
    end = time.monotonic() + WINDOW
    try:
        try:
            for action in prelude:
                _stage(cancel, problems, 'prelude', _attempt, action, problems)
            # A zero-exit leader is not proof that its invocation is empty.
            for sig, grace in ((signal.SIGTERM, TERM_GRACE), (signal.SIGKILL, KILL_GRACE)):
                _stage(cancel, problems, 'signal', _signal, process.pid, sig, problems)
                _stage(cancel, problems, 'wait', _wait, process, grace, end, sig, problems, timeout_error)
            while not _stage(cancel, problems, 'probe', _probe, process.pid, end, problems):
                if time.monotonic() >= end:
                    problems.append('owned invocation closure unconfirmed')
                    break
        finally:
            for resource in closables:
                _stage(cancel, problems, 'close', _close, resource, problems)
    finally:
        cancel.restore()
    return problems, cancel.teardown


def conclude(cancel, failure, **details):
    """Report teardown cancellation without replacing a primary failure.

    With a primary failure the count is attached and a custom previous handler is
    still called, with anything it raises recorded on the failure. Without one, the
    default handler's cancellation is raised as KeyboardInterrupt carrying
    ``details``, and a custom handler is called and may raise itself.
    """
    count = cancel.teardown
    if not count:
        return
    custom = callable(cancel.previous) and not cancel._default()
    if failure is not None:
        failure.cleanup_interrupted = count
        if custom:
            try:
                cancel.previous(signal.SIGINT, None)
            except BaseException as error:
                failure.cleanup_interrupt_error = type(error).__name__
        return
    if custom:
        cancel.previous(signal.SIGINT, None)
        return
    cancelled = KeyboardInterrupt()
    cancelled.cleanup_interrupted = count
    for name, value in details.items():
        setattr(cancelled, name, value)
    raise cancelled
