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
