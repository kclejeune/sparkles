"""Datasets: opening, loading, quads, dumps and maintenance (A1–A3, A10, A13)."""

from __future__ import annotations

import gzip
import io
from pathlib import Path

import pytest
from conftest import TURTLE, ex, foaf

import sparkles
from sparkles import (
    BlankNode,
    Dataset,
    DatasetLockedError,
    DefaultGraph,
    InvalidInputError,
    Literal,
    NamedNode,
    Quad,
    RdfFormat,
    RdfSyntaxError,
    Triple,
)


def test_a1_memory_dataset() -> None:
    ds = Dataset()
    assert len(ds) == 0 and ds.path is None
    assert ds.load(b'<a:s> <a:p> "x" .', "nt") == 1
    assert len(ds) == 1
    assert Quad(NamedNode("a:s"), NamedNode("a:p"), Literal("x")) in ds
    assert Triple(NamedNode("a:s"), NamedNode("a:p"), Literal("x")) in ds
    assert Quad(NamedNode("a:s"), NamedNode("a:p"), Literal("y")) not in ds
    assert repr(Dataset.memory()) == "<Dataset in memory>"


def test_a2_compressed_inputs(tmp_path: Path) -> None:
    plain = tmp_path / "data.ttl"
    plain.write_text(TURTLE)
    packed = tmp_path / "data.ttl.gz"
    packed.write_bytes(gzip.compress(TURTLE.encode()))
    a, b, c = Dataset(), Dataset(), Dataset()
    n = a.load(path=plain)
    assert n == 9
    assert b.load(path=packed) == n
    assert c.load(gzip.compress(TURTLE.encode()), "turtle") == n
    assert set(a) == set(b) == set(c)
    # a misleading extension: the magic bytes win
    odd = tmp_path / "odd.ttl"
    odd.write_bytes(gzip.compress(TURTLE.encode()))
    assert Dataset().load(path=odd) == n
    with pytest.raises(sparkles.SparklesError):
        Dataset().load(TURTLE, "turtle", compression="zstd")


def test_load_forms(tmp_path: Path) -> None:
    ds = Dataset()
    assert ds.load(io.BytesIO(TURTLE.encode()), RdfFormat.TURTLE) == 9
    assert ds.load(io.StringIO('<http://ex.org/z> <http://ex.org/p> "z" .'), "text/turtle") == 1
    g = NamedNode("http://ex.org/g")
    assert ds.load('<http://ex.org/z> <http://ex.org/p> "z" .', "nt", to_graph=g) == 1
    assert ds.named_graphs() == [g]
    assert ds.load("<s> <p> <o> .", "ttl", base_iri="http://base.org/") == 1
    assert Quad(NamedNode("http://base.org/s"), NamedNode("http://base.org/p"), NamedNode("http://base.org/o")) in ds
    trig = tmp_path / "data.trig"
    trig.write_text("<http://ex.org/q> { <http://ex.org/a> <http://ex.org/b> 1 . }")
    assert ds.load_files([trig]) == 1
    with pytest.raises(ValueError):
        ds.load("<a> <b> <c> .")  # no format
    with pytest.raises(ValueError):
        ds.load("x", "no-such-format")
    with pytest.raises(FileNotFoundError):
        ds.load(path=tmp_path / "missing.ttl")
    with pytest.raises(RdfSyntaxError) as e:
        ds.load("<a> <b> .", "turtle")
    assert isinstance(e.value, SyntaxError)


def test_a3_persistent_dataset(tmp_path: Path) -> None:
    db = tmp_path / "db"
    with Dataset(db) as ds:
        assert ds.path == str(db)
        ds.load(TURTLE, "turtle")
        n = len(ds)
        with pytest.raises(DatasetLockedError) as e:
            Dataset(db)
        assert isinstance(e.value, OSError)
    assert ds.closed
    with pytest.raises(InvalidInputError):
        len(ds)
    again = Dataset.open(db)
    assert len(again) == n
    again.close()


def test_quads_for_pattern(ds: Dataset) -> None:
    assert len(list(ds.quads_for_pattern(ex("alice")))) == 4
    names = {q.object for q in ds.quads_for_pattern(None, foaf("name"))}
    assert names == {Literal("Alice"), Literal("Bob"), Literal("Carol", language="en")}
    assert list(ds.quads_for_pattern(None, None, Literal(17))) == [
        Quad(ex("bob"), foaf("age"), Literal(17))
    ]
    assert list(ds.quads_for_pattern(ex("nobody"))) == []
    assert len(list(ds.quads_for_pattern(graph_name=DefaultGraph()))) == len(ds)
    assert list(ds.quads_for_pattern(graph_name=ex("g"))) == []
    with pytest.raises(TypeError):
        ds.quads_for_pattern(None, "http://xmlns.com/foaf/0.1/name")  # type: ignore[arg-type]


def test_a13_streaming_iteration() -> None:
    ds = Dataset()
    n = 10_000
    ds.extend(Quad(ex(f"s{i}"), ex("p"), Literal(i)) for i in range(n))
    it = ds.quads_for_pattern(None, ex("p"))
    first = next(it)
    ds.add(Quad(ex("late"), ex("p"), Literal(-1)))
    rest = list(it)
    assert len(rest) + 1 == n
    assert first not in rest
    assert Quad(ex("late"), ex("p"), Literal(-1)) not in rest
    assert len(list(ds)) == n + 1


def test_add_remove_extend(ds: Dataset) -> None:
    q = Quad(ex("dave"), foaf("name"), Literal("Dave"), ex("g"))
    assert ds.add(q) is True
    assert ds.add(q) is False
    assert q in ds
    assert ds.remove(q) is True
    assert ds.remove(q) is False
    assert ds.extend([q, Triple(ex("dave"), foaf("age"), Literal(30))]) == 2
    assert ds.named_graphs() == [ex("g")]
    assert ds.clear_graph(ex("g")) == 1
    assert ds.named_graphs() == []
    assert ds.clear_graph(DefaultGraph()) > 0
    assert len(ds) == 0
    ds.load(TURTLE, "turtle")
    ds.clear()
    assert len(ds) == 0


def test_blank_nodes(ds: Dataset) -> None:
    b = BlankNode("x")
    ds.extend([Quad(b, ex("p"), Literal(1)), Quad(b, ex("q"), Literal(2))])
    (q,) = ds.quads_for_pattern(None, ex("p"))
    stored = q.subject
    assert isinstance(stored, BlankNode)
    # the same label within one write names one node; a stored label names it again
    assert len(list(ds.quads_for_pattern(stored))) == 2
    assert ds.remove(Quad(stored, ex("q"), Literal(2)))
    rows = list(ds.query("SELECT ?b WHERE { ?b <http://ex.org/p> 1 }"))
    assert rows[0]["b"] == stored


def test_a10_dump(ds: Dataset, tmp_path: Path) -> None:
    ds.add(Quad(ex("a"), ex("p"), Literal("in g"), ex("g")))
    data = ds.dump(format="nq")
    assert isinstance(data, bytes)
    copy = Dataset()
    copy.load(data, "nq")
    assert set(copy) == set(ds)
    # a triple format writes the default graph, or from_graph
    ttl = ds.dump(format=RdfFormat.TURTLE)
    assert ttl is not None and b"in g" not in ttl and b"Alice" in ttl
    g = ds.dump(format="nt", from_graph=ex("g"))
    assert g == b'<http://ex.org/a> <http://ex.org/p> "in g" .\n'
    # a path takes its format and codec from its name
    out = tmp_path / "out.nq.zst"
    assert ds.dump(out) is None
    assert out.read_bytes()[:4] == b"\x28\xb5\x2f\xfd"
    back = Dataset()
    back.load(path=out)
    assert set(back) == set(ds)
    gz = tmp_path / "out.ttl"
    ds.dump(gz, compression="gzip")
    assert gzip.decompress(gz.read_bytes()) == ttl
    buf = io.BytesIO()
    ds.dump(buf, "trig")
    assert b"ex:g {" in buf.getvalue()
    with pytest.raises(InvalidInputError):
        ds.dump()
    with pytest.raises(FileNotFoundError):
        ds.dump(tmp_path / "missing" / "x.nq")


def test_compact_backup_prefixes(tmp_path: Path) -> None:
    ds = Dataset(tmp_path / "db")
    ds.load(TURTLE, "turtle")
    ds.add(Quad(ex("x"), ex("p"), Literal(1)))
    ds.compact()
    assert len(ds) == 10
    path = ds.backup(tmp_path / "backups")
    restored = Dataset()
    restored.load(path=path)
    assert set(restored) == set(ds)
    assert ds.prefixes["ex"] == "http://ex.org/"
    ds.set_prefix("s", "http://schema.org/")
    assert ds.prefixes["s"] == "http://schema.org/"
    ds.close()


def test_union_default_graph() -> None:
    ds = Dataset(union_default_graph=True)
    ds.add(Quad(ex("a"), ex("p"), Literal(1), ex("g")))
    assert ds.ask("ASK { <http://ex.org/a> ?p ?o }")
    assert Dataset().query("ASK { GRAPH ?g { ?s ?p ?o } }") is False


def test_parse_and_serialize(tmp_path: Path) -> None:
    quads = list(sparkles.parse(TURTLE, "turtle"))
    assert len(quads) == 9 and all(q.graph_name == DefaultGraph() for q in quads)
    nt = sparkles.serialize(quads, format="nt")
    assert nt is not None
    assert len(list(sparkles.parse(nt, "nt"))) == 9
    triples = [q.triple for q in quads]
    ttl = sparkles.serialize(triples, format="turtle", prefixes={"ex": "http://ex.org/"})
    assert ttl is not None and b"@prefix ex:" in ttl
    out = tmp_path / "x.nq"
    sparkles.serialize(quads, out)
    assert len(list(sparkles.parse(path=out))) == 9
    with pytest.raises(InvalidInputError):
        sparkles.serialize([Quad(ex("a"), ex("p"), Literal(1), ex("g"))], format="nt")


def test_formats() -> None:
    assert RdfFormat.from_extension("ttl") == RdfFormat.TURTLE
    assert RdfFormat.from_extension(".nq") == RdfFormat.N_QUADS
    assert RdfFormat.from_media_type("application/trig; charset=utf-8") == RdfFormat.TRIG
    assert RdfFormat.from_extension("unknown") is None
    assert RdfFormat.N_QUADS.supports_datasets and not RdfFormat.TURTLE.supports_datasets
    assert RdfFormat.JSON_LD.media_type == "application/ld+json"
    assert repr(RdfFormat.RDF_XML) == "RdfFormat.RDF_XML"
