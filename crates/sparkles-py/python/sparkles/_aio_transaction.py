"""Async transaction ownership over the existing native transaction worker."""

from __future__ import annotations

import asyncio
from typing import Any, Protocol, final

from ._aio_common import _deadline, _finish, _options, _remaining, _token
from ._aio_worker import (
    _cancel_count,
    _consume_new_cancellations,
    _owned_wait,
    _settle,
    _Worker,
)
from ._errors import CancelledError, InvalidInputError
from ._sparkles import CancelToken


class _DatasetOwner(Protocol):
    """Internal ownership requirements, independent of the public Dataset stub."""

    _worker: _Worker

    async def _ensure_open(self) -> Any: ...


@final
class Transaction:
    """A lazy async context manager; errors roll back, normal exit commits."""

    def __init__(
        self,
        dataset: _DatasetOwner,
        *,
        cancel: CancelToken | None = None,
        timeout: float | None = None,
    ) -> None:
        self._dataset = dataset
        self._worker = dataset._worker
        self._lifetime = _token(cancel)
        self._timeout = timeout
        self._deadline: float | None = None
        self._native: Any = None
        self._entered = False
        self._ended = False
        self._ending = False
        self._end_done: asyncio.Future[None] | None = None
        self._end_task: asyncio.Task[None] | None = None
        self._commit_succeeded = False

    async def __aenter__(self) -> Transaction:
        if self._entered or self._ended:
            raise InvalidInputError(
                "the async transaction was already entered or ended"
            )
        self._entered = True
        self._deadline = _deadline({"timeout": self._timeout})
        native = await self._dataset._ensure_open()
        self._worker.retain()
        try:

            def begin() -> Any:
                return native._transaction(
                    self._lifetime, timeout=_remaining(self._deadline)
                )

            transaction = await self._worker.call(
                begin, self._lifetime, discard=lambda value: value.rollback()
            )
            if self._lifetime.cancelled:
                await self._worker.call(
                    transaction.rollback, _token(), interruptible=False
                )
                raise CancelledError("transaction admission was cancelled")
            self._native = transaction
            return self
        except BaseException:
            self._ended = True
            await self._worker.release()
            raise

    async def __aexit__(
        self, exc_type: type[BaseException] | None, *_args: object
    ) -> bool:
        if not self._ended:
            if exc_type is None:
                await self.commit()
            else:
                await self.rollback()
        return False

    def _require_open(self) -> Any:
        if self._native is None or self._ended:
            raise InvalidInputError("the transaction has ended or was not entered")
        if self._ending:
            raise InvalidInputError("the transaction is ending")
        return self._native

    async def _invoke(
        self,
        method: str,
        value: str,
        options: dict[str, Any],
        cancel: CancelToken | None,
    ) -> Any:
        native = self._require_open()
        deadline = _deadline(options, self._deadline)
        token = _token(cancel)._with_cancel(self._lifetime)
        try:
            return await self._worker.call(
                lambda: getattr(native, method)(
                    value, **_options(options, token, deadline)
                ),
                token,
            )
        except asyncio.CancelledError as cancelled:
            self._lifetime.cancel()
            try:
                await self.aclose()
            except BaseException as cleanup:
                raise cancelled from cleanup
            raise

    async def query(
        self, query: str, *, cancel: CancelToken | None = None, **options: Any
    ) -> Any:
        return await self._invoke("query", query, options, cancel)

    async def update(
        self, update: str, *, cancel: CancelToken | None = None, **options: Any
    ) -> Any:
        return await self._invoke("update", update, options, cancel)

    async def _end(self, commit: bool) -> None:
        native = self._require_open()
        self._ending = True
        self._end_done = asyncio.get_running_loop().create_future()
        self._end_task = asyncio.create_task(self._end_native(native, commit))
        task = asyncio.current_task()
        baseline = _cancel_count(task)
        try:
            await _owned_wait(self._end_task)
        except asyncio.CancelledError as cancelled:
            self._lifetime.cancel()
            try:
                await _settle(self._end_task, self._lifetime.cancel)
            except BaseException as error:
                if not (
                    commit
                    and self._commit_succeeded
                    and isinstance(error, asyncio.CancelledError)
                ):
                    raise cancelled from error
            if commit and self._commit_succeeded:
                _consume_new_cancellations(task, baseline)
                return
            raise

    async def _end_native(self, native: Any, commit: bool) -> None:
        try:
            if commit:
                await self._worker.call(
                    native.commit,
                    self._lifetime,
                    write_completion=True,
                    # Native commit also ends/joins the transaction on refusal.
                    # A Python precheck would leave only asynchronous Drop rollback.
                    admit_cancelled=True,
                )
                self._commit_succeeded = True
            else:
                await self._worker.call(native.rollback, _token(), interruptible=False)
        finally:
            self._native = None
            self._ended = True
            try:
                await self._worker.release()
            finally:
                assert self._end_done is not None
                self._end_done.set_result(None)

    async def commit(self) -> None:
        await self._end(True)

    async def rollback(self) -> None:
        await self._end(False)

    async def aclose(self) -> None:
        if self._ended:
            return
        if self._ending:
            self._lifetime.cancel()
            assert self._end_done is not None
            await _finish(self._end_done)
        elif self._native is not None:
            await self.rollback()
        else:
            self._ended = True
