"""The exceptions of the sparkles package.

Every exception the engine raises derives from SparklesError. Most also derive from
the built-in exception Python code would expect, so ``except SyntaxError`` or
``except ValueError`` catch them too. I/O failures raise the built-in OSError
subclasses, such as FileNotFoundError.
"""

from __future__ import annotations

__all__ = [
    "SparklesError",
    "ParseError",
    "SparqlSyntaxError",
    "RdfSyntaxError",
    "InvalidInputError",
    "UnsupportedError",
    "QueryTimeoutError",
    "CancelledError",
    "BudgetExceededError",
    "StorageError",
    "DatasetLockedError",
    "CatalogLockedError",
    "BackupError",
    "ConflictError",
    "NotFoundError",
    "PermissionDeniedError",
    "ServiceError",
    "WriteRejectedError",
]


class SparklesError(Exception):
    """The root of the engine's exceptions."""


class ParseError(SparklesError, SyntaxError):
    """Text that does not parse: SPARQL, RDF, rules, shapes or a ShEx schema."""


class SparqlSyntaxError(ParseError):
    """A SPARQL query or update that does not parse."""


class RdfSyntaxError(ParseError):
    """RDF data, Jena rules, SHACL shapes or a ShEx schema or shape map that does not
    parse."""


class InvalidInputError(SparklesError, ValueError):
    """An argument the engine refuses, a closed dataset, or the wrong query form."""


class UnsupportedError(SparklesError, NotImplementedError):
    """A feature the engine or this build does not have."""


class QueryTimeoutError(SparklesError, TimeoutError):
    """A query that ran past its timeout."""


class CancelledError(SparklesError):
    """A cancelled operation."""


class BudgetExceededError(SparklesError):
    """A request past one of its budgets.

    ``kind`` names the budget (such as ``"rows"`` or ``"memory"``), ``limit`` is its
    limit and ``requested`` what the request needed.
    """

    kind: str
    limit: int
    requested: int


class StorageError(SparklesError, OSError):
    """A store that cannot be written or read: corruption, a failed write-ahead log, or
    a full disk."""


class DatasetLockedError(StorageError):
    """A database directory that another process (or another open Dataset) holds."""


class ConflictError(SparklesError):
    """A write that conflicts with the current state, or that would wait for the
    calling thread's own open transaction."""


class NotFoundError(SparklesError, LookupError):
    """A commit, snapshot or other named thing that does not exist."""


class PermissionDeniedError(SparklesError, PermissionError):
    """An operation the caller may not run, such as an outbound request."""


class ServiceError(SparklesError):
    """A failed SPARQL SERVICE call."""


class WriteRejectedError(SparklesError):
    """A write that the dataset's write-time validation rejected."""


class CatalogLockedError(DatasetLockedError):
    """A catalog held by another process or open Catalog."""


class BackupError(SparklesError):
    """A repository failure; code carries the engine backup error code."""

    code: str
