"""Transactions (A8)."""

from __future__ import annotations

import threading

import pytest
from conftest import ex

from sparkles import ConflictError, Dataset, InvalidInputError, Literal, Quad, Triple


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
