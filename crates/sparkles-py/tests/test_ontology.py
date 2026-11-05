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
    done = ds.ontology.edit(
        remove=[sparkles.Axiom(A1)], add=f"SubClassOf(<{EX}Course> owl:Thing)"
    )
    assert done["removed"] == 1 and done["added"] == 1 and done["committed"]
    assert sparkles.Axiom(A1) not in [
        e["axiom"] for e in ds.ontology.axioms()["axioms"]
    ]
    d = ds.ontology.diff(head)
    assert d["addedCount"] == 1 and d["removedCount"] == 1
    assert d["removed"][0]["axiom"] == sparkles.Axiom(A1)
    gone = ds.ontology.remove_entity(EX + "Course")
    assert gone["removed"] >= 1
    with pytest.raises(ValueError):
        ds.ontology.edit()
    with pytest.raises(TypeError):
        ds.ontology.edit(add=[3])


@pytest.mark.skipif(
    "reasoning" not in sparkles.FEATURES, reason="built without reasoning"
)
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
    assert (
        inferred["inferences"]["included"] and inferred["inferences"]["stale"] is False
    )
    assert a_c in [e["axiom"] for e in inferred["axioms"]]
    assert a_c not in [
        e["axiom"] for e in d.ontology.axioms(type="SubClassOf")["axioms"]
    ]


# --------------------------------------------------- roadmap item 540: accessors ----


def test_entities_and_entity_accessors(ds: sparkles.Dataset) -> None:
    page = ds.ontology.entities(kind="Class")
    assert {e["iri"] for e in page["entities"]} == {EX + "Professor", EX + "Course"}
    assert page["total"] == 2 and page["next"] is None
    both = ds.ontology.entities(kind=["Class", "ObjectProperty"], limit=1)
    assert both["total"] == 3 and both["next"] == 1
    prof = ds.ontology.entity(EX + "Professor")
    assert prof["kinds"] == ["Class"]
    (sup,) = prof["class"]["superClasses"]
    assert sup["expression"]["type"] == "ObjectSomeValuesFrom"
    assert prof["class"]["instances"] == []
    # the class assertion of :ann stays in the store until assertions are asked for
    streamed = ds.ontology.entity(sparkles.NamedNode(EX + "Professor"), assertions=True)
    assert [i["expression"] for i in streamed["class"]["instances"]] == [EX + "ann"]
    teaches = ds.ontology.entity(EX + "teaches", kind="ObjectProperty")
    assert teaches["objectProperty"]["domains"] == []
    with pytest.raises(sparkles.NotFoundError):
        ds.ontology.entity(EX + "teaches", kind="Class")
    with pytest.raises(sparkles.NotFoundError):
        ds.ontology.entity(EX + "nothing")


def test_individual_accessors_stream_assertions(ds: sparkles.Dataset) -> None:
    ds.update(
        f"{PREFIXES} INSERT DATA {{ :ann a owl:NamedIndividual ; :teaches :bob . }}"
    )
    ann = ds.ontology.entity(EX + "ann", assertions=True)
    ind = ann["namedIndividual"]
    assert [t["expression"] for t in ind["types"]] == [EX + "Professor"]
    (pa,) = ind["assertions"]
    assert isinstance(pa["axiom"], sparkles.Axiom)
    assert pa["type"] == "ObjectPropertyAssertion"
    assert ds.ontology.entity(EX + "ann")["namedIndividual"]["types"] == []


# ------------------------------------------ roadmap item 540: typed constructors ----


def test_numeric_literals_take_xml_schema_lexical_forms() -> None:
    from sparkles import owl

    xsd = "http://www.w3.org/2001/XMLSchema#"
    assert owl.Literal(float("inf")) == {"@value": "INF", "@type": xsd + "double"}
    assert owl.Literal(float("-inf"))["@value"] == "-INF"
    assert owl.Literal(float("nan"))["@value"] == "NaN"
    assert owl.Literal(10**21) == {"@value": "1" + "0" * 21, "@type": xsd + "integer"}
    assert owl.Literal(float("inf"), datatype=xsd + "float")["@value"] == "INF"


def test_typed_constructors_build_axioms() -> None:
    from sparkles import owl

    ax = owl.SubClassOf(
        owl.Class(EX + "Professor"),
        owl.ObjectSomeValuesFrom(
            owl.ObjectProperty(EX + "teaches"), owl.Class(EX + "Course")
        ),
    )
    assert ax == sparkles.Axiom(A1)
    assert owl.Declaration(owl.Class(EX + "A")) == sparkles.Axiom(
        f"Declaration(Class(<{EX}A>))"
    )
    labelled = owl.AnnotationAssertion(
        "http://www.w3.org/2000/01/rdf-schema#label", sparkles.NamedNode(EX + "A"), "A"
    )
    assert str(labelled) == f'AnnotationAssertion(rdfs:label <{EX}A> "A")'
    seen = owl.AnnotationAssertion(
        "http://www.w3.org/2000/01/rdf-schema#seeAlso", EX + "A", owl.IRI(EX + "B")
    )
    assert str(seen) == f"AnnotationAssertion(rdfs:seeAlso <{EX}A> <{EX}B>)"
    xsd = owl.XSD
    cases = [
        (
            owl.EquivalentClasses(
                EX + "A",
                owl.ObjectIntersectionOf(EX + "B", owl.ObjectComplementOf(EX + "C")),
            ),
            f"EquivalentClasses(<{EX}A> ObjectIntersectionOf(<{EX}B> ObjectComplementOf(<{EX}C>)))",
        ),
        (
            owl.DisjointUnion(EX + "A", EX + "B", EX + "C"),
            f"DisjointUnion(<{EX}A> <{EX}B> <{EX}C>)",
        ),
        (
            owl.SubObjectPropertyOf(
                owl.ObjectPropertyChain(EX + "p", EX + "q"), EX + "r"
            ),
            f"SubObjectPropertyOf(ObjectPropertyChain(<{EX}p> <{EX}q>) <{EX}r>)",
        ),
        (
            owl.InverseObjectProperties(EX + "p", owl.ObjectInverseOf(EX + "q")),
            f"InverseObjectProperties(<{EX}p> ObjectInverseOf(<{EX}q>))",
        ),
        (owl.TransitiveObjectProperty(EX + "p"), f"TransitiveObjectProperty(<{EX}p>)"),
        (
            owl.DataPropertyRange(
                EX + "d",
                owl.DatatypeRestriction(xsd + "integer", (xsd + "minInclusive", 5)),
            ),
            f'DataPropertyRange(<{EX}d> DatatypeRestriction(xsd:integer xsd:minInclusive "5"^^xsd:integer))',
        ),
        (
            owl.SubClassOf(EX + "A", owl.ObjectMaxCardinality(1, EX + "p", EX + "B")),
            f"SubClassOf(<{EX}A> ObjectMaxCardinality(1 <{EX}p> <{EX}B>))",
        ),
        (
            owl.SubClassOf(
                EX + "A",
                owl.DataSomeValuesFrom(EX + "d", owl.DataOneOf(True, 2.5, "x")),
            ),
            f'SubClassOf(<{EX}A> DataSomeValuesFrom(<{EX}d> DataOneOf("2.5"^^xsd:double "true"^^xsd:boolean "x")))',
        ),
        (
            owl.HasKey(EX + "A", [EX + "p"], [EX + "d"]),
            f"HasKey(<{EX}A> (<{EX}p>) (<{EX}d>))",
        ),
        (
            owl.DataPropertyAssertion(
                EX + "d", owl.AnonymousIndividual("x"), owl.Literal("chat", lang="fr")
            ),
            f'DataPropertyAssertion(<{EX}d> _:x "chat"@fr)',
        ),
        (
            owl.NegativeObjectPropertyAssertion(EX + "p", EX + "a", EX + "b"),
            f"NegativeObjectPropertyAssertion(<{EX}p> <{EX}a> <{EX}b>)",
        ),
    ]
    for ax, ofn in cases:
        assert ax == sparkles.Axiom(ofn), (str(ax), ofn)
    annotated = owl.SubClassOf(
        EX + "A",
        EX + "B",
        annotations=[
            owl.Annotation(
                owl.IRI("http://www.w3.org/2000/01/rdf-schema#comment"), "why"
            )
        ],
    )
    assert annotated.json()["annotations"][0]["value"] == {"@value": "why"}
    with pytest.raises(ValueError):
        owl.DisjointClasses(EX + "A")
    with pytest.raises(TypeError):
        owl.Declaration(EX + "A")


def test_every_axiom_type_has_a_constructor() -> None:
    from sparkles import owl

    names = {
        "Declaration",
        "SubClassOf",
        "EquivalentClasses",
        "DisjointClasses",
        "DisjointUnion",
        "SubObjectPropertyOf",
        "EquivalentObjectProperties",
        "DisjointObjectProperties",
        "InverseObjectProperties",
        "ObjectPropertyDomain",
        "ObjectPropertyRange",
        "FunctionalObjectProperty",
        "InverseFunctionalObjectProperty",
        "ReflexiveObjectProperty",
        "IrreflexiveObjectProperty",
        "SymmetricObjectProperty",
        "AsymmetricObjectProperty",
        "TransitiveObjectProperty",
        "SubDataPropertyOf",
        "EquivalentDataProperties",
        "DisjointDataProperties",
        "DataPropertyDomain",
        "DataPropertyRange",
        "FunctionalDataProperty",
        "DatatypeDefinition",
        "HasKey",
        "SameIndividual",
        "DifferentIndividuals",
        "ClassAssertion",
        "ObjectPropertyAssertion",
        "NegativeObjectPropertyAssertion",
        "DataPropertyAssertion",
        "NegativeDataPropertyAssertion",
        "AnnotationAssertion",
        "SubAnnotationPropertyOf",
        "AnnotationPropertyDomain",
        "AnnotationPropertyRange",
    }
    assert len(names) == 37
    assert names <= set(owl.__all__)


def test_an_edit_takes_constructed_axioms(ds: sparkles.Dataset) -> None:
    from sparkles import owl

    done = ds.ontology.edit(
        add=[owl.SubClassOf(owl.Class(EX + "Course"), owl.Class(EX + "Event"))]
    )
    assert done["added"] >= 1
    course = ds.ontology.entity(EX + "Course")
    assert [c["expression"] for c in course["class"]["superClasses"]] == [EX + "Event"]


def test_classify(ds: sparkles.Dataset) -> None:
    ds.update(
        f"{PREFIXES} INSERT DATA {{ :FullProfessor rdfs:subClassOf :Professor . "
        ":Teacher a owl:Class ; owl:equivalentClass [ a owl:Restriction ; "
        "owl:onProperty :teaches ; owl:someValuesFrom :Course ] . }"
    )
    r = ds.ontology.classify(about=EX + "FullProfessor")
    assert r["consistent"] and r["complete"]
    assert not r["reused"]
    assert EX + "Teacher" in r["class"]["superClasses"]
    again = ds.ontology.classify()
    assert again["reused"]
    page = ds.ontology.classify(nodes=True, limit=2)
    assert len(page["nodes"]) == 2 and page["next"] == 2
    with pytest.raises(sparkles.NotFoundError):
        ds.ontology.classify(about=EX + "Nobody")
    written = ds.ontology.classify(target=EX + "taxonomy", message="classified")
    assert written["written"]["committed"]
    sub = f"SubClassOf(<{EX}Professor> <{EX}Teacher>)"
    assert sparkles.Axiom(sub) in [
        e["axiom"] for e in ds.ontology.axioms(EX + "taxonomy")["axioms"]
    ]


def cases(n: int, inconsistent: bool) -> str:
    """Item 603's cases, as the Rust tests write them: a classification that the
    disjunctive rules take, long enough that a deadline stops it part way."""

    def some(p: str, c: str) -> str:
        return f"[ a owl:Restriction ; owl:onProperty {p} ; owl:someValuesFrom {c} ]"

    out = [":r a owl:ObjectProperty . :s a owl:ObjectProperty ."]
    for i in range(n):
        b, d = (7 * i + 3) % n, (13 * i + 5) % n
        out.append(
            f":C{i} a owl:Class ; rdfs:subClassOf [ owl:unionOf ( :B{i} :E{i} ) ] . "
            f":B{i} rdfs:subClassOf :K{i} , :C{b} . :E{i} rdfs:subClassOf :K{i} . "
            f":K{i} rdfs:subClassOf {some(':r', f':C{d}')} . "
            f":E{i} rdfs:subClassOf [ a owl:Restriction ; "
            f"owl:onProperty [ owl:inverseOf :r ] ; owl:allValuesFrom :F{d} ] . "
            f"{some(':r', f':F{i}')} rdfs:subClassOf :G{b} ."
        )
    if inconsistent:
        out.append(
            f"owl:Thing rdfs:subClassOf {some(':s', '[ owl:unionOf ( :N1 :N2 ) ]')} . "
            ":N1 rdfs:subClassOf owl:Nothing . :N2 rdfs:subClassOf :N3 . "
            ":N3 owl:disjointWith :N2 ."
        )
    return "\n".join(out)


def partial_answers(inconsistent: bool) -> list[str]:
    """Classify the cases under deadlines that find the saturation, check every answer and
    return the consistencies of the partial ones.

    The deadlines rise by a few percent from a short one until one gives an answer instead
    of a timeout, which has run out in the saturation, since that is the first part of a
    run that can stop with an answer. The step may be longer than the saturation, so the
    time between the last timeout and that answer is halved until the deadlines are a
    millisecond apart. The deadlines go on until twenty have answered. A ladder fixed from
    the time of a full run misses a saturation of some milliseconds on a loaded machine."""
    import time

    body = f"{PREFIXES} INSERT DATA {{ {cases(300, inconsistent)} }}"
    full_ds = sparkles.Dataset()
    full_ds.update(body)
    t = time.perf_counter()
    full = full_ds.ontology.classify(partial=True, threads=1, limit=10000)
    took = time.perf_counter() - t
    want = "inconsistent" if inconsistent else "consistent"
    assert full["complete"] and not full["partial"]
    assert (full["consistency"], full["tier"]) == (want, "disjunctive")
    assert full["incomplete"] == [] and full["incompleteCount"] == 0
    seen: list[str] = []
    answered = 0

    def answers(timeout: float) -> bool:
        nonlocal answered
        # a dataset of its own, whose read of the ontology takes as long as the full run's
        d = sparkles.Dataset()
        d.update(body)
        try:
            r = d.ontology.classify(partial=True, threads=1, limit=10000, timeout=timeout)
        except sparkles.QueryTimeoutError:
            return False
        answered += 1
        assert r["classes"] == full["classes"]
        if not r["partial"]:
            assert r["complete"] and r["consistency"] == want
            return True
        assert not r["complete"], "a partial answer is presented as complete"
        assert r["incompleteCount"] == len(r["incomplete"])
        assert r["consistency"] in (want, "unknown")
        if r["consistency"] == "unknown":
            assert len(r["incomplete"]) + 2 >= r["classes"]
        seen.append(r["consistency"])
        return True

    def after(ms: int) -> int:
        return ms + max(1, round(0.08 * ms))

    limit = round(20 * took * 1000)
    lo, ms = 0, 2
    while ms < limit and not answers(ms / 1000):
        lo, ms = ms, after(ms)
    hi = ms
    while hi - lo > 1:
        mid = (lo + hi) // 2
        if answers(mid / 1000):
            hi = mid
        else:
            lo = mid
    while answered < 20 and ms < limit:
        ms = after(ms)
        answers(ms / 1000)
    return seen


def test_classify_answers_in_part_under_a_deadline(ds: sparkles.Dataset) -> None:
    assert partial_answers(False), "no deadline gave a partial answer"
    r = ds.ontology.classify(partial=True, expansion="eager")
    assert r["complete"] and not r["partial"] and r["consistency"] == "consistent"
    assert r["tier"] == "el" and r["incomplete"] == []
    with pytest.raises(ValueError):
        ds.ontology.classify(partial=True, target=EX + "taxonomy")
    with pytest.raises(ValueError):
        ds.ontology.classify(expansion="greedy")


def test_classify_answers_in_part_when_its_token_is_cancelled() -> None:
    import threading
    import time

    body = f"{PREFIXES} INSERT DATA {{ {cases(300, False)} }}"
    d = sparkles.Dataset()
    d.update(body)
    t = time.perf_counter()
    d.ontology.classify(threads=1)
    took = time.perf_counter() - t
    partial = 0
    f = 0.02
    while f < 1.5:
        fresh = sparkles.Dataset()
        fresh.update(body)
        token = sparkles.CancelToken()
        timer = threading.Timer(took * f, token.cancel)
        timer.start()
        try:
            r = fresh.ontology.classify(partial=True, threads=1, cancel=token)
        except sparkles.CancelledError:
            r = None
        finally:
            timer.cancel()
        f *= 1.25
        if r is not None and r["partial"]:
            assert not r["complete"]
            partial += 1
    assert partial > 0, "no cancellation gave a partial answer"


def test_classify_leaves_the_consistency_unknown_when_stopped_early() -> None:
    assert "unknown" in partial_answers(True)


def test_manchester_and_dl_query(ds: sparkles.Dataset) -> None:
    ds.update(f'{PREFIXES} INSERT DATA {{ :Course rdfs:label "Course unit" . }}')
    p = ds.ontology.parse("teaches some 'Course unit'")
    assert p["value"] == {
        "type": "ObjectSomeValuesFrom",
        "property": EX + "teaches",
        "class": EX + "Course",
    }
    ax = ds.ontology.parse("Professor SubClassOf teaches some Course", as_="axiom")
    assert ax["value"]["type"] == "SubClassOf"
    with pytest.raises(sparkles.InvalidInputError, match="Lecture") as e:
        ds.ontology.parse("teaches some Lecture")
    assert isinstance(e.value, ValueError)
    assert (e.value.reason, e.value.start, e.value.end) == ("unknown", 13, 20)
    assert e.value.candidates == []
    r = ds.ontology.dl_query("teaches some Course", answers=["subclasses", "instances"])
    assert r["reasoning"]["engine"] == "el" and r["reasoning"]["complete"]
    assert r["subclasses"] == [EX + "Professor"]
    assert r["instances"] == [EX + "ann"]
    j = ds.ontology.dl_query(
        {"type": "ObjectSomeValuesFrom", "property": EX + "teaches", "class": EX + "Course"},
        reasoning="rl",
        answers=["subclasses"],
    )
    assert j["reasoning"]["engine"] == "rl"


def test_search_ranks_entities_with_typos() -> None:
    d = sparkles.Dataset()
    d.update(
        f"""{PREFIXES} PREFIX oio: <http://www.geneontology.org/formats/oboInOwl#>
        INSERT DATA {{
          :Glucose a owl:Class ; rdfs:label "glucose"@en , "Glukose"@de .
          :G6P a owl:Class ; rdfs:label "glucose 6-phosphate" .
          :Sugar a owl:Class ; rdfs:label "sugar" ; oio:hasExactSynonym "saccharide" .
          :has a owl:ObjectProperty ; rdfs:label "has glucose part" .
        }}"""
    )
    found = d.ontology.search("glucose")
    assert [r["iri"] for r in found["results"]] == [EX + "Glucose", EX + "G6P", EX + "has"]
    top = found["results"][0]
    assert top["type"] == "entity" and top["kinds"] == ["class"]
    assert top["match"]["kind"] == "exact" and top["match"]["highlight"] == [[0, 7]]
    assert found["more"] is False
    # one typo, the caller's language for the label, and a kind filter
    found = d.ontology.search("glucsoe", kind="class", limit=1)
    assert found["results"][0]["iri"] == EX + "Glucose"
    assert found["results"][0]["match"]["distance"] == 1
    assert found["more"] is True
    assert d.ontology.search("glucose", lang="de")["results"][0]["label"] == "Glukose"
    assert d.ontology.search("saccharide")["results"][0]["match"]["field"] == "synonym"
    assert [r["iri"] for r in d.ontology.search("gluc", kind="property")["results"]] == [EX + "has"]


def test_hierarchy_and_term_read_one_entity() -> None:
    d = sparkles.Dataset()
    d.update(
        f"""{PREFIXES} INSERT DATA {{
          :Animal a owl:Class ; rdfs:label "animal" .
          :Dog a owl:Class ; rdfs:label "dog" ; rdfs:subClassOf :Animal .
          :Tail a owl:Class ; rdfs:subClassOf [ a owl:Restriction ;
            owl:onProperty :partOf ; owl:someValuesFrom :Dog ] .
          :partOf a owl:ObjectProperty .
        }}"""
    )
    roots = d.ontology.hierarchy()
    assert [n["label"] for n in roots["children"]][0] == "animal"
    kids = d.ontology.hierarchy(EX + "Animal")
    assert [n["iri"] for n in kids["children"]] == [EX + "Dog"]
    up = d.ontology.hierarchy(EX + "Dog", ancestors=True)
    assert up["ancestors"]["edges"] == [[EX + "Dog", EX + "Animal"]]
    t = d.ontology.term(EX + "Dog")
    assert t["label"] == "dog" and t["kinds"] == ["class"]
    assert any(a["type"] == "SubClassOf" for a in t["axioms"])
    assert t["mentions"]["total"] == 1
    assert t["names"][EX + "Tail"]["shortForm"]
    with pytest.raises(sparkles.NotFoundError):
        d.ontology.term(EX + "Nothing")


def test_individual_pages_preserve_graph_cursor_and_revision(tmp_path) -> None:
    d = sparkles.Dataset(str(tmp_path / "individual-pages"))
    graph = EX + "individuals"
    d.update(f"""{PREFIXES} INSERT DATA {{ GRAPH <{graph}> {{
      :Person a owl:Class . :loose a owl:NamedIndividual .
      :a a :Person . :b a :Person . :c a :Person .
    }} GRAPH :hidden {{ :hidden a :Person }} }}""")
    roots = d.ontology.individuals(graph, imports=False, limit=1)
    assert roots["groups"] == [{"type": None, "total": 1}]
    assert roots["next"] == ""
    at = roots["commit"]
    types = d.ontology.individuals(graph, imports=False, limit=1, after=roots["next"], at=at)
    assert [g["type"]["iri"] for g in types["groups"]] == [EX + "Person"]
    assert types["next"] is None
    first = d.ontology.individuals(graph, imports=False, type=EX + "Person", limit=2, at=at)
    assert [i["iri"] for i in first["individuals"]] == [EX + "a", EX + "b"]
    assert first["next"] == EX + "b" and first["total"] == 3
    d.update(f"{PREFIXES} INSERT DATA {{ GRAPH <{graph}> {{ :d a :Person }} }}")
    last = d.ontology.individuals(graph, imports=False, type=EX + "Person", limit=2, after=first["next"], at=at)
    assert last["commit"] == at and last["next"] is None
    assert [i["iri"] for i in last["individuals"]] == [EX + "c"]
    located = d.ontology.individuals(graph, imports=False, iri=EX + "c", at=at)
    assert [t["iri"] for t in located["individuals"][0]["types"]] == [EX + "Person"]
