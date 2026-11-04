"""Typed constructors for OWL 2 axioms and expressions (spec F21 §2.2).

Each constructor is named after its functional-style form and takes its operands in the
order the OWL 2 structural specification gives them. An axiom constructor answers an
:class:`sparkles.Axiom`, which an edit takes as it is. Expressions are the dicts of the
axioms' JSON form, and entities are strings of their IRI, typed by the constructor that
made them, so they can be given wherever an IRI is.

>>> from sparkles import owl
>>> ax = owl.SubClassOf(
...     owl.Class("http://ex.org/Professor"),
...     owl.ObjectSomeValuesFrom(owl.ObjectProperty("http://ex.org/teaches"),
...                              owl.Class("http://ex.org/Course")),
... )
>>> str(ax)
'SubClassOf(<http://ex.org/Professor> ObjectSomeValuesFrom(<http://ex.org/teaches> <http://ex.org/Course>))'

An IRI can be given as a string, a ``NamedNode`` or an entity. A literal can be given as
a :func:`Literal`, a ``sparkles.Literal`` term, a ``str`` (a plain string), a ``bool``, an
``int`` or a ``float``. The value of an annotation is a literal unless it is an
:class:`IRI`, a ``NamedNode`` or an anonymous individual. The module is a namespace of its
own: its ``Class`` and ``Literal`` are not the package's terms.
"""

from __future__ import annotations

import math
from collections.abc import Iterable, Mapping
from typing import Any

from sparkles._sparkles import Axiom

__all__ = [
    "IRI",
    "Class",
    "Datatype",
    "ObjectProperty",
    "DataProperty",
    "AnnotationProperty",
    "NamedIndividual",
    "AnonymousIndividual",
    "Literal",
    "Annotation",
    "ObjectInverseOf",
    "ObjectPropertyChain",
    "DataIntersectionOf",
    "DataUnionOf",
    "DataComplementOf",
    "DataOneOf",
    "DatatypeRestriction",
    "ObjectIntersectionOf",
    "ObjectUnionOf",
    "ObjectComplementOf",
    "ObjectOneOf",
    "ObjectSomeValuesFrom",
    "ObjectAllValuesFrom",
    "ObjectHasValue",
    "ObjectHasSelf",
    "ObjectMinCardinality",
    "ObjectMaxCardinality",
    "ObjectExactCardinality",
    "DataSomeValuesFrom",
    "DataAllValuesFrom",
    "DataHasValue",
    "DataMinCardinality",
    "DataMaxCardinality",
    "DataExactCardinality",
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
]

XSD = "http://www.w3.org/2001/XMLSchema#"


def _iri(x: Any) -> str:
    if isinstance(x, str):
        if x.startswith("_:"):
            raise TypeError(f"expected an IRI, not the blank node {x}")
        return str(x)
    value = getattr(x, "value", None)
    if isinstance(value, str) and not hasattr(x, "datatype"):
        return value
    raise TypeError(f"expected an IRI as a str or a NamedNode, not {x!r}")


class IRI(str):
    """An IRI, as the value or subject of an annotation where a string is a literal."""

    __slots__ = ()

    def __new__(cls, iri: Any) -> IRI:
        return super().__new__(cls, _iri(iri))


class Class(IRI):
    """A class, by its IRI."""

    __slots__ = ()


class Datatype(IRI):
    """A datatype, by its IRI."""

    __slots__ = ()


class ObjectProperty(IRI):
    """An object property, by its IRI."""

    __slots__ = ()


class DataProperty(IRI):
    """A data property, by its IRI."""

    __slots__ = ()


class AnnotationProperty(IRI):
    """An annotation property, by its IRI."""

    __slots__ = ()


class NamedIndividual(IRI):
    """A named individual, by its IRI."""

    __slots__ = ()


class AnonymousIndividual(str):
    """An anonymous individual, by its blank node label (``_:label``)."""

    __slots__ = ()

    def __new__(cls, label: str) -> AnonymousIndividual:
        label = str(label).removeprefix("_:")
        if not label:
            raise ValueError("an anonymous individual needs a label")
        return super().__new__(cls, "_:" + label)


Json = dict[str, Any]
LiteralArg = Any


def Literal(value: Any, datatype: Any = None, lang: str | None = None) -> Json:
    """A literal: ``value`` as its lexical form, with a datatype or a language tag.

    Without either, a ``bool`` is ``xsd:boolean``, an ``int`` is ``xsd:integer``, a
    ``float`` is ``xsd:double`` and anything else is a plain string."""
    if datatype is not None and lang is not None:
        raise ValueError("a literal has a datatype or a language tag, not both")
    if lang is not None:
        return {"@value": str(value), "@language": lang}
    if datatype is not None:
        return {"@value": _lexical(value), "@type": _iri(datatype)}
    if isinstance(value, bool):
        return {"@value": "true" if value else "false", "@type": XSD + "boolean"}
    if isinstance(value, int):
        return {"@value": str(value), "@type": XSD + "integer"}
    if isinstance(value, float):
        return {"@value": _lexical(value), "@type": XSD + "double"}
    return {"@value": str(value)}


def _lexical(value: Any) -> str:
    """The XML Schema lexical form of a value, with the infinities and NaN of a
    ``float`` as ``INF``, ``-INF`` and ``NaN``."""
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, float):
        if math.isnan(value):
            return "NaN"
        if math.isinf(value):
            return "INF" if value > 0 else "-INF"
        return repr(value)
    return str(value)


def _literal(x: LiteralArg) -> Json:
    if isinstance(x, dict):
        if "@value" not in x:
            raise TypeError(f"expected a literal, not {x!r}")
        return x
    if (
        hasattr(x, "datatype")
        and hasattr(x, "value")
        and not isinstance(x, (str, bool, int, float))
    ):
        lang = getattr(x, "language", None)
        if lang:
            return {"@value": x.value, "@language": lang}
        return Literal(x.value, x.datatype)
    return Literal(x)


def _individual(x: Any) -> str:
    if isinstance(x, str) and x.startswith("_:"):
        return str(AnonymousIndividual(x))
    return _iri(x)


def _ope(x: Any) -> Any:
    if isinstance(x, dict):
        if "inverseOf" not in x:
            raise TypeError(f"expected an object property expression, not {x!r}")
        return x
    return _iri(x)


def _ce(x: Any) -> Any:
    if isinstance(x, dict):
        if "type" not in x:
            raise TypeError(f"expected a class expression, not {x!r}")
        return x
    return _iri(x)


def _dr(x: Any) -> Any:
    if isinstance(x, dict):
        if "type" not in x:
            raise TypeError(f"expected a data range, not {x!r}")
        return x
    return _iri(x)


def _annotation_value(x: Any) -> Any:
    if isinstance(x, (IRI, AnonymousIndividual)):
        return str(x)
    if isinstance(x, str):
        return {"@value": x}
    if hasattr(x, "value") and not hasattr(x, "datatype") and not isinstance(x, dict):
        return _iri(x)
    return _literal(x)


def Annotation(
    property: Any, value: Any, annotations: Iterable[Json] | None = None
) -> Json:
    """An annotation: a property and a value, itself annotated by ``annotations``."""
    a: Json = {"property": _iri(property), "value": _annotation_value(value)}
    if annotations:
        a["annotations"] = list(annotations)
    return a


def _axiom(
    type_: str, members: Mapping[str, Any], annotations: Iterable[Json] | None
) -> Axiom:
    j: Json = {"type": type_, **members}
    if annotations:
        j["annotations"] = list(annotations)
    return Axiom.from_json(j)


# ---------------------------------------------------------------- expressions ----


def ObjectInverseOf(property: Any) -> Json:
    return {"inverseOf": _iri(property)}


class ObjectPropertyChain(tuple):
    """The chain of a ``SubObjectPropertyOf`` axiom."""

    # No instance dictionary, so the class adds no layout to tuple's and stays a plain
    # tuple subclass rather than a disjoint base.
    __slots__ = ()

    def __new__(cls, *properties: Any) -> ObjectPropertyChain:
        if len(properties) < 2:
            raise ValueError("a property chain has at least two properties")
        return super().__new__(cls, tuple(_ope(p) for p in properties))


def DataIntersectionOf(*ranges: Any) -> Json:
    return {"type": "DataIntersectionOf", "ranges": [_dr(r) for r in ranges]}


def DataUnionOf(*ranges: Any) -> Json:
    return {"type": "DataUnionOf", "ranges": [_dr(r) for r in ranges]}


def DataComplementOf(range: Any) -> Json:
    return {"type": "DataComplementOf", "range": _dr(range)}


def DataOneOf(*literals: LiteralArg) -> Json:
    return {"type": "DataOneOf", "values": [_literal(v) for v in literals]}


def DatatypeRestriction(datatype: Any, *restrictions: tuple[Any, LiteralArg]) -> Json:
    """A datatype restricted by ``(facet, value)`` pairs, such as
    ``DatatypeRestriction(XSD + "integer", (XSD + "minInclusive", 5))``."""
    if not restrictions:
        raise ValueError("a datatype restriction has at least one facet")
    return {
        "type": "DatatypeRestriction",
        "datatype": _iri(datatype),
        "restrictions": [
            {"facet": _iri(f), "value": _literal(v)} for f, v in restrictions
        ],
    }


def ObjectIntersectionOf(*classes: Any) -> Json:
    return {"type": "ObjectIntersectionOf", "classes": [_ce(c) for c in classes]}


def ObjectUnionOf(*classes: Any) -> Json:
    return {"type": "ObjectUnionOf", "classes": [_ce(c) for c in classes]}


def ObjectComplementOf(cls: Any) -> Json:
    return {"type": "ObjectComplementOf", "class": _ce(cls)}


def ObjectOneOf(*individuals: Any) -> Json:
    return {"type": "ObjectOneOf", "individuals": [_individual(i) for i in individuals]}


def ObjectSomeValuesFrom(property: Any, cls: Any) -> Json:
    return {
        "type": "ObjectSomeValuesFrom",
        "property": _ope(property),
        "class": _ce(cls),
    }


def ObjectAllValuesFrom(property: Any, cls: Any) -> Json:
    return {
        "type": "ObjectAllValuesFrom",
        "property": _ope(property),
        "class": _ce(cls),
    }


def ObjectHasValue(property: Any, individual: Any) -> Json:
    return {
        "type": "ObjectHasValue",
        "property": _ope(property),
        "individual": _individual(individual),
    }


def ObjectHasSelf(property: Any) -> Json:
    return {"type": "ObjectHasSelf", "property": _ope(property)}


def _card(type_: str, n: int, property: Any, filler: Any, key: str, kind: Any) -> Json:
    if isinstance(n, bool) or not isinstance(n, int) or n < 0:
        raise ValueError("a cardinality is a nonnegative int")
    j: Json = {"type": type_, "cardinality": n, "property": property}
    if filler is not None:
        j[key] = kind(filler)
    return j


def ObjectMinCardinality(n: int, property: Any, cls: Any = None) -> Json:
    return _card("ObjectMinCardinality", n, _ope(property), cls, "class", _ce)


def ObjectMaxCardinality(n: int, property: Any, cls: Any = None) -> Json:
    return _card("ObjectMaxCardinality", n, _ope(property), cls, "class", _ce)


def ObjectExactCardinality(n: int, property: Any, cls: Any = None) -> Json:
    return _card("ObjectExactCardinality", n, _ope(property), cls, "class", _ce)


def _data_values(type_: str, args: tuple[Any, ...]) -> Json:
    if len(args) < 2:
        raise ValueError(f"{type_} takes data properties and a data range")
    *props, range_ = args
    j: Json = {"type": type_, "range": _dr(range_)}
    if len(props) == 1:
        j["property"] = _iri(props[0])
    else:
        j["properties"] = [_iri(p) for p in props]
    return j


def DataSomeValuesFrom(*properties_and_range: Any) -> Json:
    """``DataSomeValuesFrom(p1 … pn DR)``: data properties, then the data range."""
    return _data_values("DataSomeValuesFrom", properties_and_range)


def DataAllValuesFrom(*properties_and_range: Any) -> Json:
    """``DataAllValuesFrom(p1 … pn DR)``: data properties, then the data range."""
    return _data_values("DataAllValuesFrom", properties_and_range)


def DataHasValue(property: Any, value: LiteralArg) -> Json:
    return {
        "type": "DataHasValue",
        "property": _iri(property),
        "value": _literal(value),
    }


def DataMinCardinality(n: int, property: Any, range: Any = None) -> Json:
    return _card("DataMinCardinality", n, _iri(property), range, "range", _dr)


def DataMaxCardinality(n: int, property: Any, range: Any = None) -> Json:
    return _card("DataMaxCardinality", n, _iri(property), range, "range", _dr)


def DataExactCardinality(n: int, property: Any, range: Any = None) -> Json:
    return _card("DataExactCardinality", n, _iri(property), range, "range", _dr)


# --------------------------------------------------------------------- axioms ----

_KINDS = {
    Class: "Class",
    Datatype: "Datatype",
    ObjectProperty: "ObjectProperty",
    DataProperty: "DataProperty",
    AnnotationProperty: "AnnotationProperty",
    NamedIndividual: "NamedIndividual",
}

Annotations = Iterable[Json] | None


def Declaration(entity: IRI, *, annotations: Annotations = None) -> Axiom:
    """The declaration of an entity made by :func:`Class`, :func:`ObjectProperty` or
    another entity constructor."""
    kind = _KINDS.get(type(entity))
    if kind is None:
        raise TypeError(
            "a declaration takes an entity such as Class(iri) or ObjectProperty(iri)"
        )
    return _axiom("Declaration", {"kind": kind, "iri": str(entity)}, annotations)


def SubClassOf(sub: Any, sup: Any, *, annotations: Annotations = None) -> Axiom:
    return _axiom(
        "SubClassOf", {"subClass": _ce(sub), "superClass": _ce(sup)}, annotations
    )


def EquivalentClasses(*classes: Any, annotations: Annotations = None) -> Axiom:
    return _axiom(
        "EquivalentClasses", {"classes": [_ce(c) for c in classes]}, annotations
    )


def DisjointClasses(*classes: Any, annotations: Annotations = None) -> Axiom:
    return _axiom(
        "DisjointClasses", {"classes": [_ce(c) for c in classes]}, annotations
    )


def DisjointUnion(cls: Any, *classes: Any, annotations: Annotations = None) -> Axiom:
    return _axiom(
        "DisjointUnion",
        {"class": _iri(cls), "classes": [_ce(c) for c in classes]},
        annotations,
    )


def SubObjectPropertyOf(
    sub: Any, sup: Any, *, annotations: Annotations = None
) -> Axiom:
    """``sub`` is an object property expression or an :class:`ObjectPropertyChain`."""
    members: Json = {"superProperty": _ope(sup)}
    if isinstance(sub, ObjectPropertyChain):
        members["chain"] = list(sub)
    else:
        members["subProperty"] = _ope(sub)
    return _axiom("SubObjectPropertyOf", members, annotations)


def EquivalentObjectProperties(
    *properties: Any, annotations: Annotations = None
) -> Axiom:
    return _axiom(
        "EquivalentObjectProperties",
        {"properties": [_ope(p) for p in properties]},
        annotations,
    )


def DisjointObjectProperties(
    *properties: Any, annotations: Annotations = None
) -> Axiom:
    return _axiom(
        "DisjointObjectProperties",
        {"properties": [_ope(p) for p in properties]},
        annotations,
    )


def InverseObjectProperties(
    first: Any, second: Any, *, annotations: Annotations = None
) -> Axiom:
    return _axiom(
        "InverseObjectProperties",
        {"first": _ope(first), "second": _ope(second)},
        annotations,
    )


def ObjectPropertyDomain(
    property: Any, domain: Any, *, annotations: Annotations = None
) -> Axiom:
    return _axiom(
        "ObjectPropertyDomain",
        {"property": _ope(property), "domain": _ce(domain)},
        annotations,
    )


def ObjectPropertyRange(
    property: Any, range: Any, *, annotations: Annotations = None
) -> Axiom:
    return _axiom(
        "ObjectPropertyRange",
        {"property": _ope(property), "range": _ce(range)},
        annotations,
    )


def _characteristic(type_: str) -> Any:
    def make(property: Any, *, annotations: Annotations = None) -> Axiom:
        return _axiom(type_, {"property": _ope(property)}, annotations)

    make.__name__ = make.__qualname__ = type_
    make.__doc__ = f"``{type_}(property)``."
    return make


FunctionalObjectProperty = _characteristic("FunctionalObjectProperty")
InverseFunctionalObjectProperty = _characteristic("InverseFunctionalObjectProperty")
ReflexiveObjectProperty = _characteristic("ReflexiveObjectProperty")
IrreflexiveObjectProperty = _characteristic("IrreflexiveObjectProperty")
SymmetricObjectProperty = _characteristic("SymmetricObjectProperty")
AsymmetricObjectProperty = _characteristic("AsymmetricObjectProperty")
TransitiveObjectProperty = _characteristic("TransitiveObjectProperty")


def SubDataPropertyOf(sub: Any, sup: Any, *, annotations: Annotations = None) -> Axiom:
    return _axiom(
        "SubDataPropertyOf",
        {"subProperty": _iri(sub), "superProperty": _iri(sup)},
        annotations,
    )


def EquivalentDataProperties(
    *properties: Any, annotations: Annotations = None
) -> Axiom:
    return _axiom(
        "EquivalentDataProperties",
        {"properties": [_iri(p) for p in properties]},
        annotations,
    )


def DisjointDataProperties(*properties: Any, annotations: Annotations = None) -> Axiom:
    return _axiom(
        "DisjointDataProperties",
        {"properties": [_iri(p) for p in properties]},
        annotations,
    )


def DataPropertyDomain(
    property: Any, domain: Any, *, annotations: Annotations = None
) -> Axiom:
    return _axiom(
        "DataPropertyDomain",
        {"property": _iri(property), "domain": _ce(domain)},
        annotations,
    )


def DataPropertyRange(
    property: Any, range: Any, *, annotations: Annotations = None
) -> Axiom:
    return _axiom(
        "DataPropertyRange",
        {"property": _iri(property), "range": _dr(range)},
        annotations,
    )


def FunctionalDataProperty(property: Any, *, annotations: Annotations = None) -> Axiom:
    return _axiom("FunctionalDataProperty", {"property": _iri(property)}, annotations)


def DatatypeDefinition(
    datatype: Any, range: Any, *, annotations: Annotations = None
) -> Axiom:
    return _axiom(
        "DatatypeDefinition",
        {"datatype": _iri(datatype), "range": _dr(range)},
        annotations,
    )


def HasKey(
    cls: Any,
    object_properties: Iterable[Any] = (),
    data_properties: Iterable[Any] = (),
    *,
    annotations: Annotations = None,
) -> Axiom:
    """``HasKey(C (OPE…) (DPE…))``: the object properties and the data properties of the key."""
    return _axiom(
        "HasKey",
        {
            "class": _ce(cls),
            "objectProperties": [_ope(p) for p in object_properties],
            "dataProperties": [_iri(p) for p in data_properties],
        },
        annotations,
    )


def SameIndividual(*individuals: Any, annotations: Annotations = None) -> Axiom:
    return _axiom(
        "SameIndividual",
        {"individuals": [_individual(i) for i in individuals]},
        annotations,
    )


def DifferentIndividuals(*individuals: Any, annotations: Annotations = None) -> Axiom:
    return _axiom(
        "DifferentIndividuals",
        {"individuals": [_individual(i) for i in individuals]},
        annotations,
    )


def ClassAssertion(
    cls: Any, individual: Any, *, annotations: Annotations = None
) -> Axiom:
    return _axiom(
        "ClassAssertion",
        {"class": _ce(cls), "individual": _individual(individual)},
        annotations,
    )


def ObjectPropertyAssertion(
    property: Any, source: Any, target: Any, *, annotations: Annotations = None
) -> Axiom:
    members = {
        "property": _ope(property),
        "source": _individual(source),
        "target": _individual(target),
    }
    return _axiom("ObjectPropertyAssertion", members, annotations)


def NegativeObjectPropertyAssertion(
    property: Any, source: Any, target: Any, *, annotations: Annotations = None
) -> Axiom:
    members = {
        "property": _ope(property),
        "source": _individual(source),
        "target": _individual(target),
    }
    return _axiom("NegativeObjectPropertyAssertion", members, annotations)


def DataPropertyAssertion(
    property: Any, source: Any, value: LiteralArg, *, annotations: Annotations = None
) -> Axiom:
    members = {
        "property": _iri(property),
        "source": _individual(source),
        "value": _literal(value),
    }
    return _axiom("DataPropertyAssertion", members, annotations)


def NegativeDataPropertyAssertion(
    property: Any, source: Any, value: LiteralArg, *, annotations: Annotations = None
) -> Axiom:
    members = {
        "property": _iri(property),
        "source": _individual(source),
        "value": _literal(value),
    }
    return _axiom("NegativeDataPropertyAssertion", members, annotations)


def AnnotationAssertion(
    property: Any, subject: Any, value: Any, *, annotations: Annotations = None
) -> Axiom:
    """The subject is an IRI or an anonymous individual; the value is a literal unless it
    is an :class:`IRI`, a ``NamedNode`` or an anonymous individual."""
    members = {
        "property": _iri(property),
        "subject": _individual(subject),
        "value": _annotation_value(value),
    }
    return _axiom("AnnotationAssertion", members, annotations)


def SubAnnotationPropertyOf(
    sub: Any, sup: Any, *, annotations: Annotations = None
) -> Axiom:
    return _axiom(
        "SubAnnotationPropertyOf",
        {"subProperty": _iri(sub), "superProperty": _iri(sup)},
        annotations,
    )


def AnnotationPropertyDomain(
    property: Any, domain: Any, *, annotations: Annotations = None
) -> Axiom:
    return _axiom(
        "AnnotationPropertyDomain",
        {"property": _iri(property), "domain": _iri(domain)},
        annotations,
    )


def AnnotationPropertyRange(
    property: Any, range: Any, *, annotations: Annotations = None
) -> Axiom:
    return _axiom(
        "AnnotationPropertyRange",
        {"property": _iri(property), "range": _iri(range)},
        annotations,
    )
