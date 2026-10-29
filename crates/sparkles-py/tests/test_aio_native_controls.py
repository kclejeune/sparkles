"""Real-native cancellation at observed admission and publication boundaries.

Publication barriers delegate to the actual native operation before pausing its
Python acknowledgement. They do not replace the engine or add an engine hook.
"""

from __future__ import annotations

import asyncio
import threading
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import pytest
from sparkles import CancelledError, CancelToken, Dataset, aio


def transaction_threads() -> set[str]:
    tasks = Path("/proc/self/task")
    if not tasks.is_dir():
        pytest.skip("native admission phase observation requires Linux procfs")
    result = set()
    for task in tasks.iterdir():
        try:
            if (task / "comm").read_text().strip() == "sparkles-txn":
                result.add(task.name)
        except FileNotFoundError:
            pass  # A different completed native worker may disappear during scanning.
    return result


async def new_transaction_thread(before: set[str]) -> set[str]:
    loop = asyncio.get_running_loop()
    until = loop.time() + 3
    while loop.time() < until:
        if created := transaction_threads() - before:
            return created
        await asyncio.sleep(0)
    pytest.fail("the real native admission worker was not observed")


async def phase(value: threading.Event) -> None:
    assert await asyncio.to_thread(value.wait, 3), "native phase was not observed"


def assert_no_async_workers() -> None:
    assert not [
        t.name for t in threading.enumerate() if t.name.startswith("sparkles-aio-")
    ]


def cancellation_failure(error: asyncio.CancelledError) -> BaseException | None:
    # CPython 3.10 transports a cancelled Task's original exception through an
    # implicit CancelledError context. Newer versions expose its cause directly.
    seen = {id(error)}
    while error.__cause__ is None:
        context = error.__context__
        if not isinstance(context, asyncio.CancelledError) or id(context) in seen:
            break
        seen.add(id(context))
        error = context
    return error.__cause__


class AdmissionFailure:
    """Record the actual native admission failure without replacing its operation."""

    def __init__(self, native) -> None:
        self.native = native
        self.failure: BaseException | None = None

    def __getattr__(self, name):
        return getattr(self.native, name)

    def _transaction(self, *args, **kwargs):
        try:
            return self.native._transaction(*args, **kwargs)
        except BaseException as error:
            self.failure = error
            raise


def test_cancelled_task_keeps_native_failure_identity() -> None:
    async def run() -> None:
        native_error = CancelledError("native operation stopped")
        entered = asyncio.Event()

        async def operation() -> None:
            try:
                entered.set()
                await asyncio.Event().wait()
            except asyncio.CancelledError as cancelled:
                raise cancelled from native_error

        task = asyncio.create_task(operation())
        await entered.wait()
        task.cancel()
        done, _ = await asyncio.wait({task}, timeout=3)
        assert task in done
        with pytest.raises(asyncio.CancelledError) as caught:
            await task
        assert cancellation_failure(caught.value) is native_error

    asyncio.run(run())


def test_cancellation_failure_refuses_missing_unrelated_or_cyclic_context() -> None:
    missing = asyncio.CancelledError()
    assert cancellation_failure(missing) is None
    unrelated = asyncio.CancelledError()
    context = RuntimeError("unrelated context")
    context.__cause__ = CancelledError("must not be accepted")
    unrelated.__context__ = context
    assert cancellation_failure(unrelated) is None
    cyclic = asyncio.CancelledError()
    cyclic.__context__ = cyclic
    assert cancellation_failure(cyclic) is None


class Published:
    """Delegate every real operation, delaying only its successful acknowledgement."""

    def __init__(self, native, method: str) -> None:
        self.native, self.method = native, method
        self.published, self.release = threading.Event(), threading.Event()

    def __getattr__(self, name):
        method = getattr(self.native, name)
        if name != self.method:
            return method

        def call(*args, **kwargs):
            result = method(*args, **kwargs)
            self.published.set()
            assert self.release.wait(5), "test did not release the published result"
            return result

        return call


def test_native_task_cancel_during_observed_writer_admission(tmp_path) -> None:
    async def run() -> None:
        transaction_threads()  # Refuse unsupported observation before owning resources.
        path = tmp_path / "admission"
        ds = aio.Dataset(path)
        native = await ds._ensure_open()
        admission = AdmissionFailure(native)
        ds._native = admission
        before_head = native.head_commit.seq
        after_head = None
        loop = asyncio.get_running_loop()
        holder_thread = ThreadPoolExecutor(
            max_workers=1, thread_name_prefix="aio-test-holder"
        )
        holder = await loop.run_in_executor(holder_thread, native.transaction)
        pending = None
        caller = CancelToken()
        baseline = transaction_threads()
        try:
            pending = asyncio.create_task(ds.transaction(cancel=caller).__aenter__())
            created = await new_transaction_thread(baseline)
            assert not pending.done()
            pending.cancel()
            done, _ = await asyncio.wait({pending}, timeout=3)
            assert pending in done, "cancellation waited for the unrelated holder"
            with pytest.raises(asyncio.CancelledError) as caught:
                await pending
            assert isinstance(admission.failure, CancelledError)
            assert cancellation_failure(caught.value) is admission.failure
            assert not caller.cancelled
            assert not (transaction_threads() & created)
            # The independent holder is still live after cancellation settled.
            assert (
                await loop.run_in_executor(holder_thread, holder.query, "ASK {}")
                is True
            )
        finally:
            # Release the holder even when a failed assertion exposes bad cancellation.
            await loop.run_in_executor(holder_thread, holder.rollback)
            if pending is not None:
                pending.cancel()
                await asyncio.gather(pending, return_exceptions=True)
            after_head = native.head_commit.seq
            await asyncio.to_thread(holder_thread.shutdown, True)
            await ds.aclose()
        assert after_head == before_head
        assert_no_async_workers()
        async with aio.Dataset(path) as reopened:
            async with reopened.transaction() as tx:
                await tx.update("INSERT DATA { <urn:after-cancel> <urn:p> 1 }")
            assert await reopened.ask("ASK { <urn:after-cancel> <urn:p> 1 }")
        assert_no_async_workers()

    asyncio.run(run())


@pytest.mark.parametrize("kind", ["update", "commit"])
def test_native_published_result_wins_task_cancel_before_acknowledgement(
    tmp_path, kind
) -> None:
    async def run() -> None:
        path = tmp_path / kind
        ds = aio.Dataset(path)
        native = await ds._ensure_open()
        before_head = native.head_commit.seq
        caller = CancelToken()
        tx = None
        if kind == "commit":
            tx = ds.transaction(cancel=caller)
            await tx.__aenter__()
            await tx.update("INSERT DATA { <urn:published> <urn:p> 1 }")
            barrier = Published(tx._native, "commit")
            tx._native = barrier
            task = asyncio.create_task(tx.commit())
        else:
            barrier = Published(native, "update")
            ds._native = barrier
            task = asyncio.create_task(
                ds.update("INSERT DATA { <urn:published> <urn:p> 1 }", cancel=caller)
            )
        try:
            await phase(barrier.published)
            published_head = native.head_commit.seq
            assert published_head == before_head + 1
            # This reads the actual committed store while acknowledgement is held.
            assert await asyncio.to_thread(
                native.ask, "ASK { <urn:published> <urn:p> 1 }"
            )
            task.cancel()
            await asyncio.sleep(0)
            assert not task.done()
            barrier.release.set()
            result = await task
            if kind == "commit":
                assert result is None
            else:
                assert result.inserted == 1 and result.deleted == 0
            assert not task.cancelled()
            if hasattr(task, "cancelling"):
                assert task.cancelling() == 0
            assert not caller.cancelled
            assert native.head_commit.seq == published_head
        finally:
            barrier.release.set()
            await asyncio.gather(task, return_exceptions=True)
            if tx is not None:
                await tx.aclose()
            await ds.aclose()
        assert_no_async_workers()
        # Closing the final real native owner permits immediate persistent reopen.
        with Dataset(path) as reopened:
            assert reopened.head_commit.seq == before_head + 1
            assert reopened.ask("ASK { <urn:published> <urn:p> 1 }")

    asyncio.run(run())
