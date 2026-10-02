"""Threads and the GIL (A12)."""

from __future__ import annotations

import threading
import time
from concurrent.futures import ThreadPoolExecutor

from conftest import ex

from sparkles import Dataset, Literal, Triple

SLOW = "SELECT (COUNT(*) AS ?n) WHERE { ?a <http://ex.org/p> ?x . ?b <http://ex.org/p> ?y . FILTER(?x < ?y) }"


def big() -> Dataset:
    ds = Dataset()
    ds.extend(Triple(ex(f"s{i}"), ex("p"), Literal(i)) for i in range(2500))
    return ds


def test_a12_queries_release_the_gil() -> None:
    ds = big()
    ticks = 0
    stop = threading.Event()

    def ticker() -> None:
        nonlocal ticks
        while not stop.is_set():
            ticks += 1
            time.sleep(0.001)

    t = threading.Thread(target=ticker)
    t.start()
    start = time.perf_counter()
    (row,) = ds.query(SLOW)
    elapsed = time.perf_counter() - start
    stop.set()
    t.join()
    assert row["n"].to_python() == 2500 * 2499 // 2
    # the other thread ran while the query did (it would not if the GIL were held)
    if elapsed > 0.05:
        assert ticks > 5


def test_a12_parallel_queries() -> None:
    ds = big()

    def count(_: int) -> int:
        (row,) = ds.query("SELECT (COUNT(*) AS ?n) WHERE { ?s <http://ex.org/p> ?o }")
        return int(row["n"].to_python())

    with ThreadPoolExecutor(4) as pool:
        assert list(pool.map(count, range(16))) == [2500] * 16


def test_concurrent_writers() -> None:
    ds = Dataset()

    def write(i: int) -> None:
        for j in range(50):
            ds.add(Triple(ex(f"w{i}"), ex("p"), Literal(j)))

    with ThreadPoolExecutor(4) as pool:
        list(pool.map(write, range(4)))
    assert len(ds) == 200
