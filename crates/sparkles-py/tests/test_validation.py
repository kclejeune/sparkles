"""Reasoning, SHACL and ShEx (A11)."""

from __future__ import annotations

import json

import pytest
from conftest import ex

import sparkles
from sparkles import INFERRED_GRAPH, Dataset, InvalidInputError, Literal, NamedNode, RdfSyntaxError

pytestmark = pytest.mark.skipif(
    not {"reasoning", "shacl", "shex"} <= sparkles.FEATURES,
    reason="built without reasoning, shacl or shex",
)

ANIMALS = """
@prefix ex: <http://ex.org/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
ex:Dog rdfs:subClassOf ex:Animal .
ex:rex a ex:Dog .
"""

SHAPES = """
@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix ex: <http://ex.org/> .
@prefix foaf: <http://xmlns.com/foaf/0.1/> .
ex:PersonShape a sh:NodeShape ;
  sh:targetClass ex:Person ;
  sh:property [ sh:path foaf:age ; sh:minCount 1 ; sh:message "needs an age" ] .
"""

SHEX = """
PREFIX ex: <http://ex.org/>
PREFIX foaf: <http://xmlns.com/foaf/0.1/>
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
ex:Person { foaf:name LITERAL ; foaf:age xsd:integer }
"""


def test_a11_reasoning() -> None:
    ds = Dataset()
    ds.load(ANIMALS, "turtle")
    q = "ASK { <http://ex.org/rex> a <http://ex.org/Animal> }"
    assert not ds.ask(q)
    report = ds.reason("rdfs")
    assert report.profile == "rdfs" and report.inferred > 0 and report.iterations > 0
    assert ds.ask(q, include_inferred=True)
    assert not ds.ask(q)
    assert NamedNode(INFERRED_GRAPH) in ds.named_graphs()
    assert ds.clear_inferences() == report.inferred
    assert not ds.ask(q, include_inferred=True)
    rules = "[r1: (?x <http://ex.org/likes> ?y) -> (?y <http://ex.org/likedBy> ?x)]"
    ds.add(sparkles.Triple(ex("a"), ex("likes"), ex("b")))
    ds.reason(rules=rules)
    assert ds.ask("ASK { <http://ex.org/b> <http://ex.org/likedBy> <http://ex.org/a> }", include_inferred=True)
    with pytest.raises(InvalidInputError):
        ds.reason("no-such-profile")
    with pytest.raises(RdfSyntaxError):
        ds.reason(rules="[broken")


def test_a11_shacl(ds: Dataset) -> None:
    report = ds.validate_shacl(SHAPES)
    assert not report.conforms and not report
    (r,) = report.results
    assert r.focus_node == ex("carol")
    assert r.path == "<http://xmlns.com/foaf/0.1/age>"
    assert r.constraint_component == NamedNode("http://www.w3.org/ns/shacl#MinCountConstraintComponent")
    assert r.severity == NamedNode("http://www.w3.org/ns/shacl#Violation")
    assert r.message == "needs an age"
    assert r.value is None
    assert "sh:conforms false" in report.to_turtle() or "conforms> false" in report.to_turtle()
    # shapes from a graph of the dataset
    ds.load(SHAPES, "turtle", to_graph="http://ex.org/shapes")
    assert not ds.validate_shacl(shapes_graph="http://ex.org/shapes", data_graph="urn:x-arq:DefaultGraph").conforms
    ds.add(sparkles.Triple(ex("carol"), NamedNode("http://xmlns.com/foaf/0.1/age"), Literal(30)))
    assert ds.validate_shacl(SHAPES.encode()).conforms
    with pytest.raises(InvalidInputError):
        ds.validate_shacl()
    with pytest.raises(RdfSyntaxError):
        ds.validate_shacl("not turtle at all {")


def test_shacl_compact_syntax(ds: Dataset) -> None:
    compact = """
    PREFIX ex: <http://ex.org/>
    PREFIX foaf: <http://xmlns.com/foaf/0.1/>
    shape ex:PersonShape -> ex:Person {
        foaf:age [1..*] message="needs an age" .
    }
    """
    report = ds.validate_shacl(compact, format="shaclc")
    (r,) = report.results
    assert r.focus_node == ex("carol") and r.message == "needs an age"
    assert not ds.validate_shacl(compact, format="text/shaclc").conforms
    with pytest.raises(RdfSyntaxError):
        ds.validate_shacl("shape {", format="shaclc")


def test_a11_shex(ds: Dataset) -> None:
    report = ds.validate_shex(SHEX, "{FOCUS a ex:Person}@ex:Person")
    assert not report.conforms
    by_node = {r.node: r for r in report.results}
    assert by_node[ex("alice")].conformant
    assert by_node[ex("bob")].conformant
    carol = by_node[ex("carol")]
    assert not carol.conformant and carol.shape == "http://ex.org/Person" and carol.reason
    assert json.loads(report.to_json())
    one = ds.validate_shex(SHEX, '[{"node": "http://ex.org/alice", "shape": "http://ex.org/Person"}]')
    assert one.conforms and len(one.results) == 1
    with pytest.raises(RdfSyntaxError):
        ds.validate_shex("ex:Person {", "ex:alice@ex:Person")
    with pytest.raises(RdfSyntaxError):
        ds.validate_shex(SHEX, "{FOCUS")
