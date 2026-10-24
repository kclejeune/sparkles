"""F19's mappings: runs of R2RML, RML and YARRRML texts, and stored mappings."""
from __future__ import annotations

from pathlib import Path

import pytest
from sparkles import Dataset, InvalidInputError, NotFoundError

YARRRML = """prefixes:
  ex: http://ex.org/
mappings:
  person:
    sources:
      - [people.csv~csv]
    s: ex:person/$(id)
    graphs: ex:g
    po:
      - [ex:name, $(name)]
"""


def people(tmp_path: Path, rows: str = "id,name\n1,Ann\n2,Bo\n") -> Path:
    p = tmp_path / "people.csv"
    p.write_text(rows)
    return p


def count(ds: Dataset) -> int:
    return len(list(ds.query("SELECT * { GRAPH <http://ex.org/g> { ?s ?p ?o } }")))


def test_run_mapping_text(tmp_path: Path) -> None:
    ds = Dataset.memory()
    r = ds.run_mapping(YARRRML, sources={"people.csv": people(tmp_path)})
    assert r["stats"]["quads"] == 2
    assert r["receipt"]["committed"]
    assert count(ds) == 2
    # the directory resolves the names the sources do not give
    ds2 = Dataset.memory()
    ds2.run_mapping(YARRRML, directory=tmp_path)
    assert count(ds2) == 2
    with pytest.raises(InvalidInputError):
        Dataset.memory().run_mapping(YARRRML)


def test_stored_mappings(tmp_path: Path) -> None:
    ds = Dataset.memory()
    saved = ds.mappings.put("people", YARRRML, description="People")
    assert saved["created"] and saved["stored"]["definition"]["kind"] == "yarrrml"
    assert saved["summary"]["files"] == ["people.csv"]
    assert [m["name"] for m in ds.mappings.list()] == ["people"]
    assert ds.mappings.get("people")["definition"]["text"] == YARRRML
    assert len(ds.mappings.versions("people")) == 1
    r = ds.mappings.run("people", sources={"people.csv": people(tmp_path)})
    assert r["stats"]["rows"] == 2 and count(ds) == 2
    # replace mode removes what the file no longer holds
    ds.mappings.run("people", sources={"people.csv": people(tmp_path, "id,name\n1,Ann\n")}, replace=True)
    assert count(ds) == 1
    assert ds.mappings.list()[0]["lastRun"]["mode"] == "replace"
    with pytest.raises(NotFoundError):
        ds.mappings.run("missing")
    assert ds.mappings.delete("people")
    assert ds.mappings.get("people") is None
    with pytest.raises(InvalidInputError):
        ds.mappings.put("bad", "mappings: [nope", kind="yarrrml")
