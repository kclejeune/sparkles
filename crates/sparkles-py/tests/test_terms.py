"""Terms: construction, equality, conversion to and from Python values (A9)."""

from __future__ import annotations

import datetime
import pickle
from decimal import Decimal

import pytest

from sparkles import BlankNode, Dataset, DefaultGraph, Literal, NamedNode, Quad, Triple, Variable

XSD = "http://www.w3.org/2001/XMLSchema#"


def test_named_node() -> None:
    n = NamedNode("http://ex.org/a")
    assert n.value == "http://ex.org/a"
    assert str(n) == "<http://ex.org/a>"
    assert repr(n) == "NamedNode('http://ex.org/a')"
    assert n == NamedNode("http://ex.org/a")
    assert n != NamedNode("http://ex.org/b")
    assert len({n, NamedNode("http://ex.org/a")}) == 1
    with pytest.raises(ValueError):
        NamedNode("not an iri")


def test_blank_node() -> None:
    a, b = BlankNode(), BlankNode()
    assert a != b
    assert BlankNode("x").value == "x"
    assert str(BlankNode("x")) == "_:x"
    with pytest.raises(ValueError):
        BlankNode("not valid!")


def test_literals() -> None:
    s = Literal("chat")
    assert s.value == "chat"
    assert s.datatype == NamedNode(XSD + "string")
    assert s.language is None
    assert str(s) == '"chat"'
    fr = Literal("chat", language="fr")
    assert fr.language == "fr"
    assert str(fr) == '"chat"@fr'
    assert fr != s
    typed = Literal("42", datatype=NamedNode(XSD + "integer"))
    assert typed == Literal(42)
    assert Literal("1", datatype=XSD + "integer").datatype.value == XSD + "integer"
    rtl = Literal("مرحبا", language="ar", direction="rtl")
    assert rtl.direction == "rtl"
    assert str(rtl) == '"مرحبا"@ar--rtl'
    with pytest.raises(ValueError):
        Literal("x", language="fr", datatype=NamedNode(XSD + "string"))
    with pytest.raises(ValueError):
        Literal("x", direction="ltr")
    with pytest.raises(ValueError):
        Literal("x", language="not a tag!")
    with pytest.raises(TypeError):
        Literal(1, datatype=NamedNode(XSD + "integer"))
    with pytest.raises(TypeError):
        Literal(object())


@pytest.mark.parametrize(
    ("value", "datatype", "lexical"),
    [
        (True, "boolean", "true"),
        (False, "boolean", "false"),
        (42, "integer", "42"),
        (-(10**30), "integer", str(-(10**30))),
        (1.5, "double", "1.5"),
        (float("inf"), "double", "INF"),
        (Decimal("12.50"), "decimal", "12.50"),
        (Decimal("1E+2"), "decimal", "100"),
        (datetime.datetime(2024, 1, 2, 3, 4, 5), "dateTime", "2024-01-02T03:04:05"),
        (
            datetime.datetime(2024, 1, 2, 3, 4, 5, tzinfo=datetime.timezone.utc),
            "dateTime",
            "2024-01-02T03:04:05+00:00",
        ),
        (datetime.date(2024, 1, 2), "date", "2024-01-02"),
        (datetime.time(3, 4, 5), "time", "03:04:05"),
        (b"\x00\xffhi", "base64Binary", "AP9oaQ=="),
    ],
)
def test_native_values_round_trip(value: object, datatype: str, lexical: str) -> None:
    lit = Literal(value)
    assert lit.datatype == NamedNode(XSD + datatype)
    assert lit.value == lexical
    assert lit.to_python() == value
    assert type(lit.to_python()) is type(value)
    # and through a dataset
    ds = Dataset()
    q = Quad(NamedNode("http://ex.org/s"), NamedNode("http://ex.org/p"), lit)
    ds.add(q)
    (back,) = ds.quads_for_pattern(None, NamedNode("http://ex.org/p"))
    assert back.object.to_python() == value


def test_to_python_other_values() -> None:
    assert Literal("NaN", datatype=NamedNode(XSD + "double")).to_python() != 0
    assert Literal("2024-01-02T03:04:05Z", datatype=NamedNode(XSD + "dateTime")).to_python() == (
        datetime.datetime(2024, 1, 2, 3, 4, 5, tzinfo=datetime.timezone.utc)
    )
    assert Literal("7", datatype=NamedNode(XSD + "unsignedByte")).to_python() == 7
    assert Literal("cafe", datatype=NamedNode(XSD + "hexBinary")).to_python() == b"\xca\xfe"
    # ill-typed and unknown datatypes give the lexical form
    assert Literal("abc", datatype=NamedNode(XSD + "integer")).to_python() == "abc"
    assert Literal("x", datatype=NamedNode("http://ex.org/dt")).to_python() == "x"
    assert Literal("chat", language="fr").to_python() == "chat"


def test_triple_and_quad() -> None:
    s, p, o = NamedNode("http://ex.org/s"), NamedNode("http://ex.org/p"), Literal("o")
    t = Triple(s, p, o)
    assert (t.subject, t.predicate, t.object) == (s, p, o)
    assert tuple(t) == (s, p, o)
    assert t[2] == o and t[-1] == o and len(t) == 3
    assert str(t) == '<http://ex.org/s> <http://ex.org/p> "o"'
    q = Quad(s, p, o)
    assert q.graph_name == DefaultGraph()
    assert q.triple == t
    g = NamedNode("http://ex.org/g")
    q2 = Quad(s, p, o, g)
    sub, pred, obj, graph = q2
    assert graph == g
    assert Quad(s, p, o, "http://ex.org/g") == q2
    assert q != q2
    with pytest.raises(TypeError):
        Triple(o, p, o)  # a literal subject
    with pytest.raises(TypeError):
        Triple(s, "http://ex.org/p", o)  # type: ignore[arg-type]
    # an RDF 1.2 triple term as an object
    nested = Triple(s, p, t)
    assert nested.object == t


def test_variable_and_default_graph() -> None:
    assert Variable("?x") == Variable("x")
    assert str(Variable("x")) == "?x"
    assert DefaultGraph() == DefaultGraph()
    assert hash(DefaultGraph()) == hash(DefaultGraph())


def test_pickle() -> None:
    terms = [
        NamedNode("http://ex.org/a"),
        BlankNode("b1"),
        Literal("x"),
        Literal("chat", language="fr"),
        Literal("hi", language="en", direction="ltr"),
        Literal(42),
        Triple(NamedNode("http://ex.org/a"), NamedNode("http://ex.org/p"), Literal(1)),
        Quad(NamedNode("http://ex.org/a"), NamedNode("http://ex.org/p"), Literal(1), NamedNode("http://ex.org/g")),
        Variable("v"),
    ]
    for t in terms:
        assert pickle.loads(pickle.dumps(t)) == t
