"""Bounded, fallible engine cursors alongside the collected query API."""

from __future__ import annotations

import json

import pytest
from sparkles import (
    CancelledError,
    CancelToken,
    ConflictError,
    Dataset,
    InvalidInputError,
    QueryCursor,
    Variable,
)


def test_cursor_snapshot_close_and_variables():
    ds = Dataset()
    ds.load("<urn:s1> <urn:p> 1 . <urn:s2> <urn:p> 2 .", "ttl")
    q = "SELECT ?s ?o WHERE { ?s <urn:p> ?o }"
    expected = sorted(tuple(map(str, row)) for row in ds.select(q))
    cursor = ds.select_cursor(q, batch_rows=1, allow_materialization=False)
    assert isinstance(cursor, QueryCursor)
    assert cursor.variables == [Variable("s"), Variable("o")]
    assert cursor.stats()["rowsProduced"] == 0
    assert not cursor.plan()["materializes"]
    ds.update("INSERT DATA { <urn:s3> <urn:p> 3 }")
    ds.close()
    assert sorted(tuple(map(str, row)) for row in cursor) == expected
    assert cursor.status == "complete"
    assert cursor.stats()["emittedRows"] == 2
    cursor.close()
    assert cursor.status == "complete"


def test_cursor_close_context_cancel_and_fused_error():
    ds = Dataset()
    ds.load("<urn:s1> <urn:p> 1 . <urn:s2> <urn:p> 2 .", "ttl")
    cancel = CancelToken()
    with ds.select_cursor(
        "SELECT * WHERE { ?s ?p ?o }", batch_rows=1, cancel=cancel
    ) as cursor:
        next(cursor)
    assert cursor.status == "stopped"
    assert list(cursor) == []
    assert not cancel.cancelled
    cursor = ds.select_cursor(
        "SELECT * WHERE { ?s ?p ?o }", batch_rows=1, cancel=cancel
    )
    cancel.cancel()
    with pytest.raises(CancelledError):
        next(cursor)
    assert cursor.status == "failed"
    # A failed cursor keeps failing rather than ending like a complete one.
    with pytest.raises(InvalidInputError, match="cursor failed"):
        next(cursor)
    with pytest.raises(InvalidInputError, match="cursor failed"):
        list(cursor)
    cursor.close()
    assert cursor.status == "failed"


def test_cursor_serialization_and_transaction_guard():
    ds = Dataset()
    ds.load("<urn:s1> <urn:p> 1 . <urn:s2> <urn:p> 2 .", "ttl")
    q = "SELECT * WHERE { ?s ?p ?o }"
    for fmt in ["json", "xml", "csv", "tsv"]:
        assert ds.select_cursor(q, batch_rows=1).serialize(format=fmt) == ds.select(
            q
        ).serialize(format=fmt)
    cursor = ds.select_cursor(q, batch_rows=1)
    body = cursor.serialize()
    assert len(json.loads(body)["results"]["bindings"]) == 2
    assert cursor.status == "complete"
    with pytest.raises(InvalidInputError):
        cursor.serialize()
    cursor = ds.select_cursor(q)
    next(cursor)
    with pytest.raises(InvalidInputError):
        cursor.serialize()
    cursor.close()
    with ds.transaction(), pytest.raises(ConflictError, match="open transaction"):
        ds.select_cursor(q)


def test_cursor_pending_batch_is_open_until_delivered_or_closed():
    ds = Dataset()
    cursor = ds.select_cursor("SELECT ?x { VALUES ?x { 1 2 3 } }", batch_rows=4096)
    next(cursor)
    assert cursor.status == "open"
    assert cursor.stats()["status"] == "open"
    cursor.close()
    assert cursor.status == "stopped"
    assert list(cursor) == []


def test_graph_cursor_snapshot_dedup_pending_close_and_formats():
    from sparkles import GraphCursor

    ds = Dataset()
    ds.load("<urn:s1> <urn:p> 1 . <urn:s2> <urn:p> 2 .", "ttl")
    q = "CONSTRUCT { ?s <urn:q> ?o } WHERE { ?s <urn:p> ?o }"
    for fmt in ["nq", "nt", "turtle", "trix", "rdf-json", "rdf-protobuf"]:
        c = ds.graph_cursor(q, batch_rows=1)
        assert c.serialize(format=fmt)
        assert c.status == "complete"
    c = ds.graph_cursor(q, batch_rows=1, allow_materialization=False)
    assert isinstance(c, GraphCursor)
    ds.update("INSERT DATA { <urn:s3> <urn:p> 3 }")
    assert len(list(c)) == 2
    c = ds.graph_cursor(q)
    next(c)
    c.close()
    assert c.status == "stopped"
    with ds.transaction(), pytest.raises(ConflictError):
        ds.graph_cursor(q)
    c = ds.graph_cursor(q, batch_rows=1)
    ds.close()
    assert len(list(c)) == 3


def test_cursor_serialization_to_a_bad_path_fails_the_cursor(tmp_path):
    ds = Dataset()
    ds.load("<urn:s1> <urn:p> 1 . <urn:s2> <urn:p> 2 .", "ttl")
    bad = tmp_path / "missing" / "out.json"
    for cursor in [
        ds.select_cursor("SELECT * WHERE { ?s ?p ?o }", batch_rows=1),
        ds.graph_cursor("CONSTRUCT WHERE { ?s ?p ?o }", batch_rows=1),
    ]:
        with pytest.raises(OSError):
            cursor.serialize(bad)
        assert cursor.status == "failed"
        stats = cursor.stats()
        assert stats["status"] == "failed"
        assert stats["error"]
        assert stats["emittedRows"] == 0
        with pytest.raises(InvalidInputError, match="cursor failed"):
            next(cursor)
        cursor.close()
        assert cursor.status == "failed"


def _numbers(n):
    return " ".join(f"<urn:s{i}> <urn:p> {i} ." for i in range(n))


def test_cursor_rows_decoded_ahead_count_when_handed_out():
    ds = Dataset()
    ds.load(_numbers(1000), "ttl")
    q = "SELECT ?s ?o WHERE { ?s <urn:p> ?o }"
    expected = sorted(tuple(map(str, row)) for row in ds.select(q))
    # one batch holds every row, and a refill decodes 256 of them at a time
    cursor = ds.select_cursor(q)
    rows = [next(cursor) for _ in range(3)]
    assert cursor.stats()["emittedRows"] == 3
    assert cursor.status == "open"
    rows += list(cursor)
    assert sorted(tuple(map(str, row)) for row in rows) == expected
    assert cursor.status == "complete"
    assert cursor.stats()["emittedRows"] == 1000

    # rows decoded ahead are pending rows: closing before they are read stops the cursor
    cursor = ds.select_cursor(q)
    next(cursor)
    assert cursor.status == "open"
    cursor.close()
    assert cursor.status == "stopped"
    assert cursor.stats()["emittedRows"] == 1
    assert list(cursor) == []

    # the async bridge takes what a sync read left decoded, then the rest of the batch
    cursor = ds.select_cursor(q)
    first = next(cursor)
    rest = cursor._next_batch()
    assert len(rest) == 999
    assert cursor.stats()["emittedRows"] == 1000
    assert sorted(tuple(map(str, row)) for row in [first, *rest]) == expected
    assert cursor._next_batch() is None

    quads = ds.graph_cursor("CONSTRUCT { ?s <urn:p> ?o } WHERE { ?s <urn:p> ?o }")
    assert next(quads) is not None
    assert quads.stats()["emittedRows"] == 1
    assert len(list(quads)) == 999
    assert quads.stats()["emittedRows"] == 1000


def test_cursor_decoding_ahead_raises_a_budget_error_at_its_row():
    ds = Dataset()
    ds.load(_numbers(40) + f' <urn:big> <urn:p> "{"x" * 400_000}" .', "ttl")
    q = "SELECT ?o WHERE { ?s <urn:p> ?o } ORDER BY ?s"
    # the long literal sorts first; with a budget that cannot hold its decoded form the
    # first row fails, and nothing is handed out
    cursor = ds.select_cursor(q, max_memory_bytes=1 << 20)
    with pytest.raises(Exception):
        next(cursor)
    assert cursor.status == "failed"
    # with the literal last, the 40 rows before it are handed out before the error
    q = "SELECT ?o WHERE { ?s <urn:p> ?o } ORDER BY DESC(?s)"
    cursor = ds.select_cursor(q, max_memory_bytes=1 << 20)
    rows = []
    with pytest.raises(Exception):
        for row in cursor:
            rows.append(row)
            assert cursor.status == "open"
    assert len(rows) == 40
    assert cursor.status == "failed"
    assert cursor.stats()["emittedRows"] == 40


def test_cursor_decoding_ahead_counts_datatypes_and_triple_terms():
    ds = Dataset()
    datatype = "urn:dt:" + "d" * 65_536
    data = " ".join(f'<urn:s{i}> <urn:p> "x"^^<{datatype}> .' for i in range(300))
    data += " ".join(f" <urn:t{i}> <urn:q> <<( <urn:a{i}> <urn:b> <urn:c> )>> ." for i in range(300))
    ds.load(data, "ttl")
    for q in (
        "SELECT ?s ?o WHERE { ?s <urn:p> ?o }",
        "SELECT ?s ?o WHERE { ?s <urn:q> ?o }",
    ):
        expected = sorted(tuple(map(str, row)) for row in ds.select(q))
        assert len(expected) == 300
        # a refill stops at one mebibyte of owned text, which 16 of the typed rows fill
        cursor = ds.select_cursor(q)
        assert sorted(tuple(map(str, row)) for row in cursor) == expected
        assert cursor.stats()["emittedRows"] == 300
    quads = ds.graph_cursor("CONSTRUCT { ?s <urn:p> ?o } WHERE { ?s <urn:p> ?o }")
    assert len(list(quads)) == 300
