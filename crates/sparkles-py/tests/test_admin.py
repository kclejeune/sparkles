"""History, snapshots and clones, the text and vector indexes, and write-time
validation."""

from __future__ import annotations

from pathlib import Path

import pytest
from conftest import ex

import sparkles
from sparkles import Dataset, Literal, NamedNode, NotFoundError, Quad, Triple, WriteRejectedError


def names(ds: Dataset, at: object = None) -> list[str]:
    rows = ds.query("SELECT ?n WHERE { ?s <http://ex.org/name> ?n } ORDER BY ?n", at=at)
    return [r["n"].value for r in rows]  # type: ignore[union-attr]


# ------------------------------------------------------------------- history ----


def test_commits_and_snapshots(tmp_path: Path) -> None:
    ds = Dataset(tmp_path / "db")
    assert ds.head_commit.seq == 0
    ds.add(Triple(ex("a"), ex("name"), Literal("A")))
    first = ds.head_commit
    assert first.seq == 1 and first.inserted == 1 and first.quads == 1
    assert first.timestamp.endswith("Z")
    snap = ds.snapshots.create("v1", note="before b")
    assert (snap.name, snap.seq, snap.note) == ("v1", 1, "before b")
    ds.add(Triple(ex("b"), ex("name"), Literal("B")))
    ds.update("DELETE DATA { <http://ex.org/a> <http://ex.org/name> 'A' }")
    log = ds.history.commits()
    assert [c.seq for c in log] == [3, 2, 1, 0]
    assert [c.kind for c in log[:3]] == ["update", "transaction", "transaction"]
    assert [c.seq for c in ds.history.commits(2, after=0)] == [1, 2]
    assert [c.seq for c in ds.history.commits(before=2)] == [1, 0]
    # point-in-time queries
    assert names(ds) == ["B"]
    assert names(ds, at="snapshot:v1") == ["A"]
    assert names(ds, at=1) == ["A"]
    assert names(ds, at="commit:2") == ["A", "B"]
    assert [s.name for s in ds.snapshots.list()] == ["v1"]
    h = ds.history.status()
    assert h["head"] == 3 and h["snapshots"] == 1
    assert ds.snapshots.delete("v1") is True
    assert ds.snapshots.delete("v1") is False
    ds.close()


def test_retention_and_memory_history() -> None:
    ds = Dataset()
    h = ds.settings.retention.set(keep_commits=10)
    assert h["retention"]["keepCommits"] == 10
    ds.add(Triple(ex("a"), ex("name"), Literal("A")))
    ds.add(Triple(ex("b"), ex("name"), Literal("B")))
    assert names(ds, at=1) == ["A"]
    with pytest.raises(NotFoundError):
        names(ds, at=99)
    # without a window or a snapshot, an in-memory dataset keeps no past states
    other = Dataset()
    other.add(Triple(ex("a"), ex("name"), Literal("A")))
    other.add(Triple(ex("b"), ex("name"), Literal("B")))
    with pytest.raises(NotFoundError, match="no longer reconstructable"):
        names(other, at=1)


def test_clone(tmp_path: Path) -> None:
    ds = Dataset(tmp_path / "db")
    ds.add(Triple(ex("a"), ex("name"), Literal("A")))
    ds.add(Quad(ex("a"), ex("p"), Literal(1), ex("g")))
    ds.snapshots.create("one")
    ds.add(Triple(ex("b"), ex("name"), Literal("B")))
    report = ds.clone_to(tmp_path / "copy", graphs=["default"])
    assert report["quads"] == 2 and report["sourceQuads"] == 3
    old = ds.clone_to(tmp_path / "old", at="snapshot:one")
    assert old["commit"] == 2
    ds.close()
    with Dataset(tmp_path / "copy") as copy:
        assert names(copy) == ["A", "B"]
        assert copy.named_graphs() == []
    with Dataset(tmp_path / "old") as copy:
        assert names(copy) == ["A"]


def test_clone_can_exclude_the_inferences(tmp_path: Path) -> None:
    inferred = NamedNode("urn:x-sparkles:inferred")
    with Dataset(tmp_path / "db") as ds:
        ds.load(
            "@prefix ex: <http://ex.org/> . @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n"
            "ex:Dog rdfs:subClassOf ex:Animal . ex:rex a ex:Dog .",
            "turtle",
        )
        inferred_quads = ds.reasoning.run("rdfs").inferred
        assert inferred_quads > 0 and inferred in ds.named_graphs()
        kept = ds.clone_to(tmp_path / "kept")
        dropped = ds.clone_to(tmp_path / "dropped", inferences="drop")
        assert kept["quads"] == dropped["quads"] + inferred_quads
        assert dropped["sourceQuads"] == kept["sourceQuads"]
    with Dataset(tmp_path / "dropped") as copy:
        assert inferred not in copy.named_graphs()
        assert len(copy) == 2


# ----------------------------------------------------------- text and vectors ----


@pytest.mark.skipif("text" not in sparkles.FEATURES, reason="built without text")
def test_text_index() -> None:
    ds = Dataset()
    ds.extend(
        [
            Triple(ex("a"), ex("label"), Literal("the quick brown fox")),
            Triple(ex("b"), ex("label"), Literal("a lazy dog")),
        ]
    )
    assert ds.indexes.text.status() is None
    status = ds.indexes.text.enable({"predicates": ["http://ex.org/label"]})
    assert status["enabled"] is True and status["docs"] == 2
    q = "PREFIX text: <http://jena.apache.org/text#> SELECT ?s WHERE { ?s text:query 'fox' }"
    assert [r["s"] for r in ds.query(q)] == [ex("a")]
    ds.add(Triple(ex("c"), ex("label"), Literal("another fox")))
    assert {r["s"] for r in ds.query(q)} == {ex("a"), ex("c")}
    assert ds.indexes.text.rebuild()["docs"] == 3
    with pytest.raises(ValueError):
        ds.indexes.text.enable({"predicates": 5})
    ds.indexes.text.disable()
    assert ds.indexes.text.status() is None


def test_vector_index() -> None:
    ds = Dataset()
    vec = NamedNode("urn:x-sparkles:vector")
    vectors = {"a": "[1.0,0.0,0.0]", "b": "[0.0,1.0,0.0]", "c": "[0.9,0.1,0.0]"}
    ds.extend(Triple(ex(k), ex("emb"), Literal(v, datatype=vec)) for k, v in vectors.items())
    assert ds.indexes.vector.put("emb", {"predicate": ex("emb").value, "dimension": 3, "metric": "cosine"}) is True
    status = ds.indexes.vector.wait("emb")
    assert status is not None and status["name"] == "emb"
    assert [s["name"] for s in ds.indexes.vector.list()] == ["emb"]
    q = """PREFIX spk: <urn:x-sparkles:>
    SELECT ?s WHERE { (?s ?score) spk:vectorSearch (<http://ex.org/emb> "[1.0,0.0,0.0]"^^spk:vector 2) }
    ORDER BY DESC(?score)"""
    assert [r["s"] for r in ds.query(q)] == [ex("a"), ex("c")]
    with pytest.raises(ValueError):
        ds.indexes.vector.put("bad", {"predicate": ex("emb2").value, "dimension": 3, "metric": "nonsense"})
    ds.indexes.vector.rebuild("emb")
    ds.indexes.vector.drop("emb")
    assert ds.indexes.vector.list() == []
    assert ds.indexes.vector.get("emb") is None


def test_embeddings_on_write() -> None:
    import json
    import threading
    from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

    received: list[str] = []

    class Handler(BaseHTTPRequestHandler):
        def do_POST(self) -> None:
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            received.extend(body["input"])
            data = [
                {"index": i, "embedding": [float(len(t)), 1.0, 0.0]}
                for i, t in enumerate(body["input"])
            ]
            out = json.dumps({"data": data}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(out)))
            self.end_headers()
            self.wfile.write(out)

        def log_message(self, *args: object) -> None:
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        ds = Dataset()
        ds.extend(
            [
                Triple(ex("a"), ex("label"), Literal("ab")),
                Triple(ex("b"), ex("label"), Literal("abcd")),
            ]
        )
        url = f"http://127.0.0.1:{server.server_address[1]}/v1/embeddings"
        embedding = {"url": url, "model": "m", "predicates": ["http://ex.org/label"]}
        ds.indexes.vector.put("names", {"predicate": ex("emb").value, "dimension": 3, "embedding": embedding})
        ds.indexes.vector.embed_until_idle(timeout=30)
        assert sorted(received) == ["ab", "abcd"]
        status = ds.indexes.vector.get("names")
        assert status is not None and status["embedding"]["embedded"] == 2
        q = """PREFIX spk: <urn:x-sparkles:>
        SELECT ?s WHERE { (?s ?score) spk:vectorSearch (<http://ex.org/emb> "abcd" 1) }"""
        assert [r["s"] for r in ds.query(q)] == [ex("b")]
        ds.indexes.vector.reembed("names")
        ds.indexes.vector.embed_until_idle(timeout=30)
        # the query text equals a stored input, so it came from the cache
        assert len(received) == 4
    finally:
        server.shutdown()


# ------------------------------------------------------------ write validation ----

SHAPES = """
@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix ex: <http://ex.org/> .
ex:PersonShape a sh:NodeShape ; sh:targetClass ex:Person ;
  sh:property [ sh:path ex:name ; sh:minCount 1 ] .
"""
RDF_TYPE = NamedNode("http://www.w3.org/1999/02/22-rdf-syntax-ns#type")


@pytest.mark.skipif("shacl" not in sparkles.FEATURES, reason="built without shacl")
def test_shacl_write_validation(tmp_path: Path) -> None:
    ds = Dataset(tmp_path / "db")
    ds.extend([Triple(ex("alice"), RDF_TYPE, ex("Person")), Triple(ex("alice"), ex("name"), Literal("Alice"))])
    out = ds.validation.guard.set({"mode": "reject", "shapes": {"inline": SHAPES}})
    assert out["status"] == "installed"
    assert out["summary"]["conforms"] is True
    status = ds.validation.guard.get()
    assert status is not None and status["language"] == "shacl" and status["config"]["mode"] == "reject"
    with pytest.raises(WriteRejectedError):
        ds.add(Triple(ex("bob"), RDF_TYPE, ex("Person")))
    ds.extend([Triple(ex("bob"), RDF_TYPE, ex("Person")), Triple(ex("bob"), ex("name"), Literal("Bob"))])
    ds.close()
    # a reopened database validates its writes again
    ds = Dataset(tmp_path / "db")
    assert ds.validation.guard.get() is not None
    with pytest.raises(WriteRejectedError):
        ds.add(Triple(ex("carol"), RDF_TYPE, ex("Person")))
    assert ds.validation.guard.reset() is None
    assert ds.validation.guard.get() is None
    # resetting an absent guard is not an error
    assert ds.validation.guard.reset() is None
    ds.add(Triple(ex("carol"), RDF_TYPE, ex("Person")))
    # a configuration the data does not meet is refused in reject mode
    out = ds.validation.guard.set({"mode": "reject", "shapes": {"inline": SHAPES}})
    assert out["status"] == "not-conforming"
    with pytest.raises(ValueError):
        ds.validation.guard.set({"mode": "reject", "shapez": {}})


@pytest.mark.skipif("shex" not in sparkles.FEATURES, reason="built without shex")
def test_shex_write_validation() -> None:
    ds = Dataset()
    schema = "PREFIX ex: <http://ex.org/> ex:Person { ex:name . }"
    out = ds.validation.guard.set(
        {
            "language": "shex",
            "mode": "reject",
            "schema": {"inline": schema},
            "shapeMap": "{FOCUS a <http://ex.org/Person>}@<http://ex.org/Person>",
        }
    )
    assert out["status"] == "installed"
    assert ds.validation.guard.get()["language"] == "shex"  # type: ignore[index]
    with pytest.raises(WriteRejectedError):
        ds.add(Triple(ex("bob"), RDF_TYPE, ex("Person")))
