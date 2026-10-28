"""Async facade over the existing native result and cancellation types."""

from collections.abc import AsyncIterator, Iterable, Mapping
from typing import Any, final

from ._sparkles import (
    CancelToken,
    NamedNode,
    Quad,
    QuerySolution,
    QuerySolutions,
    UpdateStats,
    Variable,
    _Describe,
    _Path,
    _QueryResult,
    _Term,
)

__all__ = ["Dataset", "GraphCursor", "QueryCursor", "Transaction"]

@final
class Dataset:
    def __init__(
        self,
        path: _Path | None = None,
        *,
        union_default_graph: bool = False,
        group_commit: bool = True,
    ) -> None: ...
    @property
    def closed(self) -> bool: ...
    async def __aenter__(self) -> Dataset: ...
    async def __aexit__(self, *_args: object) -> bool: ...
    async def aclose(self) -> None: ...
    async def query(
        self,
        query: str,
        *,
        base_iri: str | None = None,
        prefixes: Mapping[str, str] | None = None,
        bindings: Mapping[str | Variable, _Term] | None = None,
        default_graph: Iterable[NamedNode | str] | None = None,
        named_graphs: Iterable[NamedNode | str] | None = None,
        include_inferred: bool = False,
        timeout: float | None = None,
        max_rows: int | None = None,
        max_memory_bytes: int | None = None,
        max_rows_produced: int | None = None,
        cancel: CancelToken | None = None,
        at: int | str | None = None,
        describe: _Describe | None = None,
    ) -> _QueryResult: ...
    async def select(
        self,
        query: str,
        *,
        base_iri: str | None = None,
        prefixes: Mapping[str, str] | None = None,
        bindings: Mapping[str | Variable, _Term] | None = None,
        default_graph: Iterable[NamedNode | str] | None = None,
        named_graphs: Iterable[NamedNode | str] | None = None,
        include_inferred: bool = False,
        timeout: float | None = None,
        max_rows: int | None = None,
        max_memory_bytes: int | None = None,
        max_rows_produced: int | None = None,
        cancel: CancelToken | None = None,
        at: int | str | None = None,
        describe: _Describe | None = None,
    ) -> QuerySolutions: ...
    async def ask(
        self,
        query: str,
        *,
        base_iri: str | None = None,
        prefixes: Mapping[str, str] | None = None,
        bindings: Mapping[str | Variable, _Term] | None = None,
        default_graph: Iterable[NamedNode | str] | None = None,
        named_graphs: Iterable[NamedNode | str] | None = None,
        include_inferred: bool = False,
        timeout: float | None = None,
        max_rows: int | None = None,
        max_memory_bytes: int | None = None,
        max_rows_produced: int | None = None,
        cancel: CancelToken | None = None,
        at: int | str | None = None,
        describe: _Describe | None = None,
    ) -> bool: ...
    async def update(
        self,
        update: str,
        *,
        base_iri: str | None = None,
        prefixes: Mapping[str, str] | None = None,
        timeout: float | None = None,
        max_rows: int | None = None,
        max_memory_bytes: int | None = None,
        max_rows_produced: int | None = None,
        cancel: CancelToken | None = None,
    ) -> UpdateStats: ...
    async def select_cursor(
        self,
        query: str,
        *,
        base_iri: str | None = None,
        prefixes: Mapping[str, str] | None = None,
        bindings: Mapping[str | Variable, _Term] | None = None,
        default_graph: Iterable[NamedNode | str] | None = None,
        named_graphs: Iterable[NamedNode | str] | None = None,
        include_inferred: bool = False,
        timeout: float | None = None,
        max_rows: int | None = None,
        max_memory_bytes: int | None = None,
        max_rows_produced: int | None = None,
        cancel: CancelToken | None = None,
        at: int | str | None = None,
        describe: _Describe | None = None,
        batch_rows: int = 4096,
        batch_bytes: int = 1048576,
        allow_materialization: bool = True,
    ) -> QueryCursor: ...
    async def graph_cursor(
        self,
        query: str,
        *,
        base_iri: str | None = None,
        prefixes: Mapping[str, str] | None = None,
        bindings: Mapping[str | Variable, _Term] | None = None,
        default_graph: Iterable[NamedNode | str] | None = None,
        named_graphs: Iterable[NamedNode | str] | None = None,
        include_inferred: bool = False,
        timeout: float | None = None,
        max_rows: int | None = None,
        max_memory_bytes: int | None = None,
        max_rows_produced: int | None = None,
        cancel: CancelToken | None = None,
        at: int | str | None = None,
        describe: _Describe | None = None,
        batch_rows: int = 4096,
        batch_bytes: int = 1048576,
        allow_materialization: bool = True,
    ) -> GraphCursor: ...
    def transaction(
        self, *, cancel: CancelToken | None = None, timeout: float | None = None
    ) -> Transaction: ...

@final
class QueryCursor(AsyncIterator[QuerySolution]):
    @property
    def variables(self) -> list[Variable]: ...
    @property
    def status(self) -> str: ...
    def __aiter__(self) -> QueryCursor: ...
    async def __anext__(self) -> QuerySolution: ...
    async def __aenter__(self) -> QueryCursor: ...
    async def __aexit__(self, *_args: object) -> bool: ...
    async def aclose(self) -> None: ...
    async def stats(self) -> dict[str, Any]: ...
    async def plan(self) -> dict[str, Any]: ...

@final
class GraphCursor(AsyncIterator[Quad]):
    @property
    def status(self) -> str: ...
    def __aiter__(self) -> GraphCursor: ...
    async def __anext__(self) -> Quad: ...
    async def __aenter__(self) -> GraphCursor: ...
    async def __aexit__(self, *_args: object) -> bool: ...
    async def aclose(self) -> None: ...
    async def stats(self) -> dict[str, Any]: ...
    async def plan(self) -> dict[str, Any]: ...

@final
class Transaction:
    def __init__(
        self,
        dataset: Dataset,
        *,
        cancel: CancelToken | None = None,
        timeout: float | None = None,
    ) -> None: ...
    async def __aenter__(self) -> Transaction: ...
    async def __aexit__(
        self, exc_type: type[BaseException] | None, *_args: object
    ) -> bool: ...
    async def commit(self) -> None: ...
    async def rollback(self) -> None: ...
    async def aclose(self) -> None: ...
    async def query(
        self,
        query: str,
        *,
        base_iri: str | None = None,
        prefixes: Mapping[str, str] | None = None,
        bindings: Mapping[str | Variable, _Term] | None = None,
        default_graph: Iterable[NamedNode | str] | None = None,
        named_graphs: Iterable[NamedNode | str] | None = None,
        include_inferred: bool = False,
        timeout: float | None = None,
        max_rows: int | None = None,
        max_memory_bytes: int | None = None,
        max_rows_produced: int | None = None,
        cancel: CancelToken | None = None,
        describe: _Describe | None = None,
    ) -> _QueryResult: ...
    async def update(
        self,
        update: str,
        *,
        base_iri: str | None = None,
        prefixes: Mapping[str, str] | None = None,
        timeout: float | None = None,
        max_rows: int | None = None,
        max_memory_bytes: int | None = None,
        max_rows_produced: int | None = None,
        cancel: CancelToken | None = None,
    ) -> UpdateStats: ...
