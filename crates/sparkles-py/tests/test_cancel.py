"""Cancelling queries and updates (CancelToken and Ctrl-C), and query budgets."""

from __future__ import annotations

import _thread
import os
import signal
import socket
import threading
import time
from collections.abc import Callable, Iterator
from contextlib import contextmanager
from types import FrameType

import pytest
from conftest import ex

from sparkles import (
    BudgetExceededError,
    CancelledError,
    CancelToken,
    Dataset,
    Literal,
    QueryTimeoutError,
    Triple,
)

# 360,000 rows that each hash 40 kB of text: many seconds of work in little memory
ENDLESS = (
    "SELECT (COUNT(*) AS ?n) WHERE { ?a <http://ex.org/t> ?x . ?b <http://ex.org/t> ?y "
    "FILTER(STRLEN(SHA512(CONCAT(?x, ?y))) = 1) }"
)
SLOW_UPDATE = (
    "INSERT { ?a <http://ex.org/q> ?b } WHERE { ?a <http://ex.org/t> ?x . ?b <http://ex.org/t> ?y "
    "FILTER(STRLEN(SHA512(CONCAT(?x, ?y))) = 1) }"
)


@pytest.fixture
def big() -> Dataset:
    ds = Dataset()
    ds.extend(Triple(ex(f"s{i}"), ex("p"), Literal(i)) for i in range(1500))
    ds.extend(Triple(ex(f"s{i}"), ex("t"), Literal("x" * 20_000 + str(i))) for i in range(600))
    return ds


def later(seconds: float, f: Callable[[], object]) -> threading.Thread:
    def run() -> None:
        time.sleep(seconds)
        f()

    t = threading.Thread(target=run)
    t.start()
    return t


@contextmanager
def ctrl_c_after(seconds: float, send: Callable[[], object] = _thread.interrupt_main) -> Iterator[None]:
    """Send the main thread a SIGINT, as Ctrl-C does, after `seconds`. The handler
    raises KeyboardInterrupt only inside the block, so a late signal cannot stop the
    test run."""
    assert threading.current_thread() is threading.main_thread()
    armed = True

    def handler(signum: int, frame: FrameType | None) -> None:
        if armed:
            raise KeyboardInterrupt

    old = signal.signal(signal.SIGINT, handler)
    t = later(seconds, send)
    try:
        yield
    finally:
        t.join()
        armed = False
        time.sleep(0.01)
        signal.signal(signal.SIGINT, old)


def test_cancel_token_stops_a_query(big: Dataset) -> None:
    token = CancelToken()
    assert not token.cancelled
    t = later(0.2, token.cancel)
    with pytest.raises(CancelledError):
        big.query(ENDLESS, cancel=token)
    t.join()
    assert token.cancelled
    # a cancelled token cancels the next request at once
    with pytest.raises(CancelledError):
        big.query("SELECT * WHERE { ?s ?p ?o }", cancel=token)


def test_ctrl_c_interrupts_a_query(big: Dataset) -> None:
    with ctrl_c_after(0.2), pytest.raises(KeyboardInterrupt):
        big.query(ENDLESS)
    # the dataset is usable afterwards
    assert big.ask("ASK { ?s ?p ?o }")


def test_sigint_interrupts_a_query(big: Dataset) -> None:
    # the signal itself, as a terminal sends it, and not interrupt_main's direct call
    with ctrl_c_after(0.2, lambda: os.kill(os.getpid(), signal.SIGINT)), pytest.raises(KeyboardInterrupt):
        big.query(ENDLESS)
    assert big.ask("ASK { ?s ?p ?o }")


def test_ctrl_c_interrupts_a_query_while_another_wakeup_fd_is_set(big: Dataset) -> None:
    # asyncio keeps its own wakeup descriptor, and the query then runs on the helper thread
    a, b = socket.socketpair()
    a.setblocking(False)
    old = signal.set_wakeup_fd(a.fileno())
    try:
        with ctrl_c_after(0.2), pytest.raises(KeyboardInterrupt):
            big.query(ENDLESS)
        assert signal.set_wakeup_fd(a.fileno()) == a.fileno()
    finally:
        signal.set_wakeup_fd(old)
        a.close()
        b.close()


def test_a_signal_whose_handler_returns_lets_the_query_finish(big: Dataset) -> None:
    calls = []

    def handler(signum: int, frame: FrameType | None) -> None:
        calls.append(signum)

    old = signal.signal(signal.SIGUSR1, handler)
    try:
        t = later(0.05, lambda: os.kill(os.getpid(), signal.SIGUSR1))
        r = big.query(
            "SELECT (COUNT(*) AS ?n) WHERE { ?a <http://ex.org/p> ?x . ?b <http://ex.org/p> ?y FILTER(?x < ?y) }"
        )
        t.join()
    finally:
        signal.signal(signal.SIGUSR1, old)
    assert [row[0].value for row in r] == [str(1500 * 1499 // 2)]
    # the handler ran, or the signal came after the query
    assert calls in ([signal.SIGUSR1], [])
    assert signal.set_wakeup_fd(-1) == -1


def test_ctrl_c_interrupts_an_update(big: Dataset) -> None:
    with ctrl_c_after(0.2), pytest.raises(KeyboardInterrupt):
        big.update(SLOW_UPDATE)
    # nothing was committed, and the writer lock is free again
    assert not big.ask("ASK { ?s <http://ex.org/q> ?o }")
    big.add(Triple(ex("a"), ex("q"), Literal(1)))


def test_ctrl_c_interrupts_a_query_in_a_transaction(big: Dataset) -> None:
    with big.transaction() as tx:
        tx.add(Triple(ex("new"), ex("p"), Literal(-1)))
        with ctrl_c_after(0.2), pytest.raises(KeyboardInterrupt):
            tx.query(ENDLESS)
        # the transaction goes on
        assert tx.query("ASK { <http://ex.org/new> ?p ?o }") is True
    assert Triple(ex("new"), ex("p"), Literal(-1)) in big


def test_timeout(big: Dataset) -> None:
    n = len(big)
    with pytest.raises(QueryTimeoutError):
        big.query(ENDLESS, timeout=0.2)
    with pytest.raises(TimeoutError):
        big.update(SLOW_UPDATE, timeout=0.2)
    assert len(big) == n


def test_budgets(big: Dataset) -> None:
    with pytest.raises(BudgetExceededError) as e:
        big.query("SELECT * WHERE { ?a <http://ex.org/p> ?x . ?b <http://ex.org/p> ?y }", max_rows=1000)
    assert e.value.kind == "rows"  # type: ignore[attr-defined]
    assert e.value.limit == 1000  # type: ignore[attr-defined]
    with pytest.raises(BudgetExceededError):
        big.query(ENDLESS, max_rows_produced=10_000)
    with pytest.raises(BudgetExceededError):
        big.query("SELECT * WHERE { ?a <http://ex.org/p> ?x . ?b <http://ex.org/p> ?y }", max_memory_bytes=1 << 16)
    with pytest.raises(BudgetExceededError):
        big.update(
            "INSERT { ?a <http://ex.org/q> ?y } WHERE { ?a <http://ex.org/p> ?x . ?b <http://ex.org/p> ?y }",
            max_rows_produced=10_000,
        )
    assert not big.ask("ASK { ?s <http://ex.org/q> ?o }")
    # under the budgets, queries run as usual
    (row,) = big.query(
        "SELECT (COUNT(*) AS ?n) WHERE { ?s <http://ex.org/p> ?o }", max_rows=10_000, max_memory_bytes=1 << 20
    )
    assert row["n"] == Literal(1500)
