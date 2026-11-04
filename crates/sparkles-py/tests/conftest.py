from __future__ import annotations

import importlib
import os
from types import ModuleType

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


def optional_dependency(name: str) -> ModuleType:
    """Import an optional test dependency such as mypy or rdflib, and skip the module
    without it. The suite's runners install the pinned dependencies of
    requirements-test.txt and set SPARKLES_PY_TEST_DEPS=required, so that a missing one
    fails there instead of skipping its tests."""
    if os.environ.get("SPARKLES_PY_TEST_DEPS") == "required":
        return importlib.import_module(name)
    return pytest.importorskip(name)


def ex(local: str) -> NamedNode:
    return NamedNode(EX + local)


def foaf(local: str) -> NamedNode:
    return NamedNode("http://xmlns.com/foaf/0.1/" + local)


@pytest.fixture
def ds() -> Dataset:
    d = Dataset()
    d.load(TURTLE, "turtle")
    return d
