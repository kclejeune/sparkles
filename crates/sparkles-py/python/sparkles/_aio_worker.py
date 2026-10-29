"""Owned off-loop execution for the async facade, independent of native imports.

All submissions for one owner use the same Python worker thread. Shielding keeps
cancelling an asyncio waiter from being mistaken for stopping its native call.
"""

from __future__ import annotations

import asyncio
import itertools
from collections.abc import Callable
from concurrent.futures import ThreadPoolExecutor
from functools import partial
from typing import Protocol, TypeVar

_T = TypeVar("_T")
_worker_ids = itertools.count(1)


class _Token(Protocol):
    @property
    def cancelled(self) -> bool: ...
    def cancel(self) -> None: ...


async def _owned_wait(future: asyncio.Future[_T]) -> _T:
    """Do not propagate waiter cancellation into a future we explicitly settle.

    Python 3.14 shield installs an exception logger after waiter cancellation even
    when its owner subsequently retrieves the native failure. This local bridge
    retains that ownership, without changing the loop's global error handler.
    """
    if future.done():
        return future.result()
    waiter: asyncio.Future[_T] = asyncio.get_running_loop().create_future()

    def ready(done: asyncio.Future[_T]) -> None:
        if waiter.done():
            return
        if done.cancelled():
            waiter.cancel()
        elif (error := done.exception()) is not None:
            waiter.set_exception(error)
        else:
            waiter.set_result(done.result())

    future.add_done_callback(ready)
    try:
        return await waiter
    finally:
        future.remove_done_callback(ready)
        if waiter.done() and not waiter.cancelled():
            # Consume a completed waiter's outcome even if task cancellation won
            # the scheduling race. The owned inner future is still settled below.
            waiter.exception()


async def _settle(future: asyncio.Future[_T], on_cancel: Callable[[], None]) -> _T:
    """Wait through repeated waiter cancellation, then preserve the worker outcome."""
    while not future.done():
        try:
            await _owned_wait(future)
        except asyncio.CancelledError:
            on_cancel()
        except BaseException:  # noqa: BLE001
            # The identical stored exception is re-raised by future.result below.
            break
    return future.result()


def _cancel_count(task: asyncio.Task[object] | None) -> int:
    return getattr(task, "cancelling", lambda: 0)() if task is not None else 0


def _consume_new_cancellations(
    task: asyncio.Task[object] | None, baseline: int
) -> None:
    # Python 3.10 has no uncancel. Later versions expose the request counter, which
    # must not remain artificially raised after a successful write wins the race.
    if task is not None and hasattr(task, "uncancel"):
        while _cancel_count(task) > baseline:
            task.uncancel()


class _Worker:
    """One loop-bound executor, leased by a dataset and its retained handles."""

    def __init__(self, cancelled_error: Callable[[], BaseException]) -> None:
        self._executor = ThreadPoolExecutor(
            max_workers=1, thread_name_prefix=f"sparkles-aio-{next(_worker_ids)}"
        )
        self._cancelled_error = cancelled_error
        self._loop: asyncio.AbstractEventLoop | None = None
        self._gate = asyncio.Lock()
        self._tokens: dict[object, _Token] = {}
        self._owners = 1
        self._closing = False
        self._closed = False
        self._shutdown_task: asyncio.Task[None] | None = None

    def _bind(self) -> asyncio.AbstractEventLoop:
        loop = asyncio.get_running_loop()
        if self._loop is None:
            self._loop = loop
        elif self._loop is not loop:
            raise RuntimeError("an async Sparkles handle belongs to one event loop")
        return loop

    def retain(self) -> None:
        self._bind()
        if self._closing:
            raise RuntimeError("the async Sparkles worker is closed")
        self._owners += 1

    async def call(
        self,
        function: Callable[[], _T],
        token: _Token,
        *,
        write_completion: bool = False,
        discard: Callable[[_T], None] | None = None,
        interruptible: bool = True,
        admit_cancelled: bool = False,
    ) -> _T:
        """Run once and settle native work before acknowledging task cancellation.

        A successful atomic write/commit wins a cancellation request. For a read or
        resource creation, discard can close an unreturned retained native handle.
        Resource-ending calls can admit an already cancelled token so the native
        end operation still releases its resource and observes its own controls.
        """
        loop = self._bind()
        if self._closing:
            raise RuntimeError("the async Sparkles worker is closed")
        entry = object()
        if interruptible:
            self._tokens[entry] = token
        task = asyncio.current_task()
        baseline = _cancel_count(task)
        try:
            async with self._gate:
                if self._closing:
                    raise RuntimeError("the async Sparkles worker is closed")
                if interruptible and not admit_cancelled and token.cancelled:
                    raise self._cancelled_error()
                future = loop.run_in_executor(self._executor, function)
                try:
                    return await _owned_wait(future)
                except asyncio.CancelledError as cancelled:
                    # A backend can itself raise this BaseException. Do not rewrite
                    # it into task cancellation when it is the actual worker error.
                    if (
                        future.done()
                        and not future.cancelled()
                        and future.exception() is cancelled
                    ):
                        raise
                    token.cancel()
                    try:
                        result = await _settle(future, token.cancel)
                    except BaseException as error:
                        raise cancelled from error
                    if write_completion:
                        _consume_new_cancellations(task, baseline)
                        return result
                    if discard is not None:
                        cleanup = loop.run_in_executor(
                            self._executor, partial(discard, result)
                        )
                        try:
                            await _settle(cleanup, token.cancel)
                        except BaseException as error:
                            raise cancelled from error
                    raise
        except asyncio.CancelledError:
            token.cancel()
            raise
        finally:
            self._tokens.pop(entry, None)

    def cancel_calls(self) -> None:
        """Stop submitted/pending work while leaving idle retained handles intact."""
        self._bind()
        for token in self._tokens.values():
            token.cancel()

    async def release(self) -> None:
        """Drop one retained lease; the last close awaits off-loop thread teardown."""
        loop = self._bind()
        if self._owners <= 0:
            raise RuntimeError("the async Sparkles worker lease was already released")
        self._owners -= 1
        if self._owners:
            return
        self._closing = True
        self.cancel_calls()
        self._shutdown_task = loop.create_task(self._shutdown())
        try:
            await _owned_wait(self._shutdown_task)
        except asyncio.CancelledError:
            await _settle(self._shutdown_task, lambda: None)
            raise

    async def _shutdown(self) -> None:
        try:
            async with self._gate:
                loop = self._bind()
                # Joining the owned executor must never block the event loop or try
                # to join its own worker. All native calls have settled at this gate.
                await loop.run_in_executor(
                    None, partial(self._executor.shutdown, wait=True)
                )
        finally:
            self._closed = True
