"""Stdlib-only lifecycle tests, runnable before a native extension build.

These tests prove executor/token behavior with explicit blocking events. Native
query/transaction/installed-package evidence remains a separate gate.
"""

from __future__ import annotations

import asyncio
import importlib.util
import threading
import unittest
from pathlib import Path

_SOURCE = Path(__file__).parents[1] / "python/sparkles/_aio_worker.py"
if not _SOURCE.is_file():
    # The wheel gate copies tests outside the checkout and loads installed code.
    _PACKAGE = importlib.util.find_spec("sparkles")
    assert _PACKAGE is not None and _PACKAGE.origin is not None
    _SOURCE = Path(_PACKAGE.origin).with_name("_aio_worker.py")
_SPEC = importlib.util.spec_from_file_location("sparkles_aio_worker_contract", _SOURCE)
assert _SPEC is not None and _SPEC.loader is not None
_MODULE = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(_MODULE)
_Worker = _MODULE._Worker


class NativeCancelled(Exception):
    pass


class Token:
    def __init__(self) -> None:
        self.flag = threading.Event()

    @property
    def cancelled(self) -> bool:
        return self.flag.is_set()

    def cancel(self) -> None:
        self.flag.set()


async def event(event: threading.Event) -> None:
    if not await asyncio.to_thread(event.wait, 2):
        raise AssertionError("worker event was not acknowledged")


class WorkerTests(unittest.IsolatedAsyncioTestCase):
    async def test_owner_thread_and_retained_lease_teardown(self) -> None:
        worker = _Worker(NativeCancelled)
        worker.retain()
        try:
            owner, name = await worker.call(
                lambda: (threading.get_ident(), threading.current_thread().name),
                Token(),
            )
            self.assertNotEqual(owner, threading.get_ident())
            await worker.release()
            self.assertEqual(await worker.call(threading.get_ident, Token()), owner)
        finally:
            await worker.release()
        self.assertNotIn(name, [thread.name for thread in threading.enumerate()])

    async def test_native_exception_instance_and_fields_survive(self) -> None:
        worker = _Worker(NativeCancelled)
        error = OSError(13, "fixed native refusal")

        def fail() -> None:
            raise error

        try:
            with self.assertRaises(OSError) as caught:
                await worker.call(fail, Token())
            self.assertIs(caught.exception, error)
            self.assertEqual(caught.exception.errno, 13)
        finally:
            await worker.release()

    async def test_repeated_task_cancel_waits_for_native_settlement(self) -> None:
        worker = _Worker(NativeCancelled)
        token = Token()
        started, saw_cancel, finish, stopped = [threading.Event() for _ in range(4)]

        def native() -> None:
            started.set()
            if not token.flag.wait(2):
                raise AssertionError("native cancellation flag was not set")
            saw_cancel.set()
            if not finish.wait(2):
                raise AssertionError("test cleanup did not release worker")
            stopped.set()
            raise NativeCancelled("native stopped")

        task = asyncio.create_task(worker.call(native, token))
        loop = asyncio.get_running_loop()
        old_handler = loop.get_exception_handler()
        unhandled: list[dict[str, object]] = []
        loop.set_exception_handler(lambda _loop, context: unhandled.append(context))
        try:
            await event(started)
            task.cancel()
            await event(saw_cancel)
            self.assertFalse(task.done())
            task.cancel()
            # The event loop keeps executing while native cleanup is blocked.
            for _ in range(3):
                await asyncio.sleep(0)
            self.assertFalse(task.done())
            finish.set()
            with self.assertRaises(asyncio.CancelledError):
                await task
            self.assertTrue(stopped.is_set())
            self.assertTrue(token.cancelled)
            await asyncio.sleep(0)
            self.assertEqual(
                unhandled, [], "settled native cancellation was logged as unhandled"
            )
        finally:
            finish.set()
            await asyncio.gather(task, return_exceptions=True)
            await worker.release()
            loop.set_exception_handler(old_handler)

    async def test_write_completion_wins_cancel_without_hiding_publication(
        self,
    ) -> None:
        worker = _Worker(NativeCancelled)
        token = Token()
        committed, finish = threading.Event(), threading.Event()
        receipt = {"committed": True, "sequence": 1}

        def native() -> dict[str, object]:
            committed.set()
            if not finish.wait(2):
                raise AssertionError("test cleanup did not release committed worker")
            return receipt

        task = asyncio.create_task(worker.call(native, token, write_completion=True))
        try:
            await event(committed)
            task.cancel()
            # Completion is known, but acknowledgement still waits for native return.
            await asyncio.sleep(0)
            self.assertFalse(task.done())
            finish.set()
            self.assertIs(await task, receipt)
            self.assertFalse(task.cancelled())
            if hasattr(task, "cancelling"):
                self.assertEqual(task.cancelling(), 0)
        finally:
            finish.set()
            await asyncio.gather(task, return_exceptions=True)
            await worker.release()

    async def test_cancelled_resource_creation_closes_unreturned_handle_on_owner(
        self,
    ) -> None:
        worker = _Worker(NativeCancelled)
        created, finish = threading.Event(), threading.Event()
        calls: list[tuple[str, int]] = []
        handle = object()

        def native() -> object:
            calls.append(("create", threading.get_ident()))
            created.set()
            if not finish.wait(2):
                raise AssertionError("test cleanup did not release resource creation")
            return handle

        def discard(value: object) -> None:
            self.assertIs(value, handle)
            calls.append(("close", threading.get_ident()))

        task = asyncio.create_task(worker.call(native, Token(), discard=discard))
        try:
            await event(created)
            task.cancel()
            await asyncio.sleep(0)
            finish.set()
            with self.assertRaises(asyncio.CancelledError):
                await task
            self.assertEqual([name for name, _ in calls], ["create", "close"])
            self.assertEqual(calls[0][1], calls[1][1])
        finally:
            finish.set()
            await asyncio.gather(task, return_exceptions=True)
            await worker.release()

    async def test_cancel_before_admission_does_not_dispatch_or_cancel_other_call(
        self,
    ) -> None:
        worker = _Worker(NativeCancelled)
        started, finish = threading.Event(), threading.Event()
        first_token, second_token = Token(), Token()
        dispatched: list[bool] = []

        def native() -> int:
            started.set()
            if not finish.wait(2):
                raise AssertionError("test cleanup did not release first call")
            return 7

        first = asyncio.create_task(worker.call(native, first_token))
        second = None
        try:
            await event(started)
            second = asyncio.create_task(
                worker.call(lambda: dispatched.append(True), second_token)
            )
            await asyncio.sleep(0)
            second.cancel()
            with self.assertRaises(asyncio.CancelledError):
                await second
            self.assertEqual(dispatched, [])
            self.assertTrue(second_token.cancelled)
            self.assertFalse(first_token.cancelled)
            finish.set()
            self.assertEqual(await first, 7)
        finally:
            finish.set()
            await asyncio.gather(
                first, *([second] if second is not None else []), return_exceptions=True
            )
            await worker.release()

    async def test_foreign_event_loop_refuses_before_dispatch(self) -> None:
        worker = _Worker(NativeCancelled)
        try:
            await worker.call(lambda: None, Token())

            def another_loop() -> None:
                asyncio.run(worker.call(lambda: None, Token()))

            with self.assertRaisesRegex(RuntimeError, "one event loop"):
                await asyncio.to_thread(another_loop)
        finally:
            await worker.release()


if __name__ == "__main__":
    unittest.main()
