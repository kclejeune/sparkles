"""Real native async contracts. Run against the newly built extension/package.

The fake-backend tests establish controlled races separately. These cases exercise
actual native resources, terms, private batch transport and transaction rollback.
"""

from __future__ import annotations

import asyncio
import threading

import pytest
from sparkles import (
    CancelledError,
    CancelToken,
    InvalidInputError,
    QueryTimeoutError,
    SparqlSyntaxError,
    Variable,
    aio,
)


async def assert_no_owned_threads() -> None:
    # Explicit aclose/transaction end joins the owned executor before returning.
    assert not [
        t.name for t in threading.enumerate() if t.name.startswith("sparkles-aio-")
    ]


def test_native_async_context_query_update_and_exception_identity() -> None:
    async def run() -> None:
        async with aio.Dataset() as ds:
            await ds.update('INSERT DATA { <urn:s> <urn:p> "é"@fr }')
            assert await ds.ask("ASK { <urn:s> <urn:p> ?o }")
            rows = await ds.select("SELECT ?o { <urn:s> <urn:p> ?o }")
            assert str(next(iter(rows))["o"]) == '"é"@fr'
            with pytest.raises(SparqlSyntaxError):
                await ds.query("not SPARQL")
        assert ds.closed
        await ds.aclose()
        with pytest.raises(InvalidInputError):
            await ds.ask("ASK {}")
        await assert_no_owned_threads()

    asyncio.run(run())


def test_native_async_cursor_retained_origin_and_bounded_delivery() -> None:
    async def run() -> None:
        ds = aio.Dataset()
        await ds.update(
            "INSERT DATA { <urn:s1> <urn:p> 1 . <urn:s2> <urn:p> 2 . <urn:s3> <urn:p> 3 }"
        )
        cursor = await ds.select_cursor(
            "SELECT ?s ?o { ?s <urn:p> ?o }", batch_rows=1, batch_bytes=128
        )
        assert cursor.variables == [Variable("s"), Variable("o")]
        assert (await cursor.stats())["rowsProduced"] == 0
        await ds.update("INSERT DATA { <urn:s4> <urn:p> 4 }")
        await ds.aclose()
        assert len([row async for row in cursor]) == 3
        assert cursor.status == "complete"
        assert (await cursor.stats())["emittedRows"] == 3
        await cursor.aclose()
        await assert_no_owned_threads()

    asyncio.run(run())


def test_native_async_context_break_and_graph_cursor() -> None:
    async def run() -> None:
        async with aio.Dataset() as ds:
            await ds.update("INSERT DATA { <urn:s1> <urn:p> 1 . <urn:s2> <urn:p> 2 }")
            caller = CancelToken()
            cursor = await ds.select_cursor(
                "SELECT * { ?s ?p ?o }", batch_rows=2, cancel=caller
            )
            async with cursor:
                async for _row in cursor:
                    break
            assert cursor.status == "stopped"
            assert not caller.cancelled
            assert (await cursor.stats())["emittedRows"] == 1
            graph = await ds.graph_cursor(
                "CONSTRUCT { ?s <urn:q> ?o } WHERE { ?s <urn:p> ?o }", batch_rows=1
            )
            async with graph:
                quads = [quad async for quad in graph]
            assert len(quads) == 2
            assert {str(quad.predicate) for quad in quads} == {"<urn:q>"}
        await assert_no_owned_threads()

    asyncio.run(run())


def test_native_external_precancel_does_not_poison_other_calls() -> None:
    async def run() -> None:
        caller = CancelToken()
        caller.cancel()
        async with aio.Dataset() as ds:
            with pytest.raises(CancelledError):
                await ds.ask("ASK {}", cancel=caller)
            assert await ds.ask("ASK {}")
        await assert_no_owned_threads()

    asyncio.run(run())


def test_native_async_transaction_rollback_commit_and_retained_parent_close(
    tmp_path,
) -> None:
    async def run() -> None:
        path = tmp_path / "async-native-db"
        ds = aio.Dataset(path)
        await ds.__aenter__()
        try:
            failure = ValueError("rollback this body")
            with pytest.raises(ValueError) as caught:
                async with ds.transaction() as tx:
                    await tx.update("INSERT DATA { <urn:rolled> <urn:p> 1 }")
                    raise failure
            assert caught.value is failure
            assert not await ds.ask("ASK { <urn:rolled> ?p ?o }")
            tx = ds.transaction()
            await tx.__aenter__()
            await tx.update("INSERT DATA { <urn:kept> <urn:p> 1 }")
            await ds.aclose()
            assert await tx.query("ASK { <urn:kept> ?p ?o }") is True
            await tx.commit()
            await tx.aclose()
            await assert_no_owned_threads()
            async with aio.Dataset(path) as reopened:
                assert await reopened.ask("ASK { <urn:kept> ?p ?o }")
                assert not await reopened.ask("ASK { <urn:rolled> ?p ?o }")
        finally:
            await ds.aclose()

    asyncio.run(run())


def test_native_async_transaction_precancel_does_not_leave_a_writer() -> None:
    async def run() -> None:
        caller = CancelToken()
        caller.cancel()
        async with aio.Dataset() as ds:
            with pytest.raises(CancelledError):
                async with ds.transaction(cancel=caller):
                    pytest.fail("cancelled admission entered its body")
            await ds.update("INSERT DATA { <urn:after> <urn:p> 1 }")
            assert await ds.ask("ASK { <urn:after> ?p ?o }")
        await assert_no_owned_threads()

    asyncio.run(run())


def test_native_private_batch_transport_preserves_pending_rows() -> None:
    # The async bridge must also respect a native row consumed before batch fetch.
    from sparkles import Dataset

    with Dataset() as ds:
        cursor = ds.select_cursor("SELECT ?x { VALUES ?x { 1 2 3 } }", batch_rows=3)
        assert next(cursor)["x"].value == "1"
        assert [row["x"].value for row in cursor._next_batch()] == ["2", "3"]
        assert cursor._next_batch() is None
        assert cursor.status == "complete"
        cursor.close()


def test_native_private_controlled_admission_refuses_a_cancelled_token() -> None:
    from sparkles import Dataset

    with Dataset() as ds:
        token = CancelToken()
        token.cancel()
        try:
            transaction = ds._transaction(token)
        except CancelledError:
            pass
        else:
            transaction.rollback()
            pytest.fail("controlled native admission ignored cancellation")
        ds.update("INSERT DATA { <urn:after> <urn:p> 1 }")
        assert ds.ask("ASK { <urn:after> ?p ?o }")


def test_native_cancelled_commit_joins_writer_before_database_reopen(tmp_path) -> None:
    async def run() -> None:
        path = tmp_path / "cancelled-commit-db"
        ds = aio.Dataset(path)
        await ds.__aenter__()
        caller = CancelToken()
        tx = ds.transaction(cancel=caller)
        await tx.__aenter__()
        await tx.update("INSERT DATA { <urn:never> <urn:p> 1 }")
        await ds.aclose()
        caller.cancel()
        with pytest.raises(CancelledError):
            await tx.commit()
        await tx.aclose()
        await assert_no_owned_threads()
        async with aio.Dataset(path) as reopened:
            assert not await reopened.ask("ASK { <urn:never> ?p ?o }")
            await reopened.update("INSERT DATA { <urn:next> <urn:p> 1 }")
        await assert_no_owned_threads()

    asyncio.run(run())


def test_native_controlled_admission_times_out_while_another_writer_is_held() -> None:
    async def run() -> None:
        async with aio.Dataset() as ds:
            native = await ds._ensure_open()
            # The separate caller owns the existing synchronous writer. The async
            # transaction then reaches the real native admission path on its worker.
            holder = await asyncio.to_thread(native.transaction)
            try:
                with pytest.raises(QueryTimeoutError):
                    await asyncio.wait_for(
                        ds.transaction(timeout=0.05).__aenter__(), timeout=2
                    )
                # Refusal must not release or alter the independent owner.
                assert await asyncio.to_thread(holder.query, "ASK {}") is True
            finally:
                await asyncio.to_thread(holder.rollback)
            async with ds.transaction() as tx:
                await tx.update("INSERT DATA { <urn:after-wait> <urn:p> 1 }")
            assert await ds.ask("ASK { <urn:after-wait> ?p ?o }")
        await assert_no_owned_threads()

    asyncio.run(run())
