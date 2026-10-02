from __future__ import annotations

import pytest

from sparkles import Dataset, NamedNode

EX = "http://ex.org/"

TURTLE = """
@prefix ex: <http://ex.org/> .
@prefix foaf: <http://xmlns.com/foaf/0.1/> .
ex:alice a ex:Person ; foaf:name "Alice" ; foaf:age 42 ; foaf:knows ex:bob .
ex:bob a ex:Person ; foaf:name "Bob" ; foaf:age 17 .
ex:carol a ex:Person ; foaf:name "Carol"@en .
"""


def ex(local: str) -> NamedNode:
    return NamedNode(EX + local)


def foaf(local: str) -> NamedNode:
    return NamedNode("http://xmlns.com/foaf/0.1/" + local)


@pytest.fixture
def ds() -> Dataset:
    d = Dataset()
    d.load(TURTLE, "turtle")
    return d
