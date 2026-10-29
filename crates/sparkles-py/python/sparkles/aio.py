"""Async ownership and bounded cursors over Sparkles' existing native operations.

Use async context management or aclose for deterministic cleanup. Each Dataset owns
one pinned Python worker; native transactions keep their existing Rust worker.
"""

from __future__ import annotations

import asyncio
from collections import deque
from typing import Any, Generic, TypeVar, final

from ._aio_common import _deadline, _finish, _options, _token
from ._aio_transaction import Transaction
from ._aio_worker import _Worker
from ._errors import CancelledError, InvalidInputError
from ._sparkles import CancelToken
from ._sparkles import Dataset as _NativeDataset

_T = TypeVar("_T")
_C = TypeVar("_C", bound="_Cursor[Any]")
__all__ = ["Dataset", "GraphCursor", "QueryCursor", "Transaction"]


@final
class Dataset:
    """A lazy native dataset whose calls and final thread join stay off the loop."""

    def __init__(
        self,
        path: Any = None,
        *,
        union_default_graph: bool = False,
        group_commit: bool = True,
    ) -> None:
        self._arguments = (path, union_default_graph, group_commit)
        self._worker = _Worker(
            lambda: CancelledError("operation cancelled before native dispatch")
        )
        self._open_lock = asyncio.Lock()
        self._native: _NativeDataset | None = None
        self._closed = False
        self._close_task: asyncio.Task[None] | None = None

    @property
    def closed(self) -> bool:
        return self._closed

    async def _ensure_open(self) -> _NativeDataset:
        self._worker._bind()
        if self._closed:
            raise InvalidInputError("the dataset is closed")
        owns_open = False
        try:
            async with self._open_lock:
                if self._closed:
                    raise InvalidInputError("the dataset is closed")
                if self._native is None:
                    owns_open = True
                    path, union, group = self._arguments
                    native = await self._worker.call(
                        lambda: _NativeDataset(
                            path, union_default_graph=union, group_commit=group
                        ),
                        _token(),
                        discard=lambda value: value.close(),
                    )
                    if self._closed:
                        await self._worker.call(
                            native.close, _token(), interruptible=False
                        )
                        raise InvalidInputError("the dataset is closed")
                    self._native = native
                return self._native
        except BaseException:
            if owns_open and self._native is None:
                await self.aclose()
            raise

    async def __aenter__(self) -> Dataset:
        await self._ensure_open()
        return self

    async def __aexit__(self, *_args: object) -> bool:
        await self.aclose()
        return False

    async def _invoke(
        self,
        method: str,
        value: str,
        options: dict[str, Any],
        cancel: CancelToken | None,
        *,
        write: bool = False,
    ) -> Any:
        deadline = _deadline(options)
        native = await self._ensure_open()
        token = _token(cancel)
        return await self._worker.call(
            lambda: getattr(native, method)(
                value, **_options(options, token, deadline)
            ),
            token,
            write_completion=write,
        )

    async def query(
        self, query: str, *, cancel: CancelToken | None = None, **options: Any
    ) -> Any:
        return await self._invoke("query", query, options, cancel)

    async def select(
        self, query: str, *, cancel: CancelToken | None = None, **options: Any
    ) -> Any:
        return await self._invoke("select", query, options, cancel)

    async def ask(
        self, query: str, *, cancel: CancelToken | None = None, **options: Any
    ) -> bool:
        return await self._invoke("ask", query, options, cancel)

    async def update(
        self, update: str, *, cancel: CancelToken | None = None, **options: Any
    ) -> Any:
        return await self._invoke("update", update, options, cancel, write=True)

    async def _cursor(
        self,
        method: str,
        query: str,
        options: dict[str, Any],
        cancel: CancelToken | None,
        cursor_type: type[_C],
    ) -> _C:
        deadline = _deadline(options)
        native = await self._ensure_open()
        token = _token(cancel)
        self._worker.retain()

        def create() -> tuple[Any, dict[str, Any], dict[str, Any], Any]:
            cursor = getattr(native, method)(
                query, **_options(options, token, deadline)
            )
            try:
                return (
                    cursor,
                    cursor.stats(),
                    cursor.plan(),
                    getattr(cursor, "variables", None),
                )
            except BaseException:
                cursor.close()
                raise

        try:
            values = await self._worker.call(
                create, token, discard=lambda values: values[0].close()
            )
            return cursor_type(self._worker, token, *values)
        except BaseException:
            await self._worker.release()
            raise

    async def select_cursor(
        self, query: str, *, cancel: CancelToken | None = None, **options: Any
    ) -> QueryCursor:
        return await self._cursor("select_cursor", query, options, cancel, QueryCursor)

    async def graph_cursor(
        self, query: str, *, cancel: CancelToken | None = None, **options: Any
    ) -> GraphCursor:
        return await self._cursor("graph_cursor", query, options, cancel, GraphCursor)

    def transaction(
        self, *, cancel: CancelToken | None = None, timeout: float | None = None
    ) -> Transaction:
        return Transaction(self, cancel=cancel, timeout=timeout)

    async def aclose(self) -> None:
        self._worker._bind()
        if self._close_task is None:
            self._closed = True
            self._worker.cancel_calls()
            self._close_task = asyncio.create_task(self._close())
        await _finish(self._close_task)

    async def _close(self) -> None:
        try:
            async with self._open_lock:
                native, self._native = self._native, None
                if native is not None:
                    await self._worker.call(native.close, _token(), interruptible=False)
        finally:
            await self._worker.release()


class _Cursor(Generic[_T]):
    def __init__(
        self,
        worker: _Worker,
        token: CancelToken,
        native: Any,
        stats: dict[str, Any],
        plan: dict[str, Any],
        variables: Any,
    ) -> None:
        self._worker, self._token, self._native = worker, token, native
        self._stats, self._plan, self._variables = stats, plan, variables
        self._pending: deque[_T] = deque()
        self._emitted = 0
        self._closed = False
        self._busy = False
        self._error: BaseException | None = None
        self._close_task: asyncio.Task[None] | None = None

    @property
    def status(self) -> str:
        if self._error is not None:
            return "failed"
        status = self._stats["status"]
        return "open" if status == "complete" and self._pending else status

    def __aiter__(self) -> _Cursor[_T]:
        return self

    async def __aenter__(self) -> _Cursor[_T]:
        self._worker._bind()
        return self

    async def __aexit__(self, *_args: object) -> bool:
        await self.aclose()
        return False

    async def __anext__(self) -> _T:
        self._worker._bind()
        if self._error is not None:
            raise InvalidInputError("the cursor failed") from self._error
        if self._closed:
            raise StopAsyncIteration
        if self._busy:
            raise InvalidInputError("a cursor operation is in progress")
        self._busy = True
        try:
            if not self._pending:
                native = self._native

                def fetch() -> tuple[Any, dict[str, Any], dict[str, Any]]:
                    rows = native._next_batch()
                    return rows, native.stats(), native.plan()

                rows, self._stats, self._plan = await self._worker.call(
                    fetch, self._token
                )
                if rows is None:
                    self._busy = False
                    await self.aclose()
                    raise StopAsyncIteration
                if self._closed:
                    await self.aclose()
                    raise StopAsyncIteration
                self._pending.extend(rows)
                if not self._pending:
                    raise InvalidInputError(
                        "an empty native batch is not end of stream"
                    )
            result = self._pending.popleft()
            self._emitted += 1
            return result
        except StopAsyncIteration:
            raise
        except BaseException as error:
            self._error = error
            try:
                await self.aclose()
            except BaseException as cleanup:
                raise error from cleanup
            raise
        finally:
            self._busy = False

    async def stats(self) -> dict[str, Any]:
        self._worker._bind()
        if self._busy:
            raise InvalidInputError("a cursor operation is in progress")
        if not self._closed:
            self._stats = await self._worker.call(
                self._native.stats, _token(), interruptible=False
            )
        return dict(self._stats, status=self.status, emittedRows=self._emitted)

    async def plan(self) -> dict[str, Any]:
        self._worker._bind()
        if self._busy:
            raise InvalidInputError("a cursor operation is in progress")
        if not self._closed:
            self._plan = await self._worker.call(
                self._native.plan, _token(), interruptible=False
            )
        return dict(self._plan)

    async def aclose(self) -> None:
        self._worker._bind()
        if self._close_task is None:
            self._closed = True
            self._token.cancel()
            self._close_task = asyncio.create_task(self._close())
        await _finish(self._close_task)

    async def _close(self) -> None:
        pending = bool(self._pending) or self._busy
        native = self._native
        try:

            def close() -> tuple[dict[str, Any], dict[str, Any]]:
                native.close()
                return native.stats(), native.plan()

            self._stats, self._plan = await self._worker.call(
                close, _token(), interruptible=False
            )
            if pending and self._stats["status"] == "complete":
                self._stats = dict(self._stats, status="stopped")
        finally:
            self._pending.clear()
            self._native = None
            await self._worker.release()


@final
class QueryCursor(_Cursor[Any]):
    @property
    def variables(self) -> list[Any]:
        return list(self._variables)


@final
class GraphCursor(_Cursor[Any]):
    pass
