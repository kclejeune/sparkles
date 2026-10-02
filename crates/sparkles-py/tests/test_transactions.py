"""Transactions (A8)."""

from __future__ import annotations

import threading

import pytest
from conftest import ex

from sparkles import (
    BlankNode,
    BudgetExceededError,
    ConflictError,
    Dataset,
    InvalidInputError,
    Literal,
    Quad,
    SparqlSyntaxError,
    Triple,
)


def test_a8_commit_and_visibility(ds: Dataset) -> None:
    q = Quad(ex("dave"), ex("p"), Literal(1))
    with ds.transaction() as tx:
        assert tx.add(q) is True
        assert tx.add(q) is False
        assert q in tx
        assert tx.quads_for_pattern(ex("dave")) == [q]
        # the dataset reads the last commit
        assert q not in ds
        assert not ds.ask("ASK { <http://ex.org/dave> ?p ?o }")
        with pytest.raises(ConflictError):
            ds.add(Quad(ex("x"), ex("p"), Literal(2)))
        with pytest.raises(ConflictError):
            ds.update("INSERT DATA { <http://ex.org/x> <http://ex.org/p> 1 }")
        # reads still work
        assert len(list(ds.quads_for_pattern(ex("alice")))) == 4
    assert q in ds
    # the writer slot is released
    assert ds.add(Quad(ex("x"), ex("p"), Literal(2)))


def test_a8_rollback_on_exception(ds: Dataset) -> None:
    n = len(ds)
    with pytest.raises(RuntimeError):
        with ds.transaction() as tx:
            tx.extend([Triple(ex("e"), ex("p"), Literal(i)) for i in range(10)])
            tx.remove(Quad(ex("alice"), ex("p"), Literal("absent")))
            raise RuntimeError("abort")
    assert len(ds) == n


def test_explicit_commit_rollback(ds: Dataset) -> None:
    tx = ds.transaction()
    tx.add(Triple(ex("f"), ex("p"), Literal(1)))
    tx.rollback()
    assert not ds.ask("ASK { <http://ex.org/f> ?p ?o }")
    with pytest.raises(InvalidInputError):
        tx.add(Triple(ex("f"), ex("p"), Literal(1)))
    tx = ds.transaction()
    found = tx.quads_for_pattern(ex("alice"))
    assert len(found) == 4
    assert tx.remove(found[0])
    tx.commit()
    assert len(list(ds.quads_for_pattern(ex("alice")))) == 3


def test_dropped_transaction_rolls_back(ds: Dataset) -> None:
    n = len(ds)
    tx = ds.transaction()
    tx.add(Triple(ex("g"), ex("p"), Literal(1)))
    del tx
    # the next write waits for the rollback, then proceeds
    assert ds.add(Triple(ex("h"), ex("p"), Literal(1)))
    assert len(ds) == n + 1


def test_writers_on_other_threads_wait(ds: Dataset) -> None:
    done = threading.Event()

    def writer() -> None:
        ds.add(Triple(ex("other"), ex("p"), Literal(1)))
        done.set()

    with ds.transaction() as tx:
        t = threading.Thread(target=writer)
        t.start()
        # the other thread waits for the lock without holding the GIL
        assert not done.wait(0.2)
        tx.add(Triple(ex("mine"), ex("p"), Literal(1)))
    t.join(10)
    assert done.is_set()
    assert ds.ask("ASK { <http://ex.org/other> ?p ?o }")
    assert ds.ask("ASK { <http://ex.org/mine> ?p ?o }")


def test_transaction_used_from_another_thread(ds: Dataset) -> None:
    tx = ds.transaction()
    errors: list[BaseException] = []

    def work() -> None:
        try:
            tx.add(Triple(ex("t"), ex("p"), Literal(1)))
            tx.commit()
        except BaseException as e:  # noqa: BLE001
            errors.append(e)

    t = threading.Thread(target=work)
    t.start()
    t.join(10)
    assert not errors
    assert ds.ask("ASK { <http://ex.org/t> ?p ?o }")
    assert ds.add(Triple(ex("after"), ex("p"), Literal(1)))


def test_query_and_update_in_a_transaction(ds: Dataset) -> None:
    with ds.transaction() as tx:
        tx.add(Triple(ex("dave"), ex("p"), Literal(1)))
        # queries in the transaction see its changes; the dataset does not
        assert tx.query("ASK { <http://ex.org/dave> ?p ?o }") is True
        assert ds.ask("ASK { <http://ex.org/dave> ?p ?o }") is False
        stats = tx.update("INSERT { ?s <http://ex.org/q> 2 } WHERE { ?s <http://ex.org/p> 1 }")
        assert stats.inserted == 1
        rows = list(tx.query("SELECT ?o WHERE { <http://ex.org/dave> ?p ?o } ORDER BY ?o"))
        assert [r["o"] for r in rows] == [Literal(1), Literal(2)]
        assert Quad(ex("dave"), ex("q"), Literal(2)) in tx
    assert ds.ask("ASK { <http://ex.org/dave> <http://ex.org/q> 2 }")


def test_update_in_a_rolled_back_transaction(ds: Dataset) -> None:
    n = len(ds)
    with pytest.raises(RuntimeError):
        with ds.transaction() as tx:
            tx.update("DELETE WHERE { ?s ?p ?o }")
            assert tx.query("ASK { ?s ?p ?o }") is False
            raise RuntimeError("abort")
    assert len(ds) == n


def test_failed_update_aborts_the_transaction(ds: Dataset) -> None:
    tx = ds.transaction()
    # a syntax error changes nothing, so the transaction goes on
    with pytest.raises(SparqlSyntaxError):
        tx.update("INSERT DATA {")
    tx.add(Triple(ex("x"), ex("p"), Literal(1)))
    # an update that fails while it runs may have done part of its work
    with pytest.raises(BudgetExceededError):
        tx.update(
            "INSERT { ?a <http://ex.org/q> ?b } WHERE { ?a ?p ?x . ?b ?q ?y }",
            max_rows_produced=10,
        )
    with pytest.raises(InvalidInputError, match="aborted"):
        tx.add(Triple(ex("y"), ex("p"), Literal(1)))
    with pytest.raises(InvalidInputError, match="rolled back"):
        tx.commit()
    assert not ds.ask("ASK { <http://ex.org/x> ?p ?o }")
    # the writer lock is free again
    assert ds.add(Triple(ex("z"), ex("p"), Literal(1)))


def test_stored_blank_nodes_name_their_node_in_writes(ds: Dataset) -> None:
    ds.extend([Quad(BlankNode("x"), ex("p"), Literal(1))])
    (q,) = ds.quads_for_pattern(None, ex("p"), Literal(1))
    stored = q.subject
    assert isinstance(stored, BlankNode)
    # a stored node's label names that node in later writes
    ds.add(Quad(stored, ex("q"), Literal(2)))
    with ds.transaction() as tx:
        tx.add(Quad(stored, ex("r"), Literal(3)))
        tx.extend([Quad(ex("s"), ex("p"), stored)])
    assert len(list(ds.quads_for_pattern(stored))) == 3
    assert ds.ask(
        "ASK { ?b <http://ex.org/p> 1 ; <http://ex.org/q> 2 ; <http://ex.org/r> 3 . <http://ex.org/s> ?p ?b }"
    )
    # other labels name a new node in each write
    ds.add(Quad(BlankNode("y"), ex("t"), Literal(1)))
    ds.add(Quad(BlankNode("y"), ex("t"), Literal(2)))
    assert len({q.subject for q in ds.quads_for_pattern(None, ex("t"))}) == 2


def test_apply(ds: Dataset) -> None:
    a = Quad(ex("a"), ex("p"), Literal(1))
    b = Quad(BlankNode("n1"), ex("p"), BlankNode("n2"))
    c = Quad(BlankNode("n1"), ex("q"), Literal(2), ex("g"))
    absent = Quad(ex("alice"), ex("p"), Literal(0))
    inserted, removed, labels = ds._apply([(True, a), (True, b), (True, c), (False, a), (False, absent)])
    assert (inserted, removed) == (3, 1)
    assert sorted(labels) == ["n1", "n2"]
    n1 = BlankNode(labels["n1"])
    assert {q.predicate for q in ds.quads_for_pattern(n1)} == {ex("p"), ex("q")}
    # the stored labels link later writes to the same nodes
    ds._apply([(True, Quad(n1, ex("r"), BlankNode(labels["n2"])))])
    assert len(list(ds.quads_for_pattern(n1))) == 3
    stored_c = Quad(n1, ex("q"), Literal(2), ex("g"))
    with ds.transaction() as tx:
        inserted, removed, labels2 = tx._apply([(True, Quad(BlankNode("n3"), ex("p"), n1)), (False, stored_c)])
        assert (inserted, removed) == (1, 1)
        assert list(labels2) == ["n3"]
    assert len(list(ds.quads_for_pattern(None, None, n1))) == 1
