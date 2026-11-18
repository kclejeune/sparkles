"""The reading of errors in aggregates: the query keyword and the dataset's setting."""

from pathlib import Path

import pytest

from sparkles import Dataset

Q = "SELECT (MIN(?v) AS ?min) (MAX(?v) AS ?max) WHERE { VALUES ?v { 1 UNDEF 3 } }"


def values(rows) -> tuple:
    row = list(rows)[0]
    return tuple(None if row[v] is None else row[v].value for v in ("min", "max"))


def test_each_reading_of_min_and_max() -> None:
    ds = Dataset()
    assert values(ds.select(Q)) == (None, None)
    assert values(ds.select(Q, aggregate_errors="jena")) == (None, None)
    assert values(ds.select(Q, aggregate_errors="sparkles")) == ("1", "3")
    assert values(ds.query(Q, aggregate_errors="spec")) == (None, "3")
    with pytest.raises(ValueError, match="aggregate-errors"):
        ds.select(Q, aggregate_errors="arq")


def test_the_dataset_setting_persists(tmp_path: Path) -> None:
    with Dataset(tmp_path / "db") as ds:
        status = ds.settings.aggregate_errors.get()
        assert status == {"mode": "jena", "source": "default", "modes": ["jena", "sparkles", "spec"]}
        assert ds.settings.aggregate_errors.set("sparkles")["source"] == "dataset"
        assert values(ds.select(Q)) == ("1", "3")
        # a query's own reading wins over the dataset's
        assert values(ds.select(Q, aggregate_errors="spec")) == (None, "3")
        with pytest.raises(ValueError):
            ds.settings.aggregate_errors.set("nope")
    with Dataset(tmp_path / "db") as ds:
        assert ds.settings.aggregate_errors.get()["mode"] == "sparkles"
        assert ds.settings.aggregate_errors.reset()["source"] == "default"
        assert values(ds.select(Q)) == (None, None)
