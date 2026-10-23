"""`Dataset.graph.groups` returns the answer of the library's `ds.graph().groups`."""
from __future__ import annotations

import time

import pytest
from sparkles import (
    BudgetExceededError,
    CancelToken,
    CancelledError,
    Dataset,
    InvalidInputError,
    QueryTimeoutError,
)

EX = "http://ex.org/"
DATA = f"""PREFIX ex: <{EX}>
INSERT DATA {{
  ex:a a ex:Person ; ex:livesIn ex:p1 ; ex:knows ex:b .
  ex:b a ex:Person ; ex:livesIn ex:p1 ; ex:knows ex:c .
  ex:c a ex:Person ; ex:livesIn ex:p2 .
  ex:d a ex:Person ; ex:livesIn ex:p2 .
  ex:p1 a ex:Place .
  ex:p2 a ex:Place
}}"""


@pytest.fixture
def ds() -> Dataset:
    ds = Dataset()
    ds.update(DATA)
    return ds


def edges(answer: dict, source: str | None = None) -> list[tuple[str, str, int]]:
    return sorted(
        (e["predicate"].removeprefix(EX), e["target"], e["count"])
        for e in answer["edges"]
        if source is None or e["source"] == source
    )


def test_class_level_then_the_members_of_a_group(ds: Dataset) -> None:
    answer = ds.graph.groups({"class": EX + "Person"}, limit=3)
    assert isinstance(answer, dict)
    assert answer["commit"] == 1
    assert answer["within"] == {"class": EX + "Person", "size": 4}
    assert answer["grouping"] == {"kind": "values", "predicate": EX + "livesIn"}
    assert answer["communities"] is False and answer["truncated"] is False
    groups = answer["groups"]
    assert [(g["label"], g["size"]) for g in groups] == [(EX + "p1", 2), (EX + "p2", 2)]
    assert [[t["iri"] for t in g["top"]] for g in groups] == [[EX + "a", EX + "b"], [EX + "c", EX + "d"]]
    assert "members" not in answer
    first = "g:" + groups[0]["id"]
    second = "g:" + groups[1]["id"]
    # a knows b inside the first group, b knows c across to the second, and both groups
    # live in places
    assert edges(answer, first) == [
        ("knows", first, 1),
        ("knows", second, 1),
        ("livesIn", "c:" + EX + "Place", 2),
    ]
    assert edges(answer, second) == [("livesIn", "c:" + EX + "Place", 2)]
    assert len(answer["edges"]) == 4

    members = ds.graph.groups({"group": groups[0]["id"]}, limit=3)
    assert members["within"]["group"] == groups[0]["id"] and members["within"]["size"] == 2
    assert members["grouping"] == {"kind": "members", "predicate": None}
    assert members["groups"] == []
    assert members["members"] == [
        {"iri": EX + "a", "class": EX + "Person"},
        {"iri": EX + "b", "class": EX + "Person"},
    ]
    assert edges(members, "m:" + EX + "a") == [
        ("knows", "m:" + EX + "b", 1),
        ("livesIn", "c:" + EX + "Place", 1),
    ]


def test_members_by_default_and_by_property(ds: Dataset) -> None:
    places = ds.graph.groups({"class": EX + "Place"})
    assert places["within"]["size"] == 2
    assert [m["iri"] for m in places["members"]] == [EX + "p1", EX + "p2"]
    assert edges(places) == [("livesIn", "m:" + EX + "p1", 2), ("livesIn", "m:" + EX + "p2", 2)]

    people = ds.graph.groups({"class": EX + "Person"}, by="members")
    assert [m["iri"] for m in people["members"]] == [EX + n for n in "abcd"]
    assert len(people["edges"]) == 6

    by_property = ds.graph.groups(
        {"class": EX + "Person"}, by={"property": EX + "livesIn", "buckets": 4}
    )
    assert [g["size"] for g in by_property["groups"]] == [2, 2]
    assert by_property == ds.graph.groups({"class": EX + "Person"}, limit=3)


def test_at_and_budgets(ds: Dataset) -> None:
    head = ds.graph.groups({"class": EX + "Place"})
    assert ds.graph.groups({"class": EX + "Place"}, at=1) == head
    assert ds.graph.groups({"class": EX + "Place"}, at="head") == head
    ds.snapshots.create("before")
    ds.update(f"INSERT DATA {{ <{EX}p3> a <{EX}Place> }}")
    assert ds.graph.groups({"class": EX + "Place"})["within"]["size"] == 3
    assert ds.graph.groups({"class": EX + "Place"}, at="snapshot:before") == head
    # a view that holds no Place triple
    empty = ds.graph.groups({"class": EX + "Place"}, default_graph=[EX + "none"])
    assert empty["within"]["size"] == 0
    with pytest.raises(BudgetExceededError):
        ds.graph.groups({"class": EX + "Place"}, max_rows=1)


def test_errors_and_cancellation(ds: Dataset) -> None:
    with pytest.raises(InvalidInputError):
        ds.graph.groups({"class": "not an iri"})
    with pytest.raises(InvalidInputError):
        ds.graph.groups({})
    with pytest.raises(InvalidInputError):
        ds.graph.groups({"class": EX + "Person"}, by="nope")
    with pytest.raises(InvalidInputError):
        ds.graph.groups({"class": EX + "Person"}, limit=0)
    token = CancelToken()
    token.cancel()
    with pytest.raises(CancelledError):
        ds.graph.groups({"class": EX + "Person"}, cancel=token)
    handle = ds.graph
    ds.close()
    with pytest.raises(InvalidInputError, match="closed"):
        handle.groups({"class": EX + "Person"})


def test_the_timeout_covers_all_of_the_queries() -> None:
    # the answer takes many queries, each of which is shorter than the timeout
    rows = "\n".join(
        f"<{EX}s{i}> a <{EX}T> ; <{EX}region> <{EX}r{i % 12}> ; <{EX}knows> <{EX}s{(i * 7) % 200000}> ."
        for i in range(200000)
    )
    # two copies, so that the result cache of the first has nothing to serve the second
    measured, bounded = Dataset(), Dataset()
    measured.load(rows, "turtle")
    bounded.load(rows, "turtle")
    request = {"class": EX + "T"}
    by = {"property": EX + "region"}

    start = time.perf_counter()
    answer = measured.graph.groups(request, by=by)
    whole = time.perf_counter() - start
    assert len(answer["groups"]) == 12
    assert whole > 0.04, f"the answer took {whole} s, too fast to bound"
    with pytest.raises(QueryTimeoutError):
        bounded.graph.groups(request, by=by, timeout=whole / 2)
