"""The rdflib store plugin, `sparkles.rdflib.SparklesStore`. Skipped without rdflib.

The graph, context, formula and dataset tests follow the scenarios of rdflib's own store
tests (test_graph.py, test_graph_context.py, test_graph_formula.py and test_dataset.py
in the rdflib repository), which rdflib runs for every context-aware store plugin.
"""

from __future__ import annotations

import importlib.metadata
from collections.abc import Callable, Iterator
from pathlib import Path

import pytest

rdflib = pytest.importorskip("rdflib")

from rdflib import RDF, RDFS, BNode, ConjunctiveGraph, Graph, Literal, Namespace, URIRef, Variable  # noqa: E402
from rdflib.graph import DATASET_DEFAULT_GRAPH_ID, QuotedGraph  # noqa: E402
from rdflib.plugins.sparql import prepareQuery  # noqa: E402
from rdflib.store import NO_STORE, VALID_STORE, Store  # noqa: E402

import sparkles  # noqa: E402
from sparkles import UnsupportedError  # noqa: E402
from sparkles.rdflib import SparklesStore  # noqa: E402

# the wheel registers the plugin with an entry point; the test build is not installed
rdflib.plugin.register("Sparkles", Store, "sparkles.rdflib", "SparklesStore")

EX = Namespace("urn:example:")
TAREK, MICHEL, BOB = EX.tarek, EX.michel, EX.bob
LIKES, HATES = EX.likes, EX.hates
PIZZA, CHEESE = EX.pizza, EX.cheese
C1, C2 = EX["context-1"], EX["context-2"]

# rdflib deprecates ConjunctiveGraph and Dataset.default_context, which these tests use
pytestmark = pytest.mark.filterwarnings("ignore::DeprecationWarning")


@pytest.fixture(params=["memory", "transactional"])
def store(request: pytest.FixtureRequest) -> Iterator[SparklesStore]:
    s = SparklesStore(autocommit=request.param == "memory")
    yield s
    s.close(commit_pending_transaction=True)


def populate(graph: Graph) -> None:
    graph.add((TAREK, LIKES, PIZZA))
    graph.add((TAREK, LIKES, CHEESE))
    graph.add((MICHEL, LIKES, PIZZA))
    graph.add((MICHEL, LIKES, CHEESE))
    graph.add((BOB, LIKES, CHEESE))
    graph.add((BOB, HATES, PIZZA))
    graph.add((BOB, HATES, MICHEL))


def depopulate(graph: Graph) -> None:
    for t in [
        (TAREK, LIKES, PIZZA),
        (TAREK, LIKES, CHEESE),
        (MICHEL, LIKES, PIZZA),
        (MICHEL, LIKES, CHEESE),
        (BOB, LIKES, CHEESE),
        (BOB, HATES, PIZZA),
        (BOB, HATES, MICHEL),
    ]:
        graph.remove(t)


def check_patterns(triples: Callable[..., Iterator[object]]) -> None:
    def n(s: object, p: object, o: object) -> int:
        return len(list(triples((s, p, o))))

    assert (n(None, LIKES, PIZZA), n(None, HATES, PIZZA), n(None, LIKES, CHEESE), n(None, HATES, CHEESE)) == (2, 1, 3, 0)
    assert (n(MICHEL, LIKES, None), n(TAREK, LIKES, None), n(BOB, HATES, None), n(BOB, LIKES, None)) == (2, 2, 2, 1)
    assert (n(MICHEL, None, CHEESE), n(TAREK, None, CHEESE), n(BOB, None, PIZZA), n(BOB, None, MICHEL)) == (1, 1, 1, 1)
    assert (n(None, HATES, None), n(None, LIKES, None)) == (2, 5)
    assert (n(MICHEL, None, None), n(BOB, None, None), n(TAREK, None, None)) == (2, 3, 2)
    assert (n(None, None, PIZZA), n(None, None, CHEESE), n(None, None, MICHEL)) == (3, 3, 1)
    assert n(None, None, None) == 7


# --------------------------------------------------------------------- graphs ----


def test_graph_triples(store: SparklesStore) -> None:
    g = Graph(store, identifier=C1)
    populate(g)
    check_patterns(g.triples)
    assert len(g) == 7
    assert set(g.subjects(LIKES, PIZZA)) == {MICHEL, TAREK}
    assert set(g.objects(BOB, HATES)) == {MICHEL, PIZZA}
    assert set(g.predicate_objects(BOB)) == {(LIKES, CHEESE), (HATES, PIZZA), (HATES, MICHEL)}
    assert (BOB, HATES, MICHEL) in g
    assert (BOB, LIKES, MICHEL) not in g
    depopulate(g)
    assert len(g) == 0
    assert list(g) == []


def test_graph_operators(store: SparklesStore) -> None:
    g1 = Graph(store, identifier=C1)
    g2 = Graph(store, identifier=C2)
    g1.add((TAREK, LIKES, PIZZA))
    g1.add((MICHEL, LIKES, CHEESE))
    g2.add((BOB, LIKES, CHEESE))
    g2.add((MICHEL, LIKES, CHEESE))
    assert set(g1 - g2) == {(TAREK, LIKES, PIZZA)}
    assert set(g1 * g2) == {(MICHEL, LIKES, CHEESE)}
    assert len(g1 + g2) == 3
    g1 -= g2
    assert set(g1) == {(TAREK, LIKES, PIZZA)}
    assert set(g2) == {(BOB, LIKES, CHEESE), (MICHEL, LIKES, CHEESE)}


def test_connected_and_transitive(store: SparklesStore) -> None:
    g = Graph(store, identifier=C1)
    populate(g)
    assert g.connected()
    g.add((EX.jeroen, LIKES, EX.unconnected))
    assert not g.connected()
    g.add((EX.a, RDFS.subClassOf, EX.b))
    g.add((EX.b, RDFS.subClassOf, EX.c))
    assert set(g.transitive_objects(EX.a, RDFS.subClassOf)) == {EX.a, EX.b, EX.c}


def test_set_and_value(store: SparklesStore) -> None:
    g = Graph(store, identifier=C1)
    g.add((BOB, EX.age, Literal(41)))
    g.set((BOB, EX.age, Literal(42)))
    assert g.value(BOB, EX.age) == Literal(42)
    assert len(g) == 1


def test_literals_round_trip(store: SparklesStore) -> None:
    g = Graph(store, identifier=C1)
    values = [
        Literal("plain"),
        Literal("chat", lang="fr"),
        Literal(42),
        Literal(1.5),
        Literal(True),
        Literal("2026-10-02", datatype=URIRef("http://www.w3.org/2001/XMLSchema#date")),
        Literal("x", datatype=EX.custom),
    ]
    for i, v in enumerate(values):
        g.add((EX[f"s{i}"], EX.p, v))
    for i, v in enumerate(values):
        assert g.value(EX[f"s{i}"], EX.p) == v
        assert (EX[f"s{i}"], EX.p, v) in g


def test_blank_nodes_keep_their_labels(store: SparklesStore) -> None:
    g = Graph(store, identifier=C1)
    b = BNode()
    g.add((EX.a, EX.knows, b))
    g.add((b, EX.name, Literal("bee")))
    # read back, the node has its rdflib label
    assert g.value(EX.a, EX.knows) == b
    assert (b, EX.name, Literal("bee")) in g
    # a later write with the label reaches the same node
    g.add((b, EX.age, Literal(3)))
    assert set(g.predicates(b, None)) == {EX.name, EX.age}
    assert g.query("ASK { ?x <urn:example:name> 'bee' ; <urn:example:age> 3 }").askAnswer
    # a label the store has never seen matches nothing
    assert list(g.triples((BNode(), None, None))) == []
    g.remove((b, None, None))
    assert len(g) == 1


def test_relative_iris_are_refused(store: SparklesStore) -> None:
    g = Graph(store, identifier=C1)
    with pytest.raises(ValueError):
        g.add((URIRef("relative"), LIKES, PIZZA))
    # reads find nothing for them
    assert list(g.triples((URIRef("relative"), None, None))) == []
    assert len(Graph(store, identifier=URIRef("relative"))) == 0


def test_triple_terms_are_refused() -> None:
    ds = sparkles.Dataset()
    ds.update("INSERT DATA { <urn:example:s> <urn:example:p> <<( <urn:example:a> <urn:example:b> <urn:example:c> )>> }")
    g = Graph(SparklesStore(dataset=ds), identifier=DATASET_DEFAULT_GRAPH_ID)
    with pytest.raises(UnsupportedError):
        list(g)


# ------------------------------------------------------------------- contexts ----


def multiple_contexts(ds: rdflib.Dataset) -> tuple[URIRef, URIRef, URIRef]:
    triple = (PIZZA, HATES, TAREK)
    ds.add(triple)
    Graph(ds.store, C1).add(triple)
    Graph(ds.store, C2).add(triple)
    return triple


def test_contexts(store: SparklesStore) -> None:
    ds = rdflib.Dataset(store, default_union=True)
    populate(Graph(store, C1))
    check_patterns(ds.triples)
    check_patterns(Graph(store, C1).triples)
    triple = multiple_contexts(ds)
    ids = {c.identifier for c in ds.graphs()}
    assert {C1, C2, DATASET_DEFAULT_GRAPH_ID} <= ids
    assert {c.identifier for c in ds.contexts(triple)} == {C1, C2, DATASET_DEFAULT_GRAPH_ID}
    # each triple once, with all its contexts
    quads = [q for q in ds.quads((PIZZA, HATES, TAREK, None))]
    assert len(quads) == 3
    assert len(list(ds.triples((PIZZA, HATES, TAREK)))) == 1


def test_len_in_contexts(store: SparklesStore) -> None:
    ds = rdflib.Dataset(store, default_union=True)
    multiple_contexts(ds)
    # the same triple in three contexts counts once
    assert len(ds) == 1
    assert len(Graph(store, C1)) == 1
    g = Graph(store, C1)
    for _ in range(10):
        g.add((BNode(), HATES, HATES))
    assert len(g) == 11
    assert len(ds) == 11
    ds.remove_graph(g)
    assert len(ds) == 1
    assert len(g) == 0


def test_remove_in_multiple_contexts(store: SparklesStore) -> None:
    ds = rdflib.Dataset(store, default_union=True)
    triple = multiple_contexts(ds)
    Graph(store, C1).remove(triple)
    assert triple in ds
    Graph(store, C2).remove(triple)
    assert triple in ds
    ds.remove(triple)
    assert triple not in ds
    # a triple without a context is removed from every context
    multiple_contexts(ds)
    ds.remove(triple)
    assert triple not in ds
    assert len(ds) == 0


def test_remove_context(store: SparklesStore) -> None:
    ds = rdflib.Dataset(store, default_union=True)
    multiple_contexts(ds)
    assert len(Graph(store, C1)) == 1
    ds.remove_graph(C1)
    assert C1 not in {c.identifier for c in ds.graphs()}
    ds.remove((None, None, None))
    assert len(ds) == 0


def test_conjunctive_graph(store: SparklesStore) -> None:
    cg = ConjunctiveGraph(store)
    a, b = URIRef("urn:a"), URIRef("urn:b")
    cg.get_context(a).add((a, a, a))
    cg.addN([(b, b, b, cg.get_context(b))])
    cg.add((EX.x, EX.y, EX.z))
    assert set(cg) == {(a, a, a), (b, b, b), (EX.x, EX.y, EX.z)}
    for q in cg.quads():
        assert isinstance(q[3], Graph)
    # a ConjunctiveGraph queries the union of its contexts
    assert {r[0] for r in cg.query("SELECT ?s WHERE { ?s ?p ?o }")} == {a, b, EX.x}
    # its default context is named by a blank node, kept as a graph SPARQL can name
    rows = list(cg.default_context.query("SELECT ?s WHERE { ?s ?p ?o }"))
    assert [r[0] for r in rows] == [EX.x]
    assert cg.default_context.identifier in {c.identifier for c in cg.contexts()}


def test_bnode_context_is_not_the_iri_of_its_label(store: SparklesStore) -> None:
    cg = ConjunctiveGraph(store)
    b = BNode()
    cg.get_context(b).parse(data="<d:d> <e:e> <f:f> .", format="turtle")
    assert len(list(cg.get_context(b).triples((None, None, None)))) == 1
    assert list(cg.get_context(URIRef("urn:" + b)).triples((None, None, None))) == []


# ------------------------------------------------------------------- datasets ----


def test_dataset_graphs(store: SparklesStore) -> None:
    ds = rdflib.Dataset(store)
    assert {g.identifier for g in ds.graphs()} == {DATASET_DEFAULT_GRAPH_ID}
    g1 = ds.graph(C1)
    # an empty graph is listed for the life of the store object
    assert {g.identifier for g in ds.graphs()} == {DATASET_DEFAULT_GRAPH_ID, C1}
    g1.add((TAREK, LIKES, PIZZA))
    ds.add((BOB, LIKES, CHEESE))
    assert len(g1) == 1
    assert len(ds.default_graph) == 1
    # not the union: the default graph holds its own triples only
    assert set(ds.triples((None, None, None))) == {(BOB, LIKES, CHEESE)}
    assert {(s, p, o) for s, p, o, _ in ds.quads((None, None, None, None))} == {
        (TAREK, LIKES, PIZZA),
        (BOB, LIKES, CHEESE),
    }
    ds.remove_graph(g1)
    assert {g.identifier for g in ds.graphs()} == {DATASET_DEFAULT_GRAPH_ID}
    union = rdflib.Dataset(store, default_union=True)
    union.graph(C2).add((MICHEL, LIKES, CHEESE))
    assert set(union.triples((None, None, None))) == {(BOB, LIKES, CHEESE), (MICHEL, LIKES, CHEESE)}


def test_dataset_parse_trig(store: SparklesStore) -> None:
    ds = rdflib.Dataset(store)
    ds.parse(
        data="""
        @prefix ex: <urn:example:> .
        ex:a ex:p ex:b .
        ex:g1 { ex:a ex:p [ ex:q "in g1" ] }
        """,
        format="trig",
    )
    assert len(ds.graph(EX.g1)) == 2
    assert len(ds.default_graph) == 1
    out = ds.serialize(format="nquads")
    again = rdflib.Dataset()
    again.parse(data=out, format="nquads")
    assert len(list(again.quads())) == 3


# ------------------------------------------------------------------- formulae ----


def test_formulae(store: SparklesStore) -> None:
    g = rdflib.Dataset(store)
    g.parse(
        data="""
        @prefix : <http://test/> .
        @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
        {:a :b :c ; a :foo} => {:a :d :c} .
        _:foo a rdfs:Class .
        :a :d :c .
        """,
        format="n3",
    )
    implies = URIRef("http://www.w3.org/2000/10/swap/log#implies")
    a, b, c, d = (URIRef("http://test/" + x) for x in "abcd")
    universe = rdflib.Dataset(g.store)
    formula_a, formula_b = next(universe.subject_objects(implies))
    assert isinstance(formula_a, QuotedGraph) and isinstance(formula_b, QuotedGraph)
    # quoted triples belong to their formula, not to the union
    assert len(list(formula_a.triples((None, None, None)))) == 2
    assert len(list(formula_b.triples((None, None, None)))) == 1
    assert (a, b, c) in formula_a
    assert (a, b, c) not in universe
    assert (a, d, c) in universe
    assert len(list(universe.triples((None, RDF.type, RDFS.Class)))) == 1
    # a formula with variables
    v = Graph(store, identifier=C1)
    v.parse(data="@prefix : <http://test/> . { ?x a :Dog } => { ?x a :Animal } .", format="n3")
    left, right = next(Graph(store, C1).subject_objects(implies))
    assert any(isinstance(t[0], Variable) for t in left)
    # a formula's triples go when it is removed
    formula_a.remove((None, None, None))
    assert len(formula_a) == 0


# ------------------------------------------------------------------ SPARQL ----


def test_query_select_ask_construct(store: SparklesStore) -> None:
    g = Graph(store, identifier=C1)
    populate(g)
    rows = g.query(
        "SELECT ?who WHERE { ?who ex:likes ex:pizza } ORDER BY ?who",
        initNs={"ex": EX},
    )
    assert rows.vars == [Variable("who")]
    assert [r.who for r in rows] == [MICHEL, TAREK]
    assert len(rows) == 2
    # initBindings pre-bind variables
    rows = g.query("SELECT ?what WHERE { ?who <urn:example:likes> ?what }", initBindings={"who": BOB})
    assert [r.what for r in rows] == [CHEESE]
    assert g.query("ASK { <urn:example:bob> <urn:example:hates> <urn:example:pizza> }").askAnswer is True
    made = g.query("CONSTRUCT { ?b <urn:example:dislikes> ?o } WHERE { ?b <urn:example:hates> ?o }")
    assert made.graph is not None
    assert set(made.graph) == {(BOB, EX.dislikes, PIZZA), (BOB, EX.dislikes, MICHEL)}
    # results serialize like rdflib's own
    assert b"urn:example:michel" in g.query("SELECT ?who WHERE { ?who ?p ?o }").serialize(format="json")


def test_query_runs_in_sparkles(store: SparklesStore) -> None:
    g = Graph(store, identifier=C1)
    populate(g)
    # rdflib's evaluator has no RDF 1.2 triple terms; Sparkles' does
    rows = list(g.query("SELECT (isTRIPLE(<<( <urn:a> <urn:b> <urn:c> )>>) AS ?t) WHERE {}"))
    assert rows[0][0] == Literal(True)
    # a prepared query runs in rdflib's evaluator over the store
    prepared = prepareQuery("SELECT ?who WHERE { ?who <urn:example:likes> <urn:example:pizza> }")
    assert {r[0] for r in g.query(prepared)} == {MICHEL, TAREK}


def test_query_graph_scoping(store: SparklesStore) -> None:
    ds = rdflib.Dataset(store)
    ds.graph(C1).add((TAREK, LIKES, PIZZA))
    ds.graph(C2).add((BOB, LIKES, CHEESE))
    ds.add((MICHEL, LIKES, CHEESE))
    q = "SELECT ?s WHERE { ?s ?p ?o }"
    assert [r[0] for r in ds.graph(C1).query(q)] == [TAREK]
    assert [r[0] for r in ds.query(q)] == [MICHEL]
    # GRAPH sees the named graphs
    rows = ds.graph(C1).query("SELECT ?g WHERE { GRAPH ?g { ?s ?p ?o } } ORDER BY ?g")
    assert [r[0] for r in rows] == [C1, C2]
    union = rdflib.Dataset(store, default_union=True)
    assert {r[0] for r in union.query(q)} == {TAREK, BOB, MICHEL}
    # a FROM clause chooses the query's own dataset
    rows = ds.query("SELECT ?s FROM <urn:example:context-2> WHERE { ?s ?p ?o }")
    assert [r[0] for r in rows] == [BOB]


def test_update(store: SparklesStore) -> None:
    ds = rdflib.Dataset(store)
    ds.update("PREFIX ex: <urn:example:> INSERT DATA { ex:a ex:p 1 . GRAPH ex:g { ex:a ex:p 2 } }")
    assert (EX.a, EX.p, Literal(1)) in ds
    assert (EX.a, EX.p, Literal(2)) in ds.graph(EX.g)
    ds.update("DELETE { ?s ?p ?o } INSERT { ?s ?p 3 } WHERE { ?s ?p ?o }", initNs={"ex": EX})
    assert set(ds.objects(EX.a, EX.p)) == {Literal(3)}
    # updates that Sparkles' engine does not take go to rdflib's evaluator
    g = ds.graph(C1)
    g.update("INSERT DATA { <urn:example:x> <urn:example:p> 4 }")
    assert (EX.x, EX.p, Literal(4)) in g
    ds.update("INSERT { ?s <urn:example:q> 5 } WHERE {}", initBindings={"s": EX.y})
    assert (EX.y, EX.q, Literal(5)) in ds


# ------------------------------------------------------------- transactions ----


def test_transactions() -> None:
    dataset = sparkles.Dataset()
    store = SparklesStore(dataset=dataset, autocommit=False)
    assert store.transaction_aware
    g = Graph(store, identifier=C1)
    g.add((TAREK, LIKES, PIZZA))
    b = BNode()
    g.add((b, LIKES, CHEESE))
    # reads and queries through the store see the open transaction
    assert len(g) == 2
    assert g.query("ASK { ?s <urn:example:likes> <urn:example:pizza> }").askAnswer
    # the dataset does not until it commits
    assert len(dataset) == 0
    g.commit()
    assert len(dataset) == 2
    g.add((BOB, LIKES, CHEESE))
    g.update("DELETE WHERE { ?s <urn:example:likes> <urn:example:pizza> }")
    g.rollback()
    assert set(g) == {(TAREK, LIKES, PIZZA), (b, LIKES, CHEESE)}
    # a blank node made in a rolled back transaction is new again afterwards
    c = BNode()
    g.add((c, LIKES, PIZZA))
    g.rollback()
    g.add((c, LIKES, PIZZA))
    g.commit()
    assert (c, LIKES, PIZZA) in g
    # closing without a commit discards the open transaction
    g.add((MICHEL, LIKES, PIZZA))
    store.close()
    assert len(dataset) == 3


def test_autocommit_commits_every_write() -> None:
    store = SparklesStore()
    assert not store.transaction_aware
    g = Graph(store, identifier=C1)
    g.add((TAREK, LIKES, PIZZA))
    g.rollback()
    assert (TAREK, LIKES, PIZZA) in g
    assert len(store.dataset) == 1


# --------------------------------------------------------- storage and naming ----


def test_on_disk(tmp_path: Path) -> None:
    store = SparklesStore()
    assert store.open(str(tmp_path / "db"), create=True) == VALID_STORE
    ds = rdflib.Dataset(store, default_union=True)
    populate(Graph(store, C1))
    check_patterns(ds.triples)
    triple = multiple_contexts(ds)
    assert {c.identifier for c in ds.contexts(triple)} == {C1, C2, DATASET_DEFAULT_GRAPH_ID}
    assert len(ds) == 8
    store.close()
    store = SparklesStore()
    store.open(str(tmp_path / "db"))
    ds = rdflib.Dataset(store, default_union=True)
    assert len(ds) == 8
    assert {c.identifier for c in ds.contexts(triple)} == {C1, C2, DATASET_DEFAULT_GRAPH_ID}
    store.close()


def test_persistent_store(tmp_path: Path) -> None:
    path = str(tmp_path / "db")
    g = Graph("Sparkles", identifier=C1)
    assert g.open(path, create=False) == NO_STORE
    assert g.open(path, create=True) == VALID_STORE
    populate(g)
    g.bind("ex", EX)
    b = BNode()
    g.add((b, LIKES, PIZZA))
    g.close()
    g = Graph("Sparkles", identifier=C1)
    assert g.open(path) == VALID_STORE
    assert len(g) == 8
    # the binding is in the dataset's prefixes; rdflib's own bindings are not
    assert g.store.dataset.prefixes.get("ex") == str(EX)
    assert "brick" not in g.store.dataset.prefixes
    assert ("ex", URIRef(str(EX))) in set(g.namespaces())
    g.close()
    g.store.destroy(path)
    assert not Path(path).exists()


def test_existing_dataset() -> None:
    ds = sparkles.Dataset()
    ds.load('<urn:example:a> <urn:example:p> "x" .\n_:n <urn:example:p> "y" .', "nt")
    store = SparklesStore(dataset=ds)
    g = Graph(store, identifier=DATASET_DEFAULT_GRAPH_ID)
    assert len(g) == 2
    (stored,) = g.subjects(EX.p, Literal("y"))
    assert isinstance(stored, BNode)
    # a stored blank node read through rdflib names that node in writes
    g.add((stored, EX.q, Literal("z")))
    # gathered writes reach the dataset at the next read through the store or commit()
    g.commit()
    assert ds.ask('ASK { ?n <urn:example:p> "y" ; <urn:example:q> "z" }')
    store.close()
    # the store does not close a dataset it was given
    assert not ds.closed


def test_parse_and_serialize_match_rdflib_memory(store: SparklesStore) -> None:
    lines = []
    for i in range(3000):
        lines.append(f'<urn:example:s{i % 97}> <urn:example:p{i % 7}> "v{i}" .')
        lines.append(f"<urn:example:s{i % 97}> <urn:example:r> _:b{i % 31} .")
        lines.append(f'_:b{i % 31} <urn:example:n> "{i % 31}" .')
    data = "\n".join(lines)
    ours = Graph(store, identifier=C1)
    ours.parse(data=data, format="nt")
    theirs = Graph()
    theirs.parse(data=data, format="nt")
    assert len(ours) == len(theirs)
    from rdflib.compare import isomorphic

    assert isomorphic(ours, theirs)
    again = Graph()
    again.parse(data=ours.serialize(format="turtle"), format="turtle")
    assert isomorphic(again, theirs)


def test_entry_point() -> None:
    eps = importlib.metadata.entry_points(group="rdflib.plugins.store")
    found = [ep for ep in eps if ep.name == "Sparkles"]
    if not found:
        pytest.skip("sparkles-rdf is not installed (the test build has no entry points)")
    assert found[0].load() is SparklesStore
    assert isinstance(Graph("Sparkles").store, SparklesStore)
