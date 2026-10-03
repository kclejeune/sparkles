"""Jena's syntaxes (TriX, RDF Thrift, RDF Protobuf, RDF/JSON), CSV and TSV tables, and
the DESCRIBE options of queries and transactions."""

from __future__ import annotations

import gzip
import io
from pathlib import Path

import pytest

import sparkles
from sparkles import Dataset, RdfFormat

TRIG = """@prefix ex: <http://example.org/> .
ex:a ex:p 1 ; ex:q "x"@en .
ex:g { ex:b ex:p ex:c }
"""


def sample() -> Dataset:
    ds = Dataset()
    ds.load(TRIG, "trig")
    return ds


def test_jena_formats_are_formats() -> None:
    assert RdfFormat.from_extension("trix") == RdfFormat.TRIX
    assert RdfFormat.from_extension(".rt") == RdfFormat.RDF_THRIFT
    assert RdfFormat.from_extension("rpb") == RdfFormat.RDF_PROTOBUF
    assert RdfFormat.from_extension("rj") == RdfFormat.RDF_JSON
    assert RdfFormat.from_media_type("application/rdf+thrift") == RdfFormat.RDF_THRIFT
    assert RdfFormat.TRIX.supports_datasets and not RdfFormat.RDF_JSON.supports_datasets
    assert RdfFormat.RDF_JSON.media_type == "application/rdf+json"
    assert repr(RdfFormat.RDF_PROTOBUF) == "RdfFormat.RDF_PROTOBUF"
    assert str(RdfFormat.TRIX) == "TriX"


@pytest.mark.parametrize(
    "fmt", [RdfFormat.TRIX, RdfFormat.RDF_THRIFT, RdfFormat.RDF_PROTOBUF, "trix", "rt", "rpb"]
)
def test_quad_syntaxes_round_trip(fmt: RdfFormat | str, tmp_path: Path) -> None:
    ds = sample()
    data = ds.dump(format=fmt)
    assert isinstance(data, bytes)
    back = Dataset()
    assert back.load(data, fmt) == 3
    assert len(back) == 3
    assert sorted(map(str, back)) == sorted(map(str, ds))
    # parse and serialize too
    quads = list(sparkles.parse(data, fmt))
    assert len(quads) == 3
    again = sparkles.serialize(quads, format=fmt)
    assert len(list(sparkles.parse(again, fmt))) == 3


def test_files_by_extension_and_compression(tmp_path: Path) -> None:
    ds = sample()
    for name in ["d.trix", "d.rt.gz", "d.rpb.zst"]:
        path = tmp_path / name
        ds.dump(path)
        back = Dataset()
        assert back.load(path=path) == 3, name
        assert back.load_files([path]) == 0, name
    raw = gzip.decompress((tmp_path / "d.rt.gz").read_bytes())
    assert raw == ds.dump(format=RdfFormat.RDF_THRIFT)


def test_rdf_json_holds_the_default_graph() -> None:
    ds = sample()
    rj = ds.dump(format=RdfFormat.RDF_JSON)
    assert b'"http://example.org/a"' in rj and b"http://example.org/b" not in rj
    back = Dataset()
    assert back.load(rj, "rj", to_graph="http://example.org/h") == 2
    assert len(list(back.quads_for_pattern(graph_name="http://example.org/h"))) == 2
    # a file object is read too
    assert Dataset().load(io.BytesIO(rj), RdfFormat.RDF_JSON) == 2
    with pytest.raises(sparkles.SparklesError):
        sparkles.serialize(list(ds), format=RdfFormat.RDF_JSON)


def test_a_broken_document_names_the_syntax() -> None:
    with pytest.raises(sparkles.SparklesError, match="TriX"):
        Dataset().load("<trix><graph>", "trix")


def test_shapes_are_not_read_in_jena_syntaxes() -> None:
    with pytest.raises(ValueError, match="shapes"):
        sample().validate_shacl("", format="trix")


def test_csv_and_tsv_tables(tmp_path: Path) -> None:
    ds = Dataset()
    csv = tmp_path / "people.csv"
    csv.write_text("id,name\n7,Ann\n8,Bob\n")
    assert ds.load(path=csv, base_iri="http://e/", key="id") == 4
    assert ds.ask('ASK { <http://e/7> <http://e/name> "Ann" }')
    # input bytes with a format, into a graph, gzip-compressed
    tsv = gzip.compress(b"id\tname\n9\tCy\n")
    n = ds.load(tsv, "tsv", base_iri="http://e/", key="id", to_graph="http://e/t")
    assert n == 2
    assert ds.ask('ASK { GRAPH <http://e/t> { <http://e/9> <http://e/name> "Cy" } }')
    # a CSVW mapping
    meta = tmp_path / "m.json"
    meta.write_text(
        '{"@context": "http://www.w3.org/ns/csvw", "tableSchema": {'
        '"aboutUrl": "http://e/p/{id}", "columns": ['
        '{"name": "id", "datatype": "integer", "propertyUrl": "http://e/id"},'
        '{"name": "name", "propertyUrl": "http://e/n"}]}}'
    )
    assert ds.load(path=csv, mapping=meta) == 4
    assert ds.ask("ASK { <http://e/p/8> <http://e/id> 8 }")
    # a template
    rq = tmp_path / "t.rq"
    rq.write_text("CONSTRUCT { ?s <http://e/label> ?name } WHERE { BIND(IRI(CONCAT('http://e/x/', ?id)) AS ?s) }")
    assert ds.load(path=csv, template=rq) == 2
    assert ds.ask('ASK { <http://e/x/7> <http://e/label> "Ann" }')
    with pytest.raises(ValueError, match="CSV and TSV"):
        ds.load(TRIG, "trig", key="id")


def describe_data() -> Dataset:
    ds = Dataset()
    ds.load(
        """@prefix ex: <http://example.org/> .
        @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
        ex:a ex:knows ex:b ; ex:p [ ex:q 1 ] .
        ex:z ex:knows ex:a .
        ex:b rdfs:label "B" .
        """,
        "ttl",
    )
    return ds


Q = "DESCRIBE <http://example.org/a>"


def count(triples: object) -> int:
    return len(list(triples))  # type: ignore[call-overload]


def test_describe_options_on_queries() -> None:
    ds = describe_data()
    assert count(ds.construct(Q)) == 3
    # incoming triples
    assert count(ds.construct(Q, describe="scbd")) == 4
    assert count(ds.query(Q, describe={"mode": "outgoing"})) == 2
    # labels of linked IRIs, and limits
    assert count(ds.construct(Q, describe={"labels": True})) == 4
    assert count(ds.construct(Q, describe={"max_depth": 1})) == 2
    assert count(ds.construct(Q, describe={"mode": "scbd", "max_triples": 2})) == 2
    assert count(ds.construct(Q, describe={"max_triples": None})) == 3
    with pytest.raises(sparkles.SparklesError):
        ds.construct(Q, describe="everything")
    with pytest.raises(sparkles.SparklesError):
        ds.construct(Q, describe={"colour": "blue"})
    with pytest.raises(TypeError):
        ds.construct(Q, describe=3)  # type: ignore[arg-type]


def test_describe_options_in_transactions(tmp_path: Path) -> None:
    ds = Dataset(tmp_path / "db")
    ds.load(
        "<http://example.org/a> <http://example.org/p> 1 . "
        "<http://example.org/z> <http://example.org/p> <http://example.org/a> .",
        "ttl",
    )
    with ds.transaction() as tx:
        assert count(tx.query(Q)) == 1
        assert count(tx.query(Q, describe="scbd")) == 2
