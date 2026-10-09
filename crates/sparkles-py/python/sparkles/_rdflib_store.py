"""The rdflib store plugin: `sparkles.rdflib.SparklesStore`.

This module imports rdflib, so `sparkles.rdflib` loads it only when `SparklesStore` is
first used.

How rdflib's model maps onto a Sparkles dataset:

* rdflib's default graph identifier (`DATASET_DEFAULT_GRAPH_ID`) is the dataset's
  default graph, and a context named by a URIRef is the named graph of that IRI.
* A context named by a BNode, such as the default context of a ConjunctiveGraph or a
  `Graph` made without an identifier, is the named graph
  `urn:x-sparkles:rdflib:graph:<label>`, so that SPARQL can name it.
* N3 formulae (`QuotedGraph`) are named graphs `urn:x-sparkles:rdflib:formula:…`, and a
  formula used as a term is that IRI. Their triples are quoted: the union of the
  contexts leaves them out. N3 variables are IRIs `urn:x-sparkles:rdflib:variable:…`.
* Blank nodes keep their rdflib labels for the lifetime of the store object. The first
  write of a label makes a stored node, and the store remembers which one. Labels that
  Sparkles hands out (`b…`) name their stored node.
"""

from __future__ import annotations

import os
import re
import shutil
from collections.abc import Generator, Iterable, Iterator, Mapping
from typing import Any
from urllib.parse import quote, unquote

from rdflib.graph import DATASET_DEFAULT_GRAPH_ID, ConjunctiveGraph, Graph, QuotedGraph
from rdflib.namespace import _NAMESPACE_PREFIXES_CORE, _NAMESPACE_PREFIXES_RDFLIB
from rdflib.query import Result
from rdflib.store import NO_STORE, VALID_STORE, Store, TripleAddedEvent, TripleRemovedEvent
from rdflib.term import BNode, Identifier, Node, URIRef
from rdflib.term import Literal as RLiteral
from rdflib.term import Variable as RVariable

from sparkles._errors import UnsupportedError
from sparkles._sparkles import (
    BlankNode,
    Dataset,
    DefaultGraph,
    Literal,
    NamedNode,
    Quad,
    QuerySolutions,
    QueryTriples,
    Transaction,
    Triple,
    _RdflibNodes,
)

__all__ = ["SparklesStore"]

PREFIX = "urn:x-sparkles:rdflib:"
GRAPH = PREFIX + "graph:"
FORMULA = PREFIX + "formula:"
VARIABLE = PREFIX + "variable:"
_XSD_STRING = "http://www.w3.org/2001/XMLSchema#string"
_DEFAULT_GRAPH_IRI = "urn:x-arq:DefaultGraph"
_STORED_LABEL = re.compile(r"b[0-9a-f]+")
# a FROM clause gives the query its own dataset, which the store then leaves alone
_FROM = re.compile(r"(?i)\bFROM\s+(NAMED\s+)?(<|[A-Za-z_][\w.-]*:|:)")
# the bindings rdflib makes in every graph's namespace manager, which are not written
# to the dataset's prefixes
_RDFLIB_BINDINGS = {(p, str(ns)) for p, ns in {**_NAMESPACE_PREFIXES_RDFLIB, **_NAMESPACE_PREFIXES_CORE}.items()}


class _NoMatch(Exception):
    """A pattern term that no stored quad can have."""


class SparklesStore(Store):
    """An rdflib store backed by a Sparkles dataset.

    `Graph("Sparkles")`, `Dataset("Sparkles")` and `ConjunctiveGraph("Sparkles")` use an
    in-memory dataset until `open(path)` opens a database directory. Pass `dataset=` to
    use a `sparkles.Dataset` that is already open.

    SPARQL queries and updates run in Sparkles' engine, not in rdflib's evaluator. A
    prepared query, an update with `initBindings`, and an update of a graph other than
    the default graph fall back to rdflib's evaluator over this store.

    With `autocommit=True` (the default), every write is committed. Writes are gathered
    and committed together before the next read, query, `commit()` or `close()`, so a
    parse costs one commit rather than one per triple. With `autocommit=False`, writes
    go into a transaction that reads and queries through this store see, and that
    `commit()` or `rollback()` ends.
    """

    context_aware = True
    formula_aware = True
    graph_aware = True

    def __init__(
        self,
        configuration: str | None = None,
        identifier: Identifier | None = None,
        *,
        dataset: Dataset | None = None,
        autocommit: bool = True,
        batch_size: int = 10_000,
    ) -> None:
        super().__init__(None, identifier)
        self.identifier = identifier
        self.autocommit = autocommit
        self.transaction_aware = not autocommit
        self._ds = dataset
        self._owned = dataset is None
        self._batch = max(1, batch_size)
        self._pending: list[tuple[bool, Quad]] = []
        self._tx: Transaction | None = None
        # blank node labels: rdflib's -> stored, and back
        self._bn_out: dict[str, str] = {}
        self._bn_in: dict[str, str] = {}
        # labels mapped in the open transaction, forgotten if it rolls back
        self._tx_labels: list[str] = []
        # blank-node graph names read from the dataset (stored as blank nodes)
        self._blank_graphs: set[str] = set()
        # graphs added with add_graph; Sparkles keeps no empty graphs
        self._graphs: set[Identifier] = set()
        self._contexts: dict[Any, Graph] = {}
        self._nodes: dict[str, Node] = {}
        # the Sparkles NamedNode of each URIRef used in a pattern (writes, whose IRIs
        # are often new, do not fill it)
        self._iris: dict[URIRef, NamedNode] = {}
        # the Sparkles graph of each plain Graph context read through
        self._graph_names: dict[Identifier, DefaultGraph | NamedNode] = {}
        # converts whole batches of quads and solutions to rdflib nodes natively
        self._conv = _RdflibNodes(URIRef, self._node, PREFIX)
        self._ns: dict[str, URIRef] = {}
        self._pfx: dict[URIRef, str] = {}
        self._ns_loaded = False
        if configuration:
            self.open(configuration, create=True)

    # ----------------------------------------------------------------- lifecycle ----

    @property
    def dataset(self) -> Dataset:
        """The Sparkles dataset; an in-memory one until `open` is called."""
        if self._ds is None:
            self._ds = Dataset()
            self._owned = True
        return self._ds

    def open(self, configuration: str | tuple[str, str], create: bool = False) -> int | None:
        """Open the database directory `configuration`. Without `create`, a directory
        that does not exist gives `NO_STORE`."""
        path = configuration if isinstance(configuration, str) else configuration[0]
        if not create and not os.path.exists(path):
            return NO_STORE
        self.close(commit_pending_transaction=True)
        self._ds = Dataset(path)
        self._owned = True
        self._ns_loaded = False
        return VALID_STORE

    def close(self, commit_pending_transaction: bool = False) -> None:
        """Commit the gathered writes, end the open transaction (a commit only with
        `commit_pending_transaction`), and close a dataset this store opened."""
        if self._ds is None:
            return
        if self._tx is not None:
            if commit_pending_transaction:
                self.commit()
            else:
                self.rollback()
        else:
            self._flush()
        if self._owned:
            self._ds.close()
            self._ds = None
        self._reset()

    def destroy(self, configuration: str | None) -> None:
        """Delete the database directory `configuration`."""
        if configuration is None:
            return
        if (
            self._ds is not None
            and self._ds.path is not None
            and os.path.exists(configuration)
            and os.path.samefile(self._ds.path, configuration)
        ):
            self.close()
        if os.path.exists(configuration):
            shutil.rmtree(configuration)

    def _reset(self) -> None:
        self._pending.clear()
        self._bn_out.clear()
        self._bn_in.clear()
        self._tx_labels.clear()
        self._blank_graphs.clear()
        self._graphs.clear()
        self._contexts.clear()
        self._ns.clear()
        self._pfx.clear()
        self._ns_loaded = False
        self._nodes.clear()
        self._iris.clear()
        self._graph_names.clear()
        self._conv.clear()

    def gc(self) -> None:
        pass

    # -------------------------------------------------------------- transactions ----

    def commit(self) -> None:
        """Commit the open transaction, or with autocommit the gathered writes."""
        self._flush()
        if self._tx is not None:
            tx, self._tx = self._tx, None
            tx.commit()
            self._tx_labels.clear()

    def rollback(self) -> None:
        """Discard the open transaction's writes. With autocommit every write is
        committed, so nothing is undone."""
        if self.autocommit:
            self._flush()
            return
        self._pending.clear()
        if self._tx is not None:
            tx, self._tx = self._tx, None
            tx.rollback()
        for label in self._tx_labels:
            stored = self._bn_out.pop(label, None)
            if stored is not None:
                self._bn_in.pop(stored, None)
        self._tx_labels.clear()

    def _writer(self) -> Dataset | Transaction:
        if self.autocommit:
            return self.dataset
        if self._tx is None:
            self._tx = self.dataset.transaction()
        return self._tx

    def _reader(self) -> Dataset | Transaction:
        """What reads go through: the open transaction, which sees its own writes, or
        the dataset."""
        self._flush()
        return self._tx if self._tx is not None else self.dataset

    def _flush(self) -> None:
        if not self._pending:
            return
        ops, self._pending = self._pending, []
        _, _, labels = self._writer()._apply(ops)
        for label, stored in labels.items():
            self._bn_out[label] = stored
            self._bn_in[stored] = label
            if self._tx is not None:
                self._tx_labels.append(label)

    def _queue(self, op: tuple[bool, Quad]) -> None:
        self._pending.append(op)
        if len(self._pending) >= self._batch:
            self._flush()

    # --------------------------------------------------------------------- terms ----

    def _term(self, t: Any) -> NamedNode | BlankNode | Literal:
        """A Sparkles term for an rdflib node."""
        if isinstance(t, URIRef):
            return NamedNode(t)
        if isinstance(t, BNode):
            label = str(t)
            return BlankNode(self._bn_out.get(label, label))
        if isinstance(t, RLiteral):
            if t.language:
                return Literal(str(t), language=t.language)
            if t.datatype is not None:
                return Literal(str(t), datatype=NamedNode(t.datatype))
            return Literal(str(t))
        if isinstance(t, RVariable):
            return NamedNode(VARIABLE + quote(str(t), safe=""))
        if isinstance(t, Graph):
            return NamedNode(_formula_iri(t.identifier))
        raise TypeError(f"rdflib term of an unsupported type: {type(t).__name__}")

    def _pattern(self, t: Any) -> NamedNode | BlankNode | Literal | None:
        """A pattern term: `None` matches anything. Raises _NoMatch for a term no stored
        quad has, such as a blank node label this store never wrote or read."""
        if t is None:
            return None
        if type(t) is URIRef:
            # (a URIRef equals only a URIRef, so the cache is keyed by exact type)
            n = self._iris.get(t)
            if n is None:
                try:
                    n = NamedNode(t)
                except ValueError:
                    raise _NoMatch from None
                if len(self._iris) >= 100_000:
                    self._iris.clear()
                self._iris[t] = n
            return n
        # (rdflib nodes hash with their type, so the label maps are keyed by str)
        if isinstance(t, BNode) and str(t) not in self._bn_out and not _STORED_LABEL.fullmatch(t):
            # a label in the gathered writes gets its node when they are committed
            self._flush()
            if str(t) not in self._bn_out:
                raise _NoMatch
        try:
            return self._term(t)
        except (TypeError, ValueError):
            raise _NoMatch from None

    def _spo(self, triple: tuple[Any, Any, Any]) -> tuple[Any, Any, Any]:
        """The pattern terms of a triple pattern. Raises _NoMatch when a position holds
        a term that it cannot hold, such as a literal subject."""
        pattern = self._pattern
        s, p, o = triple
        s, p, o = pattern(s), pattern(p), pattern(o)
        if isinstance(s, Literal) or (p is not None and not isinstance(p, NamedNode)):
            raise _NoMatch
        return s, p, o

    def _node(self, t: Any) -> Node:
        """The rdflib node for a Sparkles term. IRIs and literals repeat, and rdflib's
        nodes are immutable, so recent ones are reused."""
        cls = type(t)
        if cls is NamedNode:
            v = t.value
            found = self._nodes.get(v)
            if found is not None:
                return found
            node: Node
            if v.startswith(PREFIX) and v.startswith(VARIABLE):
                node = RVariable(unquote(v[len(VARIABLE) :]))
            elif v.startswith(PREFIX) and v.startswith(FORMULA):
                return self._context_for(t)
            else:
                node = URIRef(v)
            self._remember(v, node)
            return node
        if cls is BlankNode:
            v = t.value
            return BNode(self._bn_in.get(v, v))
        if cls is Literal:
            key = str(t)
            found = self._nodes.get(key)
            if found is not None:
                return found
            if t.language is not None:
                node = RLiteral(t.value, lang=t.language)
            else:
                dt = t.datatype.value
                node = RLiteral(t.value) if dt == _XSD_STRING else RLiteral(t.value, datatype=self._iri(dt))
            self._remember(key, node)
            return node
        if cls is Triple:
            raise UnsupportedError(f"rdflib has no triple terms: {t}")
        raise TypeError(f"not a Sparkles term: {type(t).__name__}")

    def _iri(self, v: str) -> URIRef:
        found = self._nodes.get(v)
        if isinstance(found, URIRef):
            return found
        node = URIRef(v)
        self._remember(v, node)
        return node

    def _remember(self, key: str, node: Node) -> None:
        # IRIs are keyed by their value and literals by their N-Triples form, which
        # starts with a quote, so the two never collide
        if len(self._nodes) >= 100_000:
            self._nodes.clear()
        self._nodes[key] = node

    def _graph_name(self, context: Any) -> DefaultGraph | NamedNode | BlankNode:
        """The Sparkles graph of an rdflib context (a Graph or its identifier)."""
        if isinstance(context, QuotedGraph):
            return NamedNode(_formula_iri(context.identifier))
        ident = context.identifier if isinstance(context, Graph) else context
        if ident == DATASET_DEFAULT_GRAPH_ID:
            return DefaultGraph()
        if isinstance(ident, BNode):
            if str(ident) in self._blank_graphs:
                return BlankNode(str(ident))
            return NamedNode(GRAPH + ident)
        return NamedNode(str(ident))

    def _graph_pattern(self, context: Any) -> DefaultGraph | NamedNode | BlankNode:
        """`_graph_name` for a read: a context Sparkles cannot hold raises _NoMatch."""
        if type(context) is Graph:
            # the common case, a plain Graph read again and again: its graph is kept,
            # except for a blank-node graph name, whose label can be learned later
            ident = context.identifier
            found = self._graph_names.get(ident)
            if found is not None:
                return found
        try:
            g = self._graph_name(context)
        except (TypeError, ValueError):
            raise _NoMatch from None
        if type(context) is Graph and not isinstance(g, BlankNode):
            if len(self._graph_names) >= 10_000:
                self._graph_names.clear()
            self._graph_names[context.identifier] = g
        return g

    def _context_for(self, g: DefaultGraph | NamedNode | BlankNode) -> Graph:
        """The rdflib context of a Sparkles graph."""
        key = g.value if isinstance(g, (NamedNode, BlankNode)) else ""
        found = self._contexts.get((type(g), key))
        if found is not None:
            return found
        ctx: Graph
        if isinstance(g, DefaultGraph):
            ctx = Graph(store=self, identifier=DATASET_DEFAULT_GRAPH_ID)
        elif isinstance(g, BlankNode):
            self._blank_graphs.add(g.value)
            ctx = Graph(store=self, identifier=BNode(g.value))
            # that identifier now names the stored blank node, not a `graph:` IRI
            self._graph_names.pop(ctx.identifier, None)
        elif g.value.startswith(FORMULA):
            ctx = QuotedGraph(self, _formula_id(g.value))
        elif g.value.startswith(GRAPH):
            ctx = Graph(store=self, identifier=BNode(g.value[len(GRAPH) :]))
        else:
            ctx = Graph(store=self, identifier=URIRef(g.value))
        if len(self._contexts) > 10_000:
            self._contexts.clear()
        self._contexts[(type(g), key)] = ctx
        return ctx

    # ---------------------------------------------------------------- statements ----

    def add(self, triple: tuple[Node, Node, Node], context: Graph, quoted: bool = False) -> None:
        if self.dispatcher.get_map():
            self.dispatcher.dispatch(TripleAddedEvent(triple=triple, context=context))
        s, p, o = triple
        if quoted and not isinstance(context, QuotedGraph):
            raise ValueError("a quoted triple needs a formula (QuotedGraph) as its context")
        g = self._graph_name(context) if context is not None else DefaultGraph()
        self._queue((True, Quad(self._term(s), self._term(p), self._term(o), g)))  # type: ignore[arg-type]

    def addN(self, quads: Iterable[tuple[Node, Node, Node, Any]]) -> None:  # noqa: N802
        for s, p, o, c in quads:
            if c is None:
                raise ValueError(f"no context for the triple {(s, p, o)}")
            self.add((s, p, o), c, quoted=isinstance(c, QuotedGraph))

    def remove(self, triple: tuple[Any, Any, Any], context: Any = None) -> None:
        if self.dispatcher.get_map():
            self.dispatcher.dispatch(TripleRemovedEvent(triple=triple, context=context))
        if context is not None and not isinstance(context, (Graph, Identifier)):
            raise TypeError(f"not a context: {context!r}")
        if context is not None and _is_union(context):
            context = None
        try:
            s, p, o = self._spo(triple)
            g = self._graph_pattern(context) if context is not None else None
        except _NoMatch:
            return
        if g is not None and s is not None and p is not None and o is not None:
            # one quad: removed with the next batch of writes
            self._queue((False, Quad(s, p, o, g)))  # type: ignore[arg-type]
            return
        reader = self._reader()
        found = list(reader.quads_for_pattern(s, p, o, g))  # type: ignore[arg-type]
        for q in found:
            if context is None and _is_formula(q.graph_name):
                continue
            self._queue((False, q))
        self._flush()

    def triples(  # type: ignore[override]
        self, triple_pattern: tuple[Any, Any, Any], context: Any = None
    ) -> Iterator[tuple[tuple[Node, Node, Node], Iterator[Graph]]]:
        if context is not None and _is_union(context):
            context = None
        try:
            s, p, o = self._spo(triple_pattern)
            g = self._graph_pattern(context) if context is not None else None
        except _NoMatch:
            return
        reader = self._reader()
        convert = self._conv.triples
        if g is not None:
            ctx = context if isinstance(context, Graph) else self._context_for(g)
            if isinstance(reader, Transaction):
                quads = reader._quads_iter(s, p, o, g)  # type: ignore[arg-type]
            else:
                quads = reader.quads_for_pattern(s, p, o, g)  # type: ignore[arg-type]
            while (batch := convert(quads)) is not None:
                for t in batch:
                    yield t, iter((ctx,))  # type: ignore[misc]
            return
        # every asserted triple once, with the contexts it is in
        if isinstance(reader, Transaction):
            groups: dict[tuple[Any, ...], list[Any]] = {}
            quads = reader._quads_iter(s, p, o)  # type: ignore[arg-type]
            while (batch := convert(quads, True)) is not None:
                for row in batch:
                    if not _is_formula(row[3]):
                        groups.setdefault(row[:3], []).append(row[3])
            for t, gs in groups.items():
                yield t, iter([self._context_for(g) for g in gs])  # type: ignore[misc]
            return
        last: tuple[Any, ...] | None = None
        graphs: list[Any] = []
        quads = reader._quads_by_triple(s, p, o)  # type: ignore[arg-type]
        while (batch := convert(quads, True)) is not None:
            for row in batch:
                gn = row[3]
                if _is_formula(gn):
                    continue
                t = row[:3]
                if t != last:
                    if last is not None:
                        yield last, iter([self._context_for(g) for g in graphs])  # type: ignore[misc]
                    last, graphs = t, []
                graphs.append(gn)
        if last is not None:
            yield last, iter([self._context_for(g) for g in graphs])  # type: ignore[misc]

    def __len__(self, context: Any = None) -> int:  # type: ignore[override]
        reader = self._reader()
        if context is not None and _is_union(context):
            context = None
        if context is None:
            # distinct asserted triples over every graph
            if isinstance(reader, Dataset) and not reader.named_graphs():
                return len(reader)
            return _count(
                reader,
                "SELECT (COUNT(*) AS ?n) WHERE { SELECT DISTINCT ?s ?p ?o WHERE { { ?s ?p ?o } UNION "
                f'{{ GRAPH ?g {{ ?s ?p ?o }} FILTER(!STRSTARTS(STR(?g), "{FORMULA}")) }} }} }}',
                None,
            )
        try:
            g = self._graph_pattern(context)
        except _NoMatch:
            return 0
        if isinstance(g, DefaultGraph):
            return _count(reader, "SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }", [_DEFAULT_GRAPH_IRI])
        if isinstance(g, BlankNode):
            return len(list(reader.quads_for_pattern(None, None, None, g)))
        return _count(reader, f"SELECT (COUNT(*) AS ?n) WHERE {{ GRAPH <{g.value}> {{ ?s ?p ?o }} }}", None)

    def contexts(self, triple: Any = None) -> Generator[Graph, None, None]:  # type: ignore[override]
        reader = self._reader()
        if triple is None or triple == (None, None, None):
            names: list[Any] = []
            if isinstance(reader, Dataset):
                names.extend(reader.named_graphs())
            else:
                names.extend(_graph_names(reader))
            if reader.query("ASK { ?s ?p ?o }", default_graph=[_DEFAULT_GRAPH_IRI]):
                names.insert(0, DefaultGraph())
            seen = set()
            for g in names:
                ctx = self._context_for(g)
                seen.add(ctx.identifier)
                yield ctx
            for ident in list(self._graphs):
                if ident not in seen:
                    yield self._context_for(self._graph_name(ident))
            return
        try:
            s, p, o = self._spo(triple)
        except _NoMatch:
            return
        for q in list(reader.quads_for_pattern(s, p, o)):  # type: ignore[arg-type]
            if not _is_formula(q.graph_name):
                yield self._context_for(q.graph_name)

    def add_graph(self, graph: Graph) -> None:
        self._graphs.add(graph.identifier)

    def remove_graph(self, graph: Graph) -> None:
        self.remove((None, None, None), graph)
        self._graphs.discard(graph.identifier)

    # ---------------------------------------------------------------- namespaces ----

    def _load_namespaces(self) -> None:
        if self._ns_loaded:
            return
        self._ns_loaded = True
        for prefix, ns in self.dataset.prefixes.items():
            self._ns.setdefault(prefix, URIRef(ns))
            self._pfx.setdefault(URIRef(ns), prefix)

    def bind(self, prefix: str, namespace: URIRef, override: bool = True) -> None:
        """Bind a prefix. A binding other than the ones rdflib makes in every graph is
        also written to the dataset's prefixes."""
        self._load_namespaces()
        namespace = URIRef(namespace)
        bound_ns = self._ns.get(prefix)
        bound_prefix = self._pfx.get(namespace)
        if bound_prefix is None and bound_ns is not None:
            bound_prefix = self._pfx.get(bound_ns)
        if override:
            if bound_prefix is not None:
                self._ns.pop(bound_prefix, None)
            if bound_ns is not None:
                self._pfx.pop(bound_ns, None)
            self._pfx[namespace] = prefix
            self._ns[prefix] = namespace
        else:
            ns = bound_ns if bound_ns is not None else namespace
            p = bound_prefix if bound_prefix is not None else prefix
            self._pfx[ns] = p
            self._ns[p] = ns
            prefix, namespace = p, ns
        if (prefix, str(namespace)) not in _RDFLIB_BINDINGS and self.dataset.prefixes.get(prefix) != str(namespace):
            try:
                self.dataset.set_prefix(prefix, str(namespace))
            except ValueError:
                pass

    def namespace(self, prefix: str) -> URIRef | None:
        self._load_namespaces()
        return self._ns.get(prefix)

    def prefix(self, namespace: URIRef) -> str | None:
        self._load_namespaces()
        return self._pfx.get(URIRef(namespace))

    def namespaces(self) -> Iterator[tuple[str, URIRef]]:
        self._load_namespaces()
        yield from list(self._ns.items())

    # -------------------------------------------------------------------- SPARQL ----

    def query(  # type: ignore[override]
        self,
        query: Any,
        initNs: Mapping[str, Any],  # noqa: N803
        initBindings: Mapping[str, Identifier],  # noqa: N803
        queryGraph: Any,  # noqa: N803
        **kwargs: Any,
    ) -> Result:
        """Run a SPARQL query in Sparkles' engine. A prepared query, or one on a graph
        SPARQL cannot name, raises NotImplementedError, and rdflib then evaluates it
        itself."""
        if not isinstance(query, str):
            raise NotImplementedError("prepared queries run in rdflib's evaluator")
        reader = self._reader()
        options: dict[str, Any] = {
            "prefixes": {str(k): str(v) for k, v in (initNs or {}).items()},
            "bindings": {str(k): self._term(v) for k, v in (initBindings or {}).items()},
        }
        if kwargs.get("base") is not None:
            options["base_iri"] = str(kwargs["base"])
        if not _FROM.search(query):
            options.update(self._query_dataset(reader, queryGraph))
        try:
            r = reader.query(query, **options)
        except UnsupportedError as e:
            raise NotImplementedError(str(e)) from e
        return self._result(r)

    def _query_dataset(self, reader: Dataset | Transaction, query_graph: Any) -> dict[str, Any]:
        """The default and named graphs of a query on `query_graph`."""
        if query_graph is None or (query_graph != "__UNION__" and _is_default(query_graph)):
            return {}
        names = [
            g.value
            for g in _graph_names(reader)
            if isinstance(g, NamedNode) and not g.value.startswith(FORMULA)
        ]
        if query_graph == "__UNION__":
            return {"default_graph": [_DEFAULT_GRAPH_IRI, *names], "named_graphs": names}
        if isinstance(query_graph, QuotedGraph):
            raise NotImplementedError("queries on a formula run in rdflib's evaluator")
        g = self._graph_name(query_graph)
        if isinstance(g, BlankNode):
            raise NotImplementedError("queries on a blank-node graph run in rdflib's evaluator")
        if isinstance(g, DefaultGraph):
            return {}
        return {"default_graph": [g.value], "named_graphs": names or [g.value]}

    def _result(self, r: QuerySolutions | bool | QueryTriples) -> Result:
        if isinstance(r, bool):
            res = Result("ASK")
            res.askAnswer = r
            return res
        if isinstance(r, QuerySolutions):
            res = Result("SELECT")
            names = [v.value for v in r.variables]
            rvars = [RVariable(n) for n in names]
            res.vars = rvars
            convert = self._conv.solutions

            def rows() -> Generator[dict[RVariable, Node], None, None]:
                while (batch := convert(r)) is not None:
                    for sol in batch:
                        yield {v: n for v, n in zip(rvars, sol) if n is not None}

            res.bindings = rows()  # type: ignore[assignment]
            return res
        res = Result("CONSTRUCT")
        g = Graph()
        node = self._node
        for t in r:
            g.add((node(t.subject), node(t.predicate), node(t.object)))  # type: ignore[arg-type]
        res.graph = g
        return res

    def update(  # type: ignore[override]
        self,
        update: Any,
        initNs: Mapping[str, Any],  # noqa: N803
        initBindings: Mapping[str, Identifier],  # noqa: N803
        queryGraph: Any,  # noqa: N803
        **kwargs: Any,
    ) -> None:
        """Run a SPARQL Update request in Sparkles' engine when it targets the default
        graph. Otherwise this raises NotImplementedError, and rdflib runs it itself."""
        if not isinstance(update, str):
            raise NotImplementedError("prepared updates run in rdflib's evaluator")
        if initBindings:
            raise NotImplementedError("updates with initBindings run in rdflib's evaluator")
        if queryGraph is not None and (queryGraph == "__UNION__" or not _is_default(queryGraph)):
            raise NotImplementedError("updates of a graph other than the default graph run in rdflib's evaluator")
        self._flush()
        options: dict[str, Any] = {"prefixes": {str(k): str(v) for k, v in (initNs or {}).items()}}
        if kwargs.get("base") is not None:
            options["base_iri"] = str(kwargs["base"])
        try:
            self._writer().update(update, **options)
        except UnsupportedError as e:
            raise NotImplementedError(str(e)) from e

    def __repr__(self) -> str:
        where = "closed" if self._ds is None else (self._ds.path or "in memory")
        return f"<SparklesStore {where}{'' if self.autocommit else ' transactional'}>"


# ---------------------------------------------------------------------- helpers ----


def _formula_iri(ident: Identifier) -> str:
    if isinstance(ident, BNode):
        return FORMULA + "_:" + ident
    return FORMULA + quote(str(ident), safe="")


def _formula_id(iri: str) -> Identifier:
    rest = iri[len(FORMULA) :]
    if rest.startswith("_:"):
        return BNode(rest[2:])
    return URIRef(unquote(rest))


def _is_formula(g: Any) -> bool:
    return isinstance(g, NamedNode) and g.value.startswith(FORMULA)


def _is_union(context: Any) -> bool:
    """A ConjunctiveGraph or Dataset passed as a context stands for all of them."""
    return type(context) is not Graph and isinstance(context, ConjunctiveGraph)


def _is_default(graph: Any) -> bool:
    ident = graph.identifier if isinstance(graph, Graph) else graph
    return bool(ident == DATASET_DEFAULT_GRAPH_ID)


def _graph_names(reader: Dataset | Transaction) -> list[Any]:
    if isinstance(reader, Dataset):
        return list(reader.named_graphs())
    return [row[0] for row in _select(reader, "SELECT DISTINCT ?g WHERE { GRAPH ?g { ?s ?p ?o } }", None)]


def _count(reader: Dataset | Transaction, query: str, default_graph: list[str] | None) -> int:
    (row,) = _select(reader, query, default_graph)
    n = row[0]
    assert isinstance(n, Literal)
    return int(n.value)


def _select(reader: Dataset | Transaction, query: str, default_graph: list[str] | None) -> QuerySolutions:
    rows = reader.query(query, default_graph=default_graph)
    assert isinstance(rows, QuerySolutions)
    return rows
