"""Sparkles: an embedded RDF database with SPARQL 1.2, reasoning and SHACL and ShEx
validation.

>>> from sparkles import Dataset, NamedNode, Literal, Quad
>>> ds = Dataset()
>>> ds.load('<http://ex.org/a> <http://ex.org/name> "A" .', "nt")
1
>>> [str(s["name"]) for s in ds.query("SELECT ?name WHERE { ?s <http://ex.org/name> ?name }")]
['"A"']

See docs/USAGE.md#python in the Sparkles repository for the full guide.
"""

from __future__ import annotations

from sparkles._errors import (
    BudgetExceededError,
    CancelledError,
    ConflictError,
    DatasetLockedError,
    InvalidInputError,
    NotFoundError,
    ParseError,
    PermissionDeniedError,
    QueryTimeoutError,
    RdfSyntaxError,
    ServiceError,
    SparklesError,
    SparqlSyntaxError,
    StorageError,
    UnsupportedError,
    WriteRejectedError,
)
from sparkles._sparkles import (
    FEATURES,
    INFERRED_GRAPH,
    BlankNode,
    CancelToken,
    Dataset,
    DefaultGraph,
    Literal,
    NamedNode,
    Quad,
    QuadIterator,
    QuerySolution,
    QuerySolutions,
    QueryTriples,
    RdfFormat,
    ReasonReport,
    ShaclReport,
    ShaclResult,
    ShexReport,
    ShexResult,
    Transaction,
    Triple,
    UpdateStats,
    Variable,
    __version__,
    parse,
    serialize,
)

__all__ = [
    "FEATURES",
    "INFERRED_GRAPH",
    "BlankNode",
    "BudgetExceededError",
    "CancelToken",
    "CancelledError",
    "ConflictError",
    "Dataset",
    "DatasetLockedError",
    "DefaultGraph",
    "InvalidInputError",
    "Literal",
    "NamedNode",
    "NotFoundError",
    "ParseError",
    "PermissionDeniedError",
    "Quad",
    "QuadIterator",
    "QuerySolution",
    "QuerySolutions",
    "QueryTimeoutError",
    "QueryTriples",
    "RdfFormat",
    "RdfSyntaxError",
    "ReasonReport",
    "ServiceError",
    "ShaclReport",
    "ShaclResult",
    "ShexReport",
    "ShexResult",
    "SparklesError",
    "SparqlSyntaxError",
    "StorageError",
    "Transaction",
    "Triple",
    "UnsupportedError",
    "UpdateStats",
    "Variable",
    "WriteRejectedError",
    "__version__",
    "parse",
    "serialize",
]
