"""Incremental views and live queries (spec F13)."""

import sparkles

Q = "SELECT ?t (COUNT(?p) AS ?n) WHERE { ?p <urn:in> ?t } GROUP BY ?t"


def test_views_are_defined_and_read() -> None:
    ds = sparkles.Dataset.memory()
    ds.update("INSERT DATA { <urn:a> <urn:in> <urn:t> . <urn:b> <urn:in> <urn:t> }")
    info = ds.views.put("sizes", Q, wait=30)
    assert info["state"] == "current"
    rows = ds.views.read("sizes")
    assert rows["columns"] == ["t", "n"]
    assert len(rows["rows"]) == 1
    assert [v["name"] for v in ds.views.list()] == ["sizes"]
    assert "plan" in ds.views.explain("SELECT * WHERE { ?s <urn:p>+ ?o }")
    assert ds.views.delete("sizes")
    assert ds.views.get("sizes") is None


def test_live_queries_send_diffs() -> None:
    ds = sparkles.Dataset.memory()
    ds.update("INSERT DATA { <urn:a> <urn:in> <urn:t> }")
    with ds.live("SELECT ?p WHERE { ?p <urn:in> <urn:t> }") as live:
        first = next(live)
        assert first["event"] == "snapshot"
        assert first["vars"] == ["p"]
        assert len(first["rows"]) == 1
        ds.update("INSERT DATA { <urn:b> <urn:in> <urn:t> }")
        diff = live.next(timeout=10)
        assert diff is not None and diff["event"] == "diff"
        (terms, count) = diff["added"][0]
        assert str(terms[0]) == "<urn:b>" and count == 1


def test_close_wakes_a_waiting_iterator() -> None:
    import threading

    ds = sparkles.Dataset.memory()
    live = ds.live("SELECT ?p WHERE { ?p <urn:in> <urn:t> }")
    next(live)
    out: list[object] = []
    t = threading.Thread(target=lambda: out.append(live.next()))
    t.start()
    live.close()
    t.join(timeout=10)
    assert not t.is_alive()
    assert out == [None]


def test_views_recompute_with_rdfs_on_read() -> None:
    ds = sparkles.Dataset.memory()
    ds.update(
        "INSERT DATA { <urn:Student> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <urn:Person> ."
        " <urn:a> a <urn:Student> }"
    )
    people = "SELECT ?x WHERE { ?x a <urn:Person> }"
    info = ds.views.put("people", people, wait=30)
    assert info["evaluation"] == "incremental"
    assert ds.views.read("people")["rows"] == []
    ds.reasoning.rdfs.set(graph="default")
    assert len(ds.views.read("people")["rows"]) == 1
    view = ds.views.get("people")
    assert view is not None
    assert view["evaluation"] == "recompute"
    assert view["evaluationReason"] == "RDFS on read"
    ds.reasoning.rdfs.reset()
    assert ds.views.read("people")["rows"] == []
    assert ds.views.list()[0]["evaluation"] == "incremental"
