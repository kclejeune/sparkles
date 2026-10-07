"""Streaming `parse` and file-object loads, and SPARQL results serialization."""

from __future__ import annotations

import csv
import bz2
import gzip
import io
import json
import lzma
import xml.etree.ElementTree as ET
from pathlib import Path

import pytest
from conftest import ex

import sparkles
from sparkles import Dataset, InvalidInputError, Literal, NamedNode, Quad, RdfSyntaxError, Triple


class Chunks(io.RawIOBase):
    """A binary file object that hands out its data in small pieces and counts reads."""

    def __init__(self, data: bytes, size: int = 64) -> None:
        self.data = data
        self.size = size
        self.pos = 0
        self.reads = 0

    def readable(self) -> bool:
        return True

    def read(self, n: int = -1) -> bytes:
        self.reads += 1
        chunk = self.data[self.pos : self.pos + min(self.size, n if n >= 0 else self.size)]
        self.pos += len(chunk)
        return chunk


def nt(n: int) -> bytes:
    return "".join(f'<http://ex.org/s{i}> <http://ex.org/p> "{i}" .\n' for i in range(n)).encode()


def test_parse_streams_a_file_object() -> None:
    src = Chunks(nt(5000))
    it = sparkles.parse(src, "nt")
    first = next(it)
    assert first == Quad(ex("s0"), ex("p"), Literal("0"))
    # the first quads come before the input has been read to the end
    assert src.pos < len(src.data)
    assert len(list(it)) == 4999
    assert src.pos == len(src.data)


def test_parse_reports_errors_after_the_quads_before_them() -> None:
    it = sparkles.parse(nt(3) + b"<http://ex.org/bad> oops .\n", "nt")
    assert len([next(it) for _ in range(3)]) == 3
    with pytest.raises(RdfSyntaxError):
        next(it)


def test_parse_compressed_text_and_paths(tmp_path: Path) -> None:
    data = nt(10)
    assert len(list(sparkles.parse(io.BytesIO(gzip.compress(data)), "nt"))) == 10
    assert len(list(sparkles.parse(gzip.compress(data), "nt", compression="gzip"))) == 10
    assert len(list(sparkles.parse(io.StringIO(data.decode()), "nt"))) == 10
    p = tmp_path / "data.nt.gz"
    p.write_bytes(gzip.compress(data))
    assert len(list(sparkles.parse(path=p))) == 10
    with pytest.raises(InvalidInputError):
        list(sparkles.parse(data, "nt", compression="zstd"))
    with pytest.raises(ValueError, match="format"):
        sparkles.parse(io.BytesIO(data))


def test_parse_base_iri_and_blank_nodes() -> None:
    quads = list(sparkles.parse("<a> <b> _:x . _:x <c> <d> .", "turtle", base_iri="http://ex.org/"))
    assert quads[0].subject == ex("a")
    # one label, one node
    assert quads[0].object == quads[1].subject


def test_load_streams_a_file_object() -> None:
    ds = Dataset()
    src = Chunks(gzip.compress(nt(3000)), size=4096)
    assert ds.load(src, "nt") == 3000
    assert src.reads > 1
    assert len(ds) == 3000
    # into a graph, with the document's prefixes
    ttl = b"@prefix ex: <http://ex.org/> . ex:a ex:p _:b . _:b ex:q 1 ."
    assert ds.load(io.BytesIO(ttl), "turtle", to_graph=ex("g")) == 2
    assert ds.prefixes["ex"] == "http://ex.org/"
    (q,) = ds.quads_for_pattern(ex("a"), ex("p"), None, ex("g"))
    assert ds.ask("ASK { GRAPH <http://ex.org/g> { ?b <http://ex.org/q> 1 } }", bindings={"b": q.object})


def test_failed_file_object_load_changes_nothing() -> None:
    ds = Dataset()
    with pytest.raises(RdfSyntaxError):
        ds.load(io.BytesIO(nt(100) + b"broken"), "nt")
    assert len(ds) == 0


@pytest.mark.parametrize("mode", ["auto", "streaming", "buffered"])
@pytest.mark.parametrize("compress,extension", [(lzma.compress, "xz"), (bz2.compress, "bz2")])
def test_load_modes_with_native_compression(mode, compress, extension, tmp_path: Path) -> None:
    data = compress(nt(100))
    path = tmp_path / f"data.nt.{extension}"
    path.write_bytes(data)
    ds = Dataset()
    assert ds.load(path=path, parse_mode=mode, auto_buffer_bytes=1024) == 100
    assert ds.load_files([path], parse_mode=mode, auto_buffer_bytes=0) == 0
    assert Dataset().load(Chunks(data), "nt", parse_mode=mode) == 100


class BoundedReads(Chunks):
    def read(self, n: int = -1) -> bytes:
        assert n >= 0, "the loader requested the whole file"
        return super().read(n)


def test_replayable_and_table_file_objects_use_bounded_reads() -> None:
    assert Dataset().load(BoundedReads(nt(100)), "nt", parse_mode="buffered") == 100
    assert Dataset().load(BoundedReads(b"id,name\n1,Alice\n2,Bob\n"), "csv", key="id", base_iri="http://e/") == 4


def test_jsonld_streaming_profile_is_opt_in() -> None:
    late = b'{"@id":"urn:s","p":"value","@context":{"p":"urn:p"}}'
    ordered = b'{"@context":{"p":"urn:p"},"@id":"urn:s","p":"value"}'
    assert Dataset().load(late, "jsonld", parse_mode="streaming") == 1
    ds = Dataset()
    with pytest.raises(RdfSyntaxError):
        ds.load(late, "jsonld-streaming", parse_mode="streaming")
    assert len(ds) == 0
    assert ds.load(ordered, "jsonld-streaming", parse_mode="streaming") == 1


# -------------------------------------------------------------- results output ----


@pytest.fixture
def people() -> Dataset:
    ds = Dataset()
    ds.extend(
        [
            Triple(ex("alice"), ex("name"), Literal("Alice")),
            Triple(ex("bob"), ex("name"), Literal("Bob", language="en")),
            Triple(ex("bob"), ex("age"), Literal(17)),
        ]
    )
    return ds


Q = "SELECT ?s ?name ?age WHERE { ?s <http://ex.org/name> ?name OPTIONAL { ?s <http://ex.org/age> ?age } } ORDER BY ?s"


def test_serialize_solutions_json(people: Dataset) -> None:
    out = people.select(Q).serialize()
    assert out is not None
    doc = json.loads(out)
    assert doc["head"]["vars"] == ["s", "name", "age"]
    rows = doc["results"]["bindings"]
    assert rows[0]["s"] == {"type": "uri", "value": "http://ex.org/alice"}
    assert "age" not in rows[0]
    assert rows[1]["name"]["xml:lang"] == "en"


def test_serialize_solutions_other_formats(people: Dataset, tmp_path: Path) -> None:
    xml = people.select(Q).serialize(format="xml")
    assert xml is not None
    assert ET.fromstring(xml).tag.endswith("sparql")
    text = people.select(Q).serialize(format="text/csv")
    assert text is not None
    rows = list(csv.reader(io.StringIO(text.decode())))
    assert rows[0] == ["s", "name", "age"]
    assert rows[2] == ["http://ex.org/bob", "Bob", "17"]
    tsv = people.select(Q).serialize(format="tsv")
    assert tsv is not None and tsv.decode().splitlines()[0] == "?s\t?name\t?age"
    path = tmp_path / "out.srj"
    assert people.select(Q).serialize(path) is None
    assert json.loads(path.read_bytes())["head"]["vars"] == ["s", "name", "age"]
    buf = io.BytesIO()
    people.select(Q).serialize(buf, "json")
    assert json.loads(buf.getvalue())["results"]["bindings"]


def test_serialize_solutions_once_and_before_iterating(people: Dataset) -> None:
    rows = people.select(Q)
    next(rows)
    with pytest.raises(InvalidInputError):
        rows.serialize()
    rows = people.select(Q)
    rows.serialize()
    with pytest.raises(InvalidInputError):
        rows.serialize()
    assert list(rows) == []
    with pytest.raises(ValueError):
        people.select(Q).serialize(format="yaml")


def test_serialize_triples(people: Dataset) -> None:
    made = people.construct("CONSTRUCT { ?s <http://ex.org/label> ?n } WHERE { ?s <http://ex.org/name> ?n }")
    out = made.serialize(format="nt")
    assert out is not None
    assert sorted(out.decode().splitlines()) == [
        '<http://ex.org/alice> <http://ex.org/label> "Alice" .',
        '<http://ex.org/bob> <http://ex.org/label> "Bob"@en .',
    ]
    ttl = people.construct("CONSTRUCT WHERE { ?s ?p ?o }").serialize(prefixes={"ex": "http://ex.org/"})
    assert ttl is not None and b"@prefix ex: <http://ex.org/>" in ttl
    again = Dataset()
    again.load(ttl, "turtle")
    assert len(again) == 3
    assert NamedNode("http://ex.org/alice") in {q.subject for q in again}
