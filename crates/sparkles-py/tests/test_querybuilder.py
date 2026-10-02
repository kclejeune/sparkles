"""The query builder (`sparkles.querybuilder`)."""

from __future__ import annotations

import pytest
from conftest import ex

from sparkles import Dataset, DefaultGraph, InvalidInputError, Literal, NamedNode, SparqlSyntaxError, Variable
from sparkles.querybuilder import (
    AskBuilder,
    ConstructBuilder,
    DescribeBuilder,
    SelectBuilder,
    UpdateBuilder,
    WhereBuilder,
)

FOAF = "http://xmlns.com/foaf/0.1/"


def test_select(ds: Dataset) -> None:
    q = (
        SelectBuilder()
        .select("?name")
        .where_("?p", "foaf:name", "?name")
        .optional(WhereBuilder().where_("?p", "foaf:age", "?age"))
        .filter("!BOUND(?age) || ?age > 30")
        .order_by("?name")
    )
    text = q.build()
    assert text.startswith(f"PREFIX foaf: <{FOAF}>\nSELECT ?name\nWHERE {{")
    assert "OPTIONAL" in text and "FILTER(!BOUND(?age) || ?age > 30)" in text
    assert str(q) == text
    rows = [r["name"] for r in ds.query(text)]
    assert rows == [Literal("Alice"), Literal("Carol", language="en")]


def test_builders_are_templates(ds: Dataset) -> None:
    by_name = SelectBuilder().select("?age").where_("?p", "foaf:name", "?name").where_("?p", "foaf:age", "?age")
    alice = by_name.set_var("?name", Literal("Alice"))
    bob = by_name.set_var(Variable("name"), Literal("Bob"))
    assert [r["age"] for r in ds.query(alice.build())] == [Literal(42)]
    assert [r["age"] for r in ds.query(bob.build())] == [Literal(17)]
    # the template is unchanged
    assert "?name" in by_name.build()
    # values are escaped, so input cannot break out of a literal
    evil = by_name.set_var("?name", Literal('x" } ; DROP ALL ; SELECT * { "'))
    assert list(ds.query(evil.build())) == []


def test_aggregates_values_and_unions(ds: Dataset) -> None:
    q = (
        SelectBuilder()
        .prefix("ex", "http://ex.org/")
        .select("?type")
        .select_expr("COUNT(?s)", "?n")
        .where_("?s", "a", "?type")
        .values("?s", [ex("alice"), ex("bob"), NamedNode("http://ex.org/nobody")])
        .group_by("?type")
        .having("COUNT(?s) > 1")
    )
    ((t, n),) = [tuple(r) for r in ds.query(q.build())]
    assert t == ex("Person") and n == Literal(2)
    u = (
        SelectBuilder()
        .select_all()
        .distinct()
        .union(
            WhereBuilder().where_("?s", "foaf:age", 42),
            WhereBuilder().where_("?s", "foaf:age", 17),
        )
        .limit(5)
    )
    assert {r["s"] for r in ds.query(u.build())} == {ex("alice"), ex("bob")}
    rows = SelectBuilder().select("?a", "?b").values_rows(["?a", "?b"], [[1, None], [2, "<http://ex.org/x>"]])
    assert len(list(ds.query(rows.build()))) == 2


def test_ask_construct_describe(ds: Dataset) -> None:
    assert ds.query(AskBuilder().where_(ex("alice"), "foaf:knows", ex("bob")).build()) is True
    assert ds.query(AskBuilder().where_(ex("bob"), "foaf:knows", "?x").build()) is False
    made = ConstructBuilder().construct("?b", "foaf:knownBy", "?a").where_("?a", "foaf:knows", "?b")
    (t,) = list(ds.query(made.build()))  # type: ignore[arg-type]
    assert (t.subject, t.object) == (ex("bob"), ex("alice"))
    described = list(ds.query(DescribeBuilder().describe(ex("bob")).build()))  # type: ignore[arg-type]
    assert len(described) == 3


def test_update(ds: Dataset) -> None:
    u = (
        UpdateBuilder()
        .insert_data(ex("dave"), "foaf:name", Literal("Dave"))
        .insert_data(ex("dave"), "foaf:age", 30, graph=ex("g"))
        .then()
        .delete("?p", "foaf:age", "?age")
        .insert("?p", "foaf:age", 18)
        .where_("?p", "foaf:age", "?age")
        .filter("?age < 18")
    )
    stats = ds.update(u.build())
    assert stats.inserted == 3 and stats.deleted == 1
    assert ds.ask("ASK { <http://ex.org/bob> <http://xmlns.com/foaf/0.1/age> 18 }")
    ds.update(UpdateBuilder().copy(DefaultGraph(), ex("backup")).build())
    assert ds.ask("ASK { GRAPH <http://ex.org/backup> { ?s ?p ?o } }")
    ds.update(UpdateBuilder().drop("ALL").build())
    assert len(ds) == 0


def test_errors() -> None:
    with pytest.raises(InvalidInputError):
        SelectBuilder().select("?x").where_("?x", "nope:p", "?y").build()
    with pytest.raises(SparqlSyntaxError):
        SelectBuilder().select("?x").filter("?x >").build()
    with pytest.raises(TypeError):
        SelectBuilder().where_(object(), "a", "?x")  # type: ignore[arg-type]
