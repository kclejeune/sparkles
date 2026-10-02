"""rdflib integration: term conversions and the `SparklesStore` store plugin.

rdflib is imported only when a function here is called or `SparklesStore` is first
used. Methods of sparkles that take a term also accept rdflib's URIRef, BNode and
Literal directly.

The wheel registers `SparklesStore` as the rdflib store plugin `Sparkles`, so
`rdflib.Graph("Sparkles")`, `rdflib.Dataset("Sparkles")` and
`rdflib.ConjunctiveGraph("Sparkles")` keep their triples in Sparkles and run SPARQL in
its engine.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any, Union

from sparkles._sparkles import BlankNode, DefaultGraph, Literal, NamedNode, Quad, Triple, Variable

if TYPE_CHECKING:
    import rdflib

    from sparkles._rdflib_store import SparklesStore

    _Term = Union[NamedNode, BlankNode, Literal, Triple, Variable, DefaultGraph]

__all__ = ["SparklesStore", "from_rdflib", "to_rdflib"]

_XSD_STRING = "http://www.w3.org/2001/XMLSchema#string"


def __getattr__(name: str) -> Any:
    if name == "SparklesStore":
        from sparkles._rdflib_store import SparklesStore

        return SparklesStore
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")


def to_rdflib(term: _Term | Quad) -> Any:
    """The rdflib form of a term: URIRef, BNode, Literal or Variable, a tuple for a
    Quad, and rdflib's default graph identifier for DefaultGraph.

    rdflib has no RDF 1.2 triple terms, so a Triple becomes a 3-tuple.
    """
    import rdflib

    if isinstance(term, NamedNode):
        return rdflib.URIRef(term.value)
    if isinstance(term, BlankNode):
        return rdflib.BNode(term.value)
    if isinstance(term, Literal):
        if term.language is not None:
            return rdflib.Literal(term.value, lang=term.language)
        if term.datatype.value == _XSD_STRING:
            return rdflib.Literal(term.value)
        return rdflib.Literal(term.value, datatype=rdflib.URIRef(term.datatype.value))
    if isinstance(term, Variable):
        return rdflib.Variable(term.value)
    if isinstance(term, DefaultGraph):
        from rdflib.graph import DATASET_DEFAULT_GRAPH_ID

        return DATASET_DEFAULT_GRAPH_ID
    if isinstance(term, Triple):
        return tuple(to_rdflib(t) for t in term)
    if isinstance(term, Quad):
        return tuple(to_rdflib(t) for t in term)
    raise TypeError(f"not a sparkles term: {type(term).__name__}")


def from_rdflib(term: rdflib.term.Node) -> NamedNode | BlankNode | Literal | Variable | DefaultGraph:
    """The sparkles form of an rdflib URIRef, BNode, Literal or Variable, or of
    rdflib's default graph identifier."""
    import rdflib
    from rdflib.graph import DATASET_DEFAULT_GRAPH_ID

    if term == DATASET_DEFAULT_GRAPH_ID:
        return DefaultGraph()
    if isinstance(term, rdflib.URIRef):
        return NamedNode(str(term))
    if isinstance(term, rdflib.BNode):
        return BlankNode(str(term))
    if isinstance(term, rdflib.Literal):
        if term.language is not None:
            return Literal(str(term), language=term.language)
        if term.datatype is not None:
            return Literal(str(term), datatype=NamedNode(str(term.datatype)))
        return Literal(str(term))
    if isinstance(term, rdflib.Variable):
        return Variable(str(term))
    raise TypeError(f"not an rdflib term: {type(term).__name__}")
