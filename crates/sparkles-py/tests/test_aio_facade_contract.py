"""Stdlib fake-backend contracts; these are not native or installed-wheel evidence."""

from __future__ import annotations

import asyncio
import importlib
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
    package.__path__ = [str(Path(__file__).parents[1] / "python/sparkles")]
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
