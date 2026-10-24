"""`Dataset.forms` returns the model, check and save of the library's `ds.forms()`."""

from __future__ import annotations

import pytest

import sparkles
from sparkles import (
    CancelToken,
    CancelledError,
    ConflictError,
    Dataset,
    InvalidInputError,
    Literal,
    NamedNode,
    PermissionDeniedError,
    WriteRejectedError,
)

pytestmark = pytest.mark.skipif("shacl" not in sparkles.FEATURES, reason="built without shacl")

EX = "http://ex.org/"

SHAPES = """
@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix ex: <http://ex.org/> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
ex:PersonShape a sh:NodeShape ; sh:targetClass ex:Person ;
  sh:property [ sh:path ex:name ; sh:name "Name" ; sh:minCount 1 ; sh:datatype xsd:string ; sh:order 1 ] ;
  sh:property [ sh:path ex:email ; sh:name "Email" ; sh:maxCount 1 ;
                sh:pattern "^[^@]+@[^@]+\\\\.[a-z]+$" ; sh:order 2 ] ;
  sh:property [ sh:path ( ex:worksFor ex:name ) ; sh:name "Employer name" ] .
"""

DATA = """
PREFIX ex: <http://ex.org/>
INSERT DATA {
  ex:alice a ex:Person ; ex:name "Alice Lima" ; ex:email "alice@ex.org" ; ex:worksFor ex:acme .
  ex:acme a ex:Org ; ex:name "Acme Corp" .
}"""

SETTINGS = {"shapes": "graphs", "graphs": ["urn:shapes"]}
NAME = f"<{EX}name>"
EMAIL = f"<{EX}email>"


def lit(value: str) -> dict:
    return {"type": "literal", "value": value}


def change(field: str, remove: list, add: list, expect: list, focus: str = "alice", **extra) -> dict:
    return {
        "focus": EX + focus,
        "changes": [{"field": field, "remove": remove, "add": add}],
        "expect": {field: expect},
        **extra,
    }


@pytest.fixture
def ds() -> Dataset:
    d = Dataset()
    d.update(DATA)
    d.load(SHAPES, "turtle", to_graph="urn:shapes")
    return d


def field(model: dict, label: str) -> dict:
    return next(f for f in model["fields"] if f["label"] == label)


def names(ds: Dataset, focus: str = "alice") -> list[str]:
    rows = ds.query(f"SELECT ?n WHERE {{ <{EX}{focus}> <{EX}name> ?n }} ORDER BY ?n")
    return [str(r["n"].value) for r in rows]


def test_the_model_lists_fields_with_their_values(ds: Dataset) -> None:
    m = ds.forms.model(EX + "alice", settings=SETTINGS)
    assert isinstance(m, dict)
    assert m["formFormat"] == 1
    assert m["types"] == [EX + "Person"]
    assert m["shapes"]["nodeShapes"] == [EX + "PersonShape"]
    assert m["editable"] is True
    assert m["branch"] == "main"
    assert [f["label"] for f in m["fields"]] == ["Name", "Email", "Employer name"]
    name = field(m, "Name")
    assert name["id"] == NAME
    assert name["constraints"]["minCount"] == 1
    assert name["values"][0]["term"] == lit("Alice Lima")
    assert field(m, "Employer name")["reason"] == "complex-path"
    # triples no editable field covers are listed read-only
    assert [o["predicate"] for o in m["other"]] == [EX + "worksFor"]


def test_the_focus_may_be_a_named_node_and_the_model_may_be_read_only(ds: Dataset) -> None:
    m = ds.forms.model(NamedNode(EX + "alice"), settings=SETTINGS, writable=False)
    assert m["editable"] is False
    assert not field(m, "Name")["editable"]


def test_a_model_at_a_past_commit_is_not_editable(ds: Dataset) -> None:
    ds.snapshots.create("before")
    ds.update(f'DELETE DATA {{ <{EX}alice> <{EX}name> "Alice Lima" }} ; INSERT DATA {{ <{EX}alice> <{EX}name> "Ali" }}')
    now = ds.forms.model(EX + "alice", settings=SETTINGS)
    then = ds.forms.model(EX + "alice", settings=SETTINGS, at="snapshot:before")
    assert field(now, "Name")["values"][0]["term"] == lit("Ali")
    assert field(then, "Name")["values"][0]["term"] == lit("Alice Lima")
    assert then["editable"] is False


def test_a_form_for_a_new_instance_has_no_values(ds: Dataset) -> None:
    m = ds.forms.model(class_=EX + "Person", settings=SETTINGS)
    assert m["focus"] is None
    assert m["types"] == [EX + "Person"]
    assert field(m, "Name")["values"] == []


def test_the_default_settings_read_the_guard_and_say_when_there_is_none(ds: Dataset) -> None:
    m = ds.forms.model(EX + "alice")
    assert m["fields"] == []
    assert "no write-time" in m["shapes"]["missing"]


def test_a_check_previews_the_change_and_reports_violations(ds: Dataset) -> None:
    good = ds.forms.check(change(EMAIL, [lit("alice@ex.org")], [lit("a@ex.org")], [lit("alice@ex.org")]), settings=SETTINGS)
    assert good["branch"] == "main"
    assert good["conflicts"] == []
    assert good["validation"]["checked"] is True
    assert good["validation"]["conforms"] is True
    assert good["preview"]["outcome"] == "commit"
    assert good["preview"]["wouldCommit"] is True
    assert "DELETE DATA" in good["update"] and "INSERT DATA" in good["update"]
    assert good["preview"]["changes"]["total"] == 2
    # a check writes nothing
    assert ds.head_commit.seq == 2
    assert next(iter(ds.query(f"SELECT ?e WHERE {{ <{EX}alice> <{EX}email> ?e }}")))["e"].value == "alice@ex.org"

    bad = ds.forms.check(change(EMAIL, [lit("alice@ex.org")], [lit("not an email")], [lit("alice@ex.org")]), settings=SETTINGS)
    assert bad["validation"]["conforms"] is False
    results = bad["validation"]["results"]
    assert results and results[0]["field"] == EMAIL


def test_a_check_reports_the_fields_another_commit_changed(ds: Dataset) -> None:
    ds.update(f'DELETE DATA {{ <{EX}alice> <{EX}name> "Alice Lima" }} ; INSERT DATA {{ <{EX}alice> <{EX}name> "Ali" }}')
    c = ds.forms.check(change(NAME, [lit("Alice Lima")], [lit("Alicia")], [lit("Alice Lima")]), settings=SETTINGS)
    assert c["conflicts"] == [{"id": NAME, "now": [lit("Ali")]}]


def test_a_save_commits_one_update_with_its_message(ds: Dataset) -> None:
    before = ds.head_commit.seq
    r = ds.forms.save(
        change(NAME, [lit("Alice Lima")], [lit("Alice Lopez")], [lit("Alice Lima")], message="rename"),
        settings=SETTINGS,
        author="test",
    )
    assert r["committed"] is True
    assert r["commit"]["seq"] == before + 1
    assert r["commit"]["message"] == "rename"
    assert r["focus"] == EX + "alice"
    assert r["branch"] == "main"
    assert names(ds) == ["Alice Lopez"]
    assert ds.head_commit.message == "rename"


def test_terms_may_be_python_terms(ds: Dataset) -> None:
    ds.forms.save(
        change(NAME, [Literal("Alice Lima")], [Literal("Alice L.")], [Literal("Alice Lima")]),
        settings=SETTINGS,
    )
    assert names(ds) == ["Alice L."]


def test_the_values_a_model_lists_go_back_as_expected_values(ds: Dataset) -> None:
    m = ds.forms.model(EX + "alice", settings=SETTINGS)
    loaded = [v["term"] for v in field(m, "Name")["values"]]
    ds.forms.save(change(NAME, loaded, [lit("Al")], loaded), settings=SETTINGS)
    assert names(ds) == ["Al"]


def test_a_stale_save_raises_a_conflict_and_writes_nothing(ds: Dataset) -> None:
    ds.update(f'DELETE DATA {{ <{EX}alice> <{EX}name> "Alice Lima" }} ; INSERT DATA {{ <{EX}alice> <{EX}name> "Ali" }}')
    head = ds.head_commit.seq
    with pytest.raises(ConflictError, match="another commit changed") as e:
        ds.forms.save(change(NAME, [lit("Alice Lima")], [lit("Alicia")], [lit("Alice Lima")]), settings=SETTINGS)
    assert e.value.code == "form-conflict"
    assert e.value.fields == [{"id": NAME, "now": [lit("Ali")]}]
    assert ds.head_commit.seq == head
    assert names(ds) == ["Ali"]


def test_a_new_instance_gets_a_minted_iri_and_its_type(ds: Dataset) -> None:
    c = {
        "class": EX + "Person",
        "changes": [{"field": NAME, "add": [lit("Carol")]}],
        "expect": {NAME: []},
    }
    check = ds.forms.check(c, settings=SETTINGS)
    assert check["validation"]["conforms"] is True
    r = ds.forms.save(c, settings=SETTINGS)
    focus = r["focus"]
    assert focus.startswith("urn:uuid:")
    assert ds.ask(f'ASK {{ <{focus}> a <{EX}Person> ; <{EX}name> "Carol" }}')


def test_a_save_on_a_session_branch_leaves_main_alone(ds: Dataset) -> None:
    ds.branches.create("edits.test")
    branch = ds.branch("edits.test")
    m = branch.forms.model(EX + "alice", settings=SETTINGS)
    assert m["branch"] == "edits.test"
    r = branch.forms.save(
        change(NAME, [lit("Alice Lima")], [lit("Alice B.")], [lit("Alice Lima")], message="on a branch"),
        settings=SETTINGS,
    )
    assert r["branch"] == "edits.test"
    assert names(branch) == ["Alice B."]
    assert names(ds) == ["Alice Lima"]
    assert branch.forms.check(change(NAME, [lit("Alice B.")], [], [lit("Alice B.")]), settings=SETTINGS)["branch"] == "edits.test"


def test_a_protected_branch_refuses_a_check_and_a_save(ds: Dataset) -> None:
    ds.branches.update("main", protected=True)
    c = change(NAME, [lit("Alice Lima")], [lit("Alicia")], [lit("Alice Lima")])
    for call in (ds.forms.check, ds.forms.save):
        with pytest.raises(PermissionDeniedError, match="protected"):
            call(c, settings=SETTINGS)
    assert names(ds) == ["Alice Lima"]


def test_a_directional_literal_goes_back_as_it_was_listed(ds: Dataset) -> None:
    ds.update(f'INSERT DATA {{ <{EX}alice> <{EX}name> "marhaba"@ar--rtl }}')
    m = ds.forms.model(EX + "alice", settings=SETTINGS)
    loaded = [v["term"] for v in field(m, "Name")["values"]]
    assert {"type": "literal", "value": "marhaba", "xml:lang": "ar", "its:dir": "rtl"} in loaded
    # the model's own values are the expected values, so no conflict is found
    c = ds.forms.check(change(NAME, [], [lit("Zed")], loaded), settings=SETTINGS)
    assert c["conflicts"] == []
    added = {"type": "literal", "value": "shalom", "xml:lang": "he", "its:dir": "rtl"}
    ds.forms.save(change(NAME, [], [added], loaded), settings=SETTINGS)
    assert ds.ask(f'ASK {{ <{EX}alice> <{EX}name> "shalom"@he--rtl }}')
    with pytest.raises(InvalidInputError, match="its:dir"):
        ds.forms.check(change(NAME, [], [{**added, "its:dir": "up"}], []), settings=SETTINGS)


def test_a_check_reports_the_storage_a_quota_refuses(tmp_path) -> None:
    ds = Dataset(str(tmp_path / "db"))
    ds.update(DATA)
    ds.load(SHAPES, "turtle", to_graph="urn:shapes")
    c = change(NAME, [lit("Alice Lima")], [lit("Alicia " * 100)], [lit("Alice Lima")])
    ok = ds.forms.check(c, settings=SETTINGS)["preview"]
    assert ok["outcome"] == "commit"
    assert ok["storage"]["status"] == "fits"
    ds.settings.quota.set(max_bytes=1)
    p = ds.forms.check(c, settings=SETTINGS)["preview"]
    assert p["outcome"] == "storage-refused"
    assert p["wouldCommit"] is True
    assert p["storage"]["status"] == "refused"
    assert p["storage"]["limit"] == 1
    assert p["error"] == p["storage"]["error"]
    assert p["budget"] == p["storage"]["budget"]
    ds.close()


def test_a_guard_rejects_what_the_check_flagged() -> None:
    ds = Dataset()
    ds.update(DATA)
    ds.validation.guard.set({"mode": "reject", "shapes": {"inline": SHAPES}})
    c = change(NAME, [lit("Alice Lima")], [], [lit("Alice Lima")])
    m = ds.forms.model(EX + "alice")
    assert m["shapes"]["source"] == "guard"
    assert field(m, "Name")["constraints"]["minCount"] == 1
    check = ds.forms.check(c)
    assert check["validation"]["conforms"] is False
    assert check["preview"]["outcome"] == "rejected"
    with pytest.raises(WriteRejectedError):
        ds.forms.save(c)
    assert names(ds) == ["Alice Lima"]


@pytest.mark.parametrize(
    "bad, message",
    [
        ({"changes": [], "expect": {}}, "focus"),
        ({"focus": EX + "alice", "class": EX + "Person", "changes": [], "expect": {}}, "focus"),
        ({"focus": EX + "alice", "expect": {}}, "changes"),
        ({"focus": EX + "alice", "changes": [{"remove": []}], "expect": {}}, "field"),
        ({"focus": EX + "alice", "changes": [], "expect": {}, "branch": "x"}, "unknown member"),
        ({"focus": "not an iri", "changes": [], "expect": {}}, "IRI"),
        ({"focus": EX + "alice", "changes": [{"field": NAME, "add": [{"type": "bnode", "value": "b"}]}], "expect": {NAME: []}}, "blank"),
        ({"focus": EX + "alice", "changes": [{"field": "<http://ex.org/missing>", "add": [lit("x")]}], "expect": {"<http://ex.org/missing>": []}}, "field"),
        ({"focus": EX + "alice", "changes": [{"field": NAME, "add": [lit("x")]}]}, "expect"),
    ],
)
def test_invalid_changes_raise_invalid_input(ds: Dataset, bad: dict, message: str) -> None:
    for call in (ds.forms.check, ds.forms.save):
        with pytest.raises((InvalidInputError, ValueError), match=message):
            call(bad, settings=SETTINGS)
    assert names(ds) == ["Alice Lima"]


def test_a_change_must_be_a_mapping(ds: Dataset) -> None:
    with pytest.raises(TypeError):
        ds.forms.check([1], settings=SETTINGS)  # type: ignore[arg-type]


@pytest.mark.parametrize(
    "settings, message",
    [
        ({"shapes": "graphs"}, "graphs"),
        ({"shapes": "nowhere"}, "settings"),
        ({"unknown": 1}, "settings"),
        ({"branchTemplate": "a/b"}, "branchTemplate"),
    ],
)
def test_invalid_settings_raise_invalid_input(ds: Dataset, settings: dict, message: str) -> None:
    with pytest.raises(InvalidInputError, match=message):
        ds.forms.model(EX + "alice", settings=settings)


def test_a_model_names_a_focus_or_a_class_and_a_class_form_takes_no_at(ds: Dataset) -> None:
    with pytest.raises(InvalidInputError, match="focus"):
        ds.forms.model(settings=SETTINGS)
    with pytest.raises(InvalidInputError, match="focus"):
        ds.forms.model(EX + "alice", class_=EX + "Person", settings=SETTINGS)
    with pytest.raises(InvalidInputError, match="at"):
        ds.forms.model(class_=EX + "Person", settings=SETTINGS, at=1)
    with pytest.raises((InvalidInputError, ValueError)):
        ds.forms.model("not an iri", settings=SETTINGS)


def test_a_shapes_graph_with_no_shapes_gives_a_form_with_no_fields() -> None:
    ds = Dataset()
    ds.update(DATA)
    assert ds.forms.model(EX + "alice", settings=SETTINGS)["fields"] == []


def test_a_closed_dataset_raises(ds: Dataset) -> None:
    forms = ds.forms
    ds.close()
    for call in (
        lambda: forms.model(EX + "alice", settings=SETTINGS),
        lambda: forms.check(change(NAME, [], [lit("x")], []), settings=SETTINGS),
        lambda: forms.save(change(NAME, [], [lit("x")], []), settings=SETTINGS),
    ):
        with pytest.raises(InvalidInputError, match="closed"):
            call()


def test_a_cancelled_token_stops_each_call(ds: Dataset) -> None:
    token = CancelToken()
    token.cancel()
    head = ds.head_commit.seq
    with pytest.raises(CancelledError):
        ds.forms.model(EX + "alice", settings=SETTINGS, cancel=token)
    c = change(NAME, [lit("Alice Lima")], [lit("Alicia")], [lit("Alice Lima")])
    with pytest.raises(CancelledError):
        ds.forms.check(c, settings=SETTINGS, cancel=token)
    with pytest.raises(CancelledError):
        ds.forms.save(c, settings=SETTINGS, cancel=token)
    assert ds.head_commit.seq == head


def test_a_bad_timeout_and_cancel_are_refused(ds: Dataset) -> None:
    c = change(NAME, [lit("Alice Lima")], [lit("Alicia")], [lit("Alice Lima")])
    for timeout in (-1.0, float("nan"), float("inf"), 1e19, 1e300):
        with pytest.raises(ValueError):
            ds.forms.model(EX + "alice", settings=SETTINGS, timeout=timeout)
        with pytest.raises(ValueError):
            ds.forms.check(c, settings=SETTINGS, timeout=timeout)
        with pytest.raises(ValueError):
            ds.forms.save(c, settings=SETTINGS, timeout=timeout)
    # the request thread is still alive
    assert ds.forms.model(EX + "alice", settings=SETTINGS, timeout=60)["editable"] is True
    with pytest.raises(TypeError):
        ds.forms.model(EX + "alice", settings=SETTINGS, cancel=object())  # type: ignore[arg-type]
