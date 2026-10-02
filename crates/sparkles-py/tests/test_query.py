"""SPARQL queries and updates (A4–A7)."""

from __future__ import annotations

import pytest
from conftest import ex, foaf

from sparkles import (
    BudgetExceededError,
    Dataset,
    InvalidInputError,
    Literal,
    NamedNode,
    QuerySolutions,
    QueryTriples,
    SparklesError,
    SparqlSyntaxError,
    Triple,
    Variable,
)

PREFIXES = "PREFIX foaf: <http://xmlns.com/foaf/0.1/> PREFIX ex: <http://ex.org/> "


def test_a4_select(ds: Dataset) -> None:
    rows = ds.query(
        PREFIXES + "SELECT ?s ?name ?knows WHERE { ?s foaf:name ?name OPTIONAL { ?s foaf:knows ?knows } } ORDER BY ?name"
    )
    assert isinstance(rows, QuerySolutions)
    assert rows.variables == [Variable("s"), Variable("name"), Variable("knows")]
    first = next(rows)
    assert first["name"] == first[1] == first[Variable("name")] == first["?name"] == Literal("Alice")
    assert first[-1] == ex("bob")
    assert tuple(first) == (ex("alice"), Literal("Alice"), ex("bob"))
    assert len(first) == 3
    assert first.as_dict() == {"s": ex("alice"), "name": Literal("Alice"), "knows": ex("bob")}
    second = next(rows)
    assert second["knows"] is None
    assert "knows" not in second and "name" in second
    assert second.get("knows", "default") == "default"
    assert second.get("nope") is None
    assert second.as_dict().keys() == {"s", "name"}
    assert second.keys() == ["s", "name", "knows"]
    with pytest.raises(KeyError):
        second["nope"]
    with pytest.raises(IndexError):
        second[3]
    assert len(list(rows)) == 1


def test_a5_ask_construct(ds: Dataset) -> None:
    assert ds.query("ASK { ?s ?p ?o }") is True
    assert ds.ask("ASK { <http://ex.org/nobody> ?p ?o }") is False
    triples = ds.query(PREFIXES + "CONSTRUCT { ?s foaf:nick ?n } WHERE { ?s foaf:name ?n }")
    assert isinstance(triples, QueryTriples)
    got = set(triples)
    assert Triple(ex("bob"), foaf("nick"), Literal("Bob")) in got and len(got) == 3
    described = list(ds.construct("DESCRIBE <http://ex.org/bob>"))
    assert len(described) == 3
    with pytest.raises(InvalidInputError):
        ds.select("ASK {}")
    with pytest.raises(InvalidInputError):
        ds.ask("SELECT * WHERE { ?s ?p ?o }")
    with pytest.raises(InvalidInputError):
        ds.construct("ASK {}")
    assert sum(1 for _ in ds.select("SELECT * WHERE { ?s ?p ?o }")) == len(ds)


def test_a6_errors(ds: Dataset) -> None:
    with pytest.raises(SparqlSyntaxError) as e:
        ds.query("SELECT * WHERE {")
    assert isinstance(e.value, SyntaxError) and isinstance(e.value, SparklesError)
    with pytest.raises(SparqlSyntaxError):
        ds.update("INSERT NONSENSE")


def test_a7_update_and_bindings(ds: Dataset) -> None:
    stats = ds.update("INSERT DATA { <http://ex.org/alice> <http://ex.org/q> 1 }")
    assert (stats.inserted, stats.deleted, stats.operations) == (1, 0, 1)
    stats = ds.update(PREFIXES + "DELETE WHERE { ?s foaf:age ?a }")
    assert stats.deleted == 2
    rows = list(
        ds.query(
            PREFIXES + "SELECT ?name WHERE { ?s foaf:name ?name }",
            bindings={"s": ex("bob")},
        )
    )
    assert [r["name"] for r in rows] == [Literal("Bob")]
    rows = list(ds.query("SELECT ?name WHERE { ?s foaf:name ?name }", prefixes={"foaf": "http://xmlns.com/foaf/0.1/"}, bindings={Variable("s"): ex("carol")}))
    assert [r["name"] for r in rows] == [Literal("Carol", language="en")]
    assert ds.ask("ASK { <alice> <q> 1 }", base_iri="http://ex.org/")


def test_datasets_of_queries(ds: Dataset) -> None:
    g = NamedNode("http://ex.org/g")
    ds.load('<http://ex.org/x> <http://ex.org/p> "in g" .', "nt", to_graph=g)
    assert not ds.ask('ASK { ?s ?p "in g" }')
    assert ds.ask('ASK { ?s ?p "in g" }', default_graph=[g])
    assert ds.ask('ASK { GRAPH ?g { ?s ?p "in g" } }', named_graphs=["http://ex.org/g"])
    assert not ds.ask('ASK { GRAPH ?g { ?s ?p "in g" } }', named_graphs=["http://ex.org/other"])


def test_rdf12_terms(ds: Dataset) -> None:
    t = Triple(ex("alice"), foaf("knows"), ex("bob"))
    ds.add(Triple(ex("claim"), ex("about"), t))
    (row,) = ds.query("SELECT ?t WHERE { <http://ex.org/claim> <http://ex.org/about> ?t }")
    assert row["t"] == t
    rtl = Literal("مرحبا", language="ar", direction="rtl")
    ds.add(Triple(ex("greeting"), ex("text"), rtl))
    (row,) = ds.query("SELECT ?t WHERE { <http://ex.org/greeting> <http://ex.org/text> ?t }")
    assert row["t"] == rtl


def test_timeout_and_budget_classes() -> None:
    # the classes exist with their built-in bases
    assert issubclass(BudgetExceededError, SparklesError)
    import sparkles

    assert issubclass(sparkles.QueryTimeoutError, TimeoutError)
    ds = Dataset()
    ds.extend(Triple(ex(f"s{i}"), ex("p"), Literal(i)) for i in range(2000))
    with pytest.raises(sparkles.QueryTimeoutError):
        list(
            ds.query(
                "SELECT (COUNT(*) AS ?n) WHERE { ?a ?p ?x . ?b ?p ?y . ?c ?p ?z FILTER(?x + ?y + ?z < 0) }",
                timeout=0.05,
            )
        )
