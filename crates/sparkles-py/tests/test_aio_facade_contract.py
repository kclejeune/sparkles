"""Stdlib fake-backend contracts; these are not native or installed-wheel evidence."""

from __future__ import annotations

import asyncio
import importlib
import importlib.util
import sys
import threading
import types
import unittest
from pathlib import Path
from typing import Any


class NativeCancelled(Exception):
    pass


class NativeInvalid(ValueError):
    pass


class NativeTimeout(TimeoutError):
    pass


class Token:
    def __init__(
        self, flag: threading.Event | None = None, extra: tuple[Token, ...] = ()
    ) -> None:
        self.flag = flag if flag is not None else threading.Event()
        self.extra = extra

    @property
    def cancelled(self) -> bool:
        return self.flag.is_set() or any(token.cancelled for token in self.extra)

    def cancel(self) -> None:
        self.flag.set()

    def _with_cancel(self, other: Token) -> Token:
        if not isinstance(other, Token):
            raise TypeError("cancel must be a CancelToken")
        return Token(self.flag, self.extra + (other,))


class Factory:
    def __init__(self) -> None:
        self.datasets: list[NativeDataset] = []
        self.query_started, self.query_stopped = threading.Event(), threading.Event()
        self.commit_published, self.commit_release = (
            threading.Event(),
            threading.Event(),
        )
        self.hold_commit = False
        self.block_cursor = False
        self.cursor_started = threading.Event()
        self.error = OSError(13, "fixed storage refusal")
        self.fail_open = False
        self.block_open = False
        self.open_started, self.open_release = threading.Event(), threading.Event()

    def open(self, *_args: Any, **_kwargs: Any) -> NativeDataset:
        self.open_started.set()
        if self.block_open and not self.open_release.wait(2):
            raise AssertionError("test did not release native opening")
        if self.fail_open:
            raise self.error
        dataset = NativeDataset(self)
        self.datasets.append(dataset)
        return dataset


class NativeDataset:
    def __init__(self, factory: Factory) -> None:
        self.factory = factory
        self.owner = threading.get_ident()
        self.name = threading.current_thread().name
        self.data: list[str] = ["one", "two", "three"]
        self.closed = False
        self.writer = False
        self.last_cursor: NativeCursor | None = None

    def touch(self, *, child: bool = False) -> None:
        if threading.get_ident() != self.owner:
            raise AssertionError("native owner thread was bypassed")
        if self.closed and not child:
            raise NativeInvalid("dataset closed")

    def close(self) -> None:
        self.touch(child=True)
        self.closed = True

    def query(self, query: str, *, cancel: Token, **_kwargs: Any) -> list[str]:
        self.touch()
        if query == "fail":
            raise self.factory.error
        if query == "slow":
            self.factory.query_started.set()
            if not cancel.flag.wait(2):
                raise AssertionError(
                    "owned cancellation did not reach fake native query"
                )
            self.factory.query_stopped.set()
            raise NativeCancelled("query stopped")
        return list(self.data)

    select = query

    def ask(self, query: str, *, cancel: Token, **kwargs: Any) -> bool:
        return bool(self.query(query, cancel=cancel, **kwargs))

    def update(
        self, update: str, *, cancel: Token, **_kwargs: Any
    ) -> dict[str, object]:
        self.touch()
        if self.writer:
            raise NativeInvalid("use the open transaction")
        if cancel.cancelled:
            raise NativeCancelled("write stopped")
        self.data.append(update)
        return {"committed": True}

    def select_cursor(
        self, _query: str, *, cancel: Token, batch_rows: int = 2, **_kwargs: Any
    ) -> NativeCursor:
        self.touch()
        self.last_cursor = NativeCursor(self, cancel, batch_rows)
        return self.last_cursor

    graph_cursor = select_cursor

    def _transaction(
        self, cancel: Token, *, timeout: float | None = None
    ) -> NativeTransaction:
        self.touch()
        if cancel.cancelled:
            raise NativeCancelled("admission stopped")
        if self.writer:
            raise NativeInvalid("writer already held")
        self.writer = True
        return NativeTransaction(self, cancel)


class NativeCursor:
    def __init__(self, dataset: NativeDataset, cancel: Token, batch_rows: int) -> None:
        self.dataset, self.cancel = dataset, cancel
        self.snapshot = list(dataset.data)
        self.batch_rows = batch_rows
        self.position = 0
        self.fetches = 0
        self.status = "open"
        self.variables = ["x"]

    def _next_batch(self) -> list[str] | None:
        self.dataset.touch(child=True)
        self.fetches += 1
        if self.dataset.factory.block_cursor:
            self.dataset.factory.cursor_started.set()
            if not self.cancel.flag.wait(2):
                raise AssertionError("cursor cancellation was not acknowledged")
            self.status = "failed"
            raise NativeCancelled("cursor stopped")
        if self.cancel.cancelled:
            self.status = "failed"
            raise NativeCancelled("cursor stopped")
        if self.position == len(self.snapshot):
            self.status = "complete"
            return None
        end = min(self.position + self.batch_rows, len(self.snapshot))
        rows = self.snapshot[self.position : end]
        self.position = end
        if end == len(self.snapshot):
            self.status = "complete"
        return rows

    def stats(self) -> dict[str, Any]:
        self.dataset.touch(child=True)
        return {
            "status": self.status,
            "emittedRows": self.position,
            "rowsProduced": self.position,
        }

    def plan(self) -> dict[str, Any]:
        self.dataset.touch(child=True)
        return {"materializes": False}

    def close(self) -> None:
        self.dataset.touch(child=True)
        if self.status == "open":
            self.status = "stopped"


class NativeTransaction:
    def __init__(self, dataset: NativeDataset, cancel: Token) -> None:
        self.dataset, self.cancel = dataset, cancel
        self.staged = list(dataset.data)

    def query(self, _query: str, *, cancel: Token, **_kwargs: Any) -> list[str]:
        self.dataset.touch(child=True)
        if cancel.cancelled:
            raise NativeCancelled("transaction query stopped")
        return list(self.staged)

    def update(
        self, update: str, *, cancel: Token, **_kwargs: Any
    ) -> dict[str, object]:
        self.dataset.touch(child=True)
        if cancel.cancelled:
            raise NativeCancelled("transaction update stopped")
        self.staged.append(update)
        if update == "slow":
            self.dataset.factory.query_started.set()
            if not cancel.flag.wait(2):
                raise AssertionError("owned transaction cancellation was not observed")
            self.dataset.factory.query_stopped.set()
            raise NativeCancelled("staged update stopped")
        return {"staged": True}

    def commit(self) -> None:
        self.dataset.touch(child=True)
        self.dataset.writer = False
        if self.cancel.cancelled:
            raise NativeCancelled("commit stopped")
        self.dataset.data = self.staged
        self.dataset.factory.commit_published.set()
        if (
            self.dataset.factory.hold_commit
            and not self.dataset.factory.commit_release.wait(2)
        ):
            raise AssertionError("test did not release published commit")

    def rollback(self) -> None:
        self.dataset.touch(child=True)
        self.dataset.writer = False


def load(factory: Factory) -> Any:
    # A private package alias keeps the real installed sparkles module untouched.
    name = "_sparkles_async_facade_contract"
    for existing in list(sys.modules):
        if existing == name or existing.startswith(name + "."):
            del sys.modules[existing]
    package = types.ModuleType(name)
    source = Path(__file__).parents[1] / "python/sparkles"
    if not (source / "aio.py").is_file():
        installed = importlib.util.find_spec("sparkles")
        assert installed is not None and installed.origin is not None
        source = Path(installed.origin).parent
    package.__path__ = [str(source)]
    sys.modules[name] = package
    native = types.ModuleType(name + "._sparkles")
    native.CancelToken, native.Dataset = Token, factory.open
    sys.modules[native.__name__] = native
    errors = types.ModuleType(name + "._errors")
    errors.CancelledError, errors.InvalidInputError, errors.QueryTimeoutError = (
        NativeCancelled,
        NativeInvalid,
        NativeTimeout,
    )
    sys.modules[errors.__name__] = errors
    return importlib.import_module(name + ".aio")


async def event(value: threading.Event) -> None:
    if not await asyncio.to_thread(value.wait, 2):
        raise AssertionError("native fixture phase was not observed")


class FacadeTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self) -> None:
        self.factory = Factory()
        self.aio = load(self.factory)

    async def test_lazy_dataset_context_and_native_exception_identity(self) -> None:
        dataset = self.aio.Dataset()
        self.assertEqual(self.factory.datasets, [])
        async with dataset as ds:
            self.assertEqual(await ds.query("read"), ["one", "two", "three"])
            await ds.update("four")
            self.assertTrue(await ds.ask("read"))
            with self.assertRaises(OSError) as caught:
                await ds.query("fail")
            self.assertIs(caught.exception, self.factory.error)
        self.assertTrue(dataset.closed)
        self.assertNotIn(
            self.factory.datasets[0].name, [t.name for t in threading.enumerate()]
        )

    async def test_idle_cursor_retains_snapshot_worker_and_incremental_batches(
        self,
    ) -> None:
        dataset = self.aio.Dataset()
        cursor = await dataset.select_cursor("rows", batch_rows=2)
        native = self.factory.datasets[0]
        self.assertEqual(native.last_cursor.fetches, 0)
        await dataset.aclose()
        self.assertEqual(await anext(cursor), "one")
        self.assertEqual(native.last_cursor.fetches, 1)
        self.assertEqual(await anext(cursor), "two")
        self.assertEqual(native.last_cursor.fetches, 1)
        self.assertEqual([row async for row in cursor], ["three"])
        self.assertEqual(cursor.status, "complete")
        self.assertEqual((await cursor.stats())["emittedRows"], 3)
        self.assertNotIn(native.name, [t.name for t in threading.enumerate()])
        await cursor.aclose()
        await dataset.aclose()

    async def test_async_cursor_context_early_break_preserves_caller_token(
        self,
    ) -> None:
        async with self.aio.Dataset() as ds:
            caller = Token()
            cursor = await ds.select_cursor("rows", cancel=caller, batch_rows=2)
            async with cursor:
                async for row in cursor:
                    self.assertEqual(row, "one")
                    break
            self.assertEqual(cursor.status, "stopped")
            self.assertEqual((await cursor.stats())["emittedRows"], 1)
            self.assertFalse(caller.cancelled)
            with self.assertRaises(StopAsyncIteration):
                await anext(cursor)

    async def test_task_cancel_stops_query_without_mutating_caller_token(self) -> None:
        async with self.aio.Dataset() as ds:
            caller = Token()
            query = asyncio.create_task(ds.query("slow", cancel=caller))
            try:
                await event(self.factory.query_started)
                query.cancel()
                with self.assertRaises(asyncio.CancelledError):
                    await query
                self.assertTrue(self.factory.query_stopped.is_set())
                self.assertFalse(caller.cancelled)
                self.assertTrue(await ds.ask("read"))
            finally:
                query.cancel()
                await asyncio.gather(query, return_exceptions=True)

    async def test_transaction_rolls_back_body_error_and_retains_parent_close(
        self,
    ) -> None:
        ds = self.aio.Dataset()
        await ds.__aenter__()
        failure = ValueError("body failed")
        try:
            with self.assertRaises(ValueError) as caught:
                async with ds.transaction() as tx:
                    await tx.update("not committed")
                    raise failure
            self.assertIs(caught.exception, failure)
            self.assertEqual(await ds.query("read"), ["one", "two", "three"])
            tx = ds.transaction()
            await tx.__aenter__()
            await tx.update("retained")
            await ds.aclose()
            await tx.commit()
            self.assertEqual(self.factory.datasets[0].data[-1], "retained")
            self.assertFalse(self.factory.datasets[0].writer)
            self.assertNotIn(
                self.factory.datasets[0].name, [t.name for t in threading.enumerate()]
            )
        finally:
            await ds.aclose()

    async def test_failed_open_preserves_error_and_joins_executor(self) -> None:
        self.factory.fail_open = True
        ds = self.aio.Dataset()
        with self.assertRaises(OSError) as caught:
            await ds.__aenter__()
        self.assertIs(caught.exception, self.factory.error)
        self.assertTrue(ds.closed)
        self.assertTrue(ds._worker._closed)
        await ds.aclose()

    async def test_cancelled_open_closes_unreturned_native_and_joins(self) -> None:
        self.factory.block_open = True
        ds = self.aio.Dataset()
        opening = asyncio.create_task(ds.__aenter__())
        try:
            await event(self.factory.open_started)
            opening.cancel()
            await asyncio.sleep(0)
            self.assertFalse(opening.done())
            self.factory.open_release.set()
            with self.assertRaises(asyncio.CancelledError):
                await opening
            native = self.factory.datasets[0]
            self.assertTrue(native.closed)
            self.assertNotIn(native.name, [t.name for t in threading.enumerate()])
        finally:
            self.factory.open_release.set()
            await asyncio.gather(opening, return_exceptions=True)
            await ds.aclose()

    async def test_cancelled_open_waiter_does_not_close_independent_opener(
        self,
    ) -> None:
        self.factory.block_open = True
        ds = self.aio.Dataset()
        first = asyncio.create_task(ds.query("read"))
        second = None
        try:
            await event(self.factory.open_started)
            second = asyncio.create_task(ds.query("read"))
            await asyncio.sleep(0)
            self.assertEqual(len(ds._open_lock._waiters), 1)
            second.cancel()
            await asyncio.sleep(0)
            self.assertFalse(ds.closed)
            self.factory.open_release.set()
            with self.assertRaises(asyncio.CancelledError):
                await second
            self.assertEqual(await first, ["one", "two", "three"])
            self.assertEqual(await ds.query("read"), ["one", "two", "three"])
            self.assertFalse(ds._worker._closed)
        finally:
            self.factory.open_release.set()
            await asyncio.gather(
                first, *([second] if second else []), return_exceptions=True
            )
            await ds.aclose()
        self.assertTrue(ds._worker._closed)

    async def test_explicit_close_during_opening_closes_created_native(self) -> None:
        self.factory.block_open = True
        ds = self.aio.Dataset()
        opening = asyncio.create_task(ds.query("read"))
        closing = None
        try:
            await event(self.factory.open_started)
            closing = asyncio.create_task(ds.aclose())
            await asyncio.sleep(0)
            self.assertTrue(ds.closed)
            self.assertFalse(closing.done())
            self.factory.open_release.set()
            with self.assertRaises(NativeInvalid):
                await opening
            await closing
            self.assertTrue(self.factory.datasets[0].closed)
            self.assertTrue(ds._worker._closed)
        finally:
            self.factory.open_release.set()
            await asyncio.gather(
                opening, *([closing] if closing else []), return_exceptions=True
            )
            await ds.aclose()

    async def test_foreign_cursor_close_preserves_owner_cleanup(self) -> None:
        ds = self.aio.Dataset()
        cursor = await ds.select_cursor("rows")
        native = cursor._native
        try:
            with self.assertRaisesRegex(RuntimeError, "one event loop"):
                await asyncio.to_thread(lambda: asyncio.run(cursor.aclose()))
            self.assertFalse(cursor._closed)
            self.assertIsNone(cursor._close_task)
            self.assertIs(cursor._native, native)
            self.assertFalse(cursor._token.cancelled)
            self.assertEqual(ds._worker._owners, 2)
            self.assertEqual(await anext(cursor), "one")
            await cursor.aclose()
            self.assertEqual(native.status, "stopped")
            await ds.aclose()
            self.assertEqual(ds._worker._owners, 0)
            self.assertTrue(ds._worker._closed)
        finally:
            # Before the repair, the foreign close strands this fixture lease.
            # Explicit test-only cleanup keeps the failing regression finite.
            if not ds._worker._closed:
                await ds._worker.call(native.close, Token(), interruptible=False)
                await ds._worker.call(
                    self.factory.datasets[0].close, Token(), interruptible=False
                )
                while ds._worker._owners:
                    await ds._worker.release()

    async def test_foreign_dataset_close_preserves_owner_cleanup(self) -> None:
        ds = self.aio.Dataset()
        await ds.__aenter__()
        native = self.factory.datasets[0]
        try:
            with self.assertRaisesRegex(RuntimeError, "one event loop"):
                await asyncio.to_thread(lambda: asyncio.run(ds.aclose()))
            self.assertFalse(ds.closed)
            self.assertIsNone(ds._close_task)
            self.assertIs(ds._native, native)
            self.assertEqual(ds._worker._owners, 1)
            self.assertTrue(await ds.ask("read"))
            await ds.aclose()
            self.assertTrue(native.closed)
            self.assertTrue(ds._worker._closed)
        finally:
            if not ds._worker._closed:
                await ds._worker.call(native.close, Token(), interruptible=False)
                while ds._worker._owners:
                    await ds._worker.release()

    async def test_foreign_transaction_end_preserves_writer_and_owner_cleanup(
        self,
    ) -> None:
        ds = self.aio.Dataset()
        tx = ds.transaction()
        await tx.__aenter__()
        native = tx._native
        try:
            for method in (tx.commit, tx.rollback, tx.aclose):
                with self.assertRaisesRegex(RuntimeError, "one event loop"):
                    await asyncio.to_thread(lambda method=method: asyncio.run(method()))
                self.assertFalse(tx._ended)
                self.assertFalse(tx._ending)
                self.assertIsNone(tx._end_task)
                self.assertIs(tx._native, native)
                self.assertFalse(tx._lifetime.cancelled)
                self.assertTrue(self.factory.datasets[0].writer)
            self.assertEqual(await tx.query("read"), ["one", "two", "three"])
            await tx.rollback()
            self.assertFalse(self.factory.datasets[0].writer)
            await ds.aclose()
            self.assertEqual(ds._worker._owners, 0)
            self.assertTrue(ds._worker._closed)
        finally:
            if not ds._worker._closed:
                await ds._worker.call(native.rollback, Token(), interruptible=False)
                await ds._worker.call(
                    self.factory.datasets[0].close, Token(), interruptible=False
                )
                while ds._worker._owners:
                    await ds._worker.release()

    async def test_foreign_cursor_fast_paths_preserve_buffer_and_state(self) -> None:
        async with self.aio.Dataset() as ds:
            cursor = await ds.select_cursor("rows", batch_rows=2)
            try:
                self.assertEqual(await anext(cursor), "one")
                for method in (
                    cursor.__aenter__,
                    cursor.stats,
                    cursor.plan,
                    lambda: anext(cursor),
                ):
                    with self.assertRaisesRegex(RuntimeError, "one event loop"):
                        await asyncio.to_thread(
                            lambda method=method: asyncio.run(method())
                        )
                    self.assertEqual(list(cursor._pending), ["two"])
                    self.assertEqual(cursor._emitted, 1)
                    self.assertIsNone(cursor._error)
                    self.assertFalse(cursor._busy)
                self.assertEqual([row async for row in cursor], ["two", "three"])
                # Cached metadata and idempotent close still enforce loop ownership.
                for method in (cursor.stats, cursor.plan, cursor.aclose):
                    with self.assertRaisesRegex(RuntimeError, "one event loop"):
                        await asyncio.to_thread(
                            lambda method=method: asyncio.run(method())
                        )
            finally:
                await cursor.aclose()

    async def test_foreign_transaction_admission_does_not_consume_context(self) -> None:
        async with self.aio.Dataset() as ds:
            tx = ds.transaction()
            with self.assertRaisesRegex(RuntimeError, "one event loop"):
                await asyncio.to_thread(lambda: asyncio.run(tx.__aenter__()))
            self.assertFalse(tx._entered)
            self.assertFalse(tx._ended)
            self.assertFalse(self.factory.datasets[0].writer)
            with self.assertRaisesRegex(RuntimeError, "one event loop"):
                await asyncio.to_thread(lambda: asyncio.run(ds.__aenter__()))
            async with tx:
                for method in (
                    lambda: tx.query("read"),
                    lambda: tx.update("foreign"),
                ):
                    with self.assertRaisesRegex(RuntimeError, "one event loop"):
                        await asyncio.to_thread(
                            lambda method=method: asyncio.run(method())
                        )
                self.assertFalse(tx._lifetime.cancelled)
                self.assertEqual(await tx.query("read"), ["one", "two", "three"])
            self.assertNotIn("foreign", self.factory.datasets[0].data)
            with self.assertRaisesRegex(RuntimeError, "one event loop"):
                await asyncio.to_thread(lambda: asyncio.run(tx.aclose()))

    async def test_foreign_open_admission_does_not_touch_opening_owner(self) -> None:
        self.factory.block_open = True
        ds = self.aio.Dataset()
        first = asyncio.create_task(ds.query("read"))
        try:
            await event(self.factory.open_started)
            with self.assertRaisesRegex(RuntimeError, "one event loop"):
                await asyncio.to_thread(lambda: asyncio.run(ds.query("foreign")))
            self.assertFalse(ds.closed)
            self.assertFalse(ds._worker._closed)
            self.factory.open_release.set()
            self.assertEqual(await first, ["one", "two", "three"])
        finally:
            self.factory.open_release.set()
            await asyncio.gather(first, return_exceptions=True)
            await ds.aclose()

    async def test_dataset_close_cancels_active_query_and_joins(self) -> None:
        ds = self.aio.Dataset()
        await ds.__aenter__()
        query = asyncio.create_task(ds.query("slow"))
        try:
            await event(self.factory.query_started)
            await ds.aclose()
            with self.assertRaises(NativeCancelled):
                await query
            self.assertTrue(self.factory.query_stopped.is_set())
            self.assertTrue(self.factory.datasets[0].closed)
            self.assertTrue(ds._worker._closed)
        finally:
            query.cancel()
            await asyncio.gather(query, return_exceptions=True)
            await ds.aclose()

    async def test_task_cancelled_transaction_update_rolls_back_before_return(
        self,
    ) -> None:
        async with self.aio.Dataset() as ds:
            tx = ds.transaction()
            await tx.__aenter__()
            operation = asyncio.create_task(tx.update("slow"))
            try:
                await event(self.factory.query_started)
                operation.cancel()
                with self.assertRaises(asyncio.CancelledError):
                    await operation
                self.assertTrue(self.factory.query_stopped.is_set())
                self.assertFalse(self.factory.datasets[0].writer)
                self.assertNotIn("slow", self.factory.datasets[0].data)
                self.assertTrue(await ds.ask("read"))
            finally:
                operation.cancel()
                await asyncio.gather(operation, return_exceptions=True)
                await tx.aclose()

    async def test_cancelled_commit_still_ends_native_writer(self) -> None:
        async with self.aio.Dataset() as ds:
            caller = Token()
            tx = ds.transaction(cancel=caller)
            await tx.__aenter__()
            await tx.update("never published")
            caller.cancel()
            with self.assertRaises(NativeCancelled):
                await tx.commit()
            self.assertFalse(self.factory.datasets[0].writer)
            self.assertNotIn("never published", self.factory.datasets[0].data)
            self.assertTrue(await ds.ask("read"))

    async def test_commit_wins_task_cancel_after_parent_close_and_joins_worker(
        self,
    ) -> None:
        ds = self.aio.Dataset()
        await ds.__aenter__()
        tx = ds.transaction()
        await tx.__aenter__()
        await tx.update("published")
        await ds.aclose()
        self.factory.hold_commit = True
        commit = asyncio.create_task(tx.commit())
        try:
            await event(self.factory.commit_published)
            commit.cancel()
            await asyncio.sleep(0)
            self.assertFalse(commit.done())
            self.factory.commit_release.set()
            self.assertIsNone(await commit)
            self.assertFalse(commit.cancelled())
            if hasattr(commit, "cancelling"):
                self.assertEqual(commit.cancelling(), 0)
            self.assertEqual(self.factory.datasets[0].data[-1], "published")
            self.assertNotIn(
                self.factory.datasets[0].name, [t.name for t in threading.enumerate()]
            )
        finally:
            self.factory.commit_release.set()
            await asyncio.gather(commit, return_exceptions=True)
            await tx.aclose()
            await ds.aclose()

    async def test_close_during_batch_error_never_looks_like_successful_exhaustion(
        self,
    ) -> None:
        async with self.aio.Dataset() as ds:
            self.factory.block_cursor = True
            cursor = await ds.select_cursor("rows")
            fetch = asyncio.create_task(anext(cursor))
            try:
                await event(self.factory.cursor_started)
                await cursor.aclose()
                with self.assertRaises(NativeCancelled):
                    await fetch
                self.assertEqual(cursor.status, "failed")
                with self.assertRaises(NativeInvalid):
                    await anext(cursor)
            finally:
                fetch.cancel()
                await asyncio.gather(fetch, return_exceptions=True)
                await cursor.aclose()


if __name__ == "__main__":
    unittest.main()
