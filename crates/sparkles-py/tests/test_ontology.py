"""The ontology handle (spec F21 Phase 2): summaries, axioms, checks, edits and diffs."""

from __future__ import annotations

import pytest
import sparkles

pytestmark = pytest.mark.skipif(
    "ontology" not in sparkles.FEATURES, reason="built without the ontology handle"
)

EX = "http://ex.org/"
PREFIXES = """PREFIX : <http://ex.org/>
PREFIX owl: <http://www.w3.org/2002/07/owl#>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
"""
BODY = """:o a owl:Ontology . :Professor a owl:Class . :Course a owl:Class .
:teaches a owl:ObjectProperty .
:Professor rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :teaches ;
                             owl:someValuesFrom :Course ] .
:ann a :Professor ."""
A1 = f"SubClassOf(<{EX}Professor> ObjectSomeValuesFrom(<{EX}teaches> <{EX}Course>))"


@pytest.fixture
def ds(tmp_path) -> sparkles.Dataset:
    # on disk, so that the diff can read the earlier commit
    d = sparkles.Dataset(str(tmp_path / "db"))
    d.update(f"{PREFIXES} INSERT DATA {{ {BODY} }}")
    return d


def test_axioms_are_frozen_values_with_structural_equality() -> None:
    a = sparkles.Axiom(A1)
    b = sparkles.Axiom.parse(
        "SubClassOf(ex:Professor ObjectSomeValuesFrom(ex:teaches ex:Course))",
        prefixes={"ex": EX},
    )
    assert a == b and hash(a) == hash(b)
    assert len({a, b}) == 1
    assert str(a) == A1
    assert a.type == "SubClassOf"
    assert sparkles.Axiom.from_json(a.json()) == a
    assert a.mentions(EX + "teaches")
    assert a.ofn({"ex": EX}).startswith("SubClassOf(ex:Professor")
    with pytest.raises(AttributeError):
        a.x = 1  # type: ignore[attr-defined]
    with pytest.raises(ValueError):
        sparkles.Axiom("SubClassOf(")
    with pytest.raises(ValueError):
        sparkles.Axiom.from_json({"type": "DisjointClasses", "classes": [EX + "A"]})


def test_summary_and_axioms(ds: sparkles.Dataset) -> None:
    s = ds.ontology.summary()
    assert s["header"]["iri"] == EX + "o"
    assert s["axioms"]["SubClassOf"] == 1
    page = ds.ontology.axioms(type="SubClassOf")
    assert page["total"] == 1
    entry = page["axioms"][0]
    assert entry["axiom"] == sparkles.Axiom(A1)
    assert entry["graph"] == "default"
    # assertions stream from the store when asked
    about = ds.ontology.axioms(about=EX + "ann", assertions=True)
    assert [e["type"] for e in about["axioms"]] == ["ClassAssertion"]
    with pytest.raises(sparkles.NotFoundError):
        ds.ontology.axioms(about=EX + "nobody")
    paged = ds.ontology.axioms(limit=1)
    assert len(paged["axioms"]) == 1 and paged["next"] == 1


def test_checks(ds: sparkles.Dataset) -> None:
    r = ds.ontology.check(profiles=["el", "ql"])
    assert [x["profile"] for x in r["reports"]] == ["el", "ql"]
    assert r["conforms"]


def test_edits_and_diffs(ds: sparkles.Dataset) -> None:
    head = ds.ontology.summary()["commit"]
    done = ds.ontology.edit(remove=[sparkles.Axiom(A1)], add=f"SubClassOf(<{EX}Course> owl:Thing)")
    assert done["removed"] == 1 and done["added"] == 1 and done["committed"]
    assert sparkles.Axiom(A1) not in [e["axiom"] for e in ds.ontology.axioms()["axioms"]]
    d = ds.ontology.diff(head)
    assert d["addedCount"] == 1 and d["removedCount"] == 1
    assert d["removed"][0]["axiom"] == sparkles.Axiom(A1)
    gone = ds.ontology.remove_entity(EX + "Course")
    assert gone["removed"] >= 1
    with pytest.raises(ValueError):
        ds.ontology.edit()
    with pytest.raises(TypeError):
        ds.ontology.edit(add=[3])


@pytest.mark.skipif("reasoning" not in sparkles.FEATURES, reason="built without reasoning")
def test_inferred_views() -> None:
    d = sparkles.Dataset()
    d.update(
        f"""{PREFIXES} INSERT DATA {{ :A a owl:Class . :B a owl:Class . :C a owl:Class .
        :A rdfs:subClassOf :B . :B rdfs:subClassOf :C . }}"""
    )
    a_c = sparkles.Axiom(f"SubClassOf(<{EX}A> <{EX}C>)")
    plain = d.ontology.axioms(type="SubClassOf", inferences=True)
    assert plain["inferences"] == {"included": False}
    d.reasoning.run(profile="rdfs")
    inferred = d.ontology.axioms(type="SubClassOf", inferences=True, limit=1000)
    assert inferred["inferences"]["included"] and inferred["inferences"]["stale"] is False
    assert a_c in [e["axiom"] for e in inferred["axioms"]]
    assert a_c not in [e["axiom"] for e in d.ontology.axioms(type="SubClassOf")["axioms"]]
