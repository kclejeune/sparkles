"""rdflib interoperability (A14, second half). Skipped without rdflib."""

from __future__ import annotations

import pytest
from conftest import ex, foaf

from sparkles import BlankNode, Dataset, DefaultGraph, Literal, NamedNode, Quad, Variable
from sparkles.rdflib import from_rdflib, to_rdflib

rdflib = pytest.importorskip("rdflib")


def test_round_trip() -> None:
    terms = [
        NamedNode("http://ex.org/a"),
        BlankNode("b1"),
        Literal("x"),
        Literal("chat", language="fr"),
        Literal(42),
        Variable("v"),
        DefaultGraph(),
    ]
    for t in terms:
        assert from_rdflib(to_rdflib(t)) == t
    assert to_rdflib(Literal(42)) == rdflib.Literal(42)
    assert to_rdflib(NamedNode("http://ex.org/a")) == rdflib.URIRef("http://ex.org/a")
    q = Quad(NamedNode("http://ex.org/a"), NamedNode("http://ex.org/p"), Literal(1))
    assert to_rdflib(q)[0] == rdflib.URIRef("http://ex.org/a")


def test_rdflib_terms_as_arguments(ds: Dataset) -> None:
    alice = rdflib.URIRef("http://ex.org/alice")
    name = rdflib.URIRef("http://xmlns.com/foaf/0.1/name")
    assert list(ds.quads_for_pattern(alice, name)) == list(ds.quads_for_pattern(ex("alice"), foaf("name")))
    assert Quad(alice, name, rdflib.Literal("Alice")) in ds  # type: ignore[arg-type]
    assert Quad(ex("carol"), foaf("name"), rdflib.Literal("Carol", lang="en")) in ds  # type: ignore[arg-type]
    assert Quad(ex("alice"), foaf("age"), rdflib.Literal(42)) in ds  # type: ignore[arg-type]
    FOAF = rdflib.Namespace("http://xmlns.com/foaf/0.1/")
    rows = list(ds.query("SELECT ?n WHERE { ?s ?p ?n }", bindings={"s": alice, "p": FOAF.name}))
    assert [r["n"] for r in rows] == [Literal("Alice")]
